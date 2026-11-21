//! What an add-on proposes to the agents. The JSON matches the core's `proposals::Content`: a
//! `kind` tag, and for triage an `action` tag.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::Priority;

/// What is proposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    /// What to do with an inbox message.
    Triage(Triage),
    /// The ticket's priority.
    Priority { priority: Priority },
    /// Active assets of the ticket's customer the ticket is about (at least one).
    Assets { asset_ids: Vec<Uuid> },
    /// A draft reply to the requester; an accepted draft goes into the agent's composer and is
    /// never sent by itself.
    Reply { body: String },
}

/// What to do with an inbox message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Triage {
    /// Open a ticket for this customer; the sender becomes its requester.
    OpenTicket { customer_id: Uuid },
    /// Attach the message to this ticket.
    Attach { ticket_number: i64 },
    /// Spam, a newsletter, an automatic reply.
    Discard,
}

impl Content {
    /// The proposal's kind, as the core names it.
    pub fn kind(&self) -> &'static str {
        match self {
            Content::Triage(_) => "triage",
            Content::Priority { .. } => "priority",
            Content::Assets { .. } => "assets",
            Content::Reply { .. } => "reply",
        }
    }
}

/// Why a proposal is made, one sentence per language of the agents' UI, keyed by language tag:
/// `{"en": "...", "it": "..."}`. Agents whose language is missing see the English one.
pub type Rationale = BTreeMap<String, String>;

/// A rationale with only an English sentence.
pub fn in_english(sentence: impl Into<String>) -> Rationale {
    Rationale::from([("en".to_owned(), sentence.into())])
}

/// Where a proposal applies: triage goes on an inbox message, everything else on a ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Message(Uuid),
    Ticket(i64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_has_the_core_json() {
        let customer = Uuid::parse_str("0e7b6a52-5d0f-4d5e-8a8e-3f2b1c0d9e8f").unwrap();
        let cases = [
            (
                Content::Triage(Triage::OpenTicket {
                    customer_id: customer,
                }),
                json!({"kind": "triage", "action": "open_ticket", "customer_id": customer}),
            ),
            (
                Content::Triage(Triage::Attach { ticket_number: 12 }),
                json!({"kind": "triage", "action": "attach", "ticket_number": 12}),
            ),
            (
                Content::Triage(Triage::Discard),
                json!({"kind": "triage", "action": "discard"}),
            ),
            (
                Content::Priority {
                    priority: Priority::Urgent,
                },
                json!({"kind": "priority", "priority": "urgent"}),
            ),
            (
                Content::Assets {
                    asset_ids: vec![customer],
                },
                json!({"kind": "assets", "asset_ids": [customer]}),
            ),
            (
                Content::Reply {
                    body: "Hello".into(),
                },
                json!({"kind": "reply", "body": "Hello"}),
            ),
        ];
        for (content, expected) in cases {
            assert_eq!(serde_json::to_value(&content).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<Content>(expected).unwrap(),
                content
            );
        }
    }
}
