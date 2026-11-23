//! Roles and the permissions they hold (ADR 0007).

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{Error, Result, rule};

/// Something beyond reading that a role allows. The list is the product's; the database checks
/// stored permissions against it (migration 0012), so a new one needs a migration too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum Permission {
    /// Notes, state, priority, links to assets and services, parent and child tickets, drafts,
    /// taking a ticket for oneself.
    #[serde(rename = "tickets.work")]
    WorkTickets,
    /// Sending replies to requesters.
    #[serde(rename = "tickets.reply")]
    Reply,
    /// Opening a ticket by hand.
    #[serde(rename = "tickets.open")]
    OpenTickets,
    /// Assigning a ticket to another agent.
    #[serde(rename = "tickets.assign")]
    AssignTickets,
    /// Moving a ticket to another customer.
    #[serde(rename = "tickets.move")]
    MoveTickets,
    /// Opening a ticket from an inbox message, attaching it to a ticket or discarding it.
    #[serde(rename = "inbox.triage")]
    SortInbox,
    /// Adding customers, correcting their details, offboarding them and bringing them back; their
    /// domains, sites, external references and requesters.
    #[serde(rename = "customers.manage")]
    ManageCustomers,
    /// Adding, changing and retiring assets and services.
    #[serde(rename = "catalog.manage")]
    ManageCatalog,
    /// Adding agents, giving them roles, resetting passwords, deactivating agents.
    #[serde(rename = "agents.manage")]
    ManageAgents,
    /// Creating, changing and removing roles.
    #[serde(rename = "roles.manage")]
    ManageRoles,
    /// Switching add-ons on and off, seeing their events.
    #[serde(rename = "addons.manage")]
    ManageAddons,
    /// Seeing the state of reading and sending mail.
    #[serde(rename = "mail.view")]
    ViewMail,
}

impl Permission {
    pub const ALL: [Permission; 12] = [
        Permission::WorkTickets,
        Permission::Reply,
        Permission::OpenTickets,
        Permission::AssignTickets,
        Permission::MoveTickets,
        Permission::SortInbox,
        Permission::ManageCustomers,
        Permission::ManageCatalog,
        Permission::ManageAgents,
        Permission::ManageRoles,
        Permission::ManageAddons,
        Permission::ViewMail,
    ];

    /// The name stored in the database and used in the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::WorkTickets => "tickets.work",
            Permission::Reply => "tickets.reply",
            Permission::OpenTickets => "tickets.open",
            Permission::AssignTickets => "tickets.assign",
            Permission::MoveTickets => "tickets.move",
            Permission::SortInbox => "inbox.triage",
            Permission::ManageCustomers => "customers.manage",
            Permission::ManageCatalog => "catalog.manage",
            Permission::ManageAgents => "agents.manage",
            Permission::ManageRoles => "roles.manage",
            Permission::ManageAddons => "addons.manage",
            Permission::ViewMail => "mail.view",
        }
    }

    pub fn parse(name: &str) -> Option<Permission> {
        Permission::ALL.into_iter().find(|p| p.as_str() == name)
    }

    /// What it allows, to complete "your role does not allow …".
    pub fn doing(self) -> &'static str {
        match self {
            Permission::WorkTickets => "working on tickets",
            Permission::Reply => "replying to requesters",
            Permission::OpenTickets => "opening tickets by hand",
            Permission::AssignTickets => "assigning tickets to other agents",
            Permission::MoveTickets => "moving tickets to another customer",
            Permission::SortInbox => "sorting the inbox",
            Permission::ManageCustomers => "managing customers",
            Permission::ManageCatalog => "managing assets and services",
            Permission::ManageAgents => "managing agents",
            Permission::ManageRoles => "managing roles",
            Permission::ManageAddons => "managing add-ons",
            Permission::ViewMail => "seeing the state of the mail",
        }
    }
}

/// What a role allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The Administrator role: every permission, whatever `permissions` says.
    pub administrator: bool,
    pub permissions: Vec<Permission>,
}

impl Grant {
    /// The command line, run by whoever runs the server: it may do everything.
    pub fn command_line() -> Grant {
        Grant {
            administrator: true,
            permissions: Vec::new(),
        }
    }

    pub fn from_stored(administrator: bool, stored: &[String]) -> Grant {
        Grant {
            administrator,
            permissions: parse_stored(stored),
        }
    }

    pub fn has(&self, permission: Permission) -> bool {
        self.administrator || self.permissions.contains(&permission)
    }

    /// Fails with the reason when the role does not allow `permission`.
    pub fn require(&self, permission: Permission) -> Result<()> {
        if self.has(permission) {
            Ok(())
        } else {
            Err(Error::Forbidden(format!(
                "your role does not allow {}",
                permission.doing()
            )))
        }
    }

    /// Whether everything `other` allows, this allows too.
    pub fn covers(&self, other: &Grant) -> bool {
        self.administrator
            || (!other.administrator && other.permissions.iter().all(|p| self.has(*p)))
    }

    /// Fails unless this covers `other`: nobody hands out more than they hold.
    pub fn require_covers(&self, other: &Grant, what: &str) -> Result<()> {
        if self.covers(other) {
            Ok(())
        } else if other.administrator {
            Err(Error::Forbidden(format!(
                "only an Administrator can {what}"
            )))
        } else {
            Err(Error::Forbidden(format!(
                "you can only {what} with permissions your own role has"
            )))
        }
    }

    /// Every permission it allows, in the product's order.
    pub fn effective(&self) -> Vec<Permission> {
        Permission::ALL
            .into_iter()
            .filter(|p| self.has(*p))
            .collect()
    }
}

fn parse_stored(stored: &[String]) -> Vec<Permission> {
    // The database accepts only known names.
    stored.iter().filter_map(|p| Permission::parse(p)).collect()
}

fn to_stored(permissions: &[Permission]) -> Vec<String> {
    // In the product's order and without repeats, whatever order they were given in.
    Permission::ALL
        .into_iter()
        .filter(|p| permissions.contains(p))
        .map(|p| p.as_str().to_owned())
        .collect()
}

/// A role as the agents' UI and the command line show it.
#[derive(Debug, Serialize, ToSchema)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    /// The built-in Administrator role: every permission, cannot be changed or removed.
    pub administrator: bool,
    /// The role new agents get when none is named.
    pub default: bool,
    /// Empty for the Administrator role, which allows every permission.
    pub permissions: Vec<Permission>,
    /// How many agents, deactivated ones included, have it.
    pub agents: i64,
}

type RoleRow = (Uuid, String, bool, bool, Vec<String>, i64);

/// Every role: the Administrator first, then by name.
pub async fn list(db: &PgPool) -> Result<Vec<Role>> {
    let rows: Vec<RoleRow> = sqlx::query_as(
        "SELECT r.id, r.name, r.is_administrator, r.is_default, r.permissions,
                (SELECT count(*) FROM agents a WHERE a.role_id = r.id)
         FROM roles r ORDER BY r.is_administrator DESC, lower(r.name)",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, name, administrator, default, permissions, agents)| Role {
                id,
                name,
                administrator,
                default,
                permissions: parse_stored(&permissions),
                agents,
            },
        )
        .collect())
}

/// The role named `name`, ignoring case.
pub async fn id_by_name(db: &PgPool, name: &str) -> Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM roles WHERE lower(name) = lower($1)")
        .bind(name.trim())
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound("role"))
}

pub async fn administrator_id(tx: &mut Transaction<'_, Postgres>) -> Result<Uuid> {
    Ok(
        sqlx::query_scalar("SELECT id FROM roles WHERE is_administrator")
            .fetch_one(&mut **tx)
            .await?,
    )
}

/// What the role allows, and whether it is the default one.
pub(crate) async fn grant_of(
    tx: &mut Transaction<'_, Postgres>,
    role: Uuid,
) -> Result<(Grant, bool)> {
    let row: Option<(bool, bool, Vec<String>)> = sqlx::query_as(
        "SELECT is_administrator, is_default, permissions FROM roles WHERE id = $1 FOR UPDATE",
    )
    .bind(role)
    .fetch_optional(&mut **tx)
    .await?;
    let (administrator, default, permissions) = row.ok_or(Error::NotFound("role"))?;
    Ok((Grant::from_stored(administrator, &permissions), default))
}

async fn name_is_free(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
    except: Option<Uuid>,
) -> Result<()> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM roles WHERE lower(name) = lower($1)
                        AND ($2::uuid IS NULL OR id <> $2))",
    )
    .bind(name)
    .bind(except)
    .fetch_one(&mut **tx)
    .await?;
    if taken {
        return rule(format!("there is already a role named {name:?}"));
    }
    Ok(())
}

fn clean_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return rule("a role needs a name");
    }
    Ok(name)
}

/// Creates a role; `by` must hold every permission it gets.
pub async fn create(
    db: &PgPool,
    by: &Grant,
    name: &str,
    permissions: &[Permission],
) -> Result<Uuid> {
    by.require(Permission::ManageRoles)?;
    let name = clean_name(name)?;
    let wanted = Grant {
        administrator: false,
        permissions: permissions.to_vec(),
    };
    by.require_covers(&wanted, "create a role")?;
    let mut tx = db.begin().await?;
    name_is_free(&mut tx, name, None).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO roles (id, name, permissions) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(name)
        .bind(to_stored(permissions))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

/// Renames a role and replaces its permissions; `by` must hold every permission it had and gets.
pub async fn update(
    db: &PgPool,
    by: &Grant,
    id: Uuid,
    name: &str,
    permissions: &[Permission],
) -> Result<()> {
    by.require(Permission::ManageRoles)?;
    let name = clean_name(name)?;
    let mut tx = db.begin().await?;
    let (current, _) = grant_of(&mut tx, id).await?;
    if current.administrator {
        return rule("the Administrator role cannot be changed");
    }
    by.require_covers(&current, "change a role")?;
    let wanted = Grant {
        administrator: false,
        permissions: permissions.to_vec(),
    };
    by.require_covers(&wanted, "change a role")?;
    name_is_free(&mut tx, name, Some(id)).await?;
    sqlx::query("UPDATE roles SET name = $2, permissions = $3 WHERE id = $1")
        .bind(id)
        .bind(name)
        .bind(to_stored(permissions))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Makes the role the one new agents get when none is named.
pub async fn make_default(db: &PgPool, by: &Grant, id: Uuid) -> Result<()> {
    by.require(Permission::ManageRoles)?;
    let mut tx = db.begin().await?;
    let (grant, _) = grant_of(&mut tx, id).await?;
    if grant.administrator {
        return rule("the Administrator role cannot be the default for new agents");
    }
    sqlx::query("UPDATE roles SET is_default = false WHERE is_default AND id <> $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE roles SET is_default = true WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes a role no agent has; never the Administrator or the default role.
pub async fn remove(db: &PgPool, by: &Grant, id: Uuid) -> Result<()> {
    by.require(Permission::ManageRoles)?;
    let mut tx = db.begin().await?;
    let (grant, default) = grant_of(&mut tx, id).await?;
    if grant.administrator {
        return rule("the Administrator role cannot be removed");
    }
    if default {
        return rule(
            "the default role for new agents cannot be removed: make another one the default first",
        );
    }
    by.require_covers(&grant, "remove a role")?;
    let used: i64 = sqlx::query_scalar("SELECT count(*) FROM agents WHERE role_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if used > 0 {
        return rule(format!(
            "{used} agent(s) have this role: give them another one first"
        ));
    }
    sqlx::query("DELETE FROM roles WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(permissions: &[Permission]) -> Grant {
        Grant {
            administrator: false,
            permissions: permissions.to_vec(),
        }
    }

    #[test]
    fn names_round_trip_and_serde_uses_them() {
        for p in Permission::ALL {
            assert_eq!(Permission::parse(p.as_str()), Some(p));
            assert_eq!(
                serde_json::to_value(p).unwrap(),
                serde_json::Value::String(p.as_str().to_owned())
            );
        }
    }

    #[test]
    fn the_administrator_has_everything_and_covers_everyone() {
        let admin = Grant::command_line();
        assert_eq!(admin.effective(), Permission::ALL.to_vec());
        assert!(admin.covers(&Grant::command_line()));
        assert!(admin.covers(&role(&[Permission::Reply])));
    }

    #[test]
    fn a_role_covers_only_what_it_holds() {
        let manager = role(&[Permission::ManageAgents, Permission::Reply]);
        assert!(manager.covers(&role(&[Permission::Reply])));
        assert!(manager.covers(&role(&[])));
        assert!(!manager.covers(&role(&[Permission::ManageRoles])));
        assert!(!manager.covers(&Grant::command_line()));
        assert!(matches!(
            manager.require(Permission::ManageRoles),
            Err(Error::Forbidden(_))
        ));
    }

    #[sqlx::test]
    async fn the_database_accepts_every_permission_and_seeds_two_roles(db: PgPool) {
        let roles = list(&db).await.unwrap();
        assert_eq!(roles.len(), 2);
        assert!(roles[0].administrator && !roles[0].default);
        assert!(roles[1].default && !roles[1].administrator);
        let id = create(&db, &Grant::command_line(), "Everything", &Permission::ALL)
            .await
            .unwrap();
        let (grant, _) = {
            let mut tx = db.begin().await.unwrap();
            grant_of(&mut tx, id).await.unwrap()
        };
        assert_eq!(grant.permissions, Permission::ALL.to_vec());
    }

    #[sqlx::test]
    async fn roles_are_guarded(db: PgPool) {
        let cli = Grant::command_line();
        let roles = list(&db).await.unwrap();
        let (admin, default) = (roles[0].id, roles[1].id);
        assert!(matches!(
            update(&db, &cli, admin, "Boss", &[]).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            remove(&db, &cli, admin).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            remove(&db, &cli, default).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            create(&db, &cli, " agent ", &[]).await,
            Err(Error::Rule(_))
        ));

        let sorter = create(&db, &cli, "Sorter", &[Permission::SortInbox])
            .await
            .unwrap();
        let roles_manager = role(&[Permission::ManageRoles, Permission::SortInbox]);
        // Nobody hands out what they do not hold.
        assert!(matches!(
            create(&db, &roles_manager, "Mail", &[Permission::ViewMail]).await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            update(&db, &roles_manager, default, "Agent", &[]).await,
            Err(Error::Forbidden(_))
        ));
        update(
            &db,
            &roles_manager,
            sorter,
            "Inbox",
            &[Permission::SortInbox],
        )
        .await
        .unwrap();
        make_default(&db, &roles_manager, sorter).await.unwrap();
        let roles = list(&db).await.unwrap();
        assert!(roles.iter().any(|r| r.name == "Inbox" && r.default));
        assert_eq!(roles.iter().filter(|r| r.default).count(), 1);
        remove(&db, &cli, default).await.unwrap();
    }
}
