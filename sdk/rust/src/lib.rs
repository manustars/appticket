//! Building blocks for add-ons of the help desk core, following the contract in `sdk/README.md`:
//!
//! - [`events`]: the envelope the core posts to `<url>/events` and the typed data of each kind;
//! - [`signature`]: checking the `hd-signature` of an event before trusting its body;
//! - [`Client`]: the add-on API, called with the add-on's token;
//! - [`proposals`]: what an add-on can propose to the agents;
//! - `server` (feature `axum`): a ready `/events` endpoint that verifies events for a handler.

pub mod api;
mod client;
pub mod events;
pub mod proposals;
#[cfg(feature = "axum")]
pub mod server;
pub mod signature;

pub use client::{Client, Error, http_client};
pub use events::{Envelope, Event, MessageReceived, TicketOpened};
pub use proposals::{Content, Rationale, Target, Triage};
pub use signature::{Verifier, VerifyError};
