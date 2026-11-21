//! What the add-on API answers. Only the fields an add-on needs are here; fields the core adds
//! later are ignored.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The add-on a token belongs to, and what it may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub scopes: Vec<String>,
    /// The languages of the agents' UI, such as `["en", "it"]`: write each rationale in all
    /// of them. Empty from a core that predates per-language rationales.
    #[serde(default)]
    pub languages: Vec<String>,
}

/// A customer that can get tickets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Customer {
    pub id: Uuid,
    pub name: String,
    /// The help desk's own organization.
    #[serde(default)]
    pub is_help_desk: bool,
    /// Mail domains that identify the customer's requesters.
    #[serde(default)]
    pub domains: Vec<String>,
}

/// An active asset of a customer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    pub customer_id: Uuid,
    /// What sort of thing it is: computer, printer, application, licence…
    pub kind: String,
    pub name: String,
}

/// A ticket's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketState {
    Open,
    Waiting,
    Resolved,
    Closed,
}

/// How soon a ticket needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }
}

/// A ticket that is not closed, as listed by `GET /api/addon/tickets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSummary {
    pub number: i64,
    pub title: String,
    pub state: TicketState,
    pub priority: Priority,
    pub customer_id: Uuid,
    #[serde(default)]
    pub requester_email: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A ticket with its conversation, as `GET /api/addon/tickets/{number}` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub number: i64,
    pub title: String,
    pub state: TicketState,
    pub priority: Priority,
    pub customer: TicketCustomer,
    #[serde(default)]
    pub requester: Option<Requester>,
    /// Assets already linked to the ticket.
    #[serde(default)]
    pub assets: Vec<LinkedAsset>,
    /// Messages and notes, oldest first.
    #[serde(default)]
    pub timeline: Vec<TimelineEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketCustomer {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub is_help_desk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    pub id: Uuid,
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedAsset {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
}

/// One entry of a ticket's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TimelineEntry {
    /// A message received from, or sent to, the requester.
    Message(TimelineMessage),
    /// An internal remark, seen only by agents: never put it in a message to the requester.
    Note(TimelineNote),
    /// An entry of a kind this version of the SDK does not know.
    #[serde(other)]
    Other,
}

/// Whether the help desk received a message or sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMessage {
    pub id: Uuid,
    pub direction: Direction,
    pub from_address: String,
    #[serde(default)]
    pub from_name: Option<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body_text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineNote {
    pub id: Uuid,
    #[serde(default)]
    pub author_name: String,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
