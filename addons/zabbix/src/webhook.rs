//! The add-on's webhook, where Zabbix's media type (`zabbix-media-type.yaml`) sends the problem,
//! recovery and update events of triggers, with the webhook's secret in `Authorization: Bearer`.
//! The add-on acts on each within the request and answers the tags Zabbix adds to the problem:
//! `hd_ticket` with the number of the ticket a problem opened, and `hd_ticket_url` with the
//! address of its page in the help desk's UI, which the media type's event menu entry opens.

pub mod back;

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
    EventKind, Opening, Payload, Service, TICKET_TAG, TICKET_URL_TAG, Update, new_ticket,
    recovery_note, resolves, ticket_url, update_note,
};
use crate::claims::{Claimed, Claims};
use crate::coverage::Covered;
use crate::hold::Hold;
use crate::rules::{Alert, Matched, OpenTicket, Outcome, Recovery, Rules};
use crate::safety::Limit;
use crate::sync;
use crate::text::same;
use crate::zabbix::{Event, HostWithGroups, OwnUser, Zabbix};

/// Where Zabbix's media type sends its events, on `addon.listen`.
pub const PATH: &str = "/webhook";

/// The media type's name as `zabbix-media-type.yaml` imports it.
pub const MEDIA_TYPE: &str = "Help desk";

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
    /// The ticket's number and, with the UI's base URL, the address of its page.
    fn ticket(number: i64, ui_url: Option<&str>) -> Answer {
        let mut tags = BTreeMap::from([(TICKET_TAG.to_owned(), number.to_string())]);
        if let Some(ui_url) = ui_url {
            tags.insert(TICKET_URL_TAG.to_owned(), ticket_url(ui_url, number));
        }
        Answer { tags }
    }
}

/// What the add-on does with Zabbix's events, between a Zabbix and the core.
pub struct Alerts {
    pub(crate) core: Client,
    pub(crate) zabbix: Zabbix,
    /// The system's name as external references name it.
    pub(crate) system: String,
    rules: Rules,
    /// The add-on's own Zabbix user, asked once.
    own_user: OnceCell<OwnUser>,
    /// The base URL of the help desk's UI, for the address of a ticket's page.
    ui_url: Option<String>,
    /// The safety limit, for the webhook and the reconciliation together.
    pub hold: Hold,
    /// The recoveries handled, or being handled, since the add-on started.
    claims: Claims,
    /// The name of the media type that sends Zabbix's events to the webhook.
    pub(crate) media_type: String,
}

impl std::fmt::Debug for Alerts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Alerts")
            .field("zabbix", &self.zabbix)
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

/// What became of a recovery the add-on was to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recovered {
    /// Handled now.
    Now,
    /// Another handling of it is under way.
    Underway,
    /// Handled before, since the add-on started.
    Before,
}

impl Alerts {
    pub fn new(core: Client, zabbix: Zabbix, system: impl Into<String>, rules: Rules) -> Alerts {
        let system = system.into();
        Alerts {
            hold: Hold::new(core.clone(), system.clone(), Limit::default()),
            core,
            zabbix,
            system,
            rules,
            own_user: OnceCell::new(),
            ui_url: None,
            claims: Claims::default(),
            media_type: MEDIA_TYPE.to_owned(),
        }
    }

    /// With the base URL of the help desk's UI, such as `https://helpdesk.example.com`: a
    /// problem's answer also gives the address of its ticket's page, in the tag `hd_ticket_url`.
    pub fn with_ui_url(mut self, ui_url: &str) -> Alerts {
        self.ui_url = Some(ui_url.to_owned());
        self
    }

    /// With the safety limit `limit` rather than the default, 20 tickets in 5 minutes.
    pub fn with_limit(mut self, limit: Limit) -> Alerts {
        self.hold = Hold::new(self.core.clone(), self.system.clone(), limit);
        self
    }

    /// With the name of the media type that sends Zabbix's events to the webhook, when it is not
    /// the one `zabbix-media-type.yaml` imports, `Help desk`.
    pub fn with_media_type(mut self, name: impl Into<String>) -> Alerts {
        self.media_type = name.into();
        self
    }

    /// Whether the add-on holds back the problems until an agent resolves or closes a safety
    /// ticket.
    pub fn holding(&self) -> bool {
        self.hold.holding()
    }

    /// The add-on's own Zabbix user, asked once.
    async fn own_user(&self) -> anyhow::Result<&OwnUser> {
        self.own_user
            .get_or_try_init(|| self.zabbix.own_user())
            .await
            .context("asking Zabbix who the add-on's user is")
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
    async fn opener(&self, event: &Event, covered: &Covered) -> anyhow::Result<Option<Opener<'_>>> {
        let hosts = self.zabbix.hosts_by_id(&event.host_ids).await?;
        if hosts.is_empty() {
            tracing::warn!(event = %event.id,
                "the trigger of the problem is on no host the add-on's user can read: no ticket");
            return Ok(None);
        }
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
    /// one, also while the add-on holds back the problems.
    async fn problem(&self, id: &str) -> anyhow::Result<Answer> {
        if let Some(ticket) = self.ticket(id).await? {
            return Ok(Answer::ticket(ticket.number, self.ui_url.as_deref()));
        }
        let event = self.zabbix.problem_event(id).await?;
        let covered = Covered::read(&self.zabbix, &self.core, &self.system).await?;
        Ok(match self.open(&event, &covered, true).await? {
            Opened::Ticket { number, .. } => Answer::ticket(number, self.ui_url.as_deref()),
            Opened::Ignored | Opened::HeldBack => Answer::default(),
        })
    }

    /// Opens the ticket of a problem that has none, when the rules open its alert and the safety
    /// limit lets it; `read_safety_ticket` reads the safety ticket first while the add-on holds
    /// back the problems. A problem that has already recovered when its ticket opens is handled
    /// at once as its recovery would be, since Zabbix may have sent the recovery before the
    /// add-on had a ticket.
    pub(crate) async fn open(
        &self,
        event: &Event,
        covered: &Covered,
        read_safety_ticket: bool,
    ) -> anyhow::Result<Opened> {
        let Some(opener) = self.opener(event, covered).await? else {
            return Ok(Opened::Ignored);
        };
        let Some(slot) = self.hold.admit(&event.id, read_safety_ticket).await? else {
            return Ok(Opened::HeldBack);
        };
        let opened = self.open_admitted(event, &opener).await;
        slot.opened(matches!(&opened, Ok((_, true))));
        let (number, created) = opened?;
        if !created {
            return Ok(Opened::Ticket { number, created });
        }
        // The problem may have recovered since it was read, its recovery reaching the webhook
        // while it had no ticket, which Zabbix then takes as received: read it again.
        let recovered = match &event.recovery_id {
            Some(_) => Some(event.clone()),
            None => match self.zabbix.find_problem_event(&event.id).await {
                Ok(now) => now.filter(|now| now.recovery_id.is_some()),
                Err(e) => {
                    tracing::warn!(event = %event.id, ticket = number, error = format!("{e:#}"),
                        "reading the problem again after opening its ticket: a recovery since \
                         is left to its message");
                    None
                }
            },
        };
        if let Some(recovered) = recovered {
            tracing::info!(event = %event.id, ticket = number,
                "the problem had already recovered: noting it and resolving as its rule says");
            if let Claimed::Now(claim) = self.claims.claim(&event.id) {
                self.recover(number, &recovered, opener.open.on_recovery)
                    .await?;
                claim.finish();
            }
        }
        Ok(Opened::Ticket { number, created })
    }

    /// Opens the ticket of an admitted problem; answers its number and whether the core opened
    /// it now.
    async fn open_admitted(
        &self,
        event: &Event,
        opener: &Opener<'_>,
    ) -> anyhow::Result<(i64, bool)> {
        let Opener {
            matched,
            open,
            host,
            customer,
        } = opener;
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
            event,
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
        Ok((opened.number, opened.created))
    }

    /// The ticket the add-on opened for the problem with this event id; none when it opened none.
    pub(crate) async fn ticket(&self, id: &str) -> anyhow::Result<Option<Ticket>> {
        match self.core.ticket_by_reference(id).await {
            Ok(ticket) => Ok(Some(ticket)),
            Err(e) if e.status() == Some(404) => Ok(None),
            Err(e) => Err(e).context("reading the problem's ticket"),
        }
    }

    /// A recovery Zabbix sent. Zabbix not answering is a failure, so that it tries again, rather
    /// than a guess, and so is another handling of the recovery under way, which may fail.
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
        let covered = Covered::read(&self.zabbix, &self.core, &self.system).await?;
        match self.recovered(ticket.number, &event, &covered).await? {
            Recovered::Now | Recovered::Before => Ok(()),
            Recovered::Underway => {
                anyhow::bail!("the problem's recovery is being handled already: try again")
            }
        }
    }

    /// The recovery of the problem `event`, whose ticket is `number`, unless the add-on has
    /// handled it since it started, or is handling it. The rule that opens the problem's alert
    /// decides, for the host it opens it for; when no rule opens it any more, such as after the
    /// rules changed, the default does.
    pub(crate) async fn recovered(
        &self,
        number: i64,
        event: &Event,
        covered: &Covered,
    ) -> anyhow::Result<Recovered> {
        let claim = match self.claims.claim(&event.id) {
            Claimed::Now(claim) => claim,
            Claimed::Underway => return Ok(Recovered::Underway),
            Claimed::Done => {
                tracing::info!(event = %event.id, "the problem's recovery is already handled");
                return Ok(Recovered::Before);
            }
        };
        let on_recovery = match self.opener(event, covered).await? {
            Some(opener) => opener.open.on_recovery,
            None => {
                tracing::info!(event = %event.id, ticket = number,
                    "no rule opens the problem's alert any more: the default decides on recovery");
                Recovery::default()
            }
        };
        self.recover(number, event, on_recovery).await?;
        claim.finish();
        Ok(Recovered::Now)
    }

    /// The recovery of the problem `event`, whose ticket is `number`: a note with its time, then
    /// resolving the ticket as `on_recovery` says, reading it just before; nothing when the
    /// add-on itself closed the problem.
    async fn recover(
        &self,
        number: i64,
        event: &Event,
        on_recovery: Recovery,
    ) -> anyhow::Result<()> {
        // Without it, whether the add-on itself closed the problem cannot be told: a failure,
        // so that Zabbix tries again, rather than a note and maybe a resolution.
        let recovery = match &event.recovery_id {
            None => None,
            Some(recovery) => Some(
                self.zabbix
                    .recovery_event(recovery)
                    .await
                    .context("reading the problem's recovery")?,
            ),
        };
        // The add-on itself closed the problem, since an agent resolved or closed its ticket:
        // nothing to note, and a ticket the agent reopened since stays open.
        if let Some(closed_by) = recovery.as_ref().and_then(|r| r.closed_by.as_deref())
            && closed_by == self.own_user().await?.id
        {
            tracing::info!(event = %event.id, ticket = number,
                "the add-on closed the problem in Zabbix: the ticket is left as it is");
            return Ok(());
        }
        let recovered_at = recovery.map(|r| r.clock);
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
            tracing::info!(event = %event.id, ticket = number,
                "the problem recovered: ticket resolved");
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
        let own = self.own_user().await?;
        let Some(note) = update_note(update, &own.username) else {
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

/// What became of a problem that had no ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opened {
    /// It has this ticket; `created` when the core opened it now rather than answering one
    /// opened meanwhile.
    Ticket { number: i64, created: bool },
    /// No rule opens its alert, or its host is not imported.
    Ignored,
    /// The safety limit holds it back.
    HeldBack,
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
