//! The conflicts an add-on reports, with the scope `conflicts:write` (docs/domain-rules.md,
//! "Add-ons"): the add-on replaces the whole list, and administrators read the latest one in its
//! state.

use axum::{
    Json,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{ApiError, AppState, Problem};
use crate::addons::{CONFLICTS_WRITE, Caller};
use crate::conflicts::{self, MAX_CONFLICTS, MAX_LANGUAGES, MAX_SENTENCE_LENGTH, ReportedConflict};

/// The largest list an add-on sends, in bytes of JSON: room for the largest list the core takes,
/// [`MAX_CONFLICTS`] conflicts each in [`MAX_LANGUAGES`] languages of [`MAX_SENTENCE_LENGTH`]
/// characters, every character written as UTF-8 (at most 4 bytes; sentences hold no control
/// characters, which JSON escapes), with room to spare for the keys, rounded up to whole MiB. A
/// larger body is refused (413).
pub const BODY_LIMIT: usize = BODY_LIMIT_MIB * MIB;
const BODY_LIMIT_MIB: usize =
    (MAX_CONFLICTS * (MAX_LANGUAGES * (MAX_SENTENCE_LENGTH * 4 + 16) + 32) + 64 * 1024)
        .div_ceil(MIB);
const MIB: usize = 1024 * 1024;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(replace_conflicts))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// The whole list of the conflicts the add-on reports, as the add-on sends it.
#[derive(Deserialize, ToSchema)]
pub struct ReportedConflicts {
    /// The conflicts, in the order administrators read them; none when there are none.
    conflicts: Vec<ReportedConflict>,
}

/// The operation's description, with the limits the core keeps.
fn description() -> String {
    format!(
        "Replaces the list of conflicts the add-on reports, such as a host of its system in two \
         groups that two customers have as external references, which it therefore does not \
         import: administrators read the latest list in the add-on's state, also while the \
         add-on is switched off, and an empty list says there are none. Each conflict has a \
         sentence an administrator can act on, one per language of the agents' UI or a single \
         one taken as English, at most {MAX_SENTENCE_LENGTH} characters once trimmed in each of \
         at most {MAX_LANGUAGES} languages, without control characters. At most {MAX_CONFLICTS} \
         conflicts, in at most {} MiB of JSON (413 above), which the largest list fits written \
         as UTF-8. A list with a conflict that does not fit is refused whole, and the list \
         before stays. Scope `conflicts:write`.",
        BODY_LIMIT_MIB
    )
}

#[utoipa::path(put, path = "/api/addon/conflicts", tag = "add-ons", request_body = ReportedConflicts,
    summary = "Replaces the list of conflicts the add-on reports",
    description = description(),
    responses((status = NO_CONTENT), (status = CONFLICT, body = Problem),
        (status = PAYLOAD_TOO_LARGE, description = "A body over the limit"),
        (status = UNAUTHORIZED, body = Problem)))]
async fn replace_conflicts(
    State(state): State<AppState>,
    caller: Caller,
    Json(list): Json<ReportedConflicts>,
) -> Result<StatusCode, ApiError> {
    caller.require(CONFLICTS_WRITE)?;
    conflicts::replace(&state.db, &caller.name, list.conflicts).await?;
    Ok(StatusCode::NO_CONTENT)
}
