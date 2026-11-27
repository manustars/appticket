//! Linked add-ons (docs/domain-rules.md, "Add-ons"; ADR 0002): besides its source, an asset can be
//! linked to other add-ons, each knowing it by a reference of its own, once agents who manage the
//! catalog accept a link proposal. A linked add-on's calls with its reference reach the asset but
//! change nothing of it: it only supplies live details and links, and unlinks itself when the asset
//! leaves its system, which retires nothing.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::catalog::StoredAsset;
use crate::error::{Error, Result, rule};
use crate::supplied_assets::reference_of_another_customer;

/// An add-on linked to an asset, which is not its source, with the reference it knows it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinkedAddon {
    pub addon: String,
    /// The add-on's own reference for the asset, which is not an external reference.
    pub reference: String,
}

/// The asset `addon` is linked to by `reference`, with its customer, if any.
pub(crate) async fn linked_by(
    tx: &mut Transaction<'_, Postgres>,
    addon: &str,
    reference: &str,
) -> Result<Option<(Uuid, Uuid)>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.customer_id FROM asset_linked_addons l JOIN assets a ON a.id = l.asset_id
         WHERE l.addon = $1 AND l.reference = $2",
    )
    .bind(addon)
    .bind(reference)
    .fetch_optional(&mut **tx)
    .await?)
}

/// The reference `addon` is linked to the asset by, if it is.
pub(crate) async fn reference_of(
    tx: &mut Transaction<'_, Postgres>,
    addon: &str,
    asset: Uuid,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT reference FROM asset_linked_addons WHERE addon = $1 AND asset_id = $2",
    )
    .bind(addon)
    .bind(asset)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Links `addon` to the asset with `reference`, as the `proposal`, accepted in this transaction,
/// proposed. The caller has locked the asset's customer and the asset, and checked it; what the
/// database still refuses, such as an asset given that add-on as its source meanwhile, is refused
/// with a sentence.
pub(crate) async fn link(
    tx: &mut Transaction<'_, Postgres>,
    proposal: Uuid,
    addon: &str,
    reference: &str,
    asset: &StoredAsset,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO asset_linked_addons (asset_id, addon, reference, proposal_id)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(asset.id)
    .bind(addon)
    .bind(reference)
    .bind(proposal)
    .execute(&mut **tx)
    .await
    .map_err(|e| {
        let why = match &e {
            sqlx::Error::Database(d) => match d.constraint() {
                // The reference linked meanwhile to an asset of another customer, whose lock does
                // not keep this one out.
                Some("asset_linked_addons_pkey") => reference_of_another_customer(reference),
                Some("addon_reference_names_one_asset") => format!(
                    "{addon} knows another asset by the reference {reference}: a reference names \
                     one asset"
                ),
                Some("asset_linked_addons_not_the_source") => format!(
                    "{addon} is the source of {}: an add-on is not linked to an asset it is the \
                     source of",
                    asset.name
                ),
                Some("asset_linked_addons_asset_id_addon_key") => format!(
                    "{addon} is linked to {} already: an add-on knows an asset by one reference",
                    asset.name
                ),
                _ => return e.into(),
            },
            _ => return e.into(),
        };
        Error::Rule(why)
    })?;
    Ok(())
}

/// Unlinks `addon` from the asset, when the asset leaves the add-on's system: the asset stays as it
/// was, and the add-on's reference no longer reaches it. Unlinking again changes nothing. Refused
/// for an add-on that never was linked to the asset, its source included, which retires it instead,
/// and for an asset of an offboarded customer.
pub async fn unlink(db: &PgPool, addon: &str, asset: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    let (_, stored) = crate::supplied_assets::lock_with_customer(&mut tx, asset).await?;
    if stored.source_addon.as_deref() == Some(addon) {
        return rule(format!(
            "{addon} is the source of {}: an asset's source retires it when it leaves its system, \
             and is never unlinked",
            stored.name
        ));
    }
    let unlinked =
        sqlx::query("DELETE FROM asset_linked_addons WHERE asset_id = $1 AND addon = $2")
            .bind(asset)
            .bind(addon)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if unlinked == 0 {
        // Unlinking again: an accepted proposal says it was linked.
        let was_linked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM asset_link_proposals
                            WHERE asset_id = $1 AND addon = $2 AND state = 'accepted')",
        )
        .bind(asset)
        .bind(addon)
        .fetch_one(&mut *tx)
        .await?;
        if !was_linked {
            return rule(format!(
                "{addon} is not linked to {}: an add-on unlinks itself only from an asset it is \
                 linked to",
                stored.name
            ));
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::asset_link_proposals::{self, AssetLinkProposal};
    use crate::supplied_assets::{self, Supplied, SuppliedDetails, supply};
    use crate::testing::{self, refusal};

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

    /// The proposal supplying gave instead of an asset, failing the test on anything else.
    fn proposal(supplied: Supplied) -> AssetLinkProposal {
        match supplied {
            Supplied::LinkProposed(proposal) => proposal,
            other => panic!("no link proposal: {other:?}"),
        }
    }

    /// The asset of `customer` called `name`, entered by hand, with the serial number and site given.
    async fn by_hand(
        db: &PgPool,
        customer: Uuid,
        name: &str,
        serial: Option<&str>,
        site: Option<Uuid>,
    ) -> Uuid {
        let kind = testing::asset_kind(db, "Laptop").await;
        crate::catalog::create_asset(
            db,
            customer,
            &testing::asset_details(kind, name, serial, site, None),
        )
        .await
        .unwrap()
    }

    /// The add-ons linked to the asset, with their references, as agents see them.
    async fn linked(db: &PgPool, asset: Uuid) -> Vec<(String, String)> {
        crate::catalog::asset(db, asset)
            .await
            .unwrap()
            .linked_addons
            .into_iter()
            .map(|l| (l.addon, l.reference))
            .collect()
    }

    #[sqlx::test]
    async fn accepting_a_link_proposal_links_the_addon_with_its_reference(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        testing::asset_kind(&db, "Server").await;
        testing::addon(&db, "watchtower").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        let pc = by_hand(&db, brightfield, "PC-0042", Some("SN-0042"), Some(head)).await;
        let sent = details("Laptop", "Ada's laptop", Some("sn-0042"), None);
        let proposed = proposal(
            supply(&db, "watchtower", brightfield, "host-1", &sent)
                .await
                .unwrap(),
        );

        asset_link_proposals::accept(&db, proposed.id, carla)
            .await
            .unwrap();
        assert_eq!(
            linked(&db, pc).await,
            [("watchtower".to_owned(), "host-1".to_owned())]
        );
        let asset = crate::catalog::asset(&db, pc).await.unwrap();
        assert_eq!(asset.source_addon, None, "it is not the asset's source");
        assert!(
            asset_link_proposals::open_of(&db, brightfield)
                .await
                .unwrap()
                .is_empty()
        );
        for decided in [
            asset_link_proposals::accept(&db, proposed.id, carla).await,
            asset_link_proposals::discard(&db, proposed.id, carla).await,
        ] {
            assert_eq!(refusal(decided), "the link proposal is no longer open");
        }
        assert!(matches!(
            asset_link_proposals::accept(&db, Uuid::new_v4(), carla).await,
            Err(crate::error::Error::NotFound("link proposal"))
        ));

        // From then on its reference reaches the asset: sending what the asset has changes
        // nothing and answers it, while any change to what it has is refused.
        // Names, kinds, sites and serial numbers are compared ignoring case, as they matched.
        for same in [
            details("laptop", " PC-0042 ", Some("SN-0042"), Some("head office")),
            details("Laptop", "pc-0042", Some("sn-0042"), Some("Head office")),
        ] {
            let reached = supply(&db, "watchtower", brightfield, "host-1", &same)
                .await
                .unwrap();
            assert_eq!(
                (reached.asset().id, reached.created()),
                (pc, false),
                "the linked asset, not a new one: {same:?}"
            );
            supplied_assets::update(&db, "watchtower", pc, &same)
                .await
                .expect("its id reaches it too");
        }
        let refused = "PC-0042 was entered by hand: watchtower is linked to it, and an add-on \
                       changes only the assets it is the source of";
        for changed in [
            sent,
            details("Laptop", "PC-0043", Some("SN-0042"), Some("Head office")),
            details("Server", "PC-0042", Some("SN-0042"), Some("Head office")),
            details("Laptop", "PC-0042", None, Some("Head office")),
            details("Laptop", "PC-0042", Some("SN-0043"), Some("Head office")),
            details("Laptop", "PC-0042", Some("SN-0042"), None),
        ] {
            assert_eq!(
                refusal(supply(&db, "watchtower", brightfield, "host-1", &changed).await),
                refused,
                "{changed:?}"
            );
            assert_eq!(
                refusal(supplied_assets::update(&db, "watchtower", pc, &changed).await),
                refused,
                "{changed:?}"
            );
        }
        let assets = crate::catalog::assets_of(&db, brightfield).await.unwrap();
        assert_eq!(assets.len(), 1, "no second asset");
        assert_eq!(
            (
                assets[0].name.as_str(),
                assets[0].serial_number.as_deref(),
                assets[0].site.as_ref().map(|s| s.id)
            ),
            ("PC-0042", Some("SN-0042"), Some(head)),
            "nothing changed"
        );
        assert!(
            asset_link_proposals::open_of(&db, brightfield)
                .await
                .unwrap()
                .is_empty(),
            "nor proposed again"
        );
    }

    #[sqlx::test]
    async fn accepting_needs_an_active_asset_not_linked_to_the_addon_yet(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        testing::addon(&db, "watchtower").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        let pc = by_hand(&db, brightfield, "PC-0042", None, None).await;
        let printer = by_hand(&db, brightfield, "PRN-01", None, None).await;
        by_hand(&db, harbor, "PC-0099", None, None).await;
        let propose = |customer: Uuid, reference: &'static str, name: &'static str| {
            let db = db.clone();
            async move {
                proposal(
                    supply(
                        &db,
                        "watchtower",
                        customer,
                        reference,
                        &details("Laptop", name, None, None),
                    )
                    .await
                    .unwrap(),
                )
            }
        };
        // Two hosts of the add-on with the asset's name, proposed while neither is decided.
        let first = propose(brightfield, "host-1", "PC-0042").await;
        let second = propose(brightfield, "host-2", "pc-0042").await;
        let on_printer = propose(brightfield, "host-3", "PRN-01").await;
        let of_harbor = propose(harbor, "host-4", "PC-0099").await;

        asset_link_proposals::accept(&db, first.id, carla)
            .await
            .unwrap();
        assert_eq!(
            refusal(asset_link_proposals::accept(&db, second.id, carla).await),
            "watchtower is linked to PC-0042 already, by the reference host-1: an add-on knows \
             an asset by one reference"
        );
        // An asset the add-on is linked to matches no other host of its own, as its own assets
        // do not: a third host with that name is an asset of its own.
        let third = supply(
            &db,
            "watchtower",
            brightfield,
            "host-5",
            &details("Laptop", "PC-0042", None, None),
        )
        .await
        .unwrap();
        assert!(third.created(), "{third:?}");

        crate::catalog::retire_asset(&db, printer).await.unwrap();
        assert_eq!(
            refusal(asset_link_proposals::accept(&db, on_printer.id, carla).await),
            "PRN-01 is retired: an add-on is linked only to an active asset"
        );
        crate::customers::offboard(&db, harbor).await.unwrap();
        assert_eq!(
            refusal(asset_link_proposals::accept(&db, of_harbor.id, carla).await),
            "the customer is offboarded"
        );
        let open: Vec<Uuid> = asset_link_proposals::open_of(&db, brightfield)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(open, [second.id, on_printer.id], "refused ones stay open");
        assert_eq!(
            linked(&db, pc).await,
            [("watchtower".to_owned(), "host-1".to_owned())]
        );
        assert!(linked(&db, printer).await.is_empty());
    }

    #[sqlx::test]
    async fn a_linked_reference_brings_back_no_retired_asset_and_stays_with_its_customer(
        db: PgPool,
    ) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        testing::asset_kind(&db, "Laptop").await;
        let server = details("Laptop", "SRV-01", Some("SN-1"), None);
        let srv = supply(&db, "netscope", brightfield, "n-1", &server)
            .await
            .unwrap()
            .asset()
            .id;
        let proposed = proposal(
            supply(&db, "watchtower", brightfield, "host-1", &server)
                .await
                .unwrap(),
        );
        asset_link_proposals::accept(&db, proposed.id, carla)
            .await
            .unwrap();

        // Only its source brings a retired asset back: the linked add-on sending what it has
        // answers it as it is, retired.
        supplied_assets::retire(&db, "netscope", srv).await.unwrap();
        let reached = supply(&db, "watchtower", brightfield, "host-1", &server)
            .await
            .unwrap();
        assert_eq!(
            (
                reached.asset().id,
                reached.created(),
                reached.asset().retired_at.is_some()
            ),
            (srv, false, true)
        );
        supplied_assets::update(&db, "watchtower", srv, &server)
            .await
            .unwrap();
        let after = crate::catalog::asset(&db, srv).await.unwrap();
        assert!(after.retired_at.is_some(), "still retired");

        // A reference names one asset of one customer, linked or not.
        assert_eq!(
            refusal(supply(&db, "watchtower", harbor, "host-1", &server).await),
            "the reference host-1 belongs to another customer: a reference names one asset of one \
             customer"
        );
    }

    /// What the database refuses beyond the core's own checks, such as an asset given its source
    /// by hand meanwhile, is refused with a sentence too.
    #[sqlx::test]
    async fn accepting_what_the_database_refuses_is_refused_with_a_sentence(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        testing::addon(&db, "watchtower").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        let pc = by_hand(&db, brightfield, "PC-0042", None, None).await;
        by_hand(&db, brightfield, "PRN-01", None, None).await;
        let other = by_hand(&db, brightfield, "PC-0099", None, None).await;
        let on_pc = proposal(
            supply(
                &db,
                "watchtower",
                brightfield,
                "host-1",
                &details("Laptop", "PC-0042", None, None),
            )
            .await
            .unwrap(),
        );
        let on_printer = proposal(
            supply(
                &db,
                "watchtower",
                brightfield,
                "host-2",
                &details("Laptop", "PRN-01", None, None),
            )
            .await
            .unwrap(),
        );
        let set_source = |asset: Uuid, reference: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "UPDATE assets SET source_addon = 'watchtower', source_ref = $2 WHERE id = $1",
                )
                .bind(asset)
                .bind(reference)
                .execute(&db)
                .await
                .unwrap();
            }
        };
        set_source(pc, "host-9").await;
        set_source(other, "host-2").await;

        assert_eq!(
            refusal(asset_link_proposals::accept(&db, on_pc.id, carla).await),
            "watchtower is the source of PC-0042: an add-on is not linked to an asset it is the \
             source of"
        );
        assert_eq!(
            refusal(asset_link_proposals::accept(&db, on_printer.id, carla).await),
            "watchtower knows another asset by the reference host-2: a reference names one asset"
        );
        assert_eq!(
            asset_link_proposals::open_of(&db, brightfield)
                .await
                .unwrap()
                .len(),
            2,
            "both stay open"
        );
    }

    /// Two transactions at once, one linking an add-on with a reference and one giving an asset
    /// that add-on and reference as its source, do not both commit: the later waits for the
    /// earlier and is refused.
    #[sqlx::test]
    async fn a_reference_is_not_linked_and_made_a_source_at_once(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        testing::addon(&db, "watchtower").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        let pc = by_hand(&db, brightfield, "PC-0042", None, None).await;
        let printer = by_hand(&db, brightfield, "PRN-01", None, None).await;
        let proposal = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO asset_link_proposals (id, addon, reference, customer_id, asset_id,
                                               matched_by, name, state, decided_at, decided_by)
             VALUES ($1, 'watchtower', 'host-1', $2, $3, 'name', 'PC-0042', 'accepted', now(),
                     $4)",
        )
        .bind(proposal)
        .bind(brightfield)
        .bind(pc)
        .bind(carla)
        .execute(&db)
        .await
        .unwrap();

        let mut linking = db.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO asset_linked_addons (asset_id, addon, reference, proposal_id)
             VALUES ($1, 'watchtower', 'host-1', $2)",
        )
        .bind(pc)
        .bind(proposal)
        .execute(&mut *linking)
        .await
        .unwrap();
        let sourcing = tokio::spawn({
            let db = db.clone();
            async move {
                sqlx::query(
                    "UPDATE assets SET source_addon = 'watchtower', source_ref = 'host-1'
                     WHERE id = $1",
                )
                .bind(printer)
                .execute(&db)
                .await
            }
        });
        assert!(testing::some_statement_waits_for_a_lock(&db).await);
        linking.commit().await.unwrap();
        assert!(
            sourcing.await.unwrap().is_err(),
            "refused once the link is in"
        );
    }

    #[sqlx::test]
    async fn a_linked_addon_unlinks_itself_and_the_asset_stays_as_it_was(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        testing::asset_kind(&db, "Laptop").await;
        let server = details("Laptop", "SRV-01", Some("SN-1"), None);
        let srv = supply(&db, "netscope", brightfield, "n-1", &server)
            .await
            .unwrap()
            .asset()
            .id;
        sqlx::query("UPDATE assets SET holder_id = $2 WHERE id = $1")
            .bind(srv)
            .bind(ada)
            .execute(&db)
            .await
            .unwrap();
        let proposed = proposal(
            supply(&db, "watchtower", brightfield, "host-1", &server)
                .await
                .unwrap(),
        );
        asset_link_proposals::accept(&db, proposed.id, carla)
            .await
            .unwrap();
        let shown = crate::catalog::asset(&db, srv).await.unwrap();
        assert_eq!(
            (
                shown.source_addon.as_deref(),
                shown.reference.as_deref(),
                linked(&db, srv).await
            ),
            (
                Some("netscope"),
                Some("n-1"),
                vec![("watchtower".to_owned(), "host-1".to_owned())]
            ),
            "the asset lists its source and its linked add-ons"
        );

        // It changes nothing of the asset, nor retires it, which only its source does.
        assert_eq!(
            refusal(
                supplied_assets::update(
                    &db,
                    "watchtower",
                    srv,
                    &details("Laptop", "SRV-02", Some("SN-1"), None)
                )
                .await
            ),
            "SRV-01 comes from the add-on netscope: watchtower is linked to it, and an add-on \
             changes only the assets it is the source of"
        );
        assert_eq!(
            refusal(supplied_assets::retire(&db, "watchtower", srv).await),
            "watchtower is linked to SRV-01: an add-on linked to an asset unlinks itself from it, \
             and never retires it"
        );
        // Its source is not unlinked: it retires the asset when it leaves its system.
        assert_eq!(
            refusal(unlink(&db, "netscope", srv).await),
            "netscope is the source of SRV-01: an asset's source retires it when it leaves its \
             system, and is never unlinked"
        );
        assert!(matches!(
            unlink(&db, "watchtower", Uuid::new_v4()).await,
            Err(Error::NotFound("asset"))
        ));

        testing::addon(&db, "scanner").await;
        assert_eq!(
            refusal(unlink(&db, "scanner", srv).await),
            "scanner is not linked to SRV-01: an add-on unlinks itself only from an asset it is \
             linked to"
        );

        unlink(&db, "watchtower", srv).await.unwrap();
        unlink(&db, "watchtower", srv)
            .await
            .expect("unlinking again changes nothing");
        let after = crate::catalog::asset(&db, srv).await.unwrap();
        assert_eq!(
            (
                after.name.as_str(),
                after.source_addon.as_deref(),
                after.holder.map(|h| h.id),
                after.retired_at.is_none(),
                after.linked_addons.is_empty(),
            ),
            ("SRV-01", Some("netscope"), Some(ada), true, true),
            "the asset stays as it was"
        );
        // Its reference no longer reaches the asset: it is proposed again, for agents to decide.
        let again = proposal(
            supply(&db, "watchtower", brightfield, "host-1", &server)
                .await
                .unwrap(),
        );
        assert_ne!(again.id, proposed.id);
        assert_eq!(again.asset_id, srv);

        // Nothing of an offboarded customer's catalog changes.
        asset_link_proposals::accept(&db, again.id, carla)
            .await
            .unwrap();
        crate::customers::offboard(&db, brightfield).await.unwrap();
        assert_eq!(
            refusal(unlink(&db, "watchtower", srv).await),
            "the customer is offboarded"
        );
    }

    /// The database keeps linked add-ons within the rules even when the core is bypassed: a
    /// reference names one asset of an add-on, as its source or linked, an add-on is linked to an
    /// asset once and never to one it is the source of, through a proposal for that very link, and
    /// a link is removed, never changed.
    #[sqlx::test]
    async fn the_database_keeps_the_rules_of_linked_addons(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        let pc = testing::asset(&db, brightfield, "Laptop", "PC-0042").await;
        let printer = testing::asset(&db, brightfield, "Printer", "PRN-01").await;
        let supplied = testing::asset(&db, brightfield, "Laptop", "SRV-01").await;
        sqlx::query(
            "UPDATE assets SET source_addon = 'netscope', source_ref = 'n-1' WHERE id = $1",
        )
        .bind(supplied)
        .execute(&db)
        .await
        .unwrap();
        // A proposal accepted by Carla, unless it is to be left open.
        let proposal_in =
            |state: &'static str, addon: &'static str, reference: &'static str, asset: Uuid| {
                let db = db.clone();
                async move {
                    let id = Uuid::new_v4();
                    sqlx::query(
                    "INSERT INTO asset_link_proposals (id, addon, reference, customer_id, asset_id,
                                                       matched_by, name, state, decided_at,
                                                       decided_by)
                     VALUES ($1, $2, $3, $4, $5, 'name', 'PC-0042', $6,
                             CASE WHEN $6 = 'open' THEN NULL ELSE now() END,
                             CASE WHEN $6 = 'open' THEN NULL ELSE $7 END)",
                )
                .bind(id)
                .bind(addon)
                .bind(reference)
                .bind(brightfield)
                .bind(asset)
                .bind(state)
                .bind(carla)
                .execute(&db)
                .await
                .unwrap();
                    id
                }
            };
        let proposal = |addon: &'static str, reference: &'static str, asset: Uuid| {
            proposal_in("accepted", addon, reference, asset)
        };
        let link = |proposal: Uuid, addon: &'static str, reference: &'static str, asset: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO asset_linked_addons (asset_id, addon, reference, proposal_id)
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(asset)
                .bind(addon)
                .bind(reference)
                .bind(proposal)
                .execute(&db)
                .await
            }
        };

        let first = proposal_in("open", "watchtower", "host-1", pc).await;
        assert!(
            link(first, "watchtower", "host-1", pc).await.is_err(),
            "a proposal not accepted"
        );
        let decide = |state: &'static str, by: Option<Uuid>| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "UPDATE asset_link_proposals
                     SET state = $2,
                         decided_at = CASE WHEN $2 = 'open' THEN NULL ELSE now() END,
                         decided_by = $3
                     WHERE id = $1",
                )
                .bind(first)
                .bind(state)
                .bind(by)
                .execute(&db)
                .await
            }
        };
        assert!(
            decide("accepted", None).await.is_err(),
            "accepted by nobody"
        );
        assert!(decide("accepted", Some(carla)).await.is_ok());
        assert!(
            link(first, "watchtower", "host-2", pc).await.is_err(),
            "a link the proposal did not propose"
        );
        assert!(link(first, "watchtower", "host-1", pc).await.is_ok());
        assert!(
            decide("open", None).await.is_err(),
            "an accepted proposal is not opened again"
        );
        assert!(
            decide("discarded", Some(carla)).await.is_err(),
            "nor discarded"
        );

        let second = proposal("watchtower", "host-2", pc).await;
        assert!(
            link(second, "watchtower", "host-2", pc).await.is_err(),
            "linked to the same asset twice"
        );
        let third = proposal("watchtower", "host-3", printer).await;
        assert!(
            link(third, "watchtower", "host-1", printer).await.is_err(),
            "a reference that names another asset"
        );
        let theirs = proposal("netscope", "n-2", supplied).await;
        assert!(
            link(theirs, "netscope", "n-2", supplied).await.is_err(),
            "linked to the asset it is the source of"
        );
        let source_ref = proposal("netscope", "n-1", pc).await;
        assert!(
            link(source_ref, "netscope", "n-1", pc).await.is_err(),
            "the reference its source knows another asset by"
        );

        // Seen from the asset: its source is not linked to it, nor known by a linked reference.
        let set_source = |asset: Uuid, addon: &'static str, reference: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query("UPDATE assets SET source_addon = $2, source_ref = $3 WHERE id = $1")
                    .bind(asset)
                    .bind(addon)
                    .bind(reference)
                    .execute(&db)
                    .await
            }
        };
        assert!(
            set_source(pc, "watchtower", "host-9").await.is_err(),
            "a source already linked to it"
        );
        assert!(
            set_source(printer, "watchtower", "host-1").await.is_err(),
            "a reference that names a linked asset"
        );
        assert!(set_source(printer, "watchtower", "host-8").await.is_ok());

        assert!(
            sqlx::query("UPDATE asset_linked_addons SET reference = 'host-7'")
                .execute(&db)
                .await
                .is_err(),
            "a link is not changed"
        );
        sqlx::query("DELETE FROM asset_linked_addons")
            .execute(&db)
            .await
            .expect("but removed");
    }
}
