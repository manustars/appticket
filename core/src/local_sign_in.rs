//! Whether agents can sign in with a local password: an installation setting kept by the
//! product and changed with the command line (docs/domain-rules.md, "Signing in"; ADR 0008).

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    /// An agent with a session opened with a local password and one opened through the identity
    /// provider, stored as the core stores them.
    async fn agent_with_sessions(db: &PgPool) -> Uuid {
        let agent = crate::testing::agent(db, "ada@northgate.test").await;
        for (hash, opened_with) in [(b"local".as_slice(), "password"), (b"idp", "openid_connect")] {
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
