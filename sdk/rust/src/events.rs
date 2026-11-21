//! Events the core sends to an add-on as `POST <url>/events`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

/// A message arrived and the core routed it to a ticket or to the inbox.
pub const MESSAGE_RECEIVED: &str = "message.received";
/// An agent opened a ticket, by hand or from an inbox message.
pub const TICKET_OPENED: &str = "ticket.opened";

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
            kind: "ticket.resolved".into(),
            ..envelope
        };
        assert!(
            matches!(future.event().unwrap(), Event::Other { kind, .. } if kind == "ticket.resolved")
        );
    }
}
