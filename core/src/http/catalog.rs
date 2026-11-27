//! Assets, services, and the links between them and tickets.

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
use crate::asset_kinds::{self, AssetKind};
use crate::asset_link_proposals::{self, AssetLinkProposal};
use crate::auth::SignedIn;
use crate::catalog::{self, Asset, AssetFields, Service, ServiceDetail};
use crate::live_details::{self, AssetLiveDetails};
use crate::roles::Permission;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_asset_kinds, create_asset_kind))
        .routes(routes!(update_asset_kind))
        .routes(routes!(customer_assets, create_asset))
        .routes(routes!(customer_asset_live_details))
        .routes(routes!(ticket_asset_live_details))
        .routes(routes!(customer_asset_link_proposals))
        .routes(routes!(accept_asset_link_proposal))
        .routes(routes!(discard_asset_link_proposal))
        .routes(routes!(customer_services))
        .routes(routes!(update_asset))
        .routes(routes!(retire_asset))
        .routes(routes!(list_services, create_service))
        .routes(routes!(get_service, update_service))
        .routes(routes!(retire_service))
        .routes(routes!(use_service, stop_using_service))
        .routes(routes!(runs_on, stops_running_on))
        .routes(routes!(link_ticket_asset, unlink_ticket_asset))
        .routes(routes!(link_ticket_service, unlink_ticket_service))
}

#[derive(Serialize, ToSchema)]
pub struct Created {
    id: Uuid,
}

/// Every asset kind the help desk manages, by name.
#[utoipa::path(get, path = "/api/asset-kinds", tag = "catalog",
    responses((status = OK, body = Vec<AssetKind>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_asset_kinds(
    State(state): State<AppState>,
    _agent: SignedIn,
) -> Result<Json<Vec<AssetKind>>, ApiError> {
    Ok(Json(asset_kinds::list(&state.db).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct AssetKindFields {
    name: String,
    /// Whether its assets move with their holder; otherwise they stay at their site.
    moves_with_holder: bool,
}

/// Adds an asset kind; no two kinds have names that differ only in case.
#[utoipa::path(post, path = "/api/asset-kinds", tag = "catalog", request_body = AssetKindFields,
    responses((status = CREATED, body = Created), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn create_asset_kind(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(fields): Json<AssetKindFields>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    let id = asset_kinds::create(&state.db, &fields.name, fields.moves_with_holder).await?;
    Ok((StatusCode::CREATED, Json(Created { id })))
}

/// Renames an asset kind or changes whether its assets move with their holder.
#[utoipa::path(put, path = "/api/asset-kinds/{id}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The asset kind")), request_body = AssetKindFields,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn update_asset_kind(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(fields): Json<AssetKindFields>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    asset_kinds::update(&state.db, id, &fields.name, fields.moves_with_holder).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The customer's assets, active ones first.
#[utoipa::path(get, path = "/api/customers/{id}/assets", tag = "catalog",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = OK, body = Vec<Asset>), (status = UNAUTHORIZED, body = Problem)))]
async fn customer_assets(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Asset>>, ApiError> {
    Ok(Json(catalog::assets_of(&state.db, id).await?))
}

/// Adds an asset the customer owns, with its serial number, site and holder if given.
#[utoipa::path(post, path = "/api/customers/{id}/assets", tag = "catalog",
    params(("id" = Uuid, Path, description = "The customer")), request_body = AssetFields,
    responses((status = CREATED, body = Created), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn create_asset(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(fields): Json<AssetFields>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    let id = catalog::create_asset(&state.db, id, &fields).await?;
    Ok((StatusCode::CREATED, Json(Created { id })))
}

/// The live details of the customer's active assets, from the add-on each comes from, asked
/// when this is read. The add-ons are asked all at once, a few assets at a time each, within one
/// 2-second deadline; an add-on that does not answer by then, answers anything but its live
/// details or is switched off has `details` null.
#[utoipa::path(get, path = "/api/customers/{id}/assets/live-details", tag = "catalog",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = OK, body = Vec<AssetLiveDetails>), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn customer_asset_live_details(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AssetLiveDetails>>, ApiError> {
    Ok(Json(
        live_details::of_customer(&state.db, &state.addon_client, id).await?,
    ))
}

/// The live details of the active assets the ticket links, as for a customer's assets.
#[utoipa::path(get, path = "/api/tickets/{number}/assets/live-details", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number")),
    responses((status = OK, body = Vec<AssetLiveDetails>), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn ticket_asset_live_details(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(number): Path<i64>,
) -> Result<Json<Vec<AssetLiveDetails>>, ApiError> {
    Ok(Json(
        live_details::of_ticket(&state.db, &state.addon_client, number).await?,
    ))
}

/// The open link proposals on the customer's assets, oldest first: an add-on would supply an asset
/// that has the same serial number or name as one of them.
#[utoipa::path(get, path = "/api/customers/{id}/asset-link-proposals", tag = "catalog",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = OK, body = Vec<AssetLinkProposal>),
        (status = UNAUTHORIZED, body = Problem)))]
async fn customer_asset_link_proposals(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AssetLinkProposal>>, ApiError> {
    Ok(Json(asset_link_proposals::open_of(&state.db, id).await?))
}

/// Accepts an open link proposal: the add-on is linked to the asset it matched, with its
/// reference, which from then on reaches that asset without changing anything of it. Refused when
/// the asset was retired meanwhile or the add-on is linked to it already by another reference.
/// Needs managing the catalog; an offboarded customer's proposals are not accepted.
#[utoipa::path(post, path = "/api/asset-link-proposals/{id}/accept", tag = "catalog",
    params(("id" = Uuid, Path, description = "The link proposal")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn accept_asset_link_proposal(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    asset_link_proposals::accept(&state.db, id, agent.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Discards an open link proposal: the add-on's next attempt with its reference creates its own
/// asset. Needs managing the catalog; an offboarded customer's proposals are not discarded.
#[utoipa::path(post, path = "/api/asset-link-proposals/{id}/discard", tag = "catalog",
    params(("id" = Uuid, Path, description = "The link proposal")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn discard_asset_link_proposal(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    asset_link_proposals::discard(&state.db, id, agent.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The services the customer uses.
#[utoipa::path(get, path = "/api/customers/{id}/services", tag = "catalog",
    params(("id" = Uuid, Path, description = "The customer")),
    responses((status = OK, body = Vec<Service>), (status = UNAUTHORIZED, body = Problem)))]
async fn customer_services(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Service>>, ApiError> {
    Ok(Json(catalog::services_of(&state.db, id).await?))
}

/// Changes an asset's kind, name, serial number, site and holder; those left out are cleared. The
/// site and the holder belong to the asset's customer; the site is not retired, the holder has not
/// departed, and a retired asset has no holder.
#[utoipa::path(put, path = "/api/assets/{id}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The asset")), request_body = AssetFields,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn update_asset(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(fields): Json<AssetFields>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::update_asset(&state.db, id, &fields).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Retires an asset: it stays on its tickets but gets no new links.
#[utoipa::path(post, path = "/api/assets/{id}/retire", tag = "catalog",
    params(("id" = Uuid, Path, description = "The asset")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn retire_asset(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::retire_asset(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every service the help desk provides, active ones first.
#[utoipa::path(get, path = "/api/services", tag = "catalog",
    responses((status = OK, body = Vec<Service>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_services(
    State(state): State<AppState>,
    _agent: SignedIn,
) -> Result<Json<Vec<Service>>, ApiError> {
    Ok(Json(catalog::services(&state.db).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct NewService {
    name: String,
    #[serde(default)]
    description: String,
}

/// Adds a service; the help desk's own organization uses it from the start.
#[utoipa::path(post, path = "/api/services", tag = "catalog", request_body = NewService,
    responses((status = CREATED, body = Created), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn create_service(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(new): Json<NewService>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    let id = catalog::create_service(&state.db, &new.name, &new.description).await?;
    Ok((StatusCode::CREATED, Json(Created { id })))
}

/// A service with the customers that use it and the assets it runs on.
#[utoipa::path(get, path = "/api/services/{id}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The service")),
    responses((status = OK, body = ServiceDetail), (status = NOT_FOUND, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn get_service(
    State(state): State<AppState>,
    _agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<ServiceDetail>, ApiError> {
    Ok(Json(catalog::service_detail(&state.db, id).await?))
}

/// The customer starts using the service.
#[utoipa::path(put, path = "/api/services/{id}/customers/{customer}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The service"),
        ("customer" = Uuid, Path, description = "The customer")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn use_service(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((id, customer)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::use_service(&state.db, id, customer).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The customer stops using the service (refused while its tickets refer to it).
#[utoipa::path(delete, path = "/api/services/{id}/customers/{customer}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The service"),
        ("customer" = Uuid, Path, description = "The customer")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn stop_using_service(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((id, customer)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::stop_using_service(&state.db, id, customer).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Records that the service runs on the asset.
#[utoipa::path(put, path = "/api/services/{id}/assets/{asset}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The service"),
        ("asset" = Uuid, Path, description = "The asset")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn runs_on(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((id, asset)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::runs_on(&state.db, id, asset, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Records that the service no longer runs on the asset.
#[utoipa::path(delete, path = "/api/services/{id}/assets/{asset}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The service"),
        ("asset" = Uuid, Path, description = "The asset")),
    responses((status = NO_CONTENT), (status = UNAUTHORIZED, body = Problem)))]
async fn stops_running_on(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((id, asset)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::runs_on(&state.db, id, asset, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Links an asset of the ticket's customer to the ticket.
#[utoipa::path(put, path = "/api/tickets/{number}/assets/{asset}", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number"),
        ("asset" = Uuid, Path, description = "The asset")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn link_ticket_asset(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((number, asset)): Path<(i64, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    catalog::link_asset(&state.db, number, asset, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unlinks an asset from the ticket.
#[utoipa::path(delete, path = "/api/tickets/{number}/assets/{asset}", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number"),
        ("asset" = Uuid, Path, description = "The asset")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn unlink_ticket_asset(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((number, asset)): Path<(i64, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    catalog::link_asset(&state.db, number, asset, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Links a service the ticket's customer uses to the ticket.
#[utoipa::path(put, path = "/api/tickets/{number}/services/{service}", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number"),
        ("service" = Uuid, Path, description = "The service")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn link_ticket_service(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((number, service)): Path<(i64, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    catalog::link_service(&state.db, number, service, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unlinks a service from the ticket.
#[utoipa::path(delete, path = "/api/tickets/{number}/services/{service}", tag = "tickets",
    params(("number" = i64, Path, description = "The ticket's number"),
        ("service" = Uuid, Path, description = "The service")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn unlink_ticket_service(
    State(state): State<AppState>,
    agent: SignedIn,
    Path((number, service)): Path<(i64, Uuid)>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::WorkTickets)?;
    catalog::link_service(&state.db, number, service, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Renames a service or changes its description.
#[utoipa::path(put, path = "/api/services/{id}", tag = "catalog",
    params(("id" = Uuid, Path, description = "The service")), request_body = NewService,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn update_service(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(fields): Json<NewService>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::update_service(&state.db, id, &fields.name, &fields.description).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Retires a service: it stays on its tickets but gets no new links or users.
#[utoipa::path(post, path = "/api/services/{id}/retire", tag = "catalog",
    params(("id" = Uuid, Path, description = "The service")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn retire_service(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageCatalog)?;
    catalog::retire_service(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
