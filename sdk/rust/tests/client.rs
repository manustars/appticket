//! The client against a small fake of the core's add-on API on a local port.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use hd_addon_sdk::api::{Direction, Priority, TicketState, TimelineEntry};
use hd_addon_sdk::proposals::in_english;
use hd_addon_sdk::{Client, Content, Error, NewTicket, OpenedTicket, Rationale, Target, Triage};
use serde_json::{Value, json};
use uuid::Uuid;

const TOKEN: &str = "token-of-the-test-addon";
const CUSTOMER: &str = "0e7b6a52-5d0f-4d5e-8a8e-3f2b1c0d9e8f";

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn problem(status: StatusCode, sentence: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": sentence })))
}

type Posted = Arc<Mutex<Vec<Value>>>;

async fn fake_core() -> (String, Posted) {
    let posted: Posted = Arc::default();
    let app = Router::new()
        .route(
            "/api/addon/whoami",
            get(|headers: HeaderMap| async move {
                if !authorized(&headers) {
                    return problem(StatusCode::UNAUTHORIZED, "Not signed in.");
                }
                (
                    StatusCode::OK,
                    Json(json!({"name": "assistant", "scopes": ["directory:read"],
                        "languages": ["en", "it"]})),
                )
            }),
        )
        .route(
            "/api/addon/customers",
            get(|| async {
                Json(json!([{"id": CUSTOMER, "name": "Brightfield", "is_help_desk": false,
                    "domains": ["brightfield.test"],
                    "external_references": [{"system": "Watchtower", "name": "Brightfield/Offices"}],
                    "added_later": 1}]))
            }),
        )
        .route(
            "/api/addon/customers/{id}/assets",
            get(|Path(id): Path<Uuid>| async move {
                Json(json!([{"id": "5a1d0c3e-7b2f-4e6a-9c8d-1f0e2d3c4b5a", "customer_id": id,
                    "customer_name": "Brightfield", "kind": "computer", "name": "PC-0042",
                    "source_addon": null, "retired_at": null}]))
            }),
        )
        .route(
            "/api/addon/customers/{id}/services",
            get(|| async {
                Json(json!([{"id": "8c9d0e1f-2a3b-4c5d-9e6f-7a8b9c0d1e2f", "name": "Posta ospitata",
                    "description": "Caselle gestite", "customers": 3, "retired_at": null}]))
            }),
        )
        .route(
            "/api/addon/tickets",
            get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
                if q.get("requester_email").map(String::as_str) == Some("ada+desk@brightfield.test") {
                    (
                        StatusCode::OK,
                        Json(json!([{"number": 12, "title": "Printer jams", "state": "waiting",
                            "priority": "normal", "customer_id": CUSTOMER,
                            "requester_email": "ada+desk@brightfield.test",
                            "updated_at": "2026-11-21T09:00:00Z"}])),
                    )
                } else if q.get("customer").map(String::as_str) == Some(CUSTOMER) {
                    (StatusCode::OK, Json(json!([])))
                } else {
                    problem(StatusCode::BAD_REQUEST, "Give requester_email or customer.")
                }
            }),
        )
        .route(
            "/api/addon/tickets/{number}",
            get(|Path(number): Path<i64>| async move {
                if number == 31 {
                    return (StatusCode::OK, Json(json!({"number": 31,
                        "title": "SRV-01: disk almost full", "state": "open", "priority": "high",
                        "customer": {"id": CUSTOMER, "name": "Brightfield"}, "requester": null,
                        "opened_by_addon": "watchtower", "worked_on_by_agent": false,
                        "timeline": []})));
                }
                if number != 12 {
                    return problem(StatusCode::NOT_FOUND, "Ticket not found.");
                }
                (StatusCode::OK, Json(json!({
                    "id": "9d8c7b6a-5f4e-4d3c-8b2a-1f0e9d8c7b6a", "number": 12,
                    "title": "Printer jams", "state": "open", "priority": "high",
                    "customer": {"id": CUSTOMER, "name": "Brightfield", "is_help_desk": false},
                    "requester": {"id": "1b2c3d4e-5f6a-4b7c-8d9e-0f1a2b3c4d5e",
                        "email": "ada@brightfield.test", "name": "Ada Ferrini", "ticket_count": 3,
                        "addresses": ["ada@brightfield.test", "ada@home.test"],
                        "reply_to": "ada@brightfield.test"},
                    "assignee": null, "assets": [],
                    "services": [{"id": "8c9d0e1f-2a3b-4c5d-9e6f-7a8b9c0d1e2f", "name": "Posta ospitata"}], "parent": null, "children": [],
                    "proposals": [{"id": "2c3d4e5f-6a7b-4c8d-9e0f-1a2b3c4d5e6f", "addon": "assistant",
                        "content": {"kind": "some_future_kind"}, "rationale": "",
                        "created_at": "2026-11-21T09:00:00Z", "customer_name": null, "assets": []}],
                    "created_at": "2026-11-21T09:00:00Z", "updated_at": "2026-11-21T09:05:00Z",
                    "timeline": [
                        {"kind": "message", "id": "3d4e5f6a-7b8c-4d9e-8f1a-2b3c4d5e6f7a",
                         "direction": "inbound", "from_address": "ada@brightfield.test",
                         "from_name": "Ada Ferrini", "to_address": null, "author_name": null,
                         "subject": "Printer jams", "body_text": "Paper stuck again.",
                         "created_at": "2026-11-21T09:00:00Z", "sent_at": null,
                         "last_error": null, "gave_up": false},
                        {"kind": "note", "id": "4e5f6a7b-8c9d-4e0f-9a2b-3c4d5e6f7a8b",
                         "author_name": "Marco Lenti", "body": "Called.",
                         "created_at": "2026-11-21T09:02:00Z"},
                        {"kind": "call", "id": "5f6a7b8c-9d0e-4f1a-8b3c-4d5e6f7a8b9c"}
                    ]
                })))
            }),
        )
        .route(
            "/api/addon/proposals",
            post({
                let posted = posted.clone();
                move |Json(body): Json<Value>| async move {
                    if body["ticket_number"] == json!(99) {
                        return problem(StatusCode::CONFLICT, "The ticket is closed.");
                    }
                    posted.lock().unwrap().push(body);
                    (
                        StatusCode::CREATED,
                        Json(json!({"id": "6a7b8c9d-0e1f-4a2b-9c4d-5e6f7a8b9c0d"})),
                    )
                }
            }),
        )
        .route(
            "/api/addon/tickets",
            post({
                let posted = posted.clone();
                move |Json(body): Json<Value>| async move {
                    if body["title"] == json!("") {
                        return problem(StatusCode::CONFLICT, "A ticket needs a title.");
                    }
                    let again = body["reference"] == json!("4711");
                    posted.lock().unwrap().push(body);
                    if again {
                        return (StatusCode::OK, Json(json!({"number": 31})));
                    }
                    (StatusCode::CREATED, Json(json!({"number": 31})))
                }
            }),
        )
        .route(
            "/api/addon/tickets/{number}/notes",
            post({
                let posted = posted.clone();
                move |Path(number): Path<i64>, Json(body): Json<Value>| async move {
                    if number != 31 {
                        return Err(problem(StatusCode::NOT_FOUND, "Ticket not found."));
                    }
                    posted
                        .lock()
                        .unwrap()
                        .push(json!({"ticket": number, "note": body}));
                    Ok(StatusCode::CREATED)
                }
            }),
        )
        .route(
            "/api/addon/tickets/{number}/resolve",
            post({
                let posted = posted.clone();
                move |Path(number): Path<i64>| async move {
                    if number != 31 {
                        return Err(problem(
                            StatusCode::CONFLICT,
                            "The add-on watchtower did not open the ticket #12: an add-on \
                             resolves only the tickets it opened.",
                        ));
                    }
                    posted.lock().unwrap().push(json!({"resolved": number}));
                    Ok(StatusCode::NO_CONTENT)
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, posted)
}

#[tokio::test]
async fn reads_what_an_addon_needs_and_ignores_the_rest() {
    let (url, _) = fake_core().await;
    let client = Client::new(&format!("{url}/"), TOKEN).unwrap();

    let me = client.whoami().await.unwrap();
    assert_eq!(
        (me.name.as_str(), me.languages),
        ("assistant", vec!["en".into(), "it".into()])
    );
    let customers = client.customers().await.unwrap();
    assert_eq!(customers[0].domains, ["brightfield.test"]);
    let reference = &customers[0].external_references[0];
    assert_eq!(
        (reference.system.as_str(), reference.name.as_str()),
        ("Watchtower", "Brightfield/Offices")
    );
    let customer = customers[0].id;
    assert_eq!(
        client.customer_assets(customer).await.unwrap()[0].name,
        "PC-0042"
    );
    assert_eq!(
        client.customer_services(customer).await.unwrap()[0].name,
        "Posta ospitata"
    );

    let mine = client
        .open_tickets_of_requester("ada+desk@brightfield.test")
        .await
        .unwrap();
    assert_eq!((mine[0].number, mine[0].state), (12, TicketState::Waiting));
    assert!(
        client
            .open_tickets_of_customer(customer)
            .await
            .unwrap()
            .is_empty()
    );

    let ticket = client.ticket(12).await.unwrap();
    assert_eq!(ticket.priority, Priority::High);
    assert_eq!(ticket.opened_by_addon, None, "from a core that predates it");
    assert!(
        ticket.worked_on_by_agent,
        "a core that does not say counts as worked on"
    );
    assert_eq!(ticket.services[0].name, "Posta ospitata");
    let requester = ticket.requester.unwrap();
    assert_eq!(requester.email, "ada@brightfield.test");
    assert_eq!(
        requester.addresses,
        ["ada@brightfield.test", "ada@home.test"]
    );
    assert_eq!(requester.reply_to.as_deref(), Some("ada@brightfield.test"));
    match &ticket.timeline[..] {
        [
            TimelineEntry::Message(m),
            TimelineEntry::Note(n),
            TimelineEntry::Other,
        ] => {
            assert_eq!(m.direction, Direction::Inbound);
            assert_eq!(n.body, "Called.");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn errors_carry_the_core_sentence() {
    let (url, _) = fake_core().await;
    let stranger = Client::new(&url, "not-a-token").unwrap();
    match stranger.whoami().await {
        Err(Error::Api { status, message }) => {
            assert_eq!((status, message.as_str()), (401, "Not signed in."));
        }
        other => panic!("{other:?}"),
    }
    let client = Client::new(&url, TOKEN).unwrap();
    let missing = client.ticket(13).await.unwrap_err();
    assert_eq!(missing.status(), Some(404));
    assert_eq!(
        missing.to_string(),
        "the core answered 404: Ticket not found."
    );
    assert!(!missing.is_transient());

    let unreachable = Client::new("http://127.0.0.1:9", TOKEN).unwrap();
    assert!(unreachable.whoami().await.unwrap_err().is_transient());
}

#[tokio::test]
async fn proposals_are_posted_in_the_core_shape() {
    let (url, posted) = fake_core().await;
    let client = Client::new(&url, TOKEN).unwrap();
    let message = Uuid::parse_str("7b8c9d0e-1f2a-4b3c-8d5e-6f7a8b9c0d1e").unwrap();
    let customer = Uuid::parse_str(CUSTOMER).unwrap();

    client
        .propose(
            Target::Message(message),
            &Content::Triage(Triage::OpenTicket {
                customer_id: customer,
            }),
            &Rationale::from([
                (
                    "en".to_owned(),
                    "The signature names Brightfield.".to_owned(),
                ),
                ("it".to_owned(), "La firma indica Brightfield.".to_owned()),
            ]),
        )
        .await
        .unwrap();
    client
        .propose(
            Target::Ticket(12),
            &Content::Priority {
                priority: Priority::Urgent,
            },
            &in_english("Nobody can print."),
        )
        .await
        .unwrap();
    let closed = client
        .propose(
            Target::Ticket(99),
            &Content::Reply { body: "Hi".into() },
            &Rationale::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(closed.status(), Some(409));

    let posted = posted.lock().unwrap();
    assert_eq!(
        posted[0],
        json!({"message_id": message, "content": {"kind": "triage", "action": "open_ticket",
            "customer_id": customer}, "rationale": {"en": "The signature names Brightfield.",
            "it": "La firma indica Brightfield."}})
    );
    assert_eq!(
        posted[1],
        json!({"ticket_number": 12, "content": {"kind": "priority", "priority": "urgent"},
            "rationale": {"en": "Nobody can print."}})
    );
}

#[tokio::test]
async fn tickets_are_opened_and_noted_in_the_core_shape() {
    let (url, posted) = fake_core().await;
    let client = Client::new(&url, TOKEN).unwrap();
    let customer = Uuid::parse_str(CUSTOMER).unwrap();
    let server = Uuid::parse_str("5a1d0c3e-7b2f-4e6a-9c8d-1f0e2d3c4b5a").unwrap();

    let opened = client
        .open_ticket(&NewTicket {
            description: Some("Disk /var is 97% full.".into()),
            priority: Some(Priority::High),
            asset_ids: vec![server],
            ..NewTicket::new(customer, "SRV-01: disk almost full")
        })
        .await
        .unwrap();
    assert_eq!(
        opened,
        OpenedTicket {
            number: 31,
            created: true
        }
    );
    client
        .open_ticket(&NewTicket::new(customer, "Backup failed"))
        .await
        .unwrap();
    let again = client
        .open_ticket(&NewTicket {
            reference: Some("4711".into()),
            ..NewTicket::new(customer, "SRV-01: disk almost full")
        })
        .await
        .unwrap();
    assert_eq!(
        again,
        OpenedTicket {
            number: 31,
            created: false
        },
        "200: the ticket opened before with that reference"
    );
    client.add_note(31, "Back to 60%.").await.unwrap();
    let refused = client
        .open_ticket(&NewTicket::new(customer, ""))
        .await
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "the core answered 409: A ticket needs a title."
    );
    assert_eq!(
        client.add_note(13, "Hello.").await.unwrap_err().status(),
        Some(404)
    );

    let posted = posted.lock().unwrap();
    assert_eq!(
        posted[0],
        json!({"customer_id": customer, "title": "SRV-01: disk almost full",
            "description": "Disk /var is 97% full.", "priority": "high",
            "asset_ids": [server], "service_ids": []})
    );
    assert_eq!(
        posted[1],
        json!({"customer_id": customer, "title": "Backup failed", "asset_ids": [],
            "service_ids": []})
    );
    assert_eq!(
        posted[2],
        json!({"customer_id": customer, "title": "SRV-01: disk almost full",
            "asset_ids": [], "service_ids": [], "reference": "4711"})
    );
    assert_eq!(
        posted[3],
        json!({"ticket": 31, "note": {"body": "Back to 60%."}})
    );
}

#[tokio::test]
async fn an_addon_resolves_a_ticket_nobody_worked_on() {
    let (url, posted) = fake_core().await;
    let client = Client::new(&url, TOKEN).unwrap();

    let ticket = client.ticket(31).await.unwrap();
    assert_eq!(ticket.opened_by_addon.as_deref(), Some("watchtower"));
    assert!(!ticket.worked_on_by_agent);
    client.resolve_ticket(31).await.unwrap();
    let refused = client.resolve_ticket(12).await.unwrap_err();
    assert_eq!(refused.status(), Some(409));
    assert_eq!(
        refused.to_string(),
        "the core answered 409: The add-on watchtower did not open the ticket #12: an add-on \
         resolves only the tickets it opened."
    );

    assert_eq!(posted.lock().unwrap()[..], [json!({"resolved": 31})]);
}
