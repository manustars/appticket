//! Requesters and their e-mail addresses: a requester has one or more, each address belongs to
//! exactly one requester, and a reply goes to the address the requester last wrote from on the
//! ticket. Rules are in docs/domain-rules.md ("People").

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

/// Lowercases and checks an e-mail address such as `ada@brightfield.test`.
pub fn clean_address(address: &str) -> Result<String> {
    let address = address.trim().to_lowercase();
    match address.rsplit_once('@') {
        Some((local, domain))
            if !local.is_empty()
                && domain.contains('.')
                && !address.chars().any(char::is_whitespace) =>
        {
            Ok(address)
        }
        _ => rule(format!("{address:?} is not an e-mail address")),
    }
}

/// The requester an address belongs to.
#[derive(Debug, sqlx::FromRow)]
pub struct Owner {
    pub address: String,
    pub requester_id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub departed: bool,
}

/// The query of [`Owner`]s of the addresses in `$1`, in their order, followed by `$tail`.
macro_rules! owners_query {
    ($tail:literal) => {
        concat!(
            "SELECT a.address, r.id AS requester_id, r.customer_id, c.name AS customer_name,
                    r.departed_at IS NOT NULL AS departed
             FROM requester_addresses a
             JOIN requesters r ON r.id = a.requester_id
             JOIN customers c ON c.id = r.customer_id
             WHERE a.address = ANY($1)
             ORDER BY array_position($1, a.address)",
            $tail
        )
    };
}

/// Who the address belongs to, if anyone; the address and its requester are locked.
pub async fn owner_of(tx: &mut Transaction<'_, Postgres>, address: &str) -> Result<Option<Owner>> {
    Ok(sqlx::query_as(owners_query!(" FOR UPDATE OF a, r"))
        .bind([address])
        .fetch_optional(&mut **tx)
        .await?)
}

/// Who the addresses belong to, those that belong to someone, in their order. Nothing is locked,
/// so that a reader that ends up changing nothing waits for no one.
pub(crate) async fn owners<'e>(
    db: impl sqlx::PgExecutor<'e>,
    addresses: &[String],
) -> Result<Vec<Owner>> {
    Ok(sqlx::query_as(owners_query!(""))
        .bind(addresses)
        .fetch_all(db)
        .await?)
}

/// Where an address a requester is given comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// It belongs to nobody yet.
    Free,
    /// It belongs to a requester who departed from another customer: a person who changed
    /// customer keeps their address, which passes to the requester they are now.
    Departed,
}

/// Where `address` would come from as an address of a requester of `customer` (`requester` when it
/// is one already); refused when it belongs to someone it cannot be taken from.
async fn source_of(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    requester: Option<Uuid>,
    address: &str,
) -> Result<Source> {
    match owner_of(tx, address).await? {
        None => Ok(Source::Free),
        Some(owner) if Some(owner.requester_id) == requester => rule(format!(
            "the address {address} already belongs to this requester"
        )),
        Some(owner) if owner.departed && owner.customer_id != customer => Ok(Source::Departed),
        Some(owner) if owner.departed => rule(format!(
            "the address {address} belongs to a requester who has departed from {}: bring them back from their page instead",
            owner.customer_name
        )),
        Some(owner) => rule(format!(
            "the address {address} already belongs to a requester of {}, and an address belongs to one requester",
            owner.customer_name
        )),
    }
}

/// Gives `requester` the address from its [`source_of`], registered after the ones they have: one
/// passing from a departed requester leaves them and counts as newly registered.
async fn give_address(
    tx: &mut Transaction<'_, Postgres>,
    requester: Uuid,
    address: &str,
    source: Source,
) -> Result<()> {
    // Several given in one transaction keep the order they were given in.
    let given = match source {
        Source::Free => sqlx::query(
            "INSERT INTO requester_addresses (address, requester_id, created_at)
             SELECT $1, $2, greatest(now(), max(created_at) + interval '1 microsecond')
             FROM requester_addresses WHERE requester_id = $2",
        ),
        Source::Departed => sqlx::query(
            "UPDATE requester_addresses SET requester_id = $2,
                 created_at = (SELECT greatest(now(), max(created_at) + interval '1 microsecond')
                               FROM requester_addresses WHERE requester_id = $2)
             WHERE address = $1",
        ),
    }
    .bind(address)
    .bind(requester)
    .execute(&mut **tx)
    .await;
    match given {
        Err(e) if taken(&e) => rule(format!(
            "the address {address} already belongs to a requester"
        )),
        Err(e) => Err(e.into()),
        Ok(_) => Ok(()),
    }
}

/// Adds a requester of `customer`, without an address yet: the caller gives them one before the
/// transaction ends.
async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    name: Option<&str>,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO requesters (id, customer_id, name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(customer)
        .bind(filled(name))
        .execute(&mut **tx)
        .await?;
    Ok(id)
}

/// Adds a requester of `customer` with its first address, which must be clean and belong to
/// nobody yet, or to a requester who departed from another customer.
pub async fn create(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    address: &str,
    name: Option<&str>,
) -> Result<Uuid> {
    let source = source_of(tx, customer, None, address).await?;
    let id = insert(tx, customer, name).await?;
    give_address(tx, id, address, source).await?;
    Ok(id)
}

/// Adds a requester of `customer`, which the caller has locked, with `addresses`: clean, belonging
/// to nobody yet, and registered in their order. An import takes no address from anyone.
pub(crate) async fn create_with_new_addresses(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    addresses: &[String],
) -> Result<Uuid> {
    let id = insert(tx, customer, None).await?;
    for address in addresses {
        give_address(tx, id, address, Source::Free).await?;
    }
    Ok(id)
}

/// Gives `requester`, whom the caller has locked, a clean address that belongs to nobody yet,
/// registered after the ones they have.
pub(crate) async fn add_new_address_in(
    tx: &mut Transaction<'_, Postgres>,
    requester: Uuid,
    address: &str,
) -> Result<()> {
    give_address(tx, requester, address, Source::Free).await
}

/// A requester's details as they are stored, and whether they have departed; none for one that
/// no longer exists, merged into another.
pub(crate) async fn stored<'e>(
    db: impl sqlx::PgExecutor<'e>,
    id: Uuid,
) -> Result<Option<(RequesterDetails, bool)>> {
    type Row = (
        Option<String>,
        Vec<String>,
        Option<String>,
        String,
        Option<Uuid>,
        bool,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT name, phone_numbers, job_title, notes, site_id, departed_at IS NOT NULL
         FROM requesters WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(
        |(name, phone_numbers, job_title, notes, site_id, departed)| {
            let details = RequesterDetails {
                name,
                phone_numbers,
                job_title,
                notes,
                site_id,
            };
            (details, departed)
        },
    ))
}

/// The requester of `customer` the address belongs to, registered with `name` if new; an address
/// of another customer's requester is refused unless they departed, and a departed requester of
/// `customer` is refused too. A known requester without a name takes `name`.
pub async fn find_or_create(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    address: &str,
    name: Option<&str>,
) -> Result<Uuid> {
    match owner_of(tx, address).await? {
        Some(owner) if owner.customer_id == customer && !owner.departed => {
            sqlx::query("UPDATE requesters SET name = $2 WHERE id = $1 AND name IS NULL")
                .bind(owner.requester_id)
                .bind(filled(name))
                .execute(&mut **tx)
                .await?;
            Ok(owner.requester_id)
        }
        _ => create(tx, customer, address, name).await,
    }
}

/// The requester's addresses, the first registered first.
pub async fn addresses<'e>(db: impl sqlx::PgExecutor<'e>, requester: Uuid) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT address FROM requester_addresses WHERE requester_id = $1
         ORDER BY created_at, address",
    )
    .bind(requester)
    .fetch_all(db)
    .await?)
}

/// Gives the requester one more address. An address already belonging to a requester is refused,
/// unless that requester departed from another customer: then it passes to this one.
pub async fn add_address(db: &PgPool, requester: Uuid, address: &str) -> Result<()> {
    let address = clean_address(address)?;
    let mut tx = db.begin().await?;
    let customer = lock_with_customer(&mut tx, requester).await?;
    let source = source_of(&mut tx, customer, Some(requester), &address).await?;
    give_address(&mut tx, requester, &address, source).await?;
    tx.commit().await?;
    Ok(())
}

/// Takes an address from the requester, who keeps at least one. A key requester's contact address
/// stays until an agent picks another.
pub async fn remove_address(db: &PgPool, requester: Uuid, address: &str) -> Result<()> {
    let address = address.trim().to_lowercase();
    let mut tx = db.begin().await?;
    lock_with_customer(&mut tx, requester).await?;
    let addresses = addresses(&mut *tx, requester).await?;
    if !addresses.contains(&address) {
        return Err(Error::NotFound("address"));
    }
    if addresses.len() == 1 {
        return rule("a requester keeps at least one address");
    }
    let contact: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM key_requesters WHERE requester_id = $1 AND contact_address = $2)",
    )
    .bind(requester)
    .bind(&address)
    .fetch_one(&mut *tx)
    .await?;
    if contact {
        return rule(format!(
            "the address {address} is where the requester is written to as a key requester (their contact address): pick another on the customer's page first"
        ));
    }
    sqlx::query("DELETE FROM requester_addresses WHERE address = $1")
        .bind(&address)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// What agents who manage customers correct on a requester.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct RequesterDetails {
    /// Empty for a requester whose name is not known.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub phone_numbers: Vec<String>,
    #[serde(default)]
    pub job_title: Option<String>,
    /// Internal notes, seen only by agents.
    #[serde(default)]
    pub notes: String,
    /// One of the customer's sites that is not retired.
    #[serde(default)]
    pub site_id: Option<Uuid>,
}

/// The site a requester is at.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct SiteRef {
    pub id: Uuid,
    pub name: String,
}

/// A requester's customer, as their page shows it.
#[derive(Debug, Serialize, ToSchema)]
pub struct RequesterCustomer {
    pub id: Uuid,
    pub name: String,
    /// An offboarded customer's requesters do not change.
    pub offboarded: bool,
}

/// Everything a requester's page shows but their tickets.
#[derive(Debug, Serialize, ToSchema)]
pub struct RequesterDetail {
    pub id: Uuid,
    pub customer: RequesterCustomer,
    /// The requester's addresses, the first registered first.
    pub addresses: Vec<String>,
    pub name: Option<String>,
    pub phone_numbers: Vec<String>,
    pub job_title: Option<String>,
    /// Internal notes, seen only by agents.
    pub notes: String,
    pub site: Option<SiteRef>,
    /// When the requester left their customer; empty while they have not. A departed requester
    /// gets no tickets and their messages wait in the inbox.
    #[serde(with = "time::serde::rfc3339::option")]
    pub departed_at: Option<OffsetDateTime>,
    /// The assets they hold, by kind.
    pub held_assets: Vec<crate::catalog::Asset>,
}

/// A requester with their customer, details, site and the assets they hold.
pub async fn detail(db: &PgPool, id: Uuid) -> Result<RequesterDetail> {
    #[derive(sqlx::FromRow)]
    struct Row {
        customer_id: Uuid,
        customer_name: String,
        offboarded: bool,
        name: Option<String>,
        phone_numbers: Vec<String>,
        job_title: Option<String>,
        notes: String,
        site_id: Option<Uuid>,
        site_name: Option<String>,
        departed_at: Option<OffsetDateTime>,
    }
    let row: Row = sqlx::query_as(
        "SELECT r.customer_id, c.name AS customer_name, c.offboarded_at IS NOT NULL AS offboarded,
                r.name, r.phone_numbers, r.job_title, r.notes,
                r.site_id, s.name AS site_name, r.departed_at
         FROM requesters r
         JOIN customers c ON c.id = r.customer_id
         LEFT JOIN sites s ON s.id = r.site_id
         WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound("requester"))?;
    Ok(RequesterDetail {
        id,
        customer: RequesterCustomer {
            id: row.customer_id,
            name: row.customer_name,
            offboarded: row.offboarded,
        },
        addresses: addresses(db, id).await?,
        name: row.name,
        phone_numbers: row.phone_numbers,
        job_title: row.job_title,
        notes: row.notes,
        site: row
            .site_id
            .zip(row.site_name)
            .map(|(id, name)| SiteRef { id, name }),
        departed_at: row.departed_at,
        held_assets: crate::catalog::held_by(db, id).await?,
    })
}

/// Marks a requester departed: they left their customer. Their history stays, they stop being a
/// key requester, and the assets they hold lose their holder.
pub async fn depart(db: &PgPool, id: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    lock_with_customer(&mut tx, id).await?;
    crate::key_requesters::unmark_in(&mut tx, id).await?;
    crate::catalog::pass_held(&mut tx, id, None).await?;
    let left = sqlx::query(
        "UPDATE requesters SET departed_at = now() WHERE id = $1 AND departed_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if left.rows_affected() == 0 {
        return rule("the requester has already departed");
    }
    tx.commit().await?;
    Ok(())
}

/// Brings a departed requester back to their customer. They need an address of their own again,
/// since the ones they kept may have passed to the requester they became elsewhere.
pub async fn bring_back(db: &PgPool, id: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    lock_with_customer(&mut tx, id).await?;
    let departed: bool =
        sqlx::query_scalar("SELECT departed_at IS NOT NULL FROM requesters WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if !departed {
        return rule("the requester has not departed");
    }
    if addresses(&mut *tx, id).await?.is_empty() {
        return rule(
            "the requester has no e-mail address left: give them one before they come back",
        );
    }
    sqlx::query("UPDATE requesters SET departed_at = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Merges `merged` into `kept`: two requesters of the same customer who are one person. The
/// addresses, tickets and held assets of `merged` pass to `kept`, which remains, and `merged` is
/// removed; the details `kept` lacks are taken from `merged`, so that nothing about the person is
/// lost, and `kept` is a key requester when either was, written to where `merged` was if not
/// already. A merge cannot be undone. Requesters of different customers are never merged, nor those of an
/// offboarded customer. `merged` may have departed; `kept` may not, since tickets are not given to
/// a departed requester.
pub async fn merge(db: &PgPool, kept: Uuid, merged: Uuid) -> Result<()> {
    if kept == merged {
        return rule("a requester cannot be merged with themselves");
    }
    let mut tx = db.begin().await?;
    let customer = lock_with_customer(&mut tx, kept).await?;
    let kept_departed: bool =
        sqlx::query_scalar("SELECT departed_at IS NOT NULL FROM requesters WHERE id = $1")
            .bind(kept)
            .fetch_one(&mut *tx)
            .await?;
    if kept_departed {
        return rule(
            "the requester that remains cannot be departed: bring them back first, or merge them into the other one",
        );
    }
    // One of another customer is refused before it is locked; one of this customer stays where it
    // is under the customer's lock, unless it moved just before.
    let other: Uuid = sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1")
        .bind(merged)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound("requester"))?;
    if other != customer {
        return rule("requesters of different customers are never merged");
    }
    let other: Option<Uuid> =
        sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1 FOR UPDATE")
            .bind(merged)
            .fetch_optional(&mut *tx)
            .await?;
    if other != Some(customer) {
        return rule("the requester has just moved to another customer; reload and try again");
    }
    sqlx::query("INSERT INTO requester_merges (merged_id, kept_id) VALUES ($1, $2)")
        .bind(merged)
        .bind(kept)
        .execute(&mut *tx)
        .await?;
    // Earlier merges into `merged` now lead to `kept`.
    sqlx::query("UPDATE requester_merges SET kept_id = $2 WHERE kept_id = $1")
        .bind(merged)
        .bind(kept)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE requesters k SET
             name = coalesce(k.name, m.name),
             job_title = coalesce(k.job_title, m.job_title),
             site_id = coalesce(k.site_id, m.site_id),
             phone_numbers = k.phone_numbers
                 || ARRAY(SELECT p FROM unnest(m.phone_numbers) WITH ORDINALITY AS u(p, i)
                          WHERE p <> ALL (k.phone_numbers) ORDER BY i),
             notes = CASE WHEN m.notes = '' THEN k.notes WHEN k.notes = '' THEN m.notes
                          ELSE k.notes || E'\\n\\n' || m.notes END
         FROM requesters m
         WHERE k.id = $1 AND m.id = $2",
    )
    .bind(kept)
    .bind(merged)
    .execute(&mut *tx)
    .await?;
    // A key requester merged away leaves `kept` one, written to at the same address, which passes
    // to `kept` below; a key requester that remains keeps their own contact address.
    let contact = crate::key_requesters::unmark_in(&mut tx, merged).await?;
    // Passed addresses count as newly registered, in their order, so the first address of `kept`
    // stays first.
    sqlx::query(
        "UPDATE requester_addresses SET requester_id = $1,
             created_at = now() + (created_at - (SELECT min(created_at) FROM requester_addresses
                                                 WHERE requester_id = $2))
         WHERE requester_id = $2",
    )
    .bind(kept)
    .bind(merged)
    .execute(&mut *tx)
    .await?;
    if let Some(contact) = contact {
        sqlx::query(
            "INSERT INTO key_requesters (requester_id, customer_id, contact_address)
             VALUES ($1, $2, $3) ON CONFLICT (requester_id) DO NOTHING",
        )
        .bind(kept)
        .bind(customer)
        .bind(contact)
        .execute(&mut *tx)
        .await?;
    }
    crate::catalog::pass_held(&mut tx, merged, Some(kept)).await?;
    sqlx::query("UPDATE tickets SET requester_id = $1, updated_at = now() WHERE requester_id = $2")
        .bind(kept)
        .bind(merged)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM requesters WHERE id = $1")
        .bind(merged)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Moves a requester identified with the wrong customer to `customer`, from their own page: under
/// the rule for moving tickets, all their tickets move with them, closed ones included, and the
/// links that do not fit the new customer are removed. They leave their site, and the assets they
/// hold lose their holder. An offboarded customer's requesters do not move.
pub async fn move_to_customer(
    db: &PgPool,
    id: Uuid,
    customer: Uuid,
) -> Result<crate::tickets::Moved> {
    let mut tx = db.begin().await?;
    lock_for_move(&mut tx, id, customer).await?;
    let moved = move_requester(&mut tx, id, customer).await?;
    tx.commit().await?;
    Ok(moved)
}

/// Locks what moving `requester` to `customer` changes, in the order every change to requesters
/// and tickets follows (customers, then requesters, then key requesters, then tickets): both
/// customers, by id, which must exist and not be offboarded, then the requester, then their row as
/// a key requester, if any, then the assets they hold, before any ticket. The assets are locked
/// only against changes (`FOR NO KEY UPDATE`): linking an asset to a ticket holds the ticket and
/// then refers to the asset, which such a lock lets it do. Returns the requester's customer.
pub(crate) async fn lock_for_move(
    tx: &mut Transaction<'_, Postgres>,
    requester: Uuid,
    customer: Uuid,
) -> Result<Uuid> {
    let current: Uuid = sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1")
        .bind(requester)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound("requester"))?;
    if current == customer {
        return rule("the requester already belongs to this customer");
    }
    for c in [current.min(customer), current.max(customer)] {
        crate::customers::lock_active(tx, c).await?;
    }
    let now: Option<Uuid> =
        sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1 FOR UPDATE")
            .bind(requester)
            .fetch_optional(&mut **tx)
            .await?;
    if now != Some(current) {
        return rule("the requester has just changed; reload and try again");
    }
    // A key requester stops being one when moved; their row is taken now, before any ticket.
    sqlx::query("SELECT 1 FROM key_requesters WHERE requester_id = $1 FOR UPDATE")
        .bind(requester)
        .execute(&mut **tx)
        .await?;
    // The assets they hold lose their holder, which is no key: they are taken now too, before any
    // ticket, still letting tickets refer to them.
    sqlx::query("SELECT 1 FROM assets WHERE holder_id = $1 FOR NO KEY UPDATE")
        .bind(requester)
        .execute(&mut **tx)
        .await?;
    Ok(current)
}

/// Moves a requester identified with the wrong customer to `customer`, with all of their tickets,
/// closed ones included; links that do not fit the new customer are removed. Their site is one of
/// the old customer's, so they leave it, they stop being a key requester, and the assets they hold
/// stay with the old customer without a holder. Called with [`lock_for_move`] held. A parent
/// ticket stays with the help desk's own organization, so a requester of one does not move.
pub(crate) async fn move_requester(
    tx: &mut Transaction<'_, Postgres>,
    requester: Uuid,
    customer: Uuid,
) -> Result<crate::tickets::Moved> {
    let tickets: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM tickets WHERE requester_id = $1")
        .bind(requester)
        .fetch_all(&mut **tx)
        .await?;
    let parent: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tickets WHERE parent_id = ANY($1))")
            .bind(&tickets)
            .fetch_one(&mut **tx)
            .await?;
    if parent {
        return rule("a parent ticket stays with the help desk's own organization");
    }
    let unlinked = crate::catalog::unlink_for_move(tx, &tickets, customer).await?;
    crate::key_requesters::unmark_in(tx, requester).await?;
    crate::catalog::pass_held(tx, requester, None).await?;
    // The foreign key cascades the new customer to every ticket of the requester.
    sqlx::query("UPDATE requesters SET customer_id = $2, site_id = NULL WHERE id = $1")
        .bind(requester)
        .bind(customer)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE tickets SET updated_at = now() WHERE requester_id = $1")
        .bind(requester)
        .execute(&mut **tx)
        .await?;
    Ok(crate::tickets::Moved {
        moved: tickets.len() as i64,
        unlinked,
    })
}

/// What moving a requester to another customer would do, for the agent to see before confirming.
#[derive(Serialize, ToSchema)]
pub struct MovePreview {
    /// How many tickets would move with the requester, closed ones included.
    pub tickets: i64,
    /// The site the requester is at, which they would leave: it is one of the old customer's.
    pub site: Option<SiteRef>,
    /// The requester is a key requester of the old customer, which they would stop being.
    pub key_requester: bool,
    /// The assets linked to their tickets: they belong to the old customer, so they are unlinked.
    pub assets: Vec<crate::catalog::LinkedAsset>,
    /// The services linked to their tickets that the new customer does not use: unlinked too.
    pub services: Vec<crate::catalog::LinkedService>,
    /// The assets they hold: these stay with the old customer, without a holder.
    pub held_assets: Vec<crate::catalog::Asset>,
}

/// What [`move_to_customer`] would move, leave and unlink, across all the requester's tickets.
pub async fn move_preview(db: &PgPool, id: Uuid, customer: Uuid) -> Result<MovePreview> {
    let mut conn = db.acquire().await?;
    let site: Option<Option<SiteRef>> = sqlx::query_as::<_, (Option<Uuid>, Option<String>)>(
        "SELECT s.id, s.name FROM requesters r LEFT JOIN sites s ON s.id = r.site_id
         WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .map(|(id, name)| id.zip(name).map(|(id, name)| SiteRef { id, name }));
    let site = site.ok_or(Error::NotFound("requester"))?;
    let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customers WHERE id = $1)")
        .bind(customer)
        .fetch_one(&mut *conn)
        .await?;
    if !known {
        return Err(Error::NotFound("customer"));
    }
    let tickets: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM tickets WHERE requester_id = $1")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    let (assets, services) = crate::catalog::unfit_for_move(&mut conn, &tickets, customer).await?;
    let key_requester: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM key_requesters WHERE requester_id = $1)")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    let held_assets = crate::catalog::held_by(&mut *conn, id).await?;
    Ok(MovePreview {
        tickets: tickets.len() as i64,
        site,
        key_requester,
        assets,
        services,
        held_assets,
    })
}

/// Trimmed, or `None` when blank.
fn filled(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|t| !t.is_empty())
}

/// Corrects a requester's details. Their site must be one of their customer's that is not
/// retired.
pub async fn update(db: &PgPool, id: Uuid, details: &RequesterDetails) -> Result<()> {
    let mut tx = db.begin().await?;
    update_in(&mut tx, id, details).await?;
    tx.commit().await?;
    Ok(())
}

/// [`update`], inside the caller's transaction.
pub(crate) async fn update_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    details: &RequesterDetails,
) -> Result<()> {
    let mut phone_numbers = Vec::new();
    for number in &details.phone_numbers {
        match filled(Some(number)) {
            Some(number) => phone_numbers.push(number),
            None => return rule("a phone number cannot be blank"),
        }
    }
    let customer = lock_with_customer(tx, id).await?;
    if let Some(site) = details.site_id {
        let retired: Option<bool> = sqlx::query_scalar(
            "SELECT retired_at IS NOT NULL FROM sites WHERE id = $1 AND customer_id = $2",
        )
        .bind(site)
        .bind(customer)
        .fetch_optional(&mut **tx)
        .await?;
        match retired {
            None => return rule("the site is not one of the requester's customer"),
            Some(true) => return rule("the site is retired"),
            Some(false) => {}
        }
    }
    sqlx::query(
        "UPDATE requesters SET name = $2, phone_numbers = $3, job_title = $4, notes = $5,
                site_id = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(filled(details.name.as_deref()))
    .bind(&phone_numbers)
    .bind(filled(details.job_title.as_deref()))
    .bind(details.notes.trim())
    .bind(details.site_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Locks the requester's customer, which must not be offboarded, then the requester, for any
/// change to the requester; retiring a site locks the customer first too. Returns the customer.
pub(crate) async fn lock_with_customer(
    tx: &mut Transaction<'_, Postgres>,
    requester: Uuid,
) -> Result<Uuid> {
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1")
        .bind(requester)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound("requester"))?;
    crate::customers::lock_active(tx, customer).await?;
    // A requester can be moved to another customer in between: check under the lock.
    // It can also have been merged into another requester, and be gone.
    let now: Uuid =
        sqlx::query_scalar("SELECT customer_id FROM requesters WHERE id = $1 FOR UPDATE")
            .bind(requester)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(Error::NotFound("requester"))?;
    if now != customer {
        return rule("the requester has just moved to another customer; reload and try again");
    }
    Ok(customer)
}

/// Where a reply on a ticket can go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    /// The address the requester last wrote from on the ticket, while it is still theirs.
    pub wrote_from: Option<String>,
    /// The address an agent picked for the last reply on the ticket, while it is still theirs.
    pub last_picked: Option<String>,
    /// All the requester's addresses, the first registered first.
    pub addresses: Vec<String>,
}

impl Recipient {
    /// The address a reply goes to. The requester's last address on the ticket wins; on a ticket
    /// they have not written on, `to` is the one the agent picks, needed when they have several,
    /// unless an agent already picked one for an earlier reply there.
    pub fn choose(&self, to: Option<&str>) -> Result<String> {
        let to = to
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty());
        if let Some(address) = &self.wrote_from {
            return match to {
                Some(to) if &to != address => rule(format!(
                    "the requester last wrote from {address} on this ticket, so the reply goes there"
                )),
                _ => Ok(address.clone()),
            };
        }
        match (to, self.addresses.as_slice()) {
            (Some(to), addresses) if addresses.contains(&to) => Ok(to),
            (Some(to), _) => rule(format!(
                "the address {to} does not belong to the ticket's requester"
            )),
            (None, []) => rule("the requester has no e-mail address left to reply to"),
            (None, [only]) => Ok(only.clone()),
            (None, _) => match &self.last_picked {
                Some(picked) => Ok(picked.clone()),
                None => rule(
                    "the requester has several addresses and has not written on this ticket: pick the one to reply to",
                ),
            },
        }
    }
}

/// The ticket requester's addresses, the one they last wrote from on it and the one the last
/// reply went to; `None` for a ticket without a requester.
pub async fn recipient(conn: &mut sqlx::PgConnection, ticket: Uuid) -> Result<Option<Recipient>> {
    type Row = (Option<Uuid>, Option<String>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT requester_id, ticket_reply_address(id), ticket_picked_address(id)
         FROM tickets WHERE id = $1",
    )
    .bind(ticket)
    .fetch_optional(&mut *conn)
    .await?;
    let (requester, wrote_from, last_picked) = row.ok_or(Error::NotFound("ticket"))?;
    let Some(requester) = requester else {
        return Ok(None);
    };
    Ok(Some(Recipient {
        wrote_from,
        last_picked,
        addresses: addresses(&mut *conn, requester).await?,
    }))
}

/// Whether a database error is a unique violation: someone else registered the address meanwhile.
fn taken(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(e) if e.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn recipient(wrote_from: Option<&str>, addresses: &[&str]) -> Recipient {
        Recipient {
            wrote_from: wrote_from.map(str::to_owned),
            last_picked: None,
            addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn a_reply_goes_where_the_requester_last_wrote_from() {
        let r = recipient(
            Some("ada@home.test"),
            &["ada@brightfield.test", "ada@home.test"],
        );
        assert_eq!(r.choose(None).unwrap(), "ada@home.test");
        assert_eq!(r.choose(Some("ADA@home.test")).unwrap(), "ada@home.test");
        assert!(matches!(
            r.choose(Some("ada@brightfield.test")),
            Err(Error::Rule(_))
        ));
    }

    #[test]
    fn on_a_ticket_they_have_not_written_on_the_agent_picks() {
        let several = recipient(None, &["ada@brightfield.test", "ada@home.test"]);
        assert!(matches!(several.choose(None), Err(Error::Rule(_))));
        assert_eq!(
            several.choose(Some("ada@home.test")).unwrap(),
            "ada@home.test"
        );
        assert!(matches!(
            several.choose(Some("ivo@harbor.test")),
            Err(Error::Rule(_))
        ));
        let one = recipient(None, &["ada@brightfield.test"]);
        assert_eq!(one.choose(None).unwrap(), "ada@brightfield.test");

        // An earlier pick holds for the next reply, and the agent can still pick another.
        let picked = Recipient {
            last_picked: Some("ada@home.test".into()),
            ..several
        };
        assert_eq!(picked.choose(None).unwrap(), "ada@home.test");
        assert_eq!(
            picked.choose(Some("ada@brightfield.test")).unwrap(),
            "ada@brightfield.test"
        );
    }

    #[test]
    fn a_requester_without_addresses_gets_no_reply() {
        let Err(Error::Rule(why)) = recipient(None, &[]).choose(None) else {
            panic!("a reply without an address");
        };
        assert!(why.contains("no e-mail address"), "{why}");
    }

    #[test]
    fn addresses_are_cleaned_and_checked() {
        assert_eq!(
            clean_address(" Ada@Brightfield.TEST ").unwrap(),
            "ada@brightfield.test"
        );
        for bad in [
            "",
            "ada",
            "@brightfield.test",
            "ada@localhost",
            "a da@b.test",
        ] {
            assert!(clean_address(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[sqlx::test]
    async fn an_address_belongs_to_one_requester(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;

        add_address(&db, ada, " Ada@Home.test").await.unwrap();
        assert_eq!(
            addresses(&db, ada).await.unwrap(),
            ["ada@brightfield.test", "ada@home.test"]
        );
        let Err(Error::Rule(why)) = add_address(&db, ivo, "ada@home.test").await else {
            panic!("a second owner was accepted");
        };
        assert!(why.contains("Brightfield"), "{why}");
        assert!(matches!(
            add_address(&db, ada, "ada@home.test").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            add_address(&db, Uuid::new_v4(), "x@home.test").await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn a_requester_keeps_at_least_one_address(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        add_address(&db, ada, "ada@home.test").await.unwrap();

        remove_address(&db, ada, "ADA@brightfield.test")
            .await
            .unwrap();
        assert_eq!(addresses(&db, ada).await.unwrap(), ["ada@home.test"]);
        assert!(matches!(
            remove_address(&db, ada, "ada@home.test").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            remove_address(&db, ada, "ada@brightfield.test").await,
            Err(Error::NotFound(_))
        ));
    }

    /// An address taken from the requester no longer receives their replies.
    #[sqlx::test]
    async fn a_removed_address_gets_no_more_replies(db: PgPool) {
        use crate::tickets::{self, TicketState};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        add_address(&db, ada, "ada@home.test").await.unwrap();
        add_address(&db, ada, "ada@studio.test").await.unwrap();
        let agent = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "h@home.test").await;
        testing::message_from(&db, n, "ada@home.test").await;
        tickets::reply(
            &db,
            &testing::sender(),
            n,
            agent,
            "Hi.",
            None,
            TicketState::Waiting,
        )
        .await
        .unwrap();

        remove_address(&db, ada, "ada@home.test").await.unwrap();
        let requester = tickets::detail(&db, n).await.unwrap().requester.unwrap();
        assert_eq!(requester.reply_to, None);
        assert_eq!(
            requester.last_picked, None,
            "the last reply went to a removed address"
        );
        assert_eq!(requester.email, "ada@brightfield.test");
        assert!(matches!(
            tickets::reply(
                &db,
                &testing::sender(),
                n,
                agent,
                "Hi.",
                None,
                TicketState::Waiting
            )
            .await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn a_requester_s_details_are_corrected(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;

        let details = RequesterDetails {
            name: Some(" Ada Ferrini ".into()),
            phone_numbers: vec![" 000 000 0101 ".into(), "000 000 0102".into()],
            job_title: Some("Office manager".into()),
            notes: "Prefers phone calls in the morning.".into(),
            site_id: None,
        };
        update(&db, ada, &details).await.unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!(seen.name.as_deref(), Some("Ada Ferrini"));
        assert_eq!(seen.phone_numbers, ["000 000 0101", "000 000 0102"]);
        assert_eq!(seen.job_title.as_deref(), Some("Office manager"));
        assert_eq!(seen.notes, "Prefers phone calls in the morning.");
        assert_eq!(seen.addresses, ["ada@brightfield.test"]);
        assert_eq!(seen.customer.name, "Brightfield");
        assert!(seen.site.is_none());

        // Emptied details are forgotten; a blank phone number is refused.
        update(
            &db,
            ada,
            &RequesterDetails {
                name: Some("  ".into()),
                job_title: Some("".into()),
                ..RequesterDetails::default()
            },
        )
        .await
        .unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!((seen.name, seen.job_title), (None, None));
        assert!(seen.phone_numbers.is_empty());
        assert!(matches!(
            update(
                &db,
                ada,
                &RequesterDetails {
                    phone_numbers: vec!["  ".into()],
                    ..RequesterDetails::default()
                }
            )
            .await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            detail(&db, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));
    }

    #[sqlx::test]
    async fn a_requester_is_at_an_active_site_of_their_customer(db: PgPool) {
        let brightfield = crate::customers::create(&db, "Brightfield", &[])
            .await
            .unwrap();
        let harbor = crate::customers::create(&db, "Harbor Lane", &[])
            .await
            .unwrap();
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Old office", "Via del Glicine 3")
            .await
            .unwrap();
        let quay = crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let at = |site| RequesterDetails {
            site_id: Some(site),
            ..RequesterDetails::default()
        };

        update(&db, ada, &at(head)).await.unwrap();
        let site = detail(&db, ada).await.unwrap().site.unwrap();
        assert_eq!((site.id, site.name.as_str()), (head, "Head office"));
        assert!(
            matches!(update(&db, ada, &at(quay)).await, Err(Error::Rule(_))),
            "another customer's"
        );
        assert!(
            matches!(update(&db, ada, &at(old)).await, Err(Error::Rule(_))),
            "retired"
        );
        update(&db, ada, &RequesterDetails::default())
            .await
            .unwrap();
        assert!(detail(&db, ada).await.unwrap().site.is_none());

        crate::customers::offboard(&db, brightfield).await.unwrap();
        assert!(
            matches!(update(&db, ada, &at(head)).await, Err(Error::Rule(_))),
            "offboarded"
        );
    }

    /// The names of the assets the requester's page lists as theirs.
    async fn held(db: &PgPool, requester: Uuid) -> Vec<String> {
        detail(db, requester)
            .await
            .unwrap()
            .held_assets
            .into_iter()
            .map(|a| a.name)
            .collect()
    }

    #[sqlx::test]
    async fn a_requester_s_page_lists_the_assets_they_hold(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        testing::held_asset(&db, brightfield, "Phone", "Phone 12", ada).await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0042", ada).await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0043", lena).await;
        testing::asset(&db, brightfield, "Laptop", "PC-0044").await;

        assert_eq!(held(&db, ada).await, ["PC-0042", "Phone 12"], "by kind");
        assert_eq!(held(&db, lena).await, ["PC-0043"]);
    }

    #[sqlx::test]
    async fn a_departed_requester_s_assets_lose_their_holder(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0042", ada).await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0043", lena).await;

        depart(&db, ada).await.unwrap();
        assert!(held(&db, ada).await.is_empty());
        let assets = crate::catalog::assets_of(&db, brightfield).await.unwrap();
        assert_eq!(assets.len(), 2, "the assets stay with the customer");
        assert_eq!(held(&db, lena).await, ["PC-0043"], "others keep theirs");
        bring_back(&db, ada).await.unwrap();
        assert!(
            held(&db, ada).await.is_empty(),
            "coming back gives none back"
        );
    }

    #[sqlx::test]
    async fn merging_keeps_the_held_assets_with_the_one_who_remains(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0042", ada).await;
        testing::held_asset(&db, brightfield, "Phone", "Phone 12", twin).await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0043", twin).await;

        merge(&db, ada, twin).await.unwrap();
        assert_eq!(held(&db, ada).await, ["PC-0042", "PC-0043", "Phone 12"]);
    }

    /// Moving a holder takes the assets they hold before their tickets, while linking an asset to a
    /// ticket holds the ticket and then refers to the asset. Moving takes the assets without
    /// keeping them from being referred to, so the two never wait for each other: the link is
    /// made, then the move unlinks it.
    #[sqlx::test]
    async fn moving_a_holder_and_linking_their_asset_do_not_wait_for_each_other(db: PgPool) {
        use crate::tickets::{self, RequesterMove};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let pc = testing::held_asset(&db, brightfield, "Laptop", "PC-0042", ada).await;
        let hers = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;

        // As linking an asset does: the ticket first, then the link that refers to the asset.
        let mut linking = db.begin().await.unwrap();
        let ticket: Uuid =
            sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1 FOR UPDATE")
                .bind(hers)
                .fetch_one(&mut *linking)
                .await
                .unwrap();
        let moving = tokio::spawn({
            let db = db.clone();
            async move { tickets::move_to_customer(&db, hers, harbor, RequesterMove::Move).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(!moving.is_finished(), "moved a ticket being linked");
        sqlx::query("SET LOCAL lock_timeout = '3s'")
            .execute(&mut *linking)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO ticket_assets (ticket_id, customer_id, asset_id) VALUES ($1, $2, $3)",
        )
        .bind(ticket)
        .bind(brightfield)
        .bind(pc)
        .execute(&mut *linking)
        .await
        .expect("the asset was locked by the move, which waits for the ticket");
        linking.commit().await.unwrap();
        let moved = moving.await.unwrap().unwrap();
        assert_eq!(
            (moved.moved, moved.unlinked),
            (1, 1),
            "the new link removed"
        );
        assert!(held(&db, ada).await.is_empty());
    }

    /// The assets a moved requester holds stay with their customer and lose their holder, whether
    /// the requester is moved from their page or with one of their tickets; the agent is told so
    /// first.
    #[sqlx::test]
    async fn a_moved_requester_s_assets_stay_without_a_holder(db: PgPool) {
        use crate::tickets::{self, RequesterMove};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        testing::held_asset(&db, brightfield, "Phone", "Phone 12", ada).await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0042", ada).await;
        testing::held_asset(&db, brightfield, "Laptop", "PC-0043", lena).await;
        let n = testing::ticket(&db, brightfield, Some(lena), "l1@brightfield.test").await;

        let preview = move_preview(&db, ada, harbor).await.unwrap();
        let names: Vec<String> = preview.held_assets.into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["PC-0042", "Phone 12"]);
        move_to_customer(&db, ada, harbor).await.unwrap();
        assert!(held(&db, ada).await.is_empty());

        tickets::move_to_customer(&db, n, harbor, RequesterMove::Move)
            .await
            .unwrap();
        assert!(held(&db, lena).await.is_empty());
        let assets = crate::catalog::assets_of(&db, brightfield).await.unwrap();
        assert_eq!(assets.len(), 3, "the assets stay with their customer");
        assert!(assets.iter().all(|a| a.holder.is_none()));
    }

    #[sqlx::test]
    async fn retiring_a_site_leaves_its_requesters_without_one(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        update(
            &db,
            ada,
            &RequesterDetails {
                site_id: Some(head),
                ..RequesterDetails::default()
            },
        )
        .await
        .unwrap();

        crate::sites::retire(&db, head).await.unwrap();
        assert!(detail(&db, ada).await.unwrap().site.is_none());
    }

    /// A departed requester identified with the wrong customer is still moved, with their tickets.
    #[sqlx::test]
    async fn a_departed_requester_can_be_moved(db: PgPool) {
        use crate::tickets::{self, RequesterMove};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        testing::ticket(&db, brightfield, Some(ada), "b@brightfield.test").await;
        depart(&db, ada).await.unwrap();

        let moved = tickets::move_to_customer(&db, n, harbor, RequesterMove::Move).await;
        assert!(moved.is_ok(), "{moved:?}");
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!(seen.customer.id, harbor);
        assert!(seen.departed_at.is_some());
    }

    #[sqlx::test]
    async fn moving_a_requester_leaves_their_site(db: PgPool) {
        use crate::tickets::{self, RequesterMove};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        update(
            &db,
            ada,
            &RequesterDetails {
                site_id: Some(head),
                ..RequesterDetails::default()
            },
        )
        .await
        .unwrap();
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;

        tickets::move_to_customer(&db, n, harbor, RequesterMove::Move)
            .await
            .unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!(seen.customer.id, harbor);
        assert!(seen.site.is_none(), "a site of the old customer");
    }

    #[sqlx::test]
    async fn a_departed_requester_can_come_back(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;

        depart(&db, ada).await.unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert!(seen.departed_at.is_some());
        assert_eq!(
            seen.addresses,
            ["ada@brightfield.test"],
            "their history stays"
        );
        assert_eq!(
            crate::tickets::detail(&db, n)
                .await
                .unwrap()
                .requester
                .map(|r| (r.id, r.departed)),
            Some((ada, true)),
            "their tickets stay theirs"
        );
        assert!(
            matches!(depart(&db, ada).await, Err(Error::Rule(_))),
            "departed twice"
        );

        bring_back(&db, ada).await.unwrap();
        assert!(detail(&db, ada).await.unwrap().departed_at.is_none());
        assert!(
            matches!(bring_back(&db, ada).await, Err(Error::Rule(_))),
            "back without having left"
        );
        assert!(matches!(
            depart(&db, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));

        crate::customers::offboard(&db, brightfield).await.unwrap();
        assert!(
            matches!(depart(&db, ada).await, Err(Error::Rule(_))),
            "an offboarded customer's requesters do not change"
        );
    }

    /// A person who leaves one customer for another is departed at the first and a new requester
    /// of the second; an address they keep passes to the new requester.
    #[sqlx::test]
    async fn an_address_passes_from_a_departed_requester_to_a_new_one(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        add_address(&db, ada, "ada@home.test").await.unwrap();
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;

        let mut tx = db.begin().await.unwrap();
        let Err(Error::Rule(why)) = find_or_create(&mut tx, harbor, "ada@home.test", None).await
        else {
            panic!("an address taken from a requester who has not departed");
        };
        assert!(why.contains("Brightfield"), "{why}");
        drop(tx);

        depart(&db, ada).await.unwrap();
        let mut tx = db.begin().await.unwrap();
        let Err(Error::Rule(why)) =
            find_or_create(&mut tx, brightfield, "ada@home.test", None).await
        else {
            panic!("a departed requester identified at their own customer");
        };
        assert!(why.contains("departed"), "{why}");
        drop(tx);

        let mut tx = db.begin().await.unwrap();
        let ada_at_harbor = find_or_create(&mut tx, harbor, "ada@home.test", Some("Ada Ferrini"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_ne!(ada_at_harbor, ada);
        assert_eq!(
            addresses(&db, ada_at_harbor).await.unwrap(),
            ["ada@home.test"]
        );
        assert_eq!(addresses(&db, ada).await.unwrap(), ["ada@brightfield.test"]);

        // Her last address passes too, from the customer's page; her history stays.
        let ada_again =
            crate::customers::add_requester(&db, harbor, "ada@brightfield.test", None).await;
        assert!(ada_again.is_ok(), "{ada_again:?}");
        let seen = detail(&db, ada).await.unwrap();
        assert!(seen.addresses.is_empty());
        assert!(seen.departed_at.is_some());
        let ticket = crate::tickets::detail(&db, n).await.unwrap();
        assert_eq!(ticket.requester.map(|r| r.id), Some(ada));

        // Without an address of her own she cannot come back, until she gets one.
        assert!(matches!(bring_back(&db, ada).await, Err(Error::Rule(_))));
        add_address(&db, ada, "a.ferrini@brightfield.test")
            .await
            .unwrap();
        bring_back(&db, ada).await.unwrap();
    }

    /// An existing requester of another customer can be given a departed requester's address too;
    /// one of the same customer cannot, since that is the person who departed.
    #[sqlx::test]
    async fn a_departed_requester_s_address_passes_to_another_customer_s_requester(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        add_address(&db, ada, "ada@home.test").await.unwrap();
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let ada_at_harbor = testing::requester(&db, harbor, "ada@harbor.test").await;
        depart(&db, ada).await.unwrap();

        let Err(Error::Rule(why)) = add_address(&db, lena, "ada@home.test").await else {
            panic!("a departed requester's address given within their customer");
        };
        assert!(why.contains("departed"), "{why}");
        add_address(&db, ada_at_harbor, "ada@home.test")
            .await
            .unwrap();
        assert_eq!(
            addresses(&db, ada_at_harbor).await.unwrap(),
            ["ada@harbor.test", "ada@home.test"]
        );
        assert_eq!(addresses(&db, ada).await.unwrap(), ["ada@brightfield.test"]);
    }

    /// The database lets an address leave its requester only when they departed, for a requester of
    /// another customer.
    #[sqlx::test]
    async fn the_database_lets_only_a_departed_requester_s_address_pass(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        add_address(&db, ada, "ada@home.test").await.unwrap();
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        let pass = |to: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "UPDATE requester_addresses SET requester_id = $1 WHERE address = 'ada@home.test'",
                )
                .bind(to)
                .execute(&db)
                .await
            }
        };
        assert!(pass(ivo).await.is_err(), "taken from a present requester");
        depart(&db, ada).await.unwrap();
        assert!(pass(lena).await.is_err(), "given within their own customer");
        assert!(pass(ivo).await.is_ok());
    }

    /// Two requesters of the same customer who are one person are merged: the addresses and
    /// tickets of one pass to the other, which remains, and each ticket still replies where the
    /// requester last wrote from on it.
    #[sqlx::test]
    async fn merging_leaves_one_requester_with_the_addresses_and_tickets_of_both(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        add_address(&db, twin, "ada.f@studio.test").await.unwrap();
        let hers = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;
        let twins = testing::ticket(&db, brightfield, Some(twin), "t1@home.test").await;
        testing::message_from(&db, twins, "ada.f@studio.test").await;

        merge(&db, ada, twin).await.unwrap();
        assert_eq!(
            detail(&db, ada).await.unwrap().addresses,
            [
                "ada@brightfield.test",
                "a.ferrini@home.test",
                "ada.f@studio.test"
            ]
        );
        assert!(
            matches!(detail(&db, twin).await, Err(Error::NotFound(_))),
            "the merged requester is gone"
        );
        for n in [hers, twins] {
            let requester = crate::tickets::detail(&db, n).await.unwrap().requester;
            assert_eq!(requester.map(|r| (r.id, r.ticket_count)), Some((ada, 2)));
        }
        let requester = crate::tickets::detail(&db, twins)
            .await
            .unwrap()
            .requester
            .unwrap();
        assert_eq!(requester.reply_to.as_deref(), Some("ada.f@studio.test"));
    }

    /// A requester identified with the wrong customer is moved from their page: all their tickets
    /// move with them, closed ones included, links that do not fit the new customer are removed,
    /// and they leave their site.
    #[sqlx::test]
    async fn moving_a_requester_from_their_page_takes_all_their_tickets(db: PgPool) {
        use crate::tickets::{self, TicketState};
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let at_head = RequesterDetails {
            site_id: Some(head),
            ..RequesterDetails::default()
        };
        update(&db, ada, &at_head).await.unwrap();
        let open = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;
        let closed = testing::ticket(&db, brightfield, Some(ada), "a2@brightfield.test").await;
        let laptop = testing::asset(&db, brightfield, "Laptop", "BF-LT-07").await;
        crate::catalog::link_asset(&db, closed, laptop, true)
            .await
            .unwrap();
        tickets::set_state(&db, closed, TicketState::Closed)
            .await
            .unwrap();
        let other = testing::ticket(&db, brightfield, None, "o1@brightfield.test").await;

        let moved = move_to_customer(&db, ada, harbor).await.unwrap();
        assert_eq!((moved.moved, moved.unlinked), (2, 1));
        for n in [open, closed] {
            assert_eq!(tickets::detail(&db, n).await.unwrap().customer.id, harbor);
        }
        assert_eq!(
            tickets::detail(&db, other).await.unwrap().customer.id,
            brightfield,
            "other requesters' tickets stay"
        );
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!(seen.customer.id, harbor);
        assert!(seen.site.is_none(), "a site of the old customer");
    }

    /// Before a requester is moved, the agent is told what the move unlinks across all their
    /// tickets: the old customer's assets, and the services the new customer does not use.
    #[sqlx::test]
    async fn a_requester_s_move_is_previewed(db: PgPool) {
        use crate::catalog;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let first = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;
        let second = testing::ticket(&db, brightfield, Some(ada), "a2@brightfield.test").await;
        let laptop = testing::asset(&db, brightfield, "Laptop", "BF-LT-07").await;
        let printer = testing::asset(&db, brightfield, "Printer", "BF-PR-02").await;
        let mail = catalog::create_service(&db, "Hosted mail", "")
            .await
            .unwrap();
        let backup = catalog::create_service(&db, "Backup", "").await.unwrap();
        for service in [mail, backup] {
            catalog::use_service(&db, service, brightfield)
                .await
                .unwrap();
        }
        catalog::use_service(&db, mail, harbor).await.unwrap();
        catalog::link_asset(&db, first, laptop, true).await.unwrap();
        catalog::link_asset(&db, second, laptop, true)
            .await
            .unwrap();
        catalog::link_asset(&db, second, printer, true)
            .await
            .unwrap();
        catalog::link_service(&db, first, mail, true).await.unwrap();
        catalog::link_service(&db, second, backup, true)
            .await
            .unwrap();

        let preview = move_preview(&db, ada, harbor).await.unwrap();
        assert!(preview.site.is_none());
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let at_head = RequesterDetails {
            site_id: Some(head),
            ..RequesterDetails::default()
        };
        update(&db, ada, &at_head).await.unwrap();

        let preview = move_preview(&db, ada, harbor).await.unwrap();
        assert_eq!(
            preview.site.map(|s| s.name).as_deref(),
            Some("Head office"),
            "the site they leave"
        );
        assert_eq!(preview.tickets, 2);
        let names = |names: Vec<&str>| names.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            preview
                .assets
                .into_iter()
                .map(|a| a.name)
                .collect::<Vec<_>>(),
            names(vec!["BF-LT-07", "BF-PR-02"])
        );
        assert_eq!(
            preview
                .services
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>(),
            names(vec!["Backup"])
        );
        let moved = move_to_customer(&db, ada, harbor).await.unwrap();
        assert_eq!(
            moved.unlinked, 4,
            "both laptop links, the printer and backup"
        );
    }

    /// A requester without tickets, or with only closed ones, is moved all the same.
    #[sqlx::test]
    async fn a_requester_without_open_tickets_is_moved_too(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let n = testing::ticket(&db, brightfield, Some(lena), "l1@brightfield.test").await;
        crate::tickets::set_state(&db, n, crate::tickets::TicketState::Closed)
            .await
            .unwrap();

        assert_eq!(move_to_customer(&db, ada, harbor).await.unwrap().moved, 0);
        assert_eq!(detail(&db, ada).await.unwrap().customer.id, harbor);
        assert_eq!(move_to_customer(&db, lena, harbor).await.unwrap().moved, 1);
        let ticket = crate::tickets::detail(&db, n).await.unwrap();
        assert_eq!(ticket.customer.id, harbor);
        assert_eq!(ticket.requester.map(|r| r.id), Some(lena));
    }

    /// A requester is not moved to their own customer or to an offboarded one, nor away from an
    /// offboarded one, nor with a parent ticket, which stays with the help desk's own organization.
    #[sqlx::test]
    async fn moving_a_requester_is_refused_where_their_tickets_cannot_go(db: PgPool) {
        use crate::tickets::{self, RequesterMove};
        let northgate = crate::customers::create(&db, "Northgate Studio", &[])
            .await
            .unwrap();
        sqlx::query("UPDATE customers SET is_help_desk = true WHERE id = $1")
            .bind(northgate)
            .execute(&db)
            .await
            .unwrap();
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let gone = testing::customer(&db, "Old Mill").await;
        crate::customers::offboard(&db, gone).await.unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let refused = |requester: Uuid, customer: Uuid| {
            let db = db.clone();
            async move {
                match move_to_customer(&db, requester, customer).await {
                    Err(Error::Rule(why)) => why,
                    other => panic!("move accepted: {other:?}"),
                }
            }
        };
        assert!(refused(ada, brightfield).await.contains("already"));
        assert!(refused(ada, gone).await.contains("offboarded"));
        assert!(matches!(
            move_to_customer(&db, ada, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));

        // Marco of the help desk opened a parent ticket for a failure; moving him would take it.
        let marco = testing::requester(&db, northgate, "marco@northgate.test").await;
        let parent = testing::ticket(&db, northgate, Some(marco), "p1@northgate.test").await;
        let report = testing::ticket(&db, northgate, Some(marco), "p2@northgate.test").await;
        let child = testing::ticket(&db, brightfield, None, "c1@brightfield.test").await;
        crate::parents::attach(&db, child, parent).await.unwrap();
        assert!(refused(marco, harbor).await.contains("parent"));
        let Err(Error::Rule(why)) =
            tickets::move_to_customer(&db, report, harbor, RequesterMove::Move).await
        else {
            panic!("a parent ticket moved along with its requester");
        };
        assert!(why.contains("parent"), "{why}");

        // An offboarded customer's requesters do not change: neither from the page nor by moving
        // one of their tickets with them.
        let hers = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;
        crate::customers::offboard(&db, brightfield).await.unwrap();
        assert!(refused(ada, harbor).await.contains("offboarded"));
        let Err(Error::Rule(why)) =
            tickets::move_to_customer(&db, hers, harbor, RequesterMove::Move).await
        else {
            panic!("an offboarded customer's requester moved with their ticket");
        };
        assert!(why.contains("offboarded"), "{why}");
    }

    /// The database keeps the moving rules even when the code is bypassed: a requester's customer
    /// does not change while the old or the new one is offboarded, and a parent ticket stays with
    /// the help desk's own organization, also when its requester moves.
    #[sqlx::test]
    async fn the_database_keeps_the_moving_rules(db: PgPool) {
        let northgate = testing::customer(&db, "Northgate Studio").await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let marco = testing::requester(&db, northgate, "marco@northgate.test").await;
        let parent = testing::ticket(&db, northgate, Some(marco), "p1@northgate.test").await;
        let child = testing::ticket(&db, brightfield, None, "c1@brightfield.test").await;
        sqlx::query(
            "UPDATE tickets SET parent_id = (SELECT id FROM tickets WHERE number = $1)
             WHERE number = $2",
        )
        .bind(parent)
        .bind(child)
        .execute(&db)
        .await
        .unwrap();
        let move_to = |requester: Uuid, customer: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query("UPDATE requesters SET customer_id = $2, site_id = NULL WHERE id = $1")
                    .bind(requester)
                    .bind(customer)
                    .execute(&db)
                    .await
            }
        };
        let offboarded = |customer: Uuid, yes: bool| {
            let db = db.clone();
            async move {
                sqlx::query(
                    "UPDATE customers SET offboarded_at = CASE WHEN $2 THEN now() END WHERE id = $1",
                )
                .bind(customer)
                .bind(yes)
                .execute(&db)
                .await
                .unwrap();
            }
        };

        assert!(
            move_to(marco, harbor).await.is_err(),
            "a parent ticket moved"
        );
        let parent_moved = sqlx::query("UPDATE tickets SET customer_id = $2 WHERE number = $1")
            .bind(parent)
            .bind(harbor)
            .execute(&db)
            .await;
        assert!(parent_moved.is_err(), "a parent ticket moved by itself");
        offboarded(harbor, true).await;
        assert!(
            move_to(ada, harbor).await.is_err(),
            "to an offboarded customer"
        );
        offboarded(harbor, false).await;
        offboarded(brightfield, true).await;
        assert!(
            move_to(ada, harbor).await.is_err(),
            "from an offboarded customer"
        );
        offboarded(brightfield, false).await;
        assert!(move_to(ada, harbor).await.is_ok());
    }

    /// A requester that remained from one merge can be merged in turn: everything ends with the
    /// last one that remains.
    #[sqlx::test]
    async fn merges_can_follow_one_another(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        let third = testing::requester(&db, brightfield, "ada.f@studio.test").await;
        let n = testing::ticket(&db, brightfield, Some(twin), "t1@home.test").await;

        merge(&db, ada, twin).await.unwrap();
        merge(&db, third, ada).await.unwrap();
        assert_eq!(
            addresses(&db, third).await.unwrap(),
            [
                "ada.f@studio.test",
                "ada@brightfield.test",
                "a.ferrini@home.test"
            ]
        );
        for gone in [ada, twin] {
            assert!(matches!(detail(&db, gone).await, Err(Error::NotFound(_))));
        }
        let requester = crate::tickets::detail(&db, n).await.unwrap().requester;
        assert_eq!(requester.map(|r| r.id), Some(third));
    }

    /// The requester that remains keeps their details and takes those they lack from the merged
    /// one, so that nothing about the person is lost.
    #[sqlx::test]
    async fn merging_keeps_the_details_of_both(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        let details =
            |name: &str, phones: &[&str], job: Option<&str>, notes: &str, site| RequesterDetails {
                name: Some(name.into()),
                phone_numbers: phones.iter().map(|p| (*p).into()).collect(),
                job_title: job.map(Into::into),
                notes: notes.into(),
                site_id: site,
            };
        update(
            &db,
            ada,
            &details(
                "Ada Ferrini",
                &["000 000 0101"],
                None,
                "Works mornings.",
                None,
            ),
        )
        .await
        .unwrap();
        update(
            &db,
            twin,
            &details(
                "A. Ferrini",
                &["000 000 0102", "000 000 0101"],
                Some("Office manager"),
                "Prefers phone calls.",
                Some(head),
            ),
        )
        .await
        .unwrap();

        merge(&db, ada, twin).await.unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert_eq!(seen.name.as_deref(), Some("Ada Ferrini"));
        assert_eq!(seen.phone_numbers, ["000 000 0101", "000 000 0102"]);
        assert_eq!(seen.job_title.as_deref(), Some("Office manager"));
        assert_eq!(
            seen.notes,
            "Works mornings.

Prefers phone calls."
        );
        assert_eq!(seen.site.map(|s| s.id), Some(head));
    }

    /// A departed requester who is the same person as a present one is merged into them: their
    /// addresses and tickets pass to the one who remains, who is not departed.
    #[sqlx::test]
    async fn a_departed_requester_is_merged_into_a_present_one(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        let n = testing::ticket(&db, brightfield, Some(twin), "t1@home.test").await;
        depart(&db, twin).await.unwrap();

        merge(&db, ada, twin).await.unwrap();
        let seen = detail(&db, ada).await.unwrap();
        assert!(seen.departed_at.is_none());
        assert_eq!(
            seen.addresses,
            ["ada@brightfield.test", "a.ferrini@home.test"]
        );
        let requester = crate::tickets::detail(&db, n)
            .await
            .unwrap()
            .requester
            .unwrap();
        assert_eq!((requester.id, requester.departed), (ada, false));
    }

    /// Requesters of different customers are never merged, nor a requester with themselves; a
    /// departed requester and an offboarded customer's requesters are not merged either.
    #[sqlx::test]
    async fn merging_is_refused_unless_both_are_present_requesters_of_one_customer(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        let refused = |kept: Uuid, merged: Uuid| {
            let db = db.clone();
            async move {
                match merge(&db, kept, merged).await {
                    Err(Error::Rule(why)) => why,
                    other => panic!("merge accepted: {other:?}"),
                }
            }
        };

        assert!(refused(ada, ivo).await.contains("different customers"));
        assert!(refused(ivo, ada).await.contains("different customers"));
        assert!(refused(ada, ada).await.contains("themselves"));
        assert!(matches!(
            merge(&db, ada, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));
        depart(&db, twin).await.unwrap();
        assert!(
            refused(twin, ada).await.contains("departed"),
            "kept a departed requester"
        );
        bring_back(&db, twin).await.unwrap();
        crate::customers::offboard(&db, brightfield).await.unwrap();
        assert!(refused(ada, twin).await.contains("offboarded"));
        for (requester, address) in [(ada, "ada@brightfield.test"), (twin, "a.ferrini@home.test")] {
            assert_eq!(addresses(&db, requester).await.unwrap(), [address]);
        }
    }

    /// The database keeps the merge rules even when the code is bypassed: a merge joins two
    /// requesters of one customer that is not offboarded, the one that remains has not departed,
    /// and the merged one is gone.
    #[sqlx::test]
    async fn the_database_keeps_the_merge_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        // A merge of `merged` into `kept` done by hand; `remove` deletes the merged requester.
        let by_hand = |kept: Uuid, merged: Uuid, remove: bool| {
            let db = db.clone();
            async move {
                let mut tx = db.begin().await?;
                sqlx::query("INSERT INTO requester_merges (merged_id, kept_id) VALUES ($1, $2)")
                    .bind(merged)
                    .bind(kept)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "UPDATE requester_addresses SET requester_id = $1 WHERE requester_id = $2",
                )
                .bind(kept)
                .bind(merged)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE tickets SET requester_id = $1 WHERE requester_id = $2")
                    .bind(kept)
                    .bind(merged)
                    .execute(&mut *tx)
                    .await?;
                if remove {
                    sqlx::query("DELETE FROM requesters WHERE id = $1")
                        .bind(merged)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await
            }
        };
        // Ivo has no tickets, so only the merge's own check can refuse him.
        let across = by_hand(ada, ivo, true).await.unwrap_err().to_string();
        assert!(across.contains("same customer"), "{across}");
        assert!(by_hand(ada, ada, true).await.is_err(), "with themselves");
        assert!(
            by_hand(ada, twin, false).await.is_err(),
            "the merged one remains"
        );
        let pass = sqlx::query(
            "UPDATE requester_addresses SET requester_id = $1 WHERE address = 'a.ferrini@home.test'",
        )
        .bind(ada)
        .execute(&db)
        .await;
        assert!(
            pass.is_err(),
            "an address passed within the customer without a merge"
        );
        depart(&db, twin).await.unwrap();
        assert!(
            by_hand(twin, ada, true).await.is_err(),
            "kept a departed requester"
        );
        crate::customers::offboard(&db, brightfield).await.unwrap();
        assert!(
            by_hand(ada, twin, true).await.is_err(),
            "an offboarded customer"
        );
        crate::customers::bring_back(&db, brightfield)
            .await
            .unwrap();
        let merged = by_hand(ada, twin, true).await;
        assert!(merged.is_ok(), "a departed one merged: {merged:?}");
    }

    /// The database keeps the departed requesters' rules even when the code is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_departed_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
        let ticket: Uuid = sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1")
            .bind(n)
            .fetch_one(&db)
            .await
            .unwrap();
        // Each statement takes the requester as $1 and the ticket as $2.
        let run = |sql: &'static str| {
            let db = db.clone();
            async move { sqlx::query(sql).bind(ada).bind(ticket).execute(&db).await }
        };
        run("UPDATE requesters SET departed_at = now() WHERE id = $1 AND $2::uuid IS NOT NULL")
            .await
            .unwrap();

        assert!(
            run("INSERT INTO tickets (id, customer_id, requester_id, title)
                 SELECT gen_random_uuid(), customer_id, $1, 'x' FROM tickets WHERE id = $2")
            .await
            .is_err(),
            "a ticket opened for a departed requester"
        );
        run("UPDATE tickets SET requester_id = NULL WHERE id = $2 AND $1::uuid IS NOT NULL")
            .await
            .unwrap();
        assert!(
            run("UPDATE tickets SET requester_id = $1 WHERE id = $2")
                .await
                .is_err(),
            "a ticket given to a departed requester"
        );
        assert!(
            run(
                "INSERT INTO messages (id, ticket_id, direction, from_address, subject)
                 SELECT gen_random_uuid(), $2, 'inbound', a.address, 'x'
                 FROM requester_addresses a WHERE a.requester_id = $1"
            )
            .await
            .is_err(),
            "a departed requester's message routed to a ticket"
        );
        assert!(
            run("INSERT INTO messages (id, direction, from_address, subject)
                 SELECT gen_random_uuid(), 'inbound', a.address, 'x'
                 FROM requester_addresses a WHERE a.requester_id = $1 AND $2::uuid IS NOT NULL")
            .await
            .is_ok(),
            "it waits in the inbox"
        );

        // A departed requester can be left without an address, and comes back only with one.
        run("DELETE FROM requester_addresses WHERE requester_id = $1 AND $2::uuid IS NOT NULL")
            .await
            .unwrap();
        assert!(
            run("UPDATE requesters SET departed_at = NULL WHERE id = $1 AND $2::uuid IS NOT NULL")
                .await
                .is_err(),
            "back without an address"
        );
    }

    /// The database keeps the details' rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_details_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let quay = crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let set = |column: &'static str, value: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE requesters SET {column} = {value} WHERE id = $1"
                )))
                .bind(ada)
                .execute(&db)
                .await
            }
        };
        assert!(set("phone_numbers", "ARRAY['000 000 0101']").await.is_ok());
        assert!(
            set("phone_numbers", "ARRAY['000 000', ' ']").await.is_err(),
            "a blank phone"
        );
        assert!(
            set("phone_numbers", "ARRAY[E'000 000\n']").await.is_err(),
            "an untrimmed phone"
        );
        assert!(
            set("job_title", "' Manager'").await.is_err(),
            "an untrimmed job title"
        );
        assert!(set("name", "' Ada'").await.is_err(), "an untrimmed name");
        assert!(
            set("name", "''").await.is_err(),
            "a blank name rather than none"
        );

        // A name taken from a message is kept trimmed.
        let mut tx = db.begin().await.unwrap();
        let lena = create(
            &mut tx,
            brightfield,
            "lena@brightfield.test",
            Some(" Lena Moro "),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            detail(&db, lena).await.unwrap().name.as_deref(),
            Some("Lena Moro")
        );
        let at = |site: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query("UPDATE requesters SET site_id = $2 WHERE id = $1")
                    .bind(ada)
                    .bind(site)
                    .execute(&db)
                    .await
            }
        };
        assert!(at(quay).await.is_err(), "another customer's site");
        assert!(at(head).await.is_ok());
        let retire = sqlx::query("UPDATE sites SET retired_at = now() WHERE id = $1")
            .bind(head)
            .execute(&db)
            .await;
        assert!(retire.is_err(), "a site retired with a requester at it");

        let old = crate::sites::create(&db, brightfield, "Old office", "Via del Glicine 3")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        assert!(at(old).await.is_err(), "a retired site");
    }

    /// The database keeps a requester from a site retired while they are being put at it, the code
    /// bypassed on both sides: putting them there waits for the retirement, and is refused.
    #[sqlx::test]
    async fn the_database_keeps_requesters_from_a_site_retired_at_the_same_time(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;

        let mut retiring = db.begin().await.unwrap();
        sqlx::query("UPDATE sites SET retired_at = now() WHERE id = $1")
            .bind(head)
            .execute(&mut *retiring)
            .await
            .unwrap();
        let meanwhile = tokio::spawn({
            let db = db.clone();
            async move {
                sqlx::query("UPDATE requesters SET site_id = $2 WHERE id = $1")
                    .bind(ada)
                    .bind(head)
                    .execute(&db)
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        retiring.commit().await.unwrap();
        assert!(meanwhile.await.unwrap().is_err(), "put at the retired site");
    }

    /// The database keeps the rules even when the code above is bypassed.
    #[sqlx::test]
    async fn the_database_keeps_the_address_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;

        let last = sqlx::query("DELETE FROM requester_addresses WHERE requester_id = $1")
            .bind(ada)
            .execute(&db)
            .await;
        assert!(last.is_err(), "the last address was removed");

        let mut tx = db.begin().await.unwrap();
        sqlx::query("INSERT INTO requesters (id, customer_id) VALUES ($1, $2)")
            .bind(Uuid::new_v4())
            .bind(brightfield)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(tx.commit().await.is_err(), "a requester without an address");

        let taken = sqlx::query(
            "INSERT INTO requester_addresses (address, requester_id) VALUES ('ada@brightfield.test', $1)",
        )
        .bind(testing::requester(&db, brightfield, "lena@brightfield.test").await)
        .execute(&db)
        .await;
        assert!(taken.is_err(), "an address with two requesters");

        let upper = sqlx::query(
            "INSERT INTO requester_addresses (address, requester_id) VALUES ('Ada@Home.test', $1)",
        )
        .bind(ada)
        .execute(&db)
        .await;
        assert!(upper.is_err(), "an address that is not lowercase");
    }

    /// The requesters registered before addresses were their own keep theirs.
    #[sqlx::test(migrations = false)]
    async fn existing_requesters_keep_their_address(db: PgPool) {
        let migrator = sqlx::migrate!();
        migrator.run_to(12, &db).await.unwrap();
        let customer = Uuid::new_v4();
        let ada = Uuid::new_v4();
        sqlx::query("INSERT INTO customers (id, name) VALUES ($1, 'Brightfield')")
            .bind(customer)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO requesters (id, customer_id, email) VALUES ($1, $2, 'ada@brightfield.test')",
        )
        .bind(ada)
        .bind(customer)
        .execute(&db)
        .await
        .unwrap();

        migrator.run(&db).await.unwrap();
        assert_eq!(addresses(&db, ada).await.unwrap(), ["ada@brightfield.test"]);
    }
}
