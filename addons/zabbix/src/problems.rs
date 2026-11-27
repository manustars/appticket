//! Problems as Zabbix's API gives them: a problem event by its id (`event.get`), and the host it
//! is about with its host groups (`host.get`), which the rules and the coverage of host groups
//! need.

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::json;

use crate::coverage::HostGroup;
use crate::rules::Severity;
use crate::zabbix::Zabbix;

/// A problem event of a trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemEvent {
    pub id: String,
    /// Its current severity, which an update in Zabbix may have changed from the trigger's.
    pub severity: Severity,
    /// The event's name: the trigger's name, or its event name when it has one, with the macros
    /// resolved when the problem started.
    pub name: String,
    pub tags: Vec<(String, String)>,
    /// The ids of the hosts of the trigger's items; usually one.
    pub host_ids: Vec<String>,
}

/// A host, with its host groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostWithGroups {
    pub id: String,
    /// The visible name, or the technical one when it has none.
    pub name: String,
    pub groups: Vec<HostGroup>,
}

/// Zabbix's numbers come as strings, such as `"4"`.
#[derive(Deserialize)]
struct RawEvent {
    eventid: String,
    value: String,
    severity: String,
    name: String,
    #[serde(default)]
    tags: Vec<RawTag>,
    #[serde(default)]
    hosts: Vec<RawHostId>,
}

#[derive(Deserialize)]
struct RawTag {
    tag: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
struct RawHostId {
    hostid: String,
}

#[derive(Deserialize)]
struct RawHost {
    hostid: String,
    host: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    hostgroups: Vec<RawGroup>,
}

#[derive(Deserialize)]
struct RawGroup {
    groupid: String,
    name: String,
}

impl RawEvent {
    fn read(self) -> anyhow::Result<ProblemEvent> {
        if self.value != "1" {
            bail!(
                "event {} is the recovery of a problem, not a problem: give the problem's event id",
                self.eventid
            );
        }
        let severity = self
            .severity
            .parse()
            .ok()
            .and_then(Severity::from_number)
            .with_context(|| {
                format!(
                    "event {} has the severity {:?}, which is none of Zabbix's",
                    self.eventid, self.severity
                )
            })?;
        Ok(ProblemEvent {
            id: self.eventid,
            severity,
            name: self.name,
            tags: self.tags.into_iter().map(|t| (t.tag, t.value)).collect(),
            host_ids: self.hosts.into_iter().map(|h| h.hostid).collect(),
        })
    }
}

impl From<RawHost> for HostWithGroups {
    fn from(raw: RawHost) -> HostWithGroups {
        let name = match raw.name.trim() {
            "" => raw.host.trim().to_owned(),
            name => name.to_owned(),
        };
        HostWithGroups {
            id: raw.hostid,
            name,
            groups: raw
                .hostgroups
                .into_iter()
                .map(|g| HostGroup {
                    id: g.groupid,
                    name: g.name,
                })
                .collect(),
        }
    }
}

const HOST_OUTPUT: [&str; 3] = ["hostid", "host", "name"];

/// The problem event with this id, of a trigger.
pub async fn problem_event(zabbix: &Zabbix, id: &str) -> anyhow::Result<ProblemEvent> {
    // event.get answers the events of triggers only, unless asked for another source.
    let events: Vec<RawEvent> = zabbix
        .call(
            "event.get",
            json!({
                "eventids": [id],
                "output": ["eventid", "value", "severity", "name"],
                "selectTags": ["tag", "value"],
                "selectHosts": ["hostid"],
            }),
        )
        .await?;
    match events.into_iter().next() {
        Some(event) => event.read(),
        None => {
            bail!("Zabbix has no problem event {id} of a trigger that the add-on's user can read")
        }
    }
}

/// The hosts with these ids, with their groups.
pub async fn hosts_by_id(zabbix: &Zabbix, ids: &[String]) -> anyhow::Result<Vec<HostWithGroups>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let hosts: Vec<RawHost> = zabbix
        .call(
            "host.get",
            json!({
                "hostids": ids,
                "output": HOST_OUTPUT,
                "selectHostGroups": ["groupid", "name"],
            }),
        )
        .await?;
    Ok(hosts.into_iter().map(HostWithGroups::from).collect())
}

/// The host with this technical name, else with this visible name, with its groups; each is
/// unique in Zabbix.
pub async fn host_by_name(zabbix: &Zabbix, name: &str) -> anyhow::Result<HostWithGroups> {
    for field in ["host", "name"] {
        let hosts: Vec<RawHost> = zabbix
            .call(
                "host.get",
                json!({
                    "filter": {field: [name]},
                    "output": HOST_OUTPUT,
                    "selectHostGroups": ["groupid", "name"],
                }),
            )
            .await?;
        if let Some(host) = hosts.into_iter().next() {
            return Ok(host.into());
        }
    }
    bail!("Zabbix has no host named {name:?} that the add-on's user can read")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An event as Zabbix 7.0 answers `event.get` with the fields the add-on asks for.
    #[test]
    fn a_problem_event_is_read_with_its_severity_name_tags_and_hosts() {
        let raw: Vec<RawEvent> = serde_json::from_value(json!([
            {"eventid": "4213", "value": "1", "severity": "4",
             "name": "Mail queue of SRV-01 is too long",
             "tags": [{"tag": "service", "value": "mail"}, {"tag": "scope", "value": ""}],
             "hosts": [{"hostid": "10683"}]}
        ]))
        .unwrap();
        let event = raw.into_iter().next().unwrap().read().unwrap();
        assert_eq!(
            event,
            ProblemEvent {
                id: "4213".into(),
                severity: Severity::High,
                name: "Mail queue of SRV-01 is too long".into(),
                tags: vec![
                    ("service".into(), "mail".into()),
                    ("scope".into(), String::new())
                ],
                host_ids: vec!["10683".into()],
            }
        );
    }

    #[test]
    fn a_recovery_event_is_no_problem() {
        let raw: RawEvent = serde_json::from_value(json!(
            {"eventid": "4220", "value": "0", "severity": "0", "name": "Mail queue is too long",
             "tags": [], "hosts": [{"hostid": "10683"}]}
        ))
        .unwrap();
        assert_eq!(
            raw.read().unwrap_err().to_string(),
            "event 4220 is the recovery of a problem, not a problem: give the problem's event id"
        );
    }

    #[test]
    fn a_host_is_read_with_its_groups_and_its_visible_name() {
        let raw: Vec<RawHost> = serde_json::from_value(json!([
            {"hostid": "10683", "host": "srv-01", "name": "SRV-01 (Brightfield)",
             "hostgroups": [{"groupid": "22", "name": "Brightfield/Offices"}]},
            {"hostid": "10685", "host": "printer-02", "name": "", "hostgroups": []}
        ]))
        .unwrap();
        let hosts: Vec<HostWithGroups> = raw.into_iter().map(HostWithGroups::from).collect();
        assert_eq!(
            hosts,
            [
                HostWithGroups {
                    id: "10683".into(),
                    name: "SRV-01 (Brightfield)".into(),
                    groups: vec![HostGroup {
                        id: "22".into(),
                        name: "Brightfield/Offices".into()
                    }],
                },
                HostWithGroups {
                    id: "10685".into(),
                    name: "printer-02".into(),
                    groups: vec![],
                },
            ]
        );
    }
}
