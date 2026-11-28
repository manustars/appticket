//! The reconciliation: at startup and at each round, the add-on catches up with what Zabbix's
//! webhook did not bring it, such as while it was down. It opens the tickets of the problems open
//! now on the hosts it imports that have none, as the webhook would (the ticket reference makes it
//! safe to repeat, and the safety limit counts them with the webhook's), and handles as a
//! recovery each open or waiting ticket it opened whose problem recovered without the webhook
//! receiving it, as Zabbix's record of the media type's alerts says. Those tickets get no tag in
//! Zabbix, which takes tags only from the webhook's answer.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use hd_addon_sdk::api::{TicketState, TicketSummary};

use crate::coverage::Covered;
use crate::references::TicketReference;
use crate::webhook::{Alerts, Opened, Recovered};
use crate::zabbix::AlertStatus;

/// The problems open now, of `active`, that have no ticket among those the add-on opened that
/// are not closed, `opened`; one may still have a closed ticket.
pub fn without_ticket<'a>(active: &'a [String], opened: &[TicketSummary]) -> Vec<&'a str> {
    let known: HashSet<&str> = opened
        .iter()
        .filter_map(|t| t.reference.as_deref())
        .collect();
    active
        .iter()
        .map(String::as_str)
        .filter(|id| !known.contains(id))
        .collect()
}

/// The tickets of problems, among those the add-on opened, that are open or waiting and whose
/// problem is not among those open now, `active`, so that it may have recovered. A resolved one
/// is left: its recovery would only add a note.
pub fn maybe_recovered<'a>(
    opened: &'a [TicketSummary],
    active: &[String],
) -> Vec<&'a TicketSummary> {
    let active: HashSet<&str> = active.iter().map(String::as_str).collect();
    opened
        .iter()
        .filter(|t| matches!(t.state, TicketState::Open | TicketState::Waiting))
        .filter(|t| {
            t.reference
                .as_deref()
                .and_then(TicketReference::read)
                .is_some_and(|r| r.problem().is_some_and(|event| !active.contains(event)))
        })
        .collect()
}

/// Whether the webhook received a recovery, by the statuses of the media type's alerts for the
/// recovery event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// One was sent: the webhook answered it, having handled the recovery.
    Received,
    /// Zabbix is still trying: the next reconciliation sees how it ended.
    Underway,
    /// None was sent, and none will be: the reconciliation handles the recovery.
    Missed,
}

pub fn delivery(statuses: &[AlertStatus]) -> Delivery {
    if statuses.contains(&AlertStatus::Sent) {
        Delivery::Received
    } else if statuses.contains(&AlertStatus::Underway) {
        Delivery::Underway
    } else {
        Delivery::Missed
    }
}

/// What a reconciliation did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Problems open now on the hosts the add-on imports.
    pub active: usize,
    /// Tickets opened for problems that had none.
    pub opened: usize,
    /// Problems the safety limit held back.
    pub held_back: usize,
    /// Tickets whose problem's recovery was handled.
    pub recovered: usize,
    /// Problems or tickets the add-on could not act on, each logged with the reason.
    pub failed: usize,
    /// Whether the add-on holds back the problems at the end.
    pub holding: bool,
}

impl Alerts {
    /// One reconciliation: whether the wait for a safety ticket has ended, the missed
    /// recoveries, then the tickets of the problems that have none. A round in which Zabbix or
    /// the core does not answer the reads it starts with does nothing.
    pub async fn reconcile(&self) -> anyhow::Result<Reconciled> {
        let mut round = Reconciled::default();
        self.hold.resume().await?;
        // Once per round: the openings below do not read the safety ticket again.
        self.hold.check().await?;
        let covered = Covered::read(&self.zabbix, &self.core, &self.system).await?;
        let active = self
            .zabbix
            .active_problems(&covered.coverage.imported_groups())
            .await
            .context("reading Zabbix's problems")?;
        round.active = active.len();
        let opened = self
            .core
            .opened_tickets()
            .await
            .context("reading the tickets the add-on opened")?;

        self.recoveries(&opened, &active, &covered, &mut round)
            .await?;

        for id in without_ticket(&active, &opened) {
            match self.ticket(id).await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(event = %id, error = format!("{e:#}"),
                        "reconciling the problem: left for the next round");
                    round.failed += 1;
                    continue;
                }
            }
            let opening = async {
                let event = self.zabbix.problem_event(id).await?;
                self.open(&event, &covered, false).await
            };
            match opening.await {
                Ok(Opened::Ticket { created: true, .. }) => round.opened += 1,
                Ok(Opened::Ticket { created: false, .. } | Opened::Ignored) => {}
                Ok(Opened::HeldBack) => round.held_back += 1,
                Err(e) => {
                    tracing::error!(event = %id, error = format!("{e:#}"),
                        "opening the problem's ticket: left for the next round");
                    round.failed += 1;
                }
            }
        }
        if let Err(e) = self.hold.tell_held_back().await {
            tracing::error!(error = format!("{e:#}"), "trying again at the next round");
            round.failed += 1;
        }
        round.holding = self.holding();
        Ok(round)
    }

    /// Handles as a recovery each ticket whose problem recovered without the webhook receiving
    /// it.
    async fn recoveries(
        &self,
        opened: &[TicketSummary],
        active: &[String],
        covered: &Covered,
        round: &mut Reconciled,
    ) -> anyhow::Result<()> {
        let candidates = maybe_recovered(opened, active);
        let ids: Vec<String> = candidates
            .iter()
            .filter_map(|t| t.reference.clone())
            .collect();
        let mut events = Vec::new();
        for event in self
            .zabbix
            .readable_problem_events(&ids)
            .await
            .context("reading the problems of the add-on's tickets")?
        {
            match event {
                Ok(event) if event.recovery_id.is_some() => events.push(event),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        error = format!("{e:#}"),
                        "the problem of a ticket of the add-on: left as it is"
                    );
                    round.failed += 1;
                }
            }
        }
        let recoveries: Vec<String> = events
            .iter()
            .filter_map(|e| e.recovery_id.clone())
            .collect();
        let deliveries = self.deliveries(&recoveries).await?;
        for event in &events {
            let Some(ticket) = candidates
                .iter()
                .find(|t| t.reference.as_deref() == Some(event.id.as_str()))
            else {
                continue;
            };
            let received = match (&deliveries, &event.recovery_id) {
                (Some(deliveries), Some(recovery)) => {
                    delivery(deliveries.get(recovery).map_or(&[][..], Vec::as_slice))
                }
                _ => Delivery::Missed,
            };
            match received {
                Delivery::Received => continue,
                Delivery::Underway => {
                    tracing::debug!(event = %event.id, ticket = ticket.number,
                        "Zabbix is still sending the problem's recovery to the webhook");
                    continue;
                }
                Delivery::Missed => {}
            }
            tracing::info!(event = %event.id, ticket = ticket.number,
                "the problem recovered without the webhook receiving it");
            match self.recovered(ticket.number, event, covered).await {
                Ok(Recovered::Now) => round.recovered += 1,
                Ok(Recovered::Underway | Recovered::Before) => {}
                Err(e) => {
                    tracing::error!(event = %event.id, ticket = ticket.number,
                        error = format!("{e:#}"),
                        "handling the problem's recovery: left for the next round");
                    round.failed += 1;
                }
            }
        }
        Ok(())
    }

    /// The statuses of the media type's alerts for each of these recovery events, by event id;
    /// none when the add-on's Zabbix user cannot read them all, or Zabbix refuses them, logged
    /// with what to change: every recovery is then taken as missed, and the reconciliation goes
    /// on.
    async fn deliveries(
        &self,
        recoveries: &[String],
    ) -> anyhow::Result<Option<HashMap<String, Vec<AlertStatus>>>> {
        if recoveries.is_empty() {
            return Ok(Some(HashMap::new()));
        }
        match self.read_deliveries(recoveries).await {
            Ok(deliveries) => Ok(deliveries),
            Err(e) => {
                tracing::warn!(media_type = %self.media_type, error = format!("{e:#}"),
                    "Zabbix does not let the add-on read the webhook's alerts, so it cannot tell \
                     which recoveries the webhook received: handling every one it finds, once per \
                     run");
                Ok(None)
            }
        }
    }

    async fn read_deliveries(
        &self,
        recoveries: &[String],
    ) -> anyhow::Result<Option<HashMap<String, Vec<AlertStatus>>>> {
        let Some(media_type) = self
            .zabbix
            .media_type_id(&self.media_type)
            .await
            .context("reading the webhook's media type")?
        else {
            tracing::warn!(media_type = %self.media_type,
                "the add-on's Zabbix user reads no media type of this name (webhook.media_type), \
                 so it cannot tell which recoveries the webhook received: handling every one it \
                 finds, once per run");
            return Ok(None);
        };
        let access = self
            .zabbix
            .alert_access(&media_type)
            .await
            .context("asking Zabbix what the add-on's user reads of the alerts")?;
        if !access.reads_the_webhook_s_alerts() {
            tracing::warn!(media_type = %self.media_type,
                "the add-on's Zabbix user reads only the alerts sent to itself, and has no media \
                 of the webhook's media type: give it that media, or make it a Super admin. Until \
                 then it cannot tell which recoveries the webhook received, and handles every one \
                 it finds, once per run");
            return Ok(None);
        }
        let statuses = self
            .zabbix
            .alert_statuses(recoveries, &media_type)
            .await
            .context("reading the media type's alerts for the recoveries")?;
        // Reading only its own alerts, the add-on reads none when the action sends to another
        // user: say so when it reads not one alert of the media type.
        if statuses.is_empty()
            && !access.super_admin
            && !self
                .zabbix
                .any_alert(&media_type)
                .await
                .context("reading whether any alert of the media type is readable")?
        {
            tracing::warn!(media_type = %self.media_type,
                "the add-on's Zabbix user reads no alert of the webhook's media type at all: if \
                 the action sends its messages to another user, the add-on reads none of them, \
                 and handles every recovery it finds as missed. Make the action send to the \
                 add-on's own user, or make that user a Super admin");
        }
        Ok(Some(statuses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hd_addon_sdk::api::Priority;
    use serde_json::{Value, json};
    use time::OffsetDateTime;
    use uuid::Uuid;

    /// A Zabbix whose API answers each method as `answer` says: a result, or `None` for a refusal
    /// such as a role that does not allow the method gives.
    async fn zabbix(answer: fn(&str, &Value) -> Option<Value>) -> crate::zabbix::Zabbix {
        let app = axum::Router::new().route(
            "/api_jsonrpc.php",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let call: Value = serde_json::from_slice(&body).unwrap();
                let method = call["method"].as_str().unwrap_or_default();
                let mut answered = match answer(method, &call["params"]) {
                    Some(result) => json!({"result": result}),
                    None => json!({"error": {"code": -32500, "message": "Application error.",
                        "data": format!("No permissions to call \"{method}\".")}}),
                };
                answered["jsonrpc"] = json!("2.0");
                answered["id"] = call["id"].clone();
                axum::Json(answered)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        crate::zabbix::Zabbix::new(&url, "zabbix-token", std::time::Duration::from_secs(5)).unwrap()
    }

    fn alerts(zabbix: crate::zabbix::Zabbix) -> Alerts {
        // The core is never called.
        let core = hd_addon_sdk::Client::new("http://127.0.0.1:9", "core-token").unwrap();
        Alerts::new(core, zabbix, "Zabbix", crate::rules::Rules::default_list())
    }

    /// A Super admin's Zabbix, with the media type 7, that answers `alert.get` as `alerts` says.
    fn readable(method: &str, alerts: Option<Value>) -> Option<Value> {
        match method {
            "mediatype.get" => Some(json!([{"mediatypeid": "7"}])),
            "user.checkAuthentication" => Some(json!({"userid": "1", "type": 3})),
            "alert.get" => alerts,
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_zabbix_that_refuses_the_alerts_or_the_media_type_leaves_the_deliveries_unknown() {
        let refused_alerts = alerts(zabbix(|method, _| readable(method, None)).await);
        assert_eq!(
            refused_alerts.deliveries(&ids(&["4220"])).await.unwrap(),
            None
        );
        let refused_media_type = alerts(
            zabbix(|method, _| match method {
                "mediatype.get" => None,
                _ => readable(method, Some(json!([]))),
            })
            .await,
        );
        assert_eq!(
            refused_media_type
                .deliveries(&ids(&["4220"]))
                .await
                .unwrap(),
            None
        );
        let refused_user = alerts(
            zabbix(|method, _| match method {
                "user.checkAuthentication" => None,
                _ => readable(method, Some(json!([]))),
            })
            .await,
        );
        assert_eq!(
            refused_user.deliveries(&ids(&["4220"])).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_user_with_the_media_who_reads_no_alert_at_all_takes_every_recovery_as_missed() {
        // An Admin with the media type 7, to whom the action sends nothing.
        let own_media = alerts(
            zabbix(|method, _| match method {
                "mediatype.get" => Some(json!([{"mediatypeid": "7"}])),
                "user.checkAuthentication" => Some(json!({"userid": "5", "type": 2})),
                "user.get" => Some(json!([{"userid": "5", "medias": [{"mediatypeid": "7"}]}])),
                "alert.get" => Some(json!([])),
                _ => None,
            })
            .await,
        );
        assert_eq!(
            own_media.deliveries(&ids(&["4220"])).await.unwrap(),
            Some(HashMap::new())
        );
        // Without the media, it cannot tell.
        let no_media = alerts(
            zabbix(|method, _| match method {
                "mediatype.get" => Some(json!([{"mediatypeid": "7"}])),
                "user.checkAuthentication" => Some(json!({"userid": "5", "type": 2})),
                "user.get" => Some(json!([{"userid": "5", "medias": []}])),
                _ => None,
            })
            .await,
        );
        assert_eq!(no_media.deliveries(&ids(&["4220"])).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_zabbix_that_answers_gives_the_deliveries_by_recovery_event() {
        let answering = alerts(
            zabbix(|method, _| {
                readable(
                    method,
                    Some(json!([{"eventid": "4220", "status": "1", "alerttype": "0"}])),
                )
            })
            .await,
        );
        assert_eq!(
            answering.deliveries(&ids(&["4220", "4221"])).await.unwrap(),
            Some(HashMap::from([(
                "4220".to_owned(),
                vec![AlertStatus::Sent]
            )]))
        );
    }

    fn ticket(number: i64, state: TicketState, reference: Option<&str>) -> TicketSummary {
        TicketSummary {
            number,
            title: "SRV-01: Disk almost full".into(),
            state,
            priority: Priority::High,
            customer_id: Uuid::from_u128(1),
            requester_email: None,
            reference: reference.map(str::to_owned),
            updated_at: OffsetDateTime::from_unix_timestamp(1_795_801_516).unwrap(),
        }
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn the_problems_open_now_without_a_ticket_are_those_no_ticket_names() {
        let opened = [
            ticket(1, TicketState::Open, Some("4213")),
            ticket(2, TicketState::Resolved, Some("4214")),
            ticket(3, TicketState::Open, None),
        ];
        assert_eq!(
            without_ticket(&ids(&["4213", "4214", "4215", "4216"]), &opened),
            ["4215", "4216"]
        );
        assert!(without_ticket(&[], &opened).is_empty());
    }

    #[test]
    fn an_open_or_waiting_ticket_may_have_recovered_when_its_problem_is_not_open_now() {
        let safety = "safety-limit:2026-11-27T09:00:00Z";
        let opened = [
            ticket(1, TicketState::Open, Some("4213")),
            ticket(2, TicketState::Waiting, Some("4214")),
            ticket(3, TicketState::Resolved, Some("4215")),
            ticket(4, TicketState::Closed, Some("4216")),
            ticket(5, TicketState::Open, Some(safety)),
            ticket(6, TicketState::Open, None),
            ticket(7, TicketState::Open, Some("4217")),
        ];
        let numbers: Vec<i64> = maybe_recovered(&opened, &ids(&["4217"]))
            .iter()
            .map(|t| t.number)
            .collect();
        assert_eq!(
            numbers,
            [1, 2],
            "neither resolved, closed, the safety ticket, nor open now"
        );
    }

    #[test]
    fn a_recovery_was_received_when_one_of_the_media_type_s_alerts_was_sent() {
        use AlertStatus::*;
        for (statuses, expected) in [
            (&[Sent][..], Delivery::Received),
            (&[Failed, Sent], Delivery::Received),
            (&[Underway, Sent], Delivery::Received),
            (&[Underway], Delivery::Underway),
            (&[Failed, Underway], Delivery::Underway),
            (&[Failed], Delivery::Missed),
            (&[], Delivery::Missed),
        ] {
            assert_eq!(delivery(statuses), expected, "{statuses:?}");
        }
    }
}
