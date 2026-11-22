//! Fixtures for database tests.

use sqlx::PgPool;
use uuid::Uuid;

use crate::outbound::Sender;

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

/// An asset of the customer, of the named kind.
pub async fn asset(db: &PgPool, customer: Uuid, kind: &str, name: &str) -> Uuid {
    let kind = asset_kind(db, kind).await;
    crate::catalog::create_asset(db, customer, kind, name)
        .await
        .unwrap()
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
