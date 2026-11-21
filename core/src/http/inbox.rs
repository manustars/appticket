use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{ApiError, AppState, Problem};
use crate::auth::SignedIn;
use crate::triage;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_inbox))
        .routes(routes!(open_ticket))
        .routes(routes!(attach))
        .routes(routes!(discard))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct InboxMessage {
    id: Uuid,
    /// The open triage proposal of an add-on, if any.
    #[sqlx(skip)]
    proposal: Option<crate::proposals::ProposalView>,
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
    State(state): State<AppState>,
    _agent: SignedIn,
) -> Result<Json<Vec<InboxMessage>>, ApiError> {
    let mut messages: Vec<InboxMessage> = sqlx::query_as(
        "SELECT id, from_address, from_name, subject, body_text, created_at AS received_at
         FROM messages
         WHERE direction = 'inbound' AND ticket_id IS NULL AND discarded_at IS NULL
         ORDER BY created_at
         LIMIT 200",
    )
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();
    for (message, proposal) in crate::proposals::open_on_messages(&state.db, &ids).await? {
        if let Some(m) = messages.iter_mut().find(|m| m.id == message) {
            m.proposal = Some(proposal);
        }
    }
    Ok(Json(messages))
}

#[derive(Deserialize, ToSchema)]
pub struct OpenTicket {
    /// The customer the new ticket belongs to.
    customer_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct OpenedTicket {
    number: i64,
}

/// Opens a ticket for an inbox message; its sender becomes the ticket's requester.
#[utoipa::path(post, path = "/api/inbox/{id}/ticket", tag = "inbox",
    params(("id" = Uuid, Path, description = "The inbox message")),
    request_body = OpenTicket,
    responses((status = CREATED, body = OpenedTicket), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn open_ticket(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(request): Json<OpenTicket>,
) -> Result<(StatusCode, Json<OpenedTicket>), ApiError> {
    let number = triage::open_ticket(&state.db, id, request.customer_id).await?;
    Ok((StatusCode::CREATED, Json(OpenedTicket { number })))
}

#[derive(Deserialize, ToSchema)]
pub struct Attach {
    /// The number of the ticket the message belongs to.
    ticket_number: i64,
}

/// Attaches an inbox message to an existing ticket.
#[utoipa::path(post, path = "/api/inbox/{id}/attach", tag = "inbox",
    params(("id" = Uuid, Path, description = "The inbox message")),
    request_body = Attach,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn attach(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(request): Json<Attach>,
) -> Result<StatusCode, ApiError> {
    triage::attach(&state.db, id, request.ticket_number).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Discards an inbox message. It stays stored but leaves the inbox.
#[utoipa::path(post, path = "/api/inbox/{id}/discard", tag = "inbox",
    params(("id" = Uuid, Path, description = "The inbox message")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn discard(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    triage::discard(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
