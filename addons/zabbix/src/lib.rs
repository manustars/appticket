//! The Zabbix add-on: the hosts of the host groups customers have as external references become
//! assets of the help desk's catalog, with the add-on as their source.

pub mod config;
pub mod conflicts;
pub mod coverage;
pub mod kinds;
pub mod sync;
pub mod zabbix;
