use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

mod addons;
mod auth;
mod catalog;
mod cli;
mod customers;
mod drafts;
mod error;
mod http;
mod imap;
mod mail;
mod outbound;
mod parents;
mod proposals;
#[cfg(test)]
mod testing;
mod tickets;
mod triage;

#[derive(Parser)]
#[command(about = "Help desk core")]
struct Cli {
    /// PostgreSQL connection string.
    #[arg(long, env = "HD_DATABASE_URL", global = true)]
    database_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply pending database migrations and serve the HTTP API.
    Serve {
        /// Address the HTTP API listens on.
        #[arg(long, env = "HD_LISTEN", default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// Mailbox to read, as imaps://user:password@host[:port]/MAILBOX (imap:// only for a
        /// server on this machine). Without it no mail is read.
        #[arg(long, env = "HD_IMAP_URL")]
        imap_url: Option<String>,
        /// Server that sends replies: smtps://user:password@host[:port] or
        /// smtp://user:password@host[:port]?tls=required (plain smtp:// only for a server on this
        /// machine). Without it replies wait in the outbox.
        #[arg(long, env = "HD_SMTP_URL")]
        smtp_url: Option<String>,
        /// PEM file of extra certificate authorities to trust for the mail servers.
        #[arg(long, env = "HD_MAIL_CA_FILE")]
        mail_ca_file: Option<std::path::PathBuf>,
        /// The help desk's own address, which replies are sent from. Without it agents cannot
        /// reply.
        #[arg(long, env = "HD_MAIL_FROM")]
        mail_from: Option<String>,
        /// The display name that goes with HD_MAIL_FROM.
        #[arg(long, env = "HD_MAIL_FROM_NAME", default_value = "Help desk")]
        mail_from_name: String,
        /// Import the mail already in the mailbox on the first read. By default only mail that
        /// arrives after the first read becomes tickets.
        #[arg(long, env = "HD_IMAP_IMPORT_EXISTING")]
        imap_import_existing: bool,
        /// Seconds between two reads of the mailbox.
        #[arg(long, env = "HD_IMAP_POLL_SECONDS", default_value_t = 30)]
        imap_poll_seconds: u64,
    },
    /// Apply pending database migrations and exit.
    Migrate,
    /// Manage customers.
    #[command(subcommand)]
    Customer(cli::CustomerCommand),
    /// Manage agents.
    #[command(subcommand)]
    Agent(cli::AgentCommand),
    /// Manage add-ons.
    #[command(subcommand)]
    Addon(cli::AddonCommand),
    /// Print the OpenAPI specification of the HTTP API (needs no database).
    Openapi,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A missing .env file is fine: the variables can come from the environment.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("HD_LOG").unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")),
        )
        .init();

    let cli = Cli::parse();
    if let Command::Openapi = cli.command {
        println!("{}", http::openapi().to_pretty_json()?);
        return Ok(());
    }
    let database_url = cli
        .database_url
        .context("set HD_DATABASE_URL or pass --database-url")?;
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("connecting to the database")?;
    sqlx::migrate!()
        .run(&db)
        .await
        .context("applying database migrations")?;

    match cli.command {
        Command::Migrate | Command::Openapi => Ok(()),
        Command::Customer(command) => cli::customer(&db, command).await,
        Command::Agent(command) => cli::agent(&db, command).await,
        Command::Addon(command) => cli::addon(&db, command).await,
        Command::Serve {
            listen,
            imap_url,
            smtp_url,
            mail_ca_file,
            mail_from,
            mail_from_name,
            imap_import_existing,
            imap_poll_seconds,
        } => {
            let sender = mail_from
                .map(|address| outbound::Sender::new(&address, &mail_from_name))
                .transpose()?;
            if sender.is_none() {
                tracing::warn!("HD_MAIL_FROM is not set: agents cannot reply");
            }
            match imap_url {
                Some(url) => {
                    let mut config = imap::MailboxConfig::from_url(&url, mail_ca_file.clone())?;
                    config.import_existing = imap_import_existing;
                    tracing::info!(host = %config.host, tls = config.tls, mailbox = %config.mailbox, "reading mail");
                    tokio::spawn(imap::poll(
                        db.clone(),
                        config,
                        Duration::from_secs(imap_poll_seconds.max(1)),
                    ));
                }
                None => tracing::warn!("HD_IMAP_URL is not set: no mail will be read"),
            }
            match smtp_url {
                Some(url) => {
                    let transport = outbound::transport(&url, mail_ca_file.as_ref())?;
                    tokio::spawn(outbound::deliver(
                        db.clone(),
                        transport,
                        Duration::from_secs(5),
                    ));
                }
                None => tracing::warn!("HD_SMTP_URL is not set: replies stay in the outbox"),
            }
            tokio::spawn(addons::deliver(
                db.clone(),
                addons::http_client()?,
                Duration::from_secs(5),
            ));
            http::serve(http::AppState { db, sender }, listen).await
        }
    }
}
