//! Administration: agents and how they sign in, roles, add-ons, and the state of the mail and of
//! the identity provider, each behind its permission (ADR 0007). Secrets never leave: no password
//! hashes, add-on tokens, mail credentials or client secrets.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{ApiError, AppState, Problem};
use crate::agents::{self, AgentAccount};
use crate::auth::SignedIn;
use crate::mail_status::Activity;
use crate::roles::{self, Permission, Role};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_agents, add_agent))
        .routes(routes!(edit_agent))
        .routes(routes!(set_role))
        .routes(routes!(reset_password, remove_password))
        .routes(routes!(unlink_identity))
        .routes(routes!(deactivate))
        .routes(routes!(reactivate))
        .routes(routes!(list_roles, create_role))
        .routes(routes!(update_role, remove_role))
        .routes(routes!(make_default))
        .routes(routes!(list_permissions))
        .routes(routes!(list_addons))
        .routes(routes!(set_addon_enabled))
        .routes(routes!(mail_state))
        .routes(routes!(identity_provider_state))
}

/// The issuer of the identity provider configured now, if any.
fn configured_issuer(state: &AppState) -> Option<&str> {
    state
        .identity_provider
        .as_ref()
        .map(|provider| provider.issuer())
}

/// Every agent, deactivated ones last.
#[utoipa::path(get, path = "/api/admin/agents", tag = "admin",
    responses((status = OK, body = Vec<AgentAccount>), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn list_agents(
    State(state): State<AppState>,
    agent: SignedIn,
) -> Result<Json<Vec<AgentAccount>>, ApiError> {
    agent.grant.require(Permission::ManageAgents)?;
    Ok(Json(agents::list(&state.db).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct NewAgent {
    email: String,
    name: String,
    /// The role; omitted, the default role.
    role_id: Option<Uuid>,
}

/// A password made by the core, shown once to the agent who asked for it.
#[derive(Serialize, ToSchema)]
pub struct GeneratedPassword {
    password: String,
}

#[derive(Serialize, ToSchema)]
pub struct AddedAgent {
    id: Uuid,
    /// The new agent's first password, generated; it is not shown again.
    password: String,
}

/// Adds an agent with a generated password.
#[utoipa::path(post, path = "/api/admin/agents", tag = "admin", request_body = NewAgent,
    responses((status = OK, body = AddedAgent), (status = CONFLICT, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn add_agent(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(new): Json<NewAgent>,
) -> Result<Json<AddedAgent>, ApiError> {
    let added = agents::add(
        &state.db,
        &agent.grant,
        &new.email,
        &new.name,
        new.role_id,
        None,
    )
    .await?;
    Ok(Json(AddedAgent {
        id: added.id,
        password: added.generated_password.unwrap_or_default(),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct AgentDetails {
    email: String,
    name: String,
}

/// Changes an agent's e-mail and name; refused for an agent linked to the identity provider,
/// which they come from.
#[utoipa::path(put, path = "/api/admin/agents/{id}", tag = "admin",
    params(("id" = Uuid, Path, description = "The agent")), request_body = AgentDetails,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn edit_agent(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(details): Json<AgentDetails>,
) -> Result<StatusCode, ApiError> {
    agents::edit(&state.db, &agent.grant, id, &details.email, &details.name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct RoleChange {
    role_id: Uuid,
}

/// Gives the agent another role.
#[utoipa::path(put, path = "/api/admin/agents/{id}/role", tag = "admin",
    params(("id" = Uuid, Path, description = "The agent")), request_body = RoleChange,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn set_role(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(change): Json<RoleChange>,
) -> Result<StatusCode, ApiError> {
    agents::set_role(&state.db, &agent.grant, id, change.role_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Gives the agent a new, generated password and ends their sessions.
#[utoipa::path(post, path = "/api/admin/agents/{id}/password", tag = "admin",
    params(("id" = Uuid, Path, description = "The agent")),
    responses((status = OK, body = GeneratedPassword), (status = NOT_FOUND, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn reset_password(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<GeneratedPassword>, ApiError> {
    let password = agents::reset_password(&state.db, &agent.grant, id, None).await?;
    Ok(Json(GeneratedPassword {
        password: password.unwrap_or_default(),
    }))
}

/// Removes the agent's local password and ends the sessions opened with it; refused unless they
/// have signed in through the identity provider configured now, and for one's own.
#[utoipa::path(delete, path = "/api/admin/agents/{id}/password", tag = "admin",
    params(("id" = Uuid, Path, description = "The agent")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn remove_password(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agents::remove_local_password(
        &state.db,
        &agent.grant,
        agent.id,
        id,
        configured_issuer(&state),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unlinks the agent's identity and ends the sessions opened through the identity provider; at
/// their next sign-in there, they are linked again by verified e-mail. Refused when they would be
/// left without a way to sign in, and for one's own.
#[utoipa::path(delete, path = "/api/admin/agents/{id}/identity", tag = "admin",
    params(("id" = Uuid, Path, description = "The agent")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn unlink_identity(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agents::unlink_identity(
        &state.db,
        &agent.grant,
        agent.id,
        id,
        configured_issuer(&state),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct Deactivated {
    /// Tickets that lost the agent as their assignee.
    unassigned: u64,
}

/// Deactivates an agent who left the help desk.
#[utoipa::path(post, path = "/api/admin/agents/{id}/deactivate", tag = "admin",
    params(("id" = Uuid, Path, description = "The agent")),
    responses((status = OK, body = Deactivated), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn deactivate(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<Json<Deactivated>, ApiError> {
    let unassigned = agents::deactivate(&state.db, &agent.grant, id).await?;
    Ok(Json(Deactivated { unassigned }))
}

/// Lets a deactivated agent sign in again.
#[utoipa::path(post, path = "/api/admin/agents/{id}/reactivate", tag = "admin",
    params(("id" = Uuid, Path, description = "The agent")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn reactivate(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    agents::reactivate(&state.db, &agent.grant, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every role, the Administrator first. Those who manage agents need it to give roles.
#[utoipa::path(get, path = "/api/admin/roles", tag = "admin",
    responses((status = OK, body = Vec<Role>), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn list_roles(
    State(state): State<AppState>,
    agent: SignedIn,
) -> Result<Json<Vec<Role>>, ApiError> {
    if !agent.grant.has(Permission::ManageAgents) {
        agent.grant.require(Permission::ManageRoles)?;
    }
    Ok(Json(roles::list(&state.db).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct RoleRequest {
    name: String,
    permissions: Vec<Permission>,
}

#[derive(Serialize, ToSchema)]
pub struct Created {
    id: Uuid,
}

/// Creates a role.
#[utoipa::path(post, path = "/api/admin/roles", tag = "admin", request_body = RoleRequest,
    responses((status = OK, body = Created), (status = CONFLICT, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn create_role(
    State(state): State<AppState>,
    agent: SignedIn,
    Json(request): Json<RoleRequest>,
) -> Result<Json<Created>, ApiError> {
    let id = roles::create(&state.db, &agent.grant, &request.name, &request.permissions).await?;
    Ok(Json(Created { id }))
}

/// Renames a role and replaces its permissions.
#[utoipa::path(put, path = "/api/admin/roles/{id}", tag = "admin",
    params(("id" = Uuid, Path, description = "The role")), request_body = RoleRequest,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn update_role(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
    Json(request): Json<RoleRequest>,
) -> Result<StatusCode, ApiError> {
    roles::update(
        &state.db,
        &agent.grant,
        id,
        &request.name,
        &request.permissions,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes a role no agent has.
#[utoipa::path(delete, path = "/api/admin/roles/{id}", tag = "admin",
    params(("id" = Uuid, Path, description = "The role")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn remove_role(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    roles::remove(&state.db, &agent.grant, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Makes the role the one new agents get when none is named.
#[utoipa::path(post, path = "/api/admin/roles/{id}/default", tag = "admin",
    params(("id" = Uuid, Path, description = "The role")),
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = CONFLICT, body = Problem), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn make_default(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    roles::make_default(&state.db, &agent.grant, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every permission a role can hold, in the product's order.
#[utoipa::path(get, path = "/api/admin/permissions", tag = "admin",
    responses((status = OK, body = Vec<Permission>), (status = UNAUTHORIZED, body = Problem)))]
async fn list_permissions(_agent: SignedIn) -> Json<Vec<Permission>> {
    Json(Permission::ALL.to_vec())
}

/// Every add-on with its deliveries; never its token or event secret.
#[utoipa::path(get, path = "/api/admin/addons", tag = "admin",
    responses((status = OK, body = Vec<crate::addons::AddonRow>), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn list_addons(
    State(state): State<AppState>,
    agent: SignedIn,
) -> Result<Json<Vec<crate::addons::AddonRow>>, ApiError> {
    agent.grant.require(Permission::ManageAddons)?;
    Ok(Json(crate::addons::list(&state.db).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct Switch {
    enabled: bool,
}

/// Switches an add-on on (its token works, its waiting events go out) or off.
#[utoipa::path(put, path = "/api/admin/addons/{name}/enabled", tag = "admin",
    params(("name" = String, Path, description = "The add-on's name")), request_body = Switch,
    responses((status = NO_CONTENT), (status = NOT_FOUND, body = Problem),
        (status = FORBIDDEN, body = Problem), (status = UNAUTHORIZED, body = Problem)))]
async fn set_addon_enabled(
    State(state): State<AppState>,
    agent: SignedIn,
    Path(name): Path<String>,
    Json(switch): Json<Switch>,
) -> Result<StatusCode, ApiError> {
    agent.grant.require(Permission::ManageAddons)?;
    crate::addons::set_enabled(&state.db, &name, switch.enabled).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A reply the outbox has not managed to send yet.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct FailingReply {
    ticket_number: Option<i64>,
    to_address: Option<String>,
    subject: String,
    attempts: i32,
    /// The outbox has stopped trying (crate::outbound::MAX_ATTEMPTS); the message stays unsent.
    given_up: bool,
    last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    next_attempt_at: Option<OffsetDateTime>,
}

#[derive(Serialize, ToSchema)]
pub struct OutboxState {
    /// Replies not sent yet.
    waiting: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    oldest_waiting_at: Option<OffsetDateTime>,
    /// Replies that failed at least once, most attempted first (at most 50).
    failing: Vec<FailingReply>,
}

#[derive(Serialize, ToSchema)]
pub struct MailState {
    /// Reading the mailbox; null when no mailbox is configured.
    reading: Option<Activity>,
    /// Sending replies; null when no server is configured, and replies wait in the outbox.
    sending: Option<Activity>,
    /// The address replies come from; null when agents cannot reply.
    from_address: Option<String>,
    outbox: OutboxState,
}

/// What reading and sending mail last did since the core started, and the outbox.
#[utoipa::path(get, path = "/api/admin/mail", tag = "admin",
    responses((status = OK, body = MailState), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn mail_state(
    State(state): State<AppState>,
    agent: SignedIn,
) -> Result<Json<MailState>, ApiError> {
    agent.grant.require(Permission::ViewMail)?;
    let (waiting, oldest_waiting_at): (i64, Option<OffsetDateTime>) = sqlx::query_as(
        "SELECT count(*), min(created_at) FROM messages
         WHERE direction = 'outbound' AND sent_at IS NULL",
    )
    .fetch_one(&state.db)
    .await?;
    let failing = sqlx::query_as(
        "SELECT t.number AS ticket_number, m.to_address, m.subject, m.send_attempts AS attempts,
                m.send_attempts >= $1 AS given_up, m.last_error, m.next_attempt_at
         FROM messages m LEFT JOIN tickets t ON t.id = m.ticket_id
         WHERE m.direction = 'outbound' AND m.sent_at IS NULL AND m.send_attempts > 0
         ORDER BY m.send_attempts DESC, m.created_at LIMIT 50",
    )
    .bind(crate::outbound::MAX_ATTEMPTS)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(MailState {
        reading: state.mail.reading.as_ref().map(|t| t.snapshot()),
        sending: state.mail.sending.as_ref().map(|t| t.snapshot()),
        from_address: state.sender.as_ref().map(|s| s.address.clone()),
        outbox: OutboxState {
            waiting,
            oldest_waiting_at,
            failing,
        },
    }))
}

#[derive(Serialize, ToSchema)]
pub struct IdentityProviderState {
    /// The OpenID Connect identity provider; null when none is configured.
    openid_connect: Option<ProviderState>,
    /// Whether agents can sign in with a local password.
    local_sign_in: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ProviderState {
    /// What the sign-in button names.
    label: String,
    /// Signing in through it since the core started: the issuer as the server, the last sign-in
    /// it confirmed as the last success, and the last failure in reaching it or in what it
    /// answered, without the client's secret.
    activity: Activity,
}

/// Whether an identity provider is configured and what signing in through it last did since the
/// core started, with whether local sign-in is on. Shown with the state of the mail; the settings
/// live in the server's configuration.
#[utoipa::path(get, path = "/api/admin/identity-provider", tag = "admin",
    responses((status = OK, body = IdentityProviderState), (status = FORBIDDEN, body = Problem),
        (status = UNAUTHORIZED, body = Problem)))]
async fn identity_provider_state(
    State(state): State<AppState>,
    agent: SignedIn,
) -> Result<Json<IdentityProviderState>, ApiError> {
    agent.grant.require(Permission::ViewMail)?;
    let openid_connect = state
        .identity_provider
        .as_ref()
        .map(|provider| ProviderState {
            label: provider.label().to_owned(),
            activity: state
                .identity_provider_activity
                .as_ref()
                .map(|activity| activity.snapshot())
                .unwrap_or_else(|| Activity {
                    server: provider.issuer().to_owned(),
                    ..Activity::default()
                }),
        });
    Ok(Json(IdentityProviderState {
        openid_connect,
        local_sign_in: crate::local_sign_in::is_on(&state.db).await?,
    }))
}
