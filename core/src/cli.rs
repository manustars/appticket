//! Administrative commands run from the command line.

use anyhow::Context;
use clap::Subcommand;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum CustomerCommand {
    /// Add a customer, optionally with the mail domains that identify its requesters.
    Add {
        /// The customer's name.
        name: String,
        /// A mail domain of the customer's requesters; repeat for several.
        #[arg(long = "domain")]
        domains: Vec<String>,
        /// Mark this customer as the help desk's own organization (only one can be).
        #[arg(long)]
        help_desk: bool,
    },
    /// List customers.
    List,
}

pub async fn customer(db: &PgPool, command: CustomerCommand) -> anyhow::Result<()> {
    match command {
        CustomerCommand::Add {
            name,
            domains,
            help_desk,
        } => {
            let mut tx = db.begin().await?;
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO customers (id, name, is_help_desk) VALUES ($1, $2, $3)")
                .bind(id)
                .bind(name.trim())
                .bind(help_desk)
                .execute(&mut *tx)
                .await
                .context("adding the customer (is there already a help desk customer?)")?;
            if help_desk {
                crate::catalog::help_desk_uses_every_service(&mut tx, id)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            for domain in &domains {
                sqlx::query("INSERT INTO customer_domains (domain, customer_id) VALUES ($1, $2)")
                    .bind(domain.trim().to_lowercase())
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .with_context(|| format!("registering domain {domain}"))?;
            }
            tx.commit().await?;
            println!("{id}");
            Ok(())
        }
        CustomerCommand::List => {
            let rows: Vec<(Uuid, String, bool, Option<String>)> = sqlx::query_as(
                "SELECT c.id, c.name, c.is_help_desk, string_agg(d.domain, ', ' ORDER BY d.domain)
                 FROM customers c LEFT JOIN customer_domains d ON d.customer_id = c.id
                 GROUP BY c.id ORDER BY c.name",
            )
            .fetch_all(db)
            .await?;
            for (id, name, help_desk, domains) in rows {
                let mark = if help_desk { " (help desk)" } else { "" };
                println!("{id}  {name}{mark}  {}", domains.unwrap_or_default());
            }
            Ok(())
        }
    }
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Add an agent. The password is read from the HD_AGENT_PASSWORD environment variable, or
    /// generated and printed once when that is unset.
    Add {
        /// The agent's e-mail address, used to sign in.
        email: String,
        /// The agent's display name.
        #[arg(long)]
        name: String,
    },
    /// List agents.
    List,
}

pub async fn agent(db: &PgPool, command: AgentCommand) -> anyhow::Result<()> {
    match command {
        AgentCommand::Add { email, name } => {
            let (password, generated) = match std::env::var("HD_AGENT_PASSWORD") {
                Ok(password) if !password.is_empty() => (password, false),
                _ => (format!("{}", Uuid::new_v4().simple()), true),
            };
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO agents (id, email, name, password_hash) VALUES ($1, $2, $3, $4)",
            )
            .bind(id)
            .bind(email.trim().to_lowercase())
            .bind(name.trim())
            .bind(crate::auth::hash_password(&password)?)
            .execute(db)
            .await
            .context("adding the agent (is the e-mail already used?)")?;
            println!("{id}");
            if generated {
                println!("password: {password}");
            }
            Ok(())
        }
        AgentCommand::List => {
            let rows: Vec<(Uuid, String, String)> =
                sqlx::query_as("SELECT id, email, name FROM agents ORDER BY name")
                    .fetch_all(db)
                    .await?;
            for (id, email, name) in rows {
                println!("{id}  {name}  {email}");
            }
            Ok(())
        }
    }
}
