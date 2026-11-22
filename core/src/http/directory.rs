//! Lists the UI needs to fill its choices: agents to assign, customers to open tickets for.

use axum::{Json, extract::State};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{ApiError, AppState, Problem};
use crate::auth::SignedIn;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_agents))
        .routes(routes!(list_customers))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct AgentEntry {
    id: Uuid,
    name: String,
    email: String,
}

/// Agents who are not deactivated, by name: those a ticket can be assigned to.
#[utoipa::path(get, path = "/api/agents", tag = "directory",
    responses((status = OK, body = Vec<AgentEntry>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_agents(
    State(state): State<AppState>,
    _agent: SignedIn,
) -> Result<Json<Vec<AgentEntry>>, ApiError> {
    Ok(Json(
        sqlx::query_as(
            "SELECT id, name, email FROM agents WHERE deactivated_at IS NULL ORDER BY name",
        )
        .fetch_all(&state.db)
        .await?,
    ))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct CustomerEntry {
    id: Uuid,
    name: String,
    /// Whether this is the help desk's own organization.
    is_help_desk: bool,
}

/// Customers that can get new tickets (offboarded ones are left out), by name.
#[utoipa::path(get, path = "/api/customers", tag = "directory",
    responses((status = OK, body = Vec<CustomerEntry>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_customers(
    State(state): State<AppState>,
    _agent: SignedIn,
) -> Result<Json<Vec<CustomerEntry>>, ApiError> {
    Ok(Json(
        sqlx::query_as(
            "SELECT id, name, is_help_desk FROM customers
             WHERE offboarded_at IS NULL ORDER BY name",
        )
        .fetch_all(&state.db)
        .await?,
    ))
}
