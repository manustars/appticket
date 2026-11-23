//! Tests of the HTTP API through the real router, signed in as an agent.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
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

    /// A client nobody signed in with, of the core in `state`.
    pub fn anonymous(state: AppState) -> Client {
        Client {
            state,
            cookie: String::new(),
            bearer: None,
            agent: Uuid::nil(),
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
            .header(header::COOKIE, &self.cookie)
            .extension(connection_from("192.0.2.1"));
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

/// What the server records of a connection from `address`.
fn connection_from(address: &str) -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::new(address.parse().unwrap(), 50000))
}

/// What a sign-in through the API answers: status, body and `Retry-After`.
struct SignInAnswer {
    status: StatusCode,
    body: Value,
    retry_after: Option<String>,
}

/// Signs in through the API on a connection from `from` that carries the `X-Forwarded-For`
/// lines `forwarded`.
async fn sign_in_from(
    state: &AppState,
    from: &str,
    forwarded: &[&str],
    email: &str,
    password: &str,
) -> SignInAnswer {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/session")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(connection_from(from));
    for line in forwarded {
        request = request.header("x-forwarded-for", *line);
    }
    let body = serde_json::json!({"email": email, "password": password}).to_string();
    let response = router(state.clone())
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    SignInAnswer {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        retry_after,
    }
}

/// The answer to an attempt refused for too many failures: the same for every account, known
/// or not, so that it does not tell which e-mails are agents'.
fn assert_refused(answer: &SignInAnswer) {
    assert_eq!(
        answer.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        answer.body
    );
    assert_eq!(
        answer.body,
        serde_json::json!({"error": "Too many failed sign-ins: try again later."})
    );
    assert!(answer.retry_after.is_some());
}

#[sqlx::test]
async fn repeated_failed_sign_ins_are_refused_for_known_and_unknown_accounts_alike(db: PgPool) {
    let api = Client::new(&db).await;
    let state = &api.state;
    for _ in 0..9 {
        let answer = sign_in_from(state, "203.0.113.7", &[], "marco@northgate.test", "guess").await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    }
    // The four attempts past the limit waited, without the test waiting.
    assert_eq!(
        state.sign_in_limits.clock().sleeps(),
        [1, 2, 4, 8].map(std::time::Duration::from_secs)
    );
    let right = sign_in_from(state, "198.51.100.4", &[], "Marco@Northgate.test", "pw").await;
    assert_refused(&right);
    assert_eq!(right.retry_after.as_deref(), Some("892"));
    // From the guessing address, any account is refused.
    assert_refused(&sign_in_from(state, "203.0.113.7", &[], "nobody@northgate.test", "pw").await);

    // An e-mail that is no agent's is counted the same way, and no agent is made for it.
    for n in 1..=9 {
        let from = format!("198.51.100.{n}");
        sign_in_from(state, &from, &[], "ghost@northgate.test", "guess").await;
    }
    assert_refused(&sign_in_from(state, "192.0.2.77", &[], "ghost@northgate.test", "pw").await);
    let agents: i64 = sqlx::query_scalar("SELECT count(*) FROM agents WHERE email LIKE 'ghost%'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(agents, 0);

    // A window after the last failure, the right password signs in again.
    state
        .sign_in_limits
        .clock()
        .advance(std::time::Duration::from_secs(15 * 60));
    let again = sign_in_from(state, "198.51.100.4", &[], "marco@northgate.test", "pw").await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
}

/// With no trusted proxy, `X-Forwarded-For` is the client's own writing: a client cannot pick
/// the address it is counted under.
#[sqlx::test]
async fn x_forwarded_for_is_ignored_from_a_connection_that_is_no_trusted_proxy(db: PgPool) {
    let api = Client::new(&db).await;
    let state = AppState {
        trusted_proxies: super::parse_proxies("192.0.2.10").unwrap(),
        ..api.state.clone()
    };
    for n in 1..=9 {
        let forwarded = format!("203.0.113.{n}");
        let email = format!("guess{n}@northgate.test");
        sign_in_from(&state, "198.51.100.4", &[&forwarded], &email, "guess").await;
    }
    assert_refused(
        &sign_in_from(
            &state,
            "198.51.100.4",
            &["203.0.113.99"],
            "marco@northgate.test",
            "pw",
        )
        .await,
    );
}

#[sqlx::test]
async fn behind_a_trusted_proxy_clients_are_counted_under_their_forwarded_addresses(db: PgPool) {
    let api = Client::new(&db).await;
    let state = AppState {
        trusted_proxies: super::parse_proxies("192.0.2.0/24").unwrap(),
        ..api.state.clone()
    };
    for n in 1..=9 {
        let email = format!("guess{n}@northgate.test");
        sign_in_from(&state, "192.0.2.10", &["203.0.113.7"], &email, "guess").await;
    }
    // The leftmost entry is whatever the client wrote: it does not get the client out.
    let spoofed = "203.0.113.8, 203.0.113.7";
    assert_refused(
        &sign_in_from(
            &state,
            "192.0.2.10",
            &[spoofed],
            "marco@northgate.test",
            "pw",
        )
        .await,
    );
    let other = sign_in_from(
        &state,
        "192.0.2.10",
        &["203.0.113.8"],
        "marco@northgate.test",
        "pw",
    )
    .await;
    assert_eq!(other.status, StatusCode::NO_CONTENT);
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

/// Tickets are listed with the sites they are at and filtered by one, retired sites included: a
/// closed ticket stays at the site it was closed at.
#[sqlx::test]
async fn tickets_are_listed_with_their_sites_and_filtered_by_site(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
        .await
        .unwrap();
    let lab = crate::sites::create(&db, brightfield, "Lab", "Via del Glicine 3")
        .await
        .unwrap();
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let at_head = crate::requesters::RequesterDetails {
        site_id: Some(head),
        ..Default::default()
    };
    crate::requesters::update(&db, ada, &at_head).await.unwrap();
    let printer = testing::asset_kind(&db, "Printer").await;
    let printer_2f = crate::catalog::create_asset(
        &db,
        brightfield,
        &testing::asset_details(printer, "Printer 2F", None, Some(lab), None),
    )
    .await
    .unwrap();
    let hers = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
    let printers = testing::ticket(&db, brightfield, None, "b@brightfield.test").await;
    crate::catalog::link_asset(&db, printers, printer_2f, true)
        .await
        .unwrap();
    let nowhere = testing::ticket(&db, brightfield, None, "c@brightfield.test").await;
    let closed = testing::ticket(&db, brightfield, Some(ada), "d@brightfield.test").await;
    crate::tickets::set_state(&db, closed, crate::tickets::TicketState::Closed)
        .await
        .unwrap();
    crate::sites::retire(&db, head).await.unwrap();

    let list = api.get("/api/tickets").await;
    let sites_of = |n: i64| -> Vec<String> {
        let ticket = list
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["number"] == n)
            .unwrap();
        ticket["sites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        sites_of(hers),
        Vec::<String>::new(),
        "she left the retired site"
    );
    assert_eq!(sites_of(printers), ["Lab"]);
    assert_eq!(sites_of(nowhere), Vec::<String>::new());
    assert_eq!(sites_of(closed), ["Head office"]);

    assert_eq!(
        numbers(&api.get(&format!("/api/tickets?site={lab}")).await),
        [printers]
    );
    assert_eq!(
        numbers(&api.get(&format!("/api/tickets?site={head}")).await),
        [closed],
        "a retired site"
    );
    let (status, _) = api.call("GET", "/api/tickets?site=lab", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
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
    let computer = testing::asset_kind(&db, "Computer").await;

    let (status, created) = api
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/assets"),
            Some(serde_json::json!({"kind_id": computer, "name": "PC-0042"})),
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
    assert_eq!(ticket["assets"][0]["kind"], "Computer");
    assert_eq!(ticket["services"][0]["name"], "Hosted mail");
}

/// Agents who manage the catalog set and clear an asset's serial number, site and holder; the
/// customer's assets, and the requester's page, show where each asset is and who holds it.
#[sqlx::test]
async fn an_asset_s_details_are_set_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let cataloguer = role_with(&db, "Cataloguer", &[Permission::ManageCatalog]).await;
    let api = Client::with_role(&db, "carla@northgate.test", cataloguer).await;
    let manager = role_with(&db, "Manager", &[Permission::ManageCustomers]).await;
    let manager = Client::with_role(&db, "mara@northgate.test", manager).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
        .await
        .unwrap();
    let lab = crate::sites::create(&db, brightfield, "Lab", "Via del Glicine 3")
        .await
        .unwrap();
    let quay = crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
        .await
        .unwrap();
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let at_lab = crate::requesters::RequesterDetails {
        name: Some("Ada Ferrini".into()),
        site_id: Some(lab),
        ..Default::default()
    };
    crate::requesters::update(&db, ada, &at_lab).await.unwrap();
    let laptop = crate::asset_kinds::create(&db, "Laptop", true)
        .await
        .unwrap();

    let assets = format!("/api/customers/{brightfield}/assets");
    let (status, created) = api
        .call(
            "POST",
            &assets,
            Some(serde_json::json!({
                "kind_id": laptop, "name": "PC-0042", "serial_number": "SN-0042",
                "site_id": head, "holder_id": ada
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let pc = created["id"].as_str().unwrap().to_owned();
    let listed = api.get(&assets).await;
    let asset = &listed[0];
    assert_eq!(asset["serial_number"], "SN-0042");
    assert_eq!(asset["moves_with_holder"], true);
    assert_eq!(asset["site"]["name"], "Head office");
    assert_eq!(
        asset["holder"],
        serde_json::json!({"id": ada, "name": "Ada Ferrini", "address": "ada@brightfield.test"})
    );
    assert_eq!(asset["at_site"]["name"], "Lab", "with its holder");
    let page = api.get(&format!("/api/requesters/{ada}")).await;
    assert_eq!(page["held_assets"][0]["name"], "PC-0042");
    let preview = api
        .get(&format!(
            "/api/requesters/{ada}/move-preview?customer_id={harbor}"
        ))
        .await;
    assert_eq!(preview["held_assets"][0]["name"], "PC-0042");

    let asset = format!("/api/assets/{pc}");
    let elsewhere = serde_json::json!({"kind_id": laptop, "name": "PC-0042", "site_id": quay});
    let (status, json) = api.call("PUT", &asset, Some(elsewhere)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "The site is not one of the asset's customer."
    );
    let cleared = serde_json::json!({"kind_id": laptop, "name": "PC-0042"});
    let (status, json) = manager.call("PUT", &asset, Some(cleared.clone())).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the holder is part of the catalog: {json}"
    );
    let (status, json) = api.call("PUT", &asset, Some(cleared)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let listed = api.get(&assets).await;
    for field in ["serial_number", "site", "holder", "at_site"] {
        assert_eq!(listed[0][field], Value::Null, "{field} cleared");
    }
}

/// Add-ons that read a customer's assets get their serial number and own site, which an add-on
/// can be the source of, but are not told who holds them, nor where their holder is.
#[sqlx::test]
async fn add_ons_read_an_asset_s_serial_number_and_site_but_not_its_holder(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
        .await
        .unwrap();
    let lab = crate::sites::create(&db, brightfield, "Lab", "Via del Glicine 3")
        .await
        .unwrap();
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let at_lab = crate::requesters::RequesterDetails {
        site_id: Some(lab),
        ..Default::default()
    };
    crate::requesters::update(&db, ada, &at_lab).await.unwrap();
    let laptop = crate::asset_kinds::create(&db, "Laptop", true)
        .await
        .unwrap();
    let fields = crate::catalog::AssetFields {
        serial_number: Some("SN-0042".into()),
        site_id: Some(head),
        holder_id: Some(ada),
        ..testing::asset_fields(laptop, "PC-0042")
    };
    crate::catalog::create_asset(&db, brightfield, &fields)
        .await
        .unwrap();
    let reader = crate::addons::register(
        &db,
        "reader",
        "http://127.0.0.1:9",
        &[],
        &[crate::addons::DIRECTORY_READ.into()],
    )
    .await
    .unwrap();
    let bot = Client::new_with_token(&db, &reader.token, api.agent);

    let seen = bot
        .get(&format!("/api/addon/customers/{brightfield}/assets"))
        .await;
    assert_eq!(seen[0]["name"], "PC-0042");
    assert_eq!(seen[0]["serial_number"], "SN-0042");
    assert_eq!(
        seen[0]["site"],
        serde_json::json!({"id": head, "name": "Head office"})
    );
    assert!(seen[0].get("holder").is_none(), "{seen}");
    for hidden in [ada.to_string(), lab.to_string(), "Lab\"".into()] {
        assert!(!seen.to_string().contains(&hidden), "{hidden} in {seen}");
    }
}

#[sqlx::test]
async fn asset_kinds_are_managed_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;

    let (status, created) = api
        .call(
            "POST",
            "/api/asset-kinds",
            Some(serde_json::json!({"name": "Laptop", "moves_with_holder": true})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let laptop = created["id"].as_str().unwrap().to_owned();
    let (status, json) = api
        .call(
            "POST",
            "/api/asset-kinds",
            Some(serde_json::json!({"name": "laptop", "moves_with_holder": false})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "There is already an asset kind called Laptop."
    );
    let (status, json) = api
        .call(
            "PUT",
            &format!("/api/asset-kinds/{laptop}"),
            Some(serde_json::json!({"name": "Notebook", "moves_with_holder": true})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    assert_eq!(
        api.get("/api/asset-kinds").await,
        serde_json::json!([{"id": laptop, "name": "Notebook", "moves_with_holder": true}])
    );

    let (status, json) = api
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/assets"),
            Some(serde_json::json!({"kind_id": Uuid::new_v4(), "name": "PC-0042"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{json}");
    let (status, json) = api
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/assets"),
            Some(serde_json::json!({"kind_id": laptop, "name": "PC-0042"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{json}");

    // Add-ons keep reading an asset's kind by its name.
    let addon = crate::addons::register(
        &db,
        "assistant",
        "http://127.0.0.1:9",
        &[],
        &[crate::addons::DIRECTORY_READ.into()],
    )
    .await
    .unwrap();
    let as_addon = Client {
        cookie: String::new(),
        ..Client::new_with_token(&db, &addon.token, api.agent)
    };
    let (status, assets) = as_addon
        .call(
            "GET",
            &format!("/api/addon/customers/{brightfield}/assets"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{assets}");
    assert_eq!(assets[0]["kind"], "Notebook");
    assert_eq!(assets[0]["name"], "PC-0042");
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
    api.get("/api/asset-kinds").await;
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

/// An agent's name and e-mail are edited, unless the identity provider they are linked to
/// supplies them.
#[sqlx::test]
async fn a_linked_agents_name_and_email_are_not_edited_through_the_api(db: PgPool) {
    let admin = Client::new(&db).await;
    let cli = crate::roles::Grant::command_line();
    let bo = crate::agents::add(&db, &cli, "bo@example.com", "Bo", None, None)
        .await
        .unwrap()
        .id;
    let (status, json) = admin
        .call(
            "PUT",
            &format!("/api/admin/agents/{bo}"),
            Some(serde_json::json!({"email": "bo.lund@example.com", "name": "Bo Lund"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let agents = admin.get("/api/admin/agents").await;
    let listed = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == bo.to_string());
    assert_eq!(listed.unwrap()["name"], "Bo Lund");

    sqlx::query(
        "INSERT INTO agent_identities (issuer, subject, agent_id)
         VALUES ('https://login.example.com/realms/staff', 'sub-bo', $1)",
    )
    .bind(bo)
    .execute(&db)
    .await
    .unwrap();
    let (status, json) = admin
        .call(
            "PUT",
            &format!("/api/admin/agents/{bo}"),
            Some(serde_json::json!({"email": "bo@example.com", "name": "Bo"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "This agent signs in through the identity provider, which their name and e-mail come from."
    );
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

/// A requester's page: details read and corrected, their site, and their tickets.
#[sqlx::test]
async fn a_requester_s_page_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
    let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
        .await
        .unwrap();
    let mine = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;
    testing::ticket(&db, brightfield, Some(lena), "l@brightfield.test").await;

    let (status, json) = api
        .call(
            "PUT",
            &format!("/api/requesters/{ada}"),
            Some(serde_json::json!({
                "name": "Ada Ferrini",
                "phone_numbers": ["000 000 0101"],
                "job_title": "Office manager",
                "notes": "Prefers phone calls.",
                "site_id": head,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let page = api.get(&format!("/api/requesters/{ada}")).await;
    assert_eq!(page["name"], "Ada Ferrini");
    assert_eq!(page["phone_numbers"], serde_json::json!(["000 000 0101"]));
    assert_eq!(page["site"]["name"], "Head office");
    assert_eq!(page["customer"]["name"], "Brightfield");
    assert_eq!(
        page["addresses"],
        serde_json::json!(["ada@brightfield.test"])
    );

    let tickets = api.get(&format!("/api/tickets?requester={ada}")).await;
    assert_eq!(numbers(&tickets), [mine]);
    let customer = api.get(&format!("/api/customers/{brightfield}")).await;
    let entry = customer["requester_list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == ada.to_string())
        .unwrap()
        .clone();
    assert_eq!(entry["site_id"], head.to_string());

    // Internal notes and the other details stay with the agents: add-ons never see them.
    let reader = crate::addons::register(
        &db,
        "reader",
        "http://127.0.0.1:9",
        &[],
        &["tickets:read".into(), "directory:read".into()],
    )
    .await
    .unwrap();
    let bot = Client::new_with_token(&db, &reader.token, api.agent);
    let seen = bot
        .get(&format!("/api/addon/tickets/{mine}"))
        .await
        .to_string();
    for private in [
        "Prefers phone calls",
        "000 000 0101",
        "Office manager",
        "Head office",
    ] {
        assert!(
            !seen.contains(private),
            "an add-on sees {private:?}: {seen}"
        );
    }

    let (status, _) = api
        .call("GET", &format!("/api/requesters/{}", Uuid::new_v4()), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A customer's details corrected and cleared on its page, and an offboarded customer that comes
/// back and is offered again for new tickets.
#[sqlx::test]
async fn a_customer_s_page_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let page = format!("/api/customers/{brightfield}");

    let (status, json) = api
        .call(
            "PUT",
            &page,
            Some(serde_json::json!({
                "name": "Brightfield",
                "legal_name": "Brightfield Labs S.r.l.",
                "tax_id": "00000000000",
                "tax_country": "it",
                "address": "Via dei Tigli 1\nValfiorita",
                "phone": "000 000 0100",
                "website": "https://brightfield.test",
                "notes": "Billing goes through Lena.",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let shown = api.get(&page).await;
    assert_eq!(shown["legal_name"], "Brightfield Labs S.r.l.");
    assert_eq!(shown["tax_country"], "IT");
    assert_eq!(shown["website"], "https://brightfield.test");
    assert_eq!(shown["notes"], "Billing goes through Lena.");

    // A detail left out is cleared; a tax identifier alone is refused.
    let (status, json) = api
        .call(
            "PUT",
            &page,
            Some(serde_json::json!({ "name": "Brightfield", "tax_id": "00000000000" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    let (status, _) = api
        .call(
            "PUT",
            &page,
            Some(serde_json::json!({ "name": "Brightfield" })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let shown = api.get(&page).await;
    assert!(
        shown["legal_name"].is_null() && shown["tax_id"].is_null(),
        "{shown}"
    );
    assert_eq!(shown["notes"], "");

    let offered = |list: &Value| {
        list.as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == brightfield.to_string())
    };
    let (status, _) = api.call("POST", &format!("{page}/offboard"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!offered(&api.get("/api/customers").await));
    let (status, json) = api
        .call("PUT", &page, Some(serde_json::json!({ "name": "X" })))
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an offboarded customer does not change: {json}"
    );

    let (status, json) = api.call("POST", &format!("{page}/bring-back"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    assert!(offered(&api.get("/api/customers").await));
    assert!(api.get(&page).await["offboarded_at"].is_null());
    let (status, _) = api.call("POST", &format!("{page}/bring-back"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "it is no longer offboarded");
}

/// A requester is marked departed and brought back from their page, and is shown as departed
/// wherever they appear.
#[sqlx::test]
async fn a_requester_departs_and_comes_back_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let api = Client::new(&db).await;
    let worker = role_with(&db, "Ticket worker", &[Permission::WorkTickets]).await;
    let worker = Client::with_role(&db, "wanda@northgate.test", worker).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let n = testing::ticket(&db, brightfield, Some(ada), "a@brightfield.test").await;

    let depart = format!("/api/requesters/{ada}/depart");
    let (status, _) = worker.call("POST", &depart, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "departing needs managing customers"
    );
    let (status, json) = api.call("POST", &depart, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let (status, _) = api.call("POST", &depart, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "departed twice");

    let page = api.get(&format!("/api/requesters/{ada}")).await;
    assert!(page["departed_at"].is_string(), "{page}");
    let customer = api.get(&format!("/api/customers/{brightfield}")).await;
    assert_eq!(customer["requester_list"][0]["departed"], true);
    let ticket = api.get(&format!("/api/tickets/{n}")).await;
    assert_eq!(ticket["requester"]["departed"], true);
    let list = api.get("/api/tickets").await;
    assert_eq!(list[0]["requester_departed"], true);
    // Their message waits in the inbox, which says why.
    testing::inbox_message(&db, "ada@brightfield.test").await;
    testing::inbox_message(&db, "someone@elsewhere.test").await;
    let inbox = api.get("/api/inbox").await;
    assert_eq!(inbox[0]["departed_from"], "Brightfield", "{inbox}");
    assert!(inbox[1]["departed_from"].is_null(), "{inbox}");

    let (status, json) = api
        .call(
            "POST",
            "/api/tickets",
            Some(serde_json::json!({
                "customer_id": brightfield,
                "title": "Phone: VPN down",
                "requester_email": "ada@brightfield.test",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");

    let back = format!("/api/requesters/{ada}/bring-back");
    let (status, _) = worker.call("POST", &back, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, json) = api.call("POST", &back, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let page = api.get(&format!("/api/requesters/{ada}")).await;
    assert!(page["departed_at"].is_null());
    let ticket = api.get(&format!("/api/tickets/{n}")).await;
    assert_eq!(ticket["requester"]["departed"], false);
}

/// External references are listed on the customer's page, added and removed by those who manage
/// customers; a name already naming another customer in the same system is refused.
#[sqlx::test]
async fn external_references_are_managed_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let add = |customer: Uuid, system: &str, name: &str| {
        (
            format!("/api/customers/{customer}/external-references"),
            serde_json::json!({ "system": system, "name": name }),
        )
    };

    let (uri, body) = add(brightfield, "Watchtower", "Brightfield/Offices");
    let (status, created) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let offices = created["id"].as_str().unwrap().to_owned();
    let (uri, body) = add(brightfield, "Watchtower", "Brightfield/Lab");
    let (status, _) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);

    let (uri, body) = add(harbor, "watchtower", "brightfield/offices");
    let (status, refused) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("Brightfield"),
        "{refused}"
    );

    let page = api.get(&format!("/api/customers/{brightfield}")).await;
    let listed: Vec<(&str, &str)> = page["external_references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["system"].as_str().unwrap(), r["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        listed,
        [
            ("Watchtower", "Brightfield/Lab"),
            ("Watchtower", "Brightfield/Offices")
        ]
    );

    // Changing them needs managing customers.
    let reader = role_with(&db, "Reader", &[Permission::WorkTickets]).await;
    let rita = Client::with_role(&db, "rita@northgate.test", reader).await;
    let (uri, body) = add(harbor, "Watchtower", "Harbor");
    let (status, _) = rita.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let remove_offices = format!("/api/external-references/{offices}");
    let (status, _) = rita.call("DELETE", &remove_offices, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let remove = || api.call("DELETE", &remove_offices, None);
    let (status, _) = remove().await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let page = api.get(&format!("/api/customers/{brightfield}")).await;
    assert_eq!(page["external_references"].as_array().unwrap().len(), 1);
    let (status, _) = remove().await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (uri, body) = add(harbor, "Watchtower", "Brightfield/Offices");
    let (status, _) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the removed name is free again"
    );
}

/// The System field offers the systems in use across all customers, one spelling each.
#[sqlx::test]
async fn the_systems_in_use_are_offered_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    crate::external_references::add(&db, brightfield, "Watchtower", "Brightfield/Offices")
        .await
        .unwrap();
    crate::external_references::add(&db, harbor, "WATCHTOWER", "Harbor")
        .await
        .unwrap();
    crate::external_references::add(&db, harbor, "Ledgerly", "HRB-0007")
        .await
        .unwrap();

    let systems = api.get("/api/external-references/systems").await;
    assert_eq!(systems, serde_json::json!(["Ledgerly", "Watchtower"]));
}

/// An offboarded customer's external references do not change: adding and removing are refused.
#[sqlx::test]
async fn an_offboarded_customer_s_external_references_are_refused_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let harbor = crate::customers::create(&db, "Harbor Lane", &[])
        .await
        .unwrap();
    let reference = crate::external_references::add(&db, harbor, "Watchtower", "Harbor")
        .await
        .unwrap();
    crate::customers::offboard(&db, harbor).await.unwrap();

    let (status, json) = api
        .call(
            "POST",
            &format!("/api/customers/{harbor}/external-references"),
            Some(serde_json::json!({ "system": "Watchtower", "name": "Harbor/Docks" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    let (status, json) = api
        .call(
            "DELETE",
            &format!("/api/external-references/{reference}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    let page = api.get(&format!("/api/customers/{harbor}")).await;
    assert_eq!(page["external_references"].as_array().unwrap().len(), 1);
}

/// Add-ons with `directory:read` read the external references of the customers they see, so that
/// they tell which customer a name of their own system belongs to.
#[sqlx::test]
async fn an_addon_reads_the_external_references_of_the_customers_it_sees(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = crate::customers::create(&db, "Harbor Lane", &[])
        .await
        .unwrap();
    crate::external_references::add(&db, brightfield, "Watchtower", "Brightfield/Offices")
        .await
        .unwrap();
    crate::external_references::add(&db, harbor, "Watchtower", "Harbor")
        .await
        .unwrap();
    crate::customers::offboard(&db, harbor).await.unwrap();

    let directory = crate::addons::register(
        &db,
        "watchtower",
        "http://127.0.0.1:9",
        &[],
        &[crate::addons::DIRECTORY_READ.into()],
    )
    .await
    .unwrap();
    let bot = Client::new_with_token(&db, &directory.token, api.agent);
    let customers = bot.get("/api/addon/customers").await;
    let customers = customers.as_array().unwrap();
    assert_eq!(customers.len(), 1, "offboarded customers are not seen");
    assert_eq!(customers[0]["name"], "Brightfield");
    assert_eq!(
        customers[0]["external_references"],
        serde_json::json!([{ "system": "Watchtower", "name": "Brightfield/Offices" }])
    );

    let blind = crate::addons::register(
        &db,
        "blind",
        "http://127.0.0.1:9",
        &[],
        &[crate::addons::TICKETS_READ.into()],
    )
    .await
    .unwrap();
    let (status, _) = Client::new_with_token(&db, &blind.token, api.agent)
        .call("GET", "/api/addon/customers", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "directory:read is needed");
}

/// Two requesters of one customer are merged from the page of the one that remains, by agents who
/// manage customers; requesters of different customers are refused.
#[sqlx::test]
async fn requesters_are_merged_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let api = Client::new(&db).await;
    let mover = role_with(&db, "Mover", &[Permission::MoveTickets]).await;
    let mover = Client::with_role(&db, "mina@northgate.test", mover).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let twin = testing::requester(&db, brightfield, "a.ferrini@home.test").await;
    let ivo = testing::requester(&db, harbor, "ivo@harbor.test").await;
    let n = testing::ticket(&db, brightfield, Some(twin), "t1@home.test").await;

    let merge = format!("/api/requesters/{ada}/merge");
    let (status, _) = mover
        .call(
            "POST",
            &merge,
            Some(serde_json::json!({ "requester_id": twin })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "merging needs managing customers"
    );
    let (status, json) = api
        .call(
            "POST",
            &merge,
            Some(serde_json::json!({ "requester_id": ivo })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    let (status, json) = api
        .call(
            "POST",
            &merge,
            Some(serde_json::json!({ "requester_id": twin })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");

    let page = api.get(&format!("/api/requesters/{ada}")).await;
    assert_eq!(
        page["addresses"],
        serde_json::json!(["ada@brightfield.test", "a.ferrini@home.test"])
    );
    let (status, _) = api
        .call("GET", &format!("/api/requesters/{twin}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let ticket = api.get(&format!("/api/tickets/{n}")).await;
    assert_eq!(ticket["requester"]["id"], ada.to_string());
    let customer = api.get(&format!("/api/customers/{brightfield}")).await;
    assert_eq!(customer["requester_list"].as_array().unwrap().len(), 1);
}

/// A requester is moved from their page with all their tickets, by agents who move tickets to
/// another customer; the page first shows what the move unlinks.
#[sqlx::test]
async fn a_requester_is_moved_from_their_page_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let manager = role_with(&db, "Manager", &[Permission::ManageCustomers]).await;
    let manager = Client::with_role(&db, "mara@northgate.test", manager).await;
    let mover = role_with(&db, "Mover", &[Permission::MoveTickets]).await;
    let mover = Client::with_role(&db, "mina@northgate.test", mover).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let first = testing::ticket(&db, brightfield, Some(ada), "a1@brightfield.test").await;
    let second = testing::ticket(&db, brightfield, Some(ada), "a2@brightfield.test").await;
    let laptop = testing::asset(&db, brightfield, "Laptop", "BF-LT-07").await;
    crate::catalog::link_asset(&db, second, laptop, true)
        .await
        .unwrap();

    let preview = manager
        .get(&format!(
            "/api/requesters/{ada}/move-preview?customer_id={harbor}"
        ))
        .await;
    assert_eq!(preview["tickets"], 2);
    assert_eq!(preview["assets"][0]["name"], "BF-LT-07");
    assert_eq!(preview["services"], serde_json::json!([]));

    let moving = format!("/api/requesters/{ada}/move");
    let to_harbor = serde_json::json!({ "customer_id": harbor });
    let (status, _) = manager.call("POST", &moving, Some(to_harbor.clone())).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "moving needs moving tickets to another customer"
    );
    let (status, json) = mover.call("POST", &moving, Some(to_harbor.clone())).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json, serde_json::json!({ "moved": 2, "unlinked": 1 }));
    for n in [first, second] {
        let ticket = mover.get(&format!("/api/tickets/{n}")).await;
        assert_eq!(ticket["customer"]["id"], harbor.to_string());
    }
    let (status, json) = mover.call("POST", &moving, Some(to_harbor)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
}

/// Key requesters are marked, given another contact address and unmarked on the customer's page by
/// agents who manage customers; a departed requester is not marked. A reply on a ticket without a
/// requester, by an agent who replies to requesters, goes to each key requester, and is refused
/// when the customer has none.
#[sqlx::test]
async fn key_requesters_are_marked_and_written_to_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let manager = role_with(&db, "Manager", &[Permission::ManageCustomers]).await;
    let manager = Client::with_role(&db, "mara@northgate.test", manager).await;
    let replier = role_with(&db, "Replier", &[Permission::Reply]).await;
    let replier = Client::with_role(&db, "remo@northgate.test", replier).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    crate::requesters::add_address(&db, ada, "ada@home.test")
        .await
        .unwrap();
    let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
    let gone = testing::requester(&db, brightfield, "rino@brightfield.test").await;
    crate::requesters::depart(&db, gone).await.unwrap();
    let n = testing::ticket(&db, brightfield, None, "alert@monitor.test").await;
    let other = testing::ticket(&db, harbor, None, "alert2@monitor.test").await;
    let key = |id: Uuid| format!("/api/requesters/{id}/key");
    let at = |address: &str| Some(serde_json::json!({ "contact_address": address }));

    let (status, _) = replier.call("PUT", &key(ada), at("ada@home.test")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "marking needs managing customers"
    );
    for (id, address) in [
        (ada, "ada@brightfield.test"),
        (lena, "lena@brightfield.test"),
    ] {
        let (status, json) = manager.call("PUT", &key(id), at(address)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    }
    let (status, json) = manager.call("PUT", &key(ada), at("ada@home.test")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let (status, json) = manager
        .call("PUT", &key(gone), at("rino@brightfield.test"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "a departed requester: {json}");
    let (status, _) = replier.call("DELETE", &key(lena), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "unmarking needs managing customers"
    );
    let (status, json) = manager.call("DELETE", &key(lena), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");

    let page = manager.get(&format!("/api/customers/{brightfield}")).await;
    let contacts: Vec<(String, Value)> = page["requester_list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_owned(),
                r["contact_address"].clone(),
            )
        })
        .collect();
    assert_eq!(
        contacts,
        [
            (ada.to_string(), Value::from("ada@home.test")),
            (lena.to_string(), Value::Null),
            (gone.to_string(), Value::Null)
        ]
    );

    let ticket = replier.get(&format!("/api/tickets/{n}")).await;
    assert_eq!(
        ticket["key_requesters"][0]["contact_address"],
        "ada@home.test"
    );
    let reply = Some(serde_json::json!({ "body": "We are on it." }));
    let (status, json) = replier
        .call("POST", &format!("/api/tickets/{n}/replies"), reply.clone())
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");
    let ticket = replier.get(&format!("/api/tickets/{n}")).await;
    assert_eq!(ticket["requester"], Value::Null);
    let to: Vec<&Value> = ticket["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["direction"] == "outbound")
        .map(|e| &e["to_address"])
        .collect();
    assert_eq!(to, [&Value::from("ada@home.test")]);
    let (status, json) = replier
        .call("POST", &format!("/api/tickets/{other}/replies"), reply)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "nobody to reply to: {json}");
}

/// Add-ons read a ticket without its customer's key requesters and their contact addresses: no
/// add-on needs them.
#[sqlx::test]
async fn add_ons_do_not_read_key_requesters(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let lena = testing::requester(&db, brightfield, "lena@brightfield.test").await;
    crate::key_requesters::mark(&db, lena, None).await.unwrap();
    let n = testing::ticket(&db, brightfield, None, "alert@monitor.test").await;
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

    let seen = bot.get(&format!("/api/addon/tickets/{n}")).await;
    assert_eq!(seen["number"], n);
    assert!(seen.get("key_requesters").is_none(), "{seen}");
    assert!(
        !seen.to_string().contains("lena@brightfield.test"),
        "{seen}"
    );
    let agents = api.get(&format!("/api/tickets/{n}")).await;
    assert_eq!(
        agents["key_requesters"][0]["contact_address"],
        "lena@brightfield.test"
    );
}

#[sqlx::test]
async fn an_import_is_previewed_then_applied(db: PgPool) {
    let api = Client::new(&db).await;
    let import = serde_json::json!({ "files": [
        { "kind": "sites", "content": "customer;name;address\r\nBrightfield;Head office;Via dei Tigli 1\r\nNowhere;Lab;Via del Glicine 3\r\n" },
        { "kind": "customers", "content": "\u{feff}name,tax_id,tax_country\nBrightfield,,\nHarbor Lane,000,ITA\n" },
    ]});

    let (status, preview) = api
        .call("POST", "/api/imports/preview", Some(import.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["files"][0]["kind"], "customers");
    assert_eq!(preview["files"][0]["rows"][0]["outcome"], "create");
    assert_eq!(preview["files"][0]["rows"][1]["outcome"], "left_out");
    assert_eq!(preview["files"][1]["rows"][1]["line"], 3);
    assert!(
        preview["files"][1]["left_out"]
            .as_str()
            .unwrap()
            .starts_with("customer;name;address;error\r\nNowhere;Lab;"),
        "{preview}"
    );
    assert_eq!(api.get("/api/customers/all").await, serde_json::json!([]));

    let (status, applied) = api.call("POST", "/api/imports", Some(import)).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["applied"], true);
    let customers = api.get("/api/customers/all").await;
    assert_eq!(customers[0]["name"], "Brightfield");
    let brightfield = customers[0]["id"].as_str().unwrap();
    let shown = api.get(&format!("/api/customers/{brightfield}")).await;
    assert_eq!(shown["sites"][0]["name"], "Head office");
}

#[sqlx::test]
async fn requesters_are_imported_after_their_customers_and_sites(db: PgPool) {
    let api = Client::new(&db).await;
    let import = serde_json::json!({ "files": [
        { "kind": "requesters", "content": "customer,email_addresses,name,site\n\
            Brightfield,ada@brightfield.test,Ada Ferrini,Head office\n\
            Brightfield,ada@brightfield.test,Ada F.,\n" },
        { "kind": "sites", "content": "customer,name,address\nBrightfield,Head office,Via dei Tigli 1\n" },
        { "kind": "customers", "content": "name\nBrightfield\n" },
    ]});
    let (status, preview) = api
        .call("POST", "/api/imports/preview", Some(import.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["files"][2]["kind"], "requesters");
    assert_eq!(preview["files"][2]["rows"][0]["outcome"], "create");
    assert_eq!(
        preview["files"][2]["rows"][0]["label"],
        "Brightfield / Ada Ferrini (ada@brightfield.test)"
    );
    assert_eq!(preview["files"][2]["rows"][1]["outcome"], "left_out");
    assert!(
        preview["files"][2]["left_out"]
            .as_str()
            .unwrap()
            .starts_with("customer,email_addresses,name,site,error\r\nBrightfield,ada@brightfield.test,Ada F.,,line 2 already imports"),
        "{preview}"
    );

    let (status, applied) = api.call("POST", "/api/imports", Some(import)).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let customers = api.get("/api/customers/all").await;
    let brightfield = customers[0]["id"].as_str().unwrap();
    let shown = api.get(&format!("/api/customers/{brightfield}")).await;
    let ada = shown["requester_list"][0]["id"].as_str().unwrap();
    let ada = api.get(&format!("/api/requesters/{ada}")).await;
    assert_eq!(ada["name"], "Ada Ferrini");
    assert_eq!(ada["site"]["name"], "Head office");
}

/// Assets are imported last, by an agent who manages customers and the catalog; an import with
/// an assets file is refused as a whole to an agent who manages customers only.
#[sqlx::test]
async fn assets_are_imported_by_an_agent_who_manages_the_catalog_too(db: PgPool) {
    use crate::roles::Permission;
    testing::asset_kind(&db, "Laptop").await;
    let import = serde_json::json!({ "files": [
        { "kind": "assets", "content": "customer,name,serial_number,kind,site\n\
            Brightfield,PC-0042,SN-0042,laptop,Head office\n\
            Brightfield,PC-0043,,Tablet,\n" },
        { "kind": "sites", "content": "customer,name,address\nBrightfield,Head office,Via dei Tigli 1\n" },
        { "kind": "customers", "content": "name\nBrightfield\n" },
    ]});
    let customers_only = role_with(&db, "Accounts", &[Permission::ManageCustomers]).await;
    let customers_only = Client::with_role(&db, "carla@northgate.test", customers_only).await;
    for uri in ["/api/imports/preview", "/api/imports"] {
        let (status, json) = customers_only.call("POST", uri, Some(import.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {json}");
    }
    assert_eq!(
        customers_only.get("/api/customers/all").await,
        serde_json::json!([])
    );

    let both = role_with(
        &db,
        "Inventory",
        &[Permission::ManageCustomers, Permission::ManageCatalog],
    )
    .await;
    let api = Client::with_role(&db, "ivo@northgate.test", both).await;
    let (status, preview) = api
        .call("POST", "/api/imports/preview", Some(import.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["files"][2]["kind"], "assets");
    assert_eq!(preview["files"][2]["rows"][0]["outcome"], "create");
    assert_eq!(
        preview["files"][2]["rows"][0]["label"],
        "Brightfield / PC-0042 (SN-0042)"
    );
    assert!(
        preview["files"][2]["left_out"]
            .as_str()
            .unwrap()
            .starts_with(
                "customer,name,serial_number,kind,site,error\r\nBrightfield,PC-0043,,Tablet,,there is no asset kind called Tablet"
            ),
        "{preview}"
    );

    let (status, applied) = api.call("POST", "/api/imports", Some(import)).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let customers = api.get("/api/customers/all").await;
    let brightfield = customers[0]["id"].as_str().unwrap();
    let assets = api
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    assert_eq!(assets.as_array().unwrap().len(), 1, "{assets}");
    assert_eq!(assets[0]["serial_number"], "SN-0042");
    assert_eq!(assets[0]["site"]["name"], "Head office");
}

#[sqlx::test]
async fn an_import_needs_managing_customers_and_files_it_can_read(db: PgPool) {
    use crate::roles::Permission;
    let catalog = role_with(&db, "Catalog", &[Permission::ManageCatalog]).await;
    let other = Client::with_role(&db, "carla@northgate.test", catalog).await;
    let import = serde_json::json!({ "files": [
        { "kind": "customers", "content": "name\nBrightfield\n" },
    ]});
    for uri in ["/api/imports/preview", "/api/imports"] {
        let (status, json) = other.call("POST", uri, Some(import.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {json}");
    }

    let api = Client::new(&db).await;
    let unknown = serde_json::json!({ "files": [
        { "kind": "customers", "content": "name,adress\nBrightfield,Via dei Tigli 1\n" },
    ]});
    let (status, json) = api
        .call("POST", "/api/imports/preview", Some(unknown))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert!(json["error"].as_str().unwrap().contains("adress"), "{json}");
    let (status, json) = api
        .call(
            "POST",
            "/api/imports",
            Some(serde_json::json!({ "files": [] })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");

    // Files larger than other requests are taken, up to a limit.
    let notes = "Long notes. ".repeat(300_000);
    let large = serde_json::json!({ "files": [
        { "kind": "customers", "content": format!("name,notes\nBrightfield,{notes}\n") },
    ]});
    let (status, json) = api.call("POST", "/api/imports/preview", Some(large)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let too_big = serde_json::json!({ "files": [
        { "kind": "customers", "content": "x".repeat(super::imports::BODY_LIMIT) },
    ]});
    let (status, _) = api
        .call("POST", "/api/imports/preview", Some(too_big))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

/// An agent who replies to requesters sends announcements from a parent ticket and sees each one
/// on the parent, with when it was sent and to whom; an agent who cannot reply is refused, and so
/// is an announcement from a closed parent. Add-ons read the parent without its announcements.
#[sqlx::test]
async fn announcements_are_sent_and_listed_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let replier = role_with(&db, "Replier", &[Permission::Reply]).await;
    let replier = Client::with_role(&db, "remo@northgate.test", replier).await;
    let worker = role_with(&db, "Worker", &[Permission::WorkTickets]).await;
    let worker = Client::with_role(&db, "wanda@northgate.test", worker).await;
    let own = testing::help_desk(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    crate::key_requesters::mark(&db, ada, None).await.unwrap();
    let outage = testing::ticket(&db, own, None, "outage@northgate.test").await;
    let child = testing::ticket(&db, brightfield, None, "a@brightfield.test").await;
    crate::parents::attach(&db, child, outage).await.unwrap();
    let uri = format!("/api/tickets/{outage}/announcements");
    let body = Some(serde_json::json!({ "body": "Mail is down; we are on it." }));

    let (status, _) = worker.call("POST", &uri, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "announcing needs replying");
    let (status, sent) = replier.call("POST", &uri, body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_eq!(sent["recipients"][0]["address"], "ada@brightfield.test");

    let ticket = worker.get(&format!("/api/tickets/{outage}")).await;
    let listed: Vec<&Value> = ticket["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "announcement")
        .collect();
    assert_eq!(listed.len(), 1, "{ticket}");
    assert_eq!(listed[0]["body"], "Mail is down; we are on it.");
    assert_eq!(listed[0]["author_name"], "remo");
    assert!(listed[0]["created_at"].is_string(), "{}", listed[0]);
    assert_eq!(listed[0]["recipients"][0]["customer_name"], "Brightfield");
    assert_eq!(
        listed[0]["recipients"][0]["address"],
        "ada@brightfield.test"
    );
    let child_seen = worker.get(&format!("/api/tickets/{child}")).await;
    assert_eq!(child_seen["state"], "open");

    let reader = crate::addons::register(
        &db,
        "reader",
        "http://127.0.0.1:9",
        &[],
        &["tickets:read".into()],
    )
    .await
    .unwrap();
    let bot = Client::new_with_token(&db, &reader.token, replier.agent);
    let seen = bot.get(&format!("/api/addon/tickets/{outage}")).await;
    assert!(!seen.to_string().contains("ada@brightfield.test"), "{seen}");

    let (status, _) = worker
        .call(
            "PUT",
            &format!("/api/tickets/{outage}/state"),
            Some(serde_json::json!({ "state": "closed" })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, json) = replier.call("POST", &uri, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "from a closed parent: {json}");
}

/// The sign-in page offers the identity provider only when one is configured.
#[sqlx::test]
async fn the_sign_in_page_offers_the_identity_provider_only_when_configured(db: PgPool) {
    let without = Client::anonymous(AppState::for_tests(&db));
    let (status, options) = without.call("GET", "/api/session/options", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(options, serde_json::json!({"identity_provider": null}));
    let (status, _) = without
        .call("GET", "/api/session/identity-provider", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = without
        .call(
            "GET",
            "/api/session/identity-provider/callback?code=c&state=s",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let settings = crate::oidc::Settings {
        issuer: Some("https://login.example.com/realms/staff".to_owned()),
        client_id: Some("helpdesk".to_owned()),
        client_secret: Some("invented-secret".to_owned()),
        client_secret_file: None,
        label: Some("Northgate".to_owned()),
        session_hours: 12,
        public_url: Some("https://helpdesk.example.com".to_owned()),
    };
    let config = crate::oidc::Config::from_settings(settings)
        .unwrap()
        .unwrap();
    let with = Client::anonymous(AppState {
        identity_provider: Some(crate::oidc::Provider::new(config).unwrap()),
        ..AppState::for_tests(&db)
    });
    let (status, options) = with.call("GET", "/api/session/options", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        options,
        serde_json::json!({"identity_provider": {"label": "Northgate"}})
    );
}
