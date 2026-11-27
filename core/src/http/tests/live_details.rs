//! The live details of assets, as agents read them beside a customer's assets and a ticket's.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

use super::Client;
use crate::testing::{self, Answer, addon_at, fake_addon, live_details_answer, supplied_asset};

/// Beside a customer's assets agents read the live details of each from its source, with every
/// label in each language the add-on gave; a hand-made asset has none.
#[sqlx::test]
async fn agents_read_the_live_details_of_a_customer_s_assets(db: PgPool) {
    let api = Client::new(&db).await;
    let (url, _) = fake_addon(|_| Answer::Json(live_details_answer())).await;
    addon_at(&db, "watchtower", &url).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    testing::asset(&db, brightfield, "Laptop", "PC-0001").await;
    let srv = supplied_asset(&db, brightfield, "SRV-01", "watchtower", "host-1042").await;

    let details = api
        .get(&format!("/api/customers/{brightfield}/assets/live-details"))
        .await;
    assert_eq!(
        details,
        json!([{
            "asset_id": srv,
            "addon": "watchtower",
            "details": {
                "state": "warning",
                "fields": [
                    {"label": {"en": "Disk /var", "it": "Disco /var"}, "value": "91% full"},
                    {"label": {"en": "Uptime"}, "value": "12 days"}
                ],
                "links": [{"label": {"en": "Open in Watchtower"},
                           "url": "https://watchtower.example.com/hosts/1042"}]
            }
        }])
    );
}

/// On a ticket agents read the live details of the ticket's assets only. One add-on that answers
/// late leaves the others' details as they are and the page's request within time; a ticket that
/// does not exist is not found.
#[sqlx::test]
async fn a_ticket_s_assets_have_their_live_details_and_a_slow_addon_holds_nothing_up(db: PgPool) {
    let api = Client::new(&db).await;
    let (fast, fast_asked) = fake_addon(|_| Answer::Json(live_details_answer())).await;
    addon_at(&db, "watchtower", &fast).await;
    let (slow, slow_asked) =
        fake_addon(|_| Answer::Late(Duration::from_secs(5), live_details_answer())).await;
    addon_at(&db, "inventory", &slow).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let srv = supplied_asset(&db, brightfield, "SRV-01", "watchtower", "host-1042").await;
    let pc = supplied_asset(&db, brightfield, "PC-0042", "inventory", "pc-42").await;
    supplied_asset(&db, brightfield, "SRV-02", "watchtower", "host-1043").await;
    let number = testing::ticket(&db, brightfield, None, "<disk@brightfield.test>").await;
    for asset in [srv, pc] {
        let (status, body) = api
            .call(
                "PUT",
                &format!("/api/tickets/{number}/assets/{asset}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    let started = Instant::now();
    let details = api
        .get(&format!("/api/tickets/{number}/assets/live-details"))
        .await;
    let took = started.elapsed();
    assert!(took < Duration::from_secs(4), "took {took:?}");
    let got: Vec<(String, String, bool)> = details
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["asset_id"].as_str().unwrap().to_owned(),
                d["addon"].as_str().unwrap().to_owned(),
                !d["details"].is_null(),
            )
        })
        .collect();
    // In the order the ticket lists its assets: by kind, then name.
    assert_eq!(
        got,
        [
            (pc.to_string(), "inventory".to_owned(), false),
            (srv.to_string(), "watchtower".to_owned(), true),
        ]
    );
    // Each add-on was asked with its own reference, and only for the assets the ticket links.
    let references = |asked: &testing::Asked| -> Vec<String> {
        asked
            .lock()
            .unwrap()
            .iter()
            .map(|(r, _)| r.clone())
            .collect()
    };
    assert_eq!(references(&fast_asked), ["host-1042"]);
    assert_eq!(references(&slow_asked), ["pc-42"]);

    let (status, _) = api
        .call("GET", "/api/tickets/999999/assets/live-details", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Live details are the agents' to read: an add-on's token does not open them, nor does no one.
#[sqlx::test]
async fn only_agents_read_live_details(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let everything: Vec<String> = crate::addons::SCOPES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let registered =
        crate::addons::register(&db, "watchtower", "http://127.0.0.1:9", &[], &everything)
            .await
            .unwrap();
    let bot = Client::new_with_token(&db, &registered.token, api.agent);
    let uri = format!("/api/customers/{brightfield}/assets/live-details");
    let (status, _) = bot.call("GET", &uri, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = Client::anonymous(crate::http::AppState::for_tests(&db))
        .call("GET", &uri, None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The specification describes the call the core makes to an add-on for live details, as a
/// webhook beside the events: `GET <url>/assets/{reference}`, signed, answered with the state,
/// fields and links, or 404.
#[test]
fn the_specification_describes_the_live_details_an_addon_serves() {
    let spec = serde_json::to_value(crate::http::openapi()).unwrap();
    let operation = &spec["webhooks"]["live_details"]["get"];
    let parameters: Vec<&str> = operation["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(parameters, ["reference", "hd-timestamp", "hd-signature"]);
    let answer = &operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"];
    let name = answer.as_str().unwrap().rsplit('/').next().unwrap();
    let mut fields: Vec<&str> = spec["components"]["schemas"][name]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(fields, ["fields", "links", "state"]);
    assert!(operation["responses"]["404"].is_object());
}
