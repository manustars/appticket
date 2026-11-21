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
use crate::tickets::{self, TicketDetail, TicketState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_tickets))
        .routes(routes!(get_ticket))
        .routes(routes!(add_note))
        .routes(routes!(reply))
        .routes(routes!(set_state))
        .routes(routes!(set_assignee))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct TicketSummary {
    id: Uuid,
    /// The number agents and requesters see.
    number: i64,
    title: String,
    state: TicketState,
    customer_name: String,
    requester_email: Option<String>,
    assignee_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

/// Tickets, most recently updated first.
#[utoipa::path(get, path = "/api/tickets", tag = "tickets",
    responses((status = OK, body = Vec<TicketSummary>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_tickets(
    State(state): State<AppState>,
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
    .fetch_all(&state.db)
    .await?;
    Ok(Json(tickets))
}

/// A ticket with its messages and notes.
#[utoipa::path(get, path = "/api/tickets/{number}", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    responses((status = OK, body = TicketDetail), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn get_ticket(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(number): Path<i64>,
) -> Result<Json<TicketDetail>, ApiError> {
    Ok(Json(tickets::detail(&state.db, number).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct NewNote {
    body: String,
}

/// Adds an internal note, seen only by agents.
#[utoipa::path(post, path = "/api/tickets/{number}/notes", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = NewNote,
    responses((status = CREATED), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_note(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(note): Json<NewNote>,
) -> Result<StatusCode, ApiError> {
    tickets::add_note(&state.db, number, agent.id, &note.body).await?;
    Ok(StatusCode::CREATED)
}

#[derive(Deserialize, ToSchema)]
pub struct NewReply {
    body: String,
    /// The state the ticket moves to; waiting (for the requester) when omitted.
    #[serde(default)]
    then: Option<TicketState>,
}

/// Queues a reply to the ticket's requester. It is sent by e-mail shortly after.
#[utoipa::path(post, path = "/api/tickets/{number}/replies", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = NewReply,
    responses((status = ACCEPTED), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn reply(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(reply): Json<NewReply>,
) -> Result<StatusCode, ApiError> {
    let Some(sender) = &state.sender else {
        return Err(ApiError::Conflict(
            "sending mail is not configured on this help desk".into(),
        ));
    };
    tickets::reply(
        &state.db,
        sender,
        number,
        agent.id,
        &reply.body,
        reply.then.unwrap_or(TicketState::Waiting),
    )
    .await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize, ToSchema)]
pub struct StateChange {
    state: TicketState,
}

/// Moves the ticket to another state. A closed ticket never changes.
#[utoipa::path(put, path = "/api/tickets/{number}/state", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = StateChange,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn set_state(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<StateChange>,
) -> Result<StatusCode, ApiError> {
    tickets::set_state(&state.db, number, change.state).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct AssigneeChange {
    /// The agent responsible from now on; null leaves the ticket unassigned.
    agent_id: Option<Uuid>,
}

/// Sets or clears the ticket's assignee.
#[utoipa::path(put, path = "/api/tickets/{number}/assignee", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = AssigneeChange,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn set_assignee(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<AssigneeChange>,
) -> Result<StatusCode, ApiError> {
    tickets::assign(&state.db, number, change.agent_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
