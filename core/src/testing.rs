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

pub async fn requester(db: &PgPool, customer: Uuid, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO requesters (id, customer_id, email) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(customer)
        .bind(email)
        .execute(db)
        .await
        .unwrap();
    id
}

pub async fn agent(db: &PgPool, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO agents (id, email, name, password_hash) VALUES ($1, $2, $3, 'x')")
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
