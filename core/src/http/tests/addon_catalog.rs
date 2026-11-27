//! The assets an add-on supplies, through the add-on API with its token, and what agents can still
//! change on them through theirs.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::Client;
use crate::addons::{CATALOG_WRITE, DIRECTORY_READ};
use crate::testing;

/// A client calling with the token of a new add-on called `name` that has `scopes`.
async fn addon(db: &PgPool, api: &Client, name: &str, scopes: &[&str]) -> Client {
    let scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
    let registered = crate::addons::register(db, name, "http://127.0.0.1:9", &[], &scopes)
        .await
        .unwrap();
    Client::new_with_token(db, &registered.token, api.agent)
}

#[sqlx::test]
async fn an_addon_supplies_updates_and_retires_its_assets_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let head = crate::sites::create(&db, brightfield, "Head office", "Via dei Tigli 1")
        .await
        .unwrap();
    testing::asset_kind(&db, "Laptop").await;
    let watchtower = addon(&db, &api, "watchtower", &[CATALOG_WRITE, DIRECTORY_READ]).await;
    let asset = json!({"reference": "host-1042", "customer_id": brightfield, "kind": "laptop",
        "name": "PC-0042", "serial_number": "SN-0042", "site": "Head office"});

    let (status, created) = watchtower
        .call("POST", "/api/addon/assets", Some(asset.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(
        (
            &created["kind"],
            &created["name"],
            &created["serial_number"],
            &created["site"],
            &created["source_addon"],
            &created["reference"],
        ),
        (
            &json!("Laptop"),
            &json!("PC-0042"),
            &json!("SN-0042"),
            &json!({"id": head, "name": "Head office"}),
            &json!("watchtower"),
            &json!("host-1042"),
        )
    );

    // The whole inventory again: the same reference updates the same asset.
    let again = json!({"reference": "host-1042", "customer_id": brightfield, "kind": "Laptop",
        "name": "PC-0042 renamed"});
    let (status, updated) = watchtower
        .call("POST", "/api/addon/assets", Some(again))
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["id"], id.as_str());
    assert_eq!(
        (&updated["serial_number"], &updated["site"]),
        (&Value::Null, &Value::Null),
        "what is left out is cleared"
    );
    let listed = watchtower
        .get(&format!("/api/addon/customers/{brightfield}/assets"))
        .await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["reference"], "host-1042");

    let (status, updated) = watchtower
        .call(
            "PUT",
            &format!("/api/addon/assets/{id}"),
            Some(json!({"kind": "Laptop", "name": "PC-0043", "site": "head office"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        (&updated["name"], &updated["site"]["name"]),
        (&json!("PC-0043"), &json!("Head office"))
    );

    for _ in 0..2 {
        let (status, json) = watchtower
            .call("POST", &format!("/api/addon/assets/{id}/retire"), None)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    }
    let (status, json) = watchtower
        .call(
            "PUT",
            &format!("/api/addon/assets/{id}"),
            Some(json!({"kind": "Laptop", "name": "PC-0044"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "PC-0043 is retired: supplying it again with its reference brings it back."
    );
    let listed = watchtower
        .get(&format!("/api/addon/customers/{brightfield}/assets"))
        .await;
    assert_eq!(listed, json!([]), "retired");
    let shown = api
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    assert_eq!(
        (&shown[0]["source_addon"], &shown[0]["reference"]),
        (&json!("watchtower"), &json!("host-1042")),
        "agents see where it comes from"
    );
    assert!(shown[0]["retired_at"].is_string());

    // Another add-on reading the asset is not told the reference its source knows it by.
    let reader = addon(&db, &api, "reader", &[DIRECTORY_READ]).await;
    let (status, _) = watchtower
        .call("POST", "/api/addon/assets", Some(asset))
        .await;
    assert_eq!(status, StatusCode::OK, "supplied again, it comes back");
    let seen = reader
        .get(&format!("/api/addon/customers/{brightfield}/assets"))
        .await;
    assert_eq!(
        (&seen[0]["source_addon"], &seen[0]["reference"]),
        (&json!("watchtower"), &Value::Null)
    );
}

#[sqlx::test]
async fn the_catalog_calls_need_the_scope_catalog_write(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    testing::asset_kind(&db, "Laptop").await;
    let reader = addon(&db, &api, "reader", &[DIRECTORY_READ]).await;
    let pc = testing::asset(&db, brightfield, "Laptop", "PC-0042").await;

    for (method, uri, body) in [
        (
            "POST",
            "/api/addon/assets".to_owned(),
            Some(
                json!({"reference": "host-1", "customer_id": brightfield, "kind": "Laptop",
                "name": "PC-0042"}),
            ),
        ),
        (
            "PUT",
            format!("/api/addon/assets/{pc}"),
            Some(json!({"kind": "Laptop", "name": "PC-0042"})),
        ),
        ("POST", format!("/api/addon/assets/{pc}/retire"), None),
    ] {
        let (status, json) = reader.call(method, &uri, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {json}");
        assert_eq!(
            json["error"], "The add-on reader lacks the scope catalog:write.",
            "{method} {uri}"
        );
    }
    let shown = api
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    assert_eq!(shown.as_array().unwrap().len(), 1, "nothing created");
    assert!(shown[0]["retired_at"].is_null(), "nothing retired");
}

#[sqlx::test]
async fn an_addon_is_refused_a_holder_and_the_assets_it_is_not_the_source_of(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    crate::sites::create(&db, harbor, "Quay office", "Molo delle Reti 2")
        .await
        .unwrap();
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    testing::asset_kind(&db, "Laptop").await;
    let watchtower = addon(&db, &api, "watchtower", &[CATALOG_WRITE]).await;
    let netscope = addon(&db, &api, "netscope", &[CATALOG_WRITE]).await;
    let by_hand = testing::asset(&db, brightfield, "Laptop", "PC-0001").await;
    let (_, theirs) = netscope
        .call(
            "POST",
            "/api/addon/assets",
            Some(
                json!({"reference": "n-7", "customer_id": brightfield, "kind": "Laptop",
                "name": "PC-0007"}),
            ),
        )
        .await;
    let theirs = theirs["id"].as_str().unwrap().to_owned();
    let (_, mine) = watchtower
        .call(
            "POST",
            "/api/addon/assets",
            Some(
                json!({"reference": "host-1", "customer_id": brightfield, "kind": "Laptop",
                "name": "PC-0042"}),
            ),
        )
        .await;
    let mine = mine["id"].as_str().unwrap().to_owned();

    let holder = "An add-on does not give an asset a holder: agents choose the requester it is \
                  given to.";
    let new = |extra: Value| {
        let mut asset = json!({"reference": "host-1", "customer_id": brightfield,
            "kind": "Laptop", "name": "PC-0042"});
        asset
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        asset
    };
    for (method, uri, body, sentence) in [
        (
            "POST",
            "/api/addon/assets".to_owned(),
            new(json!({"holder_id": ada})),
            holder,
        ),
        (
            "POST",
            "/api/addon/assets".to_owned(),
            new(json!({"holder": "ada@brightfield.test"})),
            holder,
        ),
        (
            "PUT",
            format!("/api/addon/assets/{mine}"),
            json!({"kind": "Laptop", "name": "PC-0042", "holder_id": ada}),
            holder,
        ),
        (
            "PUT",
            format!("/api/addon/assets/{by_hand}"),
            json!({"kind": "Laptop", "name": "PC-0001"}),
            "PC-0001 was entered by hand: an add-on changes only the assets it is the source of.",
        ),
        (
            "PUT",
            format!("/api/addon/assets/{theirs}"),
            json!({"kind": "Laptop", "name": "PC-0007"}),
            "PC-0007 comes from the add-on netscope: an add-on changes only the assets it is the \
             source of.",
        ),
        (
            "POST",
            "/api/addon/assets".to_owned(),
            new(json!({"kind": "Tablet"})),
            "The help desk has no asset kind called Tablet: an add-on maps its own kinds to the \
             help desk's list.",
        ),
        (
            "POST",
            "/api/addon/assets".to_owned(),
            new(json!({"site": "Quay office"})),
            "Brightfield has no site called Quay office: an asset is put only at one of its \
             customer's active sites.",
        ),
    ] {
        let (status, json) = watchtower.call(method, &uri, Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {json}");
        assert_eq!(json["error"], sentence, "{method} {uri}");
    }
    for (asset, sentence) in [
        (
            by_hand.to_string(),
            "PC-0001 was entered by hand: an add-on changes only the assets it is the source of.",
        ),
        (
            theirs,
            "PC-0007 comes from the add-on netscope: an add-on changes only the assets it is the \
             source of.",
        ),
    ] {
        let (status, json) = watchtower
            .call("POST", &format!("/api/addon/assets/{asset}/retire"), None)
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{json}");
        assert_eq!(json["error"], sentence);
    }
    let (status, _) = watchtower
        .call(
            "POST",
            &format!("/api/addon/assets/{}/retire", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let shown = api
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    let names: Vec<&str> = shown
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["PC-0001", "PC-0007", "PC-0042"], "nothing changed");
    assert!(
        shown
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["holder"].is_null() && a["retired_at"].is_null())
    );
}

#[sqlx::test]
async fn agents_set_only_the_holder_of_a_supplied_asset_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let ada = testing::requester(&db, brightfield, "ada@brightfield.test").await;
    let laptop = testing::asset_kind(&db, "Laptop").await;
    let watchtower = addon(&db, &api, "watchtower", &[CATALOG_WRITE]).await;
    let (_, pc) = watchtower
        .call(
            "POST",
            "/api/addon/assets",
            Some(
                json!({"reference": "host-1", "customer_id": brightfield, "kind": "Laptop",
                "name": "PC-0042", "serial_number": "SN-0042"}),
            ),
        )
        .await;
    let pc = pc["id"].as_str().unwrap().to_owned();

    let (status, json) = api
        .call(
            "PUT",
            &format!("/api/assets/{pc}"),
            Some(
                json!({"kind_id": laptop, "name": "PC-0042", "serial_number": "SN-0042",
                "holder_id": ada}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let (status, json) = api
        .call(
            "PUT",
            &format!("/api/assets/{pc}"),
            Some(
                json!({"kind_id": laptop, "name": "Ada's laptop", "serial_number": "SN-0042",
                "holder_id": ada}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "PC-0042 comes from the add-on watchtower: only its source changes its kind, name, \
         serial number and site, while agents give it a holder."
    );
    let (status, json) = api
        .call("POST", &format!("/api/assets/{pc}/retire"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    let shown = api
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    assert_eq!(
        (&shown[0]["name"], &shown[0]["holder"]["id"]),
        (&json!("PC-0042"), &json!(ada))
    );
}
