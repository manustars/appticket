//! Managing customers, their mail domains, their sites, their external references and their
//! requesters with their addresses.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{ApiError, AppState, Problem};
use crate::auth::SignedIn;
use crate::customers::{self, CustomerDetail, CustomerDetails, CustomerSummary};
use crate::external_references;
use crate::key_requesters;
use crate::requesters;
use crate::roles::Permission;
use crate::sites;
use crate::tickets::Moved;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_all, create))
        .routes(routes!(get_customer, update_customer))
        .routes(routes!(add_domain))
        .routes(routes!(remove_domain))
        .routes(routes!(add_requester))
        .routes(routes!(get_requester, update_requester))
        .routes(routes!(depart))
        .routes(routes!(bring_back_requester))
        .routes(routes!(merge_requester))
        .routes(routes!(preview_requester_move))
        .routes(routes!(move_requester))
        .routes(routes!(mark_key_requester, unmark_key_requester))
        .routes(routes!(add_address))
        .routes(routes!(remove_address))
        .routes(routes!(add_site))
        .routes(routes!(update_site))
        .routes(routes!(retire_site))
        .routes(routes!(add_external_reference))
        .routes(routes!(remove_external_reference))
        .routes(routes!(external_systems))
        .routes(routes!(offboard))
        .routes(routes!(bring_back))
}

/// Every customer, offboarded ones included (last), with counts.
#[utoipa::path(get, path = "/api/customers/all", tag = "customers",
    responses((status = OK, body = Vec<CustomerSummary>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_all(
    State(state): State<AppState>,
    _agent: SignedIn,
) -> Result<Json<Vec<CustomerSummary>>, ApiError> {
    Ok(Json(customers::list(&state.db).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct NewCustomer {
    name: String,
    /// Mail domains of the customer's requesters, e.g. `brightfield.test`.
    #[serde(default)]
    domains: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedCustomer {
    id: Uuid,
}

/// Adds a customer.
#[utoipa::path(post, path = "/api/customers/all", tag = "customers", request_body = NewCustomer,
    responses((status = CREATED, body = CreatedCustomer), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn create(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(new): Json<NewCustomer>,
) -> Result<(StatusCode, Json<CreatedCustomer>), ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    let id = customers::create(&state.db, &new.name, &new.domains).await?;
    Ok((StatusCode::CREATED, Json(CreatedCustomer { id })))
}

/// A customer with its domains and requesters.
#[utoipa::path(get, path = "/api/customers/{id}", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = OK, body = CustomerDetail), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn get_customer(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<CustomerDetail>, ApiError> {
    Ok(Json(customers::detail(&state.db, id).await?))
}

/// Corrects a customer's name and details: a legal name, a tax identifier with its country, an
/// address, a phone number, a website and internal notes. A detail left out or blank is cleared.
#[utoipa::path(put, path = "/api/customers/{id}", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")), request_body = CustomerDetails,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn update_customer(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(details): Json<CustomerDetails>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    customers::update(&state.db, id, &details).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct NewDomain {
    domain: String,
}

/// Registers a mail domain: mail from it opens tickets for this customer.
#[utoipa::path(post, path = "/api/customers/{id}/domains", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")), request_body = NewDomain,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_domain(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(new): Json<NewDomain>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    customers::add_domain(&state.db, id, &new.domain).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unregisters a mail domain. Requesters already registered from it stay.
#[utoipa::path(delete, path = "/api/customers/{id}/domains/{domain}", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer"),
        ("domain" = String, Path, description = "The mail domain")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn remove_domain(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((id, domain)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    customers::remove_domain(&state.db, id, &domain).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct NewRequester {
    email: String,
    name: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedRequester {
    id: Uuid,
}

/// Registers a requester with the customer. An address of a requester who departed from another
/// customer passes to the new requester.
#[utoipa::path(post, path = "/api/customers/{id}/requesters", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")), request_body = NewRequester,
    responses((status = CREATED, body = CreatedRequester), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_requester(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(new): Json<NewRequester>,
) -> Result<(StatusCode, Json<CreatedRequester>), ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    let requester =
        customers::add_requester(&state.db, id, &new.email, new.name.as_deref()).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedRequester { id: requester }),
    ))
}

/// A requester's page: their customer, addresses and details. Their tickets are in the ticket
/// list, by `requester`.
#[utoipa::path(get, path = "/api/requesters/{id}", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")),
    responses((status = OK, body = requesters::RequesterDetail),
        (status = NOT_FOUND, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn get_requester(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<requesters::RequesterDetail>, ApiError> {
    Ok(Json(requesters::detail(&state.db, id).await?))
}

/// Corrects a requester's name, phone numbers, job title, internal notes and site.
#[utoipa::path(put, path = "/api/requesters/{id}", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")), request_body = requesters::RequesterDetails,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn update_requester(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(details): Json<requesters::RequesterDetails>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    requesters::update(&state.db, id, &details).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Marks a requester departed: they left their customer. Their history stays, no ticket is
/// opened for them and their messages wait in the inbox.
#[utoipa::path(post, path = "/api/requesters/{id}/depart", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn depart(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    requesters::depart(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Brings a departed requester back to their customer; they need an e-mail address of their own.
#[utoipa::path(post, path = "/api/requesters/{id}/bring-back", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn bring_back_requester(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    requesters::bring_back(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct MergedRequester {
    /// The requester merged into this one: their addresses and tickets pass to this one, and they
    /// are removed.
    requester_id: Uuid,
}

/// Merges another requester of the same customer, who is the same person, into this one, which
/// remains. A merge cannot be undone; requesters of different customers are never merged, nor
/// departed ones.
#[utoipa::path(post, path = "/api/requesters/{id}/merge", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester that remains")),
    request_body = MergedRequester,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn merge_requester(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(merged): Json<MergedRequester>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    requesters::merge(&state.db, id, merged.requester_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RequesterMoveTarget {
    /// The customer the requester is moved to.
    customer_id: Uuid,
}

/// What moving the requester to another customer would do: how many tickets move with them, and
/// the links of those tickets that do not fit the new customer and would be removed.
#[utoipa::path(get, path = "/api/requesters/{id}/move-preview", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester"), RequesterMoveTarget),
    responses((status = OK, body = requesters::MovePreview), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn preview_requester_move(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
    Query(target): Query<RequesterMoveTarget>,
) -> Result<Json<requesters::MovePreview>, ApiError> {
    Ok(Json(
        requesters::move_preview(&state.db, id, target.customer_id).await?,
    ))
}

/// Moves a requester identified with the wrong customer to the right one, under the rule for
/// moving tickets: all their tickets move with them, closed ones included, and linked assets and
/// services that do not fit the new customer are unlinked. They leave their site.
#[utoipa::path(post, path = "/api/requesters/{id}/move", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")), request_body = RequesterMoveTarget,
    responses((status = OK, body = Moved), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn move_requester(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(target): Json<RequesterMoveTarget>,
) -> Result<Json<Moved>, ApiError> {
    agent.grant.require(Permission::MoveTickets)?;
    Ok(Json(
        requesters::move_to_customer(&state.db, id, target.customer_id).await?,
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct KeyRequesterMark {
    /// One of the requester's addresses, where the messages for key requesters go. It can be
    /// left out for a requester with one address, and for a key requester, who keeps theirs.
    #[serde(default)]
    contact_address: Option<String>,
}

/// Makes the requester a key requester of their customer, written to at their contact address;
/// for a key requester, it changes the contact address. A departed requester cannot be one.
#[utoipa::path(put, path = "/api/requesters/{id}/key", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")), request_body = KeyRequesterMark,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn mark_key_requester(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(mark): Json<KeyRequesterMark>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    key_requesters::mark(&state.db, id, mark.contact_address.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Makes a key requester a plain requester of their customer again.
#[utoipa::path(delete, path = "/api/requesters/{id}/key", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn unmark_key_requester(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    key_requesters::unmark(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct NewAddress {
    address: String,
}

/// Gives a requester one more e-mail address; mail from it identifies them too. An address that
/// already belongs to a requester is refused.
#[utoipa::path(post, path = "/api/requesters/{id}/addresses", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester")), request_body = NewAddress,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_address(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(new): Json<NewAddress>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    requesters::add_address(&state.db, id, &new.address).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Takes an e-mail address from a requester, who keeps at least one.
#[utoipa::path(delete, path = "/api/requesters/{id}/addresses/{address}", tag = "customers",
    params(("id" = Uuid, Path, description = "The requester"),
        ("address" = String, Path, description = "The e-mail address")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn remove_address(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((id, address)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    requesters::remove_address(&state.db, id, &address).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct SiteFields {
    name: String,
    /// The site's postal address.
    address: String,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedSite {
    id: Uuid,
}

/// Adds a site to the customer; the customer's active sites have different names.
#[utoipa::path(post, path = "/api/customers/{id}/sites", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")), request_body = SiteFields,
    responses((status = CREATED, body = CreatedSite), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_site(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(new): Json<SiteFields>,
) -> Result<(StatusCode, Json<CreatedSite>), ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    let site = sites::create(&state.db, id, &new.name, &new.address).await?;
    Ok((StatusCode::CREATED, Json(CreatedSite { id: site })))
}

/// Corrects a site's name and address; a retired site too.
#[utoipa::path(put, path = "/api/sites/{id}", tag = "customers",
    params(("id" = Uuid, Path, description = "The site")), request_body = SiteFields,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn update_site(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(fields): Json<SiteFields>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    sites::update(&state.db, id, &fields.name, &fields.address).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Retires a site: it stays visible but gets no new requesters or assets. Sites are never
/// deleted.
#[utoipa::path(post, path = "/api/sites/{id}/retire", tag = "customers",
    params(("id" = Uuid, Path, description = "The site")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn retire_site(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    sites::retire(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct NewExternalReference {
    /// The other system, such as a monitoring tool.
    system: String,
    /// The name that system uses for the customer.
    name: String,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedExternalReference {
    id: Uuid,
}

/// Gives the customer an external reference: a name another system uses for it. A name names
/// exactly one customer within its system, ignoring case: one already in use is refused.
#[utoipa::path(post, path = "/api/customers/{id}/external-references", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")), request_body = NewExternalReference,
    responses((status = CREATED, body = CreatedExternalReference),
        (status = NOT_FOUND, body = Problem), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn add_external_reference(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(new): Json<NewExternalReference>,
) -> Result<(StatusCode, Json<CreatedExternalReference>), ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    let reference = external_references::add(&state.db, id, &new.system, &new.name).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedExternalReference { id: reference }),
    ))
}

/// The systems external references are in, across all customers: one spelling per system,
/// ignoring case, offered so that a system is spelled alike everywhere.
#[utoipa::path(get, path = "/api/external-references/systems", tag = "customers",
    responses((status = OK, body = Vec<String>), (status = UNAUTHORIZED, body = Problem)))]
async fn external_systems(
    State(state): State<AppState>,
    _agent: SignedIn,
) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(external_references::systems(&state.db).await?))
}

/// Takes an external reference from its customer; its name is free again in its system.
#[utoipa::path(delete, path = "/api/external-references/{id}", tag = "customers",
    params(("id" = Uuid, Path, description = "The external reference")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn remove_external_reference(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    external_references::remove(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Offboards a customer: no new tickets, its mail waits in the inbox, its history stays.
#[utoipa::path(post, path = "/api/customers/{id}/offboard", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn offboard(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    customers::offboard(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Brings an offboarded customer back, with its history, requesters and assets: its mail opens
/// tickets again. Messages that waited in the inbox meanwhile stay there to be sorted.
#[utoipa::path(post, path = "/api/customers/{id}/bring-back", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn bring_back(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    customers::bring_back(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
