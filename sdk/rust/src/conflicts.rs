//! The conflicts an add-on reports, such as a host of a monitoring tool in two groups that two
//! customers have as external references, which the add-on therefore does not import: an add-on
//! with the scope `conflicts:write` sends the whole list with [`crate::Client::replace_conflicts`],
//! and administrators read the latest one in the add-on's state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A conflict, with a sentence an administrator can act on: what is not imported and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// One sentence per language of the agents' UI, keyed by language tag (`{"en": …, "it": …}`),
    /// within the limits of the add-on contract (`sdk/README.md`); administrators whose language
    /// is missing read the English one.
    pub sentence: BTreeMap<String, String>,
}

impl Conflict {
    /// A conflict with a sentence in each of the languages given, as `(tag, sentence)`.
    pub fn new<L: Into<String>, S: Into<String>>(
        sentences: impl IntoIterator<Item = (L, S)>,
    ) -> Self {
        Conflict {
            sentence: sentences
                .into_iter()
                .map(|(language, sentence)| (language.into(), sentence.into()))
                .collect(),
        }
    }

    /// A conflict with only an English sentence.
    pub fn in_english(sentence: impl Into<String>) -> Self {
        Conflict::new([("en", sentence)])
    }
}
