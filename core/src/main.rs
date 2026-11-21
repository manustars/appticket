use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

mod auth;
mod cli;
mod http;
mod imap;
mod mail;

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
        /// PEM file of extra certificate authorities to trust for the mail server.
        #[arg(long, env = "HD_IMAP_CA_FILE")]
        imap_ca_file: Option<std::path::PathBuf>,
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
        Command::Serve {
            listen,
            imap_url,
            imap_ca_file,
            imap_poll_seconds,
        } => {
            match imap_url {
                Some(url) => {
                    let config = imap::MailboxConfig::from_url(&url, imap_ca_file)?;
                    tracing::info!(host = %config.host, tls = config.tls, mailbox = %config.mailbox, "reading mail");
                    tokio::spawn(imap::poll(
                        db.clone(),
                        config,
                        Duration::from_secs(imap_poll_seconds.max(1)),
                    ));
                }
                None => tracing::warn!("HD_IMAP_URL is not set: no mail will be read"),
            }
            http::serve(db, listen).await
        }
    }
}
