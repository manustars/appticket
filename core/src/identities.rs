//! Identities of an identity provider linked to agents (docs/domain-rules.md, "Signing in";
//! ADR 0008).

use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::{self, OpenedWith};

/// Who the identity provider says signed in.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The identity provider, such as an OpenID Connect issuer URL.
    pub issuer: String,
    /// The provider's stable identifier of the person.
    pub subject: String,
    pub email: Option<String>,
    /// Whether the provider says the e-mail address is the person's.
    pub email_verified: bool,
    pub name: Option<String>,
}

/// An agent signed in through an identity provider.
#[derive(Debug)]
pub struct SignedIn {
    pub agent: Uuid,
    /// The new session's token.
    pub token: String,
}

/// Why an identity provider's sign-in was refused, shown on the sign-in page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The provider did not say the e-mail address is verified, and the identity is not linked yet.
    EmailNotVerified,
    /// No agent has the e-mail address, and the identity is not linked to one.
    NotAnAgent,
    /// The agent was deactivated: signing in never reactivates them.
    Deactivated,
    /// The e-mail address belongs to another agent than the one the identity is linked to, or
    /// to an agent linked to another identity of the provider.
    EmailOfAnotherAgent,
}

/// Signs in the agent the identity is linked to, linking it first by verified e-mail, and opens a
/// session lasting `lifetime`.
pub async fn sign_in(
    db: &PgPool,
    identity: &Identity,
    lifetime: time::Duration,
) -> anyhow::Result<Result<SignedIn, Refusal>> {
    let mut tx = db.begin().await?;
    let email = identity
        .email
        .as_deref()
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty());
    let verified_email = email.clone().filter(|_| identity.email_verified);
    // The agent, and whether they are active.
    let linked: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT a.id, a.deactivated_at IS NULL
         FROM agent_identities i JOIN agents a ON a.id = i.agent_id
         WHERE i.issuer = $1 AND i.subject = $2
         FOR UPDATE OF a",
    )
    .bind(&identity.issuer)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await?;
    let agent = match linked {
        Some((_, false)) => return Ok(Err(Refusal::Deactivated)),
        Some((agent, true)) => agent,
        None => {
            let Some(email) = &verified_email else {
                return Ok(Err(Refusal::EmailNotVerified));
            };
            let agent: Option<(Uuid, bool)> = sqlx::query_as(
                "SELECT id, deactivated_at IS NULL FROM agents WHERE email = $1 FOR UPDATE",
            )
            .bind(email)
            .fetch_optional(&mut *tx)
            .await?;
            let agent = match agent {
                None => return Ok(Err(Refusal::NotAnAgent)),
                Some((_, false)) => return Ok(Err(Refusal::Deactivated)),
                Some((agent, true)) => agent,
            };
            // Linked by e-mail once only: an agent linked to another identity of the provider
            // is not linked again. The same identity may have been linked meanwhile.
            let linked_to: Option<String> = sqlx::query_scalar(
                "SELECT subject FROM agent_identities WHERE agent_id = $1 AND issuer = $2",
            )
            .bind(agent)
            .bind(&identity.issuer)
            .fetch_optional(&mut *tx)
            .await?;
            match linked_to {
                Some(subject) if subject != identity.subject => {
                    return Ok(Err(Refusal::EmailOfAnotherAgent));
                }
                Some(_) => {}
                None => {
                    sqlx::query(
                        "INSERT INTO agent_identities (issuer, subject, agent_id)
                         VALUES ($1, $2, $3)",
                    )
                    .bind(&identity.issuer)
                    .bind(&identity.subject)
                    .bind(agent)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            agent
        }
    };
    if let Some(email) = &email {
        let other: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM agents WHERE email = $1 AND id <> $2)",
        )
        .bind(email)
        .bind(agent)
        .fetch_one(&mut *tx)
        .await?;
        if other {
            return Ok(Err(Refusal::EmailOfAnotherAgent));
        }
    }
    // The name and a verified e-mail address follow the provider.
    let name = identity
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    sqlx::query(
        "UPDATE agents SET name = coalesce($2, name), email = coalesce($3, email) WHERE id = $1",
    )
    .bind(agent)
    .bind(name)
    .bind(&verified_email)
    .execute(&mut *tx)
    .await?;
    let token = auth::open_session(&mut *tx, agent, OpenedWith::OpenIdConnect, lifetime).await?;
    tx.commit().await?;
    Ok(Ok(SignedIn { agent, token }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::tests::stored_session;
    use crate::roles::Grant;

    const ISSUER: &str = "https://login.example.com/realms/staff";
    const HOURS_12: time::Duration = time::Duration::hours(12);

    async fn agent(db: &PgPool, email: &str, name: &str) -> Uuid {
        crate::agents::add(db, &Grant::command_line(), email, name, None, None)
            .await
            .unwrap()
            .id
    }

    fn identity(subject: &str, email: &str, verified: bool, name: &str) -> Identity {
        Identity {
            issuer: ISSUER.to_owned(),
            subject: subject.to_owned(),
            email: Some(email.to_owned()),
            email_verified: verified,
            name: Some(name.to_owned()),
        }
    }

    #[sqlx::test]
    async fn a_verified_email_links_the_agent_and_opens_a_session_of_12_hours(db: PgPool) {
        let lena = agent(&db, "lena.marsh@example.com", "Lena").await;
        let signed_in = sign_in(
            &db,
            &identity("sub-lena", "Lena.Marsh@example.com", true, "Lena Marsh"),
            time::Duration::hours(12),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(signed_in.agent, lena);
        assert_eq!(
            stored_session(&db, &signed_in.token).await,
            ("openid_connect".to_owned(), 12.0)
        );
        assert_eq!(linked(&db, "sub-lena").await, Some(lena));
    }

    async fn sessions(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM agent_sessions")
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[sqlx::test]
    async fn an_unverified_email_does_not_link(db: PgPool) {
        agent(&db, "otto.vale@example.com", "Otto").await;
        let unverified = identity("sub-otto", "otto.vale@example.com", false, "Otto Vale");
        assert_eq!(
            sign_in(&db, &unverified, HOURS_12)
                .await
                .unwrap()
                .unwrap_err(),
            Refusal::EmailNotVerified
        );
        let no_email = Identity {
            email: None,
            ..unverified
        };
        assert_eq!(
            sign_in(&db, &no_email, HOURS_12)
                .await
                .unwrap()
                .unwrap_err(),
            Refusal::EmailNotVerified
        );
        assert_eq!(linked(&db, "sub-otto").await, None);
        assert_eq!(sessions(&db).await, 0);
    }

    #[sqlx::test]
    async fn someone_who_is_not_an_agent_does_not_become_one(db: PgPool) {
        agent(&db, "lena.marsh@example.com", "Lena").await;
        let nadia = identity("sub-nadia", "nadia.quist@example.com", true, "Nadia Quist");
        assert_eq!(
            sign_in(&db, &nadia, HOURS_12).await.unwrap().unwrap_err(),
            Refusal::NotAnAgent
        );
        let agents: i64 = sqlx::query_scalar("SELECT count(*) FROM agents")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(agents, 1);
        assert_eq!(sessions(&db).await, 0);
    }

    async fn name_and_email(db: &PgPool, agent: Uuid) -> (String, String) {
        sqlx::query_as("SELECT name, email FROM agents WHERE id = $1")
            .bind(agent)
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[sqlx::test]
    async fn later_sign_ins_find_the_agent_by_the_stable_identifier(db: PgPool) {
        let lena = agent(&db, "lena.marsh@example.com", "Lena").await;
        let first = identity("sub-lena", "lena.marsh@example.com", true, "Lena Marsh");
        sign_in(&db, &first, HOURS_12).await.unwrap().unwrap();
        assert_eq!(
            name_and_email(&db, lena).await,
            ("Lena Marsh".to_owned(), "lena.marsh@example.com".to_owned())
        );

        // The provider changed her e-mail address and her name.
        let renamed = identity("sub-lena", "Lena.Brook@example.com", true, " Lena Brook ");
        let again = sign_in(&db, &renamed, HOURS_12).await.unwrap().unwrap();
        assert_eq!(again.agent, lena);
        assert_eq!(
            name_and_email(&db, lena).await,
            ("Lena Brook".to_owned(), "lena.brook@example.com".to_owned())
        );

        // A linked identity no longer needs a verified address to be found; an unverified one is
        // not taken over, nor is a missing name.
        let unverified = Identity {
            name: None,
            ..identity("sub-lena", "lena@elsewhere.test", false, "")
        };
        assert_eq!(
            sign_in(&db, &unverified, HOURS_12)
                .await
                .unwrap()
                .unwrap()
                .agent,
            lena
        );
        assert_eq!(
            name_and_email(&db, lena).await,
            ("Lena Brook".to_owned(), "lena.brook@example.com".to_owned())
        );
    }

    async fn deactivated(db: &PgPool, agent: Uuid) -> bool {
        sqlx::query_scalar("SELECT deactivated_at IS NOT NULL FROM agents WHERE id = $1")
            .bind(agent)
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[sqlx::test]
    async fn a_deactivated_agent_is_refused_and_stays_deactivated(db: PgPool) {
        let cli = Grant::command_line();
        agent(&db, "lena.marsh@example.com", "Lena").await;
        let piet = agent(&db, "piet.oak@example.com", "Piet").await;
        let rhea = agent(&db, "rhea.dunn@example.com", "Rhea").await;
        let rhea_at_provider = identity("sub-rhea", "rhea.dunn@example.com", true, "Rhea Dunn");
        sign_in(&db, &rhea_at_provider, HOURS_12)
            .await
            .unwrap()
            .unwrap();
        crate::agents::deactivate(&db, &cli, piet).await.unwrap();
        crate::agents::deactivate(&db, &cli, rhea).await.unwrap();

        // Not linked yet: found by e-mail, and not linked.
        let piet_at_provider = identity("sub-piet", "piet.oak@example.com", true, "Piet Oak");
        assert_eq!(
            sign_in(&db, &piet_at_provider, HOURS_12)
                .await
                .unwrap()
                .unwrap_err(),
            Refusal::Deactivated
        );
        assert_eq!(linked(&db, "sub-piet").await, None);
        // Linked: found by the identifier.
        assert_eq!(
            sign_in(&db, &rhea_at_provider, HOURS_12)
                .await
                .unwrap()
                .unwrap_err(),
            Refusal::Deactivated
        );
        assert!(deactivated(&db, piet).await);
        assert!(deactivated(&db, rhea).await);
        assert_eq!(sessions(&db).await, 0);
    }

    #[sqlx::test]
    async fn an_email_of_another_agent_is_refused(db: PgPool) {
        let lena = agent(&db, "lena.marsh@example.com", "Lena").await;
        let sam = agent(&db, "sam.reed@example.com", "Sam").await;
        let lena_at_provider = identity("sub-lena", "lena.marsh@example.com", true, "Lena Marsh");
        sign_in(&db, &lena_at_provider, HOURS_12)
            .await
            .unwrap()
            .unwrap();
        let before = sessions(&db).await;

        // Lena's address at the provider became Sam's, verified or not.
        for verified in [true, false] {
            let taken = identity("sub-lena", "Sam.Reed@example.com", verified, "Lena Reed");
            assert_eq!(
                sign_in(&db, &taken, HOURS_12).await.unwrap().unwrap_err(),
                Refusal::EmailOfAnotherAgent
            );
        }
        assert_eq!(
            name_and_email(&db, lena).await,
            ("Lena Marsh".to_owned(), "lena.marsh@example.com".to_owned())
        );

        // Another identity of the same provider with Lena's address: she is linked already, to
        // another identity, so it is not linked again.
        let other = identity("sub-impostor", "lena.marsh@example.com", true, "L. Marsh");
        assert_eq!(
            sign_in(&db, &other, HOURS_12).await.unwrap().unwrap_err(),
            Refusal::EmailOfAnotherAgent
        );
        assert_eq!(linked(&db, "sub-impostor").await, None);
        assert_eq!(linked(&db, "sub-lena").await, Some(lena));
        assert_eq!(sessions(&db).await, before);
        assert_eq!(name_and_email(&db, sam).await.1, "sam.reed@example.com");
    }

    /// The database keeps a deactivated agent from being linked or signed in, whatever the code.
    #[sqlx::test]
    async fn the_database_links_and_opens_sessions_for_active_agents_only(db: PgPool) {
        let cli = Grant::command_line();
        agent(&db, "lena.marsh@example.com", "Lena").await;
        let piet = agent(&db, "piet.oak@example.com", "Piet").await;
        crate::agents::deactivate(&db, &cli, piet).await.unwrap();
        let link = sqlx::query(
            "INSERT INTO agent_identities (issuer, subject, agent_id) VALUES ($1, 'sub-piet', $2)",
        )
        .bind(ISSUER)
        .bind(piet)
        .execute(&db)
        .await;
        assert!(link.is_err());
        let session = auth::open_session(
            &db,
            piet,
            OpenedWith::OpenIdConnect,
            time::Duration::hours(1),
        )
        .await;
        assert!(session.is_err());
    }

    /// The agent linked to the identity `subject` of the issuer, as stored.
    async fn linked(db: &PgPool, subject: &str) -> Option<Uuid> {
        sqlx::query_scalar(
            "SELECT agent_id FROM agent_identities WHERE issuer = $1 AND subject = $2",
        )
        .bind(ISSUER)
        .bind(subject)
        .fetch_optional(db)
        .await
        .unwrap()
    }
}
