//! Listing the names of the add-on's system (scope `names:write`) against a small fake of the core
//! on a local port.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::put,
};
use hd_addon_sdk::names::{SystemName, SystemNames};
use hd_addon_sdk::{Client, Error};
use serde_json::{Value, json};

type Seen = Arc<Mutex<Vec<Value>>>;

async fn fake_core() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new().route(
        "/api/addon/names",
        put({
            let seen = seen.clone();
            move |headers: HeaderMap, Json(body): Json<Value>| async move {
                // An add-on registered without the scope.
                if headers.get("authorization").and_then(|v| v.to_str().ok())
                    == Some("Bearer token-without-the-scope")
                {
                    return (
                        StatusCode::CONFLICT,
                        Json(json!({"error": "The add-on watchtower lacks the scope \
                                              names:write."})),
                    );
                }
                seen.lock().unwrap().push(body);
                (StatusCode::NO_CONTENT, Json(Value::Null))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, seen)
}

#[tokio::test]
async fn an_addon_replaces_the_names_of_its_system_with_their_nesting() {
    let (url, seen) = fake_core().await;
    let client = Client::new(&url, "token").unwrap();
    let list = SystemNames {
        system: "Watchtower".into(),
        names: vec![
            SystemName::top("Brightfield"),
            SystemName::nested_in("Brightfield/Offices", "Brightfield"),
        ],
    };

    client.replace_names(&list).await.unwrap();
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [json!({"system": "Watchtower", "names": [
            {"name": "Brightfield"},
            {"name": "Brightfield/Offices", "parent": "Brightfield"}
        ]})]
    );
}

#[tokio::test]
async fn a_refused_list_carries_the_core_sentence() {
    let (url, _) = fake_core().await;
    let client = Client::new(&url, "token-without-the-scope").unwrap();
    let list = SystemNames {
        system: "Watchtower".into(),
        names: vec![],
    };

    let Err(Error::Api { status, message }) = client.replace_names(&list).await else {
        panic!("not refused");
    };
    assert_eq!(
        (status, message.as_str()),
        (409, "The add-on watchtower lacks the scope names:write.")
    );
}
