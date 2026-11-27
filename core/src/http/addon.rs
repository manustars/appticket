//! The part of the API add-ons call, with `Authorization: Bearer <token>`. Each operation needs
//! a scope the add-on was registered with.

use axum::{
    Json,
    extract::{FromRequestParts, Path, Query, State},
    http::{StatusCode, header, request::Parts},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{ApiError, AppState, Problem};
use crate::addon_tickets::{self, MAX_REFERENCE_LENGTH};
use crate::addons::{self, Caller, DIRECTORY_READ, PROPOSALS_WRITE, TICKETS_READ, TICKETS_WRITE};
use crate::catalog::Asset;
use crate::external_references::{self, NameInSystem};
use crate::proposals::{self, Content, Target};
use crate::requesters::SiteRef;
use crate::sentences::SentenceInput;
use crate::tickets::{self, Priority, TicketDetail, TicketState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(whoami))
        .routes(routes!(customers))
        .routes(routes!(customer_assets))
        .routes(routes!(customer_services))
        .routes(routes!(open_tickets))
        .routes(routes!(opened_tickets))
        .routes(routes!(ticket_by_reference))
        .routes(routes!(ticket))
        .routes(routes!(propose))
        .routes(routes!(open_ticket))
        .routes(routes!(add_note))
        .routes(routes!(resolve_ticket))
        .merge(super::addon_catalog::routes())
        .merge(super::addon_names::routes())
        .merge(super::addon_conflicts::routes())
}

/// Requires an enabled add-on's token.
impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        addons::caller(&state.db, token.trim())
            .await?
            .ok_or(ApiError::Unauthorized)
    }
}

#[derive(Serialize, ToSchema)]
pub struct AddonIdentity {
    name: String,
    scopes: Vec<String>,
    /// The languages of the agents' UI: write each rationale in all of them.
    languages: Vec<String>,
}

/// The add-on the token belongs to, and what it may do.
#[utoipa::path(get, path = "/api/addon/whoami", tag = "add-ons",
    responses((status = OK, body = AddonIdentity), (status = UNAUTHORIZED, body = Problem)))]
async fn whoami(caller: Caller) -> Json<AddonIdentity> {
    Json(AddonIdentity {
        name: caller.name,
        scopes: caller.scopes,
        languages: proposals::UI_LANGUAGES
            .iter()
            .map(|l| (*l).to_owned())
            .collect(),
    })
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct AddonCustomer {
    id: Uuid,
    name: String,
    is_help_desk: bool,
    /// Mail domains that identify the customer's requesters.
    domains: Vec<String>,
    /// The names other systems use for the customer, by system and name. A name names exactly
    /// one customer within its system; systems and names are compared ignoring case.
    #[sqlx(skip)]
    external_references: Vec<NameInSystem>,
}

/// Customers that can get tickets, with their mail domains and external references. Scope
/// `directory:read`.
#[utoipa::path(get, path = "/api/addon/customers", tag = "add-ons",
    responses((status = OK, body = Vec<AddonCustomer>), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn customers(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<Vec<AddonCustomer>>, ApiError> {
    caller.require(DIRECTORY_READ)?;
    let mut customers: Vec<AddonCustomer> = sqlx::query_as(
        "SELECT c.id, c.name, c.is_help_desk,
                coalesce((SELECT array_agg(d.domain ORDER BY d.domain) FROM customer_domains d
                          WHERE d.customer_id = c.id), '{}') AS domains
         FROM customers c WHERE c.offboarded_at IS NULL ORDER BY c.name",
    )
    .fetch_all(&state.db)
    .await?;
    // Only the references of the customers listed: the add-on sees nothing of the others.
    let mut references = external_references::by_customer(&state.db).await?;
    for customer in &mut customers {
        customer.external_references = references.remove(&customer.id).unwrap_or_default();
    }
    Ok(Json(customers))
}

/// An asset as add-ons read it: what an add-on can be the source of, with its own site, but not
/// its holder, who is a person, nor the site it is at through them.
#[derive(Serialize, ToSchema)]
pub struct AddonAsset {
    id: Uuid,
    customer_id: Uuid,
    customer_name: String,
    kind_id: Uuid,
    /// The name of its asset kind: laptop, printer, application, licence…
    kind: String,
    name: String,
    serial_number: Option<String>,
    /// Its own site, one of its customer's.
    site: Option<SiteRef>,
    /// The add-on it came from, when an add-on supplies it.
    source_addon: Option<String>,
    /// Your own reference for it, when you are its source or linked to it; another add-on's is not
    /// given.
    reference: Option<String>,
    /// The other add-ons linked to it, by name: they change nothing of it, and only supply live
    /// details and links.
    linked_addons: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    retired_at: Option<OffsetDateTime>,
}

impl AddonAsset {
    /// The asset as `caller` reads it.
    pub(super) fn seen_by(a: Asset, caller: &Caller) -> AddonAsset {
        let reference = if a.source_addon.as_deref() == Some(caller.name.as_str()) {
            a.reference
        } else {
            a.linked_addons
                .iter()
                .find(|l| l.addon == caller.name)
                .map(|l| l.reference.clone())
        };
        let linked_addons = a.linked_addons.into_iter().map(|l| l.addon).collect();
        AddonAsset {
            id: a.id,
            customer_id: a.customer_id,
            customer_name: a.customer_name,
            kind_id: a.kind_id,
            kind: a.kind,
            name: a.name,
            serial_number: a.serial_number,
            site: a.site,
            source_addon: a.source_addon,
            reference,
            linked_addons,
            retired_at: a.retired_at,
        }
    }
}

/// A customer's active assets. Scope `directory:read`.
#[utoipa::path(get, path = "/api/addon/customers/{id}/assets", tag = "add-ons",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = OK, body = Vec<AddonAsset>), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn customer_assets(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AddonAsset>>, ApiError> {
    caller.require(DIRECTORY_READ)?;
    let assets = crate::catalog::assets_of(&state.db, id).await?;
    Ok(Json(
        assets
            .into_iter()
            .filter(|a| a.retired_at.is_none())
            .map(|a| AddonAsset::seen_by(a, &caller))
            .collect(),
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OpenTicketsFilter {
    /// Tickets of the requester this address belongs to (any of their addresses).
    requester_email: Option<String>,
    /// Tickets of this customer.
    customer: Option<Uuid>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct AddonTicketSummary {
    number: i64,
    title: String,
    state: TicketState,
    priority: Priority,
    customer_id: Uuid,
    /// The address a reply goes to: the one the requester last wrote from on the ticket, else the
    /// one an agent last picked there, else the first they registered.
    requester_email: Option<String>,
    /// Your ticket reference, when you opened the ticket with one; another add-on's is not given.
    reference: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

/// The query of [`AddonTicketSummary`]s as the add-on `$1` reads them, with `rest` after `FROM
/// tickets t`: its own ticket references only.
macro_rules! select_summaries {
    ($rest:literal) => {
        concat!(
            "SELECT t.number, t.title, t.state, t.priority, t.customer_id,
                    ticket_requester_address(t.id) AS requester_email,
                    CASE WHEN t.opened_by_addon_id = $1 THEN t.addon_reference END AS reference,
                    t.updated_at
             FROM tickets t ",
            $rest
        )
    };
}

/// Tickets that are not closed, of a requester or a customer, most recent first (at most 50).
/// Scope `tickets:read`.
#[utoipa::path(get, path = "/api/addon/tickets", tag = "add-ons", params(OpenTicketsFilter),
    responses((status = OK, body = Vec<AddonTicketSummary>), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn open_tickets(
    State(state): State<AppState>,
    caller: Caller,
    Query(filter): Query<OpenTicketsFilter>,
) -> Result<Json<Vec<AddonTicketSummary>>, ApiError> {
    caller.require(TICKETS_READ)?;
    if filter.requester_email.is_none() && filter.customer.is_none() {
        return Err(ApiError::BadRequest(
            "give requester_email or customer".into(),
        ));
    }
    Ok(Json(
        sqlx::query_as(select_summaries!(
            "WHERE t.state <> 'closed'
               AND ($2::text IS NULL OR t.requester_id =
                    (SELECT requester_id FROM requester_addresses WHERE address = lower($2)))
               AND ($3::uuid IS NULL OR t.customer_id = $3)
             ORDER BY t.updated_at DESC LIMIT 50"
        ))
        .bind(caller.id)
        .bind(filter.requester_email.map(|e| e.trim().to_owned()))
        .bind(filter.customer)
        .fetch_all(&state.db)
        .await?,
    ))
}

/// The tickets the add-on opened that are not closed
///
/// Every ticket the add-on opened that is not closed, with its ticket reference when it gave one,
/// most recent first and without a limit: what it needs to pick up its tickets again, such as
/// after a restart. Scope `tickets:read`.
#[utoipa::path(get, path = "/api/addon/tickets/opened", tag = "add-ons",
    responses((status = OK, body = Vec<AddonTicketSummary>), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn opened_tickets(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<Vec<AddonTicketSummary>>, ApiError> {
    caller.require(TICKETS_READ)?;
    Ok(Json(
        sqlx::query_as(select_summaries!(
            "WHERE t.opened_by_addon_id = $1 AND t.state <> 'closed'
             ORDER BY t.updated_at DESC, t.number DESC"
        ))
        .bind(caller.id)
        .fetch_all(&state.db)
        .await?,
    ))
}

/// A ticket as an add-on reads it: as agents do, without internal notes, announcements and the
/// customer's key requesters, and with the add-on's own ticket reference.
#[derive(Serialize, ToSchema)]
pub struct AddonTicket {
    #[serde(flatten)]
    detail: TicketDetail,
    /// Your ticket reference, when you opened the ticket with one; left out otherwise, and never
    /// given to other add-ons.
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
}

/// The ticket as `caller` reads it.
async fn seen_by(state: &AppState, caller: &Caller, number: i64) -> Result<AddonTicket, ApiError> {
    let mut detail = tickets::detail(&state.db, number).await?;
    // Internal notes are for agents only; an add-on never sees them. Nor the customer's key
    // requesters and where they are written to, which announcements name too: no add-on needs
    // them.
    detail.timeline.retain(|entry| {
        !matches!(
            entry,
            tickets::TimelineEntry::Note(_) | tickets::TimelineEntry::Announcement(_)
        )
    });
    detail.key_requesters = None;
    Ok(AddonTicket {
        detail,
        reference: addon_tickets::reference_of(&state.db, caller.id, number).await?,
    })
}

/// A ticket with its messages and links, without internal notes, announcements and its customer's
/// key requesters; `worked_on_by_agent` says whether an agent has worked on it all the same, and
/// `reference` is given to the add-on that opened it with a ticket reference. Scope
/// `tickets:read`.
#[utoipa::path(get, path = "/api/addon/tickets/{number}", tag = "add-ons",
    params(("number" = i64, Path, description = "The ticket's number")),
    responses((status = OK, body = AddonTicket), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn ticket(
    State(state): State<AppState>,
    caller: Caller,
    Path(number): Path<i64>,
) -> Result<Json<AddonTicket>, ApiError> {
    caller.require(TICKETS_READ)?;
    Ok(Json(seen_by(&state, &caller, number).await?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ByReference {
    /// The ticket reference the add-on gave the ticket when it opened it, compared exactly once
    /// trimmed.
    reference: String,
}

/// The ticket the add-on opened with a ticket reference
///
/// The ticket the add-on opened with this ticket reference, in any state, closed included, as
/// `GET /api/addon/tickets/{number}` gives it; not found for a ticket reference the add-on never
/// gave, another add-on's included. Scope `tickets:read`.
#[utoipa::path(get, path = "/api/addon/tickets/by-reference", tag = "add-ons",
    params(ByReference),
    responses((status = OK, body = AddonTicket), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn ticket_by_reference(
    State(state): State<AppState>,
    caller: Caller,
    Query(by): Query<ByReference>,
) -> Result<Json<AddonTicket>, ApiError> {
    caller.require(TICKETS_READ)?;
    let number = addon_tickets::by_reference(&state.db, caller.id, &by.reference).await?;
    Ok(Json(seen_by(&state, &caller, number).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct NewProposal {
    /// For triage: the inbox message.
    message_id: Option<Uuid>,
    /// For anything else: the ticket.
    ticket_number: Option<i64>,
    content: Content,
    /// Why the add-on suggests it: one sentence per UI language (`{"en": "...", "it": "..."}`,
    /// see `languages` in whoami), or a single English sentence.
    #[serde(default)]
    rationale: SentenceInput,
}

#[derive(Serialize, ToSchema)]
pub struct ProposalCreated {
    id: Uuid,
}

/// Proposes something to the agents; it replaces the open proposal of the same kind on the same
/// message or ticket. Scope `proposals:write`.
#[utoipa::path(post, path = "/api/addon/proposals", tag = "add-ons", request_body = NewProposal,
    responses((status = CREATED, body = ProposalCreated), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = BAD_REQUEST, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn propose(
    State(state): State<AppState>,
    caller: Caller,
    Json(new): Json<NewProposal>,
) -> Result<(StatusCode, Json<ProposalCreated>), ApiError> {
    caller.require(PROPOSALS_WRITE)?;
    let target = match (new.message_id, new.ticket_number) {
        (Some(message), None) => Target::Message(message),
        (None, Some(number)) => Target::Ticket(number),
        _ => {
            return Err(ApiError::BadRequest(
                "give exactly one of message_id and ticket_number".into(),
            ));
        }
    };
    let id = proposals::submit(
        &state.db,
        &caller.name,
        target,
        &new.content,
        &new.rationale.into_map(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ProposalCreated { id })))
}

/// The active services a customer uses. Scope `directory:read`.
#[utoipa::path(get, path = "/api/addon/customers/{id}/services", tag = "add-ons",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = OK, body = Vec<crate::catalog::Service>), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn customer_services(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<crate::catalog::Service>>, ApiError> {
    caller.require(DIRECTORY_READ)?;
    let services = crate::catalog::services_of(&state.db, id).await?;
    Ok(Json(
        services
            .into_iter()
            .filter(|s| s.retired_at.is_none())
            .collect(),
    ))
}

#[derive(Serialize, ToSchema)]
pub struct AddonTicketOpened {
    number: i64,
}

/// The operation's description, with the limit of a reference.
fn open_ticket_description() -> String {
    format!(
        "Opens a ticket without a requester for an active customer, with a priority and the \
         customer's active assets and services it is about; replies on it go to the customer's \
         key requesters. The description becomes the ticket's first internal note, by the \
         add-on. An asset or service that does not fit refuses the ticket, with a sentence naming \
         it; one that does not exist is not found. A `reference`, the add-on's ticket \
         reference such as the id of the problem the ticket is about, names the ticket for good: \
         at most {MAX_REFERENCE_LENGTH} characters once trimmed, neither blank nor with control \
         characters, compared exactly. Opening again with a ticket reference the add-on gave \
         before, also while that ticket is still being opened, opens nothing and answers 200 \
         with the number of the ticket opened then, which stays as it is, in any state, whatever \
         else the request says; another add-on's ticket references are its own. Scope \
         `tickets:write`."
    )
}

#[utoipa::path(post, path = "/api/addon/tickets", tag = "add-ons",
    request_body = addon_tickets::NewTicket,
    summary = "Opens a ticket without a requester",
    description = open_ticket_description(),
    responses((status = CREATED, body = AddonTicketOpened, description = "A new ticket"),
        (status = OK, body = AddonTicketOpened,
            description = "The number of the ticket the add-on opened before with the same \
                ticket reference, unchanged"),
        (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn open_ticket(
    State(state): State<AppState>,
    caller: Caller,
    Json(new): Json<addon_tickets::NewTicket>,
) -> Result<(StatusCode, Json<AddonTicketOpened>), ApiError> {
    caller.require(TICKETS_WRITE)?;
    let opened = addon_tickets::open(&state.db, caller.id, &new).await?;
    let status = if opened.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(AddonTicketOpened {
            number: opened.number,
        }),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct AddonNote {
    body: String,
}

/// Adds an internal note, seen only by agents, to any ticket, closed ones too; agents see the
/// add-on as its author. Scope `tickets:write`.
#[utoipa::path(post, path = "/api/addon/tickets/{number}/notes", tag = "add-ons",
    params(("number" = i64, Path, description = "The ticket's number")),
    request_body = AddonNote,
    responses((status = CREATED), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_note(
    State(state): State<AppState>,
    caller: Caller,
    Path(number): Path<i64>,
    Json(note): Json<AddonNote>,
) -> Result<StatusCode, ApiError> {
    caller.require(TICKETS_WRITE)?;
    addon_tickets::add_note(&state.db, caller.id, number, &note.body).await?;
    Ok(StatusCode::CREATED)
}

/// Resolves a ticket the add-on opened, open or waiting; `ticket.state_changed` names the add-on
/// as who changed it. A ticket already resolved stays as it is (204 again). A ticket an agent, a
/// message or another add-on opened is refused, and so is a closed one. Read
/// `worked_on_by_agent` on the ticket first: the core does not refuse a ticket an agent has
/// worked on, the add-on's rules decide. Scope `tickets:write`.
#[utoipa::path(post, path = "/api/addon/tickets/{number}/resolve", tag = "add-ons",
    params(("number" = i64, Path, description = "The ticket's number")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn resolve_ticket(
    State(state): State<AppState>,
    caller: Caller,
    Path(number): Path<i64>,
) -> Result<StatusCode, ApiError> {
    caller.require(TICKETS_WRITE)?;
    addon_tickets::resolve(&state.db, caller.id, number).await?;
    Ok(StatusCode::NO_CONTENT)
}
