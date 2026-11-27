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
        ("POST", format!("/api/addon/assets/{pc}/unlink"), None),
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
    assert_eq!(
        api.get(&format!(
            "/api/customers/{brightfield}/asset-link-proposals"
        ))
        .await,
        json!([]),
        "nor proposed, though the name matches"
    );
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

/// An add-on that would create an asset its customer already has gets a link proposal instead;
/// every agent sees it on the customer's assets, and only agents who manage the catalog discard
/// it, after which the add-on creates its own asset.
#[sqlx::test]
async fn a_matching_asset_is_proposed_as_a_link_and_discarded_through_the_api(db: PgPool) {
    use crate::roles::Permission;
    let cataloguer = super::role_with(&db, "Cataloguer", &[Permission::ManageCatalog]).await;
    let cataloguer = Client::with_role(&db, "carla@northgate.test", cataloguer).await;
    let manager = super::role_with(&db, "Manager", &[Permission::ManageCustomers]).await;
    let manager = Client::with_role(&db, "mara@northgate.test", manager).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let laptop = testing::asset_kind(&db, "Laptop").await;
    let by_hand = crate::catalog::create_asset(
        &db,
        brightfield,
        &testing::asset_details(laptop, "PC-0042", Some("SN-0042"), None, None),
    )
    .await
    .unwrap();
    let watchtower = addon(&db, &cataloguer, "watchtower", &[CATALOG_WRITE]).await;
    let asset = json!({"reference": "host-1042", "customer_id": brightfield, "kind": "Laptop",
        "name": "Ada's laptop", "serial_number": "sn-0042"});

    let (status, proposed) = watchtower
        .call("POST", "/api/addon/assets", Some(asset.clone()))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{proposed}");
    assert_eq!(
        (
            &proposed["matched_by"],
            &proposed["asset_id"],
            &proposed["asset_name"],
            &proposed["reference"],
            &proposed["name"],
            &proposed["serial_number"],
        ),
        (
            &json!("serial_number"),
            &json!(by_hand),
            &json!("PC-0042"),
            &json!("host-1042"),
            &json!("Ada's laptop"),
            &json!("sn-0042"),
        ),
        "the answer says which asset matched"
    );
    let (status, again) = watchtower
        .call("POST", "/api/addon/assets", Some(asset.clone()))
        .await;
    assert_eq!(
        (status, &again["id"]),
        (StatusCode::ACCEPTED, &proposed["id"]),
        "no second proposal while one is open"
    );

    let proposals = format!("/api/customers/{brightfield}/asset-link-proposals");
    for agent in [&manager, &cataloguer] {
        let open = agent.get(&proposals).await;
        assert_eq!(
            (
                open.as_array().unwrap().len(),
                &open[0]["id"],
                &open[0]["addon"]
            ),
            (1, &proposed["id"], &json!("watchtower")),
            "{open}"
        );
    }
    let discard = format!(
        "/api/asset-link-proposals/{}/discard",
        proposed["id"].as_str().unwrap()
    );
    let (status, json) = manager.call("POST", &discard, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
    assert_eq!(manager.get(&proposals).await.as_array().unwrap().len(), 1);
    let (status, json) = cataloguer.call("POST", &discard, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let (status, json) = cataloguer.call("POST", &discard, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"], "The link proposal is no longer open.");
    let (status, _) = cataloguer
        .call(
            "POST",
            &format!("/api/asset-link-proposals/{}/discard", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(cataloguer.get(&proposals).await, json!([]));

    let (status, created) = watchtower
        .call("POST", "/api/addon/assets", Some(asset))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["reference"], "host-1042");
    let shown = cataloguer
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    assert_eq!(shown.as_array().unwrap().len(), 2, "{shown}");
}

/// Agents who manage the catalog accept a link proposal; the add-on's reference then reaches the
/// existing asset, which it changes nothing of, until it unlinks itself.
#[sqlx::test]
async fn a_link_proposal_is_accepted_and_the_linked_addon_unlinks_itself_through_the_api(
    db: PgPool,
) {
    use crate::roles::Permission;
    let cataloguer = super::role_with(&db, "Cataloguer", &[Permission::ManageCatalog]).await;
    let cataloguer = Client::with_role(&db, "carla@northgate.test", cataloguer).await;
    let manager = super::role_with(&db, "Manager", &[Permission::ManageCustomers]).await;
    let manager = Client::with_role(&db, "mara@northgate.test", manager).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    testing::asset_kind(&db, "Laptop").await;
    let netscope = addon(&db, &cataloguer, "netscope", &[CATALOG_WRITE]).await;
    let watchtower = addon(&db, &cataloguer, "watchtower", &[CATALOG_WRITE]).await;
    let reader = addon(&db, &cataloguer, "reader", &[DIRECTORY_READ]).await;
    let (_, srv) = netscope
        .call(
            "POST",
            "/api/addon/assets",
            Some(
                json!({"reference": "n-1", "customer_id": brightfield, "kind": "Laptop",
                "name": "SRV-01", "serial_number": "SN-1"}),
            ),
        )
        .await;
    let srv = srv["id"].as_str().unwrap().to_owned();
    let same = json!({"reference": "host-1042", "customer_id": brightfield, "kind": "Laptop",
        "name": "SRV-01", "serial_number": "SN-1"});
    let (status, proposed) = watchtower
        .call("POST", "/api/addon/assets", Some(same.clone()))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{proposed}");
    let accept = format!(
        "/api/asset-link-proposals/{}/accept",
        proposed["id"].as_str().unwrap()
    );

    let (status, json) = manager.call("POST", &accept, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
    let (status, json) = cataloguer.call("POST", &accept, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let (status, json) = cataloguer.call("POST", &accept, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"], "The link proposal is no longer open.");
    let (status, _) = cataloguer
        .call(
            "POST",
            &format!("/api/asset-link-proposals/{}/accept", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let shown = manager
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    assert_eq!(shown.as_array().unwrap().len(), 1, "{shown}");
    assert_eq!(
        (&shown[0]["source_addon"], &shown[0]["linked_addons"]),
        (
            &json!("netscope"),
            &json!([{"addon": "watchtower", "reference": "host-1042"}])
        ),
        "agents see its source and its linked add-ons"
    );

    // The linked add-on's reference reaches the asset; it reads it with its own reference, and
    // others read only which add-ons are linked.
    let (status, reached) = watchtower
        .call("POST", "/api/addon/assets", Some(same))
        .await;
    assert_eq!(status, StatusCode::OK, "{reached}");
    assert_eq!(
        (
            &reached["id"],
            &reached["source_addon"],
            &reached["reference"],
            &reached["linked_addons"]
        ),
        (
            &json!(srv),
            &json!("netscope"),
            &json!("host-1042"),
            &json!(["watchtower"])
        )
    );
    let seen = reader
        .get(&format!("/api/addon/customers/{brightfield}/assets"))
        .await;
    assert_eq!(
        (&seen[0]["reference"], &seen[0]["linked_addons"]),
        (&Value::Null, &json!(["watchtower"]))
    );
    let refused = "SRV-01 comes from the add-on netscope: watchtower is linked to it, and an \
                   add-on changes only the assets it is the source of.";
    for (method, uri, body) in [
        (
            "POST",
            "/api/addon/assets".to_owned(),
            json!({"reference": "host-1042", "customer_id": brightfield, "kind": "Laptop",
                "name": "srv-01.example.test", "serial_number": "SN-1"}),
        ),
        (
            "PUT",
            format!("/api/addon/assets/{srv}"),
            json!({"kind": "Laptop", "name": "SRV-01"}),
        ),
    ] {
        let (status, json) = watchtower.call(method, &uri, Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {json}");
        assert_eq!(json["error"], refused, "{method} {uri}");
    }
    let (status, json) = watchtower
        .call("POST", &format!("/api/addon/assets/{srv}/retire"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "Watchtower is linked to SRV-01: an add-on linked to an asset unlinks itself from it, \
         and never retires it."
    );
    let (status, json) = netscope
        .call("POST", &format!("/api/addon/assets/{srv}/unlink"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    let (status, _) = watchtower
        .call(
            "POST",
            &format!("/api/addon/assets/{}/unlink", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    for _ in 0..2 {
        let (status, json) = watchtower
            .call("POST", &format!("/api/addon/assets/{srv}/unlink"), None)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    }
    let shown = cataloguer
        .get(&format!("/api/customers/{brightfield}/assets"))
        .await;
    assert_eq!(
        (
            &shown[0]["name"],
            &shown[0]["source_addon"],
            &shown[0]["linked_addons"],
            &shown[0]["retired_at"]
        ),
        (
            &json!("SRV-01"),
            &json!("netscope"),
            &json!([]),
            &Value::Null
        ),
        "the asset stays as it was"
    );
}
