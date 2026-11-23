//! A customer's sites. Rules are in docs/domain-rules.md ("Customers, assets and services").

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Site {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    /// When the site was retired: it stays visible but gets no new requesters or assets.
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
}

/// Changes a site's name and address.
pub async fn update(db: &PgPool, id: Uuid, name: &str, address: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    update_in(&mut tx, id, name, address).await?;
    tx.commit().await?;
    Ok(())
}

/// [`update`], inside the caller's transaction.
pub(crate) async fn update_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    name: &str,
    address: &str,
) -> Result<()> {
    let (name, address) = clean(name, address)?;
    let (customer, retired) = lock(tx, id).await?;
    // Only active sites need names of their own.
    if !retired {
        name_free(tx, customer, name, Some(id)).await?;
    }
    sqlx::query("UPDATE sites SET name = $2, address = $3 WHERE id = $1")
        .bind(id)
        .bind(name)
        .bind(address)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The site's customer, locked and not offboarded, and whether the site is retired.
async fn lock(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(Uuid, bool)> {
    // A site never changes customer, so its customer can be read before it is locked.
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound("site"))?;
    crate::customers::lock_active(tx, customer).await?;
    let retired: bool =
        sqlx::query_scalar("SELECT retired_at IS NOT NULL FROM sites WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    Ok((customer, retired))
}

/// Retires a site: it stays visible but gets no new requesters or assets, and the requesters and
/// assets at it are left without a site.
pub async fn retire(db: &PgPool, id: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    let (_, retired) = lock(&mut tx, id).await?;
    if retired {
        return rule("the site is already retired");
    }
    // The site is locked before the requesters and assets at it, while changing a requester or an
    // asset locks it before their site: both lock the customer first (FOR UPDATE), so the two
    // never wait for each other.
    sqlx::query("UPDATE requesters SET site_id = NULL WHERE site_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE assets SET site_id = NULL WHERE site_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sites SET retired_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The customer's sites, active ones first.
pub async fn of(db: &PgPool, customer: Uuid) -> Result<Vec<Site>> {
    Ok(sqlx::query_as(
        "SELECT id, name, address, retired_at FROM sites WHERE customer_id = $1
         ORDER BY retired_at IS NOT NULL, lower(name), created_at",
    )
    .bind(customer)
    .fetch_all(db)
    .await?)
}

/// A site's name and address, trimmed; both are needed.
fn clean<'a>(name: &'a str, address: &'a str) -> Result<(&'a str, &'a str)> {
    let (name, address) = (name.trim(), address.trim());
    if name.is_empty() {
        return rule("a site needs a name");
    }
    if address.is_empty() {
        return rule("a site needs an address");
    }
    Ok((name, address))
}

/// Refuses a name another active site of the customer has; the customer is locked by the caller.
async fn name_free(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    name: &str,
    except: Option<Uuid>,
) -> Result<()> {
    let taken: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sites
         WHERE customer_id = $1 AND lower(name) = lower($2) AND retired_at IS NULL
           AND id IS DISTINCT FROM $3",
    )
    .bind(customer)
    .bind(name)
    .bind(except)
    .fetch_optional(&mut **tx)
    .await?;
    match taken {
        Some(taken) => rule(format!("the customer already has a site named {taken}")),
        None => Ok(()),
    }
}

/// Adds a site to the customer.
pub async fn create(db: &PgPool, customer: Uuid, name: &str, address: &str) -> Result<Uuid> {
    let mut tx = db.begin().await?;
    let id = create_in(&mut tx, customer, name, address).await?;
    tx.commit().await?;
    Ok(id)
}

/// [`create`], inside the caller's transaction.
pub(crate) async fn create_in(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    name: &str,
    address: &str,
) -> Result<Uuid> {
    let (name, address) = clean(name, address)?;
    crate::customers::lock_active(tx, customer).await?;
    name_free(tx, customer, name, None).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sites (id, customer_id, name, address) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(customer)
        .bind(name)
        .bind(address)
        .execute(&mut **tx)
        .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[sqlx::test]
    async fn a_customer_has_sites(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        create(
            &db,
            brightfield,
            " Head office ",
            "Via dei Tigli 1, Valfiorita",
        )
        .await
        .unwrap();
        create(
            &db,
            brightfield,
            "Warehouse",
            "Strada delle Gru 4, Portoalto",
        )
        .await
        .unwrap();

        let sites = of(&db, brightfield).await.unwrap();
        let names: Vec<(&str, &str)> = sites
            .iter()
            .map(|s| (s.name.as_str(), s.address.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Head office", "Via dei Tigli 1, Valfiorita"),
                ("Warehouse", "Strada delle Gru 4, Portoalto")
            ]
        );
        assert!(of(&db, harbor).await.unwrap().is_empty());
    }

    #[sqlx::test]
    async fn a_site_needs_a_name_and_an_address_and_its_own_name(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        create(
            &db,
            brightfield,
            "Head office",
            "Via dei Tigli 1, Valfiorita",
        )
        .await
        .unwrap();

        for (name, address) in [(" ", "Via dei Tigli 1"), ("Lab", "  ")] {
            assert!(
                matches!(
                    create(&db, brightfield, name, address).await,
                    Err(Error::Rule(_))
                ),
                "{name:?} at {address:?} accepted"
            );
        }
        let Err(Error::Rule(why)) =
            create(&db, brightfield, "HEAD OFFICE", "Corso dei Mulini 9").await
        else {
            panic!("a second active site with the same name was accepted");
        };
        assert!(why.contains("Head office"), "{why}");
        create(&db, harbor, "Head office", "Molo delle Reti 2, Portoalto")
            .await
            .unwrap();
    }

    #[sqlx::test]
    async fn a_site_is_corrected(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        create(&db, brightfield, "Warehouse", "Strada delle Gru 4")
            .await
            .unwrap();

        update(&db, head, "Head Office", " Via dei Tigli 1A, Valfiorita ")
            .await
            .unwrap();
        let site = &of(&db, brightfield).await.unwrap()[0];
        assert_eq!(
            (site.name.as_str(), site.address.as_str()),
            ("Head Office", "Via dei Tigli 1A, Valfiorita")
        );
        assert!(matches!(
            update(&db, head, "warehouse", "Via dei Tigli 1").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            update(&db, head, "Head office", "").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            update(&db, Uuid::new_v4(), "Lab", "Via del Glicine 3").await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn a_retired_site_stays_and_frees_its_name(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let old = create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        create(&db, brightfield, "Warehouse", "Strada delle Gru 4")
            .await
            .unwrap();

        retire(&db, old).await.unwrap();
        assert!(matches!(retire(&db, old).await, Err(Error::Rule(_))));
        let new = create(&db, brightfield, "Head office", "Corso dei Mulini 9")
            .await
            .unwrap();
        let sites = of(&db, brightfield).await.unwrap();
        let listed: Vec<(Uuid, bool)> = sites
            .iter()
            .map(|s| (s.id, s.retired_at.is_some()))
            .collect();
        assert_eq!(listed[0], (new, false));
        assert_eq!(listed[2], (old, true), "retired sites stay, last");

        // A retired site can still be corrected, even under a name an active site now has.
        update(&db, old, "Head office", "Via dei Tigli 1A, Valfiorita")
            .await
            .unwrap();
        assert!(matches!(
            retire(&db, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn retiring_a_site_leaves_its_assets_without_one(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let lab = create(&db, brightfield, "Lab", "Via del Glicine 3")
            .await
            .unwrap();
        let printer = testing::asset_kind(&db, "Printer").await;
        for (name, site) in [("Printer 2F", head), ("Printer 3F", head), ("Plotter", lab)] {
            let fields = crate::catalog::AssetFields {
                site_id: Some(site),
                ..testing::asset_fields(printer, name)
            };
            crate::catalog::create_asset(&db, brightfield, &fields)
                .await
                .unwrap();
        }
        let retired = testing::asset(&db, brightfield, "Printer", "Printer 1F").await;
        let fields = crate::catalog::AssetFields {
            site_id: Some(head),
            ..testing::asset_fields(printer, "Printer 1F")
        };
        crate::catalog::update_asset(&db, retired, &fields)
            .await
            .unwrap();
        crate::catalog::retire_asset(&db, retired).await.unwrap();

        retire(&db, head).await.unwrap();
        let sites: Vec<(String, Option<String>)> = crate::catalog::assets_of(&db, brightfield)
            .await
            .unwrap()
            .into_iter()
            .map(|a| (a.name, a.site.map(|s| s.name)))
            .collect();
        assert_eq!(
            sites,
            [
                ("Plotter".to_owned(), Some("Lab".to_owned())),
                ("Printer 2F".to_owned(), None),
                ("Printer 3F".to_owned(), None),
                ("Printer 1F".to_owned(), None),
            ]
        );
    }

    #[sqlx::test]
    async fn an_offboarded_customer_s_sites_do_not_change(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let head = create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        crate::customers::offboard(&db, brightfield).await.unwrap();

        assert!(matches!(
            create(&db, brightfield, "Lab", "Via del Glicine 3").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            update(&db, head, "Head office", "Via dei Tigli 2").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(retire(&db, head).await, Err(Error::Rule(_))));
        assert_eq!(
            of(&db, brightfield).await.unwrap()[0].address,
            "Via dei Tigli 1"
        );
    }

    /// The database keeps the rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_site_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let insert = |name: &'static str, address: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO sites (id, customer_id, name, address) VALUES ($1, $2, $3, $4)",
                )
                .bind(Uuid::new_v4())
                .bind(brightfield)
                .bind(name)
                .bind(address)
                .execute(&db)
                .await
            }
        };
        assert!(insert("Head office", "Via dei Tigli 1").await.is_ok());
        assert!(insert("", "Via del Glicine 3").await.is_err(), "no name");
        assert!(
            insert(" Lab", "Via del Glicine 3").await.is_err(),
            "untrimmed name"
        );
        assert!(insert("Lab", " ").await.is_err(), "no address");
        assert!(
            insert("\t", "Via del Glicine 3").await.is_err(),
            "a blank name"
        );
        assert!(
            insert("Lab", "Via del Glicine 3\n").await.is_err(),
            "an untrimmed address"
        );
        assert!(
            insert("Lab", "Via del Glicine 3\nPiano terra")
                .await
                .is_ok(),
            "an address on two lines"
        );
        assert!(
            insert("HEAD OFFICE", "Corso dei Mulini 9").await.is_err(),
            "same active name"
        );
        sqlx::query("UPDATE sites SET retired_at = now() WHERE name = 'Head office'")
            .execute(&db)
            .await
            .unwrap();
        assert!(
            insert("Head office", "Corso dei Mulini 9").await.is_ok(),
            "freed by retiring"
        );
    }
}
