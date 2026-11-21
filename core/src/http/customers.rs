//! Managing customers, their mail domains and their requesters.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{ApiError, AppState, Problem};
use crate::auth::SignedIn;
use crate::customers::{self, CustomerDetail, CustomerSummary};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_all, create))
        .routes(routes!(get_customer, rename))
        .routes(routes!(add_domain))
        .routes(routes!(remove_domain))
        .routes(routes!(add_requester))
        .routes(routes!(offboard))
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
    _agent: SignedIn,
    Json(new): Json<NewCustomer>,
) -> Result<(StatusCode, Json<CreatedCustomer>), ApiError> {
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

#[derive(Deserialize, ToSchema)]
pub struct Rename {
    name: String,
}

/// Renames a customer.
#[utoipa::path(put, path = "/api/customers/{id}", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")), request_body = Rename,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn rename(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(rename): Json<Rename>,
) -> Result<StatusCode, ApiError> {
    customers::rename(&state.db, id, &rename.name).await?;
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
    _agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(new): Json<NewDomain>,
) -> Result<StatusCode, ApiError> {
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
    _agent: SignedIn,
    Path((id, domain)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
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

/// Registers a requester with the customer.
#[utoipa::path(post, path = "/api/customers/{id}/requesters", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")), request_body = NewRequester,
    responses((status = CREATED, body = CreatedRequester), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_requester(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(new): Json<NewRequester>,
) -> Result<(StatusCode, Json<CreatedRequester>), ApiError> {
    let requester =
        customers::add_requester(&state.db, id, &new.email, new.name.as_deref()).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedRequester { id: requester }),
    ))
}

/// Offboards a customer: no new tickets, its mail waits in the inbox, its history stays.
#[utoipa::path(post, path = "/api/customers/{id}/offboard", tag = "customers",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn offboard(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    customers::offboard(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
