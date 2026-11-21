//! Incoming mail: reading an IMAP mailbox and routing each message to a ticket or to the inbox.
//!
//! Routing follows docs/domain-rules.md ("Incoming messages"):
//! a reply joins its ticket, a new message from a known requester opens a ticket, and anything
//! else waits in the inbox.

use std::time::Duration;

use anyhow::{Context, bail};
use futures_util::TryStreamExt;
use mail_parser::MessageParser;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// A received e-mail, reduced to what routing and display need.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub rfc_message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub from_address: String,
    pub from_name: Option<String>,
    pub subject: String,
    pub body_text: String,
    pub raw: Vec<u8>,
}

impl Incoming {
    /// Parses a raw RFC 5322 message. Returns `None` when it has no usable sender.
    pub fn parse(raw: &[u8]) -> Option<Incoming> {
        let message = MessageParser::default().parse(raw)?;
        let from = message.from()?.first()?;
        let from_address = from.address()?.trim().to_lowercase();
        match from_address.rsplit_once('@') {
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {}
            _ => return None,
        }
        // PostgreSQL text cannot hold NUL characters.
        let clean = |s: &str| s.replace('\0', "");
        let list = |l: Option<mail_parser::TextList<'_>>| -> Vec<String> {
            l.map(|l| l.iter().map(|s| s.replace('\0', "")).collect())
                .unwrap_or_default()
        };
        Some(Incoming {
            rfc_message_id: message.message_id().map(clean),
            in_reply_to: list(message.in_reply_to()),
            references: list(message.references()),
            from_name: from.name().map(clean),
            from_address: clean(&from_address),
            subject: clean(message.subject().unwrap_or_default().trim()),
            body_text: message.body_text(0).map(|b| clean(&b)).unwrap_or_default(),
            raw: raw.to_vec(),
        })
    }
}

/// Where a message ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routed {
    /// Already stored earlier (same Message-ID).
    Duplicate,
    /// Attached to an existing ticket as a reply.
    Reply { ticket_id: Uuid },
    /// Opened a new ticket.
    NewTicket { ticket_id: Uuid },
    /// Waiting in the inbox for an agent.
    Inbox,
}

/// Stores `incoming` and routes it, inside `tx`.
pub async fn route(
    tx: &mut Transaction<'_, Postgres>,
    incoming: &Incoming,
) -> anyhow::Result<Routed> {
    if let Some(id) = &incoming.rfc_message_id {
        let seen: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM messages WHERE rfc_message_id = $1)")
                .bind(id)
                .fetch_one(&mut **tx)
                .await?;
        if seen {
            return Ok(Routed::Duplicate);
        }
    }

    // 1. Everything from an offboarded customer waits in the inbox, replies included.
    if from_offboarded_customer(tx, &incoming.from_address).await? {
        insert_message(tx, incoming, None).await?;
        return Ok(Routed::Inbox);
    }

    // 2. A reply to a message already on a ticket joins that ticket, unless the ticket is closed.
    let thread: Vec<&str> = incoming
        .in_reply_to
        .iter()
        .chain(&incoming.references)
        .map(String::as_str)
        .collect();
    if !thread.is_empty() {
        let found: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT t.id, t.state FROM messages m JOIN tickets t ON t.id = m.ticket_id
             WHERE m.rfc_message_id = ANY($1)
             ORDER BY m.received_at DESC LIMIT 1",
        )
        .bind(&thread)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some((ticket_id, state)) = found
            && state != "closed"
        {
            insert_message(tx, incoming, Some(ticket_id)).await?;
            // A reply reopens a waiting or resolved ticket, and always counts as an update.
            sqlx::query(
                "UPDATE tickets SET updated_at = now(),
                        state = CASE WHEN state IN ('waiting', 'resolved') THEN 'open' ELSE state END
                 WHERE id = $1",
            )
            .bind(ticket_id)
            .execute(&mut **tx)
            .await?;
            return Ok(Routed::Reply { ticket_id });
        }
    }

    // 3. Identify the sender: a registered requester, or a domain registered on a customer.
    let Some((requester_id, customer_id)) =
        identify_sender(tx, &incoming.from_address, incoming.from_name.as_deref()).await?
    else {
        insert_message(tx, incoming, None).await?;
        return Ok(Routed::Inbox);
    };

    // 4. A new message from a known requester opens a ticket.
    let ticket_id = Uuid::new_v4();
    let title = if incoming.subject.is_empty() {
        "(no subject)"
    } else {
        incoming.subject.as_str()
    };
    sqlx::query(
        "INSERT INTO tickets (id, customer_id, requester_id, title) VALUES ($1, $2, $3, $4)",
    )
    .bind(ticket_id)
    .bind(customer_id)
    .bind(requester_id)
    .bind(title)
    .execute(&mut **tx)
    .await?;
    insert_message(tx, incoming, Some(ticket_id)).await?;
    Ok(Routed::NewTicket { ticket_id })
}

/// Whether the sender is a requester of an offboarded customer or, when not a requester, writes
/// from a domain registered on one.
async fn from_offboarded_customer(
    tx: &mut Transaction<'_, Postgres>,
    address: &str,
) -> anyhow::Result<bool> {
    let domain = address.rsplit_once('@').map(|(_, d)| d).unwrap_or_default();
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(
             (SELECT c.offboarded_at IS NOT NULL FROM requesters r
              JOIN customers c ON c.id = r.customer_id WHERE r.email = $1),
             (SELECT c.offboarded_at IS NOT NULL FROM customer_domains d
              JOIN customers c ON c.id = d.customer_id WHERE d.domain = $2),
             false)",
    )
    .bind(address)
    .bind(domain)
    .fetch_one(&mut **tx)
    .await?)
}

/// Returns the requester and their customer, creating the requester when only the domain is
/// known. Senders of offboarded customers count as unidentified.
async fn identify_sender(
    tx: &mut Transaction<'_, Postgres>,
    address: &str,
    name: Option<&str>,
) -> anyhow::Result<Option<(Uuid, Uuid)>> {
    let known: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT r.id, r.customer_id FROM requesters r JOIN customers c ON c.id = r.customer_id
         WHERE r.email = $1 AND c.offboarded_at IS NULL",
    )
    .bind(address)
    .fetch_optional(&mut **tx)
    .await?;
    if known.is_some() {
        return Ok(known);
    }
    let registered: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM requesters WHERE email = $1)")
            .bind(address)
            .fetch_one(&mut **tx)
            .await?;
    if registered {
        // A requester of an offboarded customer: not identified.
        return Ok(None);
    }

    let Some((_, domain)) = address.rsplit_once('@') else {
        return Ok(None);
    };
    let customer: Option<Uuid> = sqlx::query_scalar(
        "SELECT d.customer_id FROM customer_domains d JOIN customers c ON c.id = d.customer_id
         WHERE d.domain = $1 AND c.offboarded_at IS NULL",
    )
    .bind(domain)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(customer_id) = customer else {
        return Ok(None);
    };
    let requester_id = Uuid::new_v4();
    sqlx::query("INSERT INTO requesters (id, customer_id, email, name) VALUES ($1, $2, $3, $4)")
        .bind(requester_id)
        .bind(customer_id)
        .bind(address)
        .bind(name)
        .execute(&mut **tx)
        .await?;
    Ok(Some((requester_id, customer_id)))
}

async fn insert_message(
    tx: &mut Transaction<'_, Postgres>,
    incoming: &Incoming,
    ticket_id: Option<Uuid>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, ticket_id, direction, rfc_message_id, in_reply_to, \"references\",
                               from_address, from_name, subject, body_text, raw)
         VALUES ($1, $2, 'inbound', $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(Uuid::new_v4())
    .bind(ticket_id)
    .bind(&incoming.rfc_message_id)
    .bind(incoming.in_reply_to.first())
    .bind(&incoming.references)
    .bind(&incoming.from_address)
    .bind(&incoming.from_name)
    .bind(&incoming.subject)
    .bind(&incoming.body_text)
    .bind(&incoming.raw)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Routes one fetched message. A message that cannot be parsed or routed is parked in the inbox
/// as it is, so that one bad message never stops the ones after it.
async fn store(tx: &mut Transaction<'_, Postgres>, uid: i64, raw: &[u8]) -> anyhow::Result<()> {
    if let Some(incoming) = Incoming::parse(raw) {
        let mut attempt = sqlx::Connection::begin(&mut **tx).await?;
        match route(&mut attempt, &incoming).await {
            Ok(routed) => {
                attempt.commit().await?;
                tracing::debug!(uid, ?routed, "routed message");
                return Ok(());
            }
            Err(error) => {
                attempt.rollback().await?;
                tracing::warn!(
                    uid,
                    error = format!("{error:#}"),
                    "could not route a message; parked in the inbox"
                );
            }
        }
    } else {
        tracing::warn!(
            uid,
            "a message without a usable sender; parked in the inbox"
        );
    }
    park(tx, raw).await
}

/// Puts an unreadable message in the inbox with only its raw bytes.
async fn park(tx: &mut Transaction<'_, Postgres>, raw: &[u8]) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, direction, from_address, subject, raw)
         VALUES ($1, 'inbound', '(unreadable)', '(unreadable message)', $2)",
    )
    .bind(Uuid::new_v4())
    .bind(raw)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Connection details of the mailbox the help desk reads.
#[derive(Debug, Clone)]
pub struct MailboxConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub mailbox: String,
}

impl MailboxConfig {
    /// Parses `imap://user:password@host:port/MAILBOX`. Only plain IMAP is supported for now,
    /// which is fine for the local test server and nothing else.
    pub fn from_url(raw: &str) -> anyhow::Result<MailboxConfig> {
        let url = url::Url::parse(raw).context("HD_IMAP_URL is not a valid URL")?;
        if url.scheme() != "imap" {
            bail!("HD_IMAP_URL must use imap:// (TLS is not supported yet)");
        }
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let mailbox = decode(url.path().trim_start_matches('/'));
        Ok(MailboxConfig {
            host: url
                .host_str()
                .context("HD_IMAP_URL has no host")?
                .to_owned(),
            port: url.port().unwrap_or(143),
            user: decode(url.username()),
            password: decode(url.password().unwrap_or_default()),
            mailbox: if mailbox.is_empty() {
                "INBOX".into()
            } else {
                mailbox
            },
        })
    }
}

/// Reads the mailbox every `interval`, forever. Errors are logged and retried on the next round.
pub async fn poll(db: PgPool, config: MailboxConfig, interval: Duration) {
    loop {
        match read_new(&db, &config).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(messages = n, "read new mail"),
            Err(error) => tracing::warn!(error = format!("{error:#}"), "reading mail failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// Fetches the messages that arrived since the last run and routes them. Returns how many.
async fn read_new(db: &PgPool, config: &MailboxConfig) -> anyhow::Result<usize> {
    let tcp = tokio::net::TcpStream::connect((config.host.as_str(), config.port))
        .await
        .context("connecting to the IMAP server")?;
    let mut session = async_imap::Client::new(tcp)
        .login(&config.user, &config.password)
        .await
        .map_err(|(error, _)| error)
        .context("logging in to the IMAP server")?;
    let mailbox = session
        .select(&config.mailbox)
        .await
        .context("selecting the mailbox")?;
    let uidvalidity = i64::from(mailbox.uid_validity.context("server sent no UIDVALIDITY")?);

    let cursor: Option<(i64, i64)> =
        sqlx::query_as("SELECT uidvalidity, last_uid FROM mailbox_cursors WHERE mailbox = $1")
            .bind(&config.mailbox)
            .fetch_optional(db)
            .await?;
    // A changed UIDVALIDITY means the old UIDs are meaningless: read everything again and let
    // the Message-ID check skip what is already stored.
    let last_uid = match cursor {
        Some((validity, last)) if validity == uidvalidity => last,
        _ => 0,
    };

    let mut fetched: Vec<(i64, Vec<u8>)> = session
        .uid_fetch(format!("{}:*", last_uid + 1), "(UID BODY.PEEK[])")
        .await?
        .try_filter_map(|f| async move {
            Ok(match (f.uid, f.body()) {
                (Some(uid), Some(body)) => Some((i64::from(uid), body.to_vec())),
                _ => None,
            })
        })
        .try_collect()
        .await?;
    session.logout().await.ok();
    // `N:*` always returns the newest message, even when its UID is below N.
    fetched.retain(|(uid, _)| *uid > last_uid);
    fetched.sort_by_key(|(uid, _)| *uid);

    let count = fetched.len();
    for (uid, raw) in fetched {
        let mut tx = db.begin().await?;
        store(&mut tx, uid, &raw).await?;
        sqlx::query(
            "INSERT INTO mailbox_cursors (mailbox, uidvalidity, last_uid) VALUES ($1, $2, $3)
             ON CONFLICT (mailbox) DO UPDATE SET uidvalidity = $2, last_uid = $3",
        )
        .bind(&config.mailbox)
        .bind(uidvalidity)
        .bind(uid)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(from: &str, id: &str, in_reply_to: Option<&str>) -> Incoming {
        let mut raw = format!(
            "From: Ada Ferrini <{from}>\r\nTo: helpdesk@helpdesk.test\r\nMessage-ID: <{id}>\r\nSubject: PC will not start\r\n"
        );
        if let Some(parent) = in_reply_to {
            raw.push_str(&format!("In-Reply-To: <{parent}>\r\n"));
        }
        raw.push_str("\r\nIt shows a black screen.\r\n");
        Incoming::parse(raw.as_bytes()).expect("test mail parses")
    }

    async fn customer(db: &PgPool, name: &str, domain: Option<&str>) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO customers (id, name) VALUES ($1, $2)")
            .bind(id)
            .bind(name)
            .execute(db)
            .await
            .unwrap();
        if let Some(domain) = domain {
            sqlx::query("INSERT INTO customer_domains (domain, customer_id) VALUES ($1, $2)")
                .bind(domain)
                .bind(id)
                .execute(db)
                .await
                .unwrap();
        }
        id
    }

    async fn route_one(db: &PgPool, incoming: &Incoming) -> Routed {
        let mut tx = db.begin().await.unwrap();
        let routed = route(&mut tx, incoming).await.unwrap();
        tx.commit().await.unwrap();
        routed
    }

    #[test]
    fn parses_sender_and_headers() {
        let m = mail(
            "Ada@Brightfield.test",
            "a1@brightfield.test",
            Some("p@helpdesk.test"),
        );
        assert_eq!(m.from_address, "ada@brightfield.test");
        assert_eq!(m.from_name.as_deref(), Some("Ada Ferrini"));
        assert_eq!(m.rfc_message_id.as_deref(), Some("a1@brightfield.test"));
        assert_eq!(m.in_reply_to, vec!["p@helpdesk.test"]);
        assert_eq!(m.subject, "PC will not start");
        assert!(m.body_text.contains("black screen"));
    }

    #[sqlx::test]
    async fn unknown_sender_goes_to_the_inbox(db: PgPool) {
        assert_eq!(
            route_one(
                &db,
                &mail("someone@elsewhere.test", "x1@elsewhere.test", None)
            )
            .await,
            Routed::Inbox
        );
        let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(tickets, 0);
    }

    #[sqlx::test]
    async fn known_domain_creates_the_requester_and_opens_a_ticket(db: PgPool) {
        let brightfield = customer(&db, "Brightfield", Some("brightfield.test")).await;
        let Routed::NewTicket { ticket_id } = route_one(
            &db,
            &mail("ada@brightfield.test", "a1@brightfield.test", None),
        )
        .await
        else {
            panic!("expected a new ticket");
        };
        let (customer_id, title): (Uuid, String) =
            sqlx::query_as("SELECT customer_id, title FROM tickets WHERE id = $1")
                .bind(ticket_id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(customer_id, brightfield);
        assert_eq!(title, "PC will not start");
        let requesters: i64 =
            sqlx::query_scalar("SELECT count(*) FROM requesters WHERE customer_id = $1")
                .bind(brightfield)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(requesters, 1);
    }

    #[sqlx::test]
    async fn a_reply_joins_its_ticket_and_reopens_it(db: PgPool) {
        customer(&db, "Brightfield", Some("brightfield.test")).await;
        let Routed::NewTicket { ticket_id } = route_one(
            &db,
            &mail("ada@brightfield.test", "a1@brightfield.test", None),
        )
        .await
        else {
            panic!("expected a new ticket");
        };
        sqlx::query("UPDATE tickets SET state = 'resolved' WHERE id = $1")
            .bind(ticket_id)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(
            route_one(
                &db,
                &mail(
                    "ada@brightfield.test",
                    "a2@brightfield.test",
                    Some("a1@brightfield.test")
                )
            )
            .await,
            Routed::Reply { ticket_id }
        );
        let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = $1")
            .bind(ticket_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "open");
    }

    #[sqlx::test]
    async fn a_reply_to_a_closed_ticket_opens_a_new_one(db: PgPool) {
        customer(&db, "Brightfield", Some("brightfield.test")).await;
        let Routed::NewTicket { ticket_id } = route_one(
            &db,
            &mail("ada@brightfield.test", "a1@brightfield.test", None),
        )
        .await
        else {
            panic!("expected a new ticket");
        };
        sqlx::query("UPDATE tickets SET state = 'closed' WHERE id = $1")
            .bind(ticket_id)
            .execute(&db)
            .await
            .unwrap();
        let routed = route_one(
            &db,
            &mail(
                "ada@brightfield.test",
                "a2@brightfield.test",
                Some("a1@brightfield.test"),
            ),
        )
        .await;
        assert!(matches!(routed, Routed::NewTicket { ticket_id: other } if other != ticket_id));
    }

    #[sqlx::test]
    async fn the_same_message_is_stored_once(db: PgPool) {
        let m = mail("someone@elsewhere.test", "x1@elsewhere.test", None);
        assert_eq!(route_one(&db, &m).await, Routed::Inbox);
        assert_eq!(route_one(&db, &m).await, Routed::Duplicate);
    }

    #[sqlx::test]
    async fn replies_from_offboarded_customers_go_to_the_inbox(db: PgPool) {
        let brightfield = customer(&db, "Brightfield", Some("brightfield.test")).await;
        route_one(
            &db,
            &mail("ada@brightfield.test", "a1@brightfield.test", None),
        )
        .await;
        sqlx::query("UPDATE customers SET offboarded_at = now() WHERE id = $1")
            .bind(brightfield)
            .execute(&db)
            .await
            .unwrap();
        let reply = mail(
            "ada@brightfield.test",
            "a2@brightfield.test",
            Some("a1@brightfield.test"),
        );
        assert_eq!(route_one(&db, &reply).await, Routed::Inbox);
    }

    #[sqlx::test]
    async fn a_message_that_cannot_be_routed_is_parked_and_does_not_block(db: PgPool) {
        customer(&db, "Brightfield", Some("brightfield.test")).await;
        let bad = b"From: <@brightfield.test>\r\nMessage-ID: <bad@brightfield.test>\r\nSubject: x\r\n\r\nbody\r\n";
        let good = b"From: Ada Ferrini <ada@brightfield.test>\r\nMessage-ID: <good@brightfield.test>\r\nSubject: Printer\r\n\r\nIt jams.\r\n";
        let mut tx = db.begin().await.unwrap();
        store(&mut tx, 1, bad).await.unwrap();
        store(&mut tx, 2, good).await.unwrap();
        tx.commit().await.unwrap();
        let parked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM messages WHERE ticket_id IS NULL AND from_address = '(unreadable)'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!((parked, tickets), (1, 1));
    }

    #[sqlx::test]
    async fn a_nul_byte_in_the_body_is_dropped(db: PgPool) {
        customer(&db, "Brightfield", Some("brightfield.test")).await;
        let raw = b"From: Ada Ferrini <ada@brightfield.test>\r\nMessage-ID: <nul@brightfield.test>\r\nSubject: x\r\n\r\nbefore\0after\r\n";
        let mut tx = db.begin().await.unwrap();
        store(&mut tx, 1, raw).await.unwrap();
        tx.commit().await.unwrap();
        let body: String = sqlx::query_scalar("SELECT body_text FROM messages")
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(body.starts_with("beforeafter"));
    }

    #[sqlx::test]
    async fn senders_of_offboarded_customers_go_to_the_inbox(db: PgPool) {
        let brightfield = customer(&db, "Brightfield", Some("brightfield.test")).await;
        route_one(
            &db,
            &mail("ada@brightfield.test", "a1@brightfield.test", None),
        )
        .await;
        sqlx::query("UPDATE customers SET offboarded_at = now() WHERE id = $1")
            .bind(brightfield)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(
            route_one(
                &db,
                &mail("ada@brightfield.test", "a2@brightfield.test", None)
            )
            .await,
            Routed::Inbox
        );
        assert_eq!(
            route_one(
                &db,
                &mail("bruno@brightfield.test", "b1@brightfield.test", None)
            )
            .await,
            Routed::Inbox
        );
    }
}
