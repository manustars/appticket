//! The names of an add-on's system, which the add-on lists with its token and agents who manage
//! customers pick external references from.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{Client, addon, role_with};
use crate::addons::{DIRECTORY_READ, NAMES_WRITE};
use crate::roles::Permission;
use crate::testing;

fn watchtower_groups() -> Value {
    json!({"system": "Watchtower", "names": [
        {"name": "Harbor Lane"},
        {"name": "Brightfield/Offices", "parent": "Brightfield"},
        {"name": "Brightfield"},
        {"name": "Brightfield/Offices/Floor 2", "parent": "Brightfield/Offices"}
    ]})
}

#[sqlx::test]
async fn an_addon_lists_its_names_and_agents_pick_references_from_them(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let watchtower = addon(&db, &api, "watchtower", &[NAMES_WRITE, DIRECTORY_READ]).await;

    let (status, json) = watchtower
        .call("PUT", "/api/addon/names", Some(watchtower_groups()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let lists = api.get("/api/external-references/names").await;
    assert_eq!(lists.as_array().unwrap().len(), 1, "{lists}");
    assert_eq!(
        (&lists[0]["system"], &lists[0]["addon"]),
        (&json!("Watchtower"), &json!("watchtower"))
    );
    assert!(lists[0]["updated_at"].is_string());
    assert_eq!(
        lists[0]["names"],
        json!([
            {"name": "Brightfield", "parent": null, "customer": null},
            {"name": "Brightfield/Offices", "parent": "Brightfield", "customer": null},
            {"name": "Brightfield/Offices/Floor 2", "parent": "Brightfield/Offices",
             "customer": null},
            {"name": "Harbor Lane", "parent": null, "customer": null}
        ])
    );

    // Picked from the list, spelled as the list spells it.
    let pick = |customer: uuid::Uuid, system: &str, name: &str| {
        (
            format!("/api/customers/{customer}/external-references"),
            json!({"system": system, "name": name}),
        )
    };
    let (uri, body) = pick(brightfield, "watchtower", "brightfield");
    let (status, json) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
    let lists = api.get("/api/external-references/names").await;
    assert_eq!(
        lists[0]["names"][0]["customer"],
        json!({"id": brightfield, "name": "Brightfield", "is_help_desk": false})
    );

    // A name another customer has cannot be picked, nor one the add-on does not list.
    let (uri, body) = pick(harbor, "Watchtower", "Brightfield");
    let (status, json) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "Brightfield in Watchtower already names Brightfield."
    );
    let (uri, body) = pick(harbor, "Watchtower", "Harbor Lane/Docks");
    let (status, json) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        r#"The name "Harbor Lane/Docks" is not in the name list the add-on watchtower sends of Watchtower."#
    );

    // Switched off, the add-on's latest list is still there to pick from.
    crate::addons::set_enabled(&db, "watchtower", false)
        .await
        .unwrap();
    let (uri, body) = pick(harbor, "Watchtower", "Harbor Lane");
    let (status, json) = api.call("POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
    crate::addons::set_enabled(&db, "watchtower", true)
        .await
        .unwrap();

    let customers = watchtower.get("/api/addon/customers").await;
    let references: Vec<(&Value, &Value)> = customers
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (&c["name"], &c["external_references"]))
        .collect();
    assert_eq!(
        references,
        [
            (
                &json!("Brightfield"),
                &json!([{"system": "Watchtower", "name": "Brightfield"}])
            ),
            (
                &json!("Harbor Lane"),
                &json!([{"system": "Watchtower", "name": "Harbor Lane"}])
            ),
        ]
    );
}

/// External references typed by hand before stay, and add-ons still read them, when the add-on's
/// list does not name them.
#[sqlx::test]
async fn references_typed_by_hand_stay_and_are_still_listed_to_addons(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let (status, json) = api
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/external-references"),
            Some(json!({"system": "Watchtower", "name": "BF old group"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
    let watchtower = addon(&db, &api, "watchtower", &[NAMES_WRITE, DIRECTORY_READ]).await;
    let (status, json) = watchtower
        .call("PUT", "/api/addon/names", Some(watchtower_groups()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");

    let page = api.get(&format!("/api/customers/{brightfield}")).await;
    assert_eq!(
        page["external_references"][0]["name"], "BF old group",
        "{page}"
    );
    let customers = watchtower.get("/api/addon/customers").await;
    assert_eq!(
        customers[0]["external_references"],
        json!([{"system": "Watchtower", "name": "BF old group"}])
    );
}

#[sqlx::test]
async fn a_list_that_does_not_fit_is_refused_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let watchtower = addon(&db, &api, "watchtower", &[NAMES_WRITE]).await;
    let netscope = addon(&db, &api, "netscope", &[NAMES_WRITE]).await;
    let (status, _) = watchtower
        .call("PUT", "/api/addon/names", Some(watchtower_groups()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, json) = watchtower
        .call(
            "PUT",
            "/api/addon/names",
            Some(json!({"system": "Watchtower", "names": [
                {"name": "Brightfield/Lab", "parent": "Brightfield"}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        r#"The name "Brightfield/Lab" is nested in "Brightfield", which the list does not name."#
    );
    let (status, json) = netscope
        .call(
            "PUT",
            "/api/addon/names",
            Some(json!({"system": "watchtower", "names": []})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "The add-on watchtower lists the names of Watchtower: a system's names come from one \
         add-on."
    );
    let lists = api.get("/api/external-references/names").await;
    assert_eq!(
        lists[0]["names"].as_array().unwrap().len(),
        4,
        "the list before stays"
    );

    // Names the database compares otherwise than the core are refused with a sentence too.
    let (status, json) = watchtower
        .call(
            "PUT",
            "/api/addon/names",
            Some(json!({"system": "Watchtower", "names": [
                {"name": "İstanbul"}, {"name": "istanbul"}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        r#"The list names "istanbul" twice, once as "İstanbul"."#
    );
}

/// The largest list the core takes fits in one call: as many names as a list holds, each as long
/// as a name can be in characters of 4 bytes, nested in another as long; a body over the limit
/// is refused.
#[sqlx::test]
async fn the_largest_list_fits_in_one_call_and_a_larger_body_is_refused(db: PgPool) {
    use crate::http::addon_names::BODY_LIMIT;
    use crate::name_lists::{MAX_NAME_LENGTH, MAX_NAMES, MAX_SYSTEM_LENGTH};
    let api = Client::new(&db).await;
    let watchtower = addon(&db, &api, "watchtower", &[NAMES_WRITE]).await;
    // Three characters of 64 tell the names apart, the rest fills them up.
    let name = |i: usize| -> String {
        let digit = |d: usize| char::from_u32(0x1F300 + d as u32).unwrap();
        let mut name: String = [i / 4096, i / 64 % 64, i % 64]
            .into_iter()
            .map(digit)
            .collect();
        name.extend(std::iter::repeat_n('\u{1F600}', MAX_NAME_LENGTH - 3));
        name
    };
    let names: Vec<Value> = (0..MAX_NAMES)
        .map(|i| match i {
            0 => json!({"name": name(i)}),
            _ => json!({"name": name(i), "parent": name(i - 1)}),
        })
        .collect();
    let largest = json!({"system": "\u{1D54E}".repeat(MAX_SYSTEM_LENGTH), "names": names});
    let size = largest.to_string().len();
    assert!(
        size > BODY_LIMIT * 9 / 10 && size <= BODY_LIMIT,
        "{size} of {BODY_LIMIT}"
    );
    let (status, json) = watchtower
        .call("PUT", "/api/addon/names", Some(largest))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");

    let wrapper = json!({"system": "Watchtower", "names": [{"name": ""}]}).to_string();
    let over = json!({"system": "Watchtower", "names": [
        {"name": "x".repeat(BODY_LIMIT + 1 - wrapper.len())}
    ]});
    assert_eq!(over.to_string().len(), BODY_LIMIT + 1);
    let (status, _) = watchtower.call("PUT", "/api/addon/names", Some(over)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let lists = api.get("/api/external-references/names").await;
    assert_eq!(lists[0]["names"].as_array().unwrap().len(), MAX_NAMES);
}

#[sqlx::test]
async fn listing_names_needs_the_scope_names_write(db: PgPool) {
    let api = Client::new(&db).await;
    let reader = addon(&db, &api, "reader", &[DIRECTORY_READ]).await;

    let (status, json) = reader
        .call("PUT", "/api/addon/names", Some(watchtower_groups()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "The add-on reader lacks the scope names:write."
    );
    assert_eq!(
        api.get("/api/external-references/names").await,
        json!([]),
        "nothing listed"
    );
}

/// Every agent reads the lists; picking from them, as adding any external reference, needs
/// managing customers.
#[sqlx::test]
async fn picking_a_listed_name_needs_managing_customers(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let watchtower = addon(&db, &api, "watchtower", &[NAMES_WRITE]).await;
    watchtower
        .call("PUT", "/api/addon/names", Some(watchtower_groups()))
        .await;
    let tickets_only = role_with(&db, "Tickets", &[Permission::Reply]).await;
    let agent = Client::with_role(&db, "tina@northgate.test", tickets_only).await;

    let lists = agent.get("/api/external-references/names").await;
    assert_eq!(lists[0]["names"].as_array().unwrap().len(), 4);
    let (status, json) = agent
        .call(
            "POST",
            &format!("/api/customers/{brightfield}/external-references"),
            Some(json!({"system": "Watchtower", "name": "Brightfield"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
    let page = api.get(&format!("/api/customers/{brightfield}")).await;
    assert_eq!(page["external_references"], json!([]));
}

/// The SDK's call reaches the core: what it sends is what the core takes.
#[sqlx::test]
async fn an_addon_built_on_the_sdk_lists_its_names(db: PgPool) {
    use hd_addon_sdk::names::{SystemName, SystemNames};
    let api = Client::new(&db).await;
    let registered = crate::addons::register(
        &db,
        "watchtower",
        "http://127.0.0.1:9",
        &[],
        &[NAMES_WRITE.into()],
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::http::router(crate::http::AppState::for_tests(&db));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });

    let sdk = hd_addon_sdk::Client::new(&url, &registered.token).unwrap();
    sdk.replace_names(&SystemNames {
        system: "Watchtower".into(),
        names: vec![
            SystemName::nested_in("Brightfield/Offices", "Brightfield"),
            SystemName::top("Brightfield"),
        ],
    })
    .await
    .unwrap();
    let lists = api.get("/api/external-references/names").await;
    assert_eq!(
        lists[0]["names"],
        json!([
            {"name": "Brightfield", "parent": null, "customer": null},
            {"name": "Brightfield/Offices", "parent": "Brightfield", "customer": null}
        ])
    );
}

/// The add-on contract gives the limits the core keeps.
#[test]
fn the_addon_contract_gives_the_limits_of_a_name_list() {
    use crate::http::addon_names::BODY_LIMIT;
    use crate::name_lists::{MAX_NAME_LENGTH, MAX_NAMES, MAX_SYSTEM_LENGTH};
    let contract = include_str!("../../../../sdk/README.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for said in [
        format!("a name is at most {MAX_NAME_LENGTH} characters"),
        format!("the system's name at most {MAX_SYSTEM_LENGTH},"),
        format!(
            "at most {MAX_NAMES} names, in at most {} MiB of JSON",
            BODY_LIMIT / (1024 * 1024)
        ),
    ] {
        assert!(
            contract.contains(&said),
            "sdk/README.md does not say {said:?}"
        );
    }
}
