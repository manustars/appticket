//! Back to Zabbix: when an agent resolves or closes the ticket of a problem, which the core tells
//! the add-on with `ticket.state_changed` (on `POST /events`, signed with the event secret), the
//! add-on acknowledges the problem's event in Zabbix with a message naming the ticket and the
//! agent, and closes the problem when the rule its alert meets now says so and its trigger allows
//! closing it by hand. A resolution of the add-on's own, on recovery, sends nothing back.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use hd_addon_sdk::api::TicketState;
use hd_addon_sdk::events::{Event as CoreEvent, StateChangedBy, TicketStateChanged};
use hd_addon_sdk::server::{HandlerError, events_router};
use hd_addon_sdk::{Envelope, Verifier};
use time::OffsetDateTime;

use super::Alerts;
use crate::zabbix::Refused;

/// How long the add-on takes at most to act on an event of the core: less than the 10 seconds
/// the core waits, so that it answers a failure the core delivers again.
pub const DEADLINE: Duration = Duration::from_secs(8);

/// What an agent did to the ticket of a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    Resolved,
    Closed,
}

impl Done {
    fn word(self) -> &'static str {
        match self {
            Done::Resolved => "resolved",
            Done::Closed => "closed",
        }
    }
}

/// An agent resolving or closing the ticket of a problem: what goes back to Zabbix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChange {
    pub ticket: i64,
    pub done: Done,
    /// The agent's name.
    pub agent: String,
    /// The problem's event id, the ticket's reference.
    pub event: String,
}

impl AgentChange {
    /// The change that goes back to Zabbix: an agent resolving or closing a ticket whose
    /// reference is a problem's event id. None for anything else: a change by the add-on itself
    /// or by a received message, another state, a ticket without a reference or one whose
    /// reference is not an event id, such as a ticket of the add-on's own about itself.
    pub fn of(changed: &TicketStateChanged) -> Option<AgentChange> {
        let StateChangedBy::Agent { agent } = &changed.changed_by else {
            return None;
        };
        let done = match changed.new_state {
            TicketState::Resolved => Done::Resolved,
            TicketState::Closed => Done::Closed,
            TicketState::Open | TicketState::Waiting => return None,
        };
        let event = changed.reference.as_deref()?;
        if event.is_empty() || !event.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(AgentChange {
            ticket: changed.ticket_number,
            done,
            agent: agent.name.clone(),
            event: event.to_owned(),
        })
    }
}

/// Whether the add-on closes the problem in Zabbix, and why not when it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closing {
    /// The rule says so and the trigger allows it.
    Close,
    /// The rule the alert meets now does not say so, or no rule opens it any more.
    RuleKeepsOpen,
    /// The rule says so, but the trigger does not allow closing its problems by hand.
    TriggerKeepsOpen,
    /// Zabbix refused to close it, for this reason.
    Refused(String),
    /// The problem has already recovered in Zabbix.
    Recovered,
}

impl Closing {
    /// From whether the problem has recovered, what the rule its alert meets now says (none when
    /// no rule opens it) and whether its trigger allows closing it by hand (asked only when the
    /// rule says to close it).
    pub fn decide(recovered: bool, close_in_zabbix: Option<bool>, manual_close: bool) -> Closing {
        if recovered {
            Closing::Recovered
        } else if close_in_zabbix != Some(true) {
            Closing::RuleKeepsOpen
        } else if !manual_close {
            Closing::TriggerKeepsOpen
        } else {
            Closing::Close
        }
    }
}

/// The message the event is acknowledged with: the ticket, the agent, and what becomes of the
/// problem in Zabbix.
pub fn acknowledgement(change: &AgentChange, closing: &Closing) -> String {
    let done = format!(
        "Ticket {} was {} in the help desk by {}",
        change.ticket,
        change.done.word(),
        change.agent
    );
    let stays = "the problem stays open in Zabbix";
    match closing {
        Closing::Close => format!("{done}: the problem is closed in Zabbix, as its rule says."),
        Closing::RuleKeepsOpen => format!("{done}; {stays}, since its rule does not close it."),
        Closing::TriggerKeepsOpen => {
            format!("{done}; {stays}, since its trigger does not allow closing it by hand.")
        }
        Closing::Refused(why) => format!("{done}; {stays}, which refused to close it: {why}"),
        Closing::Recovered => format!("{done}."),
    }
}

impl Alerts {
    /// A change of a ticket's state, made at `at`: an agent resolving or closing the ticket of a
    /// problem acknowledges the problem's event, and closes it as [`Closing::decide`] says;
    /// anything else, or a problem Zabbix does not have any more, sends nothing. An
    /// acknowledgement the add-on already made for this change, such as when the core delivers it
    /// again, is not made twice.
    pub async fn state_changed(
        &self,
        changed: &TicketStateChanged,
        at: OffsetDateTime,
    ) -> anyhow::Result<()> {
        let Some(change) = AgentChange::of(changed) else {
            tracing::debug!(ticket = changed.ticket_number, ?changed.changed_by,
                new_state = ?changed.new_state, "a change of state that does not go back to Zabbix");
            return Ok(());
        };
        let Some(event) = self
            .zabbix
            .find_problem_event(&change.event)
            .await
            .context("reading the ticket's problem")?
        else {
            tracing::info!(event = %change.event, ticket = change.ticket,
                "Zabbix has no problem of the ticket any more: nothing to acknowledge");
            return Ok(());
        };
        let recovered = event.recovery_id.is_some();
        let (close_in_zabbix, manual_close) = if recovered {
            (None, false)
        } else {
            let close_in_zabbix = self
                .opener(&event)
                .await?
                .map(|opener| opener.open.close_in_zabbix);
            let manual_close = close_in_zabbix == Some(true)
                && self
                    .zabbix
                    .allows_manual_close(&event.id)
                    .await
                    .context("asking Zabbix whether the trigger allows closing its problem")?;
            (close_in_zabbix, manual_close)
        };
        let closing = Closing::decide(recovered, close_in_zabbix, manual_close);
        let message = acknowledgement(&change, &closing);
        let own = self.own_user().await?;
        let made = self
            .zabbix
            .acknowledgements(&event.id)
            .await
            .context("reading the problem's acknowledgements")?;
        if made
            .iter()
            .any(|a| a.user_id == own.id && a.message == message && a.clock >= at.unix_timestamp())
        {
            tracing::info!(event = %event.id, ticket = change.ticket,
                "the change was already acknowledged in Zabbix");
            return Ok(());
        }
        let close = closing == Closing::Close;
        match self.zabbix.acknowledge(&event.id, close, &message).await {
            Ok(()) => {}
            Err(e) if close && e.downcast_ref::<Refused>().is_some() => {
                let why = e
                    .downcast_ref::<Refused>()
                    .map(Refused::reason)
                    .unwrap_or_default();
                tracing::warn!(event = %event.id, ticket = change.ticket, %why,
                    "Zabbix refused to close the problem: acknowledging it only");
                let message = acknowledgement(&change, &Closing::Refused(why));
                self.zabbix
                    .acknowledge(&event.id, false, &message)
                    .await
                    .context("acknowledging the problem")?;
                return Ok(());
            }
            Err(e) => return Err(e).context("acknowledging the problem"),
        }
        tracing::info!(event = %event.id, ticket = change.ticket, ?closing,
            "the ticket's change acknowledged in Zabbix");
        Ok(())
    }
}

/// A router with `POST /events`, which takes the core's events signed with the event secret:
/// `ticket.state_changed` goes back to Zabbix, and the others are taken and left alone. An event
/// the add-on could not act on within its deadline, because Zabbix or the core did not answer or
/// refused, is answered with a failure, so that the core delivers it again.
pub fn router(alerts: Arc<Alerts>, verifier: Verifier) -> Router {
    events_router(verifier, move |envelope: Envelope| {
        let alerts = alerts.clone();
        async move {
            let changed = match envelope.event() {
                Ok(CoreEvent::TicketStateChanged(changed)) => changed,
                Ok(_) => return Ok(()),
                Err(e) => {
                    tracing::warn!(event = %envelope.id, kind = %envelope.kind, error = %e,
                        "an event of the core the add-on cannot read: left alone");
                    return Ok(());
                }
            };
            match tokio::time::timeout(
                DEADLINE,
                alerts.state_changed(&changed, envelope.created_at),
            )
            .await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err::<(), HandlerError>(format!("{e:#}").into()),
                Err(_) => Err(format!("not done within {} seconds", DEADLINE.as_secs()).into()),
            }
        }
    })
}

/// A router with `POST /events` that takes every event and leaves it alone, for an add-on that
/// receives no alerts: it has no ticket whose changes go back to Zabbix.
pub fn ignoring(verifier: Verifier) -> Router {
    events_router(verifier, |_: Envelope| async { Ok::<(), HandlerError>(()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hd_addon_sdk::events::Agent;
    use uuid::Uuid;

    fn changed(
        new_state: TicketState,
        by: StateChangedBy,
        reference: Option<&str>,
    ) -> TicketStateChanged {
        TicketStateChanged {
            ticket_number: 42,
            old_state: TicketState::Open,
            new_state,
            changed_by: by,
            reference: reference.map(str::to_owned),
        }
    }

    fn marco() -> StateChangedBy {
        StateChangedBy::Agent {
            agent: Agent {
                id: Uuid::from_u128(7),
                name: "Marco Bellini".into(),
            },
        }
    }

    #[test]
    fn only_an_agent_resolving_or_closing_the_ticket_of_a_problem_goes_back_to_zabbix() {
        assert_eq!(
            AgentChange::of(&changed(TicketState::Resolved, marco(), Some("4213"))),
            Some(AgentChange {
                ticket: 42,
                done: Done::Resolved,
                agent: "Marco Bellini".into(),
                event: "4213".into(),
            })
        );
        assert_eq!(
            AgentChange::of(&changed(TicketState::Closed, marco(), Some("4213"))).map(|c| c.done),
            Some(Done::Closed)
        );
        for (state, by, reference) in [
            // The add-on's own resolution, on recovery.
            (
                TicketState::Resolved,
                StateChangedBy::Addon {
                    addon: "zabbix".into(),
                },
                Some("4213"),
            ),
            (
                TicketState::Open,
                StateChangedBy::Message {
                    message_id: Uuid::from_u128(9),
                },
                Some("4213"),
            ),
            (TicketState::Open, marco(), Some("4213")),
            (TicketState::Waiting, marco(), Some("4213")),
            (TicketState::Resolved, StateChangedBy::Other, Some("4213")),
            // A ticket the add-on opened without a reference, or about itself.
            (TicketState::Resolved, marco(), None),
            (TicketState::Resolved, marco(), Some("")),
            (
                TicketState::Closed,
                marco(),
                Some("safety-limit:2026-11-27T09:00:00Z"),
            ),
            (TicketState::Resolved, marco(), Some("42a")),
        ] {
            assert_eq!(
                AgentChange::of(&changed(state, by.clone(), reference)),
                None,
                "{state:?} {by:?} {reference:?}"
            );
        }
    }

    #[test]
    fn the_problem_is_closed_only_when_its_rule_says_so_and_its_trigger_allows_it() {
        use Closing::*;
        for (recovered, rule, trigger, closing) in [
            (false, Some(true), true, Close),
            (false, Some(true), false, TriggerKeepsOpen),
            (false, Some(false), true, RuleKeepsOpen),
            (false, Some(false), false, RuleKeepsOpen),
            // No rule opens its alert any more: the default does not close.
            (false, None, true, RuleKeepsOpen),
            (true, Some(true), true, Recovered),
            (true, None, false, Recovered),
        ] {
            assert_eq!(
                Closing::decide(recovered, rule, trigger),
                closing,
                "recovered {recovered}, rule {rule:?}, trigger {trigger}"
            );
        }
    }

    #[test]
    fn the_acknowledgement_names_the_ticket_and_the_agent_and_says_what_becomes_of_the_problem() {
        let resolved = AgentChange {
            ticket: 42,
            done: Done::Resolved,
            agent: "Marco Bellini".into(),
            event: "4213".into(),
        };
        let closed = AgentChange {
            done: Done::Closed,
            ..resolved.clone()
        };
        for (change, closing, message) in [
            (
                &resolved,
                Closing::Close,
                "Ticket 42 was resolved in the help desk by Marco Bellini: the problem is closed \
                 in Zabbix, as its rule says.",
            ),
            (
                &resolved,
                Closing::RuleKeepsOpen,
                "Ticket 42 was resolved in the help desk by Marco Bellini; the problem stays open \
                 in Zabbix, since its rule does not close it.",
            ),
            (
                &closed,
                Closing::TriggerKeepsOpen,
                "Ticket 42 was closed in the help desk by Marco Bellini; the problem stays open in \
                 Zabbix, since its trigger does not allow closing it by hand.",
            ),
            (
                &resolved,
                Closing::Refused("No permissions to referred object or it does not exist!".into()),
                "Ticket 42 was resolved in the help desk by Marco Bellini; the problem stays open \
                 in Zabbix, which refused to close it: No permissions to referred object or it \
                 does not exist!",
            ),
            (
                &closed,
                Closing::Recovered,
                "Ticket 42 was closed in the help desk by Marco Bellini.",
            ),
        ] {
            assert_eq!(acknowledgement(change, &closing), message);
        }
    }
}
