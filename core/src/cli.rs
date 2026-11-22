//! Administrative commands run from the command line.

use anyhow::Context;
use clap::Subcommand;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agents;
use crate::roles::{self, Grant, Permission};

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
    /// generated and printed once when that is unset. Without --role, the very first agent gets
    /// the Administrator role and later ones the default role.
    Add {
        /// The agent's e-mail address, used to sign in.
        email: String,
        /// The agent's display name.
        #[arg(long)]
        name: String,
        /// The role's name.
        #[arg(long)]
        role: Option<String>,
    },
    /// List agents with their roles.
    List,
    /// Give an agent another role, such as Administrator.
    SetRole {
        /// The agent's e-mail address.
        email: String,
        /// The role's name.
        role: String,
    },
    /// Set an agent's password, from HD_AGENT_PASSWORD or generated and printed once, and end
    /// the agent's sessions.
    SetPassword {
        /// The agent's e-mail address.
        email: String,
    },
    /// Deactivate an agent who left the help desk: they can no longer sign in, and their tickets
    /// that are not closed lose their assignee.
    Deactivate {
        /// The agent's e-mail address.
        email: String,
    },
    /// Let a deactivated agent sign in again.
    Reactivate {
        /// The agent's e-mail address.
        email: String,
    },
}

fn password_from_env() -> Option<String> {
    std::env::var("HD_AGENT_PASSWORD")
        .ok()
        .filter(|password| !password.is_empty())
}

pub async fn agent(db: &PgPool, command: AgentCommand) -> anyhow::Result<()> {
    let fail = |e: crate::error::Error| anyhow::anyhow!("{e}");
    let cli = Grant::command_line();
    match command {
        AgentCommand::Add { email, name, role } => {
            let role = match role {
                Some(role) => Some(roles::id_by_name(db, &role).await.map_err(fail)?),
                None => None,
            };
            let password = password_from_env();
            let added = agents::add(db, &cli, &email, &name, role, password.as_deref())
                .await
                .map_err(fail)?;
            println!("{}", added.id);
            if let Some(password) = added.generated_password {
                println!("password: {password}");
            }
        }
        AgentCommand::List => {
            for a in agents::list(db).await.map_err(fail)? {
                let state = if a.deactivated_at.is_some() {
                    "  (deactivated)"
                } else {
                    ""
                };
                println!("{}  {}  {}  {}{state}", a.id, a.name, a.email, a.role_name);
            }
        }
        AgentCommand::SetRole { email, role } => {
            let agent = agents::id_by_email(db, &email).await.map_err(fail)?;
            let role = roles::id_by_name(db, &role).await.map_err(fail)?;
            agents::set_role(db, &cli, agent, role)
                .await
                .map_err(fail)?;
        }
        AgentCommand::SetPassword { email } => {
            let agent = agents::id_by_email(db, &email).await.map_err(fail)?;
            let password = password_from_env();
            if let Some(generated) = agents::reset_password(db, &cli, agent, password.as_deref())
                .await
                .map_err(fail)?
            {
                println!("password: {generated}");
            }
        }
        AgentCommand::Deactivate { email } => {
            let agent = agents::id_by_email(db, &email).await.map_err(fail)?;
            let unassigned = agents::deactivate(db, &cli, agent).await.map_err(fail)?;
            println!("deactivated; {unassigned} ticket(s) lost their assignee");
        }
        AgentCommand::Reactivate { email } => {
            let agent = agents::id_by_email(db, &email).await.map_err(fail)?;
            agents::reactivate(db, &cli, agent).await.map_err(fail)?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
pub enum RoleCommand {
    /// List roles with their permissions.
    List,
    /// List the permissions a role can hold.
    Permissions,
    /// Create a role.
    Add {
        /// The role's name.
        name: String,
        /// A permission, such as tickets.reply; repeat for several.
        #[arg(long = "permission")]
        permissions: Vec<String>,
    },
    /// Replace a role's permissions.
    Set {
        /// The role's name.
        name: String,
        /// A permission, such as tickets.reply; repeat for several, none for a role that only reads.
        #[arg(long = "permission")]
        permissions: Vec<String>,
    },
    /// Rename a role.
    Rename { name: String, new_name: String },
    /// Make a role the one new agents get when none is named.
    Default { name: String },
    /// Remove a role no agent has.
    Remove { name: String },
}

fn parse_permissions(names: &[String]) -> anyhow::Result<Vec<Permission>> {
    names
        .iter()
        .map(|name| {
            Permission::parse(name.trim()).ok_or_else(|| {
                anyhow::anyhow!("unknown permission {name:?}: see `hd role permissions`")
            })
        })
        .collect()
}

pub async fn role(db: &PgPool, command: RoleCommand) -> anyhow::Result<()> {
    let fail = |e: crate::error::Error| anyhow::anyhow!("{e}");
    let cli = Grant::command_line();
    match command {
        RoleCommand::List => {
            for r in roles::list(db).await.map_err(fail)? {
                let mut marks = Vec::new();
                if r.default {
                    marks.push("default");
                }
                let permissions = if r.administrator {
                    "every permission".to_owned()
                } else {
                    r.permissions
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let marks = if marks.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", marks.join(", "))
                };
                println!("{}{marks}  agents: {}  {permissions}", r.name, r.agents);
            }
        }
        RoleCommand::Permissions => {
            for p in Permission::ALL {
                println!("{:<18}{}", p.as_str(), p.doing());
            }
        }
        RoleCommand::Add { name, permissions } => {
            let permissions = parse_permissions(&permissions)?;
            let id = roles::create(db, &cli, &name, &permissions)
                .await
                .map_err(fail)?;
            println!("{id}");
        }
        RoleCommand::Set { name, permissions } => {
            let permissions = parse_permissions(&permissions)?;
            let id = roles::id_by_name(db, &name).await.map_err(fail)?;
            let current = roles::list(db).await.map_err(fail)?;
            let name = current
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.name.clone())
                .unwrap_or(name);
            roles::update(db, &cli, id, &name, &permissions)
                .await
                .map_err(fail)?;
        }
        RoleCommand::Rename { name, new_name } => {
            let id = roles::id_by_name(db, &name).await.map_err(fail)?;
            let current = roles::list(db).await.map_err(fail)?;
            let permissions = current
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.permissions.clone())
                .unwrap_or_default();
            roles::update(db, &cli, id, &new_name, &permissions)
                .await
                .map_err(fail)?;
        }
        RoleCommand::Default { name } => {
            let id = roles::id_by_name(db, &name).await.map_err(fail)?;
            roles::make_default(db, &cli, id).await.map_err(fail)?;
        }
        RoleCommand::Remove { name } => {
            let id = roles::id_by_name(db, &name).await.map_err(fail)?;
            roles::remove(db, &cli, id).await.map_err(fail)?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
pub enum AddonCommand {
    /// Register an add-on. Prints its token and event secret once: store them in the add-on.
    Add {
        /// A short name: lowercase letters, digits and dashes.
        name: String,
        /// The add-on's base URL; events are sent to <url>/events.
        #[arg(long)]
        url: String,
        /// An event kind the add-on receives; repeat for several.
        #[arg(long = "event")]
        events: Vec<String>,
        /// Something the add-on's token may do; repeat for several.
        #[arg(long = "scope")]
        scopes: Vec<String>,
    },
    /// List add-ons with their undelivered events.
    List,
    /// Switch an add-on on: its token works and its waiting events are delivered.
    Enable { name: String },
    /// Switch an add-on off: its token is refused and its events wait.
    Disable { name: String },
}

pub async fn addon(db: &PgPool, command: AddonCommand) -> anyhow::Result<()> {
    let fail = |e: crate::error::Error| anyhow::anyhow!("{e}");
    match command {
        AddonCommand::Add {
            name,
            url,
            events,
            scopes,
        } => {
            let added = crate::addons::register(db, &name, &url, &events, &scopes)
                .await
                .map_err(fail)?;
            println!("{}", added.id);
            println!("token: {}", added.token);
            println!("event secret: {}", added.event_secret);
        }
        AddonCommand::List => {
            for a in crate::addons::list(db).await.map_err(fail)? {
                println!(
                    "{}  {}  {}  events: {}  scopes: {}  undelivered: {}",
                    a.name,
                    if a.enabled { "on" } else { "off" },
                    a.url,
                    a.events.join(","),
                    a.scopes.join(","),
                    a.pending
                );
            }
        }
        AddonCommand::Enable { name } => crate::addons::set_enabled(db, &name, true)
            .await
            .map_err(fail)?,
        AddonCommand::Disable { name } => crate::addons::set_enabled(db, &name, false)
            .await
            .map_err(fail)?,
    }
    Ok(())
}
