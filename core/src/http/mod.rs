//! The HTTP API the agents' UI uses. Its OpenAPI specification is served at /api/openapi.json.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::auth::{self, SESSION_COOKIE, SESSION_DAYS, SignedIn};
use crate::mail_status::MailStatus;
use crate::outbound::Sender;
use crate::sign_in_limits::{Guarded, SignInLimits};
use client_address::ClientAddress;

mod addon;
mod addon_catalog;
mod admin;
mod catalog;
mod client_address;
mod customers;
mod directory;
mod identity_provider;
mod imports;
mod inbox;
mod tickets;
mod web;

pub use client_address::{TrustedProxies, parse_proxies};
pub use identity_provider::connect as connect_identity_provider;
pub use web::with_web;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Help desk core API",
        description = "API of the help desk core."
    ),
    components(schemas(TicketStateChangedEvent, crate::live_details::LiveDetails))
)]
struct ApiDoc;

/// The body of a `ticket.state_changed` delivery to an add-on.
#[derive(ToSchema)]
#[allow(dead_code)] // Only describes the body in the specification.
struct TicketStateChangedEvent {
    /// The same on every retry: skip an id already handled.
    id: Uuid,
    /// `ticket.state_changed`.
    kind: String,
    created_at: time::OffsetDateTime,
    data: crate::tickets::TicketStateChanged,
}

/// The specification with the events the core sends to add-ons as webhooks (sdk/README.md).
fn with_events(mut spec: utoipa::openapi::OpenApi) -> utoipa::openapi::OpenApi {
    use utoipa::openapi::{
        ContentBuilder, HttpMethod, PathItem, PathsBuilder, Ref, Required, Response,
        path::OperationBuilder, request_body::RequestBodyBuilder,
    };
    let operation = OperationBuilder::new()
        .summary(Some("A ticket's state changed"))
        .description(Some(
            "Sent as `POST <url>/events` to the add-ons registered for `ticket.state_changed`, \
             signed with the add-on's event secret and retried until the add-on answers 2xx.",
        ))
        .request_body(Some(
            RequestBodyBuilder::new()
                .content(
                    "application/json",
                    ContentBuilder::new()
                        .schema(Some(Ref::from_schema_name("TicketStateChangedEvent")))
                        .build(),
                )
                .required(Some(Required::True))
                .build(),
        ))
        .response(
            "2XX",
            Response::new("Delivered: the core does not send it again."),
        )
        .build();
    spec.webhooks = Some(
        PathsBuilder::new()
            .path(
                crate::addons::TICKET_STATE_CHANGED,
                PathItem::new(HttpMethod::Post, operation),
            )
            .path(
                "live_details",
                PathItem::new(HttpMethod::Get, live_details()),
            )
            .build(),
    );
    spec
}

/// The read the core makes of an add-on for an asset's live details (sdk/README.md).
fn live_details() -> utoipa::openapi::path::Operation {
    use utoipa::openapi::{
        ContentBuilder, Ref, Required, Response,
        path::{OperationBuilder, ParameterBuilder, ParameterIn},
        schema::{ObjectBuilder, Type},
    };
    let parameter = |name: &str, place: ParameterIn, description: &str| {
        ParameterBuilder::new()
            .name(name)
            .parameter_in(place)
            .required(Required::True)
            .description(Some(description))
            .schema(Some(ObjectBuilder::new().schema_type(Type::String)))
            .build()
    };
    OperationBuilder::new()
        .summary(Some("The live details of an asset"))
        .description(Some(format!(
            "Asked as `GET <url>/assets/{{reference}}` of the add-on an asset comes from, with \
             the add-on's reference for it, whenever agents look at the asset; at most {} reads \
             of one add-on at once. The core waits {} seconds at most; an add-on that does not \
             answer in time, answers another status (404 too) or something that is not live \
             details has its details shown as unavailable. A label may also be a single string, \
             taken as English.",
            crate::live_details::READS_PER_ADDON,
            crate::live_details::TIMEOUT.as_secs(),
        )))
        .parameter(parameter(
            "reference",
            ParameterIn::Path,
            "The add-on's reference for the asset, percent-encoded as one path segment.",
        ))
        .parameter(parameter(
            crate::addons::HEADER_TIMESTAMP,
            ParameterIn::Header,
            "Seconds since the Unix epoch when the core asked.",
        ))
        .parameter(parameter(
            crate::addons::HEADER_SIGNATURE,
            ParameterIn::Header,
            "`sha256=` and the hex HMAC-SHA-256 of `<hd-timestamp>.GET /assets/<reference>`, \
             the reference not percent-encoded, keyed with the add-on's event secret.",
        ))
        .response(
            "200",
            Response::builder()
                .description("The asset's live details.")
                .content(
                    "application/json",
                    ContentBuilder::new()
                        .schema(Some(Ref::from_schema_name("LiveDetails")))
                        .build(),
                )
                .build(),
        )
        .build()
}

/// What every handler can reach.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// The help desk's own address, when sending mail is configured.
    pub sender: Option<Sender>,
    /// What reading and sending mail last did.
    pub mail: MailStatus,
    /// The limits on failed sign-ins.
    pub sign_in_limits: SignInLimits,
    /// The reverse proxies whose forwarded client addresses are believed.
    pub trusted_proxies: TrustedProxies,
    /// The OpenID Connect identity provider agents may sign in through, when configured.
    pub identity_provider: Option<Arc<crate::oidc::Provider>>,
    /// What signing in through the identity provider last did, when one is configured.
    pub identity_provider_activity: Option<crate::mail_status::Tracker>,
    /// The client the core asks add-ons for the live details of assets with.
    pub addon_client: reqwest::Client,
}

#[cfg(test)]
impl AppState {
    pub fn for_tests(db: &PgPool) -> AppState {
        AppState {
            addon_client: crate::live_details::http_client().expect("an HTTP client"),
            db: db.clone(),
            sender: Some(crate::testing::sender()),
            mail: MailStatus::default(),
            sign_in_limits: SignInLimits::for_tests(5, std::time::Duration::from_secs(15 * 60)),
            trusted_proxies: TrustedProxies::default(),
            identity_provider: None,
            identity_provider_activity: None,
        }
    }
}

/// Serves the API on `listen`, and the built UI from `web_dir` when given.
pub async fn serve(
    state: AppState,
    listen: SocketAddr,
    web_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    let app = match &web_dir {
        Some(dir) => {
            tracing::info!(dir = %dir.display(), "serving the UI");
            with_web(router(state), dir)
        }
        None => router(state),
    };
    tracing::info!(%listen, "serving the HTTP API");
    // Each request carries its connection's address, which failed sign-ins are counted under.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("serving the HTTP API")
}

/// Ctrl-C, or SIGTERM where there is one: a container's stop signal.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    api_router().split_for_parts().1
}

fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(with_events(ApiDoc::openapi()))
        .routes(routes!(health))
        .routes(routes!(current_session, sign_in, sign_out))
        .routes(routes!(change_password))
        .routes(routes!(sign_in_options))
        .merge(identity_provider::routes())
        .merge(tickets::routes())
        .merge(inbox::routes())
        .merge(directory::routes())
        .merge(customers::routes())
        .merge(imports::routes())
        .merge(catalog::routes())
        .merge(addon::routes())
        .merge(admin::routes())
}

pub fn router(state: AppState) -> Router {
    let (router, spec) = api_router().split_for_parts();
    router
        .route("/api/openapi.json", get(move || async move { Json(spec) }))
        .with_state(state)
}

/// An error answered to the client; details of internal errors go to the log only.
#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    /// The request itself is malformed; the text says how.
    BadRequest(String),
    NotFound(&'static str),
    /// The request is well formed but breaks a rule; the text says which.
    Conflict(String),
    /// The agent's role does not allow the request; the text says what.
    Forbidden(String),
    /// Too many failed sign-ins: attempts are refused for `retry_after`.
    TooManySignIns {
        retry_after: std::time::Duration,
    },
    /// Too many sign-ins through the identity provider under way from the address: the first
    /// ends after `retry_after`.
    TooManySignInsUnderWay {
        retry_after: std::time::Duration,
    },
    /// A local password while local sign-in is off: the same answer for every account.
    LocalSignInOff,
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        ApiError::Internal(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut retry_after = None;
        let mut code = None;
        let (status, error) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "not signed in".to_owned()),
            ApiError::BadRequest(why) => (StatusCode::BAD_REQUEST, why),
            ApiError::NotFound(what) => (StatusCode::NOT_FOUND, format!("{what} not found")),
            ApiError::Conflict(why) => (StatusCode::CONFLICT, why),
            ApiError::Forbidden(why) => (StatusCode::FORBIDDEN, why),
            ApiError::TooManySignIns { retry_after: wait } => {
                retry_after = Some(whole_seconds(wait));
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many failed sign-ins: try again later".to_owned(),
                )
            }
            ApiError::TooManySignInsUnderWay { retry_after: wait } => {
                retry_after = Some(whole_seconds(wait));
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many sign-ins through the identity provider are under way from this \
                     address: try again later"
                        .to_owned(),
                )
            }
            ApiError::LocalSignInOff => {
                code = Some(ProblemCode::LocalSignInOff);
                (
                    StatusCode::FORBIDDEN,
                    "local sign-in is off: sign in through the identity provider".to_owned(),
                )
            }
            ApiError::Internal(error) => {
                tracing::error!(error = format!("{error:#}"), "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_owned(),
                )
            }
        };
        let mut response = (
            status,
            Json(Problem {
                error: sentence(&error),
                code,
            }),
        )
            .into_response();
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

/// Whole seconds for `Retry-After`, rounded up, so that a client retrying then is not refused
/// again.
fn whole_seconds(wait: std::time::Duration) -> u64 {
    wait.as_secs() + u64::from(wait.subsec_nanos() > 0)
}

/// Shown to agents as is: capitalised and ending with a full stop.
fn sentence(text: &str) -> String {
    let mut chars = text.trim().chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Why a request failed, in words an agent can read.
#[derive(Serialize, ToSchema)]
pub struct Problem {
    pub error: String,
    /// What went wrong, for the UI to tell apart from other refusals with the same status; only
    /// on the answers that need it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<ProblemCode>,
}

/// A refusal the UI recognises, whatever the wording of `error`.
#[derive(Serialize, ToSchema, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ProblemCode {
    /// A local password while local sign-in is off.
    LocalSignInOff,
}

/// Requires a signed-in agent.
impl FromRequestParts<AppState> for SignedIn {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let Some(cookie) = jar.get(SESSION_COOKIE) else {
            return Err(ApiError::Unauthorized);
        };
        auth::agent_for(&state.db, cookie.value())
            .await?
            .ok_or(ApiError::Unauthorized)
    }
}

#[derive(Serialize, ToSchema)]
struct Health {
    status: String,
}

/// Whether the core and its database are up.
#[utoipa::path(get, path = "/api/health", tag = "system",
    responses((status = OK, body = Health), (status = SERVICE_UNAVAILABLE, body = Health)))]
async fn health(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => (
            StatusCode::OK,
            Json(Health {
                status: "ok".into(),
            }),
        ),
        Err(error) => {
            tracing::warn!(%error, "health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Health {
                    status: "database unavailable".into(),
                }),
            )
        }
    }
}

#[derive(Serialize, ToSchema)]
struct Agent {
    id: Uuid,
    email: String,
    name: String,
    /// Whether the agent has a local password to sign in and change; agents created by signing
    /// in through the identity provider have none.
    has_local_password: bool,
    role_name: String,
    /// Whether the agent's role is the Administrator role.
    administrator: bool,
    /// Everything the agent's role allows beyond reading; every permission for an Administrator.
    permissions: Vec<crate::roles::Permission>,
}

#[derive(Deserialize, ToSchema)]
struct SignInRequest {
    email: String,
    password: String,
}

/// The signed-in agent, with what their role allows.
#[utoipa::path(get, path = "/api/session", tag = "session",
    responses((status = OK, body = Agent), (status = UNAUTHORIZED, body = Problem)))]
async fn current_session(agent: SignedIn) -> Json<Agent> {
    Json(Agent {
        id: agent.id,
        permissions: agent.grant.effective(),
        administrator: agent.grant.administrator,
        email: agent.email,
        name: agent.name,
        has_local_password: agent.has_local_password,
        role_name: agent.role_name,
    })
}

/// How agents can sign in, for the sign-in page.
#[derive(Serialize, ToSchema)]
struct SignInOptions {
    /// The identity provider's button, when one is configured.
    identity_provider: Option<IdentityProviderButton>,
    /// Whether agents can sign in with a local password; when not, the page offers the identity
    /// provider only.
    local_sign_in: bool,
}

#[derive(Serialize, ToSchema)]
struct IdentityProviderButton {
    /// What the button names: "Sign in with <label>".
    label: String,
}

/// How agents can sign in; asked before anyone is signed in.
#[utoipa::path(get, path = "/api/session/options", tag = "session",
    responses((status = OK, body = SignInOptions)))]
async fn sign_in_options(State(state): State<AppState>) -> Result<Json<SignInOptions>, ApiError> {
    Ok(Json(SignInOptions {
        identity_provider: state.identity_provider.as_ref().map(|provider| {
            IdentityProviderButton {
                label: provider.label().to_owned(),
            }
        }),
        local_sign_in: crate::local_sign_in::is_on(&state.db).await?,
    }))
}

#[derive(Deserialize, ToSchema)]
struct PasswordChange {
    current_password: String,
    new_password: String,
}

/// Changes the signed-in agent's password; their other sessions end. Refused (409) for an agent
/// without a local password.
#[utoipa::path(put, path = "/api/session/password", tag = "session", request_body = PasswordChange,
    responses((status = NO_CONTENT), (status = CONFLICT, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn change_password(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(change): Json<PasswordChange>,
) -> Result<StatusCode, ApiError> {
    crate::agents::change_own_password(
        &state.db,
        agent.id,
        &agent.token,
        &change.current_password,
        &change.new_password,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Signs an agent in and sets the session cookie.
///
/// Repeated failures for the e-mail or from the same address are slowed, then refused for a
/// while (`Retry-After` says how long) without the password being checked. While local sign-in is
/// off every attempt is refused before anything is checked, the same way for every account.
#[utoipa::path(post, path = "/api/session", tag = "session", request_body = SignInRequest,
    responses((status = NO_CONTENT), (status = UNAUTHORIZED, body = Problem),
        (status = FORBIDDEN, body = Problem, description = "Local sign-in is off: `code` is \
            `local_sign_in_off`, the same for every account"),
        (status = TOO_MANY_REQUESTS, body = Problem,
            headers(("Retry-After" = u64, description = "Seconds until attempts are accepted again")))))]
async fn sign_in(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    jar: CookieJar,
    Json(request): Json<SignInRequest>,
) -> Result<(CookieJar, StatusCode), ApiError> {
    // Before the limits: nothing is guessed, so nothing is counted.
    if !crate::local_sign_in::is_on(&state.db).await? {
        return Err(ApiError::LocalSignInOff);
    }
    let check = auth::sign_in(&state.db, &request.email, &request.password);
    let token = match state
        .sign_in_limits
        .guard(&state.db, &request.email, address, check)
        .await
    {
        Ok(Guarded::Passed(token)) => token,
        Ok(Guarded::Failed) => return Err(ApiError::Unauthorized),
        Ok(Guarded::Refused { retry_after }) => {
            return Err(ApiError::TooManySignIns { retry_after });
        }
        // Switched off while the password was being checked.
        Err(error) if error.is::<crate::local_sign_in::Off>() => {
            return Err(ApiError::LocalSignInOff);
        }
        Err(error) => return Err(error.into()),
    };
    let cookie = session_cookie(token, time::Duration::days(SESSION_DAYS.into()));
    Ok((jar.add(cookie), StatusCode::NO_CONTENT))
}

/// The cookie that carries a session's token, for as long as the session lasts.
fn session_cookie(token: String, lifetime: time::Duration) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .max_age(lifetime)
        .build()
}

/// Signs the agent out.
#[utoipa::path(delete, path = "/api/session", tag = "session", responses((status = NO_CONTENT)))]
async fn sign_out(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), ApiError> {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        auth::sign_out(&state.db, cookie.value()).await?;
    }
    Ok((
        jar.remove(Cookie::build(SESSION_COOKIE).path("/")),
        StatusCode::NO_CONTENT,
    ))
}

impl From<crate::error::Error> for ApiError {
    fn from(error: crate::error::Error) -> Self {
        match error {
            crate::error::Error::NotFound(what) => ApiError::NotFound(what),
            crate::error::Error::Rule(why) => ApiError::Conflict(why),
            crate::error::Error::Forbidden(why) => ApiError::Forbidden(why),
            crate::error::Error::Internal(error) => ApiError::Internal(error),
        }
    }
}

#[cfg(test)]
pub mod tests;
