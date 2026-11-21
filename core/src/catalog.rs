//! What tickets refer to: assets customers own and services the help desk provides (ADR 0002,
//! ADR 0003). Rules are in docs/domain-rules.md ("Customers, assets and services").

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Asset {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    /// What sort of thing it is: computer, printer, application, licence…
    pub kind: String,
    pub name: String,
    /// The add-on it came from, when an add-on supplies it.
    pub source_addon: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
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
    Ok(sqlx::query_as(
        "SELECT a.id, a.customer_id, c.name AS customer_name, a.kind, a.name,
                a.source_addon, a.retired_at
         FROM assets a JOIN customers c ON c.id = a.customer_id
         WHERE a.customer_id = $1
         ORDER BY a.retired_at IS NOT NULL, a.kind, a.name",
    )
    .bind(customer)
    .fetch_all(db)
    .await?)
}

pub async fn create_asset(db: &PgPool, customer: Uuid, kind: &str, name: &str) -> Result<Uuid> {
    let (kind, name) = (text(kind, "kind")?, text(name, "name")?);
    let offboarded: Option<bool> =
        sqlx::query_scalar("SELECT offboarded_at IS NOT NULL FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(db)
            .await?;
    match offboarded {
        None => return Err(Error::NotFound("customer")),
        Some(true) => return rule("the customer is offboarded"),
        Some(false) => {}
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO assets (id, customer_id, kind, name) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(customer)
        .bind(kind)
        .bind(name)
        .execute(db)
        .await?;
    Ok(id)
}

pub async fn update_asset(db: &PgPool, id: Uuid, kind: &str, name: &str) -> Result<()> {
    let (kind, name) = (text(kind, "kind")?, text(name, "name")?);
    let updated = sqlx::query("UPDATE assets SET kind = $2, name = $3 WHERE id = $1")
        .bind(id)
        .bind(kind)
        .bind(name)
        .execute(db)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(Error::NotFound("asset"));
    }
    Ok(())
}

/// Retires an asset: it stays on the tickets that refer to it but gets no new links.
pub async fn retire_asset(db: &PgPool, id: Uuid) -> Result<()> {
    let retired: Option<bool> =
        sqlx::query_scalar("SELECT retired_at IS NOT NULL FROM assets WHERE id = $1")
            .bind(id)
            .fetch_optional(db)
            .await?;
    match retired {
        None => Err(Error::NotFound("asset")),
        Some(true) => rule("the asset is already retired"),
        Some(false) => {
            sqlx::query("UPDATE assets SET retired_at = now() WHERE id = $1")
                .bind(id)
                .execute(db)
                .await?;
            Ok(())
        }
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
    let runs_on = sqlx::query_as(
        "SELECT a.id, a.customer_id, c.name AS customer_name, a.kind, a.name,
                a.source_addon, a.retired_at
         FROM assets a JOIN customers c ON c.id = a.customer_id
         JOIN service_assets sa ON sa.asset_id = a.id
         WHERE sa.service_id = $1 ORDER BY a.name",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(ServiceDetail {
        service,
        used_by,
        runs_on,
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

/// Before `tickets` move to `customer`: removes their links that would not fit there (every
/// asset, since assets belong to the old customer, and services the new customer does not use).
/// Returns how many links were removed.
pub async fn unlink_for_move(
    tx: &mut Transaction<'_, Postgres>,
    tickets: &[Uuid],
    customer: Uuid,
) -> Result<i64> {
    let assets = sqlx::query("DELETE FROM ticket_assets WHERE ticket_id = ANY($1)")
        .bind(tickets)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    let services = sqlx::query(
        "DELETE FROM ticket_services ts WHERE ts.ticket_id = ANY($1)
           AND NOT EXISTS (SELECT 1 FROM customer_services cs
                           WHERE cs.customer_id = $2 AND cs.service_id = ts.service_id)",
    )
    .bind(tickets)
    .bind(customer)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    // Services both customers use stay linked; the foreign keys carry them to the new customer.
    Ok((assets + services) as i64)
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct LinkedAsset {
    pub id: Uuid,
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
        "SELECT a.id, a.kind, a.name FROM assets a JOIN ticket_assets ta ON ta.asset_id = a.id
         WHERE ta.ticket_id = $1 ORDER BY a.kind, a.name",
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

    async fn help_desk(db: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO customers (id, name, is_help_desk) VALUES ($1, 'Northgate', true)",
        )
        .bind(id)
        .execute(db)
        .await
        .unwrap();
        id
    }

    #[sqlx::test]
    async fn a_ticket_links_only_assets_of_its_customer(db: PgPool) {
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let pc = create_asset(&db, brightfield, "computer", "PC-0042")
            .await
            .unwrap();
        let other = create_asset(&db, harbor, "printer", "Printer 2F")
            .await
            .unwrap();
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
        let own = help_desk(&db).await;
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

    #[sqlx::test]
    async fn moving_a_ticket_drops_links_that_do_not_fit(db: PgPool) {
        help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let harbor = testing::customer(&db, "Harbor Lane").await;
        let mail = create_service(&db, "Hosted mail", "").await.unwrap();
        let backup = create_service(&db, "Backup", "").await.unwrap();
        for customer in [brightfield, harbor] {
            use_service(&db, mail, customer).await.unwrap();
        }
        use_service(&db, backup, brightfield).await.unwrap();
        let pc = create_asset(&db, brightfield, "computer", "PC-0042")
            .await
            .unwrap();
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
        let own = help_desk(&db).await;
        let brightfield = testing::customer(&db, "Brightfield").await;
        let server = create_asset(&db, own, "server", "mail-01").await.unwrap();
        let nas = create_asset(&db, brightfield, "storage", "NAS")
            .await
            .unwrap();
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
