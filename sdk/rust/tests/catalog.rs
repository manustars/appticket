//! Supplying assets (scope `catalog:write`) against a small fake of the core on a local port.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    routing::{post, put},
};
use hd_addon_sdk::api::{MatchedBy, Supplied, SuppliedAsset, SuppliedDetails};
use hd_addon_sdk::{Client, Error};
use serde_json::{Value, json};
use uuid::Uuid;

const CUSTOMER: &str = "0e7b6a52-5d0f-4d5e-8a8e-3f2b1c0d9e8f";
const ASSET: &str = "5a1d0c3e-7b2f-4e6a-9c8d-1f0e2d3c4b5a";
const HAND_MADE: &str = "6b2e1d4f-8c3a-4f7b-8d9e-2a1f3e4d5c6b";
const PROPOSAL: &str = "7c3f2e5a-9d4b-4a8c-9e0f-3b2a4f5e6d7c";

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// The asset as the core answers, with what the add-on sent.
fn answered(body: &Value) -> Value {
    json!({"id": ASSET, "customer_id": CUSTOMER, "customer_name": "Brightfield",
        "kind_id": "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f", "kind": body["kind"],
        "name": body["name"], "serial_number": body["serial_number"],
        "site": {"id": "2d3e4f5a-6b7c-4d8e-9f0a-1b2c3d4e5f6a", "name": "Head office"},
        "source_addon": "watchtower", "reference": "host-1042", "retired_at": null})
}

async fn fake_core() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route(
            "/api/addon/assets",
            post({
                let seen = seen.clone();
                move |Json(body): Json<Value>| async move {
                    if body["reference"] == "host-9" {
                        // Agents accepted its link proposal: the reference reaches the asset
                        // another add-on supplies.
                        return (
                            StatusCode::OK,
                            Json(json!({"id": HAND_MADE, "customer_id": CUSTOMER,
                                "customer_name": "Brightfield",
                                "kind_id": "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f",
                                "kind": "Laptop", "name": "PC-0001", "serial_number": null,
                                "site": null, "source_addon": "netscope",
                                "reference": "host-9", "linked_addons": ["watchtower"],
                                "retired_at": null})),
                        );
                    }
                    if body["reference"] == "host-7" {
                        // The customer has an asset with that serial number already.
                        return (
                            StatusCode::ACCEPTED,
                            Json(json!({"id": PROPOSAL, "addon": "watchtower",
                                "reference": "host-7", "customer_id": CUSTOMER,
                                "name": body["name"], "serial_number": body["serial_number"],
                                "matched_by": "serial_number", "asset_id": HAND_MADE,
                                "asset_name": "PC-0001",
                                "created_at": "2026-11-27T09:00:00Z"})),
                        );
                    }
                    let known = !seen.lock().unwrap().is_empty();
                    seen.lock().unwrap().push(("POST".into(), body.clone()));
                    let status = if known {
                        StatusCode::OK
                    } else {
                        StatusCode::CREATED
                    };
                    (status, Json(answered(&body)))
                }
            }),
        )
        .route(
            "/api/addon/assets/{id}",
            put({
                let seen = seen.clone();
                move |Path(id): Path<String>, Json(body): Json<Value>| async move {
                    seen.lock()
                        .unwrap()
                        .push((format!("PUT {id}"), body.clone()));
                    if id == HAND_MADE {
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({"error": "PC-0001 was entered by hand: an add-on \
                                changes only the assets it is the source of."})),
                        );
                    }
                    (StatusCode::OK, Json(answered(&body)))
                }
            }),
        )
        .route(
            "/api/addon/assets/{id}/retire",
            post({
                let seen = seen.clone();
                move |Path(id): Path<String>| async move {
                    seen.lock()
                        .unwrap()
                        .push((format!("retire {id}"), Value::Null));
                    StatusCode::NO_CONTENT
                }
            }),
        )
        .route(
            "/api/addon/assets/{id}/unlink",
            post({
                let seen = seen.clone();
                move |Path(id): Path<String>| async move {
                    seen.lock()
                        .unwrap()
                        .push((format!("unlink {id}"), Value::Null));
                    StatusCode::NO_CONTENT
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, seen)
}

fn laptop() -> SuppliedDetails {
    SuppliedDetails {
        kind: "Laptop".into(),
        name: "PC-0042".into(),
        serial_number: Some("SN-0042".into()),
        site: Some("Head office".into()),
    }
}

#[tokio::test]
async fn an_addon_supplies_its_assets_in_the_core_shape() {
    let (url, seen) = fake_core().await;
    let client = Client::new(&url, "token-of-the-test-addon").unwrap();
    let customer = Uuid::parse_str(CUSTOMER).unwrap();
    let asset = SuppliedAsset {
        reference: "host-1042".into(),
        customer_id: customer,
        details: laptop(),
    };

    let Supplied::Asset {
        asset: first,
        created: true,
    } = client.supply_asset(&asset).await.unwrap()
    else {
        panic!("201: created");
    };
    assert_eq!(
        (
            first.reference.as_deref(),
            first.source_addon.as_deref(),
            first.site.map(|s| s.name)
        ),
        (
            Some("host-1042"),
            Some("watchtower"),
            Some("Head office".to_owned())
        )
    );
    assert!(
        matches!(
            client.supply_asset(&asset).await.unwrap(),
            Supplied::Asset { created: false, .. }
        ),
        "200: updated"
    );

    let id = first.id;
    let renamed = SuppliedDetails {
        name: "PC-0043".into(),
        serial_number: None,
        site: None,
        ..laptop()
    };
    assert_eq!(
        client.update_asset(id, &renamed).await.unwrap().name,
        "PC-0043"
    );
    client.retire_asset(id).await.unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0],
        (
            "POST".to_owned(),
            json!({"reference": "host-1042", "customer_id": customer, "kind": "Laptop",
                "name": "PC-0042", "serial_number": "SN-0042", "site": "Head office"})
        )
    );
    assert_eq!(
        seen[2],
        (
            format!("PUT {id}"),
            json!({"kind": "Laptop", "name": "PC-0043", "serial_number": null, "site": null})
        )
    );
    assert_eq!(seen[3], (format!("retire {id}"), Value::Null));
}

#[tokio::test]
async fn an_asset_matching_one_the_customer_has_comes_back_as_a_link_proposal() {
    let (url, seen) = fake_core().await;
    let client = Client::new(&url, "token-of-the-test-addon").unwrap();
    let asset = SuppliedAsset {
        reference: "host-7".into(),
        customer_id: Uuid::parse_str(CUSTOMER).unwrap(),
        details: SuppliedDetails {
            serial_number: Some("SN-0001".into()),
            ..laptop()
        },
    };

    let Supplied::LinkProposed(proposal) = client.supply_asset(&asset).await.unwrap() else {
        panic!("202: a link proposal");
    };
    assert_eq!(
        (
            proposal.id,
            proposal.reference.as_str(),
            proposal.matched_by,
            proposal.asset_id,
            proposal.asset_name.as_str(),
        ),
        (
            Uuid::parse_str(PROPOSAL).unwrap(),
            "host-7",
            MatchedBy::SerialNumber,
            Uuid::parse_str(HAND_MADE).unwrap(),
            "PC-0001",
        )
    );
    assert!(seen.lock().unwrap().is_empty(), "no asset was created");
}

#[tokio::test]
async fn a_linked_addon_reaches_the_asset_by_its_reference_and_unlinks_itself() {
    let (url, seen) = fake_core().await;
    let client = Client::new(&url, "token-of-the-test-addon").unwrap();
    let asset = SuppliedAsset {
        reference: "host-9".into(),
        customer_id: Uuid::parse_str(CUSTOMER).unwrap(),
        details: SuppliedDetails {
            name: "PC-0001".into(),
            serial_number: None,
            site: None,
            ..laptop()
        },
    };

    let Supplied::Asset {
        asset: reached,
        created: false,
    } = client.supply_asset(&asset).await.unwrap()
    else {
        panic!("200: the linked asset");
    };
    assert_eq!(
        (
            reached.id,
            reached.source_addon.as_deref(),
            reached.reference.as_deref(),
            reached.linked_addons.as_slice()
        ),
        (
            Uuid::parse_str(HAND_MADE).unwrap(),
            Some("netscope"),
            Some("host-9"),
            ["watchtower".to_owned()].as_slice()
        ),
        "another add-on's asset, known by its own reference"
    );
    client.unlink_asset(reached.id).await.unwrap();
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [(format!("unlink {HAND_MADE}"), Value::Null)]
    );
}

#[tokio::test]
async fn an_asset_from_a_core_without_linked_addons_has_none() {
    let asset: hd_addon_sdk::api::Asset = serde_json::from_value(json!({"id": ASSET,
        "customer_id": CUSTOMER, "kind": "Laptop", "name": "PC-0042"}))
    .unwrap();
    assert!(asset.linked_addons.is_empty());
}

#[tokio::test]
async fn a_refused_asset_carries_the_core_sentence() {
    let (url, _) = fake_core().await;
    let client = Client::new(&url, "token-of-the-test-addon").unwrap();
    let refused = client
        .update_asset(Uuid::parse_str(HAND_MADE).unwrap(), &laptop())
        .await
        .unwrap_err();
    match refused {
        Error::Api { status, message } => assert_eq!(
            (status, message.as_str()),
            (
                409,
                "PC-0001 was entered by hand: an add-on changes only the assets it is the source \
                 of."
            )
        ),
        other => panic!("{other:?}"),
    }
}
