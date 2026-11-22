//! Outgoing mail: the help desk's sender identity, and the outbox that delivers queued messages
//! over SMTP, retrying failures with a growing delay.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use sqlx::PgPool;
use uuid::Uuid;

/// After this many failed attempts a message stays in the outbox with its last error.
pub const MAX_ATTEMPTS: i32 = 8;

/// The address the help desk writes from.
#[derive(Debug, Clone)]
pub struct Sender {
    pub address: String,
    pub name: String,
    domain: String,
}

impl Sender {
    pub fn new(address: &str, name: &str) -> anyhow::Result<Sender> {
        let address = address.trim().to_lowercase();
        let Some((local, domain)) = address.rsplit_once('@') else {
            bail!("HD_MAIL_FROM is not an e-mail address: {address}");
        };
        if local.is_empty() || domain.is_empty() {
            bail!("HD_MAIL_FROM is not an e-mail address: {address}");
        }
        let domain = domain.to_owned();
        Ok(Sender {
            address,
            name: name.trim().to_owned(),
            domain,
        })
    }

    /// A new, globally unique Message-ID (without angle brackets).
    pub fn new_message_id(&self) -> String {
        format!("{}@{}", Uuid::new_v4().simple(), self.domain)
    }
}

pub type Transport = AsyncSmtpTransport<Tokio1Executor>;

/// Builds the SMTP transport from `smtps://user:password@host[:port]` (TLS from the start, port
/// 465 by default), `smtp://…?tls=required` (STARTTLS, port 587 by default) or plain `smtp://…`
/// (port 25 by default, only for a server on this machine). `ca_file` adds trusted authorities.
/// Where the outbox records what it does: the server, never the credentials.
pub fn tracker(raw: &str) -> anyhow::Result<crate::mail_status::Tracker> {
    let url = url::Url::parse(raw).context("HD_SMTP_URL is not a valid URL")?;
    let host = url.host_str().context("HD_SMTP_URL has no host")?;
    let server = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let password = percent_encoding::percent_decode_str(url.password().unwrap_or_default())
        .decode_utf8_lossy()
        .into_owned();
    Ok(crate::mail_status::Tracker::new(server, vec![password]))
}

pub fn transport(raw: &str, ca_file: Option<&PathBuf>) -> anyhow::Result<Transport> {
    let url = url::Url::parse(raw).context("HD_SMTP_URL is not a valid URL")?;
    let host = url
        .host_str()
        .context("HD_SMTP_URL has no host")?
        .to_owned();
    let starttls = url
        .query_pairs()
        .any(|(key, value)| key == "tls" && value == "required");
    let tls = || -> anyhow::Result<TlsParameters> {
        let mut builder = TlsParameters::builder(host.clone());
        if let Some(path) = ca_file {
            let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            builder = builder.add_root_certificate(
                Certificate::from_pem(&pem)
                    .with_context(|| format!("parsing {}", path.display()))?,
            );
        }
        builder.build_rustls().context("setting up TLS for SMTP")
    };
    let (security, default_port) = match url.scheme() {
        "smtps" => (Tls::Wrapper(tls()?), 465),
        "smtp" if starttls => (Tls::Required(tls()?), 587),
        "smtp" if crate::imap::is_local(&host) => (Tls::None, 25),
        "smtp" => bail!(
            "HD_SMTP_URL must use smtps:// or smtp://…?tls=required for a server that is not on this machine"
        ),
        other => bail!("HD_SMTP_URL has an unknown scheme {other}://"),
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        .port(url.port().unwrap_or(default_port))
        .tls(security)
        .timeout(Some(Duration::from_secs(30)));
    if !url.username().is_empty() {
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        builder = builder.credentials(Credentials::new(
            decode(url.username()),
            decode(url.password().unwrap_or_default()),
        ));
    }
    Ok(builder.build())
}

/// Sends what is due in the outbox every `interval`, forever.
pub async fn deliver(
    db: PgPool,
    transport: Transport,
    interval: Duration,
    status: crate::mail_status::Tracker,
) {
    loop {
        match deliver_due_tracked(&db, &transport, Some(&status)).await {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(messages = n, "sent mail");
                status.succeeded();
            }
            Err(error) => {
                tracing::warn!(error = format!("{error:#}"), "sending mail failed");
                status.failed(&error);
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(sqlx::FromRow)]
struct Queued {
    id: Uuid,
    rfc_message_id: Option<String>,
    in_reply_to: Option<String>,
    references: Vec<String>,
    from_address: String,
    from_name: Option<String>,
    to_address: Option<String>,
    subject: String,
    body_text: String,
    send_attempts: i32,
}

/// Sends every due message, one transaction each, so that several cores can share the outbox.
/// Returns how many were sent.
#[cfg(test)]
pub async fn deliver_due(db: &PgPool, transport: &Transport) -> anyhow::Result<usize> {
    deliver_due_tracked(db, transport, None).await
}

/// Like [`deliver_due`], recording each message that could not be sent in `status`.
async fn deliver_due_tracked(
    db: &PgPool,
    transport: &Transport,
    status: Option<&crate::mail_status::Tracker>,
) -> anyhow::Result<usize> {
    let mut sent = 0;
    loop {
        let mut tx = db.begin().await?;
        let queued: Option<Queued> = sqlx::query_as(
            r#"SELECT id, rfc_message_id, in_reply_to, "references", from_address, from_name,
                      to_address, subject, body_text, send_attempts
               FROM messages
               WHERE direction = 'outbound' AND sent_at IS NULL AND send_attempts < $1
                 AND coalesce(next_attempt_at, created_at) <= now()
               ORDER BY created_at
               LIMIT 1
               FOR UPDATE SKIP LOCKED"#,
        )
        .bind(MAX_ATTEMPTS)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(queued) = queued else {
            return Ok(sent);
        };
        let outcome = match build(&queued) {
            Ok(message) => transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from),
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => {
                sqlx::query(
                    "UPDATE messages SET sent_at = now(), last_error = NULL,
                            send_attempts = send_attempts + 1
                     WHERE id = $1",
                )
                .bind(queued.id)
                .execute(&mut *tx)
                .await?;
                sent += 1;
            }
            Err(error) => {
                let attempts = queued.send_attempts + 1;
                tracing::warn!(message = %queued.id, attempts, error = format!("{error:#}"), "could not send a message");
                if let Some(status) = status {
                    status.failed(&error);
                }
                sqlx::query(
                    "UPDATE messages SET send_attempts = $2, last_error = $3,
                            next_attempt_at = now() + make_interval(secs => $4)
                     WHERE id = $1",
                )
                .bind(queued.id)
                .bind(attempts)
                .bind(format!("{error:#}"))
                .bind(retry_delay(attempts).as_secs_f64())
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
    }
}

/// 30 s after the first failure, doubling up to about an hour.
fn retry_delay(attempts: i32) -> Duration {
    Duration::from_secs(30 * 2u64.pow(attempts.clamp(1, 8) as u32 - 1))
        .min(Duration::from_secs(3600))
}

fn build(queued: &Queued) -> anyhow::Result<Message> {
    let from = Mailbox::new(
        queued.from_name.clone().filter(|n| !n.is_empty()),
        queued.from_address.parse().context("the sender address")?,
    );
    let to: Mailbox = queued
        .to_address
        .as_deref()
        .context("the message has no recipient")?
        .parse()
        .context("the recipient address")?;
    let mut builder = Message::builder()
        .from(from)
        .to(to)
        .subject(&queued.subject)
        .message_id(queued.rfc_message_id.as_ref().map(|id| format!("<{id}>")))
        .header(ContentType::TEXT_PLAIN);
    if let Some(parent) = &queued.in_reply_to {
        builder = builder.in_reply_to(format!("<{parent}>"));
    }
    if !queued.references.is_empty() {
        let chain: Vec<String> = queued
            .references
            .iter()
            .map(|id| format!("<{id}>"))
            .collect();
        builder = builder.references(chain.join(" "));
    }
    builder
        .body(queued.body_text.clone())
        .context("building the message")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::tickets::{self, TicketState};

    #[test]
    fn plain_smtp_is_only_for_this_machine() {
        assert!(transport("smtp://127.0.0.1:55025", None).is_ok());
        assert!(transport("smtp://mail.example.com", None).is_err());
        assert!(transport("smtp://u:p@mail.example.com?tls=required", None).is_ok());
        assert!(transport("smtps://u:p@mail.example.com", None).is_ok());
    }

    #[test]
    fn retries_slow_down_and_stop_growing() {
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_secs(60));
        assert_eq!(retry_delay(8), Duration::from_secs(3600));
    }

    #[test]
    fn a_reply_carries_its_threading_headers() {
        let queued = Queued {
            id: Uuid::new_v4(),
            rfc_message_id: Some("out-1@helpdesk.test".into()),
            in_reply_to: Some("first@brightfield.test".into()),
            references: vec![
                "first@brightfield.test".into(),
                "second@brightfield.test".into(),
            ],
            from_address: "helpdesk@helpdesk.test".into(),
            from_name: Some("Help desk".into()),
            to_address: Some("ada@brightfield.test".into()),
            subject: "Re: Printer jams [#7]".into(),
            body_text: "Please restart it.".into(),
            send_attempts: 0,
        };
        let raw = String::from_utf8(build(&queued).unwrap().formatted()).unwrap();
        for expected in [
            "From: \"Help desk\" <helpdesk@helpdesk.test>",
            "To: ada@brightfield.test",
            "Subject: Re: Printer jams [#7]",
            "Message-ID: <out-1@helpdesk.test>",
            "In-Reply-To: <first@brightfield.test>",
            "References: <first@brightfield.test> <second@brightfield.test>",
        ] {
            assert!(raw.contains(expected), "missing {expected:?} in\n{raw}");
        }
    }

    /// Sends a queued reply through the local test mail server and checks the outbox records it;
    /// then checks that a failure is recorded for a retry.
    #[sqlx::test]
    #[ignore = "needs the local test mail server (podman compose up -d)"]
    async fn the_outbox_sends_and_records_failures(db: PgPool) {
        let c = testing::customer(&db, "Brightfield").await;
        let r = testing::requester(&db, c, "ada@brightfield.test").await;
        let a = testing::agent(&db, "marco@northgate.test").await;
        let n = testing::ticket(&db, c, Some(r), "first@brightfield.test").await;
        tickets::reply(
            &db,
            &testing::sender(),
            n,
            a,
            "Restart it.",
            TicketState::Waiting,
        )
        .await
        .unwrap();

        let working = transport("smtp://127.0.0.1:55025", None).unwrap();
        assert_eq!(deliver_due(&db, &working).await.unwrap(), 1);
        assert_eq!(deliver_due(&db, &working).await.unwrap(), 0, "nothing left");

        tickets::reply(
            &db,
            &testing::sender(),
            n,
            a,
            "Again.",
            TicketState::Waiting,
        )
        .await
        .unwrap();
        // Nothing listens on port 9 of this machine.
        let broken = transport("smtp://127.0.0.1:9", None).unwrap();
        let status = tracker("smtp://127.0.0.1:9").unwrap();
        assert_eq!(
            deliver_due_tracked(&db, &broken, Some(&status))
                .await
                .unwrap(),
            0
        );
        assert!(
            status.snapshot().last_error.is_some(),
            "the failure is recorded"
        );
        let (attempts, error, later): (i32, Option<String>, bool) = sqlx::query_as(
            "SELECT send_attempts, last_error, next_attempt_at > now() FROM messages
             WHERE direction = 'outbound' AND sent_at IS NULL",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(attempts, 1);
        assert!(error.is_some());
        assert!(later, "the retry waits");
    }
}
