//! Agents' passwords and sessions.

use std::sync::LazyLock;

use anyhow::anyhow;
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::roles::Grant;

/// Name of the cookie that carries an agent's session token.
pub const SESSION_COOKIE: &str = "hd_session";
/// How long a session opened with a local password lasts.
pub const SESSION_DAYS: i32 = 14;

/// How a session was opened (docs/domain-rules.md, "Signing in").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenedWith {
    Password,
    OpenIdConnect,
}

impl OpenedWith {
    fn as_str(self) -> &'static str {
        match self {
            OpenedWith::Password => "password",
            OpenedWith::OpenIdConnect => "openid_connect",
        }
    }
}

/// Opens a session for the agent, lasting `lifetime`. Returns its token.
pub async fn open_session(
    db: impl sqlx::PgExecutor<'_>,
    agent: Uuid,
    opened_with: OpenedWith,
    lifetime: time::Duration,
) -> anyhow::Result<String> {
    let token = crate::agents::generate_password();
    sqlx::query(
        "INSERT INTO agent_sessions (token_hash, agent_id, opened_with, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
    )
    .bind(token_hash(&token))
    .bind(agent)
    .bind(opened_with.as_str())
    .bind(lifetime.as_seconds_f64())
    .execute(db)
    .await?;
    Ok(token)
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow!("hashing the password: {error}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

pub(crate) fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Checks the credentials and opens a session. Returns the session token.
pub async fn sign_in(db: &PgPool, email: &str, password: &str) -> anyhow::Result<Option<String>> {
    let agent: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, password_hash FROM agents WHERE email = $1 AND deactivated_at IS NULL",
    )
    .bind(email.trim().to_lowercase())
    .fetch_optional(db)
    .await?;
    let Some((agent_id, hash)) = agent else {
        // Spend the same time as a wrong password, so that timing does not reveal addresses.
        let _ = verify_password(password, &DUMMY_HASH);
        return Ok(None);
    };
    if !verify_password(password, &hash) {
        return Ok(None);
    }
    let lifetime = time::Duration::days(SESSION_DAYS.into());
    let token = open_session(db, agent_id, OpenedWith::Password, lifetime).await?;
    Ok(Some(token))
}

pub async fn sign_out(db: &PgPool, token: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM agent_sessions WHERE token_hash = $1")
        .bind(token_hash(token))
        .execute(db)
        .await?;
    Ok(())
}

/// The agent a session token belongs to, with what their role allows now, if the session is
/// still valid and the agent not deactivated.
pub async fn agent_for(db: &PgPool, token: &str) -> anyhow::Result<Option<SignedIn>> {
    let row: Option<SessionRow> = sqlx::query_as(
        "SELECT a.id, a.email, a.name, r.name, r.is_administrator, r.permissions
         FROM agent_sessions s JOIN agents a ON a.id = s.agent_id JOIN roles r ON r.id = a.role_id
         WHERE s.token_hash = $1 AND s.expires_at > now() AND a.deactivated_at IS NULL",
    )
    .bind(token_hash(token))
    .fetch_optional(db)
    .await?;
    Ok(row.map(
        |(id, email, name, role_name, administrator, permissions)| SignedIn {
            id,
            email,
            name,
            role_name,
            grant: Grant::from_stored(administrator, &permissions),
            token: token.to_owned(),
        },
    ))
}

type SessionRow = (Uuid, String, String, String, bool, Vec<String>);

#[derive(Debug, Clone)]
pub struct SignedIn {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub role_name: String,
    /// What the agent's role allows, read with the session on every request.
    pub grant: Grant,
    /// The session's token, so that changing one's password can keep this session.
    pub token: String,
}

/// A hash with the same cost as real ones, checked when the e-mail is unknown.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_password("no agent has this password").expect("hashing a constant"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hashed_password_verifies_and_a_wrong_one_does_not() {
        let hash = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong horse", &hash));
    }

    #[test]
    fn the_dummy_hash_parses() {
        assert!(PasswordHash::new(&DUMMY_HASH).is_ok());
    }

    /// How a session was opened and how long it lasts, as stored.
    pub async fn stored_session(db: &PgPool, token: &str) -> (String, f64) {
        sqlx::query_as(
            "SELECT opened_with,
                    extract(epoch FROM expires_at - created_at)::float8 / 3600
             FROM agent_sessions WHERE token_hash = $1",
        )
        .bind(token_hash(token))
        .fetch_one(db)
        .await
        .unwrap()
    }

    #[sqlx::test]
    async fn a_local_password_opens_a_session_of_14_days(db: PgPool) {
        crate::agents::add(
            &db,
            &Grant::command_line(),
            "ada@helpdesk.test",
            "Ada",
            None,
            Some("ada's password"),
        )
        .await
        .unwrap();
        let token = sign_in(&db, "ada@helpdesk.test", "ada's password")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored_session(&db, &token).await,
            ("password".to_owned(), 14.0 * 24.0)
        );
    }
}
