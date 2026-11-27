//! The names of an add-on's system that customers' external references are chosen from, such as a
//! monitoring tool's host groups, with their nesting. Rules are in docs/domain-rules.md
//! ("Add-ons").

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::catalog::CustomerRef;
use crate::error::{Result, rule};

/// The most names a list holds.
pub const MAX_NAMES: usize = 10_000;
/// The longest name, in characters.
pub const MAX_NAME_LENGTH: usize = 255;
/// The longest name of a system, in characters.
pub const MAX_SYSTEM_LENGTH: usize = 100;

/// A name of the add-on's system as the add-on sends it, such as a host group of a monitoring
/// tool: trimmed, it is not blank, holds no control character, is no longer than the core takes
/// and appears once in the list, ignoring case.
#[derive(Deserialize, ToSchema)]
pub struct SystemName {
    pub name: String,
    /// The name it is nested in, another of the list's, compared ignoring case; none at the top.
    #[serde(default)]
    pub parent: Option<String>,
}

/// An add-on's latest name list: the names of its system, as agents read them to pick a
/// customer's external references.
#[derive(Serialize, ToSchema)]
pub struct NameList {
    /// The add-on that lists them.
    pub addon: String,
    /// The system external references picked from the list are in.
    pub system: String,
    /// When the add-on last sent the list.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Every name followed by those nested in it; the names of one level by name, ignoring case
    /// and accents.
    pub names: Vec<ListedName>,
}

/// A name of a name list, as agents read it.
#[derive(Serialize, ToSchema)]
pub struct ListedName {
    /// The name, as the add-on spells it.
    pub name: String,
    /// The name it is nested in, listed before it; none at the top.
    pub parent: Option<String>,
    /// The customer that has it as an external reference, compared ignoring case, offboarded
    /// customers included; none when no customer has it. A name a customer has cannot be picked
    /// again.
    pub customer: Option<CustomerRef>,
    /// How deep it is nested: 0 at the top, one more at each level below.
    #[serde(skip)]
    pub(crate) depth: i32,
}

/// An add-on's latest name list as those who manage add-ons see it in the add-on's state: the names
/// agents still have to give a customer.
#[derive(Debug, Serialize, ToSchema)]
pub struct NameListState {
    /// The system the list names.
    pub system: String,
    /// When the add-on last sent the list.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// How many names the list holds.
    pub name_count: usize,
    /// The names no customer's external reference covers, neither the name's own nor that of a
    /// name it is nested in: every name followed by those nested in it, as agents read the list.
    pub uncovered: Vec<UncoveredName>,
}

/// A name of a name list that no external reference covers.
#[derive(Debug, Serialize, ToSchema)]
pub struct UncoveredName {
    /// The name, as the add-on spells it.
    pub name: String,
    /// The name it is nested in; none at the top.
    pub parent: Option<String>,
    /// How deep it is nested: 0 at the top, one more at each level below.
    pub depth: i32,
}

/// A name picked from an add-on's list, spelled as the list spells it and its system.
pub(crate) struct Picked {
    pub system: String,
    pub name: String,
}

/// A name of a list, checked, with where it sits in the nesting.
struct Checked {
    id: Uuid,
    name: String,
    parent: Option<usize>,
    depth: Option<i32>,
}

/// `text` trimmed, refused when it is blank, holds a control character or is longer than `max`
/// characters; `what` says what it is.
fn trimmed(text: &str, max: usize, what: impl Fn() -> String) -> Result<&str> {
    let text = text.trim();
    if text.is_empty() {
        return rule(format!("{} cannot be blank", what()));
    }
    if text.chars().any(char::is_control) {
        return rule(format!("{} holds a control character", what()));
    }
    if text.chars().count() > max {
        return rule(format!("{} is longer than {max} characters", what()));
    }
    Ok(text)
}

/// The names of `names`, trimmed, each once ignoring case and nested only in another of them,
/// with their depth; refused, with a sentence naming what does not fit, otherwise.
fn check(names: &[SystemName]) -> Result<Vec<Checked>> {
    if names.len() > MAX_NAMES {
        return rule(format!(
            "a list holds at most {MAX_NAMES} names, not {}",
            names.len()
        ));
    }
    let mut checked: Vec<Checked> = Vec::with_capacity(names.len());
    let mut by_key: HashMap<String, usize> = HashMap::with_capacity(names.len());
    for (i, listed) in names.iter().enumerate() {
        let name = trimmed(&listed.name, MAX_NAME_LENGTH, || {
            format!("the listed name {:?} (number {})", listed.name, i + 1)
        })?;
        if let Some(&first) = by_key.get(&name.to_lowercase()) {
            return rule(twice(name, &checked[first].name));
        }
        by_key.insert(name.to_lowercase(), checked.len());
        checked.push(Checked {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            parent: None,
            depth: None,
        });
    }
    for (i, listed) in names.iter().enumerate() {
        let Some(parent) = &listed.parent else {
            continue;
        };
        let parent = parent.trim();
        let Some(&p) = by_key.get(&parent.to_lowercase()) else {
            return rule(format!(
                "the name {:?} is nested in {parent:?}, which the list does not name",
                checked[i].name
            ));
        };
        if p == i {
            return rule(format!(
                "the name {:?} cannot be nested in itself",
                checked[i].name
            ));
        }
        checked[i].parent = Some(p);
    }
    // Each name's depth, walking up to a name whose depth is known or to the top; a name met
    // twice on the way up is nested in itself.
    for start in 0..checked.len() {
        let mut path = Vec::new();
        let mut at = Some(start);
        let mut depth = -1;
        while let Some(i) = at {
            if let Some(known) = checked[i].depth {
                depth = known;
                break;
            }
            if path.contains(&i) {
                return rule(format!(
                    "the name {:?} is nested in itself, through the names it is nested in",
                    checked[i].name
                ));
            }
            path.push(i);
            at = checked[i].parent;
        }
        for &i in path.iter().rev() {
            depth += 1;
            checked[i].depth = Some(depth);
        }
    }
    Ok(checked)
}

fn twice(name: &str, first: &str) -> String {
    format!("the list names {name:?} twice, once as {first:?}")
}

/// Replaces the name list of `addon`'s system with `names`, under the system's name `system`: the
/// core keeps the latest list. A system's names come from one add-on, compared ignoring case.
/// The customers' external references stay as they are.
pub async fn replace(db: &PgPool, addon: &str, system: &str, names: &[SystemName]) -> Result<()> {
    let system = trimmed(system, MAX_SYSTEM_LENGTH, || {
        "the name of the system".to_owned()
    })?;
    let checked = check(names)?;
    let mut tx = db.begin().await?;
    // Two lists of one add-on replace each other in turn.
    let upserted = sqlx::query(
        "INSERT INTO name_lists (addon, system) VALUES ($1, $2)
         ON CONFLICT (addon) DO UPDATE SET system = EXCLUDED.system, updated_at = now()",
    )
    .bind(addon)
    .bind(system)
    .execute(&mut *tx)
    .await;
    match upserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("name_lists_system") => {
            drop(tx);
            let holder: Option<(String, String)> = sqlx::query_as(
                "SELECT addon, system FROM name_lists WHERE lower(system) = lower($1)",
            )
            .bind(system)
            .fetch_optional(db)
            .await?;
            return match holder {
                Some((holder, spelled)) => rule(format!(
                    "the add-on {holder} lists the names of {spelled}: a system's names come \
                     from one add-on"
                )),
                // The other list was given another system in the meantime.
                None => rule(format!(
                    "another add-on listed the names of {system} a moment ago: try again"
                )),
            };
        }
        // The database's idea of white space can differ from the core's.
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("name_lists_system_check") => {
            return rule(format!(
                "the name of the system {system:?} begins or ends with white space or holds a \
                 control character"
            ));
        }
        Err(e) => return Err(e.into()),
    }
    sqlx::query("DELETE FROM listed_names WHERE addon = $1")
        .bind(addon)
        .execute(&mut *tx)
        .await?;
    let ids: Vec<Uuid> = checked.iter().map(|c| c.id).collect();
    let names: Vec<&str> = checked.iter().map(|c| c.name.as_str()).collect();
    let parents: Vec<Option<Uuid>> = checked
        .iter()
        .map(|c| c.parent.map(|p| checked[p].id))
        .collect();
    let depths: Vec<i32> = checked.iter().map(|c| c.depth.unwrap_or(0)).collect();
    let inserted = sqlx::query(
        "INSERT INTO listed_names (id, addon, name, parent_id, depth, parent_depth)
         SELECT id, $1, name, parent_id, depth,
                CASE WHEN parent_id IS NULL THEN NULL ELSE depth - 1 END
         FROM unnest($2::uuid[], $3::text[], $4::uuid[], $5::int[])
              AS n (id, name, parent_id, depth)",
    )
    .bind(addon)
    .bind(&ids)
    .bind(&names)
    .bind(&parents)
    .bind(&depths)
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) => {
            let constraint = e.constraint().map(str::to_owned);
            drop(tx);
            return Err(
                match refused_by_the_database(db, constraint.as_deref(), &names).await? {
                    Some(refused) => refused,
                    None => sqlx::Error::Database(e).into(),
                },
            );
        }
        Err(e) => return Err(e.into()),
    }
    tx.commit().await?;
    Ok(())
}

/// Why the database refused `names`, which the core let through: it compares names ignoring case
/// and tells white space and control characters by rules of its own, which can differ from the
/// core's for some characters.
async fn refused_by_the_database(
    db: &PgPool,
    constraint: Option<&str>,
    names: &[&str],
) -> Result<Option<crate::error::Error>> {
    use crate::error::Error;
    match constraint {
        Some("listed_names_name") => {
            let pair: Option<(String, String)> = sqlx::query_as(
                "SELECT (array_agg(n ORDER BY i))[2], (array_agg(n ORDER BY i))[1]
                 FROM unnest($1::text[]) WITH ORDINALITY AS u (n, i)
                 GROUP BY lower(n) COLLATE \"C\" HAVING count(*) > 1
                 ORDER BY min(i) LIMIT 1",
            )
            .bind(names)
            .fetch_optional(db)
            .await?;
            Ok(Some(Error::Rule(match pair {
                Some((name, first)) => twice(&name, &first),
                None => "a name is listed twice, ignoring case".to_owned(),
            })))
        }
        Some("listed_names_name_check") => {
            let name: Option<String> = sqlx::query_scalar(
                "SELECT n FROM unnest($1::text[]) n
                 WHERE NOT (n ~ '^\\S(.*\\S)?$' AND n !~ '[[:cntrl:]]') LIMIT 1",
            )
            .bind(names)
            .fetch_optional(db)
            .await?;
            Ok(Some(Error::Rule(format!(
                "the listed name {:?} begins or ends with white space or holds a control character",
                name.unwrap_or_default()
            ))))
        }
        _ => Ok(None),
    }
}

/// `name` picked in `system` from the name list of the add-on that lists that system's names,
/// both compared ignoring case: `None` when no add-on lists that system's names, and refused when
/// the add-on does not list `name`.
pub(crate) async fn pick_listed(
    tx: &mut Transaction<'_, Postgres>,
    system: &str,
    name: &str,
) -> Result<Option<Picked>> {
    let found: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT l.addon, l.system, n.name FROM name_lists l
         LEFT JOIN listed_names n
                ON n.addon = l.addon AND lower(n.name) COLLATE \"C\" = lower($2) COLLATE \"C\"
         WHERE lower(l.system) = lower($1)",
    )
    .bind(system)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await?;
    match found {
        None => Ok(None),
        Some((_, system, Some(name))) => Ok(Some(Picked { system, name })),
        Some((addon, system, None)) => rule(format!(
            "the name {name:?} is not in the name list the add-on {addon} sends of {system}"
        )),
    }
}

/// A name of a list, as tests write it, with the name it is nested in.
#[cfg(test)]
pub(crate) fn named(name: &str, parent: Option<&str>) -> SystemName {
    SystemName {
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
    }
}

#[derive(sqlx::FromRow)]
struct ListRow {
    addon: String,
    system: String,
    updated_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct NameRow {
    addon: String,
    id: Uuid,
    name: String,
    parent_id: Option<Uuid>,
    depth: i32,
    customer_id: Option<Uuid>,
    customer_name: Option<String>,
    is_help_desk: Option<bool>,
}

/// Every add-on's latest list, by system; in each, every name followed by those nested in it, the
/// names of one level by name, ignoring case and accents.
pub async fn all(db: &PgPool) -> Result<Vec<NameList>> {
    let lists: Vec<ListRow> = sqlx::query_as(
        "SELECT addon, system, updated_at FROM name_lists ORDER BY lower(system), addon",
    )
    .fetch_all(db)
    .await?;
    let rows: Vec<NameRow> = sqlx::query_as(
        "SELECT n.addon, n.id, n.name, n.parent_id, n.depth,
                c.id AS customer_id, c.name AS customer_name, c.is_help_desk
         FROM listed_names n
         JOIN name_lists l ON l.addon = n.addon
         LEFT JOIN external_references r
                ON lower(r.system) = lower(l.system) AND lower(r.name) = lower(n.name)
         LEFT JOIN customers c ON c.id = r.customer_id",
    )
    .fetch_all(db)
    .await?;
    let mut by_addon: HashMap<String, Vec<NameRow>> = HashMap::new();
    for row in rows {
        by_addon.entry(row.addon.clone()).or_default().push(row);
    }
    Ok(lists
        .into_iter()
        .map(|list| NameList {
            names: in_tree_order(by_addon.remove(&list.addon).unwrap_or_default()),
            addon: list.addon,
            system: list.system,
            updated_at: list.updated_at,
        })
        .collect())
}

/// Each add-on's latest name list, by add-on, with the names no external reference covers. A name
/// an offboarded customer has is covered: it cannot be picked for another customer.
pub async fn states(db: &PgPool) -> Result<HashMap<String, NameListState>> {
    Ok(all(db)
        .await?
        .into_iter()
        .map(|list| {
            let uncovered = list
                .names
                .iter()
                .zip(covered_by(&list.names))
                .filter(|(_, covering)| covering.is_none())
                .map(|(listed, _)| UncoveredName {
                    name: listed.name.clone(),
                    parent: listed.parent.clone(),
                    depth: listed.depth,
                })
                .collect();
            let state = NameListState {
                system: list.system,
                updated_at: list.updated_at,
                name_count: list.names.len(),
                uncovered,
            };
            (list.addon, state)
        })
        .collect())
}

/// The customer whose external reference covers each of `names`, which [`all`] lists: the
/// customer that has the name itself, else the one that covers the name it is nested in, so that a
/// subgroup another customer has is that customer's, with the names nested in it; none when no
/// external reference covers it. The names may come in any order.
pub(crate) fn covered_by(names: &[ListedName]) -> Vec<Option<&CustomerRef>> {
    let index: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name.as_str(), i))
        .collect();
    let mut covering: Vec<Option<Option<&CustomerRef>>> = vec![None; names.len()];
    for start in 0..names.len() {
        // Up from the name to one whose covering is known, one a customer has, or the top; a
        // list never nests a name in itself, which the walk stops at all the same.
        let mut path = Vec::new();
        let mut found = None;
        let mut at = Some(start);
        while let Some(i) = at {
            if let Some(known) = covering[i] {
                found = known;
                break;
            }
            path.push(i);
            if let Some(customer) = &names[i].customer {
                found = Some(customer);
                break;
            }
            if path.len() > names.len() {
                break;
            }
            at = names[i]
                .parent
                .as_deref()
                .and_then(|p| index.get(p).copied());
        }
        for i in path {
            covering[i] = Some(found);
        }
    }
    covering.into_iter().map(Option::flatten).collect()
}

/// What names are put in order by: without accents and ignoring case, so that `Élan` comes
/// between `Delta` and `Foxtrot`, then as written. The core orders them rather than the
/// database, whose collation can take milliseconds to compare two long names of characters it
/// gives no weight to, such as emoji, where a list has thousands of them.
fn by_name(name: &str) -> (String, String) {
    let plain = name
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect();
    (plain, name.to_owned())
}

/// `rows` each followed by those nested in it, the names of one level by name.
fn in_tree_order(rows: Vec<NameRow>) -> Vec<ListedName> {
    let names: HashMap<Uuid, &str> = rows.iter().map(|r| (r.id, r.name.as_str())).collect();
    let mut below: HashMap<Option<Uuid>, Vec<&NameRow>> = HashMap::new();
    for row in &rows {
        below.entry(row.parent_id).or_default().push(row);
    }
    for level in below.values_mut() {
        level.sort_by_cached_key(|row| by_name(&row.name));
    }
    let children = |parent: Option<Uuid>| {
        below
            .get(&parent)
            .into_iter()
            .flatten()
            .rev()
            .copied()
            .collect::<Vec<_>>()
    };
    let mut ordered = Vec::with_capacity(rows.len());
    let mut stack = children(None);
    while let Some(row) = stack.pop() {
        ordered.push(ListedName {
            name: row.name.clone(),
            parent: row
                .parent_id
                .and_then(|p| names.get(&p).map(|n| (*n).to_owned())),
            customer: match (row.customer_id, &row.customer_name) {
                (Some(id), Some(name)) => Some(CustomerRef {
                    id,
                    name: name.clone(),
                    is_help_desk: row.is_help_desk.unwrap_or(false),
                }),
                _ => None,
            },
            depth: row.depth,
        });
        stack.extend(children(Some(row.id)));
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    /// Each list as (system, [(name, parent)]), in the order agents read it.
    async fn read(db: &PgPool) -> Vec<(String, String, Vec<(String, Option<String>)>)> {
        all(db)
            .await
            .unwrap()
            .into_iter()
            .map(|l| {
                (
                    l.addon,
                    l.system,
                    l.names.into_iter().map(|n| (n.name, n.parent)).collect(),
                )
            })
            .collect()
    }

    fn pairs(names: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        names
            .iter()
            .map(|(n, p)| ((*n).to_owned(), p.map(str::to_owned)))
            .collect()
    }

    #[sqlx::test]
    async fn an_addon_replaces_its_list_of_names(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        replace(
            &db,
            "watchtower",
            " Watchtower ",
            &[
                named("Harbor Lane", None),
                named(" Brightfield/Offices ", Some("brightfield")),
                named("Brightfield", None),
                named("Brightfield/Lab", Some("Brightfield")),
            ],
        )
        .await
        .unwrap();
        // Each name below the one it is nested in, names of one level by name.
        assert_eq!(
            read(&db).await,
            [(
                "watchtower".to_owned(),
                "Watchtower".to_owned(),
                pairs(&[
                    ("Brightfield", None),
                    ("Brightfield/Lab", Some("Brightfield")),
                    ("Brightfield/Offices", Some("Brightfield")),
                    ("Harbor Lane", None),
                ])
            )]
        );

        // The latest list replaces the whole of the one before, its system's name too.
        replace(
            &db,
            "watchtower",
            "Watchtower EU",
            &[
                named("Harbor Lane", None),
                named("Harbor Lane/Docks", Some("Harbor Lane")),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            read(&db).await,
            [(
                "watchtower".to_owned(),
                "Watchtower EU".to_owned(),
                pairs(&[
                    ("Harbor Lane", None),
                    ("Harbor Lane/Docks", Some("Harbor Lane"))
                ])
            )]
        );
        replace(&db, "watchtower", "Watchtower EU", &[])
            .await
            .unwrap();
        assert_eq!(
            read(&db).await,
            [("watchtower".to_owned(), "Watchtower EU".to_owned(), vec![])]
        );
    }

    #[sqlx::test]
    async fn names_of_one_level_are_read_by_name_ignoring_case_and_accents(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        replace(
            &db,
            "watchtower",
            "Watchtower",
            &[
                named("foxtrot", None),
                named("Élan", None),
                named("delta", None),
                named("Zulu", None),
                named("Ölberg", Some("Zulu")),
                named("alpha", Some("Zulu")),
            ],
        )
        .await
        .unwrap();
        let names: Vec<String> = all(&db)
            .await
            .unwrap()
            .remove(0)
            .names
            .into_iter()
            .map(|n| n.name)
            .collect();
        assert_eq!(
            names,
            ["delta", "Élan", "foxtrot", "Zulu", "alpha", "Ölberg"]
        );
    }

    #[sqlx::test]
    async fn agents_read_which_customer_each_listed_name_names(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        // Typed by hand before the add-on listed it, in another case.
        crate::external_references::add(&db, harbor, "WATCHTOWER", "harbor lane")
            .await
            .unwrap();
        testing::addon(&db, "watchtower").await;
        replace(
            &db,
            "watchtower",
            "Watchtower",
            &[
                named("Brightfield", None),
                named("Brightfield/Offices", Some("Brightfield")),
                named("Harbor Lane", None),
            ],
        )
        .await
        .unwrap();
        crate::external_references::add(&db, brightfield, "Watchtower", "Brightfield")
            .await
            .unwrap();
        // Offboarded, it keeps its names.
        crate::customers::offboard(&db, harbor).await.unwrap();

        let named_customers: Vec<(String, Option<String>)> = all(&db)
            .await
            .unwrap()
            .remove(0)
            .names
            .into_iter()
            .map(|n| (n.name, n.customer.map(|c| c.name)))
            .collect();
        assert_eq!(
            named_customers,
            [
                ("Brightfield".to_owned(), Some("Brightfield".to_owned())),
                ("Brightfield/Offices".to_owned(), None),
                ("Harbor Lane".to_owned(), Some("Harbor Lane".to_owned())),
            ]
        );
    }

    #[sqlx::test]
    async fn a_name_is_covered_by_its_own_reference_or_the_nearest_one_above(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let old_quay = testing::customer(&db, "Old Quay").await;
        testing::addon(&db, "watchtower").await;
        replace(
            &db,
            "watchtower",
            "Watchtower",
            &[
                named("Brightfield", None),
                named("Brightfield/Offices", Some("Brightfield")),
                named("Brightfield/Offices/Floor 2", Some("Brightfield/Offices")),
                named("Brightfield/Docks", Some("Brightfield")),
                named("Brightfield/Docks/Crane", Some("Brightfield/Docks")),
                named("Brightfield/Archive", Some("Brightfield")),
                named("Brightfield/Archive/Rack", Some("Brightfield/Archive")),
                named("Unclaimed", None),
                named("Unclaimed/Lab", Some("Unclaimed")),
                named("Unclaimed/Annex", Some("Unclaimed")),
            ],
        )
        .await
        .unwrap();
        for (customer, name) in [
            (brightfield, "brightfield"),
            // A subgroup of Brightfield's the same customer has too.
            (brightfield, "Brightfield/Offices"),
            // A subgroup of Brightfield's another customer has.
            (harbor, "Brightfield/Docks"),
            // A subgroup of Brightfield's a customer since offboarded has.
            (old_quay, "Brightfield/Archive"),
            // A subgroup of a name no customer has.
            (brightfield, "Unclaimed/Lab"),
        ] {
            crate::external_references::add(&db, customer, "Watchtower", name)
                .await
                .unwrap();
        }
        crate::customers::offboard(&db, old_quay).await.unwrap();

        let mut list = all(&db).await.unwrap().remove(0);
        let covering = |names: &[ListedName]| -> Vec<(String, Option<String>)> {
            names
                .iter()
                .zip(covered_by(names))
                .map(|(n, c)| (n.name.clone(), c.map(|c| c.name.clone())))
                .collect()
        };
        let expected = [
            ("Brightfield", Some("Brightfield")),
            ("Brightfield/Archive", Some("Old Quay")),
            ("Brightfield/Archive/Rack", Some("Old Quay")),
            ("Brightfield/Docks", Some("Harbor Lane")),
            ("Brightfield/Docks/Crane", Some("Harbor Lane")),
            ("Brightfield/Offices", Some("Brightfield")),
            ("Brightfield/Offices/Floor 2", Some("Brightfield")),
            ("Unclaimed", None),
            ("Unclaimed/Annex", None),
            ("Unclaimed/Lab", Some("Brightfield")),
        ];
        assert_eq!(covering(&list.names), pairs(&expected));
        // In any order: each name below the one it is nested in, or above it.
        list.names.reverse();
        let mut reversed = covering(&list.names);
        reversed.reverse();
        assert_eq!(reversed, pairs(&expected));
    }

    #[sqlx::test]
    async fn an_addon_s_state_lists_the_names_no_external_reference_covers(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let old_quay = testing::customer(&db, "Old Quay").await;
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        replace(
            &db,
            "watchtower",
            "Watchtower",
            &[
                named("Unclaimed", None),
                named("Unclaimed/Lab", Some("Unclaimed")),
                named("Unclaimed/Annex", Some("Unclaimed")),
                named("Unclaimed/Annex/Shelf", Some("Unclaimed/Annex")),
                named("Brightfield", None),
                named("Brightfield/Offices", Some("Brightfield")),
                named("Brightfield/Archive", Some("Brightfield")),
                named("Brightfield/Archive/Rack", Some("Brightfield/Archive")),
                named("Harbor Lane", None),
                named("Harbor Lane/Docks", Some("Harbor Lane")),
            ],
        )
        .await
        .unwrap();
        for (customer, name) in [
            (brightfield, "Brightfield"),
            (brightfield, "Brightfield/Offices"),
            (brightfield, "Unclaimed/Lab"),
            // An offboarded customer keeps the names it has: they are not to be picked for
            // another, and cover the names nested in them, inside an active customer's too.
            (old_quay, "Brightfield/Archive"),
            (harbor, "Harbor Lane"),
        ] {
            crate::external_references::add(&db, customer, "Watchtower", name)
                .await
                .unwrap();
        }
        crate::customers::offboard(&db, old_quay).await.unwrap();
        crate::customers::offboard(&db, harbor).await.unwrap();
        // Switched off, the add-on's latest list still counts.
        crate::addons::set_enabled(&db, "watchtower", false)
            .await
            .unwrap();

        /// The system, how many names the list holds and the names no reference covers.
        type State = (String, usize, Vec<(String, Option<String>, i32)>);
        let states: Vec<(String, Option<State>)> = crate::addons::states(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|a| {
                let names = a.names.map(|n| {
                    (
                        n.system,
                        n.name_count,
                        n.uncovered
                            .into_iter()
                            .map(|u| (u.name, u.parent, u.depth))
                            .collect(),
                    )
                });
                (a.addon.name, names)
            })
            .collect();
        let uncovered = |name: &str, parent: Option<&str>, depth: i32| {
            (name.to_owned(), parent.map(str::to_owned), depth)
        };
        assert_eq!(
            states,
            [
                // An add-on that sent no list.
                ("netscope".to_owned(), None),
                (
                    "watchtower".to_owned(),
                    Some((
                        "Watchtower".to_owned(),
                        10,
                        vec![
                            uncovered("Unclaimed", None, 0),
                            uncovered("Unclaimed/Annex", Some("Unclaimed"), 1),
                            uncovered("Unclaimed/Annex/Shelf", Some("Unclaimed/Annex"), 2),
                        ]
                    ))
                ),
            ]
        );
    }

    #[sqlx::test]
    async fn a_list_that_does_not_fit_is_refused_and_the_one_before_kept(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        replace(
            &db,
            "watchtower",
            "Watchtower",
            &[named("Brightfield", None)],
        )
        .await
        .unwrap();
        let long = "x".repeat(MAX_NAME_LENGTH + 1);
        let many: Vec<SystemName> = (0..=MAX_NAMES)
            .map(|i| named(&format!("group-{i}"), None))
            .collect();
        let long_system = "W".repeat(MAX_SYSTEM_LENGTH + 1);
        for (system, names, why) in [
            (
                " ",
                vec![named("Harbor", None)],
                "the name of the system cannot be blank",
            ),
            (
                long_system.as_str(),
                vec![named("Harbor", None)],
                "the name of the system is longer than 100 characters",
            ),
            (
                "Watch\u{7}tower",
                vec![named("Harbor", None)],
                "the name of the system holds a control character",
            ),
            ("Watchtower", vec![named("\t", None)], "cannot be blank"),
            (
                "Watchtower",
                vec![named("Harbor\nDocks", None)],
                r#"the listed name "Harbor\nDocks" (number 1) holds a control character"#,
            ),
            // The database folds İ into i, which the core does not: it names the two all the same.
            (
                "Watchtower",
                vec![named("İstanbul", None), named("istanbul", None)],
                r#"the list names "istanbul" twice, once as "İstanbul""#,
            ),
            (
                "Watchtower",
                vec![named(&long, None)],
                "longer than 255 characters",
            ),
            (
                "Watchtower",
                vec![named("Harbor", None), named(" HARBOR ", None)],
                r#"the list names "HARBOR" twice, once as "Harbor""#,
            ),
            (
                "Watchtower",
                vec![named("Harbor/Docks", Some("Harbor"))],
                r#"the name "Harbor/Docks" is nested in "Harbor", which the list does not name"#,
            ),
            (
                "Watchtower",
                vec![named("Harbor", Some("harbor"))],
                r#"the name "Harbor" cannot be nested in itself"#,
            ),
            (
                "Watchtower",
                vec![
                    named("Top", None),
                    named("A", Some("C")),
                    named("B", Some("A")),
                    named("C", Some("B")),
                ],
                "nested in itself, through the names it is nested in",
            ),
            ("Watchtower", many, "at most 10000 names, not 10001"),
        ] {
            let refused = testing::refusal(replace(&db, "watchtower", system, &names).await);
            assert!(refused.contains(why), "{refused}");
        }
        assert_eq!(
            read(&db).await,
            [(
                "watchtower".to_owned(),
                "Watchtower".to_owned(),
                pairs(&[("Brightfield", None)])
            )],
            "the list before stays"
        );
        // As many names as a list holds, nested as deep as they come.
        let deep: Vec<SystemName> = (0..MAX_NAMES)
            .map(|i| {
                let parent = (i > 0).then(|| format!("group-{}", i - 1));
                named(&format!("group-{i}"), parent.as_deref())
            })
            .collect();
        replace(&db, "watchtower", "Watchtower", &deep)
            .await
            .unwrap();
        let names = all(&db).await.unwrap().remove(0).names;
        assert_eq!(names.len(), MAX_NAMES);
        assert_eq!(
            names[MAX_NAMES - 1].name,
            format!("group-{}", MAX_NAMES - 1)
        );
    }

    #[sqlx::test]
    async fn a_system_s_names_come_from_one_addon(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        replace(
            &db,
            "watchtower",
            "Watchtower",
            &[named("Brightfield", None)],
        )
        .await
        .unwrap();

        let refused = testing::refusal(
            replace(&db, "netscope", "WATCHTOWER", &[named("Harbor", None)]).await,
        );
        assert_eq!(
            refused,
            "the add-on watchtower lists the names of Watchtower: a system's names come from \
             one add-on"
        );
        replace(&db, "netscope", "Netscope", &[named("Harbor", None)])
            .await
            .unwrap();
        // The same name in two systems is two names.
        replace(&db, "watchtower", "Watchtower", &[named("Harbor", None)])
            .await
            .unwrap();
        let systems: Vec<(String, String)> = read(&db)
            .await
            .into_iter()
            .map(|(addon, system, _)| (addon, system))
            .collect();
        assert_eq!(
            systems,
            [
                ("netscope".to_owned(), "Netscope".to_owned()),
                ("watchtower".to_owned(), "Watchtower".to_owned())
            ]
        );
    }

    /// The database keeps the rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_rules_of_a_list(db: PgPool) {
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        let list = |addon: &'static str, system: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query("INSERT INTO name_lists (addon, system) VALUES ($1, $2)")
                    .bind(addon)
                    .bind(system)
                    .execute(&db)
                    .await
            }
        };
        assert!(
            list("watchtower", " Watchtower").await.is_err(),
            "untrimmed"
        );
        assert!(list("watchtower", "").await.is_err(), "blank");
        let long_system: &'static str = "W".repeat(101).leak();
        assert!(list("watchtower", long_system).await.is_err(), "too long");
        assert!(
            list("watchtower", "Watch\ttower").await.is_err(),
            "a control character"
        );
        assert!(list("nobody", "Elsewhere").await.is_err(), "no such add-on");
        assert!(list("watchtower", "Watchtower").await.is_ok());
        assert!(
            list("netscope", "watchtower").await.is_err(),
            "a system another add-on lists"
        );

        let insert = |id: Uuid, name: &'static str, parent: Option<Uuid>, depth: i32| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO listed_names (id, addon, name, parent_id, depth, parent_depth)
                     VALUES ($1, 'watchtower', $2, $3, $4,
                             CASE WHEN $3::uuid IS NULL THEN NULL ELSE $4 - 1 END)",
                )
                .bind(id)
                .bind(name)
                .bind(parent)
                .bind(depth)
                .execute(&db)
                .await
            }
        };
        let (top, offices) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(insert(top, "Brightfield", None, 0).await.is_ok());
        assert!(
            insert(offices, "Brightfield/Offices", Some(top), 1)
                .await
                .is_ok()
        );
        assert!(
            insert(Uuid::new_v4(), "BRIGHTFIELD", None, 0)
                .await
                .is_err(),
            "a name twice, ignoring case"
        );
        assert!(
            insert(Uuid::new_v4(), "Lab ", None, 0).await.is_err(),
            "untrimmed"
        );
        assert!(
            insert(Uuid::new_v4(), "Lab\nB", None, 0).await.is_err(),
            "a control character"
        );
        let long: &'static str = "y".repeat(256).leak();
        assert!(
            insert(Uuid::new_v4(), long, None, 0).await.is_err(),
            "too long"
        );
        assert!(
            insert(Uuid::new_v4(), "Lab", Some(top), 2).await.is_err(),
            "two levels below its parent"
        );
        assert!(
            insert(Uuid::new_v4(), "Lab", None, 1).await.is_err(),
            "below nothing"
        );
        assert!(
            insert(Uuid::new_v4(), "Lab", Some(Uuid::new_v4()), 1)
                .await
                .is_err(),
            "below a name that is not listed"
        );
        // A name nested in one below it, in a single statement where every row is there when
        // the rules are checked.
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let cycle = sqlx::query(
            "INSERT INTO listed_names (id, addon, name, parent_id, depth, parent_depth)
             VALUES ($1, 'watchtower', 'A', $2, 1, 0), ($2, 'watchtower', 'B', $1, 1, 0)",
        )
        .bind(a)
        .bind(b)
        .execute(&db)
        .await;
        assert!(cycle.is_err(), "nested in each other");
        assert!(
            sqlx::query("UPDATE listed_names SET depth = 3, parent_depth = 2 WHERE id = $1")
                .bind(offices)
                .execute(&db)
                .await
                .is_err(),
            "moved down without its parent"
        );
        // Another add-on's name is not a parent.
        sqlx::query("INSERT INTO name_lists (addon, system) VALUES ('netscope', 'Netscope')")
            .execute(&db)
            .await
            .unwrap();
        assert!(
            sqlx::query(
                "INSERT INTO listed_names (id, addon, name, parent_id, depth, parent_depth)
                 VALUES ($1, 'netscope', 'Lab', $2, 1, 0)",
            )
            .bind(Uuid::new_v4())
            .bind(top)
            .execute(&db)
            .await
            .is_err(),
            "nested in another list's name"
        );
    }

    /// Where the database tells white space or control characters otherwise than the core, its
    /// refusal is a sentence too, naming the name, rather than a failure of the core.
    #[sqlx::test]
    async fn a_name_the_database_refuses_is_named(db: PgPool) {
        let refused = refused_by_the_database(
            &db,
            Some("listed_names_name_check"),
            &["Harbor", "Docks\u{85}"],
        )
        .await
        .unwrap();
        assert_eq!(
            testing::refusal::<()>(Err(refused.unwrap())),
            r#"the listed name "Docks\u{85}" begins or ends with white space or holds a control character"#
        );
        assert!(
            refused_by_the_database(&db, Some("something_else"), &["Harbor"])
                .await
                .unwrap()
                .is_none()
        );
    }
}
