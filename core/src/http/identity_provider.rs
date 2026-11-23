//! Signing in through the OpenID Connect identity provider: the browser is sent there, and comes
//! back to the callback, which opens the product's session or says on the sign-in page why not.

use axum::{
    extract::{Query, State},
    response::Redirect,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{ApiError, AppState};
use crate::identities::{self, Refusal};
use crate::oidc::Return;

/// The cookie that keeps a sign-in's state in the browser that set out, until it comes back.
const STATE_COOKIE: &str = "hd_sign_in_state";
const BEGIN_PATH: &str = "/api/session/identity-provider";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(begin))
        .routes(routes!(callback))
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
        configured")))]
async fn begin(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), ApiError> {
    let provider = state
        .identity_provider
        .as_ref()
        .ok_or(ApiError::NotFound("identity provider"))?;
    match provider.begin(&state.db).await {
        Ok(begun) => {
            let cookie = Cookie::build((STATE_COOKIE, begun.state))
                .path(BEGIN_PATH)
                .http_only(true)
                .secure(true)
                // Lax: sent along when the provider sends the browser back.
                .same_site(SameSite::Lax)
                .max_age(time::Duration::minutes(10));
            Ok((jar.add(cookie), Redirect::to(begun.url.as_str())))
        }
        Err(error) => {
            tracing::warn!(
                error = format!("{error:#}"),
                "the identity provider cannot be reached"
            );
            Ok((jar, sign_in_error("failed")))
        }
    }
}

/// Where the identity provider sends the browser back: opens the product's session and goes to
/// the help desk, or back to the sign-in page with `sign_in_error` saying why not:
/// `email_of_another_agent`, `email_not_verified`, `not_an_agent`, `deactivated` or `failed`.
#[utoipa::path(get, path = "/api/session/identity-provider/callback", tag = "session",
    params(Return),
    responses((status = SEE_OTHER, description = "To the help desk, signed in, or to the \
        sign-in page with the reason"),
        (status = NOT_FOUND, body = super::Problem, description = "No identity provider is \
        configured")))]
async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(back): Query<Return>,
) -> Result<(CookieJar, Redirect), ApiError> {
    let provider = state
        .identity_provider
        .as_ref()
        .ok_or(ApiError::NotFound("identity provider"))?;
    let kept = jar.get(STATE_COOKIE).map(|c| c.value().to_owned());
    let jar = jar.remove(Cookie::build(STATE_COOKIE).path(BEGIN_PATH));
    let identity = match provider.finish(&state.db, kept.as_deref(), &back).await {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(
                error = format!("{error:#}"),
                "signing in through the identity provider failed"
            );
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
            Refusal::EmailOfAnotherAgent => "email_of_another_agent",
            Refusal::EmailNotVerified => "email_not_verified",
            Refusal::NotAnAgent => "not_an_agent",
            Refusal::Deactivated => "deactivated",
        }
    }
}

#[cfg(test)]
mod tests {
    //! Against the development Keycloak (`dev/keycloak`, started by compose.yaml and in CI).

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use sqlx::PgPool;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::http::{AppState, router};
    use crate::roles::Grant;

    const ISSUER: &str = "http://127.0.0.1:55080/realms/helpdesk";
    /// Lena's stable identifier at Keycloak.
    const LENA: &str = "6f1c2a0e-8d3b-4c55-9a41-0b7e2d6f3a01";

    fn state(db: &PgPool) -> AppState {
        let settings = crate::oidc::Settings {
            issuer: Some(ISSUER.to_owned()),
            client_id: Some("hd-core".to_owned()),
            client_secret: Some("dev-only-client-secret".to_owned()),
            client_secret_file: None,
            label: Some("Northgate".to_owned()),
            session_hours: 12,
            public_url: Some("http://127.0.0.1:8080".to_owned()),
        };
        let config = crate::oidc::Config::from_settings(settings)
            .unwrap()
            .unwrap();
        AppState {
            identity_provider: Some(crate::oidc::Provider::new(config).unwrap()),
            ..AppState::for_tests(db)
        }
    }

    /// What the core answered: status, Location and the cookies it set, as name=value.
    struct Answer {
        status: StatusCode,
        location: String,
        cookies: Vec<String>,
        max_ages: Vec<String>,
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
        let mut request = Request::builder().uri(uri);
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

    #[sqlx::test]
    #[ignore = "needs the development Keycloak (podman compose up -d keycloak)"]
    async fn a_linked_agent_keeps_their_address_when_keycloak_has_not_verified_the_new_one(
        db: PgPool,
    ) {
        let state = state(&db);
        // Otto was linked with another address; Keycloak has not verified his current one.
        let otto = agent(&db, "otto.old@example.com", "Otto").await;
        sqlx::query("INSERT INTO agent_identities (issuer, subject, agent_id) VALUES ($1, $2, $3)")
            .bind(ISSUER)
            .bind("6f1c2a0e-8d3b-4c55-9a41-0b7e2d6f3a02")
            .bind(otto)
            .execute(&db)
            .await
            .unwrap();
        // The unverified address is another agent's: it is not taken over, so no reason to
        // refuse.
        agent(&db, "otto.vale@example.com", "Someone else").await;
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
            ("Otto Vale", "otto.old@example.com")
        );
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
}
