//! The reference an add-on gives a ticket it opens, with its token: it names that ticket for good,
//! the add-on finds its tickets by it, and nobody else reads it.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{Client, addon};
use crate::addons::{TICKETS_READ, TICKETS_WRITE};
use crate::testing;

fn ticket(customer: uuid::Uuid, title: &str, reference: Option<&str>) -> Value {
    let mut ticket = json!({"customer_id": customer, "title": title,
        "description": "Seen at 03:10."});
    if let Some(reference) = reference {
        ticket["reference"] = json!(reference);
    }
    ticket
}

/// Opens `body` with `client`, expecting `status`; returns the ticket's number.
async fn open(client: &Client, body: Value, status: StatusCode) -> i64 {
    let (answered, json) = client.call("POST", "/api/addon/tickets", Some(body)).await;
    assert_eq!(answered, status, "{json}");
    json["number"].as_i64().unwrap()
}

/// Opening again with a reference answers the same ticket with 200 instead of 201, in any state,
/// and changes nothing on it; another add-on's same reference is its own. A reference that does not
/// fit is refused with a sentence, and so is a token without `tickets:write`.
#[sqlx::test]
async fn an_addon_opens_one_ticket_per_reference_through_the_api(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let harbor = testing::customer(&db, "Harbor Lane").await;
    let watchtower = addon(&db, &api, "watchtower", &[TICKETS_WRITE]).await;
    let pinger = addon(&db, &api, "pinger", &[TICKETS_WRITE]).await;
    let reader = addon(&db, &api, "reader", &[TICKETS_READ]).await;

    let disk = ticket(brightfield, "SRV-01: disk full", Some("4711"));
    let (status, refused) = reader
        .call("POST", "/api/addon/tickets", Some(disk.clone()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        refused["error"],
        "The add-on reader lacks the scope tickets:write."
    );
    let n = open(&watchtower, disk, StatusCode::CREATED).await;
    let (status, json) = api
        .call(
            "PUT",
            &format!("/api/tickets/{n}/state"),
            Some(json!({"state": "closed"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    let before = api.get(&format!("/api/tickets/{n}")).await;

    let other = json!({"customer_id": harbor, "title": "Something else", "priority": "urgent",
        "description": "Again.", "reference": " 4711 "});
    assert_eq!(open(&watchtower, other, StatusCode::OK).await, n);
    assert_eq!(
        api.get(&format!("/api/tickets/{n}")).await,
        before,
        "nothing changed"
    );
    let theirs = open(
        &pinger,
        ticket(brightfield, "Host down", Some("4711")),
        StatusCode::CREATED,
    )
    .await;
    assert_ne!(theirs, n);
    let plain = ticket(brightfield, "Backup failed", None);
    let first = open(&watchtower, plain.clone(), StatusCode::CREATED).await;
    assert_ne!(open(&watchtower, plain, StatusCode::CREATED).await, first);

    let limit = crate::addon_tickets::MAX_REFERENCE_LENGTH;
    for (reference, sentence) in [
        (
            "  ".to_owned(),
            "A ticket reference cannot be blank.".to_owned(),
        ),
        (
            "x".repeat(limit + 1),
            format!("A ticket reference is longer than {limit} characters."),
        ),
    ] {
        let (status, refused) = watchtower
            .call(
                "POST",
                "/api/addon/tickets",
                Some(ticket(brightfield, "Disk full", Some(&reference))),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["error"], sentence);
    }
}

/// Two opens with the same reference at the same time open one ticket: one is answered 201, the
/// other 200 with the same number, and neither fails.
#[sqlx::test]
async fn two_opens_with_the_same_reference_at_the_same_time_open_one_ticket(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let watchtower = addon(&db, &api, "watchtower", &[TICKETS_WRITE]).await;

    // Holding the customer makes the first open wait before writing its ticket, and the second
    // waits for the first: both are under way at once when it is let go.
    let mut holder = db.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM customers WHERE id = $1 FOR UPDATE")
        .bind(brightfield)
        .execute(&mut *holder)
        .await
        .unwrap();
    let body = ticket(brightfield, "SRV-01: disk full", Some("4711"));
    let both = async {
        tokio::join!(
            watchtower.call("POST", "/api/addon/tickets", Some(body.clone())),
            watchtower.call("POST", "/api/addon/tickets", Some(body.clone())),
        )
    };
    let release = async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(&db)
            .await
            .unwrap();
            if waiting == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        holder.commit().await.unwrap();
    };
    let (((first, a), (second, b)), ()) = tokio::join!(both, release);

    let mut statuses = [first, second];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CREATED], "{a} {b}");
    assert_eq!(a["number"], b["number"]);
    let (tickets, events, notes): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM tickets), (SELECT count(*) FROM events),
                (SELECT count(*) FROM notes)",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (tickets, events, notes),
        (1, 1, 1),
        "one ticket, one ticket.opened, one description"
    );
}

/// Waits until `count` connections to the test database wait for a lock, for at most 10 seconds.
async fn until_waiting(db: &PgPool, count: i64) {
    let mut waiting = 0;
    for _ in 0..500 {
        waiting = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(db)
        .await
        .unwrap();
        if waiting == count {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{waiting} connections wait for a lock, not {count}");
}

/// An open that comes while another with the same reference is still being written answers that
/// ticket once it is, with 200, also when the rest of what it sends would be refused on its own:
/// a blank title, an offboarded customer, an asset that does not exist.
#[sqlx::test]
async fn an_open_racing_one_with_the_same_reference_answers_its_ticket_whatever_it_sends(
    db: PgPool,
) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let gone = testing::customer(&db, "Old Mill").await;
    crate::customers::offboard(&db, gone).await.unwrap();
    let watchtower = addon(&db, &api, "watchtower", &[TICKETS_WRITE]).await;

    let wrong = [
        ticket(brightfield, "   ", Some("4711")),
        ticket(gone, "Disk full", Some("4712")),
        json!({"customer_id": brightfield, "title": "Disk full", "reference": "4713",
            "asset_ids": [uuid::Uuid::new_v4()]}),
    ];
    for second in wrong {
        let reference = second["reference"].as_str().unwrap().to_owned();
        let first = ticket(brightfield, "SRV-01: disk full", Some(&reference));
        // The first open waits on the customer, held here, while the second one comes.
        let mut holder = db.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM customers WHERE id = $1 FOR UPDATE")
            .bind(brightfield)
            .execute(&mut *holder)
            .await
            .unwrap();
        let ((status_first, a), (status_second, b), ()) = tokio::join!(
            watchtower.call("POST", "/api/addon/tickets", Some(first)),
            async {
                until_waiting(&db, 1).await;
                watchtower
                    .call("POST", "/api/addon/tickets", Some(second.clone()))
                    .await
            },
            async {
                until_waiting(&db, 2).await;
                holder.commit().await.unwrap();
            },
        );
        assert_eq!(status_first, StatusCode::CREATED, "{a}");
        assert_eq!(status_second, StatusCode::OK, "{second}: {b}");
        assert_eq!(a["number"], b["number"]);
    }
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(tickets, 3);
}

/// The add-on reads the ticket it opened by its reference, in any state, as it reads it by number
/// and with the reference; a reference it never gave, another add-on's included, is not found. It
/// lists the tickets it opened that are not closed, with their references. Other add-ons and
/// agents never read the reference. Without `tickets:read` both reads are refused.
#[sqlx::test]
async fn an_addon_finds_its_tickets_by_reference_and_nobody_else_reads_it(db: PgPool) {
    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let watchtower = addon(&db, &api, "watchtower", &[TICKETS_READ, TICKETS_WRITE]).await;
    let pinger = addon(&db, &api, "pinger", &[TICKETS_READ, TICKETS_WRITE]).await;
    let writer = addon(&db, &api, "writer", &[TICKETS_WRITE]).await;

    let disk = open(
        &watchtower,
        ticket(brightfield, "SRV-01: disk full", Some("4711")),
        StatusCode::CREATED,
    )
    .await;
    let fan = open(
        &watchtower,
        ticket(brightfield, "SRV-01: fan failure", Some("4712")),
        StatusCode::CREATED,
    )
    .await;
    let plain = open(
        &watchtower,
        ticket(brightfield, "Backup failed", None),
        StatusCode::CREATED,
    )
    .await;
    let closed = open(
        &watchtower,
        ticket(brightfield, "UPS on battery", Some("4713")),
        StatusCode::CREATED,
    )
    .await;
    testing::set_state(&db, closed, crate::tickets::TicketState::Closed)
        .await
        .unwrap();
    let theirs = open(
        &pinger,
        ticket(brightfield, "Host down", Some("4799")),
        StatusCode::CREATED,
    )
    .await;
    let by_hand = testing::ticket(&db, brightfield, None, "h1@brightfield.test").await;

    let by_reference = |reference: &str| {
        format!(
            "/api/addon/tickets/by-reference?reference={}",
            reference.replace(' ', "%20")
        )
    };
    let read = watchtower.get(&by_reference("4711")).await;
    assert_eq!(read["number"], disk);
    assert_eq!(read["reference"], "4711");
    assert_eq!(read["timeline"], json!([]), "no notes, as by number");
    assert_eq!(
        read,
        watchtower.get(&format!("/api/addon/tickets/{disk}")).await
    );
    assert_eq!(
        watchtower.get(&by_reference(" 4713 ")).await["number"],
        closed,
        "closed too"
    );
    for unknown in ["4714", "4799", "  "] {
        let (status, json) = watchtower.call("GET", &by_reference(unknown), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{unknown:?}: {json}");
    }

    let opened = watchtower.get("/api/addon/tickets/opened").await;
    let mut listed: Vec<(i64, Value)> = opened
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["number"].as_i64().unwrap(), t["reference"].clone()))
        .collect();
    listed.sort_by_key(|(n, _)| *n);
    assert_eq!(
        listed,
        [
            (disk, json!("4711")),
            (fan, json!("4712")),
            (plain, Value::Null)
        ],
        "not the closed one, nor another add-on's"
    );
    assert!(opened[0]["title"].is_string());
    let mine = watchtower
        .get(&format!("/api/addon/tickets?customer={brightfield}"))
        .await;
    let references: Vec<(i64, Value)> = mine
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["number"].as_i64().unwrap(), t["reference"].clone()))
        .filter(|(n, _)| [disk, theirs, by_hand].contains(n))
        .collect();
    assert_eq!(references.len(), 3, "{mine}");
    for (n, reference) in references {
        let expected = if n == disk {
            json!("4711")
        } else {
            Value::Null
        };
        assert_eq!(reference, expected, "#{n}");
    }

    // Another add-on and the agents read the ticket without it.
    let seen = pinger.get(&format!("/api/addon/tickets/{disk}")).await;
    assert!(seen.get("reference").is_none(), "{seen}");
    let listed = pinger
        .get(&format!("/api/addon/tickets?customer={brightfield}"))
        .await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["reference"] == Value::Null || t["number"] == theirs),
        "{listed}"
    );
    let seen = api.get(&format!("/api/tickets/{disk}")).await;
    assert!(seen.get("reference").is_none(), "{seen}");

    for uri in [by_reference("4711"), "/api/addon/tickets/opened".to_owned()] {
        let (status, refused) = writer.call("GET", &uri, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}");
        assert_eq!(
            refused["error"],
            "The add-on writer lacks the scope tickets:read."
        );
    }
}

/// `ticket.state_changed` carries the reference to the add-on that opened the ticket with it, and
/// to no other add-on; the event the core keeps does not hold it.
#[sqlx::test]
async fn only_the_opener_receives_the_reference_in_ticket_state_changed(db: PgPool) {
    use axum::{body::Bytes, extract::Path, routing::post};
    use std::sync::{Arc, Mutex};
    type Seen = Arc<Mutex<Vec<(String, Value)>>>;
    let seen: Seen = Arc::default();
    let fake = axum::Router::new().route(
        "/{addon}/events",
        post({
            let seen = seen.clone();
            move |Path(addon): Path<String>, body: Bytes| async move {
                let envelope: Value = serde_json::from_slice(&body).unwrap();
                seen.lock().unwrap().push((addon, envelope["data"].clone()));
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, fake).await });

    let api = Client::new(&db).await;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let events = [crate::addons::TICKET_STATE_CHANGED.to_owned()];
    let scopes = [TICKETS_WRITE.to_owned()];
    let register = |name: &'static str| {
        let (db, url, events, scopes) = (db.clone(), url.clone(), events.clone(), scopes.clone());
        async move {
            crate::addons::register(&db, name, &format!("{url}/{name}"), &events, &scopes)
                .await
                .unwrap()
        }
    };
    let watchtower = register("watchtower").await;
    let pinger = register("pinger").await;
    let watchtower = Client::new_with_token(&db, &watchtower.token, api.agent);
    let pinger = Client::new_with_token(&db, &pinger.token, api.agent);
    let disk = open(
        &watchtower,
        ticket(brightfield, "SRV-01: disk full", Some("4711")),
        StatusCode::CREATED,
    )
    .await;
    let host = open(
        &pinger,
        ticket(brightfield, "Host down", None),
        StatusCode::CREATED,
    )
    .await;
    for n in [disk, host] {
        let (status, json) = api
            .call(
                "PUT",
                &format!("/api/tickets/{n}/state"),
                Some(json!({"state": "waiting"})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    }

    let client = crate::addons::http_client().unwrap();
    assert_eq!(crate::addons::deliver_due(&db, &client).await.unwrap(), 4);
    let mut seen: Vec<(String, i64, Value)> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(addon, data)| {
            assert_eq!(data["new_state"], "waiting");
            (
                addon.clone(),
                data["ticket_number"].as_i64().unwrap(),
                data.get("reference").cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    seen.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    assert_eq!(
        seen,
        [
            ("pinger".to_owned(), disk, Value::Null),
            ("pinger".to_owned(), host, Value::Null),
            ("watchtower".to_owned(), disk, json!("4711")),
            ("watchtower".to_owned(), host, Value::Null),
        ]
    );
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE data ? 'reference'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(kept, 0);
}

/// The specification and the add-on contract give the limit of a reference the core keeps.
#[test]
fn the_specification_and_the_contract_give_the_limit_of_a_reference() {
    let limit = crate::addon_tickets::MAX_REFERENCE_LENGTH;
    let spec = serde_json::to_value(crate::http::openapi()).unwrap();
    let open = &spec["paths"]["/api/addon/tickets"]["post"];
    let description = open["description"].as_str().unwrap();
    assert!(
        description.contains(&format!("at most {limit} characters once trimmed")),
        "{description}"
    );
    let name = open["requestBody"]["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap();
    assert_eq!(
        spec["components"]["schemas"][name]["properties"]["reference"]["maxLength"],
        json!(limit)
    );
    assert!(
        spec["components"]["schemas"]["TicketDetail"]["properties"]
            .get("reference")
            .is_none(),
        "the ticket agents read does not describe it"
    );
    let contract = include_str!("../../../../sdk/README.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let said = format!("A ticket reference is at most {limit} characters once trimmed");
    assert!(
        contract.contains(&said),
        "sdk/README.md does not say {said:?}"
    );
}

/// The SDK's calls reach the core: an add-on built on it opens a ticket once per reference, tells
/// a new ticket from the one it opened before, and finds it by a reference of any characters.
#[sqlx::test]
async fn an_addon_built_on_the_sdk_opens_and_finds_its_tickets_by_reference(db: PgPool) {
    use hd_addon_sdk::NewTicket;
    let brightfield = testing::customer(&db, "Brightfield").await;
    let registered = crate::addons::register(
        &db,
        "watchtower",
        "http://127.0.0.1:9",
        &[],
        &[TICKETS_READ.into(), TICKETS_WRITE.into()],
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
    let reference = "zabbix/4711 & co?#é";
    let disk = NewTicket {
        reference: Some(reference.into()),
        ..NewTicket::new(brightfield, "SRV-01: disk full")
    };

    let opened = sdk.open_ticket(&disk).await.unwrap();
    assert!(opened.created);
    let again = sdk
        .open_ticket(&NewTicket {
            title: "Something else".into(),
            ..disk.clone()
        })
        .await
        .unwrap();
    assert_eq!((again.number, again.created), (opened.number, false));
    let plain = sdk
        .open_ticket(&NewTicket::new(brightfield, "Backup failed"))
        .await
        .unwrap();

    let found = sdk.ticket_by_reference(reference).await.unwrap();
    assert_eq!(found.number, opened.number);
    assert_eq!(found.title, "SRV-01: disk full");
    assert_eq!(found.reference.as_deref(), Some(reference));
    assert_eq!(
        sdk.ticket_by_reference("zabbix/4712")
            .await
            .unwrap_err()
            .status(),
        Some(404)
    );
    let mut mine: Vec<(i64, Option<String>)> = sdk
        .opened_tickets()
        .await
        .unwrap()
        .into_iter()
        .map(|t| (t.number, t.reference))
        .collect();
    mine.sort();
    assert_eq!(
        mine,
        [
            (opened.number, Some(reference.to_owned())),
            (plain.number, None)
        ]
    );
}
