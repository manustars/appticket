//! Building blocks for add-ons of the help desk core, following the contract in `sdk/README.md`:
//!
//! - [`events`]: the envelope the core posts to `<url>/events` and the typed data of each kind;
//! - [`signature`]: checking the `hd-signature` of an event before trusting its body;
//! - [`Client`]: the add-on API, called with the add-on's token;
//! - [`proposals`]: what an add-on can propose to the agents;
//! - [`tickets`]: the tickets an add-on opens;
//! - [`names`]: the names of the add-on's system that agents pick external references from;
//! - [`conflicts`]: the conflicts the add-on reports, which those who manage add-ons read in its
//!   state;
//! - [`live_details`]: what the add-on answers when the core asks for an asset's live details;
//! - `server` (feature `axum`): a ready `/events` endpoint that verifies events for a handler,
//!   and a ready `/assets/{reference}` endpoint that verifies the core's reads of live details.

pub mod api;
mod client;
pub mod conflicts;
pub mod events;
pub mod live_details;
pub mod names;
pub mod proposals;
#[cfg(feature = "axum")]
pub mod server;
pub mod signature;
pub mod tickets;

pub use client::{Client, Error, http_client};
pub use conflicts::Conflict;
pub use events::{
    Envelope, Event, MessageReceived, StateChangedBy, TicketOpened, TicketStateChanged,
};
pub use live_details::{DetailState, Field, Link, LiveDetails};
pub use names::{SystemName, SystemNames};
pub use proposals::{Content, Rationale, Target, Triage};
pub use signature::{Verifier, VerifyError};
pub use tickets::NewTicket;
