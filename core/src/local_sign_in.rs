//! Whether agents can sign in with a local password: an installation setting kept by the
//! product and changed with the command line (docs/domain-rules.md, "Signing in"; ADR 0008).
//! The database keeps the rule too: switched off, it ends the sessions opened with a password and
//! refuses new ones (migration 0028).

use sqlx::{PgExecutor, PgPool};

use crate::error::{Result, rule};

/// A session opened with a local password, refused because local sign-in is off.
#[derive(Debug)]
pub struct Off;

impl std::fmt::Display for Off {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("local sign-in is off")
    }
}

impl std::error::Error for Off {}

/// `Off` for the database's refusal of a session opened with a password while local sign-in is
/// off (the switch came while the password was being checked); any other error as it is.
pub fn refused_when_off(error: sqlx::Error) -> anyhow::Error {
    match &error {
        sqlx::Error::Database(e) if e.constraint() == Some("local_sign_in_on") => Off.into(),
        _ => error.into(),
    }
}

/// Whether agents can sign in with a local password now. Read at every use: the command line
/// changes it while the core runs.
pub async fn is_on(db: impl PgExecutor<'_>) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT local_sign_in FROM installation_settings")
            .fetch_one(db)
            .await?,
    )
}

/// Switches local sign-in off, which ends every session opened with a local password and keeps
/// those opened through the identity provider; refused while no identity provider is configured.
/// Returns how many sessions ended.
pub async fn switch_off(
    db: &PgPool,
    identity_provider: Option<&crate::oidc::Config>,
) -> Result<u64> {
    if identity_provider.is_none() {
        return rule(
            "local sign-in cannot be switched off while no identity provider is configured: \
             agents could not sign in at all",
        );
    }
    let mut tx = db.begin().await?;
    // Sign-ins under way finish first, and none starts until the switch is done, so that every
    // session counted here is one the switch ends.
    sqlx::query("SELECT 1 FROM installation_settings FOR UPDATE")
        .execute(&mut *tx)
        .await?;
    let ended = sqlx::query("DELETE FROM agent_sessions WHERE opened_with = 'password'")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query("UPDATE installation_settings SET local_sign_in = false")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ended)
}

/// Switches local sign-in back on. Always possible, so that an unreachable identity provider
/// never locks the help desk out.
pub async fn switch_on(db: &PgPool) -> Result<()> {
    sqlx::query("UPDATE installation_settings SET local_sign_in = true")
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    /// An agent with a session opened with a local password and one opened through the identity
    /// provider, stored as the core stores them.
    async fn agent_with_sessions(db: &PgPool) -> Uuid {
        let agent = crate::testing::agent(db, "ada@northgate.test").await;
        for (hash, opened_with) in [
            (b"local".as_slice(), "password"),
            (b"idp", "openid_connect"),
        ] {
            sqlx::query(
                "INSERT INTO agent_sessions (token_hash, agent_id, opened_with, expires_at)
                 VALUES ($1, $2, $3, now() + interval '1 hour')",
            )
            .bind(hash)
            .bind(agent)
            .bind(opened_with)
            .execute(db)
            .await
            .unwrap();
        }
        agent
    }

    async fn sessions(db: &PgPool) -> Vec<String> {
        sqlx::query_scalar("SELECT opened_with FROM agent_sessions ORDER BY opened_with")
            .fetch_all(db)
            .await
            .unwrap()
    }

    /// The database keeps the rule against SQL that bypasses the core: switching local sign-in
    /// off ends the sessions opened with a password, and none is opened while it is off.
    #[sqlx::test]
    async fn the_database_ends_and_refuses_password_sessions_while_local_sign_in_is_off(
        db: PgPool,
    ) {
        let agent = agent_with_sessions(&db).await;
        sqlx::query("UPDATE installation_settings SET local_sign_in = false")
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(sessions(&db).await, ["openid_connect"]);

        let opened = sqlx::query(
            "INSERT INTO agent_sessions (token_hash, agent_id, opened_with, expires_at)
             VALUES ('\\x01', $1, 'password', now() + interval '1 hour')",
        )
        .bind(agent)
        .execute(&db)
        .await;
        assert!(
            matches!(&opened, Err(sqlx::Error::Database(e)) if e.constraint() == Some("local_sign_in_on")),
            "{opened:?}"
        );
        let turned = sqlx::query("UPDATE agent_sessions SET opened_with = 'password'")
            .execute(&db)
            .await;
        assert!(turned.is_err(), "{turned:?}");

        sqlx::query("UPDATE installation_settings SET local_sign_in = true")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO agent_sessions (token_hash, agent_id, opened_with, expires_at)
             VALUES ('\\x01', $1, 'password', now() + interval '1 hour')",
        )
        .bind(agent)
        .execute(&db)
        .await
        .unwrap();
    }

    /// A sign-in and the switch at the same moment: whichever comes second waits for the other,
    /// so that the session is either ended by the switch or refused.
    #[sqlx::test]
    async fn a_password_session_opened_as_local_sign_in_goes_off_is_ended_or_refused(db: PgPool) {
        let agent = crate::testing::agent(&db, "ada@northgate.test").await;
        let open = move |hash: &'static [u8]| {
            sqlx::query(
                "INSERT INTO agent_sessions (token_hash, agent_id, opened_with, expires_at)
                 VALUES ($1, $2, 'password', now() + interval '1 hour')",
            )
            .bind(hash)
            .bind(agent)
        };

        // The sign-in first: the switch waits for it, then ends its session.
        let mut signing_in = db.begin().await.unwrap();
        open(b"first").execute(&mut *signing_in).await.unwrap();
        let switch = tokio::spawn({
            let db = db.clone();
            async move {
                sqlx::query("UPDATE installation_settings SET local_sign_in = false")
                    .execute(&db)
                    .await
            }
        });
        assert!(crate::testing::some_statement_waits_for_a_lock(&db).await);
        signing_in.commit().await.unwrap();
        switch.await.unwrap().unwrap();
        assert!(sessions(&db).await.is_empty());

        // The switch first: the sign-in waits for it, then is refused.
        sqlx::query("UPDATE installation_settings SET local_sign_in = true")
            .execute(&db)
            .await
            .unwrap();
        let mut switching = db.begin().await.unwrap();
        sqlx::query("UPDATE installation_settings SET local_sign_in = false")
            .execute(&mut *switching)
            .await
            .unwrap();
        let sign_in = tokio::spawn({
            let db = db.clone();
            async move { open(b"second").execute(&db).await.map(|_| ()) }
        });
        assert!(crate::testing::some_statement_waits_for_a_lock(&db).await);
        switching.commit().await.unwrap();
        assert!(sign_in.await.unwrap().is_err());
        assert!(sessions(&db).await.is_empty());
    }

    /// An identity provider as the deployment configures it.
    pub(crate) fn identity_provider() -> crate::oidc::Config {
        let settings = crate::oidc::Settings {
            issuer: Some("https://login.example.com/realms/staff".to_owned()),
            client_id: Some("helpdesk".to_owned()),
            client_secret: Some("invented-secret".to_owned()),
            client_secret_file: None,
            label: Some("Northgate".to_owned()),
            session_hours: 12,
        };
        crate::oidc::Config::from_settings(settings, Some("https://helpdesk.example.com"))
            .unwrap()
            .unwrap()
    }

    #[sqlx::test]
    async fn switching_local_sign_in_off_ends_password_sessions_and_keeps_the_others(db: PgPool) {
        agent_with_sessions(&db).await;
        assert!(super::is_on(&db).await.unwrap());
        let ended = super::switch_off(&db, Some(&identity_provider()))
            .await
            .unwrap();
        assert_eq!(ended, 1);
        assert!(!super::is_on(&db).await.unwrap());
        assert_eq!(sessions(&db).await, ["openid_connect"]);

        // Switching it off again changes nothing; switching it on lets passwords in again.
        assert_eq!(
            super::switch_off(&db, Some(&identity_provider()))
                .await
                .unwrap(),
            0
        );
        super::switch_on(&db).await.unwrap();
        assert!(super::is_on(&db).await.unwrap());
    }

    #[sqlx::test]
    async fn local_sign_in_stays_on_while_no_identity_provider_is_configured(db: PgPool) {
        agent_with_sessions(&db).await;
        let refused = super::switch_off(&db, None).await;
        assert!(
            matches!(&refused, Err(crate::error::Error::Rule(why)) if why.contains("identity provider")),
            "{refused:?}"
        );
        assert!(super::is_on(&db).await.unwrap());
        assert_eq!(sessions(&db).await, ["openid_connect", "password"]);
    }

    /// The setting is one row, always there: local sign-in is on until switched off.
    #[sqlx::test]
    async fn the_installation_has_exactly_one_setting_on_by_default(db: PgPool) {
        let on: Vec<bool> = sqlx::query_scalar("SELECT local_sign_in FROM installation_settings")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(on, [true]);
        assert!(
            sqlx::query("INSERT INTO installation_settings DEFAULT VALUES")
                .execute(&db)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM installation_settings")
                .execute(&db)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("TRUNCATE installation_settings")
                .execute(&db)
                .await
                .is_err()
        );
    }
}
