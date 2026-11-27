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
use hd_addon_sdk::api::{Asset, Customer, Ticket};
use serde::Serialize;
use tokio::sync::OnceCell;

use crate::alerts::{
    EventKind, Opening, Payload, Service, TICKET_TAG, Update, new_ticket, recovery_note, resolves,
    update_note,
};
use crate::coverage::Covered;
use crate::rules::{Alert, Matched, OpenTicket, Outcome, Recovery, Rules};
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
    pub async fn receive(&self, kind: EventKind) -> anyhow::Result<Answer> {
        match kind {
            EventKind::Problem { event } => self.problem(&event).await,
            EventKind::Recovery { event } => {
                self.recovery(&event).await.map(|()| Answer::default())
            }
            EventKind::Update { event, update } => self
                .update(&event, &update)
                .await
                .map(|()| Answer::default()),
        }
    }

    /// The host of the event whose alert opens a ticket: the first, in Zabbix's order, that is an
    /// asset of a customer and whose alert a rule opens; none when there is none, each host
    /// passed over logged with the reason.
    async fn opener(&self, event: &Event) -> anyhow::Result<Option<Opener<'_>>> {
        let hosts = self.zabbix.hosts_by_id(&event.host_ids).await?;
        if hosts.is_empty() {
            tracing::warn!(event = %event.id,
                "the trigger of the problem is on no host the add-on's user can read: no ticket");
            return Ok(None);
        }
        let covered = Covered::read(&self.zabbix, &self.core, &self.system).await?;
        for host in hosts {
            let customer = match covered.customer_of(host.groups.iter().map(|g| g.id.as_str())) {
                Ok(customer) => customer.clone(),
                Err(why) => {
                    tracing::info!(event = %event.id, host = %host.name,
                        "the host is not imported, since {why}: no ticket");
                    continue;
                }
            };
            let alert = Alert::of(event, &host, Some(&customer.name));
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
            return Ok(Some(Opener {
                matched,
                open,
                host,
                customer,
            }));
        }
        Ok(None)
    }

    /// A problem: the host whose alert opens a ticket gets it, which a second time is the same
    /// one. A problem that has already recovered when its ticket opens is handled at once as its
    /// recovery would be, since Zabbix may have sent the recovery before the add-on had a ticket.
    async fn problem(&self, id: &str) -> anyhow::Result<Answer> {
        let event = self.zabbix.problem_event(id).await?;
        let Some(opener) = self.opener(&event).await? else {
            return Ok(Answer::default());
        };
        let Opener {
            matched,
            open,
            host,
            customer,
        } = &opener;
        let reference = sync::reference(&host.id, customer.id);
        let assets = self
            .core
            .customer_assets(customer.id)
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
                    .customer_services(customer.id)
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
            host,
            customer,
            asset,
            rule: matched,
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
        if opened.created && event.recovery_id.is_some() {
            tracing::info!(event = %event.id, ticket = opened.number,
                "the problem had already recovered: noting it and resolving as its rule says");
            self.recover(opened.number, &event, open.on_recovery)
                .await?;
        }
        Ok(Answer::ticket(opened.number))
    }

    /// The ticket the add-on opened for the problem with this event id; none when it opened none.
    async fn ticket(&self, id: &str) -> anyhow::Result<Option<Ticket>> {
        match self.core.ticket_by_reference(id).await {
            Ok(ticket) => Ok(Some(ticket)),
            Err(e) if e.status() == Some(404) => Ok(None),
            Err(e) => Err(e).context("reading the problem's ticket"),
        }
    }

    /// A recovery: the rule that opens the problem's alert decides, for the host it opens it
    /// for; when no rule opens it any more, such as after the rules changed, the default does.
    /// Zabbix not answering is a failure, so that it tries again, rather than a guess.
    async fn recovery(&self, id: &str) -> anyhow::Result<()> {
        let Some(ticket) = self.ticket(id).await? else {
            tracing::info!(event = %id, "the problem recovered, and has no ticket");
            return Ok(());
        };
        let event = self
            .zabbix
            .problem_event(id)
            .await
            .context("reading the recovered problem")?;
        let on_recovery = match self.opener(&event).await? {
            Some(opener) => opener.open.on_recovery,
            None => {
                tracing::info!(event = %id, ticket = ticket.number,
                    "no rule opens the problem's alert any more: the default decides on recovery");
                Recovery::default()
            }
        };
        self.recover(ticket.number, &event, on_recovery).await
    }

    /// The recovery of the problem `event`, whose ticket is `number`: a note with its time, then
    /// resolving the ticket as `on_recovery` says, reading it just before.
    async fn recover(
        &self,
        number: i64,
        event: &Event,
        on_recovery: Recovery,
    ) -> anyhow::Result<()> {
        let recovered_at = match &event.recovery_id {
            None => None,
            Some(recovery) => match self.zabbix.event_clock(recovery).await {
                Ok(clock) => Some(clock),
                Err(e) => {
                    tracing::warn!(event = %event.id, recovery = %recovery,
                        error = format!("{e:#}"),
                        "reading when the problem recovered: the note says it without the time");
                    None
                }
            },
        };
        self.core
            .add_note(number, &recovery_note(recovered_at))
            .await
            .context("adding the recovery's note")?;
        // An agent may have picked the ticket up since it was opened.
        let now = self
            .core
            .ticket(number)
            .await
            .context("reading the ticket before resolving it")?;
        if resolves(on_recovery, now.state, now.worked_on_by_agent) {
            self.core
                .resolve_ticket(number)
                .await
                .context("resolving the ticket")?;
            tracing::info!(event = %event.id, ticket = number, "the problem recovered: ticket resolved");
        } else {
            tracing::info!(event = %event.id, ticket = number, on_recovery = on_recovery.name(),
                state = ?now.state, worked_on_by_agent = now.worked_on_by_agent,
                "the problem recovered: ticket left as it is");
        }
        Ok(())
    }

    /// An update: a note with the user and the message, unless the add-on's own user made it or
    /// it has no message.
    async fn update(&self, id: &str, update: &Update) -> anyhow::Result<()> {
        if update.message.is_none() {
            tracing::debug!(event = %id, "an update without a message: no note");
            return Ok(());
        }
        let own = self
            .own_username
            .get_or_try_init(|| self.zabbix.username())
            .await
            .context("asking Zabbix who the add-on's user is")?;
        let Some(note) = update_note(update, own) else {
            tracing::debug!(event = %id, "an update of the add-on's own user: no note");
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

/// The host of an event whose alert opens a ticket, with its customer and the rule that opens it.
struct Opener<'a> {
    matched: Matched<'a>,
    /// The rule's outcome.
    open: &'a OpenTicket,
    host: HostWithGroups,
    customer: Customer,
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
                let kind = serde_json::from_slice::<Payload>(&body)
                    .map_err(|e| format!("the body is not what the media type sends: {e}"))
                    .and_then(|payload| EventKind::read(&payload));
                let kind = match kind {
                    Ok(Some(kind)) => kind,
                    Ok(None) => return Json(Answer::default()).into_response(),
                    Err(why) => {
                        tracing::warn!(%why, "refused an event of the webhook");
                        return (StatusCode::BAD_REQUEST, why).into_response();
                    }
                };
                let answer: Response =
                    match tokio::time::timeout(DEADLINE, alerts.receive(kind.clone())).await {
                        Ok(Ok(answer)) => Json(answer).into_response(),
                        Ok(Err(e)) => failed(&kind, format!("{e:#}")),
                        Err(_) => failed(
                            &kind,
                            format!("not done within {} seconds", DEADLINE.as_secs()),
                        ),
                    };
                answer
            }
        }),
    )
}

fn failed(kind: &EventKind, why: String) -> Response {
    tracing::error!(event = ?kind, error = %why, "could not act on the event: Zabbix tries again");
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
