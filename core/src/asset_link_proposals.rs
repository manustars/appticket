//! Link proposals (docs/domain-rules.md, "Add-ons"; ADR 0002): when an add-on would create an asset
//! and its customer already has an active one with the same name, ignoring case, or the same serial
//! number, entered by hand or supplied by another add-on, the core records a proposal that the
//! add-on's asset is that one instead of creating a second. Agents who manage the catalog discard
//! it, and the add-on's next attempt with that reference then creates its own asset.

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::catalog::AssetKey;
use crate::error::{Error, Result, rule};

/// What the asset the add-on would supply has in common with the asset it matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    /// The same serial number, ignoring case.
    SerialNumber,
    /// The same name, ignoring case.
    Name,
}

impl MatchedBy {
    fn as_str(self) -> &'static str {
        match self {
            MatchedBy::SerialNumber => "serial_number",
            MatchedBy::Name => "name",
        }
    }
}

/// A proposal that the asset an add-on would supply is an asset its customer already has.
#[derive(Debug, Serialize, ToSchema)]
pub struct AssetLinkProposal {
    pub id: Uuid,
    /// The add-on that would supply the asset.
    pub addon: String,
    /// The add-on's own reference for the asset it would supply.
    pub reference: String,
    pub customer_id: Uuid,
    /// The name the add-on sent.
    pub name: String,
    /// The serial number the add-on sent, if any.
    pub serial_number: Option<String>,
    /// What the two assets have in common: when both a serial number and a name match, the serial
    /// number decides.
    pub matched_by: MatchedBy,
    /// The asset it matched, of the same customer.
    pub asset_id: Uuid,
    /// That asset's name as it is now.
    pub asset_name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct ProposalRow {
    id: Uuid,
    addon: String,
    reference: String,
    customer_id: Uuid,
    name: String,
    serial_number: Option<String>,
    matched_by: String,
    asset_id: Uuid,
    asset_name: String,
    created_at: OffsetDateTime,
}

impl From<ProposalRow> for AssetLinkProposal {
    fn from(row: ProposalRow) -> Self {
        AssetLinkProposal {
            id: row.id,
            addon: row.addon,
            reference: row.reference,
            customer_id: row.customer_id,
            name: row.name,
            serial_number: row.serial_number,
            matched_by: if row.matched_by == "serial_number" {
                MatchedBy::SerialNumber
            } else {
                MatchedBy::Name
            },
            asset_id: row.asset_id,
            asset_name: row.asset_name,
            created_at: row.created_at,
        }
    }
}

/// The proposals picked by the end of the query (on `asset_link_proposals p`), with the name of the
/// asset each matched.
macro_rules! select_proposals {
    ($rest:literal) => {
        concat!(
            "SELECT p.id, p.addon, p.reference, p.customer_id, p.name, p.serial_number,
                    p.matched_by, p.asset_id, a.name AS asset_name, p.created_at
             FROM asset_link_proposals p
             JOIN assets a ON a.id = p.asset_id
             ",
            $rest
        )
    };
}

/// When `addon` would create the asset of `customer` it knows by `reference`, the link proposal
/// it gets instead, if any: the open one for that reference, already recorded, or a new one when
/// the customer has an active asset with the same serial number, else the same name, that the
/// add-on is not the source of. None when there is no match, or when agents discarded a proposal
/// for that reference: the add-on then creates its own asset. The caller has locked the customer,
/// so that two attempts at once do not both find no proposal.
pub(crate) async fn proposed(
    tx: &mut Transaction<'_, Postgres>,
    addon: &str,
    customer: Uuid,
    reference: &str,
    name: &str,
    serial_number: Option<&str>,
) -> Result<Option<AssetLinkProposal>> {
    let open: Option<ProposalRow> = sqlx::query_as(select_proposals!(
        "WHERE p.addon = $1 AND p.reference = $2 AND p.state = 'open'"
    ))
    .bind(addon)
    .bind(reference)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(open) = open {
        if open.customer_id != customer {
            return rule(another_customer(reference));
        }
        return Ok(Some(open.into()));
    }
    let discarded: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM asset_link_proposals
                        WHERE addon = $1 AND reference = $2 AND customer_id = $3
                          AND state = 'discarded')",
    )
    .bind(addon)
    .bind(reference)
    .bind(customer)
    .fetch_one(&mut **tx)
    .await?;
    if discarded {
        return Ok(None);
    }
    let Some((asset, matched_by)) = matching(tx, addon, customer, name, serial_number).await?
    else {
        return Ok(None);
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO asset_link_proposals (id, addon, reference, customer_id, asset_id,
                                           matched_by, name, serial_number)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(addon)
    .bind(reference)
    .bind(customer)
    .bind(asset)
    .bind(matched_by.as_str())
    .bind(name)
    .bind(serial_number)
    .execute(&mut **tx)
    .await
    .map_err(|e| match e {
        // The same reference, proposed at the same time for another customer: the customer
        // locked keeps out the attempts for this one.
        sqlx::Error::Database(d) if d.constraint() == Some("asset_link_proposals_one_open") => {
            Error::Rule(another_customer(reference))
        }
        other => other.into(),
    })?;
    let row: ProposalRow = sqlx::query_as(select_proposals!("WHERE p.id = $1"))
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(Some(row.into()))
}

fn another_customer(reference: &str) -> String {
    format!(
        "the reference {reference} is proposed as an asset of another customer: a reference names \
         one asset of one customer"
    )
}

/// The active asset of `customer` that `addon` is not the source of with the serial number, else
/// the name, both ignoring case; the oldest when several have it.
async fn matching(
    tx: &mut Transaction<'_, Postgres>,
    addon: &str,
    customer: Uuid,
    name: &str,
    serial_number: Option<&str>,
) -> Result<Option<(Uuid, MatchedBy)>> {
    let keys = serial_number
        .map(|serial| (AssetKey::SerialNumber, serial, MatchedBy::SerialNumber))
        .into_iter()
        .chain([(AssetKey::Name, name, MatchedBy::Name)]);
    for (key, value, matched_by) in keys {
        let found = crate::catalog::assets_by(&mut **tx, customer, key, value).await?;
        // Active ones first, then by when they were created.
        if let Some(asset) = found
            .into_iter()
            .find(|a| !a.retired && a.source_addon.as_deref() != Some(addon))
        {
            return Ok(Some((asset.id, matched_by)));
        }
    }
    Ok(None)
}

/// The open link proposals on the customer's assets, oldest first.
pub async fn open_of(db: &PgPool, customer: Uuid) -> Result<Vec<AssetLinkProposal>> {
    let rows: Vec<ProposalRow> = sqlx::query_as(select_proposals!(
        "WHERE p.customer_id = $1 AND p.state = 'open' ORDER BY p.created_at, p.id"
    ))
    .bind(customer)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(AssetLinkProposal::from).collect())
}

/// Discards an open link proposal, for `agent`: the add-on's next attempt with its reference creates
/// its own asset. Agents do not change an offboarded customer's catalog.
pub async fn discard(db: &PgPool, id: Uuid, agent: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    // A proposal never changes customer, so its customer can be read before it is locked; the
    // customer is locked first, as by supplying an asset.
    let customer: Uuid =
        sqlx::query_scalar("SELECT customer_id FROM asset_link_proposals WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(Error::NotFound("link proposal"))?;
    crate::customers::lock_active(&mut tx, customer).await?;
    let state: String =
        sqlx::query_scalar("SELECT state FROM asset_link_proposals WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if state != "open" {
        return rule("the link proposal is no longer open");
    }
    sqlx::query(
        "UPDATE asset_link_proposals SET state = 'discarded', decided_at = now(), decided_by = $2
         WHERE id = $1",
    )
    .bind(id)
    .bind(agent)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supplied_assets::{Supplied, SuppliedDetails, supply};
    use crate::testing;

    fn laptop(name: &str, serial: Option<&str>) -> SuppliedDetails {
        SuppliedDetails {
            kind: "Laptop".to_owned(),
            name: name.to_owned(),
            serial_number: serial.map(str::to_owned),
            site: None,
        }
    }

    /// The proposal supplying gave instead of an asset, failing the test on anything else.
    fn proposal(supplied: Supplied) -> AssetLinkProposal {
        match supplied {
            Supplied::LinkProposed(proposal) => proposal,
            other => panic!("no link proposal: {other:?}"),
        }
    }

    /// The asset supplying created, failing the test on anything else.
    fn created(supplied: Supplied) -> Uuid {
        match supplied {
            Supplied::Asset {
                asset,
                created: true,
            } => asset.id,
            other => panic!("not created: {other:?}"),
        }
    }

    /// An asset of `customer` entered by hand, with a serial number if given.
    async fn by_hand(db: &PgPool, customer: Uuid, name: &str, serial: Option<&str>) -> Uuid {
        let kind = testing::asset_kind(db, "Laptop").await;
        crate::catalog::create_asset(
            db,
            customer,
            &testing::asset_details(kind, name, serial, None, None),
        )
        .await
        .unwrap()
    }

    /// The sentence of a refusal, failing the test on anything else.
    fn refusal<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(Error::Rule(why)) => why,
            other => panic!("not refused by a rule: {other:?}"),
        }
    }

    #[sqlx::test]
    async fn an_asset_matching_one_of_its_customer_s_is_proposed_as_a_link_instead(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        testing::addon(&db, "watchtower").await;
        testing::addon(&db, "netscope").await;
        let pc = by_hand(&db, brightfield, "PC-0042", None).await;
        let numbered = by_hand(&db, brightfield, "PC-0043", Some("SN-7")).await;
        let old = by_hand(&db, brightfield, "PC-0001", Some("SN-1")).await;
        crate::catalog::retire_asset(&db, old).await.unwrap();
        by_hand(&db, harbor, "PC-0099", Some("SN-99")).await;
        let theirs = created(
            supply(&db, "netscope", brightfield, "n-1", &laptop("SRV-01", None))
                .await
                .unwrap(),
        );
        created(
            supply(
                &db,
                "watchtower",
                brightfield,
                "host-9",
                &laptop("PRN-01", None),
            )
            .await
            .unwrap(),
        );

        // The same name, ignoring case, as an asset entered by hand.
        let by_name = proposal(
            supply(
                &db,
                "watchtower",
                brightfield,
                "host-1",
                &laptop("pc-0042", None),
            )
            .await
            .unwrap(),
        );
        assert_eq!(
            (
                by_name.asset_id,
                by_name.asset_name.as_str(),
                by_name.matched_by,
                by_name.addon.as_str(),
                by_name.reference.as_str(),
                by_name.name.as_str(),
            ),
            (
                pc,
                "PC-0042",
                MatchedBy::Name,
                "watchtower",
                "host-1",
                "pc-0042"
            )
        );
        // The serial number decides over the name, which another asset has.
        let by_serial = proposal(
            supply(
                &db,
                "watchtower",
                brightfield,
                "host-2",
                &laptop("PC-0042", Some("sn-7")),
            )
            .await
            .unwrap(),
        );
        assert_eq!(
            (by_serial.asset_id, by_serial.matched_by),
            (numbered, MatchedBy::SerialNumber)
        );
        // An asset another add-on supplied matches too.
        let supplied_by_another = proposal(
            supply(
                &db,
                "watchtower",
                brightfield,
                "host-3",
                &laptop("srv-01", None),
            )
            .await
            .unwrap(),
        );
        assert_eq!(supplied_by_another.asset_id, theirs);

        // The add-on's own assets, retired assets and other customers' assets do not match.
        for (reference, details) in [
            ("host-10", laptop("prn-01", None)),
            ("host-11", laptop("PC-0001", Some("SN-1"))),
            ("host-12", laptop("PC-0099", Some("SN-99"))),
        ] {
            created(
                supply(&db, "watchtower", brightfield, reference, &details)
                    .await
                    .unwrap(),
            );
        }

        // Sending the same asset again records no second proposal while one is open.
        let again = proposal(
            supply(
                &db,
                "watchtower",
                brightfield,
                "host-1",
                &laptop("PC-0042", None),
            )
            .await
            .unwrap(),
        );
        assert_eq!(again.id, by_name.id);
        let open = open_of(&db, brightfield).await.unwrap();
        assert_eq!(
            open.iter().map(|p| p.id).collect::<Vec<_>>(),
            [by_name.id, by_serial.id, supplied_by_another.id]
        );
        assert!(open_of(&db, harbor).await.unwrap().is_empty());
        let mut created_by_watchtower: Vec<String> = crate::catalog::assets_of(&db, brightfield)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a.source_addon.as_deref() == Some("watchtower"))
            .map(|a| a.name)
            .collect();
        created_by_watchtower.sort();
        assert_eq!(
            created_by_watchtower,
            ["PC-0001", "PC-0099", "PRN-01", "prn-01"],
            "only the assets that matched nothing were created"
        );
        // The reference stays with its customer while it is proposed.
        assert_eq!(
            refusal(
                supply(
                    &db,
                    "watchtower",
                    harbor,
                    "host-1",
                    &laptop("PC-0042", None)
                )
                .await
            ),
            "the reference host-1 is proposed as an asset of another customer: a reference \
             names one asset of one customer"
        );
    }

    #[sqlx::test]
    async fn after_a_discard_the_addon_s_next_attempt_creates_its_own_asset(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        testing::addon(&db, "watchtower").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        let pc = by_hand(&db, brightfield, "PC-0042", Some("SN-0042")).await;
        by_hand(&db, harbor, "PC-0099", None).await;
        let host = laptop("PC-0042", Some("SN-0042"));
        let first = proposal(
            supply(&db, "watchtower", brightfield, "host-1", &host)
                .await
                .unwrap(),
        );

        discard(&db, first.id, carla).await.unwrap();
        assert!(open_of(&db, brightfield).await.unwrap().is_empty());
        assert_eq!(
            refusal(discard(&db, first.id, carla).await),
            "the link proposal is no longer open"
        );
        assert!(matches!(
            discard(&db, Uuid::new_v4(), carla).await,
            Err(Error::NotFound("link proposal"))
        ));
        let own = created(
            supply(&db, "watchtower", brightfield, "host-1", &host)
                .await
                .unwrap(),
        );
        assert_ne!(own, pc, "a second asset, the add-on's own");
        let supplied_again = supply(&db, "watchtower", brightfield, "host-1", &host)
            .await
            .unwrap();
        assert_eq!(
            (supplied_again.asset().id, supplied_again.created()),
            (own, false),
            "from then on its reference updates it"
        );
        assert!(open_of(&db, brightfield).await.unwrap().is_empty());

        // Agents change nothing of an offboarded customer's catalog.
        let other = proposal(
            supply(
                &db,
                "watchtower",
                harbor,
                "host-2",
                &laptop("pc-0099", None),
            )
            .await
            .unwrap(),
        );
        crate::customers::offboard(&db, harbor).await.unwrap();
        assert_eq!(
            refusal(discard(&db, other.id, carla).await),
            "the customer is offboarded"
        );
        assert_eq!(open_of(&db, harbor).await.unwrap().len(), 1);
    }

    /// The database keeps link proposals within the rules even when the code above is bypassed:
    /// at most one open proposal for a reference of an add-on, about an asset of the same customer,
    /// and a decided one says by whom.
    #[sqlx::test]
    async fn the_database_keeps_one_open_link_proposal_per_reference(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        testing::addon(&db, "watchtower").await;
        let carla = testing::agent(&db, "carla@northgate.test").await;
        let pc = by_hand(&db, brightfield, "PC-0042", None).await;
        let theirs = by_hand(&db, harbor, "PC-0099", None).await;
        let insert = |reference: &'static str,
                      customer: Uuid,
                      asset: Uuid,
                      state: &'static str,
                      decided_by: Option<Uuid>| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "INSERT INTO asset_link_proposals (id, addon, reference, customer_id, asset_id,
                                                       matched_by, name, state, decided_at,
                                                       decided_by)
                     VALUES ($1, 'watchtower', $2, $3, $4, 'name', 'PC-0042', $5,
                             CASE WHEN $5 = 'open' THEN NULL ELSE now() END, $6)",
                )
                .bind(Uuid::new_v4())
                .bind(reference)
                .bind(customer)
                .bind(asset)
                .bind(state)
                .bind(decided_by)
                .execute(&db)
                .await
            }
        };

        assert!(
            insert("host-1", brightfield, pc, "open", None)
                .await
                .is_ok()
        );
        assert!(
            insert("host-1", brightfield, pc, "open", None)
                .await
                .is_err(),
            "a second open proposal for the reference"
        );
        assert!(
            insert("host-1", brightfield, pc, "discarded", Some(carla))
                .await
                .is_ok(),
            "discarded ones stay beside it"
        );
        assert!(
            insert("host-2", brightfield, pc, "discarded", None)
                .await
                .is_err(),
            "discarded by nobody"
        );
        assert!(
            insert("host-3", brightfield, theirs, "open", None)
                .await
                .is_err(),
            "an asset of another customer"
        );
        assert!(
            insert(" host-4", brightfield, pc, "open", None)
                .await
                .is_err(),
            "an untrimmed reference"
        );
    }
}
