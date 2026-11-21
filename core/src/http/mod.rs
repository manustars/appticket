//! The HTTP API the agents' UI uses. Its OpenAPI specification is served at /api/openapi.json.

use std::net::SocketAddr;

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{StatusCode, request::Parts},
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
use crate::outbound::Sender;

mod addon;
mod catalog;
mod customers;
mod directory;
mod inbox;
mod tickets;

#[derive(OpenApi)]
#[openapi(info(
    title = "Help desk core API",
    description = "API of the help desk core."
))]
struct ApiDoc;

/// What every handler can reach.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// The help desk's own address, when sending mail is configured.
    pub sender: Option<Sender>,
}

pub async fn serve(state: AppState, listen: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, "serving the HTTP API");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving the HTTP API")
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    api_router().split_for_parts().1
}

fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health))
        .routes(routes!(current_session, sign_in, sign_out))
        .merge(tickets::routes())
        .merge(inbox::routes())
        .merge(directory::routes())
        .merge(customers::routes())
        .merge(catalog::routes())
        .merge(addon::routes())
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
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        ApiError::Internal(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "not signed in".to_owned()),
            ApiError::BadRequest(why) => (StatusCode::BAD_REQUEST, why),
            ApiError::NotFound(what) => (StatusCode::NOT_FOUND, format!("{what} not found")),
            ApiError::Conflict(why) => (StatusCode::CONFLICT, why),
            ApiError::Internal(error) => {
                tracing::error!(error = format!("{error:#}"), "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_owned(),
                )
            }
        };
        (
            status,
            Json(Problem {
                error: sentence(&error),
            }),
        )
            .into_response()
    }
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
}

#[derive(Deserialize, ToSchema)]
struct SignInRequest {
    email: String,
    password: String,
}

/// The signed-in agent.
#[utoipa::path(get, path = "/api/session", tag = "session",
    responses((status = OK, body = Agent), (status = UNAUTHORIZED, body = Problem)))]
async fn current_session(agent: SignedIn) -> Json<Agent> {
    Json(Agent {
        id: agent.id,
        email: agent.email,
        name: agent.name,
    })
}

/// Signs an agent in and sets the session cookie.
#[utoipa::path(post, path = "/api/session", tag = "session", request_body = SignInRequest,
    responses((status = NO_CONTENT), (status = UNAUTHORIZED, body = Problem)))]
async fn sign_in(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(request): Json<SignInRequest>,
) -> Result<(CookieJar, StatusCode), ApiError> {
    let token = auth::sign_in(&state.db, &request.email, &request.password)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let cookie = Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(SESSION_DAYS.into()));
    Ok((jar.add(cookie), StatusCode::NO_CONTENT))
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
            crate::error::Error::Internal(error) => ApiError::Internal(error),
        }
    }
}

#[cfg(test)]
pub mod tests;
