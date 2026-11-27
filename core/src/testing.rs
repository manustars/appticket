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

/// What a fake add-on does when asked for an asset's live details.
#[derive(Clone)]
pub enum Answer {
    /// 200 with this body.
    Json(serde_json::Value),
    /// After this long, 200 with this body.
    Late(std::time::Duration, serde_json::Value),
    Status(axum::http::StatusCode),
    /// 200 with a body that is not JSON.
    Garbage,
}

/// The references a fake add-on was asked for, with the headers of each request.
pub type Asked = std::sync::Arc<std::sync::Mutex<Vec<(String, axum::http::HeaderMap)>>>;

/// A fake add-on on a local port that answers every `GET /assets/{reference}` as `answer` says
/// for that reference. Returns its URL and what it was asked.
pub async fn fake_addon(
    answer: impl Fn(&str) -> Answer + Clone + Send + Sync + 'static,
) -> (String, Asked) {
    use axum::response::IntoResponse;
    let asked = Asked::default();
    let app = axum::Router::new().route(
        "/assets/{reference}",
        axum::routing::get({
            let asked = asked.clone();
            move |axum::extract::Path(reference): axum::extract::Path<String>,
                  headers: axum::http::HeaderMap| async move {
                asked.lock().unwrap().push((reference.clone(), headers));
                match answer(&reference) {
                    Answer::Json(body) => axum::Json(body).into_response(),
                    Answer::Late(wait, body) => {
                        tokio::time::sleep(wait).await;
                        axum::Json(body).into_response()
                    }
                    Answer::Status(status) => status.into_response(),
                    Answer::Garbage => "<html>not live details</html>".into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, asked)
}

/// An add-on called `name` at `url`, whose token may write the assets it supplies.
pub async fn addon_at(db: &PgPool, name: &str, url: &str) -> crate::addons::Registered {
    crate::addons::register(db, name, url, &[], &[crate::addons::CATALOG_WRITE.into()])
        .await
        .unwrap()
}

/// An asset of `customer` called `name` that `addon` supplied with `reference`.
pub async fn supplied_asset(
    db: &PgPool,
    customer: Uuid,
    name: &str,
    addon: &str,
    reference: &str,
) -> Uuid {
    let asset = asset(db, customer, "Server", name).await;
    sqlx::query("UPDATE assets SET source_addon = $2, source_ref = $3 WHERE id = $1")
        .bind(asset)
        .bind(addon)
        .bind(reference)
        .execute(db)
        .await
        .unwrap();
    asset
}

/// Live details as a fake add-on answers them: a warning, two fields (one label only in
/// English, as a single string) and a link.
pub fn live_details_answer() -> serde_json::Value {
    serde_json::json!({
        "state": "warning",
        "fields": [
            {"label": {"en": "Disk /var", "it": "Disco /var"}, "value": "91% full"},
            {"label": "Uptime", "value": "12 days"}
        ],
        "links": [
            {"label": {"en": "Open in Watchtower"},
             "url": "https://watchtower.example.com/hosts/1042"}
        ]
    })
}
