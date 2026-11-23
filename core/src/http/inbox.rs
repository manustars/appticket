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
use crate::roles::Permission;
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
    /// When the sender is a departed requester, the customer they left: their messages wait here
    /// and no ticket is opened for them there.
    departed_from: Option<String>,
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
        "SELECT m.id, m.from_address, m.from_name, m.subject, m.body_text,
                m.created_at AS received_at, c.name AS departed_from
         FROM messages m
         LEFT JOIN requester_addresses a ON a.address = m.from_address
         LEFT JOIN requesters r ON r.id = a.requester_id AND r.departed_at IS NOT NULL
         LEFT JOIN customers c ON c.id = r.customer_id
         WHERE m.direction = 'inbound' AND m.ticket_id IS NULL AND m.discarded_at IS NULL
         ORDER BY m.created_at
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
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(request): Json<OpenTicket>,
) -> Result<(StatusCode, Json<OpenedTicket>), ApiError> {
    agent.grant.require(Permission::SortInbox)?;
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
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(request): Json<Attach>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::SortInbox)?;
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
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::SortInbox)?;
    triage::discard(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
