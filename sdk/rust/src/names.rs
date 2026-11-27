//! The names of the add-on's system that agents pick customers' external references from, such as
//! a monitoring tool's host groups, which an add-on with the scope `names:write` sends with
//! [`crate::Client::replace_names`].

use serde::{Deserialize, Serialize};

/// The whole list of the names of the add-on's system: the core keeps the latest one, also while
/// the add-on is switched off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemNames {
    /// The add-on's system, such as `Watchtower`: the external references agents pick from the
    /// list are in it. A system's names come from one add-on, compared ignoring case.
    pub system: String,
    /// The names, in any order, each once ignoring case, within the limits of the add-on contract
    /// (`sdk/README.md`).
    pub names: Vec<SystemName>,
}

/// A name of the add-on's system, nested in another of the list or at the top: not blank once
/// trimmed, without control characters, and no longer than the add-on contract allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemName {
    pub name: String,
    /// The name it is nested in, another of the list's, compared ignoring case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl SystemName {
    /// A name nested in no other.
    pub fn top(name: impl Into<String>) -> Self {
        SystemName {
            name: name.into(),
            parent: None,
        }
    }

    /// A name nested in `parent`, another name of the list.
    pub fn nested_in(name: impl Into<String>, parent: impl Into<String>) -> Self {
        SystemName {
            name: name.into(),
            parent: Some(parent.into()),
        }
    }
}
