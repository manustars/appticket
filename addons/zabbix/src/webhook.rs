//! The add-on's webhook, where Zabbix's media type (`zabbix-media-type.yaml`) sends the problem,
//! recovery and update events of triggers, with the webhook's secret in `Authorization: Bearer`.
//! The add-on acts on each within the request and answers the tags Zabbix adds to the problem:
//! `hd_ticket` with the number of the ticket a problem opened.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use hd_addon_sdk::Client;
use hd_addon_sdk::api::{Asset, Ticket};
use serde::Serialize;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::alerts::{
    Notice, Opening, Payload, Service, TICKET_TAG, new_ticket, recovery_note, resolves, update_note,
};
use crate::coverage::{Coverage, Placement};
use crate::rules::{Alert, Outcome, Recovery, Rules};
use crate::sync;
use crate::text::same;
use crate::zabbix::{Event, HostWithGroups, Zabbix};

/// Where Zabbix's media type sends its events, on `addon.listen`.
pub const PATH: &str = "/webhook";

/// How long the add-on takes at most to act on an event: less than the media type's timeout
/// (30 seconds), so that Zabbix gets an answer, and tries again when it says the add-on failed.
pub const DEADLINE: Duration = Duration::from_secs(20);

/// Whether the `Authorization` header carries the webhook's secret, compared in constant time.
pub fn authorized(authorization: Option<&str>, secret: &str) -> bool {
    let Some(given) = authorization.and_then(|value| value.strip_prefix("Bearer ")) else {
        return false;
    };
    let (given, secret) = (given.trim().as_bytes(), secret.as_bytes());
    given.len() == secret.len()
        && given
            .iter()
            .zip(secret)
            .fold(0u8, |differ, (a, b)| differ | (a ^ b))
            == 0
}

/// What the add-on answers Zabbix: the tags the media type adds to the problem.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Answer {
    pub tags: BTreeMap<String, String>,
}

impl Answer {
    fn ticket(number: i64) -> Answer {
        Answer {
            tags: BTreeMap::from([(TICKET_TAG.to_owned(), number.to_string())]),
        }
    }
}

/// What the add-on does with Zabbix's events, between a Zabbix and the core.
pub struct Alerts {
    core: Client,
    zabbix: Zabbix,
    /// The system's name as external references name it.
    system: String,
    rules: Rules,
    /// The username of the add-on's own Zabbix user, asked once.
    own_username: OnceCell<String>,
}

impl std::fmt::Debug for Alerts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Alerts")
            .field("zabbix", &self.zabbix)
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

impl Alerts {
    pub fn new(core: Client, zabbix: Zabbix, system: impl Into<String>, rules: Rules) -> Alerts {
        Alerts {
            core,
            zabbix,
            system: system.into(),
            rules,
            own_username: OnceCell::new(),
        }
    }

    /// Acts on one event and answers the tags for Zabbix.
    pub async fn receive(&self, notice: Notice) -> anyhow::Result<Answer> {
        match notice {
            Notice::Problem { event } => self.problem(&event).await,
            Notice::Recovery { event } => self.recovery(&event).await.map(|()| Answer::default()),
            Notice::Update {
                event,
                user,
                action,
                message,
            } => self
                .update(
                    &event,
                    user.as_deref(),
                    action.as_deref(),
                    message.as_deref(),
                )
                .await
                .map(|()| Answer::default()),
        }
    }

    /// The hosts of the event, each with the customer whose asset it is, by id and name, when it
    /// is one; a host that is not is logged with the reason.
    async fn hosts(
        &self,
        event: &Event,
    ) -> anyhow::Result<Vec<(HostWithGroups, Option<(Uuid, String)>)>> {
        let hosts = self.zabbix.hosts_by_id(&event.host_ids).await?;
        if hosts.is_empty() {
            tracing::warn!(
                event = %event.id,
                "the trigger of the problem is on no host the add-on's user can read: no ticket"
            );
        }
        let groups = self.zabbix.host_groups().await?;
        let customers = self
            .core
            .customers()
            .await
            .context("reading the customers")?;
        let offboarded = self
            .core
            .offboarded_references()
            .await
            .context("reading the offboarded customers' external references")?;
        let coverage = Coverage::new(&self.system, &groups, &customers, &offboarded);
        Ok(hosts
            .into_iter()
            .map(|host| {
                let customer = match coverage.place(host.groups.iter().map(|g| g.id.as_str())) {
                    Placement::Customer(id) => customers
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| (c.id, c.name.clone())),
                    Placement::Uncovered => {
                        tracing::info!(event = %event.id, host = %host.name,
                            "the host is not imported, since no customer covers its groups: no ticket");
                        None
                    }
                    Placement::Offboarded => {
                        tracing::info!(event = %event.id, host = %host.name,
                            "the host is not imported, since an offboarded customer covers its \
                             groups: no ticket");
                        None
                    }
                    Placement::Conflict(_) => {
                        tracing::info!(event = %event.id, host = %host.name,
                            "the host is not imported, since two customers or more cover its \
                             groups: no ticket");
                        None
                    }
                };
                (host, customer)
            })
            .collect())
    }

    /// A problem: the first host of the event that is an asset of a customer and whose alert a
    /// rule opens gets its ticket, which a second time is the same one.
    async fn problem(&self, id: &str) -> anyhow::Result<Answer> {
        let event = self.zabbix.problem_event(id).await?;
        for (host, customer) in self.hosts(&event).await? {
            let Some((customer, customer_name)) = customer else {
                continue;
            };
            let alert = alert(&event, &host, &customer_name);
            let Some(matched) = self.rules.decide(&alert) else {
                tracing::info!(event = %event.id, host = %host.name,
                    "no rule matches the alert: it is ignored");
                continue;
            };
            let Outcome::Open(open) = &matched.rule.outcome else {
                tracing::info!(event = %event.id, host = %host.name, rule = %matched.label(),
                    "the rule ignores the alert");
                continue;
            };
            let reference = sync::reference(&host.id, customer);
            let assets = self
                .core
                .customer_assets(customer)
                .await
                .context("reading the customer's assets")?;
            let asset = assets
                .iter()
                .find(|a: &&Asset| a.reference.as_deref() == Some(reference.as_str()))
                .map(|a| a.id);
            let service = match &open.service {
                None => Service::None,
                Some(name) => {
                    let services = self
                        .core
                        .customer_services(customer)
                        .await
                        .context("reading the services the customer uses")?;
                    match services.iter().find(|s| same(&s.name, name)) {
                        Some(service) => Service::Used(service.id),
                        None => Service::NotUsed(name.clone()),
                    }
                }
            };
            let ticket = new_ticket(&Opening {
                event: &event,
                host: &host,
                customer,
                customer_name: &customer_name,
                asset,
                rule: &matched,
                priority: open.priority,
                service,
            });
            let opened = self
                .core
                .open_ticket(&ticket)
                .await
                .context("opening the ticket")?;
            tracing::info!(event = %event.id, host = %host.name, rule = %matched.label(),
                ticket = opened.number, created = opened.created, "the problem has its ticket");
            return Ok(Answer::ticket(opened.number));
        }
        Ok(Answer::default())
    }

    /// The ticket the add-on opened for the problem with this event id; none when it opened none.
    async fn ticket(&self, id: &str) -> anyhow::Result<Option<Ticket>> {
        match self.core.ticket_by_reference(id).await {
            Ok(ticket) => Ok(Some(ticket)),
            Err(e) if e.status() == Some(404) => Ok(None),
            Err(e) => Err(e).context("reading the problem's ticket"),
        }
    }

    /// A recovery: a note with its time, then resolving the ticket as the rule says, reading it
    /// just before.
    async fn recovery(&self, id: &str) -> anyhow::Result<()> {
        let Some(ticket) = self.ticket(id).await? else {
            tracing::info!(event = %id, "the problem recovered, and has no ticket");
            return Ok(());
        };
        let (recovered_at, on_recovery) = match self.zabbix.problem_event(id).await {
            Ok(event) => {
                let recovered_at = match &event.recovery_id {
                    Some(recovery) => self.zabbix.event_clock(recovery).await.ok(),
                    None => None,
                };
                (recovered_at, self.on_recovery(&event, &ticket).await)
            }
            Err(e) => {
                tracing::warn!(event = %id, error = format!("{e:#}"),
                    "reading the problem from Zabbix: the note has no time, and the default \
                     decides whether to resolve the ticket");
                (None, Recovery::default())
            }
        };
        self.core
            .add_note(ticket.number, &recovery_note(recovered_at))
            .await
            .context("adding the recovery's note")?;
        // An agent may have picked the ticket up since it was opened.
        let now = self
            .core
            .ticket(ticket.number)
            .await
            .context("reading the ticket before resolving it")?;
        if resolves(on_recovery, now.state, now.worked_on_by_agent) {
            self.core
                .resolve_ticket(ticket.number)
                .await
                .context("resolving the ticket")?;
            tracing::info!(event = %id, ticket = ticket.number, "the problem recovered: ticket resolved");
        } else {
            tracing::info!(event = %id, ticket = ticket.number, on_recovery = on_recovery.name(),
                state = ?now.state, worked_on_by_agent = now.worked_on_by_agent,
                "the problem recovered: ticket left as it is");
        }
        Ok(())
    }

    /// What the rule that opens the alert now, for the ticket's customer, says on recovery; the
    /// default when none does, such as after the rules changed.
    async fn on_recovery(&self, event: &Event, ticket: &Ticket) -> Recovery {
        let hosts = match self.zabbix.hosts_by_id(&event.host_ids).await {
            Ok(hosts) => hosts,
            Err(e) => {
                tracing::warn!(event = %event.id, error = format!("{e:#}"),
                    "reading the problem's hosts: the default decides");
                return Recovery::default();
            }
        };
        hosts
            .iter()
            .find_map(|host| {
                let alert = alert(event, host, &ticket.customer.name);
                match &self.rules.decide(&alert)?.rule.outcome {
                    Outcome::Open(open) => Some(open.on_recovery),
                    Outcome::Ignore => None,
                }
            })
            .unwrap_or_default()
    }

    /// An update: a note with the user and the message, unless the add-on's own user made it or
    /// it has no message.
    async fn update(
        &self,
        id: &str,
        user: Option<&str>,
        action: Option<&str>,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        let own = self
            .own_username
            .get_or_try_init(|| self.zabbix.username())
            .await
            .context("asking Zabbix who the add-on's user is")?;
        let Some(note) = update_note(user, action, message, own) else {
            tracing::debug!(event = %id, "an update without a message, or the add-on's own: no note");
            return Ok(());
        };
        let Some(ticket) = self.ticket(id).await? else {
            tracing::info!(event = %id, "the problem was updated, and has no ticket");
            return Ok(());
        };
        self.core
            .add_note(ticket.number, &note)
            .await
            .context("adding the update's note")?;
        Ok(())
    }
}

/// The alert of the event on this host, as the rules see it.
fn alert(event: &Event, host: &HostWithGroups, customer: &str) -> Alert {
    Alert {
        severity: event.severity,
        customer: Some(customer.to_owned()),
        host_groups: host.groups.iter().map(|g| g.name.clone()).collect(),
        tags: event.tags.clone(),
        trigger: event.name.clone(),
    }
}

/// A router with `POST /webhook`, which takes an event only with the webhook's `secret`. Answers:
///
/// - 200 with the tags for the problem;
/// - 401 without the secret;
/// - 400 when the body is not what the media type sends;
/// - 502 when the add-on could not act on it within its deadline, with the reason, so that
///   Zabbix tries again.
pub fn router(alerts: Arc<Alerts>, secret: String) -> Router {
    let secret = Arc::new(secret);
    Router::new().route(
        PATH,
        post(move |headers: HeaderMap, body: Bytes| {
            let alerts = alerts.clone();
            let secret = secret.clone();
            async move {
                let authorization = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok());
                if !authorized(authorization, &secret) {
                    tracing::warn!("refused an event of the webhook without its secret");
                    return (StatusCode::UNAUTHORIZED, "the webhook's secret is wrong")
                        .into_response();
                }
                let notice = serde_json::from_slice::<Payload>(&body)
                    .map_err(|e| format!("the body is not what the media type sends: {e}"))
                    .and_then(|payload| Notice::read(&payload));
                let notice = match notice {
                    Ok(Some(notice)) => notice,
                    Ok(None) => return Json(Answer::default()).into_response(),
                    Err(why) => {
                        tracing::warn!(%why, "refused an event of the webhook");
                        return (StatusCode::BAD_REQUEST, why).into_response();
                    }
                };
                let answer: Response =
                    match tokio::time::timeout(DEADLINE, alerts.receive(notice.clone())).await {
                        Ok(Ok(answer)) => Json(answer).into_response(),
                        Ok(Err(e)) => failed(&notice, format!("{e:#}")),
                        Err(_) => failed(
                            &notice,
                            format!("not done within {} seconds", DEADLINE.as_secs()),
                        ),
                    };
                answer
            }
        }),
    )
}

fn failed(notice: &Notice, why: String) -> Response {
    tracing::error!(?notice, error = %why, "could not act on the event: Zabbix tries again");
    (StatusCode::BAD_GATEWAY, why).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_webhook_s_secret_as_a_bearer_is_authorized() {
        let secret = "0123456789abcdef-webhook";
        assert!(authorized(Some("Bearer 0123456789abcdef-webhook"), secret));
        for refused in [
            None,
            Some(""),
            Some("Bearer "),
            Some("0123456789abcdef-webhook"),
            Some("Basic 0123456789abcdef-webhook"),
            Some("Bearer 0123456789abcdef-webhoo"),
            Some("Bearer 0123456789abcdef-webhookx"),
            Some("Bearer 0123456789abcdef-WEBHOOK"),
            Some("Bearer {$HD_WEBHOOK_SECRET}"),
        ] {
            assert!(!authorized(refused, secret), "{refused:?}");
        }
    }
}
