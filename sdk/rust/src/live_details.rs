//! The live details of an asset, which the core asks the add-on an asset comes from and each
//! add-on linked to it for as `GET <url>/assets/{reference}` whenever agents look at the asset,
//! and waits 2 seconds for.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How long the core waits for the answer, from connecting to its last byte.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// A label per language of the agents' UI, keyed by language tag: `{"en": "Disk", "it":
/// "Disco"}`. Agents whose language is missing read the English one.
pub type Label = BTreeMap<String, String>;

/// A label with only an English text.
pub fn in_english(text: impl Into<String>) -> Label {
    Label::from([("en".to_owned(), text.into())])
}

/// What the add-on says of an asset now. The core shows it beside the asset only as a whole:
/// at most 20 fields and 10 links, labels of at most 100 characters, values of at most 500 and
/// `http` or `https` links of at most 2000, all within 64 KiB; anything else is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveDetails {
    pub state: DetailState,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub links: Vec<Link>,
}

/// How the asset is doing, as the add-on sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailState {
    Ok,
    Warning,
    Problem,
}

/// A value with its label, such as "Disk /var: 91% full".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub label: Label,
    pub value: String,
}

/// A link into the add-on's system, such as the asset's page there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub label: Label,
    /// `http` or `https`.
    pub url: String,
}

impl Field {
    pub fn new(label: Label, value: impl Into<String>) -> Self {
        Field {
            label,
            value: value.into(),
        }
    }
}

impl Link {
    pub fn new(label: Label, url: impl Into<String>) -> Self {
        Link {
            label,
            url: url.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn live_details_have_the_core_json() {
        let details = LiveDetails {
            state: DetailState::Warning,
            fields: vec![Field::new(
                Label::from([
                    ("en".to_owned(), "Disk /var".to_owned()),
                    ("it".to_owned(), "Disco /var".to_owned()),
                ]),
                "91% full",
            )],
            links: vec![Link::new(
                in_english("Open in Watchtower"),
                "https://watchtower.example.com/hosts/1042",
            )],
        };
        let expected = json!({
            "state": "warning",
            "fields": [{"label": {"en": "Disk /var", "it": "Disco /var"}, "value": "91% full"}],
            "links": [{"label": {"en": "Open in Watchtower"},
                       "url": "https://watchtower.example.com/hosts/1042"}]
        });
        assert_eq!(serde_json::to_value(&details).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<LiveDetails>(expected).unwrap(),
            details
        );
        assert_eq!(
            serde_json::from_value::<LiveDetails>(json!({"state": "ok"})).unwrap(),
            LiveDetails {
                state: DetailState::Ok,
                fields: vec![],
                links: vec![]
            }
        );
    }
}
