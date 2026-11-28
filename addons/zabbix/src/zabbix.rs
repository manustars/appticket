//! Zabbix's JSON-RPC API (`<frontend>/api_jsonrpc.php`), called with an API token in
//! `Authorization: Bearer`, as Zabbix 7.0 and later take it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::coverage::HostGroup;
use crate::kinds::HostTraits;
use crate::rules::Severity;

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

/// Zabbix answering a call with an error, such as a method the user may not call: Zabbix
/// answered, and said no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub method: String,
    pub message: String,
    pub data: String,
    pub code: i64,
}

impl Refused {
    /// Why Zabbix refused, as it says it, such as `Cannot close problem: trigger does not allow
    /// manual closing.`
    pub fn reason(&self) -> String {
        match self.data.trim() {
            "" => self.message.clone(),
            data => data.to_owned(),
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Zabbix refused {}: {} {} ({})",
            self.method, self.message, self.data, self.code
        )
    }
}

impl std::error::Error for Refused {}

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
            (Some(e), _) => Err(Refused {
                method: method.to_owned(),
                message: e.message,
                data: e.data,
                code: e.code,
            }
            .into()),
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

impl RawHost {
    /// The visible name, or the technical one when it has none.
    fn visible_name(&self) -> String {
        match self.name.trim() {
            "" => self.host.trim().to_owned(),
            name => name.to_owned(),
        }
    }
}

impl From<RawHost> for Host {
    fn from(raw: RawHost) -> Host {
        let name = raw.visible_name();
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

/// Zabbix's event of an alert: a problem event of a trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    /// Its current severity, which an update in Zabbix may have changed from the trigger's.
    pub severity: Severity,
    /// The event's name: the trigger's name, or its event name when it has one, with the macros
    /// resolved when the problem started.
    pub name: String,
    /// The event's tags, as name and value: the trigger's, the items' and the host's.
    pub tags: Vec<(String, String)>,
    /// The ids of the hosts of the trigger's items; usually one.
    pub host_ids: Vec<String>,
    /// When the problem started, in seconds since the Unix epoch.
    pub clock: i64,
    /// The trigger's operational data, with its macros resolved when the problem started; empty
    /// when the trigger has none.
    pub operational_data: String,
    /// The id of the event that recovered the problem; none while it is open.
    pub recovery_id: Option<String>,
}

/// The recovery event of a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEvent {
    /// When it happened, in seconds since the Unix epoch.
    pub clock: i64,
    /// The id of the user who closed the problem by hand; none when it recovered by itself.
    pub closed_by: Option<String>,
}

/// The Zabbix user the add-on calls Zabbix as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnUser {
    pub id: String,
    pub username: String,
}

/// An update a user made of a problem, as `event.acknowledge` makes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acknowledgement {
    pub user_id: String,
    /// Empty when it has none.
    pub message: String,
}

/// The bits of `event.acknowledge`'s `action`.
const CLOSE: u32 = 1;
const ACKNOWLEDGE: u32 = 2;
const ADD_MESSAGE: u32 = 4;

/// A host by its visible name, with its host groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostWithGroups {
    pub id: String,
    /// The visible name, or the technical one when it has none.
    pub name: String,
    pub groups: Vec<HostGroup>,
}

/// An event as `event.get` answers it; Zabbix's numbers come as strings, such as `"4"`.
#[derive(Deserialize)]
struct RawEvent {
    eventid: String,
    value: String,
    severity: String,
    name: String,
    clock: String,
    #[serde(default)]
    opdata: String,
    /// `0` while the problem is open.
    #[serde(default)]
    r_eventid: String,
    #[serde(default)]
    tags: Vec<RawTag>,
    #[serde(default)]
    hosts: Vec<RawHost>,
}

impl RawEvent {
    fn read(self) -> anyhow::Result<Event> {
        if self.value != "1" {
            bail!(
                "Zabbix's event {} is the recovery of a problem: give the event id of the problem",
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
                    "Zabbix's event {} has the severity {:?}, which is none of Zabbix's",
                    self.eventid, self.severity
                )
            })?;
        let clock = self.clock.parse().with_context(|| {
            format!(
                "Zabbix's event {} started at {:?}, which is no time",
                self.eventid, self.clock
            )
        })?;
        Ok(Event {
            id: self.eventid,
            severity,
            name: self.name,
            tags: self.tags.into_iter().map(|t| (t.tag, t.value)).collect(),
            host_ids: self.hosts.into_iter().map(|h| h.hostid).collect(),
            clock,
            operational_data: self.opdata.trim().to_owned(),
            recovery_id: Some(self.r_eventid).filter(|id| !id.is_empty() && id != "0"),
        })
    }
}

/// How Zabbix's sending of a message alert stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertStatus {
    Sent,
    /// Failed, after its retries.
    Failed,
    /// Not sent yet, or not yet processed: Zabbix is still trying.
    Underway,
}

impl AlertStatus {
    /// From `alert.get`'s `status` of a message alert: 0 not sent, 1 sent, 2 failed, 3 new.
    fn from_number(status: &str) -> Option<AlertStatus> {
        match status {
            "1" => Some(AlertStatus::Sent),
            "2" => Some(AlertStatus::Failed),
            "0" | "3" => Some(AlertStatus::Underway),
            _ => None,
        }
    }
}

/// A message alert as `alert.get` answers it with the fields the add-on asks for.
#[derive(Deserialize)]
struct RawAlert {
    /// The event that set the action's operation off: a recovery's alert has the recovery event.
    eventid: String,
    status: String,
    /// `0` for a message, `1` for a remote command.
    #[serde(default)]
    alerttype: String,
}

/// The statuses of the message alerts, by the event that set them off.
fn statuses(alerts: Vec<RawAlert>) -> HashMap<String, Vec<AlertStatus>> {
    let mut by_event: HashMap<String, Vec<AlertStatus>> = HashMap::new();
    for alert in alerts {
        if !matches!(alert.alerttype.as_str(), "" | "0") {
            continue;
        }
        if let Some(status) = AlertStatus::from_number(&alert.status) {
            by_event.entry(alert.eventid).or_default().push(status);
        }
    }
    by_event
}

/// What the add-on's Zabbix user reads of the alerts Zabbix sent through the webhook's media
/// type: since Zabbix 7.0.9, a user that is not a Super admin reads only the message alerts sent
/// to itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertAccess {
    pub super_admin: bool,
    /// The user has a media of the webhook's media type, so that the alerts sent to it are its
    /// own.
    pub has_the_media: bool,
}

impl AlertAccess {
    pub fn reads_the_webhook_s_alerts(self) -> bool {
        self.super_admin || self.has_the_media
    }
}

/// The user type of `user.checkAuthentication`: a number, 3 for a Super admin.
fn super_admin(user_type: &Value) -> bool {
    match user_type {
        Value::Number(n) => n.as_u64() == Some(3),
        Value::String(s) => s == "3",
        _ => false,
    }
}

/// A problem as `problem.get` answers it with the fields the add-on asks for.
#[derive(Deserialize)]
struct RawProblem {
    eventid: String,
    /// `1` while suppressed.
    #[serde(default)]
    suppressed: String,
    /// The problem it is a symptom of; `0` for a cause.
    #[serde(default)]
    cause_eventid: String,
}

/// The event ids of the problems that are neither suppressed nor symptoms.
fn active(problems: Vec<RawProblem>) -> Vec<String> {
    problems
        .into_iter()
        .filter(|p| p.suppressed != "1" && matches!(p.cause_eventid.as_str(), "" | "0"))
        .map(|p| p.eventid)
        .collect()
}

impl Zabbix {
    /// The id of the media type with this name, when the add-on's user reads it.
    pub async fn media_type_id(&self, name: &str) -> anyhow::Result<Option<String>> {
        #[derive(Deserialize)]
        struct MediaType {
            mediatypeid: String,
        }
        let found: Vec<MediaType> = self
            .call(
                "mediatype.get",
                json!({"output": ["mediatypeid"], "filter": {"name": [name]}}),
            )
            .await?;
        Ok(found.into_iter().next().map(|m| m.mediatypeid))
    }

    /// What the add-on's user reads of the alerts of the media type `media_type`.
    pub async fn alert_access(&self, media_type: &str) -> anyhow::Result<AlertAccess> {
        #[derive(Deserialize)]
        struct Me {
            userid: String,
            #[serde(rename = "type", default)]
            user_type: Value,
        }
        #[derive(Deserialize)]
        struct Media {
            mediatypeid: String,
        }
        #[derive(Deserialize)]
        struct User {
            #[serde(default)]
            medias: Vec<Media>,
        }
        // Zabbix answers it only to a call without a token in the header.
        let me: Me = self
            .send(
                "user.checkAuthentication",
                json!({"token": self.token}),
                false,
            )
            .await?;
        if super_admin(&me.user_type) {
            return Ok(AlertAccess {
                super_admin: true,
                has_the_media: false,
            });
        }
        let users: Vec<User> = self
            .call(
                "user.get",
                json!({"userids": [me.userid], "output": ["userid"],
                       "selectMedias": ["mediatypeid"]}),
            )
            .await?;
        Ok(AlertAccess {
            super_admin: false,
            has_the_media: users
                .iter()
                .flat_map(|u| &u.medias)
                .any(|m| m.mediatypeid == media_type),
        })
    }

    /// The statuses of the message alerts of the media type `media_type` that these events set
    /// off, such as recovery events, by event id; an event without any is left out.
    pub async fn alert_statuses(
        &self,
        events: &[String],
        media_type: &str,
    ) -> anyhow::Result<HashMap<String, Vec<AlertStatus>>> {
        if events.is_empty() {
            return Ok(HashMap::new());
        }
        let alerts: Vec<RawAlert> = self
            .call(
                "alert.get",
                json!({"eventids": events, "mediatypeids": [media_type],
                       "output": ["eventid", "status", "alerttype"]}),
            )
            .await?;
        Ok(statuses(alerts))
    }

    /// Whether the add-on's user reads any alert of the media type `media_type` at all.
    pub async fn any_alert(&self, media_type: &str) -> anyhow::Result<bool> {
        let alerts: Vec<Value> = self
            .call(
                "alert.get",
                json!({"mediatypeids": [media_type], "output": ["alertid"], "limit": 1}),
            )
            .await?;
        Ok(!alerts.is_empty())
    }
}

impl From<RawHost> for HostWithGroups {
    fn from(raw: RawHost) -> HostWithGroups {
        HostWithGroups {
            name: raw.visible_name(),
            id: raw.hostid,
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

impl Zabbix {
    /// The problem event of a trigger with this id.
    pub async fn problem_event(&self, id: &str) -> anyhow::Result<Event> {
        match self.find_problem_event(id).await? {
            Some(event) => Ok(event),
            None => bail!(
                "Zabbix has no event {id} of a trigger's problem that the add-on's user can read"
            ),
        }
    }

    /// The problem event of a trigger with this id; none when Zabbix has none that the add-on's
    /// user can read, such as one its housekeeper removed.
    pub async fn find_problem_event(&self, id: &str) -> anyhow::Result<Option<Event>> {
        self.readable_problem_events(&[id.to_owned()])
            .await?
            .into_iter()
            .next()
            .transpose()
    }

    /// The problem events of triggers with these ids that the add-on's user can read, each read
    /// on its own, so that one the add-on cannot read as a problem leaves the others; none for no
    /// id.
    pub async fn readable_problem_events(
        &self,
        ids: &[String],
    ) -> anyhow::Result<Vec<anyhow::Result<Event>>> {
        // Without eventids, event.get answers every event.
        if ids.is_empty() {
            return Ok(vec![]);
        }
        // event.get answers the events of triggers only, unless asked for another source.
        let events: Vec<RawEvent> = self
            .call(
                "event.get",
                json!({
                    "eventids": ids,
                    "output": ["eventid", "value", "severity", "name", "clock", "opdata",
                               "r_eventid"],
                    "selectTags": ["tag", "value"],
                    "selectHosts": ["hostid", "host", "name"],
                }),
            )
            .await?;
        Ok(events.into_iter().map(RawEvent::read).collect())
    }

    /// The event ids of the problems of triggers open now on the hosts of the groups with these
    /// ids, in id order; none for no group. Left out are those a trigger action pauses its
    /// operations for by default, so that the webhook never had them: suppressed problems (in a
    /// maintenance, or suppressed by hand) and symptoms of another problem.
    pub async fn active_problems(&self, group_ids: &[String]) -> anyhow::Result<Vec<String>> {
        // Without groupids, problem.get answers the problems of every host.
        if group_ids.is_empty() {
            return Ok(vec![]);
        }
        let problems: Vec<RawProblem> = self
            .call(
                "problem.get",
                json!({
                    "groupids": group_ids,
                    "source": 0,
                    "object": 0,
                    "recent": false,
                    "output": ["eventid", "suppressed", "cause_eventid"],
                    "sortfield": ["eventid"],
                }),
            )
            .await?;
        Ok(active(problems))
    }

    /// The recovery event of a problem with this id: when it happened and, for a problem closed
    /// by hand, who closed it.
    pub async fn recovery_event(&self, id: &str) -> anyhow::Result<RecoveryEvent> {
        #[derive(Deserialize)]
        struct Raw {
            clock: String,
            #[serde(default)]
            userid: String,
        }
        let events: Vec<Raw> = self
            .call(
                "event.get",
                json!({"eventids": [id], "output": ["clock", "userid"]}),
            )
            .await?;
        let Raw { clock, userid } = events
            .into_iter()
            .next()
            .with_context(|| format!("Zabbix has no event {id} that the add-on's user can read"))?;
        let clock = clock.parse().with_context(|| {
            format!("Zabbix's event {id} happened at {clock:?}, which is no time")
        })?;
        Ok(RecoveryEvent {
            clock,
            // 0 when the trigger's expression recovered it rather than a user.
            closed_by: Some(userid).filter(|id| !id.is_empty() && id != "0"),
        })
    }

    /// The Zabbix user whose API token the add-on calls Zabbix with.
    pub async fn own_user(&self) -> anyhow::Result<OwnUser> {
        #[derive(Deserialize)]
        struct User {
            userid: String,
            username: String,
        }
        // Zabbix answers it only to a call without a token in the header.
        let user: User = self
            .send(
                "user.checkAuthentication",
                json!({"token": self.token}),
                false,
            )
            .await?;
        Ok(OwnUser {
            id: user.userid,
            username: user.username,
        })
    }

    /// Whether the trigger of the problem event with this id allows closing its problems by hand
    /// (its `manual_close`).
    pub async fn allows_manual_close(&self, event: &str) -> anyhow::Result<bool> {
        #[derive(Deserialize)]
        struct Trigger {
            manual_close: String,
        }
        #[derive(Deserialize)]
        struct Related {
            #[serde(rename = "relatedObject")]
            related: Trigger,
        }
        let events: Vec<Related> = self
            .call(
                "event.get",
                json!({"eventids": [event], "output": ["eventid"],
                       "selectRelatedObject": ["triggerid", "manual_close"]}),
            )
            .await?;
        let event = events.into_iter().next().with_context(|| {
            format!("Zabbix has no event {event} that the add-on's user can read")
        })?;
        Ok(event.related.manual_close == "1")
    }

    /// The updates users made of the problem event with this id, such as acknowledgements.
    pub async fn acknowledgements(&self, event: &str) -> anyhow::Result<Vec<Acknowledgement>> {
        #[derive(Deserialize)]
        struct Raw {
            userid: String,
            #[serde(default)]
            message: String,
        }
        #[derive(Deserialize)]
        struct WithAcknowledges {
            #[serde(default)]
            acknowledges: Vec<Raw>,
        }
        let events: Vec<WithAcknowledges> = self
            .call(
                "event.get",
                json!({"eventids": [event], "output": ["eventid"],
                       "selectAcknowledges": ["userid", "message"]}),
            )
            .await?;
        Ok(events
            .into_iter()
            .flat_map(|e| e.acknowledges)
            .map(|a| Acknowledgement {
                user_id: a.userid,
                message: a.message,
            })
            .collect())
    }

    /// Acknowledges the problem event with this id with `message`, closing the problem when
    /// `close` says so, which its trigger must allow.
    pub async fn acknowledge(&self, event: &str, close: bool, message: &str) -> anyhow::Result<()> {
        let mut action = ACKNOWLEDGE | ADD_MESSAGE;
        if close {
            action |= CLOSE;
        }
        let _: Value = self
            .call(
                "event.acknowledge",
                json!({"eventids": [event], "action": action, "message": message}),
            )
            .await?;
        Ok(())
    }

    /// The hosts that `filter` selects, such as `{"hostids": [...]}`, with their groups.
    async fn hosts_with_groups(&self, filter: Value) -> anyhow::Result<Vec<HostWithGroups>> {
        let mut params = filter;
        params["output"] = json!(["hostid", "host", "name"]);
        params["selectHostGroups"] = json!(["groupid", "name"]);
        let hosts: Vec<RawHost> = self.call("host.get", params).await?;
        Ok(hosts.into_iter().map(HostWithGroups::from).collect())
    }

    /// The hosts with these ids, with their groups.
    pub async fn hosts_by_id(&self, ids: &[String]) -> anyhow::Result<Vec<HostWithGroups>> {
        // Without hostids, host.get answers every host.
        if ids.is_empty() {
            return Ok(vec![]);
        }
        self.hosts_with_groups(json!({"hostids": ids})).await
    }

    /// The host with this technical name, else with this visible name, with its groups; each is
    /// unique in Zabbix.
    pub async fn host_by_name(&self, name: &str) -> anyhow::Result<HostWithGroups> {
        for field in ["host", "name"] {
            let hosts = self
                .hosts_with_groups(json!({"filter": {field: [name]}}))
                .await?;
            if let Some(host) = hosts.into_iter().next() {
                return Ok(host);
            }
        }
        bail!("Zabbix has no host named {name:?} that the add-on's user can read")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An event as Zabbix 7.0 answers `event.get` with the fields the add-on asks for.
    #[test]
    fn a_problem_event_is_read_with_its_severity_name_tags_and_hosts() {
        let raw: RawEvent = serde_json::from_value(json!(
            {"eventid": "4213", "value": "1", "severity": "4",
             "name": "Mail queue of SRV-01 is too long", "clock": "1795801516",
             "opdata": "Queue: 1200", "r_eventid": "0",
             "tags": [{"tag": "service", "value": "mail"}, {"tag": "scope", "value": ""}],
             "hosts": [{"hostid": "10683", "host": "srv-01", "name": "SRV-01"}]}
        ))
        .unwrap();
        assert_eq!(
            raw.read().unwrap(),
            Event {
                id: "4213".into(),
                severity: Severity::High,
                name: "Mail queue of SRV-01 is too long".into(),
                tags: vec![
                    ("service".into(), "mail".into()),
                    ("scope".into(), String::new())
                ],
                host_ids: vec!["10683".into()],
                clock: 1_795_801_516,
                operational_data: "Queue: 1200".into(),
                recovery_id: None,
            }
        );
        // Recovered: the id of its recovery event.
        let recovered: RawEvent = serde_json::from_value(json!(
            {"eventid": "4213", "value": "1", "severity": "4", "name": "Disk full",
             "clock": "1795801516", "opdata": "", "r_eventid": "4220"}
        ))
        .unwrap();
        assert_eq!(
            recovered.read().unwrap().recovery_id.as_deref(),
            Some("4220")
        );
    }

    /// Alerts as Zabbix 7.0 answers `alert.get` with the fields the add-on asks for.
    #[test]
    fn the_alerts_statuses_are_read_by_their_event() {
        let raw: Vec<RawAlert> = serde_json::from_value(json!([
            {"eventid": "4220", "status": "2", "alerttype": "0"},
            {"eventid": "4220", "status": "1", "alerttype": "0"},
            {"eventid": "4221", "status": "3", "alerttype": "0"},
            {"eventid": "4222", "status": "0", "alerttype": "0"},
            {"eventid": "4223", "status": "1", "alerttype": "1"},
            {"eventid": "4224", "status": "9", "alerttype": "0"}
        ]))
        .unwrap();
        assert_eq!(
            statuses(raw),
            HashMap::from([
                (
                    "4220".to_owned(),
                    vec![AlertStatus::Failed, AlertStatus::Sent]
                ),
                ("4221".to_owned(), vec![AlertStatus::Underway]),
                ("4222".to_owned(), vec![AlertStatus::Underway]),
            ]),
            "a remote command's and an unknown status are left out"
        );
    }

    #[test]
    fn a_super_admin_reads_every_alert_and_another_user_those_of_its_own_media() {
        assert!(super_admin(&json!(3)) && super_admin(&json!("3")));
        for other in [json!(1), json!(2), json!("2"), Value::Null] {
            assert!(!super_admin(&other), "{other}");
        }
        let access = |super_admin, has_the_media| AlertAccess {
            super_admin,
            has_the_media,
        };
        assert!(access(true, false).reads_the_webhook_s_alerts());
        assert!(access(false, true).reads_the_webhook_s_alerts());
        assert!(!access(false, false).reads_the_webhook_s_alerts());
    }

    /// Problems as Zabbix 7.0 answers `problem.get` with the fields the add-on asks for.
    #[test]
    fn the_active_problems_leave_out_the_suppressed_ones_and_the_symptoms() {
        let raw: Vec<RawProblem> = serde_json::from_value(json!([
            {"eventid": "4213", "suppressed": "0", "cause_eventid": "0"},
            {"eventid": "4214", "suppressed": "1", "cause_eventid": "0"},
            {"eventid": "4215", "suppressed": "0", "cause_eventid": "4213"},
            {"eventid": "4216", "suppressed": "0", "cause_eventid": "0"}
        ]))
        .unwrap();
        assert_eq!(active(raw), ["4213", "4216"]);
    }

    #[test]
    fn a_recovery_event_is_no_alert() {
        let raw: RawEvent = serde_json::from_value(json!(
            {"eventid": "4220", "value": "0", "severity": "0", "name": "Mail queue is too long",
             "clock": "1795801522", "opdata": "", "r_eventid": "0", "tags": [], "hosts": []}
        ))
        .unwrap();
        assert_eq!(
            raw.read().unwrap_err().to_string(),
            "Zabbix's event 4220 is the recovery of a problem: give the event id of the problem"
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
