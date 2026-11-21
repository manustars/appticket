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
use time::OffsetDateTime;
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::auth::{self, SESSION_COOKIE, SESSION_DAYS, SignedIn};

#[derive(OpenApi)]
#[openapi(info(
    title = "Help desk core API",
    description = "API of the help desk core."
))]
struct ApiDoc;

pub async fn serve(db: PgPool, listen: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, "serving the HTTP API");
    axum::serve(listener, router(db))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving the HTTP API")
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    api_router().split_for_parts().1
}

fn api_router() -> OpenApiRouter<PgPool> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health))
        .routes(routes!(current_session, sign_in, sign_out))
        .routes(routes!(list_tickets))
        .routes(routes!(list_inbox))
}

fn router(db: PgPool) -> Router {
    let (router, spec) = api_router().split_for_parts();
    router
        .route("/api/openapi.json", get(move || async move { Json(spec) }))
        .with_state(db)
}

/// An error answered to the client; details of internal errors go to the log only.
pub enum ApiError {
    Unauthorized,
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        ApiError::Internal(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(Problem {
                    error: "not signed in".into(),
                }),
            )
                .into_response(),
            ApiError::Internal(error) => {
                tracing::error!(error = format!("{error:#}"), "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(Problem {
                        error: "internal error".into(),
                    }),
                )
                    .into_response()
            }
        }
    }
}

#[derive(Serialize, ToSchema)]
struct Problem {
    error: String,
}

/// Requires a signed-in agent.
impl FromRequestParts<PgPool> for SignedIn {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, db: &PgPool) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let Some(cookie) = jar.get(SESSION_COOKIE) else {
            return Err(ApiError::Unauthorized);
        };
        auth::agent_for(db, cookie.value())
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
async fn health(State(db): State<PgPool>) -> (StatusCode, Json<Health>) {
    match sqlx::query("SELECT 1").execute(&db).await {
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
    State(db): State<PgPool>,
    jar: CookieJar,
    Json(request): Json<SignInRequest>,
) -> Result<(CookieJar, StatusCode), ApiError> {
    let token = auth::sign_in(&db, &request.email, &request.password)
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
    State(db): State<PgPool>,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), ApiError> {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        auth::sign_out(&db, cookie.value()).await?;
    }
    Ok((
        jar.remove(Cookie::build(SESSION_COOKIE).path("/")),
        StatusCode::NO_CONTENT,
    ))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
struct TicketSummary {
    id: Uuid,
    /// The number agents and requesters see.
    number: i64,
    title: String,
    #[schema(inline)]
    state: TicketState,
    customer_name: String,
    requester_email: Option<String>,
    assignee_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

#[derive(Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
enum TicketState {
    Open,
    Waiting,
    Resolved,
    Closed,
}

/// Tickets, most recently updated first.
#[utoipa::path(get, path = "/api/tickets", tag = "tickets",
    responses((status = OK, body = Vec<TicketSummary>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_tickets(
    State(db): State<PgPool>,
    _agent: SignedIn,
) -> Result<Json<Vec<TicketSummary>>, ApiError> {
    let tickets = sqlx::query_as(
        "SELECT t.id, t.number, t.title, t.state, c.name AS customer_name,
                r.email AS requester_email, a.name AS assignee_name, t.updated_at
         FROM tickets t
         JOIN customers c ON c.id = t.customer_id
         LEFT JOIN requesters r ON r.id = t.requester_id
         LEFT JOIN agents a ON a.id = t.assignee_id
         ORDER BY t.updated_at DESC
         LIMIT 200",
    )
    .fetch_all(&db)
    .await?;
    Ok(Json(tickets))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
struct InboxMessage {
    id: Uuid,
    from_address: String,
    from_name: Option<String>,
    subject: String,
    body_text: String,
    #[serde(with = "time::serde::rfc3339")]
    received_at: OffsetDateTime,
}

/// Messages waiting in the inbox, oldest first.
#[utoipa::path(get, path = "/api/inbox", tag = "inbox",
    responses((status = OK, body = Vec<InboxMessage>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_inbox(
    State(db): State<PgPool>,
    _agent: SignedIn,
) -> Result<Json<Vec<InboxMessage>>, ApiError> {
    let messages = sqlx::query_as(
        "SELECT id, from_address, from_name, subject, body_text, received_at
         FROM messages WHERE ticket_id IS NULL AND discarded_at IS NULL
         ORDER BY received_at
         LIMIT 200",
    )
    .fetch_all(&db)
    .await?;
    Ok(Json(messages))
}
