//! Importing customers and sites from CSV files: a preview first, then the import itself.

use axum::{Json, extract::DefaultBodyLimit, extract::State};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{ApiError, AppState, Problem};
use crate::auth::SignedIn;
use crate::import::{self, Kind, Mode, Report};
use crate::roles::Permission;

/// The largest request an import takes: the JSON body, all its files together.
pub const BODY_LIMIT: usize = 10 * 1024 * 1024;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(preview))
        .routes(routes!(apply))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// CSV files to import, at most one of each kind; they are taken customers first, then sites.
#[derive(Deserialize, ToSchema)]
pub struct ImportRequest {
    files: Vec<ImportFile>,
}

#[derive(Deserialize, ToSchema)]
pub struct ImportFile {
    kind: Kind,
    /// The file's text: CSV with a header of fixed column names, comma or semicolon separated.
    content: String,
}

/// Previews or applies the request's files for the agent.
async fn import(
    state: &AppState,
    agent: &SignedIn,
    request: ImportRequest,
    mode: Mode,
) -> Result<Json<Report>, ApiError> {
    agent.grant.require(Permission::ManageCustomers)?;
    let files = request
        .files
        .into_iter()
        .map(|file| import::File {
            kind: file.kind,
            content: file.content.into_bytes(),
        })
        .collect();
    Ok(Json(
        import::run(&state.db, &agent.grant, files, mode).await?,
    ))
}

/// What importing the files would create, update and leave out, with the reason of each row
/// left out; nothing changes.
#[utoipa::path(post, path = "/api/imports/preview", tag = "customers", request_body = ImportRequest,
    responses((status = OK, body = Report), (status = CONFLICT, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = PAYLOAD_TOO_LARGE),
        (status = UNAUTHORIZED, body = Problem)))]
async fn preview(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(request): Json<ImportRequest>,
) -> Result<Json<Report>, ApiError> {
    import(&state, &agent, request, Mode::Preview).await
}

/// Imports the files: the rows that break no rule are created or update what exists, checked
/// again against the help desk's data as it is now; the rows left out come back as a CSV file,
/// with their reason, to correct and import again. A file with an unknown or missing column
/// stops the whole import.
#[utoipa::path(post, path = "/api/imports", tag = "customers", request_body = ImportRequest,
    responses((status = OK, body = Report), (status = CONFLICT, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = PAYLOAD_TOO_LARGE),
        (status = UNAUTHORIZED, body = Problem)))]
async fn apply(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(request): Json<ImportRequest>,
) -> Result<Json<Report>, ApiError> {
    import(&state, &agent, request, Mode::Apply).await
}
