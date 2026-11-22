//! Managing agents: adding them, their roles, passwords and deactivation (ADR 0007).

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth;
use crate::error::{Error, Result, rule};
use crate::roles::{self, Grant, Permission};

/// The shortest password an agent may choose; generated ones are longer.
pub const MIN_PASSWORD_CHARS: usize = 12;

/// An agent as those who manage agents see them.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct AgentAccount {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub role_id: Uuid,
    pub role_name: String,
    /// Whether the agent's role is the Administrator role.
    pub administrator: bool,
    /// When the agent was deactivated; empty while they work at the help desk.
    #[serde(with = "time::serde::rfc3339::option")]
    pub deactivated_at: Option<OffsetDateTime>,
}

/// Every agent: active ones first, then by name.
pub async fn list(db: &PgPool) -> Result<Vec<AgentAccount>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.email, a.name, a.role_id, r.name AS role_name,
                r.is_administrator AS administrator, a.deactivated_at
         FROM agents a JOIN roles r ON r.id = a.role_id
         ORDER BY a.deactivated_at IS NOT NULL, lower(a.name)",
    )
    .fetch_all(db)
    .await?)
}

pub async fn id_by_email(db: &PgPool, email: &str) -> Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM agents WHERE email = $1")
        .bind(email.trim().to_lowercase())
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound("agent"))
}

fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return rule(format!(
            "a password needs at least {MIN_PASSWORD_CHARS} characters"
        ));
    }
    Ok(())
}

/// A password for an agent to use until they choose their own.
pub fn generate_password() -> String {
    // Two v4 UUIDs: 244 random bits from the operating system's generator.
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// A new agent; the password, when the caller gave none, is generated and returned once.
pub struct Added {
    pub id: Uuid,
    pub generated_password: Option<String>,
}

/// Adds an agent with `role`, or else the Administrator role for the very first agent and the
/// default role for later ones. `by` must hold every permission the role has.
pub async fn add(
    db: &PgPool,
    by: &Grant,
    email: &str,
    name: &str,
    role: Option<Uuid>,
    password: Option<&str>,
) -> Result<Added> {
    by.require(Permission::ManageAgents)?;
    let email = email.trim().to_lowercase();
    if !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        return rule("an agent needs an e-mail address");
    }
    let name = name.trim();
    if name.is_empty() {
        return rule("an agent needs a name");
    }
    if let Some(password) = password {
        check_password(password)?;
    }
    let mut tx = db.begin().await?;
    let role = match role {
        Some(role) => role,
        None => {
            let first: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM agents)")
                .fetch_one(&mut *tx)
                .await?;
            let id: Option<Uuid> = if first {
                Some(roles::administrator_id(&mut tx).await?)
            } else {
                sqlx::query_scalar("SELECT id FROM roles WHERE is_default")
                    .fetch_optional(&mut *tx)
                    .await?
            };
            id.ok_or_else(|| {
                Error::Rule("there is no default role for new agents: name one".into())
            })?
        }
    };
    let (grant, _) = roles::grant_of(&mut tx, role).await?;
    by.require_covers(&grant, "give a role")?;
    let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agents WHERE email = $1)")
        .bind(&email)
        .fetch_one(&mut *tx)
        .await?;
    if taken {
        return rule(format!(
            "there is already an agent with the address {email}"
        ));
    }
    let generated = password.is_none().then(generate_password);
    let password = password.or(generated.as_deref()).unwrap_or_default();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, email, name, password_hash, role_id) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(&email)
    .bind(name)
    .bind(auth::hash_password(password)?)
    .bind(role)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Added {
        id,
        generated_password: generated,
    })
}

/// Locks the Administrator role, so that two changes cannot each think the other leaves an
/// Administrator in place, and checks that `by` holds everything the agent's role allows.
async fn lock_for_change(
    tx: &mut Transaction<'_, Postgres>,
    by: &Grant,
    agent: Uuid,
    what: &str,
) -> Result<()> {
    sqlx::query("SELECT 1 FROM roles WHERE is_administrator FOR UPDATE")
        .execute(&mut **tx)
        .await?;
    let role: Option<Uuid> =
        sqlx::query_scalar("SELECT role_id FROM agents WHERE id = $1 FOR UPDATE")
            .bind(agent)
            .fetch_optional(&mut **tx)
            .await?;
    let role = role.ok_or(Error::NotFound("agent"))?;
    let (grant, _) = roles::grant_of(tx, role).await?;
    by.require_covers(&grant, what)
}

/// Refuses a change that leaves no active agent with the Administrator role.
async fn an_administrator_remains(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agents a JOIN roles r ON r.id = a.role_id
         WHERE r.is_administrator AND a.deactivated_at IS NULL",
    )
    .fetch_one(&mut **tx)
    .await?;
    if remaining == 0 {
        return rule("at least one active agent must keep the Administrator role");
    }
    Ok(())
}

/// Gives the agent another role.
pub async fn set_role(db: &PgPool, by: &Grant, agent: Uuid, role: Uuid) -> Result<()> {
    by.require(Permission::ManageAgents)?;
    let mut tx = db.begin().await?;
    lock_for_change(&mut tx, by, agent, "change the role of this agent").await?;
    let (grant, _) = roles::grant_of(&mut tx, role).await?;
    by.require_covers(&grant, "give this role")?;
    sqlx::query("UPDATE agents SET role_id = $2 WHERE id = $1")
        .bind(agent)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    an_administrator_remains(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Sets another agent's password, the given one or a generated one (returned), and ends all of
/// that agent's sessions.
pub async fn reset_password(
    db: &PgPool,
    by: &Grant,
    agent: Uuid,
    password: Option<&str>,
) -> Result<Option<String>> {
    by.require(Permission::ManageAgents)?;
    if let Some(password) = password {
        check_password(password)?;
    }
    let mut tx = db.begin().await?;
    lock_for_change(&mut tx, by, agent, "reset the password of this agent").await?;
    let generated = password.is_none().then(generate_password);
    let password = password.or(generated.as_deref()).unwrap_or_default();
    sqlx::query("UPDATE agents SET password_hash = $2 WHERE id = $1")
        .bind(agent)
        .bind(auth::hash_password(password)?)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM agent_sessions WHERE agent_id = $1")
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(generated)
}

/// An agent changes their own password; every session but `keep_session` ends.
pub async fn change_own_password(
    db: &PgPool,
    agent: Uuid,
    keep_session: &str,
    current: &str,
    new: &str,
) -> Result<()> {
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM agents WHERE id = $1")
        .bind(agent)
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound("agent"))?;
    if !auth::verify_password(current, &hash) {
        return Err(Error::Forbidden("the current password is wrong".into()));
    }
    check_password(new)?;
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE agents SET password_hash = $2 WHERE id = $1")
        .bind(agent)
        .bind(auth::hash_password(new)?)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM agent_sessions WHERE agent_id = $1 AND token_hash <> $2")
        .bind(agent)
        .bind(auth::token_hash(keep_session))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Deactivates an agent, who leaves the help desk: their sessions end and the tickets assigned to
/// them that are not closed lose their assignee. Returns how many tickets did.
pub async fn deactivate(db: &PgPool, by: &Grant, agent: Uuid) -> Result<u64> {
    by.require(Permission::ManageAgents)?;
    let mut tx = db.begin().await?;
    lock_for_change(&mut tx, by, agent, "deactivate this agent").await?;
    sqlx::query("UPDATE agents SET deactivated_at = coalesce(deactivated_at, now()) WHERE id = $1")
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM agent_sessions WHERE agent_id = $1")
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    let unassigned = sqlx::query(
        "UPDATE tickets SET assignee_id = NULL, updated_at = now()
         WHERE assignee_id = $1 AND state <> 'closed'",
    )
    .bind(agent)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    an_administrator_remains(&mut tx).await?;
    tx.commit().await?;
    Ok(unassigned)
}

/// Lets a deactivated agent sign in and be assigned again.
pub async fn reactivate(db: &PgPool, by: &Grant, agent: Uuid) -> Result<()> {
    by.require(Permission::ManageAgents)?;
    let mut tx = db.begin().await?;
    lock_for_change(&mut tx, by, agent, "reactivate this agent").await?;
    sqlx::query("UPDATE agents SET deactivated_at = NULL WHERE id = $1")
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn grant(permissions: &[Permission]) -> Grant {
        Grant {
            administrator: false,
            permissions: permissions.to_vec(),
        }
    }

    async fn role_named(db: &PgPool, name: &str) -> Uuid {
        roles::id_by_name(db, name).await.unwrap()
    }

    #[sqlx::test]
    async fn the_first_agent_is_an_administrator_and_later_ones_get_the_default(db: PgPool) {
        let cli = Grant::command_line();
        let first = add(&db, &cli, "Ada@Helpdesk.test", "Ada", None, None)
            .await
            .unwrap();
        assert_eq!(first.generated_password.as_ref().map(String::len), Some(64));
        let second = add(
            &db,
            &cli,
            "bo@helpdesk.test",
            "Bo",
            None,
            Some("twelve chars"),
        )
        .await
        .unwrap();
        assert!(second.generated_password.is_none());
        let agents = list(&db).await.unwrap();
        let ada = agents.iter().find(|a| a.id == first.id).unwrap();
        assert_eq!(ada.email, "ada@helpdesk.test");
        assert!(ada.administrator);
        let bo = agents.iter().find(|a| a.id == second.id).unwrap();
        assert_eq!(bo.role_name, "Agent");
        assert!(matches!(
            add(&db, &cli, "bo@helpdesk.test", "Bo", None, None).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            add(&db, &cli, "cy@helpdesk.test", "Cy", None, Some("short")).await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn the_last_administrator_stays(db: PgPool) {
        let cli = Grant::command_line();
        let ada = add(&db, &cli, "ada@helpdesk.test", "Ada", None, None)
            .await
            .unwrap()
            .id;
        let agent = role_named(&db, "Agent").await;
        assert!(matches!(
            set_role(&db, &cli, ada, agent).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            deactivate(&db, &cli, ada).await,
            Err(Error::Rule(_))
        ));

        let admin = role_named(&db, "Administrator").await;
        let bo = add(&db, &cli, "bo@helpdesk.test", "Bo", Some(admin), None)
            .await
            .unwrap()
            .id;
        deactivate(&db, &cli, bo).await.unwrap();
        // A deactivated Administrator does not count.
        assert!(matches!(
            set_role(&db, &cli, ada, agent).await,
            Err(Error::Rule(_))
        ));
        reactivate(&db, &cli, bo).await.unwrap();
        set_role(&db, &cli, ada, agent).await.unwrap();
    }

    #[sqlx::test]
    async fn nobody_hands_out_more_than_they_hold(db: PgPool) {
        let cli = Grant::command_line();
        add(&db, &cli, "ada@helpdesk.test", "Ada", None, None)
            .await
            .unwrap();
        let admin = role_named(&db, "Administrator").await;
        let agent = role_named(&db, "Agent").await;
        let manager = grant(&[Permission::ManageAgents, Permission::WorkTickets]);
        let workers = roles::create(&db, &cli, "Workers", &[Permission::WorkTickets])
            .await
            .unwrap();

        // The default role also allows replying, which the manager does not hold.
        assert!(matches!(
            add(&db, &manager, "bo@helpdesk.test", "Bo", None, None).await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            add(&db, &manager, "bo@helpdesk.test", "Bo", Some(admin), None).await,
            Err(Error::Forbidden(_))
        ));
        let bo = add(&db, &manager, "bo@helpdesk.test", "Bo", Some(workers), None)
            .await
            .unwrap()
            .id;
        assert!(matches!(
            set_role(&db, &manager, bo, agent).await,
            Err(Error::Forbidden(_))
        ));
        let ada = id_by_email(&db, "ada@helpdesk.test").await.unwrap();
        assert!(matches!(
            reset_password(&db, &manager, ada, None).await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            deactivate(&db, &manager, ada).await,
            Err(Error::Forbidden(_))
        ));
        reset_password(&db, &manager, bo, None).await.unwrap();
        assert!(matches!(
            deactivate(&db, &grant(&[Permission::WorkTickets]), bo).await,
            Err(Error::Forbidden(_))
        ));
    }

    #[sqlx::test]
    async fn deactivating_ends_sessions_and_unassigns_open_tickets(db: PgPool) {
        let cli = Grant::command_line();
        add(&db, &cli, "ada@helpdesk.test", "Ada", None, None)
            .await
            .unwrap();
        let bo = add(
            &db,
            &cli,
            "bo@helpdesk.test",
            "Bo",
            None,
            Some("bo's password"),
        )
        .await
        .unwrap()
        .id;
        let token = auth::sign_in(&db, "bo@helpdesk.test", "bo's password")
            .await
            .unwrap()
            .unwrap();
        let customer = testing::customer(&db, "Brightfield").await;
        let open = testing::ticket(&db, customer, None, "<a@brightfield.test>").await;
        let closed = testing::ticket(&db, customer, None, "<b@brightfield.test>").await;
        for number in [open, closed] {
            crate::tickets::assign(&db, number, Some(bo)).await.unwrap();
        }
        sqlx::query("UPDATE tickets SET state = 'closed' WHERE number = $1")
            .bind(closed)
            .execute(&db)
            .await
            .unwrap();

        assert_eq!(deactivate(&db, &cli, bo).await.unwrap(), 1);
        assert!(auth::agent_for(&db, &token).await.unwrap().is_none());
        assert!(
            auth::sign_in(&db, "bo@helpdesk.test", "bo's password")
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            crate::tickets::assign(&db, open, Some(bo)).await,
            Err(Error::Rule(_))
        ));
        let assignee = |number: i64| {
            sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT assignee_id FROM tickets WHERE number = $1",
            )
            .bind(number)
            .fetch_one(&db)
        };
        assert_eq!(assignee(open).await.unwrap(), None);
        assert_eq!(assignee(closed).await.unwrap(), Some(bo));

        reactivate(&db, &cli, bo).await.unwrap();
        assert!(
            auth::sign_in(&db, "bo@helpdesk.test", "bo's password")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[sqlx::test]
    async fn changing_ones_own_password_needs_the_current_one(db: PgPool) {
        let cli = Grant::command_line();
        let ada = add(
            &db,
            &cli,
            "ada@helpdesk.test",
            "Ada",
            None,
            Some("first password"),
        )
        .await
        .unwrap()
        .id;
        let here = auth::sign_in(&db, "ada@helpdesk.test", "first password")
            .await
            .unwrap()
            .unwrap();
        let elsewhere = auth::sign_in(&db, "ada@helpdesk.test", "first password")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            change_own_password(&db, ada, &here, "wrong password", "second password").await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            change_own_password(&db, ada, &here, "first password", "short").await,
            Err(Error::Rule(_))
        ));
        change_own_password(&db, ada, &here, "first password", "second password")
            .await
            .unwrap();
        assert!(auth::agent_for(&db, &here).await.unwrap().is_some());
        assert!(auth::agent_for(&db, &elsewhere).await.unwrap().is_none());
        assert!(
            auth::sign_in(&db, "ada@helpdesk.test", "second password")
                .await
                .unwrap()
                .is_some()
        );
    }
}
