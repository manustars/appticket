//! What an add-on with the scope `tickets:write` sends to open a ticket.

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
}

impl NewTicket {
    /// A ticket for `customer` with only a title: no description, the normal priority, no assets
    /// or services.
    pub fn new(customer: Uuid, title: &str) -> Self {
        NewTicket {
            customer_id: customer,
            title: title.to_owned(),
            description: None,
            priority: None,
            asset_ids: Vec::new(),
            service_ids: Vec::new(),
        }
    }
}
