//! The Zabbix add-on: the hosts of the host groups customers have as external references become
//! assets of the help desk's catalog, with the add-on as their source.

pub mod alerts;
pub mod check;
pub mod claims;
pub mod config;
pub mod conflicts;
pub mod coverage;
pub mod details;
pub mod hold;
pub mod kinds;
pub mod reconcile;
pub mod references;
pub mod rules;
pub mod safety;
pub mod sync;
pub mod text;
pub mod webhook;
pub mod zabbix;
