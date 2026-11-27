use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

mod addon_tickets;
mod addons;
mod agents;
mod announcements;
mod asset_kinds;
mod asset_link_proposals;
mod auth;
mod catalog;
mod cli;
mod customers;
mod database;
mod drafts;
mod error;
mod external_references;
mod http;
mod http_client;
mod identities;
mod imap;
mod import;
mod key_requesters;
mod local_sign_in;
mod mail;
mod mail_status;
mod oidc;
mod outbound;
mod parents;
mod postgres_version;
mod proposals;
mod requesters;
mod roles;
mod secret;
mod sign_in_limits;
mod sites;
mod supplied_assets;
#[cfg(test)]
mod testing;
mod tickets;
mod triage;

#[derive(Parser)]
#[command(about = "Help desk core")]
struct Cli {
    #[command(flatten)]
    database: database::Settings,

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
        #[command(flatten)]
        imap_url: secret::ImapUrl,
        #[command(flatten)]
        smtp_url: secret::SmtpUrl,
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
        /// The built UI (`web/dist`) to serve next to the API. Without it only the API is
        /// served, as in development, where Vite serves the UI.
        #[arg(long, env = "HD_WEB_DIR")]
        web_dir: Option<PathBuf>,
        /// Failed sign-ins within the window, for an account or from an address, after which
        /// attempts are slowed: the next four wait 1, 2, 4 and 8 seconds, and attempts after
        /// those are refused until a window has passed since the last failure.
        #[arg(long, env = "HD_SIGN_IN_FAILURES", default_value_t = 5,
            value_parser = clap::value_parser!(u32).range(1..=100))]
        sign_in_failures: u32,
        /// Seconds over which failed sign-ins are counted, and that a refusal lasts.
        #[arg(long, env = "HD_SIGN_IN_WINDOW_SECONDS", default_value_t = 900,
            value_parser = clap::value_parser!(u64).range(1..=86_400))]
        sign_in_window_seconds: u64,
        /// Reverse proxies, as addresses or networks separated by commas (192.0.2.10,
        /// 198.51.100.0/24), whose X-Forwarded-For gives the address a sign-in counts under.
        /// Without them the header is ignored and a sign-in counts under its connection's address.
        #[arg(long, env = "HD_TRUSTED_PROXIES", default_value = "", hide_default_value = true,
            value_parser = http::parse_proxies)]
        trusted_proxies: http::TrustedProxies,
        /// The address agents open the help desk at, such as https://helpdesk.example.com:
        /// https:// unless on this machine. The identity provider sends agents back to it after
        /// they sign in; needed with an identity provider.
        #[arg(long, env = "HD_PUBLIC_URL")]
        public_url: Option<String>,
        #[command(flatten)]
        identity_provider: Box<oidc::Settings>,
    },
    /// Apply pending database migrations and exit.
    Migrate,
    /// Manage customers.
    #[command(subcommand)]
    Customer(cli::CustomerCommand),
    /// Manage agents.
    #[command(subcommand)]
    Agent(cli::AgentCommand),
    /// Manage roles and their permissions.
    #[command(subcommand)]
    Role(cli::RoleCommand),
    /// Manage add-ons.
    #[command(subcommand)]
    Addon(cli::AddonCommand),
    /// How agents sign in: switch local sign-in off and on, and check the identity provider.
    #[command(subcommand)]
    SignIn(cli::SignInCommand),
    /// Import customers, sites, requesters and assets from CSV files: shows what it would do,
    /// and does it with --apply (--help lists the columns).
    Import(cli::ImportArgs),
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
        // Colours only on a terminal: container logs stay plain text.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    let cli = Cli::parse();
    // The commands that need no database, so that they work before anything else is set up.
    let command = match cli.command {
        Command::Openapi => {
            println!("{}", http::openapi().to_pretty_json()?);
            return Ok(());
        }
        Command::SignIn(cli::SignInCommand::Check { provider }) => {
            return cli::sign_in_check(*provider, &mut std::io::stdout()).await;
        }
        command => command,
    };
    let database = cli.database.connect_options()?;
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(database)
        .await
        .context("connecting to the database")?;
    postgres_version::check(&db).await?;
    sqlx::migrate!()
        .run(&db)
        .await
        .context("applying database migrations")?;

    match command {
        Command::Migrate | Command::Openapi => Ok(()),
        Command::Customer(command) => cli::customer(&db, command).await,
        Command::Agent(command) => cli::agent(&db, command).await,
        Command::Role(command) => cli::role(&db, command).await,
        Command::Addon(command) => cli::addon(&db, command).await,
        Command::SignIn(command) => cli::sign_in(&db, command, &mut std::io::stdout()).await,
        Command::Import(args) => cli::import(&db, args, &mut std::io::stdout()).await,
        Command::Serve {
            listen,
            imap_url,
            smtp_url,
            mail_ca_file,
            mail_from,
            mail_from_name,
            imap_import_existing,
            imap_poll_seconds,
            web_dir,
            sign_in_failures,
            sign_in_window_seconds,
            trusted_proxies,
            public_url,
            identity_provider,
        } => {
            // A provider configured in part stops the core; one that is down at startup does not,
            // since it is asked only when an agent signs in through it.
            let (identity_provider, identity_provider_activity) = match oidc::Config::from_settings(
                *identity_provider,
                public_url.as_deref(),
            )? {
                Some(config) => {
                    tracing::info!(issuer = %config.issuer.as_str(), "agents can sign in through the OpenID Connect identity provider");
                    let (provider, activity) = http::connect_identity_provider(config)?;
                    (Some(provider), Some(activity))
                }
                None => (None, None),
            };
            local_sign_in::at_startup(&db, identity_provider.is_some())
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(dir) = &web_dir
                && !dir.join("index.html").is_file()
            {
                anyhow::bail!(
                    "HD_WEB_DIR is {}, which has no index.html: point it at the built UI (web/dist)",
                    dir.display()
                );
            }
            let sender = mail_from
                .map(|address| outbound::Sender::new(&address, &mail_from_name))
                .transpose()?;
            if sender.is_none() {
                tracing::warn!("HD_MAIL_FROM is not set: agents cannot reply");
            }
            let mut mail = mail_status::MailStatus::default();
            match imap_url.read()? {
                Some(url) => {
                    let mut config = imap::MailboxConfig::from_url(&url, mail_ca_file.clone())?;
                    config.import_existing = imap_import_existing;
                    tracing::info!(host = %config.host, tls = config.tls, mailbox = %config.mailbox, "reading mail");
                    let tracker = mail_status::Tracker::new(
                        format!("{}:{} ({})", config.host, config.port, config.mailbox),
                        vec![config.password.clone()],
                    );
                    mail.reading = Some(tracker.clone());
                    tokio::spawn(imap::poll(
                        db.clone(),
                        config,
                        Duration::from_secs(imap_poll_seconds.max(1)),
                        tracker,
                    ));
                }
                None => tracing::warn!("HD_IMAP_URL is not set: no mail will be read"),
            }
            match smtp_url.read()? {
                Some(url) => {
                    let transport = outbound::transport(&url, mail_ca_file.as_ref())?;
                    let tracker = outbound::tracker(&url)?;
                    mail.sending = Some(tracker.clone());
                    tokio::spawn(outbound::deliver(
                        db.clone(),
                        transport,
                        Duration::from_secs(5),
                        tracker,
                    ));
                }
                None => tracing::warn!("HD_SMTP_URL is not set: replies stay in the outbox"),
            }
            tokio::spawn(addons::deliver(
                db.clone(),
                addons::http_client()?,
                Duration::from_secs(5),
            ));
            if trusted_proxies.is_empty() {
                tracing::info!("HD_TRUSTED_PROXIES is not set: X-Forwarded-For is ignored");
            }
            let state = http::AppState {
                db,
                sender,
                mail,
                sign_in_limits: sign_in_limits::SignInLimits::new(
                    sign_in_failures,
                    Duration::from_secs(sign_in_window_seconds),
                ),
                trusted_proxies,
                identity_provider,
                identity_provider_activity,
            };
            http::serve(state, listen, web_dir).await
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, FromArgMatches};

    use super::{Cli, Command};
    use crate::cli::{AddonCommand, AgentCommand, CustomerCommand, RoleCommand};
    use crate::database::Database;

    /// The command line `args` give, with the environment of the tests (such as CI's
    /// HD_DATABASE_URL) left out.
    fn parse(args: &[&str]) -> anyhow::Result<Cli> {
        let mut command = Cli::command();
        let ids: Vec<_> = command
            .get_arguments()
            .filter(|arg| arg.get_env().is_some())
            .map(|arg| arg.get_id().clone())
            .collect();
        for id in ids {
            command = command.mut_arg(id, |arg| arg.env(None::<&'static str>));
        }
        let matches = command.try_get_matches_from(args)?;
        Ok(Cli::from_arg_matches(&matches)?)
    }

    /// The database `args` give on the command line, with the environment of the tests left out.
    fn database(args: &[&str]) -> anyhow::Result<Database> {
        parse(args)?.database.read()
    }

    const DATABASE_URL: &str = "postgres://helpdesk:secret@db.example.com/helpdesk";

    #[test]
    fn a_customer_keeps_its_name_beside_a_database_url() {
        let cli = parse(&[
            "hd",
            "--database-url",
            DATABASE_URL,
            "customer",
            "add",
            "Northgate Studio",
        ])
        .unwrap();
        let Command::Customer(CustomerCommand::Add { name, .. }) = cli.command else {
            panic!("not `customer add`");
        };
        assert_eq!(name, "Northgate Studio");
        assert_eq!(
            cli.database.read().unwrap(),
            Database::Url(DATABASE_URL.to_owned())
        );
    }

    #[test]
    fn an_agent_keeps_its_name_beside_a_database_url() {
        let cli = parse(&[
            "hd",
            "--database-url",
            DATABASE_URL,
            "agent",
            "add",
            "ada@example.com",
            "--name",
            "Ada Agent",
        ])
        .unwrap();
        let Command::Agent(AgentCommand::Add { email, name, .. }) = cli.command else {
            panic!("not `agent add`");
        };
        assert_eq!(
            (email.as_str(), name.as_str()),
            ("ada@example.com", "Ada Agent")
        );
        assert_eq!(
            cli.database.read().unwrap(),
            Database::Url(DATABASE_URL.to_owned())
        );
    }

    #[test]
    fn a_role_keeps_its_name_beside_a_database_url() {
        let cli = parse(&[
            "hd",
            "role",
            "rename",
            "Front Desk",
            "First Line",
            "--database-url",
            DATABASE_URL,
        ])
        .unwrap();
        let Command::Role(RoleCommand::Rename { name, new_name }) = cli.command else {
            panic!("not `role rename`");
        };
        assert_eq!(
            (name.as_str(), new_name.as_str()),
            ("Front Desk", "First Line")
        );
        assert_eq!(
            cli.database.read().unwrap(),
            Database::Url(DATABASE_URL.to_owned())
        );
    }

    #[test]
    fn an_addon_keeps_its_name_beside_a_database_url() {
        let cli = parse(&[
            "hd",
            "--database-url",
            DATABASE_URL,
            "addon",
            "enable",
            "probe-monitor",
        ])
        .unwrap();
        let Command::Addon(AddonCommand::Enable { name }) = cli.command else {
            panic!("not `addon enable`");
        };
        assert_eq!(name, "probe-monitor");
        assert_eq!(
            cli.database.read().unwrap(),
            Database::Url(DATABASE_URL.to_owned())
        );
    }

    /// Checks that the help of `command` and of every command under it lists each of `options`.
    fn help_lists(command: &mut clap::Command, path: &str, options: &[String]) {
        let help = command.render_help().to_string();
        for option in options {
            assert!(
                help.contains(option),
                "`{path} --help` lacks {option}:\n{help}"
            );
        }
        // Clap's own `help` command prints the others' help and takes no options.
        for subcommand in command
            .get_subcommands_mut()
            .filter(|subcommand| subcommand.get_name() != "help")
        {
            let path = format!("{path} {}", subcommand.get_name());
            help_lists(subcommand, &path, options);
        }
    }

    #[test]
    fn every_command_lists_the_database_options_in_its_help() {
        let mut command = Cli::command();
        command.build();
        let options: Vec<String> = command
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .filter_map(|arg| arg.get_long())
            // Each takes a value: `--database-url <` is not found in `--database-url-file <`.
            .map(|long| format!("--{long} <"))
            .collect();
        assert!(
            options.iter().any(|o| o == "--database-name <"),
            "{options:?}"
        );
        help_lists(&mut command, "hd", &options);
    }

    #[test]
    fn the_separate_database_parameters_are_options_of_every_command() {
        assert_eq!(
            database(&[
                "hd",
                "role",
                "list",
                "--database-host",
                "db.example.com",
                "--database-port",
                "6543",
                "--database-name",
                "helpdesk",
                "--database-user",
                "helpdesk_core",
                "--database-password",
                "p@ss/w:rd",
            ])
            .unwrap(),
            Database::Parameters {
                host: "db.example.com".to_owned(),
                port: 6543,
                name: "helpdesk".to_owned(),
                user: "helpdesk_core".to_owned(),
                password: "p@ss/w:rd".to_owned(),
            }
        );
    }

    #[test]
    fn a_database_url_option_with_a_separate_parameter_is_refused_naming_both() {
        let error = database(&[
            "hd",
            "--database-url",
            "postgres://helpdesk:secret@db.example.com/helpdesk",
            "migrate",
            "--database-port",
            "5432",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("HD_DATABASE_URL") && error.contains("HD_DATABASE_PORT"),
            "{error}"
        );
    }

    #[test]
    fn database_options_given_in_part_are_refused_naming_what_is_missing() {
        let error = database(&[
            "hd",
            "--database-host",
            "db.example.com",
            "migrate",
            "--database-user",
            "helpdesk_core",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("HD_DATABASE_NAME") && error.contains("HD_DATABASE_PASSWORD"),
            "{error}"
        );
        for given in ["HD_DATABASE_HOST", "HD_DATABASE_USER", "HD_DATABASE_PORT"] {
            assert!(!error.contains(given), "{given}: {error}");
        }
    }

    #[test]
    fn the_database_password_option_and_its_file_together_are_refused() {
        let error = database(&[
            "hd",
            "migrate",
            "--database-host",
            "db.example.com",
            "--database-name",
            "helpdesk",
            "--database-user",
            "helpdesk_core",
            "--database-password",
            "p@ss/w:rd",
            "--database-password-file",
            "database-password",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("HD_DATABASE_PASSWORD ") && error.contains("HD_DATABASE_PASSWORD_FILE"),
            "{error}"
        );
    }

    /// The secrets of every command, by variable, each checked to have its `_FILE` variant.
    fn secrets(command: &clap::Command, path: &str, found: &mut Vec<String>) {
        let variables: Vec<String> = command
            .get_arguments()
            .filter_map(|arg| arg.get_env())
            .map(|env| env.to_string_lossy().into_owned())
            .collect();
        for arg in command.get_arguments() {
            let Some(env) = arg.get_env().filter(|_| arg.is_hide_env_values_set()) else {
                continue;
            };
            let variable = env.to_string_lossy().into_owned();
            assert!(
                variables.contains(&format!("{variable}_FILE")),
                "`{path}` reads the secret {variable} but not {variable}_FILE"
            );
            found.push(variable);
        }
        for subcommand in command.get_subcommands() {
            secrets(
                subcommand,
                &format!("{path} {}", subcommand.get_name()),
                found,
            );
        }
    }

    #[test]
    fn every_secret_can_be_read_from_a_file() {
        let mut found = Vec::new();
        secrets(&Cli::command(), "hd", &mut found);
        // A secret missing here is one the walk above no longer sees: its value is not hidden.
        for secret in [
            "HD_DATABASE_URL",
            "HD_DATABASE_PASSWORD",
            "HD_IMAP_URL",
            "HD_SMTP_URL",
            "HD_AGENT_PASSWORD",
            "HD_OIDC_CLIENT_SECRET",
        ] {
            assert!(
                found.iter().any(|f| f == secret),
                "{secret} is not a secret: {found:?}"
            );
        }
    }
}
