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
    /// The names other systems use for the customer, by system and name. Empty from a core that
    /// predates external references.
    #[serde(default)]
    pub external_references: Vec<ExternalReference>,
}

/// A name another system uses for a customer, such as a group in a monitoring tool. A name names
/// exactly one customer within its system; the core compares systems and names ignoring case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalReference {
    pub system: String,
    pub name: String,
}

/// An active asset of a customer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    pub customer_id: Uuid,
    /// The name of its asset kind, one of the list the help desk manages: laptop, printer,
    /// application, licence…
    pub kind: String,
    pub name: String,
    /// Empty when it has none, or from a core that predates serial numbers.
    #[serde(default)]
    pub serial_number: Option<String>,
    /// Its own site, one of its customer's; add-ons are not told its holder, nor the site it is at
    /// through them.
    #[serde(default)]
    pub site: Option<SiteRef>,
    /// The add-on it came from, when an add-on supplies it.
    #[serde(default)]
    pub source_addon: Option<String>,
    /// The add-on's own reference for it, when the add-on reading it is its source or linked to it.
    #[serde(default)]
    pub reference: Option<String>,
    /// The other add-ons linked to it, by name, besides its source: they change nothing of it and
    /// only supply live details and links. Empty from a core that predates linked add-ons.
    #[serde(default)]
    pub linked_addons: Vec<String>,
}

/// What an add-on supplies of an asset it is the source of (scope `catalog:write`): everything but
/// its customer, which never changes, and its holder, whom agents choose. What is left out is
/// cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuppliedDetails {
    /// The name of an asset kind of the help desk's list, compared ignoring case: the add-on maps
    /// its own kinds to that list.
    pub kind: String,
    pub name: String,
    pub serial_number: Option<String>,
    /// The name of one of the customer's active sites, compared ignoring case.
    pub site: Option<String>,
}

/// An asset of a customer as the add-on supplies it, known by the add-on's own reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuppliedAsset {
    /// The add-on's own identifier for the asset, such as a host's id in its system: the same
    /// reference always names the same asset, of the same customer.
    pub reference: String,
    pub customer_id: Uuid,
    #[serde(flatten)]
    pub details: SuppliedDetails,
}

/// What supplying an asset did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Supplied {
    /// The asset as the core stored it: created (201), or the one the add-on supplied before with
    /// that reference updated (200), or the one it is linked to by that reference, unchanged (200;
    /// its `source_addon` is then another add-on or none).
    Asset { asset: Asset, created: bool },
    /// Nothing was created (202): the customer already has an asset with the same serial number or
    /// name, and the core proposes to the agents that the add-on's asset is that one.
    LinkProposed(LinkProposal),
}

/// A proposal, waiting for the agents, that the asset an add-on would create is one its customer
/// already has, entered by hand or supplied by another add-on. Supplying the asset again while it
/// is open answers the same proposal, with the name and serial number just sent; once agents
/// discard it, the next attempt creates the asset, and once they accept it, the add-on is linked to
/// the asset that matched and its reference reaches that asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkProposal {
    pub id: Uuid,
    /// The add-on's reference for the asset it would supply.
    pub reference: String,
    pub customer_id: Uuid,
    /// The name and serial number the add-on sent.
    pub name: String,
    #[serde(default)]
    pub serial_number: Option<String>,
    /// What the two assets have in common.
    pub matched_by: MatchedBy,
    /// The asset it matched, and that asset's name.
    pub asset_id: Uuid,
    pub asset_name: String,
}

/// What an asset an add-on would create has in common with one its customer already has: the
/// serial number decides over the name, both compared ignoring case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    SerialNumber,
    Name,
}

/// A site of a customer, by id and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteRef {
    pub id: Uuid,
    pub name: String,
}

/// An active service a customer uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
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

/// A ticket that is not closed, as listed by `GET /api/addon/tickets` and
/// `GET /api/addon/tickets/opened`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSummary {
    pub number: i64,
    pub title: String,
    pub state: TicketState,
    pub priority: Priority,
    pub customer_id: Uuid,
    /// The address a reply goes to: the one the requester last wrote from on the ticket, else the
    /// one an agent last picked there, else the first they registered.
    #[serde(default)]
    pub requester_email: Option<String>,
    /// The add-on's ticket reference, when it opened it with one; never another
    /// add-on's.
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

fn worked_on_unless_said() -> bool {
    true
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
    /// The add-on that opened the ticket, by name, when an add-on did.
    #[serde(default)]
    pub opened_by_addon: Option<String>,
    /// The add-on's ticket reference, when it opened it with one; never another
    /// add-on's.
    #[serde(default)]
    pub reference: Option<String>,
    /// Whether an agent has worked on the ticket: once they have been its assignee, written a
    /// reply, a note or an announcement on it, or changed it by hand (its state, even to the one
    /// it had, as when attaching an inbox message to it; its priority, links or customer; a child
    /// attached to or detached from it, or it to or from a parent), and so for good; notes of
    /// add-ons and messages received do not count. The notes themselves are never given to
    /// add-ons. True from a core that does not say, so that an add-on resolving only tickets
    /// nobody worked on leaves them alone.
    #[serde(default = "worked_on_unless_said")]
    pub worked_on_by_agent: bool,
    /// Assets already linked to the ticket.
    #[serde(default)]
    pub assets: Vec<LinkedAsset>,
    /// Services already linked to the ticket.
    #[serde(default)]
    pub services: Vec<LinkedService>,
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

/// A requester has one or more e-mail addresses; any of them identifies them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    pub id: Uuid,
    /// The address that stands for the requester on this ticket, where a reply goes: the one
    /// they last wrote from on it, else the one an agent last picked there, else the first they
    /// registered.
    pub email: String,
    /// All the requester's addresses, the first registered first.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// The address the requester last wrote from on this ticket, where a reply goes; empty when
    /// they have not written on it.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// On a ticket the requester has not written on, the address an agent picked for the last
    /// reply there.
    #[serde(default)]
    pub last_picked: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedAsset {
    pub id: Uuid,
    /// The name of its asset kind.
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedService {
    pub id: Uuid,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(details: serde_json::Value) -> Result<Asset, serde_json::Error> {
        let mut json = serde_json::json!({
            "id": "6f1c2a52-6d0e-4bd6-9a5e-0c1f3f7b9a01",
            "customer_id": "0b7f6c1e-2a43-4e7d-8c55-3d2e1f0a9b02",
            "kind": "Laptop",
            "name": "PC-0042",
        });
        json.as_object_mut()
            .unwrap()
            .extend(details.as_object().unwrap().clone());
        serde_json::from_value(json)
    }

    #[test]
    fn an_asset_carries_its_serial_number_and_own_site_when_it_has_them() {
        let read = asset(serde_json::json!({
            "serial_number": "SN-0042",
            "site": {"id": "9d8e7f6a-5b4c-4d3e-8f2a-1b0c9d8e7f03", "name": "Head office"}
        }))
        .unwrap();
        assert_eq!(read.serial_number.as_deref(), Some("SN-0042"));
        assert_eq!(read.site.map(|s| s.name).as_deref(), Some("Head office"));

        let bare = asset(serde_json::json!({"serial_number": null, "site": null})).unwrap();
        assert_eq!((bare.serial_number, bare.site), (None, None));
        let older = asset(serde_json::json!({})).unwrap();
        assert_eq!(
            (older.serial_number, older.site),
            (None, None),
            "from a core that predates them"
        );
    }
}
