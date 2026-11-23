//! Signing in through the OpenID Connect identity provider: the browser is sent there, and comes
//! back to the callback, which opens the product's session or says on the sign-in page why not.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::Redirect,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use utoipa_axum::{router::OpenApiRouter, routes};

use super::client_address::ClientAddress;
use super::{ApiError, AppState};
use crate::identities::{self, Refusal};
use crate::mail_status::Tracker;
use crate::oidc::{Begin, BrowserError, CallbackQuery, Config, Provider};

/// The cookie that keeps a sign-in's state in the browser that set out, until it comes back.
const STATE_COOKIE: &str = "hd_sign_in_state";
const BEGIN_PATH: &str = "/api/session/identity-provider";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(begin))
        .routes(routes!(callback))
}

/// The configured identity provider, and the record of what signing in through it last did, for
/// the agents who view its state: the issuer, and errors without the client's secret.
pub fn connect(config: Config) -> anyhow::Result<(Arc<Provider>, Tracker)> {
    let activity = Tracker::new(
        config.issuer.as_str().to_owned(),
        vec![config.client_secret.secret().clone()],
    );
    Ok((Provider::new(config)?, activity))
}

/// Records a failure of signing in through the identity provider: one in reaching it or in what
/// it answered, not in what a browser brought back, which anyone can make up, nor in the help
/// desk's own database.
fn record_failure(state: &AppState, error: &anyhow::Error) {
    if let Some(activity) = &state.identity_provider_activity
        && !error.is::<BrowserError>()
        && !error.is::<sqlx::Error>()
    {
        activity.failed(error);
    }
}

/// The sign-in page after a sign-in that did not open a session, saying why.
fn sign_in_error(reason: &str) -> Redirect {
    Redirect::to(&format!("/?sign_in_error={reason}"))
}

/// Sends the browser to the identity provider to sign in; it comes back to the callback.
#[utoipa::path(get, path = "/api/session/identity-provider", tag = "session",
    responses((status = SEE_OTHER, description = "To the identity provider, or back to the \
        sign-in page with `sign_in_error=failed` when it cannot be reached"),
        (status = NOT_FOUND, body = super::Problem, description = "No identity provider is \
        configured"),
        (status = TOO_MANY_REQUESTS, body = super::Problem, description = "Too many sign-ins \
        under way from the client's address; `Retry-After` says when one ends")))]
async fn begin(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), ApiError> {
    let provider = state
        .identity_provider
        .as_ref()
        .ok_or(ApiError::NotFound("identity provider"))?;
    match provider.begin(&state.db, address).await {
        Ok(Begin::TooMany { retry_after }) => {
            tracing::warn!(%address, "too many sign-ins through the identity provider under way");
            Err(ApiError::TooManySignInsUnderWay { retry_after })
        }
        Ok(Begin::Redirect(redirect)) => {
            let cookie = Cookie::build((STATE_COOKIE, redirect.state))
                .path(BEGIN_PATH)
                .http_only(true)
                .secure(true)
                // Lax: sent along when the provider sends the browser back.
                .same_site(SameSite::Lax)
                .max_age(time::Duration::minutes(10));
            Ok((jar.add(cookie), Redirect::to(redirect.url.as_str())))
        }
        Err(error) => {
            tracing::warn!(
                error = format!("{error:#}"),
                "the identity provider cannot be reached"
            );
            record_failure(&state, &error);
            Ok((jar, sign_in_error("failed")))
        }
    }
}

/// Where the identity provider sends the browser back: opens the product's session and goes to
/// the help desk, or back to the sign-in page with `sign_in_error` saying why not:
/// `not_in_authorized_group`, `no_email`, `email_of_another_agent`, `email_not_verified`,
/// `not_an_agent`, `deactivated` or `failed`. Someone in the authorized group who is not an agent
/// yet becomes one.
#[utoipa::path(get, path = "/api/session/identity-provider/callback", tag = "session",
    params(CallbackQuery),
    responses((status = SEE_OTHER, description = "To the help desk, signed in, or to the \
        sign-in page with the reason"),
        (status = NOT_FOUND, body = super::Problem, description = "No identity provider is \
        configured")))]
async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), ApiError> {
    let provider = state
        .identity_provider
        .as_ref()
        .ok_or(ApiError::NotFound("identity provider"))?;
    let state_cookie = jar.get(STATE_COOKIE).map(|c| c.value().to_owned());
    let jar = jar.remove(Cookie::build(STATE_COOKIE).path(BEGIN_PATH));
    let identity = match provider
        .finish(&state.db, state_cookie.as_deref(), &query)
        .await
    {
        Ok(identity) => {
            // The provider said who signed in: it works, whatever the product makes of that.
            if let Some(activity) = &state.identity_provider_activity {
                activity.succeeded();
            }
            identity
        }
        Err(error) => {
            tracing::warn!(
                error = format!("{error:#}"),
                "signing in through the identity provider failed"
            );
            record_failure(&state, &error);
            return Ok((jar, sign_in_error("failed")));
        }
    };
    match identities::sign_in(&state.db, &identity, provider.session()).await? {
        Ok(signed_in) => {
            tracing::info!(agent = %signed_in.agent, "signed in through the identity provider");
            let cookie = super::session_cookie(signed_in.token, provider.session());
            Ok((jar.add(cookie), Redirect::to("/")))
        }
        Err(refusal) => {
            tracing::info!(subject = %identity.subject, reason = refusal.code(), "sign-in through the identity provider refused");
            Ok((jar, sign_in_error(refusal.code())))
        }
    }
}

impl Refusal {
    /// The reason the sign-in page is sent, which it shows in the agent's language.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::NotInAuthorizedGroup => "not_in_authorized_group",
            Refusal::NoEmail => "no_email",
            Refusal::EmailOfAnotherAgent => "email_of_another_agent",
            Refusal::EmailNotVerified => "email_not_verified",
            Refusal::NotAnAgent => "not_an_agent",
            Refusal::Deactivated => "deactivated",
        }
    }
}

#[cfg(test)]
mod tests {
    //! Mostly against the development Keycloak (`dev/keycloak`, started by compose.yaml and in
    //! CI).

    use std::net::SocketAddr;

    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode, header};
    use sqlx::PgPool;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::http::{AppState, router};
    use crate::roles::Grant;

    use crate::oidc::keycloak::ISSUER;

    /// Lena's stable identifier at Keycloak.
    const LENA: &str = "6f1c2a0e-8d3b-4c55-9a41-0b7e2d6f3a01";
    /// Otto's stable identifier at Keycloak, where his address is not verified.
    const OTTO: &str = "6f1c2a0e-8d3b-4c55-9a41-0b7e2d6f3a02";

    /// Lena, Otto and Piet are in this group at Keycloak; Nadia is not.
    const GROUP: &str = "helpdesk-agents";

    fn state(db: &PgPool) -> AppState {
        state_with_issuer(db, ISSUER)
    }

    fn state_with_issuer(db: &PgPool, issuer: &str) -> AppState {
        state_with(db, issuer, None, "groups")
    }

    /// The core, with `group` as the authorized group, looked for in the claim `claim`.
    fn state_with_group(db: &PgPool, group: &str, claim: &str) -> AppState {
        state_with(db, ISSUER, Some(group), claim)
    }

    fn state_with(
        db: &PgPool,
        issuer: &str,
        authorized_group: Option<&str>,
        group_claim: &str,
    ) -> AppState {
        let settings = crate::oidc::Settings {
            label: Some("Northgate".to_owned()),
            authorized_group: authorized_group.map(str::to_owned),
            group_claim: group_claim.to_owned(),
            ..crate::oidc::keycloak::settings(issuer, "hd-core", crate::oidc::keycloak::SECRET)
        };
        state_from(db, settings)
    }

    fn state_from(db: &PgPool, settings: crate::oidc::Settings) -> AppState {
        let config = crate::oidc::Config::from_settings(settings, Some("http://127.0.0.1:8080"))
            .unwrap()
            .unwrap();
        let (provider, activity) = super::connect(config).unwrap();
        AppState {
            identity_provider: Some(provider),
            identity_provider_activity: Some(activity),
            ..AppState::for_tests(db)
        }
    }

    /// What the core answered: status, Location and the cookies it set, as name=value.
    struct Answer {
        status: StatusCode,
        location: String,
        cookies: Vec<String>,
        max_ages: Vec<String>,
        retry_after: Option<String>,
    }

    impl Answer {
        fn cookie(&self, name: &str) -> Option<String> {
            self.cookies
                .iter()
                .find(|c| c.starts_with(&format!("{name}=")))
                .filter(|c| c.len() > name.len() + 1)
                .cloned()
        }
    }

    async fn get(state: &AppState, uri: &str, cookie: Option<&str>) -> Answer {
        get_from(state, uri, cookie, "192.0.2.1").await
    }

    /// Asks the core for `uri` on a connection from the address `from`.
    async fn get_from(state: &AppState, uri: &str, cookie: Option<&str>, from: &str) -> Answer {
        let connection = ConnectInfo(SocketAddr::new(from.parse().unwrap(), 50000));
        let mut request = Request::builder().uri(uri).extension(connection);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = router(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let set_cookies: Vec<String> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect();
        Answer {
            retry_after: response
                .headers()
                .get(header::RETRY_AFTER)
                .map(|v| v.to_str().unwrap().to_owned()),
            status: response.status(),
            location: response
                .headers()
                .get(header::LOCATION)
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default(),
            cookies: set_cookies
                .iter()
                .map(|c| c.split(';').next().unwrap().to_owned())
                .collect(),
            max_ages: set_cookies
                .iter()
                .filter_map(|c| {
                    c.split(';')
                        .map(str::trim)
                        .find(|part| part.starts_with("Max-Age="))
                        .map(str::to_owned)
                })
                .collect(),
        }
    }

    /// A browser at Keycloak, which keeps its own cookies and follows no redirect.
    fn browser() -> reqwest::Client {
        let tls = tokio_rustls::rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(tokio_rustls::rustls::RootCertStore::empty())
        .with_no_client_auth();
        reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    /// Opens the provider's page in `browser`, signs in there when it asks, and returns where
    /// the provider sends the browser back to.
    async fn at_keycloak(browser: &reqwest::Client, url: &str, user: &str) -> String {
        let page = browser.get(url).send().await.unwrap();
        if page.status().is_redirection() {
            // Still signed in at the provider: straight back.
            return page.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .to_owned();
        }
        let html = page.text().await.unwrap();
        let form = &html[html.find("id=\"kc-form-login\"").expect("the login form")..];
        let action = &form[form.find("action=\"").unwrap() + 8..];
        let action = action[..action.find('"').unwrap()].replace("&amp;", "&");
        let password = format!("{user}-dev-password");
        let answer = browser
            .post(action)
            .form(&[
                ("username", user),
                ("password", &password),
                ("credentialId", ""),
            ])
            .send()
            .await
            .unwrap();
        assert!(
            answer.status().is_redirection(),
            "{}",
            answer.text().await.unwrap()
        );
        answer.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned()
    }

    /// The callback's path and query, as the browser would ask the core for it.
    fn path_of(url: &str) -> String {
        let url = url::Url::parse(url).unwrap();
        assert_eq!(url.path(), crate::oidc::CALLBACK_PATH);
        format!("{}?{}", url.path(), url.query().unwrap_or_default())
    }

    /// Sets out from the core, signs in at Keycloak as `user` and comes back: the core's answer.
    async fn sign_in_as(state: &AppState, user: &str) -> Answer {
        let begun = get(state, "/api/session/identity-provider", None).await;
        assert_eq!(begun.status, StatusCode::SEE_OTHER);
        let cookie = begun.cookie("hd_sign_in_state").expect("the state cookie");
        let back = at_keycloak(&browser(), &begun.location, user).await;
        get(state, &path_of(&back), Some(&cookie)).await
    }

    async fn agent(db: &PgPool, email: &str, name: &str) -> Uuid {
        crate::agents::add(db, &Grant::command_line(), email, name, None, None)
            .await
            .unwrap()
            .id
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn an_existing_agent_signs_in_through_keycloak_and_out_of_the_product_only(db: PgPool) {
        let state = state(&db);
        agent(&db, "ada@helpdesk.test", "Ada").await;
        let lena = agent(&db, "lena.marsh@example.com", "Lena").await;

        let begun = get(&state, "/api/session/identity-provider", None).await;
        assert_eq!(begun.status, StatusCode::SEE_OTHER);
        let to = url::Url::parse(&begun.location).unwrap();
        assert!(
            to.as_str()
                .starts_with(&format!("{ISSUER}/protocol/openid-connect/auth"))
        );
        let query: std::collections::HashMap<_, _> = to.query_pairs().into_owned().collect();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(query["code_challenge"].len() >= 43);
        assert!(!query["nonce"].is_empty());
        assert!(query["scope"].split(' ').any(|s| s == "openid"));
        let cookie = begun.cookie("hd_sign_in_state").unwrap();
        assert_eq!(cookie, format!("hd_sign_in_state={}", query["state"]));
        let from: String = sqlx::query_scalar("SELECT address FROM identity_provider_sign_ins")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(from, "192.0.2.1");

        let keycloak = browser();
        let back = at_keycloak(&keycloak, &begun.location, "lena").await;
        let signed_in = get(&state, &path_of(&back), Some(&cookie)).await;
        assert_eq!(signed_in.status, StatusCode::SEE_OTHER);
        assert_eq!(signed_in.location, "/");
        let session = signed_in.cookie("hd_session").expect("a session");
        assert!(signed_in.max_ages.contains(&"Max-Age=43200".to_owned()));
        let token = session.trim_start_matches("hd_session=");
        let agent = crate::auth::agent_for(&db, token).await.unwrap().unwrap();
        assert_eq!(agent.id, lena);
        assert_eq!(agent.name, "Lena Marsh");
        assert_eq!(
            crate::auth::tests::stored_session(&db, token).await,
            ("openid_connect".to_owned(), 12.0)
        );

        // Signing out ends the product's session only: Keycloak still knows Lena, and lets her
        // straight back in.
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/session")
                    .header(header::COOKIE, &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(crate::auth::agent_for(&db, token).await.unwrap().is_none());
        let again = get(&state, "/api/session/identity-provider", None).await;
        let cookie = again.cookie("hd_sign_in_state").unwrap();
        let page = keycloak.get(&again.location).send().await.unwrap();
        assert!(page.status().is_redirection(), "Keycloak asked again");
        let back = page.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned();
        let signed_in = get(&state, &path_of(&back), Some(&cookie)).await;
        assert!(signed_in.cookie("hd_session").is_some());
    }

    /// With local sign-in off, agents still sign in through Keycloak, and sessions opened there
    /// outlive the switch.
    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn agents_sign_in_through_keycloak_with_local_sign_in_off(db: PgPool) {
        let state = state(&db);
        let lena = agent(&db, "lena.marsh@example.com", "Lena").await;
        let before = sign_in_as(&state, "lena").await;
        let before = before.cookie("hd_session").expect("a session");
        crate::local_sign_in::switch_off(&db, true).await.unwrap();

        let token = before.trim_start_matches("hd_session=");
        let still = crate::auth::agent_for(&db, token).await.unwrap().unwrap();
        assert_eq!(still.id, lena);
        let after = sign_in_as(&state, "lena").await;
        assert_eq!(after.location, "/");
        let session = after.cookie("hd_session").expect("a session");
        let token = session.trim_start_matches("hd_session=");
        let agent = crate::auth::agent_for(&db, token).await.unwrap().unwrap();
        assert_eq!(agent.id, lena);
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn the_return_needs_the_browser_that_set_out_a_fresh_state_and_the_nonce(db: PgPool) {
        let state = state(&db);
        agent(&db, "lena.marsh@example.com", "Lena").await;
        let failed = "/?sign_in_error=failed";

        // Without the cookie of the browser that set out, or with another sign-in's.
        let begun = get(&state, "/api/session/identity-provider", None).await;
        let other = get(&state, "/api/session/identity-provider", None).await;
        let back = path_of(&at_keycloak(&browser(), &begun.location, "lena").await);
        assert_eq!(get(&state, &back, None).await.location, failed);
        let other_cookie = other.cookie("hd_sign_in_state").unwrap();
        assert_eq!(
            get(&state, &back, Some(&other_cookie)).await.location,
            failed
        );

        // A state is used once.
        let begun = get(&state, "/api/session/identity-provider", None).await;
        let cookie = begun.cookie("hd_sign_in_state").unwrap();
        let back = path_of(&at_keycloak(&browser(), &begun.location, "lena").await);
        assert_eq!(get(&state, &back, Some(&cookie)).await.location, "/");
        assert_eq!(get(&state, &back, Some(&cookie)).await.location, failed);

        // The ID token must carry the nonce the sign-in set out with.
        let begun = get(&state, "/api/session/identity-provider", None).await;
        let cookie = begun.cookie("hd_sign_in_state").unwrap();
        let back = path_of(&at_keycloak(&browser(), &begun.location, "lena").await);
        sqlx::query("UPDATE identity_provider_sign_ins SET nonce = 'another-nonce'")
            .execute(&db)
            .await
            .unwrap();
        let refused = get(&state, &back, Some(&cookie)).await;
        assert_eq!(refused.location, failed);
        assert!(refused.cookie("hd_session").is_none());

        // The provider's own refusal, such as the agent cancelling there.
        let begun = get(&state, "/api/session/identity-provider", None).await;
        let cookie = begun.cookie("hd_sign_in_state").unwrap();
        let query: std::collections::HashMap<_, _> = url::Url::parse(&begun.location)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        let cancelled = format!(
            "{}?error=access_denied&state={}",
            crate::oidc::CALLBACK_PATH,
            query["state"]
        );
        assert_eq!(
            get(&state, &cancelled, Some(&cookie)).await.location,
            failed
        );

        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_sessions")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(sessions, 1);
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn keycloak_sign_ins_are_refused_with_the_reason(db: PgPool) {
        let state = state(&db);
        // Otto's address is not verified at Keycloak; Nadia is not an agent; Piet was
        // deactivated.
        agent(&db, "ada@helpdesk.test", "Ada").await;
        agent(&db, "otto.vale@example.com", "Otto").await;
        let piet = agent(&db, "piet.oak@example.com", "Piet").await;
        crate::agents::deactivate(&db, &Grant::command_line(), piet)
            .await
            .unwrap();
        for (user, reason) in [
            ("otto", "email_not_verified"),
            ("nadia", "not_an_agent"),
            ("piet", "deactivated"),
        ] {
            let answer = sign_in_as(&state, user).await;
            assert_eq!(answer.status, StatusCode::SEE_OTHER);
            assert_eq!(
                answer.location,
                format!("/?sign_in_error={reason}"),
                "{user}"
            );
            assert!(answer.cookie("hd_session").is_none());
        }
        let agents: i64 = sqlx::query_scalar("SELECT count(*) FROM agents")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(agents, 3);
        let still_deactivated: bool =
            sqlx::query_scalar("SELECT deactivated_at IS NOT NULL FROM agents WHERE id = $1")
                .bind(piet)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(still_deactivated);
    }

    /// The agent with the address, as stored: name, role, whether they have a local password,
    /// and the Keycloak identity they are linked to.
    async fn stored_agent(db: &PgPool, email: &str) -> (String, String, bool, Option<String>) {
        sqlx::query_as(
            "SELECT a.name, r.name, a.password_hash IS NOT NULL, i.subject
             FROM agents a JOIN roles r ON r.id = a.role_id
                 LEFT JOIN agent_identities i ON i.agent_id = a.id AND i.issuer = $2
             WHERE a.email = $1",
        )
        .bind(email)
        .bind(ISSUER)
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn agent_count(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM agents")
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn a_first_sign_in_in_the_keycloak_group_makes_an_agent(db: PgPool) {
        let state = state_with_group(&db, GROUP, "groups");
        agent(&db, "ada@helpdesk.test", "Ada").await;

        let answer = sign_in_as(&state, "lena").await;
        assert_eq!(answer.location, "/");
        let token = answer.cookie("hd_session").expect("a session");
        let lena = crate::auth::agent_for(&db, token.trim_start_matches("hd_session="))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lena.email, "lena.marsh@example.com");
        assert_eq!(
            stored_agent(&db, "lena.marsh@example.com").await,
            (
                "Lena Marsh".to_owned(),
                "Agent".to_owned(),
                false,
                Some(LENA.to_owned())
            )
        );

        // Otto's address is not verified at Keycloak; nobody has it, so it is his new agent's.
        assert_eq!(sign_in_as(&state, "otto").await.location, "/");
        assert_eq!(
            stored_agent(&db, "otto.vale@example.com").await,
            (
                "Otto Vale".to_owned(),
                "Agent".to_owned(),
                false,
                Some(OTTO.to_owned())
            )
        );

        // Signing in again finds the same agents.
        assert_eq!(sign_in_as(&state, "lena").await.location, "/");
        assert_eq!(agent_count(&db).await, 3);
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn keycloak_sign_ins_outside_the_authorized_group_are_refused(db: PgPool) {
        // Nadia is an agent, but not in the group at Keycloak.
        agent(&db, "nadia.quist@example.com", "Nadia").await;
        let not_in_group = "/?sign_in_error=not_in_authorized_group";
        let answer = sign_in_as(&state_with_group(&db, GROUP, "groups"), "nadia").await;
        assert_eq!(answer.location, not_in_group);
        assert!(answer.cookie("hd_session").is_none());

        // Keycloak sends no `roles` claim: looked for there, nobody is in the group, not even an
        // agent who is in it under `groups`.
        agent(&db, "lena.marsh@example.com", "Lena").await;
        let roles = state_with_group(&db, GROUP, "roles");
        assert_eq!(sign_in_as(&roles, "lena").await.location, not_in_group);
        // Another group's name: Lena is not in it.
        let other = state_with_group(&db, "helpdesk-admins", "groups");
        assert_eq!(sign_in_as(&other, "lena").await.location, not_in_group);

        assert_eq!(agent_count(&db).await, 2);
        let (linked, sessions): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM agent_identities), (SELECT count(*) FROM agent_sessions)",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!((linked, sessions), (0, 0));
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn a_deactivated_agent_in_the_keycloak_group_is_refused_and_not_recreated(db: PgPool) {
        let state = state_with_group(&db, GROUP, "groups");
        agent(&db, "ada@helpdesk.test", "Ada").await;
        let piet = agent(&db, "piet.oak@example.com", "Piet").await;
        crate::agents::deactivate(&db, &Grant::command_line(), piet)
            .await
            .unwrap();
        let answer = sign_in_as(&state, "piet").await;
        assert_eq!(answer.location, "/?sign_in_error=deactivated");
        assert!(answer.cookie("hd_session").is_none());
        assert_eq!(agent_count(&db).await, 2);
        let still_deactivated: bool =
            sqlx::query_scalar("SELECT deactivated_at IS NOT NULL FROM agents WHERE id = $1")
                .bind(piet)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(still_deactivated);
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn a_linked_agent_is_found_by_the_identifier_after_the_email_changed(db: PgPool) {
        let state = state(&db);
        // Lena was linked when her address was another one.
        let lena = agent(&db, "lena.old@example.com", "Lena Old").await;
        sqlx::query("INSERT INTO agent_identities (issuer, subject, agent_id) VALUES ($1, $2, $3)")
            .bind(ISSUER)
            .bind(LENA)
            .bind(lena)
            .execute(&db)
            .await
            .unwrap();
        let answer = sign_in_as(&state, "lena").await;
        assert_eq!(answer.location, "/");
        let (name, email): (String, String) =
            sqlx::query_as("SELECT name, email FROM agents WHERE id = $1")
                .bind(lena)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            (name.as_str(), email.as_str()),
            ("Lena Marsh", "lena.marsh@example.com")
        );
    }

    /// Otto's agent, linked to his identity at Keycloak when his address was `otto.old`.
    async fn otto_linked(db: &PgPool) -> Uuid {
        let otto = agent(db, "otto.old@example.com", "Otto").await;
        sqlx::query("INSERT INTO agent_identities (issuer, subject, agent_id) VALUES ($1, $2, $3)")
            .bind(ISSUER)
            .bind(OTTO)
            .bind(otto)
            .execute(db)
            .await
            .unwrap();
        otto
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn a_linked_agent_takes_over_the_address_keycloak_has_not_verified(db: PgPool) {
        let state = state(&db);
        let otto = otto_linked(&db).await;
        let answer = sign_in_as(&state, "otto").await;
        assert_eq!(answer.location, "/");
        let (name, email): (String, String) =
            sqlx::query_as("SELECT name, email FROM agents WHERE id = $1")
                .bind(otto)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            (name.as_str(), email.as_str()),
            ("Otto Vale", "otto.vale@example.com")
        );
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn an_unverified_email_of_another_agent_is_refused_at_keycloak_sign_in(db: PgPool) {
        let state = state(&db);
        let otto = otto_linked(&db).await;
        // The address Keycloak has not verified is another agent's.
        agent(&db, "otto.vale@example.com", "Someone else").await;
        let answer = sign_in_as(&state, "otto").await;
        assert_eq!(answer.location, "/?sign_in_error=email_of_another_agent");
        assert!(answer.cookie("hd_session").is_none());
        let email: String = sqlx::query_scalar("SELECT email FROM agents WHERE id = $1")
            .bind(otto)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(email, "otto.old@example.com");
    }

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn an_email_of_another_agent_is_refused_at_keycloak_sign_in(db: PgPool) {
        let state = state(&db);
        // Lena is linked; her address at Keycloak is now Sam's agent's.
        let lena = agent(&db, "lena.old@example.com", "Lena").await;
        sqlx::query("INSERT INTO agent_identities (issuer, subject, agent_id) VALUES ($1, $2, $3)")
            .bind(ISSUER)
            .bind(LENA)
            .bind(lena)
            .execute(&db)
            .await
            .unwrap();
        agent(&db, "lena.marsh@example.com", "Sam").await;
        let answer = sign_in_as(&state, "lena").await;
        assert_eq!(answer.location, "/?sign_in_error=email_of_another_agent");
        assert!(answer.cookie("hd_session").is_none());
    }

    /// Each refusal reaches the sign-in page as its own reason, the one the UI shows a message
    /// for (`providerErrors` in web/src/App.tsx).
    #[test]
    fn each_refusal_has_its_own_reason_on_the_sign_in_page() {
        use crate::identities::Refusal;
        assert_eq!(
            [
                Refusal::NotInAuthorizedGroup,
                Refusal::NoEmail,
                Refusal::EmailNotVerified,
                Refusal::NotAnAgent,
                Refusal::Deactivated,
                Refusal::EmailOfAnotherAgent,
            ]
            .map(Refusal::code),
            [
                "not_in_authorized_group",
                "no_email",
                "email_not_verified",
                "not_an_agent",
                "deactivated",
                "email_of_another_agent",
            ]
        );
    }

    /// Sign-ins under way from `address`, ending in `minutes` (negative: ended).
    async fn pending(db: &PgPool, address: &str, count: i64, minutes: i32) {
        sqlx::query(
            "INSERT INTO identity_provider_sign_ins
                 (state_hash, nonce, pkce_verifier, address, expires_at)
             SELECT sha256(gen_random_uuid()::text::bytea), 'n', 'v', $1,
                    now() + make_interval(mins => $3)
             FROM generate_series(1, $2)",
        )
        .bind(address)
        .bind(count)
        .bind(minutes)
        .execute(db)
        .await
        .unwrap();
    }

    /// Sign-ins under way from `address`, and ended ones from anywhere.
    async fn pending_and_ended(db: &PgPool, address: &str) -> (i64, i64) {
        sqlx::query_as(
            "SELECT count(*) FILTER (WHERE address = $1 AND expires_at > now()),
                    count(*) FILTER (WHERE expires_at <= now())
             FROM identity_provider_sign_ins",
        )
        .bind(address)
        .fetch_one(db)
        .await
        .unwrap()
    }

    /// Setting a sign-in out costs a row and a call to the provider, for anyone: an address has
    /// a bounded number under way, and ended ones are deleted.
    #[sqlx::test]
    async fn sign_ins_under_way_are_limited_per_address_and_ended_ones_deleted(db: PgPool) {
        // A provider that does not answer: a sign-in allowed to set out comes back failed.
        let state = state_with_issuer(&db, "http://127.0.0.1:9/realms/nowhere");
        pending(&db, "192.0.2.7", 20, 5).await;
        pending(&db, "198.51.100.4", 3, -1).await;

        let refused = get_from(&state, "/api/session/identity-provider", None, "192.0.2.7").await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        let wait: u64 = refused.retry_after.clone().unwrap().parse().unwrap();
        assert!((1..=300).contains(&wait), "{wait}");
        assert!(refused.cookie("hd_sign_in_state").is_none());

        // Another address may set out; the provider not answering, nothing stays under way.
        let other = get_from(
            &state,
            "/api/session/identity-provider",
            None,
            "203.0.113.9",
        )
        .await;
        assert_eq!(other.location, "/?sign_in_error=failed");
        assert_eq!(pending_and_ended(&db, "203.0.113.9").await, (0, 0));
        // The ended ones are gone, the 20 under way stay.
        assert_eq!(pending_and_ended(&db, "192.0.2.7").await, (20, 0));
    }

    /// What signing in through the provider last did, as the agents who view its state see it.
    async fn activity(admin: &crate::http::tests::Client) -> serde_json::Value {
        let state = admin.get("/api/admin/identity-provider").await;
        state["openid_connect"]["activity"].clone()
    }

    #[test]
    fn the_provider_s_errors_are_recorded_without_the_client_s_secret() {
        let config = crate::oidc::Config::from_settings(
            crate::oidc::keycloak::settings(
                "https://login.example.com/realms/staff",
                "hd-core",
                "invented-secret",
            ),
            Some("https://helpdesk.example.com"),
        )
        .unwrap()
        .unwrap();
        let (_, activity) = super::connect(config).unwrap();
        activity.failed(&anyhow::anyhow!(
            "client hd-core with invented-secret refused"
        ));
        let recorded = activity.snapshot();
        assert_eq!(recorded.server, "https://login.example.com/realms/staff");
        assert_eq!(
            recorded.last_error.as_deref(),
            Some("client hd-core with *** refused")
        );
    }

    /// A provider that cannot be reached shows as failing; what a browser brings back to the
    /// callback, which anyone can make up, says nothing of the provider and is not recorded.
    #[sqlx::test]
    async fn the_provider_s_state_shows_its_failures_not_what_browsers_bring(db: PgPool) {
        let state = state_with_issuer(&db, "http://127.0.0.1:9/realms/nowhere");
        let admin = crate::http::tests::Client::new(&db)
            .await
            .in_state(state.clone());
        let callback = crate::oidc::CALLBACK_PATH;
        for (made_up, cookie) in [
            ("?error=server_error&state=s", None),
            ("?code=c", None),
            ("?code=c&state=s", None),
            ("?code=c&state=s", Some("hd_sign_in_state=s")),
        ] {
            let back = get(&state, &format!("{callback}{made_up}"), cookie).await;
            assert_eq!(back.location, "/?sign_in_error=failed", "{made_up}");
        }
        // Nor is a failure of the help desk's own database, which says nothing of the provider
        // either.
        let unreachable = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_secs(1))
            .connect_lazy("postgres://hd:hd@127.0.0.1:9/hd")
            .unwrap();
        let broken = AppState {
            db: unreachable,
            ..state.clone()
        };
        let back = get(
            &broken,
            &format!("{callback}?code=c&state=s"),
            Some("hd_sign_in_state=s"),
        )
        .await;
        assert_eq!(back.location, "/?sign_in_error=failed");
        let nothing = activity(&admin).await;
        assert!(
            nothing["last_error"].is_null() && nothing["last_success_at"].is_null(),
            "{nothing}"
        );

        let begun = get(&state, "/api/session/identity-provider", None).await;
        assert_eq!(begun.location, "/?sign_in_error=failed");
        let failing = activity(&admin).await;
        let error = failing["last_error"].as_str().unwrap_or_default();
        assert!(
            error.contains("identity provider's configuration"),
            "{failing}"
        );
        assert!(failing["last_error_at"].is_string(), "{failing}");
    }

    /// A client secret Keycloak does not accept fails at the code's exchange, which is shown
    /// without the secret; a sign-in Keycloak confirms shows the provider working.
    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn the_provider_s_state_follows_sign_ins_at_keycloak(db: PgPool) {
        agent(&db, "lena.marsh@example.com", "Lena").await;
        let wrong = state_from(
            &db,
            crate::oidc::keycloak::settings(ISSUER, "hd-core", "not-the-client-secret"),
        );
        let admin = crate::http::tests::Client::new(&db)
            .await
            .in_state(wrong.clone());
        let refused = sign_in_as(&wrong, "lena").await;
        assert_eq!(refused.location, "/?sign_in_error=failed");
        let failing = activity(&admin).await;
        let error = failing["last_error"].as_str().unwrap_or_default();
        assert!(error.contains("exchanging the code"), "{failing}");
        assert!(failing["last_success_at"].is_null(), "{failing}");
        assert!(!failing.to_string().contains("not-the-client-secret"));

        let right = state(&db);
        let admin = admin.in_state(right.clone());
        assert_eq!(sign_in_as(&right, "lena").await.location, "/");
        let working = activity(&admin).await;
        assert!(working["last_success_at"].is_string(), "{working}");
        assert!(working["last_error"].is_null(), "{working}");
    }
}
