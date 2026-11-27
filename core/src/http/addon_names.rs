//! The name list of an add-on's system, with the scope `names:write` (docs/domain-rules.md,
//! "Add-ons"): the add-on replaces it, and agents who manage customers pick external references
//! from the latest one.

use axum::{
    Json,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{ApiError, AppState, Problem};
use crate::addons::{Caller, NAMES_WRITE};
use crate::name_lists::{self, MAX_NAME_LENGTH, MAX_NAMES, MAX_SYSTEM_LENGTH, SystemName};

/// The largest list an add-on sends, in bytes of JSON: room for the largest list the core takes,
/// [`MAX_NAMES`] names of [`MAX_NAME_LENGTH`] characters each nested in another as long, every
/// character written as UTF-8 (at most 4 bytes; names hold no control characters, which JSON
/// escapes), with room to spare for the keys and the system, rounded up to whole MiB. A larger
/// body is refused (413).
pub const BODY_LIMIT: usize = BODY_LIMIT_MIB * MIB;
const BODY_LIMIT_MIB: usize =
    (MAX_NAMES * (2 * MAX_NAME_LENGTH * 4 + 64) + 64 * 1024).div_ceil(MIB);
const MIB: usize = 1024 * 1024;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(replace_names))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// The whole name list of the add-on's system, as the add-on sends it.
#[derive(Deserialize, ToSchema)]
pub struct SystemNames {
    /// The add-on's system, such as a monitoring tool: the external references picked from the
    /// list are in it. A system's names come from one add-on, compared ignoring case.
    system: String,
    /// The names, in any order.
    names: Vec<SystemName>,
}

/// The operation's description, with the limits the core keeps.
fn description() -> String {
    format!(
        "Replaces the name list of the add-on's system that agents who manage customers pick \
         external references from, such as its host groups, with their nesting: the core keeps \
         the latest list, also while the add-on is switched off. At most {MAX_NAMES} names, each \
         at most {MAX_NAME_LENGTH} characters once trimmed, the system's name at most \
         {MAX_SYSTEM_LENGTH}, neither blank nor with control characters, in at most {} MiB of \
         JSON (413 above), which the largest list fits written as UTF-8. A list with a name \
         twice (ignoring case), nested in a name it does not list or in itself, or under a \
         system another add-on lists, is refused whole. The customers' external references \
         stay as they are, also those whose name leaves the list. Scope `names:write`.",
        BODY_LIMIT_MIB
    )
}

#[utoipa::path(put, path = "/api/addon/names", tag = "add-ons", request_body = SystemNames,
    summary = "Replaces the name list of the add-on's system",
    description = description(),
    responses((status = NO_CONTENT), (status = CONFLICT, body = Problem),
        (status = PAYLOAD_TOO_LARGE, description = "A body over the limit"),
        (status = UNAUTHORIZED, body = Problem)))]
async fn replace_names(
    State(state): State<AppState>,
    caller: Caller,
    Json(list): Json<SystemNames>,
) -> Result<StatusCode, ApiError> {
    caller.require(NAMES_WRITE)?;
    name_lists::replace(&state.db, &caller.name, &list.system, &list.names).await?;
    Ok(StatusCode::NO_CONTENT)
}
