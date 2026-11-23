//! What tickets refer to: assets customers own and services the help desk provides (ADR 0002,
//! ADR 0003). Rules are in docs/domain-rules.md ("Customers, assets and services").

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};
use crate::requesters::SiteRef;

#[derive(Debug, Serialize, ToSchema)]
pub struct Asset {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub kind_id: Uuid,
    /// The name of its asset kind: laptop, printer, application, licence…
    pub kind: String,
    /// Whether its kind moves with its holder; otherwise it stays at its own site.
    pub moves_with_holder: bool,
    pub name: String,
    pub serial_number: Option<String>,
    /// Its own site, one of its customer's.
    pub site: Option<SiteRef>,
    /// The requester it has been given to, of its customer and not departed.
    pub holder: Option<Holder>,
    /// Where it is: its holder's site while it has a holder, when its kind moves with its holder;
    /// its own site otherwise.
    pub at_site: Option<SiteRef>,
    /// The add-on it came from, when an add-on supplies it.
    pub source_addon: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
}

/// The requester an asset has been given to.
#[derive(Debug, Serialize, ToSchema)]
pub struct Holder {
    pub id: Uuid,
    pub name: Option<String>,
    /// The first of their addresses.
    pub address: Option<String>,
}

/// An asset as the database gives it, before its sites and holder are put together.
#[derive(sqlx::FromRow)]
struct AssetRow {
    id: Uuid,
    customer_id: Uuid,
    customer_name: String,
    kind_id: Uuid,
    kind: String,
    moves_with_holder: bool,
    name: String,
    serial_number: Option<String>,
    site_id: Option<Uuid>,
    site_name: Option<String>,
    holder_id: Option<Uuid>,
    holder_name: Option<String>,
    holder_address: Option<String>,
    at_site_id: Option<Uuid>,
    at_site_name: Option<String>,
    source_addon: Option<String>,
    retired_at: Option<OffsetDateTime>,
}

impl From<AssetRow> for Asset {
    fn from(row: AssetRow) -> Asset {
        let site = |id: Option<Uuid>, name: Option<String>| {
            id.zip(name).map(|(id, name)| SiteRef { id, name })
        };
        Asset {
            id: row.id,
            customer_id: row.customer_id,
            customer_name: row.customer_name,
            kind_id: row.kind_id,
            kind: row.kind,
            moves_with_holder: row.moves_with_holder,
            name: row.name,
            serial_number: row.serial_number,
            site: site(row.site_id, row.site_name),
            holder: row.holder_id.map(|id| Holder {
                id,
                name: row.holder_name,
                address: row.holder_address,
            }),
            at_site: site(row.at_site_id, row.at_site_name),
            source_addon: row.source_addon,
            retired_at: row.retired_at,
        }
    }
}

/// The assets picked by the end of the query (`WHERE`, `ORDER BY`, on `assets a` and `asset_kinds
/// k`), with their site and holder, and the site each is at (the `asset_sites` view follows its
/// kind).
macro_rules! select_assets {
    ($rest:literal) => {
        concat!(
            "SELECT a.id, a.customer_id, c.name AS customer_name, a.kind_id, k.name AS kind,
                    k.moves_with_holder, a.name, a.serial_number,
                    a.site_id, s.name AS site_name,
                    a.holder_id, h.name AS holder_name,
                    (SELECT ha.address FROM requester_addresses ha WHERE ha.requester_id = h.id
                     ORDER BY ha.created_at, ha.address LIMIT 1) AS holder_address,
                    w.site_id AS at_site_id, ws.name AS at_site_name,
                    a.source_addon, a.retired_at
             FROM assets a
             JOIN customers c ON c.id = a.customer_id
             JOIN asset_kinds k ON k.id = a.kind_id
             LEFT JOIN sites s ON s.id = a.site_id
             LEFT JOIN requesters h ON h.id = a.holder_id
             JOIN asset_sites w ON w.asset_id = a.id
             LEFT JOIN sites ws ON ws.id = w.site_id
             ",
            $rest
        )
    };
}

/// What agents who manage the catalog set on an asset.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AssetFields {
    /// Its asset kind, one of the list the help desk manages.
    pub kind_id: Uuid,
    pub name: String,
    /// Empty or left out for none.
    #[serde(default)]
    pub serial_number: Option<String>,
    /// One of the asset's customer's sites that is not retired.
    #[serde(default)]
    pub site_id: Option<Uuid>,
    /// A requester of the asset's customer who has not departed.
    #[serde(default)]
    pub holder_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Service {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// How many customers use it, the help desk's own organization included.
    pub customers: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
}

#[derive(Serialize, ToSchema)]
pub struct ServiceDetail {
    #[serde(flatten)]
    pub service: Service,
    pub used_by: Vec<CustomerRef>,
    /// The assets the service runs on.
    pub runs_on: Vec<Asset>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct CustomerRef {
    pub id: Uuid,
    pub name: String,
    pub is_help_desk: bool,
}

fn text(value: &str, what: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return rule(format!("the {what} cannot be empty"));
    }
    Ok(value.to_owned())
}

/// The customer's assets, active ones first.
pub async fn assets_of(db: &PgPool, customer: Uuid) -> Result<Vec<Asset>> {
    let rows: Vec<AssetRow> = sqlx::query_as(select_assets!(
        "WHERE a.customer_id = $1
         ORDER BY a.retired_at IS NOT NULL, lower(k.name), a.name"
    ))
    .bind(customer)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Asset::from).collect())
}

/// The assets the requester holds, by kind.
pub async fn held_by<'e>(db: impl sqlx::PgExecutor<'e>, requester: Uuid) -> Result<Vec<Asset>> {
    let rows: Vec<AssetRow> = sqlx::query_as(select_assets!(
        "WHERE a.holder_id = $1 ORDER BY lower(k.name), a.name"
    ))
    .bind(requester)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Asset::from).collect())
}

/// Passes the assets `requester` holds to `to`, or leaves them without a holder when `to` is
/// `None`: when the requester departs, is moved to another customer or is merged into another.
/// The caller has locked the requester's customer and the requester.
pub(crate) async fn pass_held(
    tx: &mut Transaction<'_, Postgres>,
    requester: Uuid,
    to: Option<Uuid>,
) -> Result<()> {
    sqlx::query("UPDATE assets SET holder_id = $2 WHERE holder_id = $1")
        .bind(requester)
        .bind(to)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Adds an asset the customer owns, with its details (see [`update_asset`]).
pub async fn create_asset(db: &PgPool, customer: Uuid, fields: &AssetFields) -> Result<Uuid> {
    let mut tx = db.begin().await?;
    crate::customers::lock_active(&mut tx, customer).await?;
    let details = checked_details(&mut tx, customer, fields).await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO assets (id, customer_id, kind_id, name, serial_number, site_id, holder_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(customer)
    .bind(fields.kind_id)
    .bind(details.name)
    .bind(details.serial_number)
    .bind(fields.site_id)
    .bind(fields.holder_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Changes an asset's kind, name, serial number, site and holder. The site is one of the asset's
/// customer's that is not retired; the holder is a requester of that customer who has not departed,
/// and a retired asset has none. Agents do not change an offboarded customer's assets, as they add
/// none to it.
pub async fn update_asset(db: &PgPool, id: Uuid, fields: &AssetFields) -> Result<()> {
    let mut tx = db.begin().await?;
    // An asset never changes customer, so its customer can be read before it is locked. The
    // customer is locked first (FOR UPDATE), as by every change to its requesters, sites and
    // assets; under that lock the order of the rest cannot make two such changes wait for each
    // other: here the holder, then the site, then the asset, while retiring a site takes the site,
    // then the requesters and assets at it. The asset is locked only against changes (`FOR NO KEY
    // UPDATE`), so that a ticket being linked to it is not kept waiting.
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM assets WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound("asset"))?;
    crate::customers::lock_active(&mut tx, customer).await?;
    let details = checked_details(&mut tx, customer, fields).await?;
    let retired: bool = sqlx::query_scalar(
        "SELECT retired_at IS NOT NULL FROM assets WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if retired && fields.holder_id.is_some() {
        return rule("a retired asset has no holder");
    }
    sqlx::query(
        "UPDATE assets SET kind_id = $2, name = $3, serial_number = $4, site_id = $5,
                holder_id = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(fields.kind_id)
    .bind(details.name)
    .bind(details.serial_number)
    .bind(fields.site_id)
    .bind(fields.holder_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// An asset's name and serial number, cleaned to be stored.
struct CleanDetails {
    name: String,
    serial_number: Option<String>,
}

/// Checks the details of an asset of `customer`, which the caller has locked: refuses those that
/// break a rule, and locks the holder and the site, so that one departing or retired at the same
/// time is waited for. Returns the name and serial number to store.
async fn checked_details(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
    fields: &AssetFields,
) -> Result<CleanDetails> {
    let name = text(&fields.name, "name")?;
    kind_exists(&mut **tx, fields.kind_id).await?;
    if let Some(holder) = fields.holder_id {
        let departed: Option<bool> = sqlx::query_scalar(
            "SELECT departed_at IS NOT NULL FROM requesters WHERE id = $1 AND customer_id = $2
             FOR SHARE",
        )
        .bind(holder)
        .bind(customer)
        .fetch_optional(&mut **tx)
        .await?;
        match departed {
            None => return rule("the holder is not a requester of the asset's customer"),
            Some(true) => return rule("the holder has departed"),
            Some(false) => {}
        }
    }
    if let Some(site) = fields.site_id {
        let retired: Option<bool> = sqlx::query_scalar(
            "SELECT retired_at IS NOT NULL FROM sites WHERE id = $1 AND customer_id = $2
             FOR SHARE",
        )
        .bind(site)
        .bind(customer)
        .fetch_optional(&mut **tx)
        .await?;
        match retired {
            None => return rule("the site is not one of the asset's customer"),
            Some(true) => return rule("the site is retired"),
            Some(false) => {}
        }
    }
    let serial_number = fields
        .serial_number
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Ok(CleanDetails {
        name,
        serial_number,
    })
}

/// Refuses a kind that is not on the list; kinds are never removed, so the check holds.
async fn kind_exists<'e>(db: impl sqlx::PgExecutor<'e>, kind: Uuid) -> Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM asset_kinds WHERE id = $1)")
            .bind(kind)
            .fetch_one(db)
            .await?;
    if !exists {
        return Err(Error::NotFound("asset kind"));
    }
    Ok(())
}

/// Retires an asset: it stays on the tickets that refer to it but gets no new links, and it loses
/// its holder.
pub async fn retire_asset(db: &PgPool, id: Uuid) -> Result<()> {
    let retired = sqlx::query(
        "UPDATE assets SET retired_at = now(), holder_id = NULL
         WHERE id = $1 AND retired_at IS NULL",
    )
    .bind(id)
    .execute(db)
    .await?;
    if retired.rows_affected() == 1 {
        return Ok(());
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assets WHERE id = $1)")
        .bind(id)
        .fetch_one(db)
        .await?;
    if exists {
        rule("the asset is already retired")
    } else {
        Err(Error::NotFound("asset"))
    }
}

const SERVICES: &str = "SELECT s.id, s.name, s.description, s.retired_at,
        (SELECT count(*) FROM customer_services cs WHERE cs.service_id = s.id) AS customers
     FROM services s
     WHERE $1::uuid IS NULL OR s.id = $1
     ORDER BY s.retired_at IS NOT NULL, s.name";

pub async fn services(db: &PgPool) -> Result<Vec<Service>> {
    Ok(sqlx::query_as(SERVICES)
        .bind(None::<Uuid>)
        .fetch_all(db)
        .await?)
}

/// The services a customer uses.
pub async fn services_of(db: &PgPool, customer: Uuid) -> Result<Vec<Service>> {
    Ok(sqlx::query_as(
        "SELECT s.id, s.name, s.description, s.retired_at,
                (SELECT count(*) FROM customer_services o WHERE o.service_id = s.id) AS customers
         FROM services s JOIN customer_services cs ON cs.service_id = s.id
         WHERE cs.customer_id = $1
         ORDER BY s.retired_at IS NOT NULL, s.name",
    )
    .bind(customer)
    .fetch_all(db)
    .await?)
}

pub async fn service_detail(db: &PgPool, id: Uuid) -> Result<ServiceDetail> {
    let service: Service = sqlx::query_as(SERVICES)
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound("service"))?;
    let used_by = sqlx::query_as(
        "SELECT c.id, c.name, c.is_help_desk FROM customers c
         JOIN customer_services cs ON cs.customer_id = c.id
         WHERE cs.service_id = $1 ORDER BY c.is_help_desk DESC, c.name",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    let runs_on: Vec<AssetRow> = sqlx::query_as(select_assets!(
        "WHERE a.id IN (SELECT asset_id FROM service_assets WHERE service_id = $1)
         ORDER BY a.name"
    ))
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(ServiceDetail {
        service,
        used_by,
        runs_on: runs_on.into_iter().map(Asset::from).collect(),
    })
}

/// Adds a service; the help desk's own organization uses it from the start.
pub async fn create_service(db: &PgPool, name: &str, description: &str) -> Result<Uuid> {
    let name = text(name, "name")?;
    let mut tx = db.begin().await?;
    let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM services WHERE name = $1)")
        .bind(&name)
        .fetch_one(&mut *tx)
        .await?;
    if taken {
        return rule(format!("there is already a service called {name}"));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO services (id, name, description) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(&name)
        .bind(description.trim())
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO customer_services (customer_id, service_id)
         SELECT id, $1 FROM customers WHERE is_help_desk",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn update_service(db: &PgPool, id: Uuid, name: &str, description: &str) -> Result<()> {
    let name = text(name, "name")?;
    let mut tx = db.begin().await?;
    let taken: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM services WHERE name = $1 AND id <> $2)")
            .bind(&name)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if taken {
        return rule(format!("there is already a service called {name}"));
    }
    let updated = sqlx::query("UPDATE services SET name = $2, description = $3 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .bind(description.trim())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(Error::NotFound("service"));
    }
    tx.commit().await?;
    Ok(())
}

/// Retires a service: it stays on the tickets that refer to it but gets no new links or users.
pub async fn retire_service(db: &PgPool, id: Uuid) -> Result<()> {
    let retired: Option<bool> =
        sqlx::query_scalar("SELECT retired_at IS NOT NULL FROM services WHERE id = $1")
            .bind(id)
            .fetch_optional(db)
            .await?;
    match retired {
        None => Err(Error::NotFound("service")),
        Some(true) => rule("the service is already retired"),
        Some(false) => {
            sqlx::query("UPDATE services SET retired_at = now() WHERE id = $1")
                .bind(id)
                .execute(db)
                .await?;
            Ok(())
        }
    }
}

/// Makes the help desk's own organization a user of every service; run when it is created.
pub async fn help_desk_uses_every_service(
    tx: &mut Transaction<'_, Postgres>,
    customer: Uuid,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO customer_services (customer_id, service_id)
         SELECT $1, id FROM services ON CONFLICT DO NOTHING",
    )
    .bind(customer)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn use_service(db: &PgPool, service: Uuid, customer: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    let retired: Option<bool> =
        sqlx::query_scalar("SELECT retired_at IS NOT NULL FROM services WHERE id = $1")
            .bind(service)
            .fetch_optional(&mut *tx)
            .await?;
    match retired {
        None => return Err(Error::NotFound("service")),
        Some(true) => return rule("the service is retired"),
        Some(false) => {}
    }
    let offboarded: Option<bool> =
        sqlx::query_scalar("SELECT offboarded_at IS NOT NULL FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(&mut *tx)
            .await?;
    match offboarded {
        None => return Err(Error::NotFound("customer")),
        Some(true) => return rule("the customer is offboarded"),
        Some(false) => {}
    }
    sqlx::query(
        "INSERT INTO customer_services (customer_id, service_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(customer)
    .bind(service)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The customer stops using the service. Refused for the help desk's own organization, and while
/// tickets of the customer still refer to the service.
pub async fn stop_using_service(db: &PgPool, service: Uuid, customer: Uuid) -> Result<()> {
    let mut tx = db.begin().await?;
    let help_desk: Option<bool> =
        sqlx::query_scalar("SELECT is_help_desk FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(&mut *tx)
            .await?;
    match help_desk {
        None => return Err(Error::NotFound("customer")),
        Some(true) => return rule("the help desk's own organization uses every service"),
        Some(false) => {}
    }
    let tickets: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ticket_services WHERE customer_id = $1 AND service_id = $2",
    )
    .bind(customer)
    .bind(service)
    .fetch_one(&mut *tx)
    .await?;
    if tickets > 0 {
        return rule(format!(
            "{tickets} ticket(s) of this customer refer to the service; unlink it from them first"
        ));
    }
    let removed =
        sqlx::query("DELETE FROM customer_services WHERE customer_id = $1 AND service_id = $2")
            .bind(customer)
            .bind(service)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if removed == 0 {
        return rule("the customer does not use this service");
    }
    tx.commit().await?;
    Ok(())
}

/// Records that the service runs on the asset (an asset of any customer).
pub async fn runs_on(db: &PgPool, service: Uuid, asset: Uuid, linked: bool) -> Result<()> {
    if linked {
        let ok: Option<(bool, bool)> = sqlx::query_as(
            "SELECT s.retired_at IS NULL, a.retired_at IS NULL FROM services s, assets a
             WHERE s.id = $1 AND a.id = $2",
        )
        .bind(service)
        .bind(asset)
        .fetch_optional(db)
        .await?;
        match ok {
            None => return Err(Error::NotFound("service or asset")),
            Some((false, _)) => return rule("the service is retired"),
            Some((_, false)) => return rule("the asset is retired"),
            Some(_) => {}
        }
        sqlx::query(
            "INSERT INTO service_assets (service_id, asset_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(service)
        .bind(asset)
        .execute(db)
        .await?;
    } else {
        sqlx::query("DELETE FROM service_assets WHERE service_id = $1 AND asset_id = $2")
            .bind(service)
            .bind(asset)
            .execute(db)
            .await?;
    }
    Ok(())
}

/// The ticket's id and customer, locked, if it is not closed.
async fn open_ticket(tx: &mut Transaction<'_, Postgres>, number: i64) -> Result<(Uuid, Uuid)> {
    let row: Option<(Uuid, Uuid, String)> =
        sqlx::query_as("SELECT id, customer_id, state FROM tickets WHERE number = $1 FOR UPDATE")
            .bind(number)
            .fetch_optional(&mut **tx)
            .await?;
    match row {
        None => Err(Error::NotFound("ticket")),
        Some((_, _, state)) if state == "closed" => {
            rule("the ticket is closed; a closed ticket cannot change")
        }
        Some((id, customer, _)) => Ok((id, customer)),
    }
}

/// Links an asset of the ticket's customer to the ticket, or unlinks it.
pub async fn link_asset(db: &PgPool, number: i64, asset: Uuid, linked: bool) -> Result<()> {
    let mut tx = db.begin().await?;
    let (ticket, customer) = open_ticket(&mut tx, number).await?;
    if linked {
        let row: Option<(Uuid, bool)> =
            sqlx::query_as("SELECT customer_id, retired_at IS NOT NULL FROM assets WHERE id = $1")
                .bind(asset)
                .fetch_optional(&mut *tx)
                .await?;
        match row {
            None => return Err(Error::NotFound("asset")),
            Some((owner, _)) if owner != customer => {
                return rule("the asset belongs to another customer than the ticket");
            }
            Some((_, true)) => return rule("the asset is retired"),
            Some(_) => {}
        }
        sqlx::query(
            "INSERT INTO ticket_assets (ticket_id, customer_id, asset_id) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(ticket)
        .bind(customer)
        .bind(asset)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("DELETE FROM ticket_assets WHERE ticket_id = $1 AND asset_id = $2")
            .bind(ticket)
            .bind(asset)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE tickets SET updated_at = now() WHERE id = $1")
        .bind(ticket)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Links a service the ticket's customer uses to the ticket, or unlinks it.
pub async fn link_service(db: &PgPool, number: i64, service: Uuid, linked: bool) -> Result<()> {
    let mut tx = db.begin().await?;
    let (ticket, customer) = open_ticket(&mut tx, number).await?;
    if linked {
        let row: Option<(bool, bool)> = sqlx::query_as(
            "SELECT s.retired_at IS NOT NULL,
                    EXISTS (SELECT 1 FROM customer_services cs
                            WHERE cs.service_id = s.id AND cs.customer_id = $2)
             FROM services s WHERE s.id = $1",
        )
        .bind(service)
        .bind(customer)
        .fetch_optional(&mut *tx)
        .await?;
        match row {
            None => return Err(Error::NotFound("service")),
            Some((true, _)) => return rule("the service is retired"),
            Some((_, false)) => return rule("the ticket's customer does not use this service"),
            Some(_) => {}
        }
        sqlx::query(
            "INSERT INTO ticket_services (ticket_id, customer_id, service_id) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(ticket)
        .bind(customer)
        .bind(service)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("DELETE FROM ticket_services WHERE ticket_id = $1 AND service_id = $2")
            .bind(ticket)
            .bind(service)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE tickets SET updated_at = now() WHERE id = $1")
        .bind(ticket)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The links of `tickets` that would not fit `customer` if they moved there, each listed once:
/// every linked asset, since assets belong to the old customer, and the linked services the new
/// customer does not use. Moving removes exactly these ([`unlink_for_move`]).
pub async fn unfit_for_move(
    conn: &mut PgConnection,
    tickets: &[Uuid],
    customer: Uuid,
) -> Result<(Vec<LinkedAsset>, Vec<LinkedService>)> {
    let assets = sqlx::query_as(
        "SELECT a.id, k.name AS kind, a.name FROM assets a JOIN asset_kinds k ON k.id = a.kind_id
         WHERE a.id IN (SELECT asset_id FROM ticket_assets WHERE ticket_id = ANY($1))
         ORDER BY lower(k.name), a.name",
    )
    .bind(tickets)
    .fetch_all(&mut *conn)
    .await?;
    let services = sqlx::query_as(
        "SELECT s.id, s.name FROM services s
         WHERE s.id IN (SELECT service_id FROM ticket_services WHERE ticket_id = ANY($1))
           AND NOT EXISTS (SELECT 1 FROM customer_services cs
                           WHERE cs.customer_id = $2 AND cs.service_id = s.id)
         ORDER BY s.name",
    )
    .bind(tickets)
    .bind(customer)
    .fetch_all(&mut *conn)
    .await?;
    Ok((assets, services))
}

/// Before `tickets` move to `customer`: removes their links that would not fit there (see
/// [`unfit_for_move`]). Returns how many links were removed.
pub async fn unlink_for_move(
    tx: &mut Transaction<'_, Postgres>,
    tickets: &[Uuid],
    customer: Uuid,
) -> Result<i64> {
    let (assets, services) = unfit_for_move(tx, tickets, customer).await?;
    let assets: Vec<Uuid> = assets.into_iter().map(|a| a.id).collect();
    let services: Vec<Uuid> = services.into_iter().map(|s| s.id).collect();
    let assets =
        sqlx::query("DELETE FROM ticket_assets WHERE ticket_id = ANY($1) AND asset_id = ANY($2)")
            .bind(tickets)
            .bind(&assets)
            .execute(&mut **tx)
            .await?
            .rows_affected();
    let services = sqlx::query(
        "DELETE FROM ticket_services WHERE ticket_id = ANY($1) AND service_id = ANY($2)",
    )
    .bind(tickets)
    .bind(&services)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    // Services both customers use stay linked; the foreign keys carry them to the new customer.
    Ok((assets + services) as i64)
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct LinkedAsset {
    pub id: Uuid,
    /// The name of its asset kind.
    pub kind: String,
    pub name: String,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct LinkedService {
    pub id: Uuid,
    pub name: String,
}

pub async fn ticket_links(
    db: &PgPool,
    ticket: Uuid,
) -> Result<(Vec<LinkedAsset>, Vec<LinkedService>)> {
    let assets = sqlx::query_as(
        "SELECT a.id, k.name AS kind, a.name FROM assets a JOIN ticket_assets ta ON ta.asset_id = a.id
         JOIN asset_kinds k ON k.id = a.kind_id
         WHERE ta.ticket_id = $1 ORDER BY lower(k.name), a.name",
    )
    .bind(ticket)
    .fetch_all(db)
    .await?;
    let services = sqlx::query_as(
        "SELECT s.id, s.name FROM services s JOIN ticket_services ts ON ts.service_id = s.id
         WHERE ts.ticket_id = $1 ORDER BY s.name",
    )
    .bind(ticket)
    .fetch_all(db)
    .await?;
    Ok((assets, services))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::tickets::{self, RequesterMove, TicketState};

    use crate::testing::asset_details as fields;

    #[sqlx::test]
    async fn an_asset_has_a_serial_number_a_site_and_a_holder(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let printer = testing::asset_kind(&db, "Printer").await;
        let id = create_asset(
            &db,
            brightfield,
            &fields(printer, "Printer 2F", None, None, None),
        )
        .await
        .unwrap();

        update_asset(
            &db,
            id,
            &fields(
                printer,
                "Printer 2F",
                Some(" SN-0042 "),
                Some(head),
                Some(ada),
            ),
        )
        .await
        .unwrap();
        let asset = &assets_of(&db, brightfield).await.unwrap()[0];
        assert_eq!(asset.serial_number.as_deref(), Some("SN-0042"));
        assert_eq!(
            asset.site.as_ref().map(|s| (s.id, s.name.as_str())),
            Some((head, "Head office"))
        );
        let holder = asset.holder.as_ref().unwrap();
        assert_eq!(
            (holder.id, holder.address.as_deref()),
            (ada, Some("ada@brightfield.test"))
        );

        update_asset(
            &db,
            id,
            &fields(printer, "Printer 2F", Some(" "), None, None),
        )
        .await
        .unwrap();
        let asset = &assets_of(&db, brightfield).await.unwrap()[0];
        assert_eq!(
            (
                asset.serial_number.as_deref(),
                asset.site.is_none(),
                asset.holder.is_none()
            ),
            (None, true, true),
            "all three cleared"
        );

        let set = create_asset(
            &db,
            brightfield,
            &fields(
                printer,
                "Printer 3F",
                Some("SN-0043"),
                Some(head),
                Some(ada),
            ),
        )
        .await
        .unwrap();
        let assets = assets_of(&db, brightfield).await.unwrap();
        let created = assets.iter().find(|a| a.id == set).unwrap();
        assert_eq!(
            (
                created.serial_number.as_deref(),
                created.site.as_ref().map(|s| s.id),
                created.holder.as_ref().map(|h| h.id)
            ),
            (Some("SN-0043"), Some(head), Some(ada)),
            "set when the asset is added"
        );
    }

    #[sqlx::test]
    async fn an_asset_s_site_and_holder_are_its_customer_s_and_active(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let quay = crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Old office", "Via del Glicine 3")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::requesters::depart(&db, lena).await.unwrap();
        let laptop = testing::asset_kind(&db, "Laptop").await;
        let pc = testing::asset(&db, brightfield, "Laptop", "PC-0042").await;

        for (site, holder, why) in [
            (Some(quay), None, "not one of the asset's customer"),
            (
                Some(Uuid::new_v4()),
                None,
                "not one of the asset's customer",
            ),
            (Some(old), None, "retired"),
            (None, Some(ivo), "not a requester of the asset's customer"),
            (
                None,
                Some(Uuid::new_v4()),
                "not a requester of the asset's customer",
            ),
            (None, Some(lena), "departed"),
        ] {
            for result in [
                update_asset(&db, pc, &fields(laptop, "PC-0042", None, site, holder)).await,
                create_asset(
                    &db,
                    brightfield,
                    &fields(laptop, "PC-0043", None, site, holder),
                )
                .await
                .map(|_| ()),
            ] {
                let Err(Error::Rule(refused)) = result else {
                    panic!("{site:?} and {holder:?} accepted");
                };
                assert!(refused.contains(why), "{refused}");
            }
        }
        assert_eq!(assets_of(&db, brightfield).await.unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn an_asset_is_where_its_kind_says(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let lab = crate::sites::create(&db, brightfield, "Lab", "Via del Glicine 3")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        let at_lab = crate::requesters::RequesterDetails {
            site_id: Some(lab),
            ..Default::default()
        };
        crate::requesters::update(&db, ada, &at_lab).await.unwrap();
        let laptop = crate::asset_kinds::create(&db, "Laptop", true)
            .await
            .unwrap();
        let printer = crate::asset_kinds::create(&db, "Printer", false)
            .await
            .unwrap();
        let add = |kind: Uuid, name: &'static str, holder: Option<Uuid>| {
            let db = db.clone();
            async move {
                create_asset(
                    &db,
                    brightfield,
                    &fields(kind, name, None, Some(head), holder),
                )
                .await
                .unwrap()
            }
        };
        let with_ada = add(laptop, "PC-0042", Some(ada)).await;
        let with_lena = add(laptop, "PC-0043", Some(lena)).await;
        let spare = add(laptop, "PC-0044", None).await;
        let printer_2f = add(printer, "Printer 2F", Some(ada)).await;

        let at = |assets: &[Asset], id: Uuid| {
            assets
                .iter()
                .find(|a| a.id == id)
                .unwrap()
                .at_site
                .as_ref()
                .map(|s| s.name.clone())
        };
        let assets = assets_of(&db, brightfield).await.unwrap();
        assert_eq!(
            at(&assets, with_ada).as_deref(),
            Some("Lab"),
            "with its holder"
        );
        assert_eq!(
            at(&assets, with_lena),
            None,
            "with a holder at no site, it is at none"
        );
        assert_eq!(
            at(&assets, spare).as_deref(),
            Some("Head office"),
            "at its own site without a holder"
        );
        assert_eq!(
            at(&assets, printer_2f).as_deref(),
            Some("Head office"),
            "a kind that stays at its site"
        );

        // Where it is follows the kind and the holder.
        crate::asset_kinds::update(&db, laptop, "Laptop", false)
            .await
            .unwrap();
        crate::asset_kinds::update(&db, printer, "Printer", true)
            .await
            .unwrap();
        let assets = assets_of(&db, brightfield).await.unwrap();
        assert_eq!(at(&assets, with_ada).as_deref(), Some("Head office"));
        assert_eq!(at(&assets, printer_2f).as_deref(), Some("Lab"));
    }

    #[sqlx::test]
    async fn a_retired_asset_loses_its_holder(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let laptop = testing::asset_kind(&db, "Laptop").await;
        let pc = create_asset(
            &db,
            brightfield,
            &fields(laptop, "PC-0042", Some("SN-0042"), Some(head), Some(ada)),
        )
        .await
        .unwrap();

        retire_asset(&db, pc).await.unwrap();
        let asset = &assets_of(&db, brightfield).await.unwrap()[0];
        assert!(asset.holder.is_none(), "it lost its holder");
        assert_eq!(
            (
                asset.serial_number.as_deref(),
                asset.site.as_ref().map(|s| s.id)
            ),
            (Some("SN-0042"), Some(head)),
            "it keeps the rest"
        );
        let Err(Error::Rule(why)) = update_asset(
            &db,
            pc,
            &fields(laptop, "PC-0042", Some("SN-0042"), Some(head), Some(ada)),
        )
        .await
        else {
            panic!("a retired asset was given a holder");
        };
        assert!(why.contains("retired"), "{why}");
    }

    #[sqlx::test]
    async fn a_ticket_links_only_assets_of_its_customer(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let pc = testing::asset(&db, brightfield, "computer", "PC-0042").await;
        let other = testing::asset(&db, harbor, "printer", "Printer 2F").await;
        let n = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;

        link_asset(&db, n, pc, true).await.unwrap();
        assert!(matches!(
            link_asset(&db, n, other, true).await,
            Err(Error::Rule(_))
        ));
        retire_asset(&db, pc).await.unwrap();
        let ticket = tickets::detail(&db, n).await.unwrap();
        assert_eq!(
            ticket.assets.len(),
            1,
            "a retired asset stays on its tickets"
        );
        link_asset(&db, n, pc, false).await.unwrap();
        assert!(
            matches!(link_asset(&db, n, pc, true).await, Err(Error::Rule(_))),
            "retired"
        );
    }

    #[sqlx::test]
    async fn a_ticket_links_only_services_its_customer_uses(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let mail = create_service(&db, "Hosted mail", "").await.unwrap();
        let n = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
        assert!(matches!(
            link_service(&db, n, mail, true).await,
            Err(Error::Rule(_))
        ));

        use_service(&db, mail, brightfield).await.unwrap();
        link_service(&db, n, mail, true).await.unwrap();
        assert!(
            matches!(
                stop_using_service(&db, mail, brightfield).await,
                Err(Error::Rule(_))
            ),
            "still on a ticket"
        );
        // The help desk uses every service, from the start and for good.
        let parent = testing::ticket(&db, own, None, "p@northgate.test").await;
        link_service(&db, parent, mail, true).await.unwrap();
        assert!(matches!(
            stop_using_service(&db, mail, own).await,
            Err(Error::Rule(_))
        ));
        assert_eq!(service_detail(&db, mail).await.unwrap().used_by.len(), 2);
    }

    /// The database keeps an asset's details within the rules even when the code above is
    /// bypassed: its serial number trimmed, its site and holder of its customer, the site not
    /// retired, the holder not departed, and no holder once it is retired.
    #[sqlx::test]
    async fn the_database_keeps_the_asset_rules(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let quay = crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
            .await
            .unwrap();
        let old = crate::sites::create(&db, brightfield, "Old office", "Via del Glicine 3")
            .await
            .unwrap();
        crate::sites::retire(&db, old).await.unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
        crate::requesters::depart(&db, lena).await.unwrap();
        let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
        let pc = testing::asset(&db, brightfield, "Laptop", "PC-0042").await;
        let set = |column: &'static str, value: Option<Uuid>| {
            let db = db.clone();
            async move {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE assets SET {column} = $2 WHERE id = $1"
                )))
                .bind(pc)
                .bind(value)
                .execute(&db)
                .await
            }
        };
        let serial = |value: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query("UPDATE assets SET serial_number = $2 WHERE id = $1")
                    .bind(pc)
                    .bind(value)
                    .execute(&db)
                    .await
            }
        };

        assert!(serial("SN-0042").await.is_ok());
        assert!(serial(" SN-0042").await.is_err(), "an untrimmed serial");
        assert!(serial("").await.is_err(), "a blank serial rather than none");
        assert!(
            set("site_id", Some(quay)).await.is_err(),
            "another customer's site"
        );
        assert!(set("site_id", Some(old)).await.is_err(), "a retired site");
        assert!(
            set("holder_id", Some(ivo)).await.is_err(),
            "another customer's requester"
        );
        assert!(
            set("holder_id", Some(lena)).await.is_err(),
            "a departed holder"
        );
        assert!(set("site_id", Some(head)).await.is_ok());
        assert!(set("holder_id", Some(ada)).await.is_ok());
        let retire = sqlx::query("UPDATE assets SET retired_at = now() WHERE id = $1")
            .bind(pc)
            .execute(&db)
            .await;
        assert!(retire.is_err(), "retired with its holder");

        // What would leave the asset breaking them is refused too.
        let run = |sql: &'static str, id: Uuid| {
            let db = db.clone();
            async move { sqlx::query(sql).bind(id).execute(&db).await }
        };
        assert!(
            run("UPDATE sites SET retired_at = now() WHERE id = $1", head)
                .await
                .is_err(),
            "a site retired with an asset at it"
        );
        assert!(
            run(
                "UPDATE requesters SET departed_at = now() WHERE id = $1",
                ada
            )
            .await
            .is_err(),
            "a holder departed"
        );
        let to_harbor = sqlx::query("UPDATE requesters SET customer_id = $2 WHERE id = $1")
            .bind(ada)
            .bind(harbor)
            .execute(&db)
            .await;
        assert!(to_harbor.is_err(), "a holder moved to another customer");
        let mut tx = db.begin().await.unwrap();
        let removed = sqlx::query("DELETE FROM requesters WHERE id = $1")
            .bind(ada)
            .execute(&mut *tx)
            .await;
        assert!(removed.is_err(), "a holder merged away");
    }

    /// The database keeps the rules when the site is retired, or the holder departs, while the
    /// asset is being put there or given to them, the code bypassed on both sides: whichever
    /// commits second is refused.
    #[sqlx::test]
    async fn the_database_keeps_the_asset_rules_at_the_same_time(db: PgPool) {
        // Waits on locks: it fails rather than hang when they do not go as planned.
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            asset_rules_at_the_same_time(db),
        )
        .await
        .expect("the statements did not end in time");
    }

    async fn asset_rules_at_the_same_time(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
            .await
            .unwrap();
        let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
        let pc = testing::asset(&db, brightfield, "Laptop", "PC-0042").await;
        for (first, second, value, refusal) in [
            (
                "UPDATE sites SET retired_at = now() WHERE id = $1",
                "UPDATE assets SET site_id = $2 WHERE id = $1",
                head,
                "an asset cannot be at a retired site",
            ),
            (
                "UPDATE requesters SET departed_at = now() WHERE id = $1",
                "UPDATE assets SET holder_id = $2 WHERE id = $1",
                ada,
                "a departed requester holds no asset",
            ),
        ] {
            let mut tx = db.begin().await.unwrap();
            sqlx::query(first)
                .bind(value)
                .execute(&mut *tx)
                .await
                .unwrap();
            let other = db.clone();
            let meanwhile = tokio::spawn(async move {
                sqlx::query(second)
                    .bind(pc)
                    .bind(value)
                    .execute(&other)
                    .await
            });
            assert!(
                testing::some_statement_waits_for_a_lock(&db).await,
                "{second} did not wait for {first}"
            );
            assert!(!meanwhile.is_finished(), "{second} ended before {first}");
            tx.commit().await.unwrap();
            let why = meanwhile.await.unwrap().unwrap_err().to_string();
            assert!(why.contains(refusal), "{second} after {first}: {why}");
        }
    }

    #[sqlx::test]
    async fn moving_a_ticket_drops_links_that_do_not_fit(db: PgPool) {
        testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let mail = create_service(&db, "Hosted mail", "").await.unwrap();
        let backup = create_service(&db, "Backup", "").await.unwrap();
        for customer in [brightfield, harbor] {
            use_service(&db, mail, customer).await.unwrap();
        }
        use_service(&db, backup, brightfield).await.unwrap();
        let pc = testing::asset(&db, brightfield, "computer", "PC-0042").await;
        let r = testing::requester(&db, brightfield, "luca.bassi@mail.test").await;
        let n = testing::ticket(&db, brightfield, Some(r), "l1@mail.test").await;
        link_asset(&db, n, pc, true).await.unwrap();
        link_service(&db, n, mail, true).await.unwrap();
        link_service(&db, n, backup, true).await.unwrap();

        let moved = tickets::move_to_customer(&db, n, harbor, RequesterMove::Move)
            .await
            .unwrap();
        assert_eq!(
            (moved.moved, moved.unlinked),
            (1, 2),
            "the asset and backup go, mail stays"
        );
        let ticket = tickets::detail(&db, n).await.unwrap();
        assert!(ticket.assets.is_empty());
        assert_eq!(
            ticket.services.iter().map(|s| s.id).collect::<Vec<_>>(),
            [mail]
        );
        tickets::set_state(&db, n, TicketState::Closed)
            .await
            .unwrap();
        assert!(matches!(
            link_service(&db, n, mail, false).await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn a_service_runs_on_assets_of_any_customer(db: PgPool) {
        let own = testing::help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let server = testing::asset(&db, own, "server", "mail-01").await;
        let nas = testing::asset(&db, brightfield, "storage", "NAS").await;
        let backup = create_service(&db, "Managed backup", "").await.unwrap();
        runs_on(&db, backup, server, true).await.unwrap();
        runs_on(&db, backup, nas, true).await.unwrap();
        assert_eq!(service_detail(&db, backup).await.unwrap().runs_on.len(), 2);
        assert!(matches!(
            create_service(&db, "Managed backup", "").await,
            Err(Error::Rule(_))
        ));
    }
}
