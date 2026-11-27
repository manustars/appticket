//! The assets an add-on supplies, with the scope `catalog:write` (docs/domain-rules.md, "Add-ons"):
//! it creates, updates and retires those it is the source of, and nothing else; linked to an asset
//! by an accepted link proposal, it reaches it by its reference and unlinks itself.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::addon::AddonAsset;
use super::{ApiError, AppState, Problem};
use crate::addons::{CATALOG_WRITE, Caller};
use crate::asset_link_proposals::AssetLinkProposal;
use crate::supplied_assets::{self, Supplied, SuppliedDetails};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(supply_asset))
        .routes(routes!(update_supplied_asset))
        .routes(routes!(retire_supplied_asset))
        .routes(routes!(unlink_asset))
}

/// An asset as its source supplies it, known by the add-on's own reference.
#[derive(Deserialize, ToSchema)]
pub struct SuppliedAsset {
    /// The add-on's own identifier for the asset, such as a host's id in its system: the same
    /// reference always names the same asset. Compared exactly, once trimmed.
    reference: String,
    /// The customer that owns the asset; an asset never changes customer.
    customer_id: Uuid,
    #[serde(flatten)]
    details: SuppliedDetails,
    #[serde(flatten)]
    #[schema(ignore)]
    holder: HolderGiven,
}

/// What an add-on sends to update an asset it is the source of.
#[derive(Deserialize, ToSchema)]
pub struct SuppliedUpdate {
    #[serde(flatten)]
    details: SuppliedDetails,
    #[serde(flatten)]
    #[schema(ignore)]
    holder: HolderGiven,
}

/// A holder an add-on tried to give, which is refused rather than silently dropped.
#[derive(Deserialize, Default)]
struct HolderGiven {
    #[serde(default)]
    holder_id: Option<Value>,
    #[serde(default)]
    holder: Option<Value>,
}

impl HolderGiven {
    fn refuse(&self) -> Result<(), ApiError> {
        if self.holder_id.is_some() || self.holder.is_some() {
            return Err(ApiError::Conflict(
                "an add-on does not give an asset a holder: agents choose the requester it is \
                 given to"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// The asset as the add-on reads it, once written.
async fn written(
    state: &AppState,
    caller: &Caller,
    id: Uuid,
) -> Result<Json<AddonAsset>, ApiError> {
    let asset = crate::catalog::asset(&state.db, id).await?;
    Ok(Json(AddonAsset::seen_by(asset, caller)))
}

/// Creates an asset of a customer known by the add-on's reference, or updates the one the add-on
/// already supplied with that reference (bringing it back if it retired it), so that the add-on
/// can send its whole inventory again safely: 201 when created, 200 when updated. Its kind and its
/// site are named; what is left out is cleared, and its holder, whom agents choose, stays.
///
/// When the add-on would create the asset and its customer already has an active one with the same
/// serial number, else the same name, both ignoring case, that the add-on is not the source of,
/// nothing is created: the answer is 202 with the link proposal recorded for the agents, which
/// says which asset matched. Sending the asset again while the proposal is open answers the same
/// proposal, with the name and serial number just sent, even when the asset it matched was renamed
/// or retired since; once agents discard it, the next attempt creates the asset.
///
/// Once agents accept it, the add-on is linked to that asset and the reference reaches it: sending
/// what the asset has answers it with 200 and changes nothing, while any change to its kind, name,
/// serial number or site is refused, since only its source changes them. Scope `catalog:write`.
#[utoipa::path(post, path = "/api/addon/assets", tag = "add-ons", request_body = SuppliedAsset,
    responses((status = CREATED, body = AddonAsset), (status = OK, body = AddonAsset),
        (status = ACCEPTED, body = AssetLinkProposal,
            description = "A link proposal instead of the asset"),
        (status = NOT_FOUND, body = Problem), (status = CONFLICT, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn supply_asset(
    State(state): State<AppState>,
    caller: Caller,
    Json(asset): Json<SuppliedAsset>,
) -> Result<Response, ApiError> {
    caller.require(CATALOG_WRITE)?;
    asset.holder.refuse()?;
    let supplied = supplied_assets::supply(
        &state.db,
        &caller.name,
        asset.customer_id,
        &asset.reference,
        &asset.details,
    )
    .await?;
    Ok(match supplied {
        Supplied::Asset { asset, created } => {
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (status, Json(AddonAsset::seen_by(asset, &caller))).into_response()
        }
        Supplied::LinkProposed(proposal) => (StatusCode::ACCEPTED, Json(proposal)).into_response(),
    })
}

/// Updates an asset the add-on is the source of, as supplying it again does, but refuses a retired
/// one, which only supplying it again with its reference brings back; an asset entered by
/// hand or supplied by another add-on is refused. For an asset the add-on is linked to, sending
/// what it has changes nothing, and any change is refused. Scope `catalog:write`.
#[utoipa::path(put, path = "/api/addon/assets/{id}", tag = "add-ons",
    params(("id" = Uuid, Path, description = "The asset")), request_body = SuppliedUpdate,
    responses((status = OK, body = AddonAsset), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn update_supplied_asset(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Json(update): Json<SuppliedUpdate>,
) -> Result<Json<AddonAsset>, ApiError> {
    caller.require(CATALOG_WRITE)?;
    update.holder.refuse()?;
    supplied_assets::update(&state.db, &caller.name, id, &update.details).await?;
    written(&state, &caller, id).await
}

/// Retires an asset the add-on is the source of, when it leaves the add-on's system: it stays on
/// the tickets that refer to it, gets no new links and loses its holder. Retiring it again changes
/// nothing; an asset entered by hand or supplied by another add-on is refused, and an add-on linked
/// to an asset unlinks itself instead. Scope `catalog:write`.
#[utoipa::path(post, path = "/api/addon/assets/{id}/retire", tag = "add-ons",
    params(("id" = Uuid, Path, description = "The asset")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn retire_supplied_asset(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    caller.require(CATALOG_WRITE)?;
    supplied_assets::retire(&state.db, &caller.name, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unlinks the add-on from an asset it is linked to, when the asset leaves the add-on's system:
/// the asset stays as it was, retired or not, and the add-on's reference no longer reaches it.
/// Unlinking again changes nothing; the asset's source is refused, which retires it instead, as is
/// an asset of an offboarded customer. Scope `catalog:write`.
#[utoipa::path(post, path = "/api/addon/assets/{id}/unlink", tag = "add-ons",
    params(("id" = Uuid, Path, description = "The asset")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn unlink_asset(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    caller.require(CATALOG_WRITE)?;
    crate::linked_addons::unlink(&state.db, &caller.name, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
