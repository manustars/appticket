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
use crate::roles::Permission;
use crate::tickets::{self, Moved, Priority, RequesterMove, TicketDetail, TicketState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_tickets, open_ticket))
        .routes(routes!(get_ticket))
        .routes(routes!(add_note))
        .routes(routes!(reply))
        .routes(routes!(set_state))
        .routes(routes!(set_priority))
        .routes(routes!(get_draft, save_draft))
        .routes(routes!(accept_proposal))
        .routes(routes!(discard_proposal))
        .routes(routes!(set_assignee))
        .routes(routes!(move_ticket))
        .routes(routes!(set_parent, clear_parent))
        .routes(routes!(resolve_children))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct TicketSummary {
    id: Uuid,
    /// The number agents and requesters see.
    number: i64,
    title: String,
    state: TicketState,
    priority: Priority,
    customer_name: String,
    /// The address a reply goes to: the one the requester last wrote from on the ticket, else the
    /// one an agent last picked there, else the first they registered.
    requester_email: Option<String>,
    /// The ticket's requester has left their customer.
    requester_departed: bool,
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
    customer: Option<String>,
    /// A requester's id.
    requester: Option<String>,
    /// Text found in the title, any of the requester's addresses or the customer's name; `#12` or
    /// `12` also finds ticket 12.
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
    let customer = match filter.customer.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(id) => Some(
            Uuid::parse_str(id)
                .map_err(|_| ApiError::BadRequest("customer must be a customer's id".into()))?,
        ),
    };
    let requester = match filter.requester.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(id) => Some(
            Uuid::parse_str(id)
                .map_err(|_| ApiError::BadRequest("requester must be a requester's id".into()))?,
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
        "SELECT t.id, t.number, t.title, t.state, t.priority, c.name AS customer_name,
                ticket_requester_address(t.id) AS requester_email,
                coalesce(r.departed_at IS NOT NULL, false) AS requester_departed,
                a.name AS assignee_name, t.updated_at
         FROM tickets t
         JOIN customers c ON c.id = t.customer_id
         LEFT JOIN requesters r ON r.id = t.requester_id
         LEFT JOIN agents a ON a.id = t.assignee_id
         WHERE ($1::text[] IS NULL OR t.state = ANY($1))
           AND (NOT $2 OR t.assignee_id IS NULL)
           AND ($3::uuid IS NULL OR t.assignee_id = $3)
           AND ($4::uuid IS NULL OR t.customer_id = $4)
           AND ($7::uuid IS NULL OR t.requester_id = $7)
           AND ($5::text IS NULL OR t.number = $6
                OR t.title ILIKE $5 OR c.name ILIKE $5
                OR EXISTS (SELECT 1 FROM requester_addresses ra
                           WHERE ra.requester_id = t.requester_id AND ra.address ILIKE $5))
         ORDER BY t.updated_at DESC
         LIMIT 200",
    )
    .bind(states)
    .bind(unassigned)
    .bind(assignee)
    .bind(customer)
    .bind(pattern)
    .bind(number)
    .bind(requester)
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
    agent.grant.require(Permission::WorkTickets)?;
    tickets::add_note(&state.db, number, agent.id, &note.body).await?;
    Ok(StatusCode::CREATED)
}

#[derive(Deserialize, ToSchema)]
pub struct NewReply {
    body: String,
    /// The state the ticket moves to; waiting (for the requester) when omitted.
    #[serde(default)]
    then: Option<TicketState>,
    /// One of the requester's addresses, picked by the agent on a ticket the requester has not
    /// written on; needed then when they have several. Where they last wrote from on the ticket,
    /// the reply goes there, and any other address is refused. On a ticket without a requester
    /// none is picked.
    #[serde(default)]
    to: Option<String>,
}

/// Queues a reply to the ticket's requester or, on a ticket without one, to each key requester of
/// its customer at their contact address, in a message of their own; a ticket with neither cannot
/// be replied to. It is sent by e-mail shortly after.
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
    agent.grant.require(Permission::Reply)?;
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
        reply.to.as_deref(),
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
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<StateChange>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
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
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<AssigneeChange>,
) -> Result<StatusCode, ApiError> {
    // Taking a ticket for oneself, or giving back one's own, is working on it; anything else is
    // assigning to others.
    let current: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT assignee_id FROM tickets WHERE number = $1")
            .bind(number)
            .fetch_optional(&state.db)
            .await?;
    let current = current.ok_or(ApiError::NotFound("ticket"))?;
    let own = match change.agent_id {
        Some(target) => target == agent.id,
        None => current.is_none_or(|assignee| assignee == agent.id),
    };
    agent.grant.require(if own {
        Permission::WorkTickets
    } else {
        Permission::AssignTickets
    })?;
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
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<CustomerChange>,
) -> Result<Json<Moved>, ApiError> {
    agent.grant.require(Permission::MoveTickets)?;
    Ok(Json(
        tickets::move_to_customer(&state.db, number, change.customer_id, change.requester).await?,
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct ParentChange {
    /// The number of the help desk's ticket that tracks the failure.
    parent_number: i64,
}

/// Makes the ticket a child of a parent ticket of the help desk's own organization.
#[utoipa::path(put, path = "/api/tickets/{number}/parent", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = ParentChange,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn set_parent(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<ParentChange>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    crate::parents::attach(&state.db, number, change.parent_number).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Detaches the ticket from its parent.
#[utoipa::path(delete, path = "/api/tickets/{number}/parent", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn clear_parent(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    crate::parents::detach(&state.db, number).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct ResolveChildren {
    /// Sent to each child's requester and to the key requesters of each child's customer, each
    /// person once.
    body: String,
}

/// Resolves every open or waiting child of this parent ticket, writing to each child's requester
/// and to the key requesters of each child's customer, one message to each person.
#[utoipa::path(post, path = "/api/tickets/{number}/resolve-children", tag = "tickets",
    params(("number" = i64, Path, description = "The parent ticket's number")),
    request_body = ResolveChildren,
    responses((status = OK, body = crate::parents::ResolvedChildren),
        (status = NOT_FOUND, body = Problem), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn resolve_children(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(request): Json<ResolveChildren>,
) -> Result<Json<crate::parents::ResolvedChildren>, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    agent.grant.require(Permission::Reply)?;
    let Some(sender) = &state.sender else {
        return Err(ApiError::Conflict(
            "sending mail is not configured on this help desk".into(),
        ));
    };
    Ok(Json(
        crate::parents::resolve_children(&state.db, sender, number, agent.id, &request.body)
            .await?,
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct NewTicket {
    customer_id: Uuid,
    title: String,
    /// The requester's address; a new one is registered with the customer, and one of a requester
    /// who departed from another customer passes to a new requester. A departed requester of
    /// this customer is refused. Omit it for planned work without a requester.
    requester_email: Option<String>,
    /// An internal note recording what was said, e.g. on the phone.
    note: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OpenedTicket {
    number: i64,
}

/// Opens a ticket by hand.
#[utoipa::path(post, path = "/api/tickets", tag = "tickets", request_body = NewTicket,
    responses((status = CREATED, body = OpenedTicket), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn open_ticket(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(new): Json<NewTicket>,
) -> Result<(StatusCode, Json<OpenedTicket>), ApiError> {
    agent.grant.require(Permission::OpenTickets)?;
    let number = tickets::open(
        &state.db,
        new.customer_id,
        &new.title,
        new.requester_email.as_deref(),
        new.note.as_deref(),
        agent.id,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(OpenedTicket { number })))
}

#[derive(Deserialize, ToSchema)]
pub struct PriorityChange {
    priority: Priority,
}

/// Changes the ticket's priority.
#[utoipa::path(put, path = "/api/tickets/{number}/priority", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = PriorityChange,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn set_priority(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<PriorityChange>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    tickets::set_priority(&state.db, number, change.priority).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Accepts a proposal: it is applied as if the agent did it. A draft reply comes back to be put
/// in the composer; nothing is sent.
#[utoipa::path(post, path = "/api/proposals/{id}/accept", tag = "proposals",
    params(("id" = Uuid, Path, description = "The proposal")),
    responses((status = OK, body = crate::proposals::Accepted), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn accept_proposal(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::proposals::Accepted>, ApiError> {
    agent
        .grant
        .require(crate::proposals::needs(&state.db, id, true).await?)?;
    Ok(Json(
        crate::proposals::accept(&state.db, id, agent.id).await?,
    ))
}

/// Discards a proposal.
#[utoipa::path(post, path = "/api/proposals/{id}/discard", tag = "proposals",
    params(("id" = Uuid, Path, description = "The proposal")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn discard_proposal(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent
        .grant
        .require(crate::proposals::needs(&state.db, id, false).await?)?;
    crate::proposals::discard(&state.db, id, agent.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The signed-in agent's unsent draft on the ticket, or null.
#[utoipa::path(get, path = "/api/tickets/{number}/draft", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    responses((status = OK, body = Option<crate::drafts::Draft>), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn get_draft(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
) -> Result<Json<Option<crate::drafts::Draft>>, ApiError> {
    Ok(Json(crate::drafts::get(&state.db, number, agent.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct DraftChange {
    mode: crate::drafts::DraftMode,
    /// The text so far; empty removes the draft.
    body: String,
}

/// Saves the signed-in agent's draft on the ticket. Sending a reply or a note clears it.
#[utoipa::path(put, path = "/api/tickets/{number}/draft", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = DraftChange,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn save_draft(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(number): Path<i64>,
    Json(change): Json<DraftChange>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(match change.mode {
        crate::drafts::DraftMode::Reply => Permission::Reply,
        crate::drafts::DraftMode::Note => Permission::WorkTickets,
    })?;
    crate::drafts::save(&state.db, number, agent.id, change.mode, &change.body).await?;
    Ok(StatusCode::NO_CONTENT)
}
