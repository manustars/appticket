//! The assets an add-on supplies (docs/domain-rules.md, "Add-ons"; ADR 0002): an add-on with the
//! scope `catalog:write` creates, updates and retires the assets it is the source of, each known by
//! the add-on's own reference, and nothing else: never a holder, an asset entered by hand or one
//! another add-on supplied.

use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::catalog::{Asset, StoredAsset};
use crate::error::{Error, Result, rule};

/// What an add-on supplies of an asset: everything but its customer, which never changes, and its
/// holder, whom agents choose.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct SuppliedDetails {
    /// The name of its asset kind, one of the help desk's list, compared ignoring case.
    pub kind: String,
    pub name: String,
    /// Empty or left out for none.
    #[serde(default)]
    pub serial_number: Option<String>,
    /// The name of one of its customer's active sites, compared ignoring case; empty or left out
    /// for none.
    #[serde(default)]
    pub site: Option<String>,
}

/// The asset an add-on supplied, and whether supplying it created it.
#[derive(Debug)]
pub struct Supplied {
    /// The asset as stored.
    pub asset: Asset,
    /// Whether supplying it created it; otherwise it updated the asset the add-on supplied before
    /// with that reference.
    pub created: bool,
}

/// Creates the asset of `customer` that `addon` knows by `reference`, or, when the add-on already
/// supplied one with that reference, updates it with `details`; so the add-on can send its whole
/// inventory again safely. An asset it retired comes back. The asset's holder is left as it is.
pub async fn supply(
    db: &PgPool,
    addon: &str,
    customer: Uuid,
    reference: &str,
    details: &SuppliedDetails,
) -> Result<Supplied> {
    let reference = crate::catalog::text(reference, "reference")?;
    let mut tx = db.begin().await?;
    // The customer first, as by every change to its sites and assets (see catalog::update_asset).
    crate::customers::lock_active(&mut tx, customer).await?;
    let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, customer_id FROM assets WHERE source_addon = $1 AND source_ref = $2
         FOR NO KEY UPDATE",
    )
    .bind(addon)
    .bind(&reference)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((_, owner)) = existing
        && owner != customer
    {
        return rule(another_customer(&reference));
    }
    let checked = checked(&mut tx, customer, details).await?;
    let (id, created) = match existing {
        Some((id, _)) => {
            write(&mut tx, id, &checked).await?;
            (id, false)
        }
        None => {
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO assets (id, customer_id, kind_id, name, serial_number, site_id,
                                     source_addon, source_ref)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(id)
            .bind(customer)
            .bind(checked.kind)
            .bind(&checked.name)
            .bind(&checked.serial_number)
            .bind(checked.site)
            .bind(addon)
            .bind(&reference)
            .execute(&mut *tx)
            .await
            .map_err(|e| match e {
                // The same reference, given at the same time for another customer.
                sqlx::Error::Database(d)
                    if d.constraint() == Some("assets_source_addon_source_ref_key") =>
                {
                    Error::Rule(another_customer(&reference))
                }
                other => other.into(),
            })?;
            (id, true)
        }
    };
    tx.commit().await?;
    Ok(Supplied {
        asset: crate::catalog::asset(db, id).await?,
        created,
    })
}

/// Updates an asset the add-on is the source of with `details`, as [`supply`] does, except that a
/// retired asset is refused: what the add-on sends about it may be older than the retirement, and
/// only supplying it again with its reference brings it back.
pub async fn update(db: &PgPool, addon: &str, id: Uuid, details: &SuppliedDetails) -> Result<()> {
    let mut tx = db.begin().await?;
    let (customer, asset) = lock_own(&mut tx, addon, id).await?;
    if asset.retired {
        return rule(format!(
            "{} is retired: supplying it again with its reference brings it back",
            asset.name
        ));
    }
    let checked = checked(&mut tx, customer, details).await?;
    write(&mut tx, id, &checked).await?;
    tx.commit().await?;
    Ok(())
}

/// Retires an asset the add-on is the source of, when it leaves the add-on's system; it loses its
/// holder. Retiring it again changes nothing.
pub async fn retire(db: &PgPool, addon: &str, id: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    lock_own(&mut tx, addon, id).await?;
    sqlx::query(
        "UPDATE assets SET retired_at = now(), holder_id = NULL
         WHERE id = $1 AND retired_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Locks the asset's customer, refused when offboarded, then the asset, refused unless `addon` is
/// its source. Returns the customer and the asset as stored.
async fn lock_own(
    tx: &mut Transaction<'_, Postgres>,
    addon: &str,
    id: Uuid,
) -> Result<(Uuid, StoredAsset)> {
    // An asset never changes customer, so its customer can be read before it is locked; the
    // customer is locked first, as by every change to its sites and assets.
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM assets WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound("asset"))?;
    crate::customers::lock_active(tx, customer).await?;
    let asset = crate::catalog::lock_stored(tx, id).await?;
    its_own(&asset, addon)?;
    Ok((customer, asset))
}

/// Refuses an asset `addon` is not the source of.
fn its_own(asset: &StoredAsset, addon: &str) -> Result<()> {
    match &asset.source_addon {
        Some(source) if source == addon => Ok(()),
        Some(source) => rule(format!(
            "{} comes from the add-on {source}: an add-on changes only the assets it is the \
             source of",
            asset.name
        )),
        None => rule(format!(
            "{} was entered by hand: an add-on changes only the assets it is the source of",
            asset.name
        )),
    }
}

fn another_customer(reference: &str) -> String {
    format!(
        "the asset with the reference {reference} belongs to another customer: an asset never \
         changes customer"
    )
}

/// What an add-on supplied, checked and resolved to be stored.
struct Checked {
    kind: Uuid,
    name: String,
    serial_number: Option<String>,
    site: Option<Uuid>,
}

/// Checks what an add-on supplies for an asset of `customer`, which the caller has locked: the
/// kind is one of the help desk's list and the site one of the customer's active sites, both
/// named; the site is locked, so that one being retired at the same time is waited for.
async fn checked(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    details: &SuppliedDetails,
) -> Result<Checked> {
    let name = crate::catalog::text(&details.name, "name")?;
    let kind_name = details.kind.trim();
    let Some(kind) = crate::asset_kinds::named(&mut **tx, kind_name).await? else {
        return rule(format!(
            "the help desk has no asset kind called {kind_name}: an add-on maps its own kinds to \
             the help desk's list"
        ));
    };
    let site = match details.site.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(site_name) => Some(site_named(tx, customer, site_name).await?),
    };
    let serial_number = details
        .serial_number
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Ok(Checked {
        kind,
        name,
        serial_number,
        site,
    })
}

/// The active site of `customer` called `name`, ignoring case, locked against being retired.
async fn site_named(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    name: &str,
) -> Result<Uuid> {
    // Active sites have different names; retired ones may share theirs with an active one.
    let site: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT id, retired_at IS NOT NULL FROM sites
         WHERE customer_id = $1 AND lower(name) = lower($2)
         ORDER BY retired_at IS NOT NULL
         LIMIT 1
         FOR SHARE",
    )
    .bind(customer)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((id, false)) = site {
        return Ok(id);
    }
    let customer_name: String = sqlx::query_scalar("SELECT name FROM customers WHERE id = $1")
        .bind(customer)
        .fetch_one(&mut **tx)
        .await?;
    match site {
        Some(_) => rule(format!(
            "the site {name} of {customer_name} is retired: an asset is put only at one of its \
             customer's active sites"
        )),
        None => rule(format!(
            "{customer_name} has no site called {name}: an asset is put only at one of its \
             customer's active sites"
        )),
    }
}

/// Stores what the add-on supplied on the asset, bringing it back if it was retired; its holder,
/// whom agents choose, stays.
async fn write(tx: &mut Transaction<'_, Postgres>, id: Uuid, checked: &Checked) -> Result<()> {
    sqlx::query(
        "UPDATE assets SET kind_id = $2, name = $3, serial_number = $4, site_id = $5,
                retired_at = NULL
         WHERE id = $1",
    )
    .bind(id)
    .bind(checked.kind)
    .bind(&checked.name)
    .bind(&checked.serial_number)
    .bind(checked.site)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn details(
        kind: &str,
        name: &str,
        serial: Option<&str>,
        site: Option<&str>,
    ) -> SuppliedDetails {
        SuppliedDetails {
            kind: kind.to_owned(),
            name: name.to_owned(),
            serial_number: serial.map(str::to_owned),
            site: site.map(str::to_owned),
        }
    }

    #[sqlx::test]
    async fn an_addon_supplies_an_asset_and_supplying_it_again_updates_it(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        testing::asset_kind(&db, "Laptop").await;
        let server = testing::asset_kind(&db, "Server").await;
        testing::addon(&db, "watchtower").await;

        let first = supply(
            &db,
            "watchtower",
            brightfield,
            " host-1042 ",
            &details(
                "laptop",
                " PC-0042 ",
                Some(" SN-0042 "),
                Some("head OFFICE"),
            ),
        )
        .await
        .unwrap();
        assert!(first.created);
        let again = supply(
            &db,
            "watchtower",
            brightfield,
            "host-1042",
            &details("Server", "srv-01", None, None),
        )
        .await
        .unwrap();
        assert_eq!(
            (again.asset.id, again.created),
            (first.asset.id, false),
            "the same reference updates the same asset"
        );
        assert_eq!(again.asset.name, "srv-01", "answered as stored");
        let assets = crate::catalog::assets_of(&db, brightfield).await.unwrap();
        assert_eq!(assets.len(), 1, "no second asset");
        let asset = &assets[0];
        assert_eq!(
            (
                asset.kind_id,
                asset.name.as_str(),
                asset.serial_number.as_deref(),
                asset.site.is_none(),
                asset.source_addon.as_deref(),
                asset.reference.as_deref(),
            ),
            (
                server,
                "srv-01",
                None,
                true,
                Some("watchtower"),
                Some("host-1042")
            ),
            "what the add-on left out is cleared"
        );

        supply(
            &db,
            "watchtower",
            brightfield,
            "host-1042",
            &details("Laptop", "PC-0042", Some("SN-0042"), Some("Head office")),
        )
        .await
        .unwrap();
        let asset = &crate::catalog::assets_of(&db, brightfield).await.unwrap()[0];
        assert_eq!(
            (
                asset.serial_number.as_deref(),
                asset.site.as_ref().map(|s| s.id)
            ),
            (Some("SN-0042"), Some(head))
        );
    }

    /// The sentence of a refusal, failing the test on anything else.
    fn refusal<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(Error::Rule(why)) => why,
            other => panic!("not refused by a rule: {other:?}"),
        }
    }

    #[sqlx::test]
    async fn an_unknown_kind_or_a_site_not_active_at_the_customer_is_refused_by_name(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Old office", "Via del Glicine 3")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        testing::asset_kind(&db, "Laptop").await;
        testing::addon(&db, "watchtower").await;
        let try_supply = |reference: &'static str, details: SuppliedDetails| {
            let db = db.clone();
            async move { supply(&db, "watchtower", brightfield, reference, &details).await }
        };

        for (reference, details, sentence) in [
            (
                "host-1",
                details("Tablet", "TAB-01", None, None),
                "the help desk has no asset kind called Tablet: an add-on maps its own kinds to \
                 the help desk's list",
            ),
            (
                "host-1",
                details("Laptop", "PC-01", None, Some("Quay office")),
                "Brightfield has no site called Quay office: an asset is put only at one of its \
                 customer's active sites",
            ),
            (
                "host-1",
                details("Laptop", "PC-01", None, Some("old office")),
                "the site old office of Brightfield is retired: an asset is put only at one of \
                 its customer's active sites",
            ),
            (
                "host-1",
                details("Laptop", " ", None, None),
                "the name cannot be empty",
            ),
            (
                " ",
                details("Laptop", "PC-01", None, None),
                "the reference cannot be empty",
            ),
        ] {
            assert_eq!(refusal(try_supply(reference, details).await), sentence);
        }
        assert!(
            crate::catalog::assets_of(&db, brightfield)
                .await
                .unwrap()
                .is_empty(),
            "nothing was created"
        );

        // A reference names one asset, of one customer, for good.
        supply(
            &db,
            "watchtower",
            harbor,
            "host-2",
            &details("Laptop", "PC-02", None, None),
        )
        .await
        .unwrap();
        assert_eq!(
            refusal(try_supply("host-2", details("Laptop", "PC-02", None, None)).await),
            "the asset with the reference host-2 belongs to another customer: an asset never \
             changes customer"
        );
        // References are the add-on's own: another add-on's are another matter, and case counts.
        testing::addon(&db, "netscope").await;
        supply(
            &db,
            "netscope",
            brightfield,
            "host-2",
            &details("Laptop", "PC-02", None, None),
        )
        .await
        .unwrap();
        assert!(
            try_supply("HOST-2", details("Laptop", "PC-02", None, None))
                .await
                .unwrap()
                .created
        );

        crate::customers::offboard(&db, harbor).await.unwrap();
        assert_eq!(
            refusal(
                supply(
                    &db,
                    "watchtower",
                    harbor,
                    "host-2",
                    &details("Laptop", "PC-02", None, None),
                )
                .await
            ),
            "the customer is offboarded"
        );
        // Nor are an offboarded customer's assets updated or retired.
        let (harbor_pc,): (Uuid,) = sqlx::query_as("SELECT id FROM assets WHERE customer_id = $1")
            .bind(harbor)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(
            refusal(
                update(
                    &db,
                    "watchtower",
                    harbor_pc,
                    &details("Laptop", "PC-02", None, None)
                )
                .await
            ),
            "the customer is offboarded"
        );
        assert_eq!(
            refusal(retire(&db, "watchtower", harbor_pc).await),
            "the customer is offboarded"
        );
    }

    #[sqlx::test]
    async fn an_addon_updates_and_retires_only_the_assets_it_is_the_source_of(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        testing::asset_kind(&db, "Laptop").await;
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        let by_hand = testing::asset(&db, brightfield, "Laptop", "PC-0001").await;
        let theirs = supply(
            &db,
            "netscope",
            brightfield,
            "n-7",
            &details("Laptop", "PC-0007", None, None),
        )
        .await
        .unwrap()
        .asset
        .id;
        let mine = supply(
            &db,
            "watchtower",
            brightfield,
            "host-1",
            &details("Laptop", "PC-0042", None, None),
        )
        .await
        .unwrap()
        .asset
        .id;
        let renamed = details("Laptop", "Renamed", None, None);

        for (asset, sentence) in [
            (
                by_hand,
                "PC-0001 was entered by hand: an add-on changes only the assets it is the source of",
            ),
            (
                theirs,
                "PC-0007 comes from the add-on netscope: an add-on changes only the assets it is \
                 the source of",
            ),
        ] {
            assert_eq!(
                refusal(update(&db, "watchtower", asset, &renamed).await),
                sentence
            );
            assert_eq!(refusal(retire(&db, "watchtower", asset).await), sentence);
        }
        assert!(matches!(
            update(&db, "watchtower", Uuid::new_v4(), &renamed).await,
            Err(Error::NotFound("asset"))
        ));
        assert!(matches!(
            retire(&db, "watchtower", Uuid::new_v4()).await,
            Err(Error::NotFound("asset"))
        ));

        update(&db, "watchtower", mine, &renamed).await.unwrap();
        retire(&db, "watchtower", mine).await.unwrap();
        retire(&db, "watchtower", mine)
            .await
            .expect("retiring again changes nothing");
        let assets = crate::catalog::assets_of(&db, brightfield).await.unwrap();
        let name_and_retired = |id: Uuid| {
            let a = assets.iter().find(|a| a.id == id).unwrap();
            (a.name.clone(), a.retired_at.is_some())
        };
        assert_eq!(name_and_retired(mine), ("Renamed".to_owned(), true));
        assert_eq!(name_and_retired(by_hand), ("PC-0001".to_owned(), false));
        assert_eq!(name_and_retired(theirs), ("PC-0007".to_owned(), false));
    }

    #[sqlx::test]
    async fn an_asset_back_in_the_addon_s_system_comes_back_and_keeps_its_holder(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        testing::asset_kind(&db, "Laptop").await;
        testing::addon(&db, "watchtower").await;
        let laptop = details("Laptop", "PC-0042", None, None);
        let pc = supply(&db, "watchtower", brightfield, "host-1", &laptop)
            .await
            .unwrap()
            .asset
            .id;
        let holder = |db: PgPool| async move {
            let asset = crate::catalog::assets_of(&db, brightfield).await.unwrap();
            (
                asset[0].holder.as_ref().map(|h| h.id),
                asset[0].retired_at.is_some(),
            )
        };

        // Agents give it a holder; the add-on sending it again leaves them.
        sqlx::query("UPDATE assets SET holder_id = $2 WHERE id = $1")
            .bind(pc)
            .bind(ada)
            .execute(&db)
            .await
            .unwrap();
        supply(&db, "watchtower", brightfield, "host-1", &laptop)
            .await
            .unwrap();
        update(&db, "watchtower", pc, &laptop).await.unwrap();
        assert_eq!(holder(db.clone()).await, (Some(ada), false));

        // Retired when it left the add-on's system, it loses its holder, and comes back when the
        // add-on supplies it again.
        retire(&db, "watchtower", pc).await.unwrap();
        assert_eq!(holder(db.clone()).await, (None, true));
        let back = supply(&db, "watchtower", brightfield, "host-1", &laptop)
            .await
            .unwrap();
        assert_eq!((back.asset.id, back.created), (pc, false));
        assert_eq!(holder(db.clone()).await, (None, false));
        // Updated by its id, it does not come back: what the add-on sends about an asset it retired
        // may be older than the retirement.
        retire(&db, "watchtower", pc).await.unwrap();
        assert_eq!(
            refusal(
                update(
                    &db,
                    "watchtower",
                    pc,
                    &details("Laptop", "Renamed", None, None)
                )
                .await
            ),
            "PC-0042 is retired: supplying it again with its reference brings it back"
        );
        let asset = &crate::catalog::assets_of(&db, brightfield).await.unwrap()[0];
        assert_eq!(
            (asset.name.as_str(), asset.retired_at.is_some()),
            ("PC-0042", true)
        );
    }

    /// The database keeps an asset's source within the rules even when the code above is
    /// bypassed: a registered add-on with a trimmed reference, which stays the asset's source.
    #[sqlx::test]
    async fn the_database_keeps_the_source_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        let by_hand = testing::asset(&db, brightfield, "Laptop", "PC-0001").await;
        let set = |addon: Option<&'static str>, reference: Option<&'static str>| {
            let db = db.clone();
            async move {
                sqlx::query("UPDATE assets SET source_addon = $2, source_ref = $3 WHERE id = $1")
                    .bind(by_hand)
                    .bind(addon)
                    .bind(reference)
                    .execute(&db)
                    .await
            }
        };

        assert!(
            set(Some("nobody"), Some("host-1")).await.is_err(),
            "an add-on that is not registered"
        );
        assert!(
            set(Some("watchtower"), Some(" host-1")).await.is_err(),
            "an untrimmed reference"
        );
        assert!(
            set(Some("watchtower"), Some("")).await.is_err(),
            "a blank reference"
        );
        assert!(
            set(Some("watchtower"), None).await.is_err(),
            "a source without a reference"
        );
        assert!(set(Some("watchtower"), Some("host-1")).await.is_ok());
        assert!(
            set(Some("netscope"), Some("host-1")).await.is_err(),
            "taken over by another add-on"
        );
        assert!(
            set(Some("watchtower"), Some("host-2")).await.is_err(),
            "known by another reference"
        );
        assert!(set(None, None).await.is_err(), "left without its source");
    }
}
