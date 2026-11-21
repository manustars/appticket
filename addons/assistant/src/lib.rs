//! The Assistant (ADR 0006): an add-on that reads incoming messages and proposes what to do with
//! them. It never acts by itself; every proposal waits for an agent.

pub mod assistant;
pub mod config;
pub mod model;
pub mod prompts;
pub mod server;

pub use assistant::{Assistant, Failure, Proposed};
