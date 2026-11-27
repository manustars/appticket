//! The conflicts an add-on reports, such as a host of a monitoring tool in two groups that are
//! external references of two customers, which the add-on therefore does not import: the add-on
//! replaces the whole list and administrators read the latest one in its state. Rules are in
//! docs/domain-rules.md ("Add-ons").

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::error::{Result, rule};

/// The most conflicts a list holds.
pub const MAX_CONFLICTS: usize = 1000;
/// The most languages a sentence is written in.
pub const MAX_LANGUAGES: usize = 4;
/// The longest sentence, in characters, in each language.
pub const MAX_SENTENCE_LENGTH: usize = 500;

/// What a conflict is, one sentence per language: `{"en": "...", "it": "..."}`.
pub type Sentence = BTreeMap<String, String>;

/// A sentence as an add-on sends it.
#[derive(Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SentenceInput {
    /// One sentence per language.
    PerLanguage(Sentence),
    /// A single sentence, taken as English.
    English(String),
}

/// A conflict as the add-on reports it.
#[derive(Deserialize, ToSchema)]
pub struct ReportedConflict {
    /// What is not imported and why, so that an administrator can act on it: written in every
    /// language of the agents' UI, as a rationale is, or a single sentence taken as English.
    pub sentence: SentenceInput,
}

/// An add-on's latest list of conflicts, as administrators read it in the add-on's state.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConflictList {
    /// When the add-on last sent the list.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// The conflicts, in the order the add-on sent them.
    pub conflicts: Vec<Conflict>,
}

/// A conflict an add-on reports.
#[derive(Debug, Serialize, ToSchema)]
pub struct Conflict {
    /// What is not imported and why, in each language the add-on wrote it in.
    pub sentence: Sentence,
}

/// `sentence` with language tags of 2 to 8 lowercase letters, each text trimmed, empty ones
/// dropped, at least one left; `which` says which conflict it is.
fn clean(sentence: SentenceInput, which: usize) -> Result<Sentence> {
    let sentence = match sentence {
        SentenceInput::PerLanguage(sentence) => sentence,
        SentenceInput::English(text) => Sentence::from([("en".to_owned(), text)]),
    };
    let mut clean = Sentence::new();
    for (language, text) in sentence {
        let valid_tag =
            (2..=8).contains(&language.len()) && language.chars().all(|c| c.is_ascii_lowercase());
        if !valid_tag {
            return rule(format!(
                "conflict number {which}: {language:?} is not a language tag such as \"en\" or \
                 \"it\""
            ));
        }
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if text.chars().any(char::is_control) {
            return rule(format!(
                "conflict number {which}: the {language} sentence holds a control character"
            ));
        }
        if text.chars().count() > MAX_SENTENCE_LENGTH {
            return rule(format!(
                "conflict number {which}: the {language} sentence is longer than \
                 {MAX_SENTENCE_LENGTH} characters"
            ));
        }
        clean.insert(language, text.to_owned());
    }
    if clean.is_empty() {
        return rule(format!("conflict number {which} has no sentence"));
    }
    if clean.len() > MAX_LANGUAGES {
        return rule(format!(
            "conflict number {which} is written in more than {MAX_LANGUAGES} languages"
        ));
    }
    Ok(clean)
}

/// Replaces the list of conflicts `addon` reports with `conflicts`: the core keeps the latest
/// list. A list that does not fit is refused whole, and the one before stays.
pub async fn replace(db: &PgPool, addon: &str, conflicts: Vec<ReportedConflict>) -> Result<()> {
    if conflicts.len() > MAX_CONFLICTS {
        return rule(format!(
            "a list holds at most {MAX_CONFLICTS} conflicts, not {}",
            conflicts.len()
        ));
    }
    let sentences = conflicts
        .into_iter()
        .enumerate()
        .map(|(i, c)| clean(c.sentence, i + 1).map(sqlx::types::Json))
        .collect::<Result<Vec<_>>>()?;
    let mut tx = db.begin().await?;
    // Two lists of one add-on replace each other in turn.
    sqlx::query(
        "INSERT INTO conflict_lists (addon) VALUES ($1)
         ON CONFLICT (addon) DO UPDATE SET updated_at = now()",
    )
    .bind(addon)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM reported_conflicts WHERE addon = $1")
        .bind(addon)
        .execute(&mut *tx)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO reported_conflicts (addon, position, sentence)
         SELECT $1, i - 1, s FROM unnest($2::jsonb[]) WITH ORDINALITY AS u (s, i)",
    )
    .bind(addon)
    .bind(&sentences)
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => {}
        // The database's idea of white space and control characters can differ from the core's.
        Err(sqlx::Error::Database(e))
            if e.constraint() == Some("reported_conflicts_sentence_check") =>
        {
            return rule(
                "a conflict's sentence begins or ends with white space or holds a control \
                 character",
            );
        }
        Err(e) => return Err(e.into()),
    }
    tx.commit().await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ConflictRow {
    addon: String,
    updated_at: OffsetDateTime,
    sentence: Option<sqlx::types::Json<Sentence>>,
}

/// Every add-on's latest list of conflicts, by add-on.
pub async fn lists(db: &PgPool) -> Result<HashMap<String, ConflictList>> {
    let rows: Vec<ConflictRow> = sqlx::query_as(
        "SELECT l.addon, l.updated_at, c.sentence
         FROM conflict_lists l
         LEFT JOIN reported_conflicts c ON c.addon = l.addon
         ORDER BY l.addon, c.position",
    )
    .fetch_all(db)
    .await?;
    let mut lists: HashMap<String, ConflictList> = HashMap::new();
    for row in rows {
        let list = lists.entry(row.addon).or_insert_with(|| ConflictList {
            updated_at: row.updated_at,
            conflicts: Vec::new(),
        });
        if let Some(sentence) = row.sentence {
            list.conflicts.push(Conflict {
                sentence: sentence.0,
            });
        }
    }
    Ok(lists)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn english(text: &str) -> ReportedConflict {
        ReportedConflict {
            sentence: SentenceInput::English(text.to_owned()),
        }
    }

    fn per_language(texts: &[(&str, &str)]) -> ReportedConflict {
        ReportedConflict {
            sentence: SentenceInput::PerLanguage(sentence(texts)),
        }
    }

    fn sentence(texts: &[(&str, &str)]) -> Sentence {
        texts
            .iter()
            .map(|(l, t)| ((*l).to_owned(), (*t).to_owned()))
            .collect()
    }

    /// The sentences of each add-on's state, by add-on, none for an add-on that reported none.
    async fn reported(db: &PgPool) -> Vec<(String, Option<Vec<Sentence>>)> {
        crate::addons::list(db)
            .await
            .unwrap()
            .into_iter()
            .map(|a| {
                let sentences = a
                    .conflicts
                    .map(|l| l.conflicts.into_iter().map(|c| c.sentence).collect());
                (a.name, sentences)
            })
            .collect()
    }

    #[sqlx::test]
    async fn an_addon_replaces_its_list_of_conflicts(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        replace(
            &db,
            "watchtower",
            vec![
                per_language(&[
                    (
                        "en",
                        " The host db-03 is in Brightfield and Harbor Lane, which two customers \
                         have: it is not imported. ",
                    ),
                    (
                        "it",
                        "L'host db-03 è in Brightfield e Harbor Lane: non è importato.",
                    ),
                    ("de", " "),
                ]),
                english("The host web-07 is in no group a customer has: it is not imported."),
            ],
        )
        .await
        .unwrap();
        // Switched off, its latest list still shows.
        crate::addons::set_enabled(&db, "watchtower", false)
            .await
            .unwrap();
        assert_eq!(
            reported(&db).await,
            [
                ("netscope".to_owned(), None),
                (
                    "watchtower".to_owned(),
                    Some(vec![
                        sentence(&[
                            (
                                "en",
                                "The host db-03 is in Brightfield and Harbor Lane, which two \
                                 customers have: it is not imported."
                            ),
                            (
                                "it",
                                "L'host db-03 è in Brightfield e Harbor Lane: non è importato."
                            )
                        ]),
                        sentence(&[(
                            "en",
                            "The host web-07 is in no group a customer has: it is not imported."
                        )]),
                    ])
                ),
            ]
        );

        // The latest list replaces the whole of the one before; an empty one says there are none.
        replace(
            &db,
            "watchtower",
            vec![english("The host db-03 is in two groups.")],
        )
        .await
        .unwrap();
        assert_eq!(
            reported(&db).await[1].1,
            Some(vec![sentence(&[(
                "en",
                "The host db-03 is in two groups."
            )])])
        );
        replace(&db, "watchtower", vec![]).await.unwrap();
        assert_eq!(reported(&db).await[1].1, Some(vec![]));
    }

    #[sqlx::test]
    async fn a_list_of_conflicts_that_does_not_fit_is_refused_and_the_one_before_kept(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        replace(
            &db,
            "watchtower",
            vec![english("The host db-03 is in two groups.")],
        )
        .await
        .unwrap();
        let long = "x".repeat(MAX_SENTENCE_LENGTH + 1);
        let many = || {
            (0..=MAX_CONFLICTS)
                .map(|i| english(&format!("Host {i}.")))
                .collect()
        };
        let refusals: [(Vec<ReportedConflict>, &str); 7] = [
            (
                vec![english("Fine."), english("  ")],
                "conflict number 2 has no sentence",
            ),
            (vec![per_language(&[])], "conflict number 1 has no sentence"),
            (
                vec![per_language(&[("EN", "Host db-03.")])],
                r#"conflict number 1: "EN" is not a language tag such as "en" or "it""#,
            ),
            (
                vec![english("Host\ndb-03.")],
                "conflict number 1: the en sentence holds a control character",
            ),
            (
                vec![english(&long)],
                "conflict number 1: the en sentence is longer than 500 characters",
            ),
            (
                vec![per_language(&[
                    ("en", "Host."),
                    ("it", "Host."),
                    ("de", "Host."),
                    ("fr", "Host."),
                    ("es", "Host."),
                ])],
                "conflict number 1 is written in more than 4 languages",
            ),
            (many(), "a list holds at most 1000 conflicts, not 1001"),
        ];
        for (conflicts, why) in refusals {
            assert_eq!(
                testing::refusal(replace(&db, "watchtower", conflicts).await),
                why
            );
        }
        assert_eq!(
            reported(&db).await,
            [(
                "watchtower".to_owned(),
                Some(vec![sentence(&[(
                    "en",
                    "The host db-03 is in two groups."
                )])])
            )],
            "the list before stays"
        );
        // As many conflicts as a list holds, each as long as it comes in as many languages.
        let longest = "é".repeat(MAX_SENTENCE_LENGTH);
        let full: Vec<ReportedConflict> = (0..MAX_CONFLICTS)
            .map(|_| {
                per_language(&[
                    ("en", &longest),
                    ("it", &longest),
                    ("de", &longest),
                    ("fr", &longest),
                ])
            })
            .collect();
        replace(&db, "watchtower", full).await.unwrap();
        assert_eq!(
            reported(&db).await[0].1.as_ref().map(Vec::len),
            Some(MAX_CONFLICTS)
        );
    }

    /// The database keeps the rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_rules_of_a_list_of_conflicts(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        assert!(
            sqlx::query("INSERT INTO conflict_lists (addon) VALUES ('nobody')")
                .execute(&db)
                .await
                .is_err(),
            "no such add-on"
        );
        sqlx::query("INSERT INTO conflict_lists (addon) VALUES ('watchtower')")
            .execute(&db)
            .await
            .unwrap();
        let insert = |position: i32, sentence: serde_json::Value| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO reported_conflicts (addon, position, sentence)
                     VALUES ('watchtower', $1, $2)",
                )
                .bind(position)
                .bind(sentence)
                .execute(&db)
                .await
            }
        };
        use serde_json::json;
        let long = "x".repeat(501);
        for (sentence, why) in [
            (json!("Host db-03."), "not per language"),
            (json!({}), "no sentence"),
            (json!({"en": ""}), "blank"),
            (json!({"en": " Host db-03."}), "untrimmed"),
            (json!({"en": "Host\tdb-03."}), "a control character"),
            (json!({"en": long}), "too long"),
            (json!({"EN": "Host db-03."}), "not a language tag"),
            (json!({"en": 3}), "not a text"),
            (
                json!({"en": "H.", "it": "H.", "de": "H.", "fr": "H.", "es": "H."}),
                "too many languages",
            ),
        ] {
            assert!(insert(0, sentence).await.is_err(), "{why}");
        }
        assert!(
            insert(1000, json!({"en": "Host db-03."})).await.is_err(),
            "past the most a list holds"
        );
        assert!(
            insert(0, json!({"en": "Host db-03.", "it": "Host db-03."}))
                .await
                .is_ok()
        );
        assert!(
            insert(0, json!({"en": "Host web-07."})).await.is_err(),
            "two at one place"
        );
        assert!(insert(999, json!({"en": "Host web-07."})).await.is_ok());
    }
}
