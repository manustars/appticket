//! The webhook as Zabbix's media type reaches it, without Zabbix or the core: an event without
//! the webhook's secret is refused before anything else, and one the add-on cannot act on is
//! answered with a failure, so that Zabbix tries again.

use std::sync::Arc;
use std::time::Duration;

use hd_addon_sdk::Client;
use hd_zabbix::rules::Rules;
use hd_zabbix::webhook::{self, Alerts};
use hd_zabbix::zabbix::Zabbix;
use reqwest::StatusCode;
use serde_json::{Value, json};

const SECRET: &str = "a-webhook-secret-of-the-test";

/// The add-on's webhook, with a core and a Zabbix that nobody answers at; answers its URL.
async fn webhook() -> String {
    let nobody = "http://127.0.0.1:9";
    let alerts = Alerts::new(
        Client::new(nobody, "core-token").unwrap(),
        Zabbix::new(nobody, "zabbix-token", Duration::from_secs(2)).unwrap(),
        "Zabbix",
        Rules::default_list(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}{}", listener.local_addr().unwrap(), webhook::PATH);
    let app = webhook::router(Arc::new(alerts), SECRET.to_owned());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn post(url: &str, authorization: Option<&str>, body: Value) -> (StatusCode, String) {
    let mut request = hd_addon_sdk::http_client(Duration::from_secs(30))
        .unwrap()
        .post(url)
        .json(&body);
    if let Some(authorization) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }
    let response = request.send().await.unwrap();
    (response.status(), response.text().await.unwrap())
}

fn problem() -> Value {
    json!({"event_id": "26", "event_value": "1", "event_update_status": "0",
           "event_source": "0", "user": "{USER.FULLNAME}",
           "action": "{EVENT.UPDATE.ACTION}", "message": "{EVENT.UPDATE.MESSAGE}"})
}

#[tokio::test]
async fn an_event_without_the_webhook_s_secret_is_refused() {
    let url = webhook().await;
    let bearer = format!("Bearer {SECRET}");
    for refused in [
        None,
        Some("Bearer another-secret-of-the-test"),
        Some(SECRET),
        // The media type's own value when the secret's macro is not set in Zabbix.
        Some("Bearer {$HD_WEBHOOK_SECRET}"),
    ] {
        assert_eq!(
            post(&url, refused, problem()).await,
            (
                StatusCode::UNAUTHORIZED,
                "the webhook's secret is wrong".to_owned()
            ),
            "{refused:?}"
        );
    }

    // With it, what is not one of the media type's events is refused, and an event of another
    // source left alone.
    let (status, why) = post(&url, Some(&bearer), json!({"event_id": "26"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{why}");
    let mut internal = problem();
    internal["event_source"] = json!("3");
    assert_eq!(
        post(&url, Some(&bearer), internal).await,
        (StatusCode::OK, r#"{"tags":{}}"#.to_owned())
    );
}

#[tokio::test]
async fn an_event_the_add_on_cannot_act_on_is_answered_with_a_failure_so_that_zabbix_tries_again() {
    let url = webhook().await;
    let (status, why) = post(&url, Some(&format!("Bearer {SECRET}")), problem()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(why.contains("event.get"), "{why}");
}

#[tokio::test]
async fn an_update_without_a_message_needs_neither_zabbix_nor_the_core() {
    let url = webhook().await;
    let update = json!({"event_id": "26", "event_value": "1", "event_update_status": "1",
                        "event_source": "0", "user": "Olga Operator (olga)",
                        "action": "acknowledged", "message": "{EVENT.UPDATE.MESSAGE}"});
    assert_eq!(
        post(&url, Some(&format!("Bearer {SECRET}")), update).await,
        (StatusCode::OK, r#"{"tags":{}}"#.to_owned())
    );
}
