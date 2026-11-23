//! Identities of an identity provider linked to agents (docs/domain-rules.md, "Signing in";
//! ADR 0008).

use sqlx::{PgPool, Postgres, Transaction};
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
    /// What the provider says of the installation's authorized group.
    pub authorized_group: AuthorizedGroup,
}

/// Whether the person is in the installation's authorized group, checked at every sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizedGroup {
    /// The installation names no authorized group: only existing agents sign in.
    NotConfigured,
    /// In the group: someone who is not an agent yet becomes one.
    Member,
    /// Not in the group: refused, agent or not.
    NotMember,
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
    /// The installation names an authorized group, and the person is not in it.
    NotInAuthorizedGroup,
    /// The provider sent no usable e-mail address, and the identity is not linked yet: there is
    /// nobody to link and nothing to make an agent with.
    NoEmail,
    /// The provider did not say the e-mail address is verified, and the identity is not linked yet.
    EmailNotVerified,
    /// No agent has the e-mail address, and the identity is not linked to one.
    NotAnAgent,
    /// The agent was deactivated: signing in never reactivates them.
    Deactivated,
    /// The e-mail address, verified or not, belongs to another agent than the one the identity is
    /// linked to, or to an agent linked to another identity of the provider.
    EmailOfAnotherAgent,
}

/// The advisory lock class of first sign-ins through an identity provider (sign_in_limits uses 1
/// and 2, oidc 3).
const FIRST_SIGN_IN_LOCK_CLASS: i32 = 4;

/// The constraint that keeps agents' addresses apart.
const AGENTS_EMAIL_CONSTRAINT: &str = "agents_email_key";

/// Signs in the agent the identity is linked to, linking it first by verified e-mail, or making
/// the agent when the person is in the authorized group and not an agent yet; opens a session
/// lasting `lifetime`.
pub async fn sign_in(
    db: &PgPool,
    identity: &Identity,
    lifetime: time::Duration,
) -> anyhow::Result<Result<SignedIn, Refusal>> {
    // Checked at every sign-in, before anything else: someone outside the group is neither
    // linked, made an agent, nor updated, agent or not.
    if identity.authorized_group == AuthorizedGroup::NotMember {
        return Ok(Err(Refusal::NotInAuthorizedGroup));
    }
    match sign_in_transaction(db, identity, lifetime).await {
        // An address another sign-in or change took at the same moment.
        Err(error) if violates(&error, AGENTS_EMAIL_CONSTRAINT) => {
            Ok(Err(Refusal::EmailOfAnotherAgent))
        }
        answer => answer,
    }
}

/// The sign-in's transaction: finds, links or makes the agent, takes over the provider's name and
/// e-mail, and opens the session, all or nothing.
async fn sign_in_transaction(
    db: &PgPool,
    identity: &Identity,
    lifetime: time::Duration,
) -> anyhow::Result<Result<SignedIn, Refusal>> {
    let mut tx = db.begin().await?;
    let email = identity
        .email
        .as_deref()
        .map(crate::agents::normalized_email)
        .filter(|email| crate::agents::is_address(email));
    let mut linked = linked_agent(&mut tx, identity).await?;
    if linked.is_none() {
        // First sign-ins one at a time, so that two at once neither link nor make two agents:
        // the later one finds what the earlier one did.
        sqlx::query("SELECT pg_advisory_xact_lock($1, 0)")
            .bind(FIRST_SIGN_IN_LOCK_CLASS)
            .execute(&mut *tx)
            .await?;
        linked = linked_agent(&mut tx, identity).await?;
    }
    let agent = match linked {
        Some((_, false)) => return Ok(Err(Refusal::Deactivated)),
        Some((agent, true)) => agent,
        None => match first_sign_in(&mut tx, identity, email.as_deref()).await? {
            Ok(agent) => agent,
            Err(refusal) => return Ok(Err(refusal)),
        },
    };
    if let Some(email) = &email
        && crate::agents::email_taken(&mut *tx, email, Some(agent)).await?
    {
        return Ok(Err(Refusal::EmailOfAnotherAgent));
    }
    // The name and the e-mail address follow the provider, the address verified or not; a
    // missing or blank one keeps what the agent has.
    sqlx::query(
        "UPDATE agents SET name = coalesce($2, name), email = coalesce($3, email) WHERE id = $1",
    )
    .bind(agent)
    .bind(provider_name(identity))
    .bind(&email)
    .execute(&mut *tx)
    .await?;
    let token = auth::open_session(&mut *tx, agent, OpenedWith::OpenIdConnect, lifetime).await?;
    tx.commit().await?;
    Ok(Ok(SignedIn { agent, token }))
}

/// The agent the identity is linked to, and whether they are active; the agent is locked.
async fn linked_agent(
    tx: &mut Transaction<'_, Postgres>,
    identity: &Identity,
) -> sqlx::Result<Option<(Uuid, bool)>> {
    sqlx::query_as(
        "SELECT a.id, a.deactivated_at IS NULL
         FROM agent_identities i JOIN agents a ON a.id = i.agent_id
         WHERE i.issuer = $1 AND i.subject = $2
         FOR UPDATE OF a",
    )
    .bind(&identity.issuer)
    .bind(&identity.subject)
    .fetch_optional(&mut **tx)
    .await
}

/// The agent an identity that is not linked yet signs in as: the agent with its address, linked
/// when the provider verified it, or a new agent when the person is in the authorized group and
/// nobody has the address.
async fn first_sign_in(
    tx: &mut Transaction<'_, Postgres>,
    identity: &Identity,
    email: Option<&str>,
) -> anyhow::Result<Result<Uuid, Refusal>> {
    let Some(email) = email else {
        return Ok(Err(Refusal::NoEmail));
    };
    let agent: Option<(Uuid, bool)> =
        sqlx::query_as("SELECT id, deactivated_at IS NULL FROM agents WHERE email = $1 FOR UPDATE")
            .bind(email)
            .fetch_optional(&mut **tx)
            .await?;
    let agent = match agent {
        // Only an address the provider verified picks an existing agent.
        Some(_) if !identity.email_verified => return Ok(Err(Refusal::EmailNotVerified)),
        Some((_, false)) => return Ok(Err(Refusal::Deactivated)),
        Some((agent, true)) => {
            // Linked by e-mail once only: an agent linked to another identity of the provider
            // is not linked again.
            let linked_to: Option<String> = sqlx::query_scalar(
                "SELECT subject FROM agent_identities WHERE agent_id = $1 AND issuer = $2",
            )
            .bind(agent)
            .bind(&identity.issuer)
            .fetch_optional(&mut **tx)
            .await?;
            if linked_to.is_some() {
                return Ok(Err(Refusal::EmailOfAnotherAgent));
            }
            agent
        }
        // Nobody has the address: only the authorized group makes agents.
        None if identity.authorized_group == AuthorizedGroup::Member => {
            new_agent(tx, identity, email).await?
        }
        None if identity.email_verified => return Ok(Err(Refusal::NotAnAgent)),
        None => return Ok(Err(Refusal::EmailNotVerified)),
    };
    sqlx::query("INSERT INTO agent_identities (issuer, subject, agent_id) VALUES ($1, $2, $3)")
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .bind(agent)
        .execute(&mut **tx)
        .await?;
    Ok(Ok(agent))
}

/// Makes the person an agent with the default role and no local password, named as the provider
/// names them, or by their address without a name. Being in the authorized group says who may
/// enter, not what they may do: even the very first agent gets the default role.
async fn new_agent(
    tx: &mut Transaction<'_, Postgres>,
    identity: &Identity,
    email: &str,
) -> anyhow::Result<Uuid> {
    let role = crate::agents::default_role(tx)
        .await
        .map_err(crate::error::Error::into_anyhow)?;
    let name = provider_name(identity).unwrap_or(email);
    crate::agents::insert(tx, email, name, role, None)
        .await
        .map_err(crate::error::Error::into_anyhow)
}

/// The name the provider gives, trimmed; none when missing or blank.
fn provider_name(identity: &Identity) -> Option<&str> {
    identity
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// Whether the error is the database refusing a row for the constraint `constraint`.
fn violates(error: &anyhow::Error, constraint: &str) -> bool {
    error
        .downcast_ref::<sqlx::Error>()
        .and_then(sqlx::Error::as_database_error)
        .is_some_and(|error| error.constraint() == Some(constraint))
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
            authorized_group: AuthorizedGroup::NotConfigured,
        }
    }

    /// The identity, in the installation's authorized group.
    fn member(subject: &str, email: &str, verified: bool, name: &str) -> Identity {
        Identity {
            authorized_group: AuthorizedGroup::Member,
            ..identity(subject, email, verified, name)
        }
    }

    /// The identity, outside the installation's authorized group.
    fn outsider(subject: &str, email: &str, verified: bool, name: &str) -> Identity {
        Identity {
            authorized_group: AuthorizedGroup::NotMember,
            ..identity(subject, email, verified, name)
        }
    }

    async fn agent_count(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM agents")
            .fetch_one(db)
            .await
            .unwrap()
    }

    /// An agent as stored: name, e-mail, role, whether they have a local password.
    async fn stored_agent(db: &PgPool, agent: Uuid) -> (String, String, String, bool) {
        sqlx::query_as(
            "SELECT a.name, a.email, r.name, a.password_hash IS NOT NULL
             FROM agents a JOIN roles r ON r.id = a.role_id WHERE a.id = $1",
        )
        .bind(agent)
        .fetch_one(db)
        .await
        .unwrap()
    }

    /// Even on an installation without agents, the first sign-in gives the default role, not the
    /// Administrator role: being in the group says who may enter, not what they may do.
    #[sqlx::test]
    async fn a_first_sign_in_in_the_authorized_group_creates_an_agent(db: PgPool) {
        let vera = member("sub-vera", " Vera.Lind@example.com", true, " Vera Lind ");
        let signed_in = sign_in(&db, &vera, HOURS_12).await.unwrap().unwrap();
        assert_eq!(
            stored_agent(&db, signed_in.agent).await,
            (
                "Vera Lind".to_owned(),
                "vera.lind@example.com".to_owned(),
                "Agent".to_owned(),
                false
            )
        );
        assert_eq!(linked(&db, "sub-vera").await, Some(signed_in.agent));
        assert_eq!(
            stored_session(&db, &signed_in.token).await,
            ("openid_connect".to_owned(), 12.0)
        );

        // Later sign-ins find her agent: nobody is made twice.
        let again = sign_in(&db, &vera, HOURS_12).await.unwrap().unwrap();
        assert_eq!(again.agent, signed_in.agent);
        assert_eq!(agent_count(&db).await, 1);
    }

    #[sqlx::test]
    async fn someone_not_in_the_authorized_group_is_refused_even_when_an_agent(db: PgPool) {
        let lena = agent(&db, "lena.marsh@example.com", "Lena").await;
        let rhea = agent(&db, "rhea.dunn@example.com", "Rhea").await;
        sign_in(
            &db,
            &member("sub-rhea", "rhea.dunn@example.com", true, "Rhea Dunn"),
            HOURS_12,
        )
        .await
        .unwrap()
        .unwrap();
        let before = sessions(&db).await;

        for refused in [
            // Linked: neither renamed nor signed in.
            outsider("sub-rhea", "rhea.new@example.com", true, "Rhea New"),
            // An agent's verified address: not linked.
            outsider("sub-lena", "lena.marsh@example.com", true, "Lena Marsh"),
            // Not an agent: not made one.
            outsider("sub-nadia", "nadia.quist@example.com", true, "Nadia Quist"),
        ] {
            assert_eq!(
                sign_in(&db, &refused, HOURS_12).await.unwrap().unwrap_err(),
                Refusal::NotInAuthorizedGroup,
                "{}",
                refused.subject
            );
        }
        assert_eq!(
            name_and_email(&db, rhea).await,
            ("Rhea Dunn".to_owned(), "rhea.dunn@example.com".to_owned())
        );
        assert_eq!(linked(&db, "sub-lena").await, None);
        assert_eq!(name_and_email(&db, lena).await.0, "Lena");
        assert_eq!(agent_count(&db).await, 2);
        assert_eq!(sessions(&db).await, before);
    }

    #[sqlx::test]
    async fn a_deactivated_agent_in_the_group_is_refused_and_not_recreated(db: PgPool) {
        let cli = Grant::command_line();
        agent(&db, "lena.marsh@example.com", "Lena").await;
        let piet = agent(&db, "piet.oak@example.com", "Piet").await;
        let rhea = agent(&db, "rhea.dunn@example.com", "Rhea").await;
        let rhea_at_provider = member("sub-rhea", "rhea.dunn@example.com", true, "Rhea Dunn");
        sign_in(&db, &rhea_at_provider, HOURS_12)
            .await
            .unwrap()
            .unwrap();
        crate::agents::deactivate(&db, &cli, piet).await.unwrap();
        crate::agents::deactivate(&db, &cli, rhea).await.unwrap();

        for refused in [
            member("sub-piet", "piet.oak@example.com", true, "Piet Oak"),
            rhea_at_provider,
            // Linked, with an address nobody has: still Rhea's deactivated agent.
            member("sub-rhea", "rhea.new@example.com", true, "Rhea New"),
        ] {
            assert_eq!(
                sign_in(&db, &refused, HOURS_12).await.unwrap().unwrap_err(),
                Refusal::Deactivated,
                "{}",
                refused.subject
            );
        }
        assert_eq!(agent_count(&db).await, 3);
        assert_eq!(linked(&db, "sub-piet").await, None);
        assert!(deactivated(&db, piet).await);
        assert!(deactivated(&db, rhea).await);
        assert_eq!(sessions(&db).await, 0);
    }

    /// An address the provider has not verified never picks an existing agent, but it is the new
    /// agent's address when nobody has it: a linked agent's address follows the provider anyway.
    #[sqlx::test]
    async fn an_unverified_email_makes_an_agent_but_never_picks_one(db: PgPool) {
        let otto = member("sub-otto", "otto.vale@example.com", false, "Otto Vale");
        let made = sign_in(&db, &otto, HOURS_12).await.unwrap().unwrap();
        assert_eq!(
            name_and_email(&db, made.agent).await,
            ("Otto Vale".to_owned(), "otto.vale@example.com".to_owned())
        );

        let sam = agent(&db, "sam.reed@example.com", "Sam").await;
        let impostor = member("sub-impostor", "sam.reed@example.com", false, "Not Sam");
        assert_eq!(
            sign_in(&db, &impostor, HOURS_12)
                .await
                .unwrap()
                .unwrap_err(),
            Refusal::EmailNotVerified
        );
        assert_eq!(linked(&db, "sub-impostor").await, None);
        assert_eq!(name_and_email(&db, sam).await.0, "Sam");
        assert_eq!(agent_count(&db).await, 2);
    }

    /// Without an address there is nobody to link and nothing to make an agent with; without a
    /// name, the new agent is named by their address.
    #[sqlx::test]
    async fn a_new_agent_needs_an_email_and_is_named_by_it_without_a_name(db: PgPool) {
        for email in [
            None,
            Some(" ".to_owned()),
            Some("not an address".to_owned()),
        ] {
            let without = Identity {
                email,
                ..member("sub-vera", "", true, "Vera Lind")
            };
            assert_eq!(
                sign_in(&db, &without, HOURS_12).await.unwrap().unwrap_err(),
                Refusal::NoEmail
            );
        }
        assert_eq!(agent_count(&db).await, 0);

        let nameless = Identity {
            name: None,
            ..member("sub-vera", "vera.lind@example.com", true, "")
        };
        let made = sign_in(&db, &nameless, HOURS_12).await.unwrap().unwrap();
        assert_eq!(
            name_and_email(&db, made.agent).await,
            (
                "vera.lind@example.com".to_owned(),
                "vera.lind@example.com".to_owned()
            )
        );
    }

    /// First sign-ins at the same moment make one agent: the same identity signs in as it, and
    /// another identity with the same address is refused.
    #[sqlx::test]
    async fn first_sign_ins_at_once_make_one_agent(db: PgPool) {
        let vera = member("sub-vera", "vera.lind@example.com", true, "Vera Lind");
        let other = member("sub-other", "vera.lind@example.com", true, "Vera L.");
        // Connections open beforehand, so that the sign-ins run side by side.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect_with((*db.connect_options()).clone())
            .await
            .unwrap();
        let open = futures_util::future::try_join_all((0..8).map(|_| pool.acquire()))
            .await
            .unwrap();
        drop(open);
        let attempts = (0..8).map(|n| {
            let db = pool.clone();
            let who = if n % 2 == 0 {
                vera.clone()
            } else {
                other.clone()
            };
            async move { sign_in(&db, &who, HOURS_12).await.unwrap() }
        });
        let answers = futures_util::future::join_all(attempts).await;
        pool.close().await;
        assert_eq!(agent_count(&db).await, 1);
        let agent: Uuid = sqlx::query_scalar("SELECT id FROM agents")
            .fetch_one(&db)
            .await
            .unwrap();
        let winner = sqlx::query_scalar::<_, String>(
            "SELECT subject FROM agent_identities WHERE agent_id = $1",
        )
        .bind(agent)
        .fetch_one(&db)
        .await
        .unwrap();
        for (n, answer) in answers.into_iter().enumerate() {
            let subject = if n % 2 == 0 { "sub-vera" } else { "sub-other" };
            if subject == winner {
                assert_eq!(answer.unwrap().agent, agent);
            } else {
                assert_eq!(answer.unwrap_err(), Refusal::EmailOfAnotherAgent);
            }
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
            Refusal::NoEmail
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

        // A linked identity no longer needs a verified address: an unverified one is taken over
        // all the same, while a missing name is not.
        let unverified = Identity {
            name: None,
            ..identity("sub-lena", "Lena@Elsewhere.test", false, "")
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
            ("Lena Brook".to_owned(), "lena@elsewhere.test".to_owned())
        );

        // Without an address from the provider, or with a blank one, she keeps hers.
        for email in [None, Some(" ".to_owned())] {
            let without = Identity {
                email,
                ..identity("sub-lena", "", true, "Lena Brook")
            };
            assert_eq!(
                sign_in(&db, &without, HOURS_12)
                    .await
                    .unwrap()
                    .unwrap()
                    .agent,
                lena
            );
            assert_eq!(name_and_email(&db, lena).await.1, "lena@elsewhere.test");
        }
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

        // Lena's verified address at the provider became Sam's.
        let taken = identity("sub-lena", "Sam.Reed@example.com", true, "Lena Reed");
        assert_eq!(
            sign_in(&db, &taken, HOURS_12).await.unwrap().unwrap_err(),
            Refusal::EmailOfAnotherAgent
        );
        assert_eq!(
            name_and_email(&db, lena).await,
            ("Lena Marsh".to_owned(), "lena.marsh@example.com".to_owned())
        );
        let before = sessions(&db).await;

        // An unverified address is taken over all the same, so it is refused too.
        let unverified = identity("sub-lena", "Sam.Reed@example.com", false, "Lena Reed");
        assert_eq!(
            sign_in(&db, &unverified, HOURS_12)
                .await
                .unwrap()
                .unwrap_err(),
            Refusal::EmailOfAnotherAgent
        );
        assert_eq!(
            name_and_email(&db, lena).await,
            ("Lena Marsh".to_owned(), "lena.marsh@example.com".to_owned())
        );
        assert_eq!(sessions(&db).await, before);

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
