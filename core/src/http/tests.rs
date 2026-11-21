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
    pub agent: Uuid,
}

impl Client {
    pub async fn new(db: &PgPool) -> Client {
        let agent = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agents (id, email, name, password_hash) VALUES ($1, 'marco@northgate.test', 'Marco Lenti', $2)",
        )
        .bind(agent)
        .bind(crate::auth::hash_password("pw").unwrap())
        .execute(db)
        .await
        .unwrap();
        let token = crate::auth::sign_in(db, "marco@northgate.test", "pw")
            .await
            .unwrap()
            .unwrap();
        Client {
            state: AppState {
                db: db.clone(),
                sender: Some(testing::sender()),
            },
            cookie: format!("{}={token}", crate::auth::SESSION_COOKIE),
            agent,
        }
    }

    pub async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, &self.cookie);
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
