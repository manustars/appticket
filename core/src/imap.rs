//! Reading the help desk's mailbox over IMAP, with TLS (`imaps://`) or, on the local machine only,
//! without it (`imap://`).

use std::fmt::Debug;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use futures_util::TryStreamExt;
use sqlx::PgPool;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore};

use crate::mail;

/// Connection details of the mailbox the help desk reads.
#[derive(Debug, Clone)]
pub struct MailboxConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub user: String,
    pub password: String,
    pub mailbox: String,
    /// Extra certificate authorities to trust (PEM), for servers with a private CA.
    pub ca_file: Option<PathBuf>,
    /// Whether the first read imports the mail already in the mailbox; otherwise only mail that
    /// arrives afterwards is read.
    pub import_existing: bool,
}

impl MailboxConfig {
    /// Parses `imaps://user:password@host[:port]/MAILBOX` (TLS, port 993 by default) or
    /// `imap://…` (no TLS, port 143 by default, accepted only for a host on this machine).
    pub fn from_url(raw: &str, ca_file: Option<PathBuf>) -> anyhow::Result<MailboxConfig> {
        let url = url::Url::parse(raw).context("HD_IMAP_URL is not a valid URL")?;
        let host = url
            .host_str()
            .context("HD_IMAP_URL has no host")?
            .to_owned();
        let (tls, default_port) = match url.scheme() {
            "imaps" => (true, 993),
            "imap" if is_local(&host) => (false, 143),
            "imap" => {
                bail!("HD_IMAP_URL must use imaps:// for a server that is not on this machine")
            }
            other => bail!("HD_IMAP_URL has an unknown scheme {other}://"),
        };
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let mailbox = decode(url.path().trim_start_matches('/'));
        Ok(MailboxConfig {
            host,
            port: url.port().unwrap_or(default_port),
            tls,
            user: decode(url.username()),
            password: decode(url.password().unwrap_or_default()),
            mailbox: if mailbox.is_empty() {
                "INBOX".into()
            } else {
                mailbox
            },
            ca_file,
            import_existing: false,
        })
    }
}

pub fn is_local(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Reads the mailbox every `interval`, forever. Errors are logged and retried on the next round.
pub async fn poll(
    db: PgPool,
    config: MailboxConfig,
    interval: Duration,
    status: crate::mail_status::Tracker,
) {
    let tls = match config
        .tls
        .then(|| tls_connector(config.ca_file.as_ref()))
        .transpose()
    {
        Ok(tls) => tls,
        Err(error) => {
            tracing::error!(
                error = format!("{error:#}"),
                "cannot read mail: TLS setup failed"
            );
            status.failed(&error.context("cannot read mail: TLS setup failed"));
            return;
        }
    };
    loop {
        match read_new(&db, &config, tls.as_ref()).await {
            Ok(n) => {
                status.succeeded();
                if n > 0 {
                    tracing::info!(messages = n, "read new mail");
                }
            }
            Err(error) => {
                tracing::warn!(error = format!("{error:#}"), "reading mail failed");
                status.failed(&error);
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// Trusts the public web PKI plus, when given, the authorities in `ca_file`.
fn tls_connector(ca_file: Option<&PathBuf>) -> anyhow::Result<TlsConnector> {
    let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = ca_file {
        let mut added = 0;
        for cert in CertificateDer::pem_file_iter(path)
            .with_context(|| format!("reading {}", path.display()))?
        {
            roots
                .add(cert.with_context(|| format!("parsing {}", path.display()))?)
                .with_context(|| format!("trusting a certificate in {}", path.display()))?;
            added += 1;
        }
        if added == 0 {
            bail!("{} holds no PEM certificate", path.display());
        }
    }
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Fetches the messages that arrived since the last run and routes them. Returns how many.
async fn read_new(
    db: &PgPool,
    config: &MailboxConfig,
    tls: Option<&TlsConnector>,
) -> anyhow::Result<usize> {
    let tcp = TcpStream::connect((config.host.as_str(), config.port))
        .await
        .context("connecting to the IMAP server")?;
    let fetched = match tls {
        Some(tls) => {
            let name = ServerName::try_from(config.host.clone())
                .context("the IMAP host is not a valid TLS server name")?;
            let stream = tls
                .connect(name, tcp)
                .await
                .context("TLS handshake with the IMAP server")?;
            fetch_new(db, config, stream).await?
        }
        None => fetch_new(db, config, tcp).await?,
    };

    if let Some(last_uid) = fetched.skip_to {
        sqlx::query(
            "INSERT INTO mailbox_cursors (mailbox, uidvalidity, last_uid) VALUES ($1, $2, $3)
             ON CONFLICT (mailbox) DO NOTHING",
        )
        .bind(&config.mailbox)
        .bind(fetched.uidvalidity)
        .bind(last_uid)
        .execute(db)
        .await?;
        tracing::info!(
            mailbox = %config.mailbox,
            last_uid,
            "first read: mail already in the mailbox is left alone (set HD_IMAP_IMPORT_EXISTING to import it)"
        );
    }
    let count = fetched.messages.len();
    for (uid, raw) in fetched.messages {
        let mut tx = db.begin().await?;
        mail::store(&mut tx, uid, &raw).await?;
        sqlx::query(
            "INSERT INTO mailbox_cursors (mailbox, uidvalidity, last_uid) VALUES ($1, $2, $3)
             ON CONFLICT (mailbox) DO UPDATE SET uidvalidity = $2, last_uid = $3",
        )
        .bind(&config.mailbox)
        .bind(fetched.uidvalidity)
        .bind(uid)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(count)
}

struct Fetched {
    uidvalidity: i64,
    /// On the first read, without import: the UID the cursor starts from.
    skip_to: Option<i64>,
    /// New messages as (UID, raw bytes), oldest first.
    messages: Vec<(i64, Vec<u8>)>,
}

async fn fetch_new<S>(db: &PgPool, config: &MailboxConfig, stream: S) -> anyhow::Result<Fetched>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Debug,
{
    let mut session = async_imap::Client::new(stream)
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
        Some(_) => 0,
        None if config.import_existing => 0,
        None => {
            // First contact: start after the newest message already there.
            let next = i64::from(mailbox.uid_next.context("server sent no UIDNEXT")?);
            session.logout().await.ok();
            return Ok(Fetched {
                uidvalidity,
                skip_to: Some(next - 1),
                messages: Vec::new(),
            });
        }
    };

    let mut messages: Vec<(i64, Vec<u8>)> = session
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
    messages.retain(|(uid, _)| *uid > last_uid);
    messages.sort_by_key(|(uid, _)| *uid);
    Ok(Fetched {
        uidvalidity,
        skip_to: None,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_imap_is_only_for_this_machine() {
        assert!(MailboxConfig::from_url("imap://u:p@127.0.0.1:55143/INBOX", None).is_ok());
        assert!(MailboxConfig::from_url("imap://u:p@localhost/INBOX", None).is_ok());
        assert!(MailboxConfig::from_url("imap://u:p@[::1]/INBOX", None).is_ok());
        assert!(MailboxConfig::from_url("imap://u:p@mail.example.com/INBOX", None).is_err());
        assert!(MailboxConfig::from_url("pop3://u:p@localhost/", None).is_err());
    }

    #[test]
    fn imaps_defaults_to_port_993_and_decodes_credentials() {
        let c = MailboxConfig::from_url("imaps://help%40example.com:p%2Fw@mail.example.com", None)
            .unwrap();
        assert!(c.tls);
        assert_eq!((c.port, c.mailbox.as_str()), (993, "INBOX"));
        assert_eq!(
            (c.user.as_str(), c.password.as_str()),
            ("help@example.com", "p/w")
        );
    }

    /// The first read of a mailbox leaves the mail already there alone and starts after it.
    #[sqlx::test]
    #[ignore = "needs the local test mail server (podman compose up -d)"]
    async fn the_first_read_skips_existing_mail(db: PgPool) {
        let config =
            MailboxConfig::from_url("imap://helpdesk:helpdesk@127.0.0.1:55143/INBOX", None)
                .unwrap();
        assert_eq!(read_new(&db, &config, None).await.unwrap(), 0);
        let messages: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
            .fetch_one(&db)
            .await
            .unwrap();
        let cursors: i64 = sqlx::query_scalar("SELECT count(*) FROM mailbox_cursors")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!((messages, cursors), (0, 1));
    }

    /// Puts a TLS terminator with a freshly made certificate in front of the local test mail
    /// server, then reads it over `imaps://`: once trusting that certificate (works) and once
    /// without it (refused).
    #[sqlx::test]
    #[ignore = "needs the local test mail server (podman compose up -d)"]
    async fn reads_over_tls_and_refuses_an_untrusted_certificate(db: PgPool) {
        use tokio_rustls::TlsAcceptor;
        use tokio_rustls::rustls::ServerConfig;
        use tokio_rustls::rustls::pki_types::PrivateKeyDer;

        let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let ca = std::env::temp_dir().join(format!("hd-test-ca-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(&ca, made.cert.pem()).unwrap();
        let server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![made.cert.der().clone()],
                    PrivateKeyDer::try_from(made.signing_key.serialize_der()).unwrap(),
                )
                .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (client, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(client).await else {
                        return;
                    };
                    let mut upstream = TcpStream::connect("127.0.0.1:55143").await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut tls, &mut upstream).await;
                });
            }
        });

        let url = format!("imaps://helpdesk:helpdesk@localhost:{port}/INBOX");
        let mut trusted = MailboxConfig::from_url(&url, Some(ca.clone())).unwrap();
        trusted.import_existing = true;
        let connector = tls_connector(trusted.ca_file.as_ref()).unwrap();
        read_new(&db, &trusted, Some(&connector))
            .await
            .expect("reading over TLS");

        let untrusted = MailboxConfig::from_url(&url, None).unwrap();
        let connector = tls_connector(None).unwrap();
        let error = read_new(&db, &untrusted, Some(&connector))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("TLS handshake"), "{error:#}");
        std::fs::remove_file(ca).ok();
    }
}
