//! The part of the API add-ons call, with `Authorization: Bearer <token>`.

use axum::{
    Json,
    extract::FromRequestParts,
    http::{header, request::Parts},
};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{ApiError, AppState, Problem};
use crate::addons::{self, Caller};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(whoami))
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
}

/// The add-on the token belongs to, and what it may do.
#[utoipa::path(get, path = "/api/addon/whoami", tag = "add-ons",
    responses((status = OK, body = AddonIdentity), (status = UNAUTHORIZED, body = Problem)))]
async fn whoami(caller: Caller) -> Json<AddonIdentity> {
    Json(AddonIdentity {
        name: caller.name,
        scopes: caller.scopes,
    })
}
