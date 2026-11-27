//! Back to Zabbix, against the Zabbix in containers and the real core: an agent resolving or
//! closing the ticket of a problem acknowledges its event with a message, and closes the problem
//! only when its rule says so and its trigger allows it; the add-on's own resolutions send
//! nothing back; the media type's event menu entry opens the ticket's page, and what the add-on
//! does in Zabbix comes back through the media type without adding to the ticket. Ignored by
//! default, since they need Zabbix (`HD_TEST_ZABBIX_URL`), the test database server
//! (`DATABASE_URL`) and the core's binary (`cargo build -p hd-core`); CI runs them.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Core, TestZabbix};
use hd_addon_sdk::api::TicketState;
use hd_addon_sdk::events::{Agent, StateChangedBy, TicketStateChanged};
use hd_zabbix::kinds::Kinds;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const SECRET: &str = "a-webhook-secret-of-the-test";

/// The name of the core's administrator, who resolves and closes the tickets.
const AGENT: &str = "Ada Admin";

fn server() -> Kinds {
    Kinds {
        mappings: vec![],
        default: "Server".into(),
    }
}

/// A trapper item of the host, with a trigger on it that has a problem while its last value is
/// above 100 and the fields given, such as `manual_close`; answers the item's and the trigger's
/// ids.
async fn trapped(
    zabbix: &TestZabbix,
    host: &str,
    key: &str,
    trigger: &str,
    fields: Value,
) -> (String, String) {
    let item = zabbix
        .item(host, key, json!({"key_": key, "type": 2, "value_type": 3}))
        .await;
    let technical: Vec<Value> = zabbix
        .admin
        .call("host.get", json!({"output": ["host"], "hostids": [host]}))
        .await
        .unwrap();
    let technical = technical[0]["host"].as_str().unwrap();
    let trigger = zabbix
        .trigger_with(trigger, &format!("last(/{technical}/{key})>100"), 4, fields)
        .await;
    (item, trigger)
}

/// The base URL of the add-on's port, from the webhook's URL on it.
fn base(webhook: &str) -> String {
    webhook
        .strip_suffix(hd_zabbix::webhook::PATH)
        .unwrap()
        .to_owned()
}

/// Zabbix's acknowledgement bits: acknowledged with a message, and also closed.
const ACKNOWLEDGED: u32 = 6;
const CLOSED: u32 = 7;

const RULES: &str = r#"
[[alerts.rules]]
name = "Closing"
trigger_contains = "closing"
outcome = "open"
priority = "normal"
close_in_zabbix = true

[[alerts.rules]]
name = "Kept"
outcome = "open"
priority = "normal"
on_recovery = "resolve"
"#;

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn an_agent_resolving_or_closing_the_ticket_acknowledges_the_problem_and_closes_it_only_when_rule_and_trigger_allow_it()
 {
    let (listener, url, _) = common::webhook_listener().await;
    let core = Core::start_at(&base(&url)).await;
    let mut zabbix = TestZabbix::connect().await;
    core.asset_kind("Server").await;
    let brightfield = core.customer("Brightfield").await;
    let group = zabbix.group("Brightfield").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let host = zabbix
        .host("srv-01", &[&group], json!({"name": zabbix.name("SRV-01")}))
        .await;
    zabbix.addon(&core, server()).run().await.unwrap();
    let alerts = Arc::new(zabbix.alerts(&core, common::rules(RULES)));
    common::serve_alerts(listener, alerts.clone(), SECRET, &core.event_secret);

    // Each problem gets its ticket through the webhook.
    let mut problems = Vec::new();
    for (key, name, manual_close) in [
        ("closable", "Closing, closable", 1),
        ("fixed", "Closing, fixed", 0),
        ("kept", "Kept, closable", 1),
        ("recovered", "Kept, recovered", 1),
    ] {
        let (item, trigger) = trapped(
            &zabbix,
            &host,
            key,
            name,
            json!({"manual_close": manual_close}),
        )
        .await;
        let event = zabbix.raise(&item, &trigger, "500").await;
        let (status, answer) = common::post_event(&url, SECRET, common::problem_sent(&event)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let number: i64 = answer["tags"]["hd_ticket"]
            .as_str()
            .unwrap_or_else(|| panic!("no ticket in {answer}"))
            .parse()
            .unwrap();
        problems.push((item, event, number));
    }
    let [closable, fixed, kept, recovered] = &problems[..] else {
        unreachable!()
    };

    // The add-on's own resolution, on recovery, sends nothing back.
    zabbix.recover(&recovered.0, &recovered.1, "1").await;
    let answer = common::post_event(&url, SECRET, common::recovery_sent(&recovered.1)).await;
    assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})));
    assert_eq!(core.ticket(recovered.2).await["state"], "resolved");
    core.wait_for_deliveries().await;
    assert_eq!(zabbix.acknowledges(&recovered.1).await, []);

    // A ticket of the add-on whose reference is no problem's, such as one about the add-on
    // itself, sends nothing back either, and is taken.
    let own = core
        .addon()
        .open_ticket(&hd_addon_sdk::NewTicket {
            reference: Some("safety-limit:2026-11-27T09:00:00Z".into()),
            ..hd_addon_sdk::NewTicket::new(brightfield, "The add-on stopped opening tickets")
        })
        .await
        .unwrap();
    core.set_state(own.number, "resolved").await;
    // Nor does one whose problem Zabbix no longer has, such as one its housekeeper removed.
    let gone = core
        .addon()
        .open_ticket(&hd_addon_sdk::NewTicket {
            reference: Some("999999999".into()),
            ..hd_addon_sdk::NewTicket::new(brightfield, "SRV-02: Disk almost full")
        })
        .await
        .unwrap();
    core.set_state(gone.number, "resolved").await;

    core.set_state(closable.2, "resolved").await;
    core.set_state(fixed.2, "closed").await;
    core.set_state(kept.2, "resolved").await;
    core.set_state(recovered.2, "closed").await;
    core.wait_for_deliveries().await;

    let admin = "Admin".to_owned();
    assert_eq!(
        said(&zabbix, &closable.1).await,
        [(
            admin.clone(),
            CLOSED,
            format!(
                "Ticket {} was resolved in the help desk by {AGENT}: the problem is closed in \
                 Zabbix, as its rule says.",
                closable.2
            )
        )]
    );
    // Zabbix's server closes it soon after.
    let started = Instant::now();
    while zabbix.is_open(&closable.1).await {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "not closed in Zabbix"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(
        said(&zabbix, &fixed.1).await,
        [(
            admin.clone(),
            ACKNOWLEDGED,
            format!(
                "Ticket {} was closed in the help desk by {AGENT}; the problem stays open in \
                 Zabbix, since its trigger does not allow closing it by hand.",
                fixed.2
            )
        )]
    );
    assert!(
        zabbix.is_open(&fixed.1).await,
        "the trigger does not allow it"
    );
    assert_eq!(
        said(&zabbix, &kept.1).await,
        [(
            admin.clone(),
            ACKNOWLEDGED,
            format!(
                "Ticket {} was resolved in the help desk by {AGENT}; the problem stays open in \
                 Zabbix, since its rule does not close it.",
                kept.2
            )
        )]
    );
    assert!(zabbix.is_open(&kept.1).await, "the rule does not say so");
    // Closed by the agent once the add-on resolved it, after the problem recovered.
    assert_eq!(
        said(&zabbix, &recovered.1).await,
        [(
            admin.clone(),
            ACKNOWLEDGED,
            format!(
                "Ticket {} was closed in the help desk by {AGENT}.",
                recovered.2
            )
        )]
    );

    // The same change again, as when the core delivers it once more after the add-on acted on
    // it, is not acknowledged twice: also once the problem is closed, which would now be
    // acknowledged as recovered.
    for (event, number) in [(&closable.1, closable.2), (&kept.1, kept.2)] {
        let change = changes(&zabbix, event).await[0];
        let again = resolved_by_ada(&core, number, event).await;
        alerts.state_changed(&again, change).await.unwrap();
        assert_eq!(zabbix.acknowledges(event).await.len(), 1, "ticket {number}");
    }

    zabbix.clean_up().await;
}

/// The acknowledgements of the problem of `event`, as [`TestZabbix::acknowledges`] gives them,
/// each message without the key of the change it acknowledges, which each must end with.
async fn said(zabbix: &TestZabbix, event: &str) -> Vec<(String, u32, String)> {
    zabbix
        .acknowledges(event)
        .await
        .into_iter()
        .map(|(user, action, message)| {
            let (text, _) = keyed(&message);
            (user, action, text.to_owned())
        })
        .collect()
}

/// The ids of the changes the acknowledgements of the problem of `event` acknowledge.
async fn changes(zabbix: &TestZabbix, event: &str) -> Vec<Uuid> {
    zabbix
        .acknowledges(event)
        .await
        .iter()
        .map(|(_, _, message)| keyed(message).1)
        .collect()
}

/// An acknowledgement's message, split into its text and the id of the core's event that told
/// the change: `… [help desk change 6f1c…]`.
fn keyed(message: &str) -> (&str, Uuid) {
    let (text, key) = message
        .rsplit_once(" [help desk change ")
        .unwrap_or_else(|| panic!("no change in {message:?}"));
    let id = key
        .strip_suffix(']')
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| panic!("no change's id in {message:?}"));
    (text, id)
}

/// The change of the core's administrator resolving the ticket `number` of the problem of
/// `event`, as `ticket.state_changed` tells it.
async fn resolved_by_ada(core: &Core, number: i64, event: &str) -> TicketStateChanged {
    let agents = core.get("/api/agents").await;
    let ada = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == AGENT)
        .unwrap_or_else(|| panic!("no {AGENT} in {agents}"));
    TicketStateChanged {
        ticket_number: number,
        old_state: TicketState::Open,
        new_state: TicketState::Resolved,
        changed_by: StateChangedBy::Agent {
            agent: Agent {
                id: ada["id"].as_str().unwrap().parse().unwrap(),
                name: AGENT.into(),
            },
        },
        reference: Some(event.to_owned()),
    }
}

const CLOSING_RULES: &str = r#"
[[alerts.rules]]
outcome = "open"
priority = "normal"
close_in_zabbix = true
"#;

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_problem_zabbix_refuses_to_close_is_acknowledged_with_its_reason() {
    let (listener, url, _) = common::webhook_listener().await;
    let core = Core::start_at(&base(&url)).await;
    let mut zabbix = TestZabbix::connect().await;
    let brightfield = core.customer("Brightfield").await;
    let group = zabbix.group("Brightfield").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let host = zabbix
        .host("srv-01", &[&group], json!({"name": zabbix.name("SRV-01")}))
        .await;
    let (item, trigger) = trapped(
        &zabbix,
        &host,
        "queue",
        "Mail queue is too long",
        json!({"manual_close": 1}),
    )
    .await;
    // The add-on's user may not close problems.
    let role = zabbix
        .role(
            "No closing",
            json!({"actions": [{"name": "close_problems", "status": 0}]}),
        )
        .await;
    let user = zabbix.user("no-closing", json!({"roleid": role})).await;
    let token = zabbix.token_of(&user).await;
    let alerts = Arc::new(hd_zabbix::webhook::Alerts::new(
        core.addon(),
        hd_zabbix::zabbix::Zabbix::new(&common::zabbix_url(), &token, hd_zabbix::webhook::DEADLINE)
            .unwrap(),
        "Zabbix",
        common::rules(CLOSING_RULES),
    ));
    common::serve_alerts(listener, alerts.clone(), SECRET, &core.event_secret);

    let event = zabbix.raise(&item, &trigger, "500").await;
    let (status, answer) = common::post_event(&url, SECRET, common::problem_sent(&event)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let number: i64 = answer["tags"]["hd_ticket"]
        .as_str()
        .unwrap_or_else(|| panic!("no ticket in {answer}"))
        .parse()
        .unwrap();
    core.set_state(number, "resolved").await;
    core.wait_for_deliveries().await;

    let made = said(&zabbix, &event).await;
    assert_eq!(made.len(), 1, "{made:?}");
    let (by, action, message) = &made[0];
    assert_eq!((by, *action), (&user, ACKNOWLEDGED));
    let stays = format!(
        "Ticket {number} was resolved in the help desk by {AGENT}; the problem stays open in \
         Zabbix, which refused to close it: "
    );
    assert!(
        message.starts_with(&stays) && message.len() > stays.len(),
        "{message}"
    );
    assert!(zabbix.is_open(&event).await);

    // Delivered again, it is not acknowledged twice, though the message differs from the one
    // the add-on meant to give before Zabbix refused.
    let change = changes(&zabbix, &event).await[0];
    let again = resolved_by_ada(&core, number, &event).await;
    alerts.state_changed(&again, change).await.unwrap();
    assert_eq!(zabbix.acknowledges(&event).await.len(), 1);

    zabbix.clean_up().await;
}

/// A Zabbix that has the problem 4213, recovered by the event 4220, but fails to read 4220, as
/// one that fails a read now and then does.
async fn zabbix_failing_the_recovery() -> String {
    let app = axum::Router::new().route(
        "/api_jsonrpc.php",
        // The add-on sends application/json-rpc, which axum's Json does not take.
        axum::routing::post(|body: axum::body::Bytes| async move {
            let call: Value = serde_json::from_slice(&body).unwrap();
            let id = call["id"].clone();
            let answer = match (
                call["method"].as_str(),
                call["params"]["eventids"][0].as_str(),
            ) {
                (Some("event.get"), Some("4213")) => json!({"result": [
                    {"eventid": "4213", "value": "1", "severity": "4",
                     "name": "Mail queue is too long", "clock": "1795801516", "opdata": "",
                     "r_eventid": "4220", "tags": [], "hosts": []}]}),
                _ => json!({"error": {"code": -32500, "message": "Application error.",
                                      "data": "Database error occurred."}}),
            };
            let mut answer = answer;
            answer["jsonrpc"] = json!("2.0");
            answer["id"] = id;
            axum::Json(answer)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
#[ignore = "needs the test database"]
async fn a_recovery_whose_closer_cannot_be_read_adds_nothing_and_is_answered_with_a_failure() {
    let core = Core::start().await;
    let brightfield = core.customer("Brightfield").await;
    let opened = core
        .addon()
        .open_ticket(&hd_addon_sdk::NewTicket {
            reference: Some("4213".into()),
            ..hd_addon_sdk::NewTicket::new(brightfield, "SRV-01: Mail queue is too long")
        })
        .await
        .unwrap();
    let alerts = hd_zabbix::webhook::Alerts::new(
        core.addon(),
        hd_zabbix::zabbix::Zabbix::new(
            &zabbix_failing_the_recovery().await,
            "zabbix-token",
            Duration::from_secs(5),
        )
        .unwrap(),
        "Zabbix",
        hd_zabbix::rules::Rules::default_list(),
    );
    let (listener, url, _) = common::webhook_listener().await;
    common::serve_webhook(listener, alerts, SECRET);

    // Whether the add-on itself closed the problem cannot be told: Zabbix tries again, rather
    // than the ticket get a note and, by the default, be resolved.
    let (status, why) = common::post_event(&url, SECRET, common::recovery_sent("4213")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{why}");
    assert_eq!(core.ticket(opened.number).await["state"], "open");
    assert_eq!(core.addon_notes(opened.number).await, Vec::<String>::new());
}

const MEDIA_TYPE_RULES: &str = r#"
[[alerts.rules]]
outcome = "open"
priority = "high"
on_recovery = "resolve"
close_in_zabbix = true
"#;

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn the_event_menu_opens_the_ticket_and_the_add_on_s_own_acknowledgement_and_close_come_back_adding_nothing()
 {
    let secret = uuid::Uuid::new_v4().simple().to_string();
    let (listener, url, reached) = common::webhook_listener().await;
    let core = Core::start_at(&base(&url)).await;
    let mut zabbix = TestZabbix::connect().await;
    core.asset_kind("Server").await;
    let brightfield = core.customer("Brightfield").await;
    let group = zabbix.group("Brightfield").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let host = zabbix
        .host("srv-01", &[&group], json!({"name": zabbix.name("SRV-01")}))
        .await;
    zabbix.addon(&core, server()).run().await.unwrap();
    let (item, trigger) = trapped(
        &zabbix,
        &host,
        "queue",
        "Mail queue is too long",
        json!({"manual_close": 1}),
    )
    .await;
    // The UI at the core's own address, where the browser opens it.
    let ui = core.url.clone();
    let alerts = Arc::new(
        zabbix
            .alerts(&core, common::rules(MEDIA_TYPE_RULES))
            .with_ui_url(&ui),
    );
    common::serve_alerts(listener, alerts, &secret, &core.event_secret);
    let media_type = zabbix.media_type(&reached, &secret).await;
    let imported: Vec<Value> = zabbix
        .admin
        .call(
            "mediatype.get",
            json!({"mediatypeids": [media_type],
                   "output": ["show_event_menu", "event_menu_url", "event_menu_name"]}),
        )
        .await
        .unwrap();
    assert_eq!(
        imported[0],
        json!({"mediatypeid": media_type, "show_event_menu": "1",
               "event_menu_url": "{EVENT.TAGS.hd_ticket_url}",
               "event_menu_name": "Help desk ticket {EVENT.TAGS.hd_ticket}"})
    );
    let receiver = zabbix
        .user(
            "helpdesk",
            json!({"medias": [{"mediatypeid": media_type, "sendto": "helpdesk",
                               "active": 0, "severity": 63,
                               "period": "1-7,00:00-24:00"}]}),
        )
        .await;
    let action = zabbix.action(&group, &media_type, &receiver).await;

    let event = zabbix.raise(&item, &trigger, "500").await;
    let addon = core.addon();
    let started = Instant::now();
    let number = loop {
        if let Ok(ticket) = addon.ticket_by_reference(&event).await {
            break ticket.number;
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "no ticket for problem {event}; Zabbix's alerts: {}",
            zabbix.alerts_of(&event).await
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    // The event's menu has the entry of the media type, to the ticket's page.
    let page = format!("{ui}/#/tickets/{number}");
    let started = Instant::now();
    loop {
        let events: Vec<Value> = zabbix
            .admin
            .call(
                "event.get",
                json!({"eventids": [event], "output": ["urls"]}),
            )
            .await
            .unwrap();
        // Other media types with an entry may be listed too.
        if events[0]["urls"]
            .as_array()
            .unwrap()
            .contains(&json!({"name": format!("Help desk ticket {number}"), "url": page}))
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the event's menu links {}; its tags are {:?}",
            events[0]["urls"],
            zabbix.event_tags(&event).await
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // The agent resolves it: the add-on acknowledges and closes the problem, and Zabbix sends
    // that update and the recovery to the add-on, which add no note and resolve nothing again.
    core.set_state(number, "resolved").await;
    core.wait_for_deliveries().await;
    assert_eq!(
        said(&zabbix, &event).await,
        [(
            "Admin".to_owned(),
            CLOSED,
            format!(
                "Ticket {number} was resolved in the help desk by {AGENT}: the problem is closed \
                 in Zabbix, as its rule says."
            )
        )]
    );
    // The problem, its update and its recovery, each sent to the add-on.
    let started = Instant::now();
    loop {
        let sent: Vec<Value> = zabbix
            .admin
            .call(
                "alert.get",
                json!({"actionids": [action], "output": ["status", "error", "eventid"]}),
            )
            .await
            .unwrap();
        if sent.len() >= 3 && sent.iter().all(|a| a["status"] == "1") {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "Zabbix's alerts of the action are {sent:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let notes = core.addon_notes(number).await;
    assert_eq!(notes.len(), 1, "the description only: {notes:?}");
    assert_eq!(core.ticket(number).await["state"], "resolved");
    assert_eq!(zabbix.acknowledges(&event).await.len(), 1);

    zabbix.clean_up().await;
}
