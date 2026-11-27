//! Fixtures for database tests.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::outbound::Sender;

/// The sentence of a refusal by a rule, failing the test on anything else.
pub fn refusal<T: std::fmt::Debug>(result: Result<T>) -> String {
    match result {
        Err(Error::Rule(why)) => why,
        other => panic!("not refused by a rule: {other:?}"),
    }
}

/// The help desk's own organization.
pub async fn help_desk(db: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO customers (id, name, is_help_desk) VALUES ($1, 'Northgate', true)")
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    id
}

pub async fn customer(db: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO customers (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(name)
        .execute(db)
        .await
        .unwrap();
    id
}

/// The asset kind with that name, added (staying at its site) when the list does not have it.
pub async fn asset_kind(db: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO asset_kinds (id, name) VALUES ($1, $2)
         ON CONFLICT (lower(name)) DO UPDATE SET name = asset_kinds.name
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .fetch_one(db)
    .await
    .unwrap()
}

/// An add-on called `name`, whose token may write the assets it supplies.
pub async fn addon(db: &PgPool, name: &str) -> crate::addons::Registered {
    crate::addons::register(
        db,
        name,
        "http://127.0.0.1:9",
        &[],
        &[crate::addons::CATALOG_WRITE.into()],
    )
    .await
    .unwrap()
}

/// An asset of the customer, of the named kind.
pub async fn asset(db: &PgPool, customer: Uuid, kind: &str, name: &str) -> Uuid {
    let kind = asset_kind(db, kind).await;
    crate::catalog::create_asset(db, customer, &asset_fields(kind, name))
        .await
        .unwrap()
}

/// Whether some connection to the test's database comes to wait for a lock it has not been
/// granted, within ten seconds.
pub async fn some_statement_waits_for_a_lock(db: &PgPool) -> bool {
    for _ in 0..200 {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
                            WHERE NOT l.granted AND a.datname = current_database())",
        )
        .fetch_one(db)
        .await
        .unwrap();
        if waiting {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// An asset of the customer, of the named kind, given to `holder`.
pub async fn held_asset(db: &PgPool, customer: Uuid, kind: &str, name: &str, holder: Uuid) -> Uuid {
    let kind = asset_kind(db, kind).await;
    let fields = crate::catalog::AssetFields {
        holder_id: Some(holder),
        ..asset_fields(kind, name)
    };
    crate::catalog::create_asset(db, customer, &fields)
        .await
        .unwrap()
}

/// An asset of `kind` called `name`, with no serial number, site or holder.
pub fn asset_fields(kind: Uuid, name: &str) -> crate::catalog::AssetFields {
    asset_details(kind, name, None, None, None)
}

/// An asset of `kind` called `name`, with the serial number, site and holder given.
pub fn asset_details(
    kind: Uuid,
    name: &str,
    serial_number: Option<&str>,
    site_id: Option<Uuid>,
    holder_id: Option<Uuid>,
) -> crate::catalog::AssetFields {
    crate::catalog::AssetFields {
        kind_id: kind,
        name: name.to_owned(),
        serial_number: serial_number.map(str::to_owned),
        site_id,
        holder_id,
    }
}

/// A requester with one address.
pub async fn requester(db: &PgPool, customer: Uuid, email: &str) -> Uuid {
    let mut tx = db.begin().await.unwrap();
    let id = crate::requesters::create(&mut tx, customer, email, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

/// A message received on the ticket from `from`.
pub async fn message_from(db: &PgPool, number: i64, from: &str) {
    sqlx::query(
        "INSERT INTO messages (id, ticket_id, direction, from_address, subject, body_text)
         SELECT $1, id, 'inbound', $3, 'Printer jams', 'Still jams.' FROM tickets WHERE number = $2",
    )
    .bind(Uuid::new_v4())
    .bind(number)
    .bind(from)
    .execute(db)
    .await
    .unwrap();
}

pub async fn agent(db: &PgPool, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, email, name, password_hash, role_id)
         VALUES ($1, $2, $3, 'x', (SELECT id FROM roles WHERE is_administrator))",
    )
    .bind(id)
    .bind(email)
    .bind(email.split('@').next().unwrap())
    .execute(db)
    .await
    .unwrap();
    id
}

/// A ticket with one received message; returns its number.
pub async fn ticket(db: &PgPool, customer: Uuid, requester: Option<Uuid>, message_id: &str) -> i64 {
    let id = Uuid::new_v4();
    let number: i64 = sqlx::query_scalar(
        "INSERT INTO tickets (id, customer_id, requester_id, title) VALUES ($1, $2, $3, 'Printer jams')
         RETURNING number",
    )
    .bind(id)
    .bind(customer)
    .bind(requester)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO messages (id, ticket_id, direction, rfc_message_id, from_address, subject, body_text)
         VALUES ($1, $2, 'inbound', $3, 'someone@brightfield.test', 'Printer jams', 'It jams.')",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(message_id)
    .execute(db)
    .await
    .unwrap();
    number
}

/// A message waiting in the inbox; returns its id.
pub async fn inbox_message(db: &PgPool, from: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO messages (id, direction, from_address, subject, body_text)
         VALUES ($1, 'inbound', $2, 'Question', 'Hello.')",
    )
    .bind(id)
    .bind(from)
    .execute(db)
    .await
    .unwrap();
    id
}

pub fn sender() -> Sender {
    Sender::new("helpdesk@helpdesk.test", "Help desk").unwrap()
}

pub async fn state(db: &PgPool, number: i64) -> String {
    sqlx::query_scalar("SELECT state FROM tickets WHERE number = $1")
        .bind(number)
        .fetch_one(db)
        .await
        .unwrap()
}

/// Moves the ticket to `state` as the agent desk@northgate.test, added the first time.
pub async fn set_state(
    db: &PgPool,
    number: i64,
    state: crate::tickets::TicketState,
) -> crate::error::Result<()> {
    let agent: Uuid = sqlx::query_scalar(
        "INSERT INTO agents (id, email, name, password_hash, role_id)
         VALUES ($1, 'desk@northgate.test', 'desk', 'x',
                 (SELECT id FROM roles WHERE is_administrator))
         ON CONFLICT (email) DO UPDATE SET email = agents.email
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .fetch_one(db)
    .await
    .unwrap();
    crate::tickets::set_state(db, number, state, agent).await
}

/// The data of every `ticket.state_changed` event recorded, oldest first and, within one
/// transaction, by ticket number.
pub async fn state_changes(db: &PgPool) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT data FROM events WHERE kind = 'ticket.state_changed'
         ORDER BY created_at, (data->>'ticket_number')::bigint",
    )
    .fetch_all(db)
    .await
    .unwrap()
}
