//! The asset kinds the help desk manages. Rules are in docs/domain-rules.md ("Customers, assets
//! and services").

use serde::Serialize;
use sqlx::PgPool;
use sqlx::postgres::PgQueryResult;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct AssetKind {
    pub id: Uuid,
    pub name: String,
    /// Whether its assets move with their holder; otherwise they stay at their site.
    pub moves_with_holder: bool,
}

/// Every asset kind, by name.
pub async fn list(db: &PgPool) -> Result<Vec<AssetKind>> {
    Ok(sqlx::query_as(
        "SELECT id, name, moves_with_holder FROM asset_kinds ORDER BY lower(name), name",
    )
    .fetch_all(db)
    .await?)
}

/// Adds an asset kind; no two kinds have names that differ only in case.
pub async fn create(db: &PgPool, name: &str, moves_with_holder: bool) -> Result<Uuid> {
    let name = clean(name)?;
    let id = Uuid::new_v4();
    let inserted =
        sqlx::query("INSERT INTO asset_kinds (id, name, moves_with_holder) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(name)
            .bind(moves_with_holder)
            .execute(db)
            .await;
    refuse_taken_name(db, name, inserted).await?;
    Ok(id)
}

/// Renames an asset kind or changes whether its assets move with their holder.
pub async fn update(db: &PgPool, id: Uuid, name: &str, moves_with_holder: bool) -> Result<()> {
    let name = clean(name)?;
    let updated =
        sqlx::query("UPDATE asset_kinds SET name = $2, moves_with_holder = $3 WHERE id = $1")
            .bind(id)
            .bind(name)
            .bind(moves_with_holder)
            .execute(db)
            .await;
    if refuse_taken_name(db, name, updated).await?.rows_affected() == 0 {
        return Err(Error::NotFound("asset kind"));
    }
    Ok(())
}

fn clean(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return rule("an asset kind needs a name");
    }
    Ok(name)
}

/// The outcome of a write, with the database's refusal of a name another kind has turned into a
/// rule. The unique index decides, so that two agents adding the same kind at once cannot both
/// succeed.
async fn refuse_taken_name(
    db: &PgPool,
    name: &str,
    written: sqlx::Result<PgQueryResult>,
) -> Result<PgQueryResult> {
    match written {
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("asset_kinds_name") => {
            // The other kind may have been renamed since; then its name is not known any more.
            let other: Option<String> =
                sqlx::query_scalar("SELECT name FROM asset_kinds WHERE lower(name) = lower($1)")
                    .bind(name)
                    .fetch_optional(db)
                    .await?;
            rule(format!(
                "there is already an asset kind called {}",
                other.as_deref().unwrap_or(name)
            ))
        }
        written => Ok(written?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn the_help_desk_keeps_a_list_of_kinds(db: PgPool) {
        let laptop = create(&db, " Laptop ", true).await.unwrap();
        create(&db, "Desktop", false).await.unwrap();

        let kinds: Vec<(String, bool)> = list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|k| (k.name, k.moves_with_holder))
            .collect();
        assert_eq!(
            kinds,
            [("Desktop".to_owned(), false), ("Laptop".to_owned(), true)]
        );
        assert!(
            matches!(create(&db, " \n", false).await, Err(Error::Rule(_))),
            "a kind needs a name"
        );
        let Err(Error::Rule(why)) = create(&db, "LAPTOP", false).await else {
            panic!("a second kind with the same name was accepted");
        };
        assert!(why.contains("Laptop"), "{why}");

        update(&db, laptop, "Notebook", false).await.unwrap();
        let notebook = list(&db).await.unwrap().pop().unwrap();
        assert_eq!(
            (
                notebook.id,
                notebook.name.as_str(),
                notebook.moves_with_holder
            ),
            (laptop, "Notebook", false)
        );
        update(&db, laptop, "notebook", true)
            .await
            .expect("a kind keeps its own name in another case");
        assert!(matches!(
            update(&db, laptop, "desktop", true).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            update(&db, Uuid::new_v4(), "Printer", false).await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn an_asset_has_a_kind_from_the_list(db: PgPool) {
        let brightfield = crate::testing::customer(&db, "Brightfield").await;
        let laptop = create(&db, "Laptop", true).await.unwrap();
        let printer = create(&db, "Printer", false).await.unwrap();

        let pc = crate::catalog::create_asset(&db, brightfield, laptop, "PC-0042")
            .await
            .unwrap();
        assert!(matches!(
            crate::catalog::create_asset(&db, brightfield, Uuid::new_v4(), "PC-0043").await,
            Err(Error::NotFound(_))
        ));
        crate::catalog::update_asset(&db, pc, printer, "Printer 2F")
            .await
            .unwrap();
        assert!(matches!(
            crate::catalog::update_asset(&db, pc, Uuid::new_v4(), "Printer 2F").await,
            Err(Error::NotFound(_))
        ));
        let asset = &crate::catalog::assets_of(&db, brightfield).await.unwrap()[0];
        assert_eq!((asset.kind_id, asset.kind.as_str()), (printer, "Printer"));
    }

    /// The database keeps the rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_rules(db: PgPool) {
        let insert = |name: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query("INSERT INTO asset_kinds (id, name) VALUES ($1, $2)")
                    .bind(Uuid::new_v4())
                    .bind(name)
                    .execute(&db)
                    .await
            }
        };
        assert!(insert("Laptop").await.is_ok());
        assert!(insert("").await.is_err(), "no name");
        assert!(insert("\t").await.is_err(), "a blank name");
        assert!(insert(" Printer").await.is_err(), "an untrimmed name");
        assert!(
            insert("LAPTOP").await.is_err(),
            "the same name in another case"
        );
        let moves: bool =
            sqlx::query_scalar("SELECT moves_with_holder FROM asset_kinds WHERE name = 'Laptop'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(
            !moves,
            "a kind stays at its site unless the help desk says otherwise"
        );

        let customer = crate::testing::customer(&db, "Brightfield").await;
        let without = sqlx::query(
            "INSERT INTO assets (id, customer_id, kind_id, name) VALUES ($1, $2, $3, 'PC-0042')",
        )
        .bind(Uuid::new_v4())
        .bind(customer)
        .bind(Uuid::new_v4())
        .execute(&db)
        .await;
        assert!(without.is_err(), "an asset of a kind not on the list");
        let none = sqlx::query(
            "INSERT INTO assets (id, customer_id, kind_id, name) VALUES ($1, $2, NULL, 'PC-0042')",
        )
        .bind(Uuid::new_v4())
        .bind(customer)
        .execute(&db)
        .await;
        assert!(none.is_err(), "an asset without a kind");
    }

    /// The free-text kinds of the assets there were become entries of the list, each staying at
    /// its site, and the assets keep their kind.
    #[sqlx::test(migrations = false)]
    async fn existing_kinds_become_entries(db: PgPool) {
        let migrator = sqlx::migrate!();
        migrator.run_to(15, &db).await.unwrap();
        let customer = Uuid::new_v4();
        sqlx::query("INSERT INTO customers (id, name) VALUES ($1, 'Brightfield')")
            .bind(customer)
            .execute(&db)
            .await
            .unwrap();
        for (name, kind) in [
            ("PC-0042", "Laptop"),
            ("PC-0043", " laptop\t"),
            ("PC-0044", "Laptop"),
            ("Printer 2F", "printer"),
        ] {
            sqlx::query("INSERT INTO assets (id, customer_id, kind, name) VALUES ($1, $2, $3, $4)")
                .bind(Uuid::new_v4())
                .bind(customer)
                .bind(kind)
                .bind(name)
                .execute(&db)
                .await
                .unwrap();
        }

        migrator.run(&db).await.unwrap();
        let kinds: Vec<(String, bool)> = list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|k| (k.name, k.moves_with_holder))
            .collect();
        assert_eq!(
            kinds,
            [("Laptop".to_owned(), false), ("printer".to_owned(), false)],
            "one entry per kind, spelled as most assets had it, staying at its site"
        );
        let assets: Vec<(String, String)> = crate::catalog::assets_of(&db, customer)
            .await
            .unwrap()
            .into_iter()
            .map(|a| (a.name, a.kind))
            .collect();
        assert_eq!(
            assets,
            [
                ("PC-0042".to_owned(), "Laptop".to_owned()),
                ("PC-0043".to_owned(), "Laptop".to_owned()),
                ("PC-0044".to_owned(), "Laptop".to_owned()),
                ("Printer 2F".to_owned(), "printer".to_owned()),
            ]
        );
    }
}
