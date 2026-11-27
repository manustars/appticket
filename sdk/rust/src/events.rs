//! Events the core sends to an add-on as `POST <url>/events`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::api::TicketState;

/// A message arrived and the core routed it to a ticket or to the inbox.
pub const MESSAGE_RECEIVED: &str = "message.received";
/// An agent opened a ticket, by hand or from an inbox message, or an add-on opened one.
pub const TICKET_OPENED: &str = "ticket.opened";
/// A ticket's state changed: who changed it, from which state to which.
pub const TICKET_STATE_CHANGED: &str = "ticket.state_changed";

/// Header carrying the event's id; the same on every retry. It is not signed: once the body is
/// verified, rely on [`Envelope::id`] instead.
pub const HEADER_EVENT_ID: &str = "hd-event-id";
/// Header carrying the seconds since the Unix epoch when the attempt was sent.
pub const HEADER_TIMESTAMP: &str = "hd-timestamp";
/// Header carrying `sha256=<hex>` of HMAC-SHA-256 over `<timestamp>.<raw body>`.
pub const HEADER_SIGNATURE: &str = "hd-signature";

/// The body of an event delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// The same on every retry: skip an id already handled.
    pub id: Uuid,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default)]
    pub data: Value,
}

/// An event's data, typed by its kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    MessageReceived(MessageReceived),
    TicketOpened(TicketOpened),
    TicketStateChanged(TicketStateChanged),
    /// A kind this version of the SDK does not know; newer cores may send more kinds.
    Other {
        kind: String,
        data: Value,
    },
}

impl Envelope {
    /// The typed data. Fails when a known kind carries data of the wrong shape.
    pub fn event(&self) -> Result<Event, serde_json::Error> {
        Ok(match self.kind.as_str() {
            MESSAGE_RECEIVED => Event::MessageReceived(serde_json::from_value(self.data.clone())?),
            TICKET_OPENED => Event::TicketOpened(serde_json::from_value(self.data.clone())?),
            TICKET_STATE_CHANGED => {
                Event::TicketStateChanged(serde_json::from_value(self.data.clone())?)
            }
            _ => Event::Other {
                kind: self.kind.clone(),
                data: self.data.clone(),
            },
        })
    }
}

/// Data of `message.received`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageReceived {
    /// The message's id in the core.
    pub message_id: Uuid,
    /// The ticket it joined or opened, or `None` when it waits in the inbox.
    pub ticket_number: Option<i64>,
    pub from_address: String,
    #[serde(default)]
    pub from_name: Option<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body_text: String,
}

/// Data of `ticket.opened`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketOpened {
    pub ticket_number: i64,
}

/// Data of `ticket.state_changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketStateChanged {
    pub ticket_number: i64,
    /// The state the ticket left.
    pub old_state: TicketState,
    /// The state the ticket is in now; never the old one.
    pub new_state: TicketState,
    pub changed_by: StateChangedBy,
}

/// Who or what changed a ticket's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateChangedBy {
    /// An agent: by hand, with a reply, by attaching an inbox message to the ticket, or by
    /// resolving the children of a parent ticket.
    Agent { agent: Agent },
    /// A received message that joined the ticket and reopened it; `message.received` carries the
    /// same `message_id`. Its sender is not always the ticket's requester.
    Message { message_id: Uuid },
    /// A kind this version of the SDK does not know; newer cores may send more kinds.
    #[serde(other)]
    Other,
}

/// An agent of the help desk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: Uuid,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_kinds_are_typed_and_others_kept() {
        let body = r#"{"id":"6f1c2c1e-8d2a-4c4f-9a43-0d5b8f6e2a10","kind":"message.received",
            "created_at":"2026-11-21T09:00:00Z","data":{"message_id":"0e7b6a52-5d0f-4d5e-8a8e-3f2b1c0d9e8f",
            "ticket_number":null,"from_address":"ada@brightfield.test","from_name":null,
            "subject":"PC will not start","body_text":"Black screen.","later":"ignored"}}"#;
        let envelope: Envelope = serde_json::from_str(body).unwrap();
        match envelope.event().unwrap() {
            Event::MessageReceived(m) => {
                assert_eq!(m.ticket_number, None);
                assert_eq!(m.from_address, "ada@brightfield.test");
            }
            other => panic!("{other:?}"),
        }

        let opened = Envelope {
            kind: TICKET_OPENED.into(),
            data: serde_json::json!({"ticket_number": 7}),
            ..envelope.clone()
        };
        assert_eq!(
            opened.event().unwrap(),
            Event::TicketOpened(TicketOpened { ticket_number: 7 })
        );

        let future = Envelope {
            kind: "ticket.merged".into(),
            ..envelope
        };
        assert!(
            matches!(future.event().unwrap(), Event::Other { kind, .. } if kind == "ticket.merged")
        );
    }

    /// `ticket.state_changed` comes typed, with who changed it; a kind of changer this version
    /// does not know yet still reads.
    #[test]
    fn a_change_of_state_says_who_made_it() {
        let marco = Uuid::parse_str("2b9f3c6e-1d4a-4e8b-9c7d-5a6f0e1b2c3d").unwrap();
        let message = Uuid::parse_str("7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f").unwrap();
        let changed = |changed_by: Value| Envelope {
            id: Uuid::nil(),
            kind: TICKET_STATE_CHANGED.into(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            data: serde_json::json!({"ticket_number": 42, "old_state": "waiting",
                "new_state": "open", "changed_by": changed_by}),
        };
        let event = |changed_by: StateChangedBy| {
            Event::TicketStateChanged(TicketStateChanged {
                ticket_number: 42,
                old_state: TicketState::Waiting,
                new_state: TicketState::Open,
                changed_by,
            })
        };

        assert_eq!(
            changed(serde_json::json!({"kind": "agent",
                "agent": {"id": marco, "name": "Marco Bellini"}}))
            .event()
            .unwrap(),
            event(StateChangedBy::Agent {
                agent: Agent {
                    id: marco,
                    name: "Marco Bellini".into()
                }
            })
        );
        assert_eq!(
            changed(serde_json::json!({"kind": "message", "message_id": message}))
                .event()
                .unwrap(),
            event(StateChangedBy::Message {
                message_id: message
            })
        );
        assert_eq!(
            changed(serde_json::json!({"kind": "addon", "addon": "watchtower"}))
                .event()
                .unwrap(),
            event(StateChangedBy::Other)
        );
    }
}
