//! What an add-on writes for the agents in every language of their UI, such as a proposal's
//! rationale or a conflict: one sentence per language, so that each reads it in the language they
//! chose.

use std::collections::BTreeMap;

use serde::Deserialize;
use utoipa::ToSchema;

/// One sentence per language: `{"en": "...", "it": "..."}`.
pub type PerLanguage = BTreeMap<String, String>;

/// Sentences as an add-on sends them.
#[derive(Deserialize, ToSchema, Default)]
#[serde(untagged)]
pub enum SentenceInput {
    /// One sentence per language.
    PerLanguage(PerLanguage),
    /// A single sentence, taken as English.
    English(String),
    #[default]
    #[serde(skip)]
    None,
}

impl SentenceInput {
    /// The sentences by language; a single one as English, none when none was sent.
    pub fn into_map(self) -> PerLanguage {
        match self {
            SentenceInput::PerLanguage(map) => map,
            SentenceInput::English(text) => PerLanguage::from([("en".to_owned(), text)]),
            SentenceInput::None => PerLanguage::new(),
        }
    }
}

/// `sentences` with language tags of 2 to 8 lowercase letters, each sentence trimmed and the empty
/// ones dropped; refused, with a sentence naming the language, when a tag is not one or a sentence
/// is longer than `max` characters. `what` is what the sentences are, such as `rationale`.
pub fn clean(sentences: &PerLanguage, max: usize, what: &str) -> Result<PerLanguage, String> {
    let mut clean = PerLanguage::new();
    for (language, text) in sentences {
        let valid_tag =
            (2..=8).contains(&language.len()) && language.chars().all(|c| c.is_ascii_lowercase());
        if !valid_tag {
            return Err(format!(
                "{language:?} is not a language tag such as \"en\" or \"it\""
            ));
        }
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if text.chars().count() > max {
            return Err(format!(
                "the {language} {what} is longer than {max} characters"
            ));
        }
        clean.insert(language.clone(), text.to_owned());
    }
    Ok(clean)
}
