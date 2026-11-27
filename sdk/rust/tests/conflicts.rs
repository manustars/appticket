//! Reporting the add-on's conflicts (scope `conflicts:write`) against a small fake of the core on
//! a local port.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::put,
};
use hd_addon_sdk::{Client, Conflict, Error};
use serde_json::{Value, json};

type Seen = Arc<Mutex<Vec<Value>>>;

async fn fake_core() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new().route(
        "/api/addon/conflicts",
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
                                              conflicts:write."})),
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
async fn an_addon_replaces_its_conflicts_each_with_a_sentence_per_language() {
    let (url, seen) = fake_core().await;
    let client = Client::new(&url, "token").unwrap();

    client
        .replace_conflicts(&[
            Conflict::in_english("The host db-03 is in two groups: it is not imported."),
            Conflict::new([
                ("en", "The host web-07 is in no group a customer has."),
                ("it", "L'host web-07 non è in alcun gruppo di un cliente."),
            ]),
        ])
        .await
        .unwrap();
    client.replace_conflicts(&[]).await.unwrap();
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [
            json!({"conflicts": [
                {"sentence": {"en": "The host db-03 is in two groups: it is not imported."}},
                {"sentence": {"en": "The host web-07 is in no group a customer has.",
                              "it": "L'host web-07 non è in alcun gruppo di un cliente."}}
            ]}),
            json!({"conflicts": []})
        ]
    );
}

#[tokio::test]
async fn a_refused_list_of_conflicts_carries_the_core_sentence() {
    let (url, _) = fake_core().await;
    let client = Client::new(&url, "token-without-the-scope").unwrap();

    let Err(Error::Api { status, message }) = client.replace_conflicts(&[]).await else {
        panic!("not refused");
    };
    assert_eq!(
        (status, message.as_str()),
        (
            409,
            "The add-on watchtower lacks the scope conflicts:write."
        )
    );
}
