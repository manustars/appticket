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

/// Name of the cookie that carries an agent's session token.
pub const SESSION_COOKIE: &str = "hd_session";
/// How long a session lasts.
pub const SESSION_DAYS: i32 = 14;

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

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Checks the credentials and opens a session. Returns the session token.
pub async fn sign_in(db: &PgPool, email: &str, password: &str) -> anyhow::Result<Option<String>> {
    let agent: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, password_hash FROM agents WHERE email = $1")
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
    // Two v4 UUIDs: 244 random bits from the operating system's generator.
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO agent_sessions (token_hash, agent_id, expires_at)
         VALUES ($1, $2, now() + make_interval(days => $3))",
    )
    .bind(token_hash(&token))
    .bind(agent_id)
    .bind(SESSION_DAYS)
    .execute(db)
    .await?;
    Ok(Some(token))
}

pub async fn sign_out(db: &PgPool, token: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM agent_sessions WHERE token_hash = $1")
        .bind(token_hash(token))
        .execute(db)
        .await?;
    Ok(())
}

/// The agent a session token belongs to, if the session is still valid.
pub async fn agent_for(db: &PgPool, token: &str) -> anyhow::Result<Option<SignedIn>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.email, a.name FROM agent_sessions s JOIN agents a ON a.id = s.agent_id
         WHERE s.token_hash = $1 AND s.expires_at > now()",
    )
    .bind(token_hash(token))
    .fetch_optional(db)
    .await?)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SignedIn {
    pub id: Uuid,
    pub email: String,
    pub name: String,
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
}
