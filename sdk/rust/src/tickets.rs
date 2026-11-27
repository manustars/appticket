//! What an add-on with the scope `tickets:write` sends to open a ticket, and what the core
//! answers.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::Priority;

/// A ticket without a requester for a customer, as [`crate::Client::open_ticket`] sends it.
/// Replies on it go to the customer's key requesters; the add-on itself never replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTicket {
    /// An active customer; an offboarded one gets no new tickets.
    pub customer_id: Uuid,
    pub title: String,
    /// What the add-on knows of the problem, such as the alert it saw. The core keeps it as the
    /// ticket's first internal note, by the add-on: seen by agents only, never sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Normal when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Active assets of the customer the ticket is about.
    #[serde(default)]
    pub asset_ids: Vec<Uuid>,
    /// Active services the customer uses that the ticket is about.
    #[serde(default)]
    pub service_ids: Vec<Uuid>,
    /// The add-on's own reference for the ticket, such as the id of the problem of its system the
    /// ticket is about: it names the ticket for good, so opening again with it opens no second
    /// ticket and answers the one opened then, unchanged ([`OpenedTicket::created`] is then
    /// false). At most 255 characters once trimmed, neither blank nor with control characters,
    /// compared exactly; another add-on's references are its own. `None` opens a new ticket each
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// What [`crate::Client::open_ticket`] answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenedTicket {
    pub number: i64,
    /// The ticket was opened now; false when the reference named a ticket the add-on opened
    /// before, which the core answered unchanged, in any state.
    pub created: bool,
}

impl NewTicket {
    /// A ticket for `customer` with only a title: no description, the normal priority, no assets
    /// or services, no reference.
    pub fn new(customer: Uuid, title: &str) -> Self {
        NewTicket {
            customer_id: customer,
            title: title.to_owned(),
            description: None,
            priority: None,
            asset_ids: Vec::new(),
            service_ids: Vec::new(),
            reference: None,
        }
    }
}
