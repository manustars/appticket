//! Tests of the HTTP API through the real router, signed in as an agent.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::{AppState, router};
use crate::testing;

/// A signed-in client of the API.
pub struct Client {
    state: AppState,
    cookie: String,
    bearer: Option<String>,
    pub agent: Uuid,
}

impl Client {
    /// Signed in as an Administrator.
    pub async fn new(db: &PgPool) -> Client {
        let admin: Uuid = sqlx::query_scalar("SELECT id FROM roles WHERE is_administrator")
            .fetch_one(db)
            .await
            .unwrap();
        Client::with_role(db, "marco@northgate.test", admin).await
    }

    /// Signed in as a new agent with `role`.
    pub async fn with_role(db: &PgPool, email: &str, role: Uuid) -> Client {
        let agent = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agents (id, email, name, password_hash, role_id) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(agent)
        .bind(email)
        .bind(email.split('@').next().unwrap())
        .bind(crate::auth::hash_password("pw").unwrap())
        .bind(role)
        .execute(db)
        .await
        .unwrap();
        let token = crate::auth::sign_in(db, email, "pw")
            .await
            .unwrap()
            .unwrap();
        Client {
            state: AppState::for_tests(db),
            cookie: format!("{}={token}", crate::auth::SESSION_COOKIE),
            bearer: None,
            agent,
        }
    }

    /// A client that authenticates with an add-on's bearer token instead of a session.
    pub fn new_with_token(db: &PgPool, token: &str, agent: Uuid) -> Client {
        Client {
            state: AppState::for_tests(db),
            cookie: String::new(),
            bearer: Some(token.to_owned()),
            agent,
        }
    }

    pub async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, &self.cookie);
        if let Some(token) = &self.bearer {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(json) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = router(self.state.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, json)
    }

    pub async fn get(&self, uri: &str) -> Value {
        let (status, json) = self.call("GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}: {json}");
        json
    }
}

fn numbers(list: &Value) -> Vec<i64> {
    let mut n: Vec<i64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["number"].as_i64().unwrap())
        .collect();
    n.sort();
    n
}

#[sqlx::test]
async fn tickets_can_be_filtered(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let a = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
    let b = testing::ticket(&db, brightfield, None, "b@brightfield.test").await;
    let c = testing::ticket(&db, harbor, None, "c@harbor.test").await;
    sqlx::query("UPDATE tickets SET state = 'closed' WHERE number = $1")
        .bind(b)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET assignee_id = $2 WHERE number = $1")
        .bind(c)
        .bind(api.agent)
        .execute(&db)
        .await
        .unwrap();

    assert_eq!(numbers(&api.get("/api/tickets").await), [a, b, c]);
    assert_eq!(
        numbers(&api.get("/api/tickets?state=open,waiting").await),
        [a, c]
    );
    assert_eq!(numbers(&api.get("/api/tickets?assignee=me").await), [c]);
    assert_eq!(
        numbers(&api.get("/api/tickets?assignee=none").await),
        [a, b]
    );
    assert_eq!(
        numbers(&api.get(&format!("/api/tickets?customer={harbor}")).await),
        [c]
    );
    assert_eq!(numbers(&api.get("/api/tickets?q=ADA%40bright").await), [a]);
    assert_eq!(numbers(&api.get("/api/tickets?q=harbor").await), [c]);
    assert_eq!(
        numbers(&api.get(&format!("/api/tickets?q=%23{b}")).await),
        [b]
    );
    assert_eq!(
        numbers(&api.get("/api/tickets?q=100%25").await),
        Vec::<i64>::new()
    );

    let (status, json) = api.call("GET", "/api/tickets?state=pending", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "Unknown ticket state \"pending\".");
}

#[sqlx::test]
async fn the_api_refuses_requests_without_a_session(db: PgPool) {
    let api = Client {
        cookie: String::new(),
        ..Client::new(&db).await
    };
    let (status, _) = api.call("GET", "/api/tickets", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn assets_and_services_are_linked_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let n = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;

    let (status, created) = api
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/assets"),
            Some(serde_json::json!({"kind": "computer", "name": "PC-0042"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let asset = created["id"].as_str().unwrap().to_owned();
    let (_, service) = api
        .call(
            "POST",
            "/api/services",
            Some(serde_json::json!({"name": "Hosted mail"})),
        )
        .await;
    let service = service["id"].as_str().unwrap().to_owned();

    let (status, refused) = api
        .call("PUT", &format!("/api/tickets/{n}/services/{service}"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        refused["error"],
        "The ticket's customer does not use this service."
    );
    for (method, uri) in [
        (
            "PUT",
            format!("/api/services/{service}/customers/{brightfield}"),
        ),
        ("PUT", format!("/api/tickets/{n}/services/{service}")),
        ("PUT", format!("/api/tickets/{n}/assets/{asset}")),
    ] {
        let (status, json) = api.call(method, &uri, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{method} {uri}: {json}");
    }
    let ticket = api.get(&format!("/api/tickets/{n}")).await;
    assert_eq!(ticket["assets"][0]["name"], "PC-0042");
    assert_eq!(ticket["services"][0]["name"], "Hosted mail");
}

#[sqlx::test]
async fn an_addon_calls_with_its_token_and_receives_messages(db: PgPool) {
    let api = Client::new(&db).await;
    let addon = crate::addons::register(
        &db,
        "assistant",
        "http://127.0.0.1:9",
        &[crate::addons::MESSAGE_RECEIVED.into()],
        &[],
    )
    .await
    .unwrap();
    let as_addon = Client {
        cookie: String::new(),
        ..Client::new_with_token(&db, &addon.token, api.agent)
    };
    let (status, me) = as_addon.call("GET", "/api/addon/whoami", None).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "assistant");
    // An agent's session is not an add-on's token.
    let (status, _) = api.call("GET", "/api/addon/whoami", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A routed message is recorded as an event for the add-on.
    let raw = b"From: Ada <ada@elsewhere.test>\r\nMessage-ID: <e1@elsewhere.test>\r\nSubject: Printer\r\n\r\nIt jams.\r\n";
    let mut tx = db.begin().await.unwrap();
    crate::mail::store(&mut tx, 1, raw).await.unwrap();
    tx.commit().await.unwrap();
    let (kind, subject, pending): (String, String, i64) = sqlx::query_as(
        "SELECT e.kind, e.data->>'subject',
                (SELECT count(*) FROM event_deliveries d WHERE d.event_id = e.id)
         FROM events e",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), subject.as_str(), pending),
        ("message.received", "Printer", 1)
    );
}

#[sqlx::test]
async fn an_addon_proposes_and_an_agent_accepts(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let message = testing::inbox_message(&db, "ada@brightfield.test").await;
    let limited = crate::addons::register(
        &db,
        "reader",
        "http://127.0.0.1:9",
        &[],
        &["tickets:read".into()],
    )
    .await
    .unwrap();
    let assistant = crate::addons::register(
        &db,
        "assistant",
        "http://127.0.0.1:9",
        &[],
        &["proposals:write".into(), "directory:read".into()],
    )
    .await
    .unwrap();
    let proposal = serde_json::json!({
        "message_id": message,
        "content": {"kind": "triage", "action": "open_ticket", "customer_id": brightfield},
        "rationale": "The sender writes from Brightfield's domain."
    });

    let reader = Client::new_with_token(&db, &limited.token, api.agent);
    let (status, refused) = reader
        .call("POST", "/api/addon/proposals", Some(proposal.clone()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        refused["error"],
        "The add-on reader lacks the scope proposals:write."
    );

    let bot = Client::new_with_token(&db, &assistant.token, api.agent);
    let customers = bot.get("/api/addon/customers").await;
    let n = testing::ticket(&db, brightfield, None, "n@brightfield.test").await;
    crate::tickets::add_note(&db, n, api.agent, "Internal: the customer is late paying.")
        .await
        .unwrap();
    let (_, refused) = bot
        .call("GET", &format!("/api/addon/tickets/{n}"), None)
        .await;
    assert_eq!(
        refused["error"],
        "The add-on assistant lacks the scope tickets:read."
    );
    let full = crate::addons::register(
        &db,
        "full",
        "http://127.0.0.1:9",
        &[],
        &["tickets:read".into()],
    )
    .await
    .unwrap();
    let seen = Client::new_with_token(&db, &full.token, api.agent)
        .get(&format!("/api/addon/tickets/{n}"))
        .await;
    assert!(
        seen["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["kind"] != "note"),
        "notes never reach add-ons: {seen}"
    );
    assert_eq!(customers[0]["name"], "Brightfield");
    let (status, created) = bot
        .call("POST", "/api/addon/proposals", Some(proposal))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let inbox = api.get("/api/inbox").await;
    let shown = &inbox[0]["proposal"];
    assert_eq!(shown["customer_name"], "Brightfield");
    assert_eq!(shown["addon"], "assistant");
    let id = shown["id"].as_str().unwrap();
    let (status, accepted) = api
        .call("POST", &format!("/api/proposals/{id}/accept"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    let number = accepted["ticket_number"].as_i64().unwrap();
    let ticket = api.get(&format!("/api/tickets/{number}")).await;
    assert_eq!(ticket["customer"]["name"], "Brightfield");
    assert_eq!(ticket["priority"], "normal");
    assert!(api.get("/api/inbox").await.as_array().unwrap().is_empty());
}

/// A request body that the extractors accept, built from the OpenAPI schema: every property
/// filled with a plain value of its type.
fn sample(spec: &Value, schema: &Value) -> Value {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap();
        return sample(spec, &spec["components"]["schemas"][name]);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values[0].clone();
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(first) = schema.get(key).and_then(|list| list.get(0)) {
            return sample(spec, first);
        }
    }
    let kind = match &schema["type"] {
        Value::String(kind) => kind.clone(),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null")
            .unwrap_or("null")
            .to_owned(),
        _ => "object".to_owned(),
    };
    match kind.as_str() {
        "object" => Value::Object(
            schema["properties"]
                .as_object()
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(name, property)| (name.clone(), sample(spec, property)))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "array" => Value::Array(Vec::new()),
        "string" if schema["format"] == "uuid" => Value::String(Uuid::nil().to_string()),
        "string" => Value::String("someone@example.com".into()),
        "integer" | "number" => Value::from(1),
        "boolean" => Value::Bool(true),
        _ => Value::Null,
    }
}

async fn role_with(db: &PgPool, name: &str, permissions: &[crate::roles::Permission]) -> Uuid {
    crate::roles::create(db, &crate::roles::Grant::command_line(), name, permissions)
        .await
        .unwrap()
}

/// Every request that changes something needs a permission: an agent whose role has none is
/// refused all of them. A new route without a check fails here.
#[sqlx::test]
async fn an_agent_without_permissions_changes_nothing(db: PgPool) {
    let reader = role_with(&db, "Reader", &[]).await;
    let api = Client::with_role(&db, "rita@northgate.test", reader).await;
    let customer = testing::customer(&db, "Brightfield").await;
    let number = testing::ticket(&db, customer, None, "<one@brightfield.test>").await;
    let addon = crate::addons::register(&db, "assistant", "http://127.0.0.1:1", &[], &[])
        .await
        .unwrap();
    let proposal = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO proposals (id, addon_id, ticket_id, kind, content)
         SELECT $1, $2, id, 'priority', '{\"kind\": \"priority\", \"priority\": \"high\"}'
         FROM tickets WHERE number = $3",
    )
    .bind(proposal)
    .bind(addon.id)
    .bind(number)
    .execute(&db)
    .await
    .unwrap();

    let spec = serde_json::to_value(super::openapi()).unwrap();
    let mut checked = 0;
    for (path, operations) in spec["paths"].as_object().unwrap() {
        // Signing in and out and one's own password are for everyone; add-ons use tokens.
        if path.starts_with("/api/session") || path.starts_with("/api/addon/") {
            continue;
        }
        for (method, operation) in operations.as_object().unwrap() {
            if method == "get" {
                continue;
            }
            let uri = path
                .replace("{number}", &number.to_string())
                .replace("{name}", "assistant")
                .replace("{domain}", "brightfield.test")
                .replace("{address}", "ada%40brightfield.test")
                .replace(
                    "{id}",
                    &if path.starts_with("/api/proposals/") {
                        proposal
                    } else {
                        Uuid::new_v4()
                    }
                    .to_string(),
                )
                .replace("{asset}", &Uuid::new_v4().to_string())
                .replace("{service}", &Uuid::new_v4().to_string())
                .replace("{customer}", &Uuid::new_v4().to_string());
            let body = operation
                .pointer("/requestBody/content/application~1json/schema")
                .map(|schema| sample(&spec, schema));
            let (status, json) = api.call(&method.to_uppercase(), &uri, body).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{} {uri} answered {status}: {json}",
                method.to_uppercase()
            );
            checked += 1;
        }
    }
    assert!(checked > 40, "only {checked} routes checked");

    for uri in [
        "/api/admin/agents",
        "/api/admin/roles",
        "/api/admin/addons",
        "/api/admin/mail",
    ] {
        let (status, _) = api.call("GET", uri, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "GET {uri}");
    }
    // Reading stays open to every agent.
    api.get("/api/tickets").await;
    api.get(&format!("/api/tickets/{number}")).await;
    api.get("/api/inbox").await;
    let me = api.get("/api/session").await;
    assert_eq!(me["role_name"], "Reader");
    assert_eq!(me["permissions"], serde_json::json!([]));
}

#[sqlx::test]
async fn a_role_allows_what_it_holds_and_changes_apply_at_once(db: PgPool) {
    use crate::roles::Permission;
    let sorter = role_with(&db, "Sorter", &[Permission::SortInbox]).await;
    let api = Client::with_role(&db, "sara@northgate.test", sorter).await;
    let message = testing::inbox_message(&db, "someone@elsewhere.test").await;
    let customer = testing::customer(&db, "Brightfield").await;
    let number = testing::ticket(&db, customer, None, "<two@brightfield.test>").await;

    let (status, _) = api
        .call("POST", &format!("/api/inbox/{message}/discard"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let note = serde_json::json!({"body": "Called back."});
    let (status, json) = api
        .call(
            "POST",
            &format!("/api/tickets/{number}/notes"),
            Some(note.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        json["error"],
        "Your role does not allow working on tickets."
    );

    crate::roles::update(
        &db,
        &crate::roles::Grant::command_line(),
        sorter,
        "Sorter",
        &[Permission::SortInbox, Permission::WorkTickets],
    )
    .await
    .unwrap();
    let (status, _) = api
        .call("POST", &format!("/api/tickets/{number}/notes"), Some(note))
        .await;
    assert!(status.is_success(), "{status}");

    // Taking a ticket is working on it; giving it to someone else is assigning.
    let me = serde_json::json!({"agent_id": api.agent});
    let (status, _) = api
        .call("PUT", &format!("/api/tickets/{number}/assignee"), Some(me))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let admin = Client::new(&db).await;
    let other = serde_json::json!({"agent_id": admin.agent});
    let (status, _) = api
        .call(
            "PUT",
            &format!("/api/tickets/{number}/assignee"),
            Some(other),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn administrators_manage_agents_through_the_api(db: PgPool) {
    let admin = Client::new(&db).await;
    let roles = admin.get("/api/admin/roles").await;
    let agent_role = roles
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["default"] == true)
        .unwrap()["id"]
        .clone();
    let (status, added) = admin
        .call(
            "POST",
            "/api/admin/agents",
            Some(serde_json::json!({"email": "Nina@Northgate.test", "name": "Nina", "role_id": agent_role})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    let password = added["password"].as_str().unwrap().to_owned();
    assert!(
        crate::auth::sign_in(&db, "nina@northgate.test", &password)
            .await
            .unwrap()
            .is_some()
    );
    let id = added["id"].as_str().unwrap();
    let (status, _) = admin
        .call("POST", &format!("/api/admin/agents/{id}/deactivate"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let agents = admin.get("/api/admin/agents").await;
    assert!(
        agents
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["email"] == "nina@northgate.test" && a["deactivated_at"].is_string())
    );
    assert!(
        !admin
            .get("/api/agents")
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["email"] == "nina@northgate.test")
    );
    // The last Administrator cannot step down.
    let (status, json) = admin
        .call(
            "PUT",
            &format!("/api/admin/agents/{}/role", admin.agent),
            Some(serde_json::json!({"role_id": agent_role})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    let mail = admin.get("/api/admin/mail").await;
    assert!(mail["reading"].is_null() && mail["outbox"]["waiting"] == 0);
}

/// A requester with two addresses: either finds their tickets, the reply goes where they wrote
/// from, and the agent picks on a ticket they have not written on.
#[sqlx::test]
async fn a_requester_with_several_addresses_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let (status, json) = api
        .call(
            "POST",
            &format!("/api/requesters/{ada}/addresses"),
            Some(serde_json::json!({ "address": "Ada@Home.test" })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let customer = api.get(&format!("/api/customers/{brightfield}")).await;
    assert_eq!(
        customer["requester_list"][0]["addresses"],
        serde_json::json!(["ada@brightfield.test", "ada@home.test"])
    );

    let wrote = testing::ticket(&db, brightfield, Some(ada), "w@home.test").await;
    testing::message_from(&db, wrote, "ada@home.test").await;
    let silent = testing::ticket(&db, brightfield, Some(ada), "s@brightfield.test").await;

    let ticket = api.get(&format!("/api/tickets/{wrote}")).await;
    assert_eq!(ticket["requester"]["reply_to"], "ada@home.test");
    assert_eq!(ticket["requester"]["email"], "ada@home.test");
    let ticket = api.get(&format!("/api/tickets/{silent}")).await;
    assert!(ticket["requester"]["reply_to"].is_null());
    assert_eq!(ticket["requester"]["email"], "ada@brightfield.test");
    assert_eq!(
        numbers(&api.get("/api/tickets?q=home.test").await),
        [wrote, silent]
    );

    let reply = |n: i64, to: Option<&'static str>| {
        let api = &api;
        async move {
            api.call(
                "POST",
                &format!("/api/tickets/{n}/replies"),
                Some(serde_json::json!({ "body": "Restart it.", "to": to })),
            )
            .await
        }
    };
    let (status, _) = reply(wrote, Some("ada@brightfield.test")).await;
    assert_eq!(status, StatusCode::CONFLICT, "not where she wrote from");
    let (status, _) = reply(silent, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "the agent has to pick");
    assert_eq!(reply(wrote, None).await.0, StatusCode::ACCEPTED);
    assert_eq!(
        reply(silent, Some("ada@home.test")).await.0,
        StatusCode::ACCEPTED
    );
    let ticket = api.get(&format!("/api/tickets/{silent}")).await;
    assert_eq!(ticket["requester"]["last_picked"], "ada@home.test");
    assert!(ticket["requester"]["reply_to"].is_null());
    assert_eq!(
        ticket["requester"]["email"], "ada@home.test",
        "the ticket shows where its replies go"
    );
    let listed = api.get(&format!("/api/tickets?q=%23{silent}")).await;
    assert_eq!(listed[0]["requester_email"], "ada@home.test");
    let sent: Vec<(i64, String)> = sqlx::query_as(
        "SELECT t.number, m.to_address FROM messages m JOIN tickets t ON t.id = m.ticket_id
         WHERE m.direction = 'outbound' ORDER BY t.number",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(
        sent,
        [
            (wrote, "ada@home.test".to_owned()),
            (silent, "ada@home.test".to_owned())
        ]
    );

    // Add-ons find the requester's tickets by any of the addresses.
    let reader = crate::addons::register(
        &db,
        "reader",
        "http://127.0.0.1:9",
        &[],
        &["tickets:read".into()],
    )
    .await
    .unwrap();
    let bot = Client::new_with_token(&db, &reader.token, api.agent);
    for address in ["ada@brightfield.test", "ADA@home.test"] {
        let found = bot
            .get(&format!("/api/addon/tickets?requester_email={address}"))
            .await;
        assert_eq!(numbers(&found), [wrote, silent], "{address}");
    }
    let seen = bot.get(&format!("/api/addon/tickets/{wrote}")).await;
    assert_eq!(
        seen["requester"]["addresses"],
        serde_json::json!(["ada@brightfield.test", "ada@home.test"])
    );

    let (status, _) = api
        .call(
            "DELETE",
            &format!("/api/requesters/{ada}/addresses/ada%40brightfield.test"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, json) = api
        .call(
            "DELETE",
            &format!("/api/requesters/{ada}/addresses/ada%40home.test"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
}

/// Sites are kept on the customer: added, corrected and retired, never deleted.
#[sqlx::test]
async fn sites_are_managed_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let (status, created) = api
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/sites"),
            Some(serde_json::json!({ "name": "Head office", "address": "Via dei Tigli 1, Valfiorita" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let site = created["id"].as_str().unwrap().to_owned();

    let (status, json) = api
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/sites"),
            Some(serde_json::json!({ "name": "head office", "address": "Corso dei Mulini 9" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");

    let (status, _) = api
        .call(
            "PUT",
            &format!("/api/sites/{site}"),
            Some(
                serde_json::json!({ "name": "Head office", "address": "Via dei Tigli 1A, Valfiorita" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = api
        .call("POST", &format!("/api/sites/{site}/retire"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let customer = api.get(&format!("/api/customers/{brightfield}")).await;
    let sites = customer["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0]["address"], "Via dei Tigli 1A, Valfiorita");
    assert!(sites[0]["retired_at"].is_string());
    let (status, _) = api
        .call("DELETE", &format!("/api/sites/{site}"), None)
        .await;
    assert_eq!(
        status,
        StatusCode::METHOD_NOT_ALLOWED,
        "sites are never deleted"
    );
}
