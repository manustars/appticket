//! A customer's external references. Rules are in docs/domain-rules.md ("Customers, assets and
//! services", "Add-ons").

use std::collections::HashMap;

use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

/// A name another system uses for a customer, such as a group in a monitoring tool.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct ExternalReference {
    pub id: Uuid,
    /// The other system, such as a monitoring tool.
    pub system: String,
    /// The name the system uses for the customer; it names no other customer in that system.
    pub name: String,
}

/// An external reference as add-ons read it: a name in a system, without the core's id.
#[derive(Debug, Serialize, ToSchema)]
pub struct NameInSystem {
    pub system: String,
    pub name: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    customer_id: Uuid,
    #[sqlx(flatten)]
    reference: ExternalReference,
}

/// External references by system and name: every customer's, or only `customer`'s.
async fn list(db: &PgPool, customer: Option<Uuid>) -> Result<Vec<Row>> {
    Ok(sqlx::query_as(
        "SELECT customer_id, id, system, name FROM external_references
         WHERE $1::uuid IS NULL OR customer_id = $1
         ORDER BY lower(system), lower(name), created_at",
    )
    .bind(customer)
    .fetch_all(db)
    .await?)
}

/// The customer's external references, by system and name.
pub async fn of(db: &PgPool, customer: Uuid) -> Result<Vec<ExternalReference>> {
    Ok(list(db, Some(customer))
        .await?
        .into_iter()
        .map(|row| row.reference)
        .collect())
}

/// Every customer's external references, by system and name, keyed by the customer's id.
pub async fn by_customer(db: &PgPool) -> Result<HashMap<Uuid, Vec<NameInSystem>>> {
    let mut grouped: HashMap<Uuid, Vec<NameInSystem>> = HashMap::new();
    for row in list(db, None).await? {
        grouped
            .entry(row.customer_id)
            .or_default()
            .push(NameInSystem {
                system: row.reference.system,
                name: row.reference.name,
            });
    }
    Ok(grouped)
}

/// The systems external references are in, across all customers, so that a system is spelled
/// alike everywhere: one spelling per system, ignoring case, the first one given.
pub async fn systems(db: &PgPool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT system FROM (
             SELECT DISTINCT ON (lower(system)) system FROM external_references
             ORDER BY lower(system), created_at) first
         ORDER BY lower(system)",
    )
    .fetch_all(db)
    .await?)
}

/// Gives the customer an external reference: a name in another system. A name that already names
/// a customer in that system, ignoring case, is refused, even when it is this customer.
pub async fn add(db: &PgPool, customer: Uuid, system: &str, name: &str) -> Result<Uuid> {
    let (system, name) = (system.trim(), name.trim());
    if system.is_empty() {
        return rule("an external reference needs the system it belongs to");
    }
    if name.is_empty() {
        return rule("an external reference needs a name");
    }
    let mut tx = db.begin().await?;
    crate::customers::lock_active(&mut tx, customer).await?;
    let id = Uuid::new_v4();
    // Another customer can take the name at the same time: the database decides.
    let added = sqlx::query(
        "INSERT INTO external_references (id, customer_id, system, name) VALUES ($1, $2, $3, $4)
         ON CONFLICT (lower(system), lower(name)) DO NOTHING",
    )
    .bind(id)
    .bind(customer)
    .bind(system)
    .bind(name)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if added == 0 {
        let taken: Option<(String, String, String)> = sqlx::query_as(
            "SELECT r.system, r.name, c.name FROM external_references r
             JOIN customers c ON c.id = r.customer_id
             WHERE lower(r.system) = lower($1) AND lower(r.name) = lower($2)",
        )
        .bind(system)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
        return match taken {
            Some((taken_system, taken, owner)) => {
                rule(format!("{taken} in {taken_system} already names {owner}"))
            }
            // The reference that held the name was removed in the meantime.
            None => rule(format!(
                "{name} in {system} was in use a moment ago: try again"
            )),
        };
    }
    tx.commit().await?;
    Ok(id)
}

/// Takes an external reference from its customer; the name is free again in its system.
pub async fn remove(db: &PgPool, id: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    // A reference never changes customer, so its customer can be read before it is locked.
    let customer: Uuid =
        sqlx::query_scalar("SELECT customer_id FROM external_references WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(Error::NotFound("external reference"))?;
    crate::customers::lock_active(&mut tx, customer).await?;
    let removed = sqlx::query("DELETE FROM external_references WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(Error::NotFound("external reference"));
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[sqlx::test]
    async fn a_customer_has_external_references(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        add(&db, brightfield, " Watchtower ", " Brightfield/Offices ")
            .await
            .unwrap();
        add(&db, brightfield, "Watchtower", "Brightfield/Lab")
            .await
            .unwrap();
        add(&db, brightfield, "Ledgerly", "BRF-0042").await.unwrap();

        let listed: Vec<(String, String)> = of(&db, brightfield)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.system, r.name))
            .collect();
        assert_eq!(
            listed,
            [
                ("Ledgerly".to_owned(), "BRF-0042".to_owned()),
                ("Watchtower".to_owned(), "Brightfield/Lab".to_owned()),
                ("Watchtower".to_owned(), "Brightfield/Offices".to_owned()),
            ]
        );
        assert!(of(&db, harbor).await.unwrap().is_empty());
    }

    #[sqlx::test]
    async fn a_name_names_one_customer_within_its_system(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        add(&db, brightfield, "Watchtower", "Offices")
            .await
            .unwrap();

        for (customer, system, name) in [
            (harbor, "Watchtower", "Offices"),
            (harbor, "WATCHTOWER", "offices"),
            (brightfield, "watchtower", "Offices"),
        ] {
            let Err(Error::Rule(why)) = add(&db, customer, system, name).await else {
                panic!("{name} in {system} accepted a second time");
            };
            assert!(why.contains("Brightfield"), "{why}");
        }
        // The same name in another system is another reference.
        add(&db, harbor, "Ledgerly", "Offices").await.unwrap();
        assert_eq!(of(&db, harbor).await.unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn the_systems_in_use_are_offered_once_each(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        assert!(systems(&db).await.unwrap().is_empty());
        add(&db, brightfield, "Watchtower", "Offices")
            .await
            .unwrap();
        add(&db, harbor, "watchtower", "Harbor").await.unwrap();
        add(&db, harbor, "Ledgerly", "HRB-0007").await.unwrap();

        // One spelling per system, the first one given.
        assert_eq!(systems(&db).await.unwrap(), ["Ledgerly", "Watchtower"]);
    }

    #[sqlx::test]
    async fn an_external_reference_needs_a_system_and_a_name(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        for (system, name) in [(" ", "Offices"), ("Watchtower", "\t"), ("", "")] {
            assert!(
                matches!(
                    add(&db, brightfield, system, name).await,
                    Err(Error::Rule(_))
                ),
                "{name:?} in {system:?} accepted"
            );
        }
        assert!(matches!(
            add(&db, Uuid::new_v4(), "Watchtower", "Offices").await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn an_external_reference_is_removed_and_its_name_freed(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let offices = add(&db, brightfield, "Watchtower", "Offices")
            .await
            .unwrap();
        add(&db, brightfield, "Watchtower", "Lab").await.unwrap();

        remove(&db, offices).await.unwrap();
        let left: Vec<String> = of(&db, brightfield)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(left, ["Lab"]);
        add(&db, harbor, "Watchtower", "Offices").await.unwrap();
        assert!(matches!(
            remove(&db, offices).await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn an_offboarded_customer_s_external_references_do_not_change(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let offices = add(&db, brightfield, "Watchtower", "Offices")
            .await
            .unwrap();
        crate::customers::offboard(&db, brightfield).await.unwrap();

        assert!(matches!(
            add(&db, brightfield, "Watchtower", "Lab").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(remove(&db, offices).await, Err(Error::Rule(_))));
        assert_eq!(of(&db, brightfield).await.unwrap().len(), 1);
        // Its names stay taken: the customer can come back with them.
        assert!(matches!(
            add(&db, harbor, "Watchtower", "Offices").await,
            Err(Error::Rule(_))
        ));
    }

    /// The database keeps the rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_external_reference_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let insert = |customer: Uuid, system: &'static str, name: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO external_references (id, customer_id, system, name)
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(Uuid::new_v4())
                .bind(customer)
                .bind(system)
                .bind(name)
                .execute(&db)
                .await
            }
        };
        assert!(insert(brightfield, "Watchtower", "Offices").await.is_ok());
        assert!(insert(brightfield, "Watchtower", "Lab").await.is_ok());
        assert!(insert(brightfield, "", "Store").await.is_err(), "no system");
        assert!(
            insert(brightfield, "Watchtower", "\t").await.is_err(),
            "a blank name"
        );
        assert!(
            insert(brightfield, " Watchtower", "Store").await.is_err(),
            "untrimmed system"
        );
        assert!(
            insert(brightfield, "Watchtower", "Store\n").await.is_err(),
            "untrimmed name"
        );
        assert!(
            insert(harbor, "Watchtower", "Offices").await.is_err(),
            "a name of another customer"
        );
        assert!(
            insert(harbor, "watchtower", "OFFICES").await.is_err(),
            "the same name in another case"
        );
        assert!(
            insert(brightfield, "Watchtower", "Offices").await.is_err(),
            "the same reference twice"
        );
        assert!(
            insert(harbor, "Ledgerly", "Offices").await.is_ok(),
            "the same name in another system"
        );
        assert!(
            insert(Uuid::new_v4(), "Ledgerly", "Depot").await.is_err(),
            "no such customer"
        );
    }
}
