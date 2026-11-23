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
use crate::addons::{self, Caller, DIRECTORY_READ, PROPOSALS_WRITE, TICKETS_READ};
use crate::catalog::Asset;
use crate::external_references::{self, NameInSystem};
use crate::proposals::{self, Content, Target};
use crate::requesters::SiteRef;
use crate::tickets::{self, Priority, TicketDetail, TicketState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(whoami))
        .routes(routes!(customers))
        .routes(routes!(customer_assets))
        .routes(routes!(customer_services))
        .routes(routes!(open_tickets))
        .routes(routes!(ticket))
        .routes(routes!(propose))
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

/// A rationale as an add-on sends it.
#[derive(Deserialize, ToSchema, Default)]
#[serde(untagged)]
pub enum RationaleInput {
    /// One sentence per language.
    PerLanguage(proposals::Rationale),
    /// A single sentence, taken as English.
    English(String),
    #[default]
    #[serde(skip)]
    None,
}

impl RationaleInput {
    fn into_map(self) -> proposals::Rationale {
        match self {
            RationaleInput::PerLanguage(map) => map,
            RationaleInput::English(text) => proposals::Rationale::from([("en".to_owned(), text)]),
            RationaleInput::None => proposals::Rationale::new(),
        }
    }
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
    #[serde(with = "time::serde::rfc3339::option")]
    retired_at: Option<OffsetDateTime>,
}

impl From<Asset> for AddonAsset {
    fn from(a: Asset) -> AddonAsset {
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
            .map(AddonAsset::from)
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
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
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
        sqlx::query_as(
            "SELECT t.number, t.title, t.state, t.priority, t.customer_id,
                    ticket_requester_address(t.id) AS requester_email,
                    t.updated_at
             FROM tickets t
             WHERE t.state <> 'closed'
               AND ($1::text IS NULL OR t.requester_id =
                    (SELECT requester_id FROM requester_addresses WHERE address = lower($1)))
               AND ($2::uuid IS NULL OR t.customer_id = $2)
             ORDER BY t.updated_at DESC LIMIT 50",
        )
        .bind(filter.requester_email.map(|e| e.trim().to_owned()))
        .bind(filter.customer)
        .fetch_all(&state.db)
        .await?,
    ))
}

/// A ticket with its messages and links, without internal notes, announcements and its customer's
/// key requesters. Scope `tickets:read`.
#[utoipa::path(get, path = "/api/addon/tickets/{number}", tag = "add-ons",
    params(("number" = i64, Path, description = "The ticket's number")),
    responses((status = OK, body = TicketDetail), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn ticket(
    State(state): State<AppState>,
    caller: Caller,
    Path(number): Path<i64>,
) -> Result<Json<TicketDetail>, ApiError> {
    caller.require(TICKETS_READ)?;
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
    Ok(Json(detail))
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
    rationale: RationaleInput,
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
