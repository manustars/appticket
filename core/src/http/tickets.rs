use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{ApiError, AppState, Problem};
use crate::auth::SignedIn;
use crate::tickets::{self, Moved, RequesterMove, TicketDetail, TicketState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_tickets))
        .routes(routes!(get_ticket))
        .routes(routes!(add_note))
        .routes(routes!(reply))
        .routes(routes!(set_state))
        .routes(routes!(set_assignee))
        .routes(routes!(move_ticket))
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

/// Which tickets to list. Every filter is optional; together they all apply.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TicketFilter {
    /// Comma-separated states, e.g. `open,waiting`.
    state: Option<String>,
    /// `me` (the signed-in agent), `none` (unassigned) or an agent's id.
    assignee: Option<String>,
    /// A customer's id.
    customer: Option<Uuid>,
    /// Text found in the title, the requester's address or the customer's name; `#12` or `12`
    /// also finds ticket 12.
    q: Option<String>,
}

/// Tickets, most recently updated first (at most 200).
#[utoipa::path(get, path = "/api/tickets", tag = "tickets", params(TicketFilter),
    responses((status = OK, body = Vec<TicketSummary>), (status = BAD_REQUEST, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn list_tickets(
    State(state): State<AppState>,
    agent: SignedIn,
    Query(filter): Query<TicketFilter>,
) -> Result<Json<Vec<TicketSummary>>, ApiError> {
    let states: Option<Vec<String>> = match filter.state.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(list) => {
            let mut states = Vec::new();
            for s in list.split(',').map(str::trim) {
                if !["open", "waiting", "resolved", "closed"].contains(&s) {
                    return Err(ApiError::BadRequest(format!("unknown ticket state {s:?}")));
                }
                states.push(s.to_owned());
            }
            Some(states)
        }
    };
    let (unassigned, assignee) = match filter.assignee.as_deref().map(str::trim) {
        None | Some("") => (false, None),
        Some("none") => (true, None),
        Some("me") => (false, Some(agent.id)),
        Some(id) => (
            false,
            Some(Uuid::parse_str(id).map_err(|_| {
                ApiError::BadRequest("assignee must be me, none or an agent's id".into())
            })?),
        ),
    };
    let q = filter.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let number: Option<i64> = q.and_then(|q| q.trim_start_matches('#').parse().ok());
    let pattern = q.map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });

    let tickets = sqlx::query_as(
        "SELECT t.id, t.number, t.title, t.state, c.name AS customer_name,
                r.email AS requester_email, a.name AS assignee_name, t.updated_at
         FROM tickets t
         JOIN customers c ON c.id = t.customer_id
         LEFT JOIN requesters r ON r.id = t.requester_id
         LEFT JOIN agents a ON a.id = t.assignee_id
         WHERE ($1::text[] IS NULL OR t.state = ANY($1))
           AND (NOT $2 OR t.assignee_id IS NULL)
           AND ($3::uuid IS NULL OR t.assignee_id = $3)
           AND ($4::uuid IS NULL OR t.customer_id = $4)
           AND ($5::text IS NULL OR t.number = $6
                OR t.title ILIKE $5 OR r.email ILIKE $5 OR c.name ILIKE $5)
         ORDER BY t.updated_at DESC
         LIMIT 200",
    )
    .bind(states)
    .bind(unassigned)
    .bind(assignee)
    .bind(filter.customer)
    .bind(pattern)
    .bind(number)
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

#[derive(Deserialize, ToSchema)]
pub struct CustomerChange {
    customer_id: Uuid,
    /// When the ticket has a requester: move the requester (and all of their tickets) too, or
    /// remove the requester from this ticket.
    requester: RequesterMove,
}

/// Moves the ticket to another customer, for when it was assigned to the wrong one.
#[utoipa::path(put, path = "/api/tickets/{number}/customer", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = CustomerChange,
    responses((status = OK, body = Moved), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn move_ticket(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<CustomerChange>,
) -> Result<Json<Moved>, ApiError> {
    Ok(Json(
        tickets::move_to_customer(&state.db, number, change.customer_id, change.requester).await?,
    ))
}
