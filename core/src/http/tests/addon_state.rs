//! An add-on's state, where those who manage add-ons see them: the names of its system no external
//! reference covers, and the conflicts the add-on reports with its token.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{Client, addon, role_with};
use crate::addons::{CONFLICTS_WRITE, NAMES_WRITE};
use crate::roles::Permission;
use crate::testing;

/// The add-on called `name` as those who manage add-ons read it.
async fn state(api: &Client, name: &str) -> Value {
    let addons = api.get("/api/admin/addons").await;
    addons
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("no add-on {name} in {addons}"))
        .clone()
}

#[sqlx::test]
async fn those_who_manage_addons_read_the_names_no_external_reference_covers(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let watchtower = addon(&db, &api, "watchtower", &[NAMES_WRITE]).await;
    assert_eq!(state(&api, "watchtower").await["names"], Value::Null);

    let (status, json) = watchtower
        .call(
            "PUT",
            "/api/addon/names",
            Some(json!({"system": "Watchtower", "names": [
                {"name": "Brightfield"},
                {"name": "Brightfield/Offices", "parent": "Brightfield"},
                {"name": "Brightfield/Docks", "parent": "Brightfield"},
                {"name": "Brightfield/Docks/Crane", "parent": "Brightfield/Docks"},
                {"name": "Unclaimed"},
                {"name": "Unclaimed/Lab", "parent": "Unclaimed"}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let names = &state(&api, "watchtower").await["names"];
    assert_eq!(names["system"], "Watchtower");
    assert!(names["updated_at"].is_string(), "{names}");
    assert_eq!(names["name_count"], 6);
    assert_eq!(
        names["uncovered"],
        json!([
            {"name": "Brightfield", "parent": null, "depth": 0},
            {"name": "Brightfield/Docks", "parent": "Brightfield", "depth": 1},
            {"name": "Brightfield/Docks/Crane", "parent": "Brightfield/Docks", "depth": 2},
            {"name": "Brightfield/Offices", "parent": "Brightfield", "depth": 1},
            {"name": "Unclaimed", "parent": null, "depth": 0},
            {"name": "Unclaimed/Lab", "parent": "Unclaimed", "depth": 1}
        ])
    );

    // A name covers those nested in it, except a subgroup another customer has, with its own.
    for (customer, name) in [(harbor, "Brightfield/Docks"), (brightfield, "Brightfield")] {
        let (status, json) = api
            .call(
                "POST",
                &format!("/api/customers/{customer}/external-references"),
                Some(json!({"system": "Watchtower", "name": name})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{json}");
    }
    assert_eq!(
        state(&api, "watchtower").await["names"]["uncovered"],
        json!([
            {"name": "Unclaimed", "parent": null, "depth": 0},
            {"name": "Unclaimed/Lab", "parent": "Unclaimed", "depth": 1}
        ])
    );
}

#[sqlx::test]
async fn an_addon_reports_its_conflicts_and_those_who_manage_addons_read_the_latest(db: PgPool) {
    let api = Client::new(&db).await;
    let watchtower = addon(&db, &api, "watchtower", &[CONFLICTS_WRITE]).await;
    assert_eq!(state(&api, "watchtower").await["conflicts"], Value::Null);

    let (status, json) = watchtower
        .call(
            "PUT",
            "/api/addon/conflicts",
            Some(json!({"conflicts": [
                {"sentence": {
                    "en": "The host db-03 is in Brightfield and Harbor Lane, which two customers \
                           have: it is not imported.",
                    "it": "L'host db-03 è in Brightfield e Harbor Lane, che due clienti hanno: \
                           non è importato."
                }},
                {"sentence": "The host web-07 is in no group a customer has: it is not imported."}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let conflicts = &state(&api, "watchtower").await["conflicts"];
    assert!(conflicts["updated_at"].is_string(), "{conflicts}");
    assert_eq!(
        conflicts["conflicts"],
        json!([
            {"sentence": {
                "en": "The host db-03 is in Brightfield and Harbor Lane, which two customers \
                       have: it is not imported.",
                "it": "L'host db-03 è in Brightfield e Harbor Lane, che due clienti hanno: \
                       non è importato."
            }},
            {"sentence": {
                "en": "The host web-07 is in no group a customer has: it is not imported."
            }}
        ])
    );

    // A list that does not fit is refused whole, with a sentence, and the one before stays.
    let (status, json) = watchtower
        .call(
            "PUT",
            "/api/addon/conflicts",
            Some(json!({"conflicts": [{"sentence": "Fine."}, {"sentence": " "}]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"], "Conflict number 2 has no sentence.");
    assert_eq!(
        state(&api, "watchtower").await["conflicts"]["conflicts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // An empty list says there are none.
    let (status, json) = watchtower
        .call(
            "PUT",
            "/api/addon/conflicts",
            Some(json!({"conflicts": []})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    assert_eq!(
        state(&api, "watchtower").await["conflicts"]["conflicts"],
        json!([])
    );
}

#[sqlx::test]
async fn reporting_conflicts_needs_the_scope_conflicts_write(db: PgPool) {
    let api = Client::new(&db).await;
    let lister = addon(&db, &api, "lister", &[NAMES_WRITE]).await;

    let (status, json) = lister
        .call(
            "PUT",
            "/api/addon/conflicts",
            Some(json!({"conflicts": [{"sentence": "The host db-03 is in two groups."}]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(
        json["error"],
        "The add-on lister lacks the scope conflicts:write."
    );
    assert_eq!(state(&api, "lister").await["conflicts"], Value::Null);
}

/// Only those who manage add-ons read their state.
#[sqlx::test]
async fn reading_an_addon_s_state_needs_managing_addons(db: PgPool) {
    let api = Client::new(&db).await;
    addon(&db, &api, "watchtower", &[CONFLICTS_WRITE]).await;
    let customers_only = role_with(&db, "Customers", &[Permission::ManageCustomers]).await;
    let agent = Client::with_role(&db, "carl@northgate.test", customers_only).await;

    let (status, json) = agent.call("GET", "/api/admin/addons", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
}

/// The largest list fits in one call, each sentence in as many languages as a conflict is
/// written in and as long as it can be in characters of 4 bytes; a body over the limit is
/// refused.
#[sqlx::test]
async fn the_largest_list_of_conflicts_fits_in_one_call_and_a_larger_body_is_refused(db: PgPool) {
    use crate::conflicts::{MAX_CONFLICTS, MAX_LANGUAGES, MAX_SENTENCE_LENGTH};
    use crate::http::addon_conflicts::BODY_LIMIT;
    let api = Client::new(&db).await;
    let watchtower = addon(&db, &api, "watchtower", &[CONFLICTS_WRITE]).await;
    let longest = "\u{1F600}".repeat(MAX_SENTENCE_LENGTH);
    let sentence: serde_json::Map<String, Value> = ["en", "it", "de", "fr"]
        .into_iter()
        .take(MAX_LANGUAGES)
        .map(|l| (l.to_owned(), json!(longest)))
        .collect();
    let largest = json!({"conflicts": vec![json!({"sentence": sentence}); MAX_CONFLICTS]});
    let size = largest.to_string().len();
    assert!(
        size > BODY_LIMIT * 9 / 10 && size <= BODY_LIMIT,
        "{size} of {BODY_LIMIT}"
    );
    let (status, json) = watchtower
        .call("PUT", "/api/addon/conflicts", Some(largest))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");

    let wrapper = json!({"conflicts": [{"sentence": ""}]}).to_string();
    let over = json!({"conflicts": [{"sentence": "x".repeat(BODY_LIMIT + 1 - wrapper.len())}]});
    assert_eq!(over.to_string().len(), BODY_LIMIT + 1);
    let (status, _) = watchtower
        .call("PUT", "/api/addon/conflicts", Some(over))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        state(&api, "watchtower").await["conflicts"]["conflicts"]
            .as_array()
            .unwrap()
            .len(),
        MAX_CONFLICTS
    );
}

/// The SDK's call reaches the core: what it sends is what the core takes.
#[sqlx::test]
async fn an_addon_built_on_the_sdk_reports_its_conflicts(db: PgPool) {
    use hd_addon_sdk::conflicts::Conflict;
    let api = Client::new(&db).await;
    let registered = crate::addons::register(
        &db,
        "watchtower",
        "http://127.0.0.1:9",
        &[],
        &[CONFLICTS_WRITE.into()],
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
    sdk.replace_conflicts(&[
        Conflict::in_english("The host db-03 is in two groups: it is not imported."),
        Conflict::new([
            ("en", "The host web-07 is in no group a customer has."),
            ("it", "L'host web-07 non è in alcun gruppo di un cliente."),
        ]),
    ])
    .await
    .unwrap();
    assert_eq!(
        state(&api, "watchtower").await["conflicts"]["conflicts"],
        json!([
            {"sentence": {"en": "The host db-03 is in two groups: it is not imported."}},
            {"sentence": {
                "en": "The host web-07 is in no group a customer has.",
                "it": "L'host web-07 non è in alcun gruppo di un cliente."
            }}
        ])
    );
}

/// The add-on contract gives the limits the core keeps.
#[test]
fn the_addon_contract_gives_the_limits_of_a_list_of_conflicts() {
    use crate::conflicts::{MAX_CONFLICTS, MAX_LANGUAGES, MAX_SENTENCE_LENGTH};
    use crate::http::addon_conflicts::BODY_LIMIT;
    let contract = include_str!("../../../../sdk/README.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for said in [
        format!(
            "at most {MAX_SENTENCE_LENGTH} characters in each of at most {MAX_LANGUAGES} languages"
        ),
        format!(
            "at most {MAX_CONFLICTS} conflicts, in at most {} MiB of JSON",
            BODY_LIMIT / (1024 * 1024)
        ),
    ] {
        assert!(
            contract.contains(&said),
            "sdk/README.md does not say {said:?}"
        );
    }
}
