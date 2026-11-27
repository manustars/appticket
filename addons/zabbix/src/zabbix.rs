//! Zabbix's JSON-RPC API (`<frontend>/api_jsonrpc.php`), called with an API token in
//! `Authorization: Bearer`, as Zabbix 7.0 and later take it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::coverage::HostGroup;
use crate::kinds::HostTraits;

/// The API of one Zabbix, called with one token.
pub struct Zabbix {
    http: reqwest::Client,
    endpoint: String,
    token: String,
    id: AtomicU64,
}

impl std::fmt::Debug for Zabbix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Zabbix")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

/// A host of Zabbix, with what the add-on supplies of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub id: String,
    /// The visible name, or the technical one when it has none.
    pub name: String,
    /// The ids of its host groups.
    pub group_ids: Vec<String>,
    /// Its templates and tags, which decide its kind.
    pub traits: HostTraits,
    /// The inventory's serial number A, when the inventory is on and has one.
    pub serial_number: Option<String>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
    #[serde(default)]
    data: String,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RawGroup {
    groupid: String,
    /// Not asked for a host's groups.
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawTemplate {
    name: String,
}

#[derive(Deserialize)]
struct RawTag {
    tag: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
struct RawHost {
    hostid: String,
    host: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    hostgroups: Vec<RawGroup>,
    #[serde(default, rename = "parentTemplates")]
    parent_templates: Vec<RawTemplate>,
    #[serde(default)]
    tags: Vec<RawTag>,
    /// An object with the fields asked for, or an empty array when the inventory is off.
    #[serde(default)]
    inventory: Value,
}

impl Zabbix {
    /// The API of the Zabbix whose frontend is at `url` (such as `https://zabbix.example.com`),
    /// each call taking at most `timeout`.
    pub fn new(url: &str, token: &str, timeout: Duration) -> anyhow::Result<Zabbix> {
        Ok(Zabbix {
            http: hd_addon_sdk::http_client(timeout)?,
            endpoint: format!("{}/api_jsonrpc.php", url.trim_end_matches('/')),
            token: token.to_owned(),
            id: AtomicU64::new(1),
        })
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        authorized: bool,
    ) -> anyhow::Result<T> {
        let id = self.id.fetch_add(1, Ordering::Relaxed);
        let mut request = self
            .http
            .post(&self.endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json-rpc")
            .body(
                json!({"jsonrpc": "2.0", "method": method, "params": params, "id": id}).to_string(),
            );
        if authorized {
            request = request.bearer_auth(&self.token);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("calling {method} on {}", self.endpoint))?;
        let status = response.status();
        if !status.is_success() {
            bail!("Zabbix answered {method} with {status}");
        }
        let answer: Answer = response
            .json()
            .await
            .with_context(|| format!("reading Zabbix's answer to {method}"))?;
        match (answer.error, answer.result) {
            (Some(e), _) => bail!(
                "Zabbix refused {method}: {} {} ({})",
                e.message,
                e.data,
                e.code
            ),
            (None, Some(result)) => serde_json::from_value(result)
                .with_context(|| format!("reading Zabbix's answer to {method}")),
            (None, None) => bail!("Zabbix answered {method} with neither result nor error"),
        }
    }

    /// Calls `method` with `params`, with the token.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<T> {
        self.send(method, params, true).await
    }

    /// The API's version, such as `7.0.5`; Zabbix answers it only to a call without a token.
    pub async fn version(&self) -> anyhow::Result<String> {
        self.send("apiinfo.version", json!({}), false).await
    }

    /// Every host group the token's user can read.
    pub async fn host_groups(&self) -> anyhow::Result<Vec<HostGroup>> {
        let groups: Vec<RawGroup> = self
            .call("hostgroup.get", json!({"output": ["groupid", "name"]}))
            .await?;
        Ok(groups
            .into_iter()
            .map(|g| HostGroup {
                id: g.groupid,
                name: g.name,
            })
            .collect())
    }

    /// The hosts in any of the groups with these ids, with all their groups; none for no group.
    pub async fn hosts(&self, group_ids: &[String]) -> anyhow::Result<Vec<Host>> {
        // Without groupids, host.get answers every host.
        if group_ids.is_empty() {
            return Ok(vec![]);
        }
        let hosts: Vec<RawHost> = self
            .call(
                "host.get",
                json!({
                    "output": ["hostid", "host", "name"],
                    "groupids": group_ids,
                    "selectHostGroups": ["groupid"],
                    "selectParentTemplates": ["name"],
                    "selectTags": ["tag", "value"],
                    "selectInventory": ["serialno_a"],
                }),
            )
            .await?;
        Ok(hosts.into_iter().map(Host::from).collect())
    }
}

impl From<RawHost> for Host {
    fn from(raw: RawHost) -> Host {
        let name = match raw.name.trim() {
            "" => raw.host.trim().to_owned(),
            name => name.to_owned(),
        };
        Host {
            id: raw.hostid,
            name,
            group_ids: raw.hostgroups.into_iter().map(|g| g.groupid).collect(),
            traits: HostTraits {
                templates: raw.parent_templates.into_iter().map(|t| t.name).collect(),
                tags: raw.tags.into_iter().map(|t| (t.tag, t.value)).collect(),
            },
            serial_number: raw
                .inventory
                .get("serialno_a")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hosts as Zabbix 7.0 answers `host.get` with the fields the add-on asks for: an inventory
    /// that is on, and one that is off (an empty array).
    #[test]
    fn a_host_is_read_with_its_groups_templates_tags_and_serial_number() {
        let raw: Vec<RawHost> = serde_json::from_value(json!([
            {"hostid": "10683", "host": "srv-01", "name": "SRV-01 (Brightfield)",
             "hostgroups": [{"groupid": "22"}, {"groupid": "23"}],
             "parentTemplates": [{"name": "Linux by Zabbix agent"}],
             "tags": [{"tag": "device", "value": "server"}],
             "inventory": {"serialno_a": " SN-0042 "}},
            {"hostid": "10685", "host": "printer-02", "name": "",
             "hostgroups": [{"groupid": "22"}], "parentTemplates": [], "tags": [],
             "inventory": []}
        ]))
        .unwrap();
        let hosts: Vec<Host> = raw.into_iter().map(Host::from).collect();
        assert_eq!(
            hosts,
            [
                Host {
                    id: "10683".into(),
                    name: "SRV-01 (Brightfield)".into(),
                    group_ids: vec!["22".into(), "23".into()],
                    traits: HostTraits {
                        templates: vec!["Linux by Zabbix agent".into()],
                        tags: vec![("device".into(), "server".into())],
                    },
                    serial_number: Some("SN-0042".into()),
                },
                Host {
                    id: "10685".into(),
                    name: "printer-02".into(),
                    group_ids: vec!["22".into()],
                    traits: HostTraits::default(),
                    serial_number: None,
                },
            ]
        );
    }
}
