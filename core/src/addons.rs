//! Add-ons (ADR 0005): their registration, the events the core records and delivers to them,
//! signed with a secret shared with each add-on, and the tokens they use to call the core.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

/// Event kinds the core records.
pub const EVENT_KINDS: &[&str] = &[MESSAGE_RECEIVED, TICKET_OPENED, TICKET_STATE_CHANGED];
/// A message arrived and was routed to a ticket or to the inbox.
pub const MESSAGE_RECEIVED: &str = "message.received";
/// An agent opened a ticket, by hand or from an inbox message, or an add-on opened one.
pub const TICKET_OPENED: &str = "ticket.opened";
/// A ticket's state changed: [`crate::tickets::TicketStateChanged`].
pub const TICKET_STATE_CHANGED: &str = "ticket.state_changed";

/// What an add-on's token may do.
pub const SCOPES: &[&str] = &[
    DIRECTORY_READ,
    TICKETS_READ,
    PROPOSALS_WRITE,
    CATALOG_WRITE,
    TICKETS_WRITE,
    NAMES_WRITE,
    CONFLICTS_WRITE,
];
/// Read customers and their assets.
pub const DIRECTORY_READ: &str = "directory:read";
/// Read tickets.
pub const TICKETS_READ: &str = "tickets:read";
/// Make proposals.
pub const PROPOSALS_WRITE: &str = "proposals:write";
/// Create, update and retire the assets the add-on is the source of.
pub const CATALOG_WRITE: &str = "catalog:write";
/// Open tickets without a requester, add notes to tickets and resolve the tickets it opened.
pub const TICKETS_WRITE: &str = "tickets:write";
/// Replace the name list of the add-on's system, which customers' external references are
/// picked from.
pub const NAMES_WRITE: &str = "names:write";
/// Replace the list of conflicts the add-on reports, which those who manage add-ons read in its
/// state.
pub const CONFLICTS_WRITE: &str = "conflicts:write";

/// After this many failed attempts a delivery stops being retried.
pub const MAX_ATTEMPTS: i32 = 12;

/// Header names of an event delivery.
pub const HEADER_EVENT_ID: &str = "hd-event-id";
pub const HEADER_TIMESTAMP: &str = "hd-timestamp";
pub const HEADER_SIGNATURE: &str = "hd-signature";

/// The credentials of a new add-on, shown once.
#[derive(Debug)]
pub struct Registered {
    pub id: Uuid,
    /// For the add-on's calls to the core: `Authorization: Bearer <token>`.
    pub token: String,
    /// To check the signature of the events it receives.
    pub event_secret: String,
}

fn random() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub async fn register(
    db: &PgPool,
    name: &str,
    url: &str,
    events: &[String],
    scopes: &[String],
) -> Result<Registered> {
    let parsed = url::Url::parse(url).map_err(|_| Error::Rule(format!("{url:?} is not a URL")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return rule("an add-on's URL must use http or https");
    }
    if let Some(kind) = events.iter().find(|k| !EVENT_KINDS.contains(&k.as_str())) {
        return rule(format!("unknown event kind {kind:?}"));
    }
    if let Some(scope) = scopes.iter().find(|s| !SCOPES.contains(&s.as_str())) {
        return rule(format!("unknown scope {scope:?}"));
    }
    let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM addons WHERE name = $1)")
        .bind(name)
        .fetch_one(db)
        .await?;
    if taken {
        return rule(format!("there is already an add-on called {name}"));
    }
    let registered = Registered {
        id: Uuid::new_v4(),
        token: random(),
        event_secret: random(),
    };
    sqlx::query(
        "INSERT INTO addons (id, name, url, event_secret, token_hash, events, scopes)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(registered.id)
    .bind(name)
    .bind(parsed.as_str().trim_end_matches('/'))
    .bind(&registered.event_secret)
    .bind(token_hash(&registered.token))
    .bind(events)
    .bind(scopes)
    .execute(db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(d) if d.constraint() == Some("addons_name_check") => Error::Rule(
            "an add-on's name is 2 to 40 lowercase letters, digits or dashes, starting with a letter"
                .into(),
        ),
        other => other.into(),
    })?;
    Ok(registered)
}

/// Switches an add-on on or off. While it is off its token is refused and its events wait.
pub async fn set_enabled(db: &PgPool, name: &str, enabled: bool) -> Result<()> {
    let changed = sqlx::query("UPDATE addons SET enabled = $2 WHERE name = $1")
        .bind(name)
        .bind(enabled)
        .execute(db)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(Error::NotFound("add-on"));
    }
    Ok(())
}

/// An add-on as the command line and those who manage add-ons see it; never its secrets.
#[derive(Debug, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct AddonRow {
    pub name: String,
    pub url: String,
    /// Event kinds it receives.
    pub events: Vec<String>,
    /// What its token may do.
    pub scopes: Vec<String>,
    pub enabled: bool,
    /// Events not yet delivered.
    pub pending: i64,
    /// Undelivered events whose delivery has failed at least once.
    pub failing: i64,
    /// Why the latest failed delivery failed.
    pub last_error: Option<String>,
}

/// An add-on with its state, as those who manage add-ons see it: its deliveries, the names of its
/// system no external reference covers and the conflicts it reports.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AddonState {
    #[serde(flatten)]
    pub addon: AddonRow,
    /// The name list the add-on last sent, with the names no external reference covers; none
    /// until it sends one.
    pub names: Option<crate::name_lists::NameListState>,
    /// The conflicts the add-on last reported; none until it reports any.
    pub conflicts: Option<crate::conflicts::ConflictList>,
}

/// Every add-on with its deliveries.
pub async fn list(db: &PgPool) -> Result<Vec<AddonRow>> {
    Ok(sqlx::query_as(
        "SELECT a.name, a.url, a.events, a.scopes, a.enabled,
                (SELECT count(*) FROM event_deliveries d
                 WHERE d.addon_id = a.id AND d.delivered_at IS NULL) AS pending,
                (SELECT count(*) FROM event_deliveries d
                 WHERE d.addon_id = a.id AND d.delivered_at IS NULL AND d.attempts > 0) AS failing,
                (SELECT d.last_error FROM event_deliveries d
                 WHERE d.addon_id = a.id AND d.delivered_at IS NULL AND d.last_error IS NOT NULL
                 ORDER BY d.next_attempt_at DESC NULLS LAST LIMIT 1) AS last_error
         FROM addons a ORDER BY a.name",
    )
    .fetch_all(db)
    .await?)
}

/// Every add-on with its deliveries and its state.
pub async fn states(db: &PgPool) -> Result<Vec<AddonState>> {
    let addons = list(db).await?;
    let mut names = crate::name_lists::states(db).await?;
    let mut conflicts = crate::conflicts::lists(db).await?;
    Ok(addons
        .into_iter()
        .map(|addon| AddonState {
            names: names.remove(&addon.name),
            conflicts: conflicts.remove(&addon.name),
            addon,
        })
        .collect())
}

/// Records an event in the caller's transaction, for every add-on that receives its kind.
pub async fn emit(tx: &mut Transaction<'_, Postgres>, kind: &str, data: Value) -> Result<()> {
    debug_assert!(EVENT_KINDS.contains(&kind), "unknown event kind {kind}");
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO events (id, kind, data) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(kind)
        .bind(&data)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO event_deliveries (event_id, addon_id)
         SELECT $1, id FROM addons WHERE $2 = ANY(events)",
    )
    .bind(id)
    .bind(kind)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The body of an event delivery.
#[derive(Debug, Serialize)]
pub struct Envelope<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub data: &'a Value,
}

/// `sha256=<hex>` of HMAC-SHA-256 over `<timestamp>.<body>` with the add-on's secret.
pub fn signature(secret: &str, timestamp: u64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// The client events are delivered with; an add-on's URL may redirect.
pub fn http_client() -> anyhow::Result<reqwest::Client> {
    crate::http_client::http_client(reqwest::redirect::Policy::default())
}

/// Delivers due events every `interval`, forever.
pub async fn deliver(db: PgPool, client: reqwest::Client, interval: Duration) {
    loop {
        match deliver_due(&db, &client).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(events = n, "delivered events to add-ons"),
            Err(error) => tracing::warn!(error = format!("{error:#}"), "delivering events failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(sqlx::FromRow)]
struct Due {
    event_id: Uuid,
    addon_id: Uuid,
    addon: String,
    url: String,
    event_secret: String,
    kind: String,
    data: Value,
    created_at: OffsetDateTime,
    attempts: i32,
}

/// Delivers every due event to enabled add-ons, one transaction each. Returns how many arrived.
pub async fn deliver_due(db: &PgPool, client: &reqwest::Client) -> anyhow::Result<usize> {
    let mut delivered = 0;
    loop {
        let mut tx = db.begin().await?;
        let due: Option<Due> = sqlx::query_as(
            "SELECT d.event_id, d.addon_id, a.name AS addon, a.url, a.event_secret,
                    e.kind, e.data, e.created_at, d.attempts
             FROM event_deliveries d
             JOIN addons a ON a.id = d.addon_id
             JOIN events e ON e.id = d.event_id
             WHERE d.delivered_at IS NULL AND a.enabled AND d.attempts < $1
               AND coalesce(d.next_attempt_at, e.created_at) <= now()
             ORDER BY e.created_at
             LIMIT 1
             FOR UPDATE OF d SKIP LOCKED",
        )
        .bind(MAX_ATTEMPTS)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(due) = due else {
            return Ok(delivered);
        };
        let body = serde_json::to_vec(&Envelope {
            id: due.event_id,
            kind: &due.kind,
            created_at: due.created_at,
            data: &due.data,
        })?;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let outcome = client
            .post(format!("{}/events", due.url))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(HEADER_EVENT_ID, due.event_id.to_string())
            .header(HEADER_TIMESTAMP, timestamp.to_string())
            .header(
                HEADER_SIGNATURE,
                signature(&due.event_secret, timestamp, &body),
            )
            .body(body)
            .send()
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| {
                if r.status().is_success() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("the add-on answered {}", r.status()))
                }
            });
        match outcome {
            Ok(()) => {
                sqlx::query(
                    "UPDATE event_deliveries SET delivered_at = now(), attempts = attempts + 1,
                            last_error = NULL
                     WHERE event_id = $1 AND addon_id = $2",
                )
                .bind(due.event_id)
                .bind(due.addon_id)
                .execute(&mut *tx)
                .await?;
                delivered += 1;
            }
            Err(error) => {
                let attempts = due.attempts + 1;
                tracing::warn!(addon = %due.addon, event = %due.event_id, attempts,
                    error = format!("{error:#}"), "could not deliver an event");
                let delay = 30.0 * 2f64.powi((attempts - 1).clamp(0, 7)).min(120.0);
                sqlx::query(
                    "UPDATE event_deliveries SET attempts = $3, last_error = $4,
                            next_attempt_at = now() + make_interval(secs => $5)
                     WHERE event_id = $1 AND addon_id = $2",
                )
                .bind(due.event_id)
                .bind(due.addon_id)
                .bind(attempts)
                .bind(format!("{error:#}"))
                .bind(delay.min(3600.0))
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
    }
}

/// An add-on calling the core with its token.
#[derive(Debug, Clone)]
pub struct Caller {
    pub id: Uuid,
    pub name: String,
    pub scopes: Vec<String>,
}

impl Caller {
    /// Fails unless the add-on's token has `scope`.
    pub fn require(&self, scope: &str) -> Result<()> {
        if self.scopes.iter().any(|s| s == scope) {
            Ok(())
        } else {
            rule(format!("the add-on {} lacks the scope {scope}", self.name))
        }
    }
}

/// The enabled add-on a token belongs to.
pub async fn caller(db: &PgPool, token: &str) -> Result<Option<Caller>> {
    let row: Option<(Uuid, String, Vec<String>)> =
        sqlx::query_as("SELECT id, name, scopes FROM addons WHERE token_hash = $1 AND enabled")
            .bind(token_hash(token))
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(id, name, scopes)| Caller { id, name, scopes }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_signature_covers_timestamp_and_body() {
        let a = signature("secret", 1_700_000_000, b"{}");
        assert!(a.starts_with("sha256=") && a.len() == 7 + 64);
        assert_ne!(a, signature("secret", 1_700_000_001, b"{}"));
        assert_ne!(a, signature("secret", 1_700_000_000, b"{ }"));
        assert_ne!(a, signature("other", 1_700_000_000, b"{}"));
    }

    #[sqlx::test]
    async fn names_kinds_and_urls_are_checked(db: PgPool) {
        let kinds = vec![MESSAGE_RECEIVED.to_owned()];
        assert!(
            register(&db, "Assistant", "http://127.0.0.1:1", &kinds, &[])
                .await
                .is_err()
        );
        assert!(
            register(&db, "assistant", "ftp://x.test", &kinds, &[])
                .await
                .is_err()
        );
        assert!(
            register(
                &db,
                "assistant",
                "http://x.test",
                &["ticket.eaten".into()],
                &[]
            )
            .await
            .is_err()
        );
        register(&db, "assistant", "http://x.test/", &kinds, &[])
            .await
            .unwrap();
        assert!(
            register(&db, "assistant", "http://y.test", &kinds, &[])
                .await
                .is_err()
        );
        assert_eq!(list(&db).await.unwrap()[0].url, "http://x.test");
    }

    /// Runs a fake add-on on a local port, emits an event, and checks it arrives signed; then
    /// checks that a switched-off add-on's events wait instead of being attempted.
    #[sqlx::test]
    async fn events_arrive_signed_and_wait_while_the_addon_is_off(db: PgPool) {
        type Seen = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;
        let seen: Seen = Arc::default();
        let app = Router::new().route(
            "/events",
            post({
                let seen = seen.clone();
                move |headers: HeaderMap, body: Bytes| async move {
                    seen.lock().unwrap().push((headers, body));
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let addon = register(&db, "assistant", &url, &[MESSAGE_RECEIVED.into()], &[])
            .await
            .unwrap();
        let mut tx = db.begin().await.unwrap();
        emit(
            &mut tx,
            MESSAGE_RECEIVED,
            serde_json::json!({"subject": "Printer jams"}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let client = http_client().unwrap();
        assert_eq!(deliver_due(&db, &client).await.unwrap(), 1);
        let (headers, body) = seen.lock().unwrap().pop().unwrap();
        let timestamp: u64 = headers[HEADER_TIMESTAMP].to_str().unwrap().parse().unwrap();
        assert_eq!(
            headers[HEADER_SIGNATURE].to_str().unwrap(),
            signature(&addon.event_secret, timestamp, &body)
        );
        let envelope: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope["kind"], MESSAGE_RECEIVED);
        assert_eq!(envelope["data"]["subject"], "Printer jams");

        set_enabled(&db, "assistant", false).await.unwrap();
        assert!(
            caller(&db, &addon.token).await.unwrap().is_none(),
            "token refused while off"
        );
        let mut tx = db.begin().await.unwrap();
        emit(&mut tx, MESSAGE_RECEIVED, serde_json::json!({}))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(deliver_due(&db, &client).await.unwrap(), 0);
        assert_eq!(list(&db).await.unwrap()[0].pending, 1, "the event waits");
        set_enabled(&db, "assistant", true).await.unwrap();
        assert_eq!(deliver_due(&db, &client).await.unwrap(), 1);
        assert!(caller(&db, &addon.token).await.unwrap().is_some());
    }
}
