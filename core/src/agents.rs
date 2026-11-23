//! Managing agents: adding them, their roles, passwords, how they sign in and deactivation (ADR
//! 0007).

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
    /// Whether the agent has a local password.
    pub has_local_password: bool,
    /// Whether the agent has an identity of an identity provider linked to them.
    pub has_linked_identity: bool,
    /// When the agent last signed in, with a local password or through the identity provider;
    /// empty if they never did.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_signed_in_at: Option<OffsetDateTime>,
}

/// Every agent: active ones first, then by name.
pub async fn list(db: &PgPool) -> Result<Vec<AgentAccount>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.email, a.name, a.role_id, r.name AS role_name,
                r.is_administrator AS administrator, a.deactivated_at,
                a.password_hash IS NOT NULL AS has_local_password,
                EXISTS (SELECT 1 FROM agent_identities i WHERE i.agent_id = a.id)
                    AS has_linked_identity,
                a.last_signed_in_at
         FROM agents a JOIN roles r ON r.id = a.role_id
         ORDER BY a.deactivated_at IS NOT NULL, lower(a.name)",
    )
    .fetch_all(db)
    .await?)
}

/// An e-mail address as agents' addresses are kept: trimmed and in lower case.
pub fn normalized_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Whether an agent other than `except` has the (normalized) address `email`.
pub async fn email_taken(
    db: impl sqlx::PgExecutor<'_>,
    email: &str,
    except: Option<Uuid>,
) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM agents WHERE email = $1 AND id IS DISTINCT FROM $2)",
    )
    .bind(email)
    .bind(except)
    .fetch_one(db)
    .await
}

pub async fn id_by_email(db: &PgPool, email: &str) -> Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM agents WHERE email = $1")
        .bind(normalized_email(email))
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
    let (email, name) = email_and_name(email, name)?;
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
            if first {
                roles::administrator_id(&mut tx).await?
            } else {
                default_role(&mut tx).await?
            }
        }
    };
    let (grant, _) = roles::grant_of(&mut tx, role).await?;
    by.require_covers(&grant, "give a role")?;
    let generated = password.is_none().then(generate_password);
    let password = password.or(generated.as_deref()).unwrap_or_default();
    let id = insert(
        &mut tx,
        &email,
        name,
        role,
        Some(&auth::hash_password(password)?),
    )
    .await?;
    tx.commit().await?;
    Ok(Added {
        id,
        generated_password: generated,
    })
}

/// The role new agents get when none is named.
pub(crate) async fn default_role(tx: &mut Transaction<'_, Postgres>) -> Result<Uuid> {
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM roles WHERE is_default")
        .fetch_optional(&mut **tx)
        .await?;
    id.ok_or_else(|| Error::Rule("there is no default role for new agents: name one".into()))
}

/// Inserts an agent with `role`, and with the hash of their local password or none: every new
/// agent goes through here, whoever makes them. The address and the name are kept as every
/// agent's are, and no other agent may have the address.
pub(crate) async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    email: &str,
    name: &str,
    role: Uuid,
    password_hash: Option<&str>,
) -> Result<Uuid> {
    let (email, name) = email_and_name(email, name)?;
    if email_taken(&mut **tx, &email, None).await? {
        return rule(format!(
            "there is already an agent with the address {email}"
        ));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, email, name, password_hash, role_id) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(&email)
    .bind(name)
    .bind(password_hash)
    .bind(role)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Whether a (normalized) address can be an agent's: something on both sides of an `@`.
pub fn is_address(email: &str) -> bool {
    email.contains('@') && !email.starts_with('@') && !email.ends_with('@')
}

/// The e-mail address, in lower case, and the name, trimmed, of an agent.
fn email_and_name<'a>(email: &str, name: &'a str) -> Result<(String, &'a str)> {
    let email = normalized_email(email);
    if !is_address(&email) {
        return rule("an agent needs an e-mail address");
    }
    let name = name.trim();
    if name.is_empty() {
        return rule("an agent needs a name");
    }
    Ok((email, name))
}

/// Changes an agent's e-mail address and name. Those of an agent linked to an identity
/// provider come from it and are not edited here.
pub async fn edit(db: &PgPool, by: &Grant, agent: Uuid, email: &str, name: &str) -> Result<()> {
    by.require(Permission::ManageAgents)?;
    let (email, name) = email_and_name(email, name)?;
    let mut tx = db.begin().await?;
    lock_for_change(
        &mut tx,
        by,
        agent,
        "change the name and e-mail of this agent",
    )
    .await?;
    let linked: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agent_identities WHERE agent_id = $1)")
            .bind(agent)
            .fetch_one(&mut *tx)
            .await?;
    if linked {
        return rule(
            "this agent signs in through the identity provider, which their name and e-mail \
             come from",
        );
    }
    if email_taken(&mut *tx, &email, Some(agent)).await? {
        return rule(format!(
            "there is already an agent with the address {email}"
        ));
    }
    sqlx::query("UPDATE agents SET email = $2, name = $3 WHERE id = $1")
        .bind(agent)
        .bind(&email)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
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

/// An agent changes their own password; every session but `keep_session` ends. An agent without
/// a local password has none to change.
pub async fn change_own_password(
    db: &PgPool,
    agent: Uuid,
    keep_session: &str,
    current: &str,
    new: &str,
) -> Result<()> {
    let hash: Option<String> = sqlx::query_scalar("SELECT password_hash FROM agents WHERE id = $1")
        .bind(agent)
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound("agent"))?;
    let Some(hash) = hash else {
        return rule(
            "you have no local password: you sign in through the identity provider, and an \
             administrator can give you a password",
        );
    };
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

/// Refuses a change of how an agent signs in made by that agent: another agent who manages
/// agents makes it, so that nobody takes away the way they are signed in with by mistake.
fn not_oneself(actor: Uuid, agent: Uuid) -> Result<()> {
    if actor == agent {
        return rule(
            "you cannot change how you sign in yourself: another agent who manages agents can",
        );
    }
    Ok(())
}

/// How the agent can sign in, read under the lock of [`lock_for_change`]: whether they have a
/// local password, and the issuers of the identity providers whose identities are linked to them.
async fn ways_to_sign_in(
    tx: &mut Transaction<'_, Postgres>,
    agent: Uuid,
) -> Result<(bool, Vec<String>)> {
    let has_local_password: bool =
        sqlx::query_scalar("SELECT password_hash IS NOT NULL FROM agents WHERE id = $1")
            .bind(agent)
            .fetch_one(&mut **tx)
            .await?;
    let issuers: Vec<String> =
        sqlx::query_scalar("SELECT issuer FROM agent_identities WHERE agent_id = $1")
            .bind(agent)
            .fetch_all(&mut **tx)
            .await?;
    Ok((has_local_password, issuers))
}

/// Unlinks the agent's identities, which ends the sessions they opened through the identity
/// provider. Refused unless the agent has a local password: linking again at their next sign-in
/// needs the provider to vouch for their e-mail and the authorized group to let them in, so
/// without a password they could be left with no way to sign in.
pub async fn unlink_identity(db: &PgPool, by: &Grant, actor: Uuid, agent: Uuid) -> Result<()> {
    by.require(Permission::ManageAgents)?;
    not_oneself(actor, agent)?;
    let mut tx = db.begin().await?;
    lock_for_change(&mut tx, by, agent, "unlink the identity of this agent").await?;
    let (has_local_password, issuers) = ways_to_sign_in(&mut tx, agent).await?;
    if issuers.is_empty() {
        return rule("this agent has no identity of an identity provider linked to them");
    }
    if !has_local_password {
        return rule(
            "this agent has no local password: unlinked, they might not be able to sign in at \
             all: reset their password first",
        );
    }
    // The database ends the sessions opened through the identity provider (migration 0030).
    sqlx::query("DELETE FROM agent_identities WHERE agent_id = $1")
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes the agent's local password, which ends the sessions they opened with it. `provider`
/// is the issuer of the identity provider configured now, if any. Refused unless an identity of
/// that provider is linked to the agent: without it they could not sign in at all.
pub async fn remove_local_password(
    db: &PgPool,
    by: &Grant,
    actor: Uuid,
    agent: Uuid,
    provider: Option<&str>,
) -> Result<()> {
    by.require(Permission::ManageAgents)?;
    not_oneself(actor, agent)?;
    let mut tx = db.begin().await?;
    lock_for_change(
        &mut tx,
        by,
        agent,
        "remove the local password of this agent",
    )
    .await?;
    let (has_local_password, issuers) = ways_to_sign_in(&mut tx, agent).await?;
    if !has_local_password {
        return rule("this agent has no local password");
    }
    if !provider.is_some_and(|provider| issuers.iter().any(|issuer| issuer == provider)) {
        return rule(
            "this agent has not signed in through the identity provider configured now: without \
             their local password they could not sign in at all",
        );
    }
    // The database ends the sessions opened with the password (migration 0030).
    sqlx::query("UPDATE agents SET password_hash = NULL WHERE id = $1")
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
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

    /// An agent without a local password has none to change; whoever manages agents, or the
    /// command line, can give them one.
    #[sqlx::test]
    async fn an_agent_without_a_local_password_is_given_one_by_who_manages_agents(db: PgPool) {
        let cli = Grant::command_line();
        add(&db, &cli, "ada@helpdesk.test", "Ada", None, None)
            .await
            .unwrap();
        let vera = auth::tests::agent_without_local_password(&db, "vera.lind@example.com").await;
        let session = auth::open_session(
            &db,
            vera,
            auth::OpenedWith::OpenIdConnect,
            time::Duration::HOUR,
        )
        .await
        .unwrap();
        assert!(matches!(
            change_own_password(&db, vera, &session, "", "a new password").await,
            Err(Error::Rule(_))
        ));
        assert!(
            auth::sign_in(&db, "vera.lind@example.com", "a new password")
                .await
                .unwrap()
                .is_none()
        );
        assert!(list(&db).await.unwrap().iter().any(|a| a.id == vera));

        reset_password(&db, &cli, vera, Some("vera's own password"))
            .await
            .unwrap();
        assert!(
            auth::sign_in(&db, "vera.lind@example.com", "vera's own password")
                .await
                .unwrap()
                .is_some()
        );
    }

    fn identity_of(subject: &str, email: &str) -> crate::identities::Identity {
        crate::identities::Identity {
            issuer: ISSUER.to_owned(),
            subject: subject.to_owned(),
            email: Some(email.to_owned()),
            email_verified: true,
            name: None,
            authorized_group: crate::identities::AuthorizedGroup::NotConfigured,
        }
    }

    const ISSUER: &str = "https://login.example.com/realms/staff";

    /// Signs the agent with `email` in through the identity provider, linking them the first
    /// time; the session's token.
    async fn through_provider(db: &PgPool, subject: &str, email: &str) -> String {
        crate::identities::sign_in(db, &identity_of(subject, email), time::Duration::hours(12))
            .await
            .unwrap()
            .unwrap()
            .token
    }

    fn listed(agents: &[AgentAccount], id: Uuid) -> &AgentAccount {
        agents.iter().find(|a| a.id == id).unwrap()
    }

    #[sqlx::test]
    async fn the_list_shows_how_each_agent_signs_in_and_when_they_last_did(db: PgPool) {
        let cli = Grant::command_line();
        let ada = add(
            &db,
            &cli,
            "ada@helpdesk.test",
            "Ada",
            None,
            Some("ada's password"),
        )
        .await
        .unwrap()
        .id;
        let lena = add(&db, &cli, "lena.marsh@example.com", "Lena", None, None)
            .await
            .unwrap()
            .id;
        let vera = auth::tests::agent_without_local_password(&db, "vera.lind@example.com").await;

        let agents = list(&db).await.unwrap();
        let ada_listed = listed(&agents, ada);
        assert!(ada_listed.has_local_password && !ada_listed.has_linked_identity);
        assert_eq!(ada_listed.last_signed_in_at, None);
        let vera_listed = listed(&agents, vera);
        assert!(!vera_listed.has_local_password && !vera_listed.has_linked_identity);

        let token = auth::sign_in(&db, "ada@helpdesk.test", "ada's password")
            .await
            .unwrap()
            .unwrap();
        through_provider(&db, "sub-lena", "lena.marsh@example.com").await;
        // Signing out ends the session, not the record of the sign-in.
        auth::sign_out(&db, &token).await.unwrap();

        let agents = list(&db).await.unwrap();
        assert!(listed(&agents, ada).last_signed_in_at.is_some());
        let lena_listed = listed(&agents, lena);
        assert!(lena_listed.has_local_password && lena_listed.has_linked_identity);
        assert!(lena_listed.last_signed_in_at.is_some());
        assert_eq!(listed(&agents, vera).last_signed_in_at, None);
    }

    /// Every session opened counts as a sign-in, however it is opened: the database keeps the
    /// time, so that no way of opening one forgets it.
    #[sqlx::test]
    async fn the_database_records_every_sign_in(db: PgPool) {
        let vera = auth::tests::agent_without_local_password(&db, "vera.lind@example.com").await;
        sqlx::query(
            "INSERT INTO agent_sessions (token_hash, agent_id, opened_with, expires_at)
             VALUES ('\\x01', $1, 'openid_connect', now() + interval '1 hour')",
        )
        .bind(vera)
        .execute(&db)
        .await
        .unwrap();
        let (last, opened): (Option<OffsetDateTime>, OffsetDateTime) = sqlx::query_as(
            "SELECT a.last_signed_in_at, s.created_at
             FROM agents a JOIN agent_sessions s ON s.agent_id = a.id WHERE a.id = $1",
        )
        .bind(vera)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(last, Some(opened));
    }

    /// Whether the session still works.
    async fn works(db: &PgPool, token: &str) -> bool {
        auth::agent_for(db, token).await.unwrap().is_some()
    }

    /// Ada, the first agent and an Administrator, who acts; and Lena, with a local password and
    /// an identity of the provider, signed in both ways: her two sessions' tokens.
    async fn lena_signed_in_both_ways(db: &PgPool) -> (Uuid, Uuid, String, String) {
        let cli = Grant::command_line();
        let ada = add(db, &cli, "ada@helpdesk.test", "Ada", None, None)
            .await
            .unwrap()
            .id;
        let lena = add(
            db,
            &cli,
            "lena.marsh@example.com",
            "Lena",
            None,
            Some("lena's password"),
        )
        .await
        .unwrap()
        .id;
        let with_password = auth::sign_in(db, "lena.marsh@example.com", "lena's password")
            .await
            .unwrap()
            .unwrap();
        let through_provider = through_provider(db, "sub-lena", "lena.marsh@example.com").await;
        (ada, lena, with_password, through_provider)
    }

    #[sqlx::test]
    async fn unlinking_an_identity_ends_the_sessions_opened_through_it(db: PgPool) {
        let (ada, lena, with_password, through) = lena_signed_in_both_ways(&db).await;
        unlink_identity(&db, &Grant::command_line(), ada, lena)
            .await
            .unwrap();
        assert!(!works(&db, &through).await);
        assert!(works(&db, &with_password).await);
        assert!(!listed(&list(&db).await.unwrap(), lena).has_linked_identity);
        assert!(matches!(
            unlink_identity(&db, &Grant::command_line(), ada, lena).await,
            Err(Error::Rule(_))
        ));
    }

    #[sqlx::test]
    async fn removing_a_local_password_ends_the_sessions_opened_with_it(db: PgPool) {
        let (ada, lena, with_password, through) = lena_signed_in_both_ways(&db).await;
        remove_local_password(&db, &Grant::command_line(), ada, lena, Some(ISSUER))
            .await
            .unwrap();
        assert!(!works(&db, &with_password).await);
        assert!(works(&db, &through).await);
        assert!(!listed(&list(&db).await.unwrap(), lena).has_local_password);
        assert!(
            auth::sign_in(&db, "lena.marsh@example.com", "lena's password")
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            remove_local_password(&db, &Grant::command_line(), ada, lena, Some(ISSUER)).await,
            Err(Error::Rule(_))
        ));
    }

    /// Neither change may leave the agent without a sure way to sign in: a local password needs
    /// an identity linked at the identity provider configured now to be removed, and an identity
    /// needs a local password to be unlinked, since linking again depends on the provider.
    #[sqlx::test]
    async fn an_agent_keeps_a_way_to_sign_in(db: PgPool) {
        let cli = Grant::command_line();
        let (ada, lena, _, _) = lena_signed_in_both_ways(&db).await;
        let bo = add(&db, &cli, "bo@helpdesk.test", "Bo", None, None)
            .await
            .unwrap()
            .id;
        assert!(matches!(
            remove_local_password(&db, &cli, ada, bo, Some(ISSUER)).await,
            Err(Error::Rule(_))
        ));
        let elsewhere = Some("https://login.example.com/realms/other");
        for provider in [None, elsewhere] {
            assert!(matches!(
                remove_local_password(&db, &cli, ada, lena, provider).await,
                Err(Error::Rule(_))
            ));
        }
        remove_local_password(&db, &cli, ada, lena, Some(ISSUER))
            .await
            .unwrap();
        assert!(matches!(
            unlink_identity(&db, &cli, ada, lena).await,
            Err(Error::Rule(_))
        ));
        assert!(listed(&list(&db).await.unwrap(), lena).has_linked_identity);
        // With a password again, she can be unlinked; she links again by her verified address at
        // her next sign-in.
        reset_password(&db, &cli, lena, Some("lena's new password"))
            .await
            .unwrap();
        unlink_identity(&db, &cli, ada, lena).await.unwrap();
        through_provider(&db, "sub-lena", "lena.marsh@example.com").await;
        assert!(listed(&list(&db).await.unwrap(), lena).has_linked_identity);
    }

    /// The rules for acting on an agent: managing agents, every permission of their role, only
    /// an Administrator on an Administrator; and nobody on themselves.
    #[sqlx::test]
    async fn sign_in_is_administered_under_the_rules_for_acting_on_an_agent(db: PgPool) {
        let cli = Grant::command_line();
        let (ada, lena, _, _) = lena_signed_in_both_ways(&db).await;
        let workers = roles::create(&db, &cli, "Workers", &[Permission::WorkTickets])
            .await
            .unwrap();
        let piet = add(
            &db,
            &cli,
            "piet@example.com",
            "Piet",
            Some(workers),
            Some("piet's password"),
        )
        .await
        .unwrap()
        .id;
        through_provider(&db, "sub-piet", "piet@example.com").await;
        let manager = grant(&[Permission::ManageAgents, Permission::WorkTickets]);
        let manager_id = add(&db, &cli, "max@helpdesk.test", "Max", Some(workers), None)
            .await
            .unwrap()
            .id;

        // Lena's role, the default, allows replying, which the manager does not hold.
        assert!(matches!(
            unlink_identity(&db, &manager, manager_id, lena).await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            remove_local_password(&db, &manager, manager_id, lena, Some(ISSUER)).await,
            Err(Error::Forbidden(_))
        ));
        let worker = grant(&[Permission::WorkTickets]);
        assert!(matches!(
            unlink_identity(&db, &worker, manager_id, piet).await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            remove_local_password(&db, &worker, manager_id, piet, Some(ISSUER)).await,
            Err(Error::Forbidden(_))
        ));
        // Only an Administrator acts on an Administrator.
        assert!(matches!(
            unlink_identity(&db, &manager, manager_id, ada).await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            remove_local_password(&db, &manager, manager_id, ada, Some(ISSUER)).await,
            Err(Error::Forbidden(_))
        ));
        assert!(matches!(
            unlink_identity(&db, &cli, ada, Uuid::new_v4()).await,
            Err(Error::NotFound(_))
        ));
        // Not on oneself: another agent who manages agents does it.
        assert!(matches!(
            unlink_identity(&db, &cli, lena, lena).await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            remove_local_password(&db, &cli, lena, lena, Some(ISSUER)).await,
            Err(Error::Rule(_))
        ));

        remove_local_password(&db, &manager, manager_id, piet, Some(ISSUER))
            .await
            .unwrap();
        reset_password(&db, &manager, piet, Some("piet's new password"))
            .await
            .unwrap();
        unlink_identity(&db, &manager, manager_id, piet)
            .await
            .unwrap();
        assert!(matches!(
            unlink_identity(&db, &cli, ada, lena).await,
            Ok(())
        ));
    }

    /// The database ends the sessions of a way to sign in that is taken away, however it is.
    #[sqlx::test]
    async fn the_database_ends_the_sessions_of_a_way_to_sign_in_taken_away(db: PgPool) {
        let (_, lena, with_password, through) = lena_signed_in_both_ways(&db).await;
        sqlx::query("DELETE FROM agent_identities WHERE agent_id = $1")
            .bind(lena)
            .execute(&db)
            .await
            .unwrap();
        assert!(!works(&db, &through).await);
        assert!(works(&db, &with_password).await);

        let through = through_provider(&db, "sub-lena", "lena.marsh@example.com").await;
        sqlx::query("UPDATE agents SET password_hash = NULL WHERE id = $1")
            .bind(lena)
            .execute(&db)
            .await
            .unwrap();
        assert!(!works(&db, &with_password).await);
        assert!(works(&db, &through).await);
    }

    #[sqlx::test]
    async fn a_linked_agents_name_and_email_are_not_edited(db: PgPool) {
        let cli = Grant::command_line();
        add(&db, &cli, "ada@helpdesk.test", "Ada", None, None)
            .await
            .unwrap();
        let bo = add(&db, &cli, "bo@example.com", "Bo", None, None)
            .await
            .unwrap()
            .id;
        let lena = add(&db, &cli, "lena.marsh@example.com", "Lena", None, None)
            .await
            .unwrap()
            .id;

        edit(&db, &cli, bo, " Bo.Lund@Example.com ", " Bo Lund ")
            .await
            .unwrap();
        let bo_now = list(&db).await.unwrap().into_iter().find(|a| a.id == bo);
        let bo_now = bo_now.unwrap();
        assert_eq!(
            (bo_now.email.as_str(), bo_now.name.as_str()),
            ("bo.lund@example.com", "Bo Lund")
        );
        assert!(matches!(
            edit(&db, &cli, bo, "ada@helpdesk.test", "Bo Lund").await,
            Err(Error::Rule(_))
        ));
        assert!(matches!(
            edit(
                &db,
                &grant(&[Permission::WorkTickets]),
                bo,
                "bo@example.com",
                "Bo"
            )
            .await,
            Err(Error::Forbidden(_))
        ));

        let at_provider = crate::identities::Identity {
            issuer: "https://login.example.com/realms/staff".to_owned(),
            subject: "sub-lena".to_owned(),
            email: Some("lena.marsh@example.com".to_owned()),
            email_verified: true,
            name: Some("Lena Marsh".to_owned()),
            authorized_group: crate::identities::AuthorizedGroup::NotConfigured,
        };
        crate::identities::sign_in(&db, &at_provider, time::Duration::hours(12))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            edit(&db, &cli, lena, "lena.marsh@example.com", "Lena M.").await,
            Err(Error::Rule(_))
        ));
    }
}
