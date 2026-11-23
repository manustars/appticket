//! Administrative commands run from the command line.

use std::io::Write;
use std::path::PathBuf;

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

/// The columns of the import files, shown by `hd import --help`.
pub const IMPORT_HELP: &str = "\
Each file is CSV in UTF-8 (Excel: \"CSV UTF-8\"), separated by commas or semicolons, with a
header row of these column names, in any order; only the required ones must be there. A filled
cell sets a value, an empty one leaves it as it is. An import never deletes, retires or
offboards anything, nor brings anything back. A file may also have an `error` column, as the
rows left out come back with: it is ignored.

Customers file (--customers); a customer is recognised by its external reference, else by its
name, ignoring case:
  name                The customer's name (required column; required to create a customer).
  external_system     The system of an external reference, such as a monitoring tool.
  external_name       The name that system uses for the customer. With external_system, it
                      recognises the customer, or is added to it.
  legal_name          The legal name.
  tax_id              The tax identifier, such as a VAT number; it needs its country.
  tax_country         The country of the tax identifier: two letters of ISO 3166-1, such as IT.
  address             The postal address; it can span several lines.
  phone               A phone number.
  website             An http or https address; https:// is added when there is none.
  notes               Internal notes, seen only by agents; they replace the ones there.

Sites file (--sites); a site is recognised by its name within its customer, ignoring case:
  customer            The site's customer, by name (required column).
  customer_external_system
  customer_external_name
                      Or by one of its external references, a system and a name in it.
  name                The site's name (required column and value).
  address             The site's address (required column; required to create a site).

The customers file is taken first, so that a site can be at a customer it creates. A row that
breaks a rule is left out with its reason, as are the sites of a customer row left out; the
rest is imported. The rows left out are written to <kind>-left-out.csv, with the same columns
and an error column, ready to correct and import again.";

/// Import customers and sites from CSV files: shows what it would do, and does it with --apply.
#[derive(clap::Args)]
#[command(after_long_help = IMPORT_HELP)]
pub struct ImportArgs {
    /// A customers file; `-` reads it from standard input.
    #[arg(long)]
    customers: Option<PathBuf>,
    /// A sites file; `-` reads it from standard input.
    #[arg(long)]
    sites: Option<PathBuf>,
    /// Import; without it nothing changes, and the command only shows what it would do.
    #[arg(long)]
    apply: bool,
    /// The folder the rows left out are written to (default: the folder of each file; none
    /// for a file read from standard input).
    #[arg(long, value_name = "DIR")]
    left_out: Option<PathBuf>,
}

pub async fn import(db: &PgPool, args: ImportArgs, out: &mut impl Write) -> anyhow::Result<()> {
    use crate::import::{File, Kind, Mode, Outcome};
    let mut files = Vec::new();
    let mut paths = Vec::new();
    for (kind, path) in [(Kind::Customers, args.customers), (Kind::Sites, args.sites)] {
        let Some(path) = path else { continue };
        let content = if path.as_os_str() == "-" {
            let mut content = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut content)
                .context("reading standard input")?;
            content
        } else {
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?
        };
        files.push(File { kind, content });
        paths.push((kind, path));
    }
    if files.is_empty() {
        anyhow::bail!("name a file to import: --customers, --sites (see hd import --help)");
    }
    if paths.iter().filter(|(_, p)| p.as_os_str() == "-").count() > 1 {
        anyhow::bail!("only one file can be read from standard input");
    }
    let mode = if args.apply {
        Mode::Apply
    } else {
        Mode::Preview
    };
    let report = crate::import::run(db, &Grant::command_line(), files, mode)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    for file in &report.files {
        let count = |outcome| file.rows.iter().filter(|r| r.outcome == outcome).count();
        let (would, did) = if report.applied {
            ("", "d")
        } else {
            ("to ", "")
        };
        writeln!(
            out,
            "{} file: {} {would}create{did}, {} {would}update{did}, {} unchanged, {} left out",
            file.kind.as_str(),
            count(Outcome::Create),
            count(Outcome::Update),
            count(Outcome::Unchanged),
            count(Outcome::LeftOut),
        )?;
        for row in &file.rows {
            let outcome = match row.outcome {
                Outcome::Create => "create",
                Outcome::Update => "update",
                Outcome::Unchanged => "unchanged",
                Outcome::LeftOut => "left out",
            };
            let detail = match (&row.reason, row.changes.is_empty()) {
                (Some(why), _) => format!(": {why}"),
                (None, false) => format!(" ({})", row.changes.join(", ")),
                (None, true) => String::new(),
            };
            writeln!(
                out,
                "  line {}  {outcome:<9}  {}{detail}",
                row.line, row.label
            )?;
        }
        let Some(left_out) = &file.left_out else {
            continue;
        };
        let path = &paths.iter().find(|(kind, _)| *kind == file.kind).unwrap().1;
        let dir = match (&args.left_out, path.as_os_str() == "-") {
            (Some(dir), _) => Some(dir.clone()),
            (None, false) => Some(
                path.parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(".")),
            ),
            (None, true) => None,
        };
        match dir {
            Some(dir) => {
                let written = dir.join(format!("{}-left-out.csv", file.kind.as_str()));
                std::fs::write(&written, left_out)
                    .with_context(|| format!("writing {}", written.display()))?;
                writeln!(out, "  rows left out written to {}", written.display())?;
            }
            None => writeln!(
                out,
                "  pass --left-out DIR to get the rows left out as a file"
            )?,
        }
    }
    if report.applied {
        writeln!(out, "Imported.")?;
    } else {
        writeln!(out, "Nothing changed: run again with --apply to import.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_import_help_describes_every_column() {
        for column in crate::import::CUSTOMER_COLUMNS
            .iter()
            .chain(crate::import::SITE_COLUMNS)
        {
            assert!(
                IMPORT_HELP
                    .lines()
                    .any(|l| l.starts_with("  ") && l.split_whitespace().next() == Some(column)),
                "{column} is not described"
            );
        }
    }

    #[sqlx::test]
    async fn an_import_is_previewed_then_applied_from_the_command_line(db: PgPool) {
        let dir = std::env::temp_dir().join(format!("hd-import-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let customers = dir.join("customers.csv");
        std::fs::write(
            &customers,
            "name;tax_country\nBrightfield;\nHarbor Lane;ITA\n",
        )
        .unwrap();
        let sites = dir.join("sites.csv");
        std::fs::write(
            &sites,
            "customer,name,address\nBrightfield,Lab,Via del Glicine 3\n",
        )
        .unwrap();
        let args = |apply| ImportArgs {
            customers: Some(customers.clone()),
            sites: Some(sites.clone()),
            apply,
            left_out: None,
        };

        let mut out = Vec::new();
        import(&db, args(false), &mut out).await.unwrap();
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains("line 2  create     Brightfield"), "{shown}");
        assert!(
            shown.contains("line 3  left out   Harbor Lane: "),
            "{shown}"
        );
        assert!(shown.contains("--apply"), "{shown}");
        assert!(crate::customers::list(&db).await.unwrap().is_empty());
        let left_out = std::fs::read_to_string(dir.join("customers-left-out.csv")).unwrap();
        assert!(
            left_out.starts_with("name;tax_country;error\r\nHarbor Lane;ITA;"),
            "{left_out}"
        );
        assert!(!dir.join("sites-left-out.csv").exists());

        let mut out = Vec::new();
        import(&db, args(true), &mut out).await.unwrap();
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains("Imported"), "{shown}");
        let names: Vec<String> = crate::customers::list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Brightfield"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
