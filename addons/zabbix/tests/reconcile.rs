//! The reconciliation and the safety limit, against the Zabbix in containers and the real core:
//! problems raised and recovered while the add-on heard nothing get their tickets and recoveries
//! from the reconciliation, and more tickets than the limit in its window open a safety ticket
//! for the help desk's own organization and hold back the problems until an agent resolves it.
//! Ignored by default, since they need Zabbix (`HD_TEST_ZABBIX_URL`), the test database server
//! (`DATABASE_URL`) and the core's binary (`cargo build -p hd-core`); CI runs them.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Core, TestZabbix};
use hd_addon_sdk::api::TicketSummary;
use hd_zabbix::alerts::EventKind;
use hd_zabbix::kinds::Kinds;
use hd_zabbix::reconcile::Reconciled;
use hd_zabbix::references::TicketReference;
use hd_zabbix::safety::{self, Limit};
use reqwest::StatusCode;
use serde_json::{Value, json};

const SECRET: &str = "a-webhook-secret-of-the-test";

fn server() -> Kinds {
    Kinds {
        mappings: vec![],
        default: "Server".into(),
    }
}

/// A trapper item of the host with a trigger of this severity on it, which has a problem while
/// its last value is above 100; answers the item's and the trigger's ids.
async fn trapped(zabbix: &TestZabbix, host: &str, key: &str, severity: u8) -> (String, String) {
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
        .trigger_with(
            &format!("Queue {key} is too long"),
            &format!("last(/{technical}/{key})>100"),
            severity,
            json!({}),
        )
        .await;
    (item, trigger)
}

/// The number of the ticket the webhook's answer gives in the tag `hd_ticket`.
fn tagged(answer: &(StatusCode, Value)) -> i64 {
    assert_eq!(answer.0, StatusCode::OK, "{}", answer.1);
    answer.1["tags"]["hd_ticket"]
        .as_str()
        .unwrap_or_else(|| panic!("no ticket in {}", answer.1))
        .parse()
        .unwrap()
}

/// The add-on's tickets that are not closed, by the reference they were opened with.
async fn opened(core: &Core) -> Vec<TicketSummary> {
    core.addon().opened_tickets().await.unwrap()
}

/// The ticket the add-on opened for the problem of `event`.
async fn ticket_of(core: &Core, event: &str) -> i64 {
    core.addon()
        .ticket_by_reference(event)
        .await
        .unwrap()
        .number
}

/// The safety tickets the add-on opened that are not closed.
async fn safety_tickets(core: &Core) -> Vec<TicketSummary> {
    opened(core)
        .await
        .into_iter()
        .filter(|t| {
            t.reference
                .as_deref()
                .and_then(TicketReference::read)
                .is_some_and(|r| r.safety().is_some())
        })
        .collect()
}

/// The time Zabbix's event `id` happened, as the notes write it.
async fn happened(zabbix: &TestZabbix, id: &str) -> String {
    let events: Vec<Value> = zabbix
        .admin
        .call("event.get", json!({"eventids": [id], "output": ["clock"]}))
        .await
        .unwrap();
    hd_zabbix::alerts::utc(events[0]["clock"].as_str().unwrap().parse().unwrap())
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn after_the_add_on_was_down_the_reconciliation_opens_the_missing_tickets_once_and_handles_the_missed_recoveries()
 {
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    core.asset_kind("Server").await;
    let brightfield = core.customer("Brightfield").await;
    let group = zabbix.group("Brightfield").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let srv = zabbix.name("SRV-01");
    let host = zabbix.host("srv-01", &[&group], json!({"name": srv})).await;
    zabbix.addon(&core, server()).run().await.unwrap();
    // The default list: high opens a ticket, warning none.
    let (untouched_item, untouched) = trapped(&zabbix, &host, "untouched", 4).await;
    let (taken_item, taken) = trapped(&zabbix, &host, "taken", 4).await;
    let (missed_item, missed) = trapped(&zabbix, &host, "missed", 4).await;
    let (warning_item, warning) = trapped(&zabbix, &host, "warning", 2).await;
    let (late_item, late) = trapped(&zabbix, &host, "late", 4).await;
    let (suppressed_item, suppressed) = trapped(&zabbix, &host, "suppressed", 4).await;
    let (symptom_item, symptom) = trapped(&zabbix, &host, "symptom", 4).await;

    // Before the add-on goes down, the webhook opens two tickets.
    let (listener, url, _) = common::webhook_listener().await;
    common::serve_webhook(
        listener,
        zabbix.alerts(&core, hd_zabbix::rules::Rules::default_list()),
        SECRET,
    );
    let untouched_event = zabbix.raise(&untouched_item, &untouched, "500").await;
    let untouched_ticket =
        tagged(&common::post_event(&url, SECRET, common::problem_sent(&untouched_event)).await);
    let taken_event = zabbix.raise(&taken_item, &taken, "500").await;
    let taken_ticket =
        tagged(&common::post_event(&url, SECRET, common::problem_sent(&taken_event)).await);

    // While it is down: a problem the rules open and one they ignore start, a problem starts and
    // recovers, and both tickets' problems recover. Two more the rules would open are left out,
    // as a trigger action leaves them by default: one is suppressed, one a symptom of another.
    let missed_event = zabbix.raise(&missed_item, &missed, "500").await;
    let warning_event = zabbix.raise(&warning_item, &warning, "500").await;
    let late_event = zabbix.raise(&late_item, &late, "500").await;
    zabbix.recover(&late_item, &late_event, "1").await;
    let untouched_recovery = zabbix.recover(&untouched_item, &untouched_event, "1").await;
    let taken_recovery = zabbix.recover(&taken_item, &taken_event, "1").await;
    // An agent works on one after its problem recovered: its recovery is still missed.
    core.work_on(taken_ticket).await;
    let suppressed_event = zabbix.raise(&suppressed_item, &suppressed, "500").await;
    let symptom_event = zabbix.raise(&symptom_item, &symptom, "500").await;
    // Suppress indefinitely (32), and change the rank to symptom (256).
    for (event, update) in [
        (
            &suppressed_event,
            json!({"action": 32, "suppress_until": 0}),
        ),
        (
            &symptom_event,
            json!({"action": 256, "cause_eventid": missed_event}),
        ),
    ] {
        let mut params = json!({"eventids": [event]});
        params
            .as_object_mut()
            .unwrap()
            .extend(update.as_object().unwrap().clone());
        let _: Value = zabbix
            .admin
            .call("event.acknowledge", params)
            .await
            .unwrap();
    }
    // The server applies both in a moment.
    let started = std::time::Instant::now();
    loop {
        let listed: Vec<Value> = zabbix
            .admin
            .call(
                "problem.get",
                json!({"eventids": [suppressed_event, symptom_event],
                       "output": ["eventid", "suppressed", "cause_eventid"]}),
            )
            .await
            .unwrap();
        let applied = listed.iter().any(|p| p["suppressed"] == "1")
            && listed
                .iter()
                .any(|p| p["cause_eventid"] == missed_event.as_str());
        if applied {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "neither suppressed nor a symptom yet: {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Started again, the reconciliation opens the missing ticket and handles both recoveries.
    let alerts = zabbix.alerts(&core, hd_zabbix::rules::Rules::default_list());
    let round = alerts.reconcile().await.unwrap();
    assert_eq!(
        round,
        Reconciled {
            active: 2,
            opened: 1,
            held_back: 0,
            recovered: 2,
            failed: 0,
            holding: false,
        }
    );
    let missed_ticket = ticket_of(&core, &missed_event).await;
    let ticket = core.ticket(missed_ticket).await;
    assert_eq!(ticket["customer"]["name"], "Brightfield");
    assert_eq!(ticket["priority"], "high");
    assert_eq!(ticket["title"], format!("{srv}: Queue missed is too long"));
    assert_eq!(ticket["assets"][0]["name"], srv.as_str());
    assert!(
        core.addon_notes(missed_ticket).await[0].starts_with(&format!(
            "Zabbix's problem {missed_event} on {srv}, opened by rule 2"
        )),
        "the default list's rule High"
    );
    assert!(
        !zabbix
            .event_tags(&missed_event)
            .await
            .iter()
            .any(|(tag, _)| tag == "hd_ticket"),
        "Zabbix takes tags only from the webhook's answer"
    );
    // problem.get gives what the add-on leaves suppressed problems and symptoms out by.
    let listed: Vec<Value> = zabbix
        .admin
        .call(
            "problem.get",
            json!({"eventids": [missed_event], "output": ["eventid", "suppressed",
                                                         "cause_eventid"]}),
        )
        .await
        .unwrap();
    assert_eq!(
        (&listed[0]["suppressed"], &listed[0]["cause_eventid"]),
        (&json!("0"), &json!("0"))
    );
    let no_ticket = core.addon().ticket_by_reference(&warning_event).await;
    assert_eq!(
        no_ticket.unwrap_err().status(),
        Some(404),
        "the rules ignore it"
    );
    let no_ticket = core.addon().ticket_by_reference(&late_event).await;
    assert_eq!(no_ticket.unwrap_err().status(), Some(404), "it recovered");
    for (event, why) in [
        (&suppressed_event, "suppressed"),
        (&symptom_event, "a symptom"),
    ] {
        let no_ticket = core.addon().ticket_by_reference(event).await;
        assert_eq!(no_ticket.unwrap_err().status(), Some(404), "{why}");
    }

    let recovered_at = happened(&zabbix, &untouched_recovery).await;
    assert_eq!(
        core.addon_notes(untouched_ticket).await[1..],
        [format!(
            "The problem recovered in Zabbix at {recovered_at}."
        )]
    );
    assert_eq!(core.ticket(untouched_ticket).await["state"], "resolved");
    let recovered_at = happened(&zabbix, &taken_recovery).await;
    assert_eq!(
        core.addon_notes(taken_ticket).await[1..],
        [format!(
            "The problem recovered in Zabbix at {recovered_at}."
        )]
    );
    assert_eq!(
        core.ticket(taken_ticket).await["state"],
        "open",
        "an agent worked on it"
    );

    // Again: nothing more, though one ticket stays open.
    let tickets = core.tickets().await;
    let round = alerts.reconcile().await.unwrap();
    assert_eq!((round.opened, round.recovered, round.failed), (0, 0, 0));
    assert_eq!(core.tickets().await, tickets);
    assert_eq!(core.addon_notes(taken_ticket).await.len(), 2);
    assert_eq!(core.addon_notes(untouched_ticket).await.len(), 2);
    // After a restart, the add-on cannot tell that it handled the recovery of the ticket left
    // open: Zabbix has no alert of the webhook for it, as when no action sends recoveries to the
    // webhook. It notes it again; the resolved one is left alone, and nothing is resolved.
    let round = zabbix
        .alerts(&core, hd_zabbix::rules::Rules::default_list())
        .reconcile()
        .await
        .unwrap();
    assert_eq!((round.opened, round.recovered, round.failed), (0, 1, 0));
    assert_eq!(core.tickets().await, tickets);
    assert_eq!(core.addon_notes(taken_ticket).await.len(), 3);
    assert_eq!(core.ticket(taken_ticket).await["state"], "open");
    assert_eq!(core.addon_notes(untouched_ticket).await.len(), 2);

    zabbix.clean_up().await;
}

/// The rules of the test of the safety limit: every problem opens a ticket.
const EVERY_PROBLEM: &str = r#"
[[alerts.rules]]
outcome = "open"
priority = "normal"
"#;

/// Two tickets in 5 minutes.
const TWO: Limit = Limit {
    tickets: 2,
    window: Duration::from_secs(300),
};

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn more_tickets_than_the_limit_open_a_safety_ticket_and_the_problems_wait_until_an_agent_resolves_it()
 {
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    core.asset_kind("Server").await;
    core.help_desk("Northwind Help Desk");
    let brightfield = core.customer("Brightfield").await;
    let group = zabbix.group("Brightfield").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let host = zabbix
        .host("srv-01", &[&group], json!({"name": zabbix.name("SRV-01")}))
        .await;
    zabbix.addon(&core, server()).run().await.unwrap();
    let mut problems = Vec::new();
    for key in ["first", "second", "third", "fourth", "fifth"] {
        problems.push(trapped(&zabbix, &host, key, 3).await);
    }
    let mut events = Vec::new();
    for (item, trigger) in &problems {
        events.push(zabbix.raise(item, trigger, "500").await);
    }
    let rules = || common::rules(EVERY_PROBLEM);

    let alerts = Arc::new(zabbix.alerts(&core, rules()).with_limit(TWO));
    let (listener, url, _) = common::webhook_listener().await;
    let webhook = common::serve_webhook(listener, alerts.clone(), SECRET);
    let post = |event: &str| common::post_event(&url, SECRET, common::problem_sent(event));

    // Two tickets, then the third problem is held back and opens the safety ticket.
    let first = tagged(&post(&events[0]).await);
    let second = tagged(&post(&events[1]).await);
    assert_eq!(
        post(&events[2]).await,
        (StatusCode::OK, json!({"tags": {}}))
    );
    let safety = safety_tickets(&core).await;
    assert_eq!(safety.len(), 1, "{safety:?}");
    let safety = &safety[0];
    let ticket = core.ticket(safety.number).await;
    assert_eq!(ticket["customer"]["name"], "Northwind Help Desk");
    assert_eq!(ticket["priority"], "urgent");
    assert_eq!(ticket["requester"], Value::Null);
    assert_eq!(
        ticket["title"],
        "Zabbix's alerts are held back: more than 2 tickets in 5 minutes"
    );
    let since = TicketReference::read(safety.reference.as_deref().unwrap())
        .and_then(|r| r.safety())
        .unwrap();
    let description = &core.addon_notes(safety.number).await[0];
    assert!(
        description.contains(&format!(
            "stopped opening tickets at {}: 1 problem the rules open was held back.",
            hd_zabbix::alerts::utc(since.unix_timestamp())
        )),
        "{description}"
    );
    assert!(alerts.holding());
    // The same problem again, and the next, get no ticket; the problems with one keep it.
    assert_eq!(
        post(&events[2]).await,
        (StatusCode::OK, json!({"tags": {}}))
    );
    assert_eq!(
        post(&events[3]).await,
        (StatusCode::OK, json!({"tags": {}}))
    );
    assert_eq!(tagged(&post(&events[0]).await), first);
    let held = core.tickets().await;
    assert_eq!(held, {
        let mut numbers = vec![first, second, safety.number];
        numbers.sort();
        numbers
    });

    // While it waits, a recovery still adds its note and resolves as the rule says.
    let recovery = zabbix.recover(&problems[0].0, &events[0], "1").await;
    let answer = common::post_event(&url, SECRET, common::recovery_sent(&events[0])).await;
    assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})));
    assert_eq!(
        core.addon_notes(first).await.last().unwrap(),
        &format!(
            "The problem recovered in Zabbix at {}.",
            happened(&zabbix, &recovery).await
        )
    );
    assert_eq!(core.ticket(first).await["state"], "resolved");
    // And so does an update with a message.
    let answer = common::post_event(
        &url,
        SECRET,
        common::update_sent(&events[1], "Olga Operator (olga)", "commented", "On it"),
    )
    .await;
    assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})));
    assert_eq!(
        core.addon_notes(second).await.last().unwrap(),
        "Olga Operator (olga) updated the problem in Zabbix (commented): On it"
    );

    // The reconciliation holds back what the webhook would, and tells the safety ticket.
    let round = alerts.reconcile().await.unwrap();
    assert_eq!((round.opened, round.held_back), (0, 3), "{round:?}");
    assert!(round.holding);
    assert_eq!(
        core.addon_notes(safety.number).await.last().unwrap(),
        &safety::held_back_note(since, 3)
    );

    // Started again, the add-on still waits: it reads the safety ticket at startup.
    let restarted = zabbix.alerts(&core, rules()).with_limit(TWO);
    let round = restarted.reconcile().await.unwrap();
    assert_eq!((round.opened, round.held_back), (0, 3), "{round:?}");
    assert!(round.holding);
    assert_eq!(core.tickets().await, held);
    let restarted = Arc::new(restarted);
    // On the same port, when the tests are given one.
    webhook.abort();
    let _ = webhook.await;
    let (listener, url, _) = common::webhook_listener().await;
    common::serve_webhook(listener, restarted.clone(), SECRET);
    let answer = common::post_event(&url, SECRET, common::problem_sent(&events[3])).await;
    assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})));
    assert_eq!(core.tickets().await, held);

    // An agent resolves the safety ticket: the problems still active get their tickets, counted
    // under the limit again, and one more than it starts another wait with a safety ticket of
    // its own.
    core.set_state(safety.number, "resolved").await;
    let round = restarted.reconcile().await.unwrap();
    assert_eq!((round.opened, round.held_back), (2, 1), "{round:?}");
    assert!(round.holding);
    let third = ticket_of(&core, &events[2]).await;
    let fourth = ticket_of(&core, &events[3]).await;
    assert!(third > safety.number && fourth > safety.number);
    let again = safety_tickets(&core).await;
    let latest = again
        .iter()
        .find(|t| t.number != safety.number)
        .unwrap_or_else(|| panic!("no second safety ticket in {again:?}"));
    assert_ne!(latest.reference, safety.reference);
    assert_eq!(core.ticket(latest.number).await["priority"], "urgent");
    let no_ticket = core.addon().ticket_by_reference(&events[4]).await;
    assert_eq!(no_ticket.unwrap_err().status(), Some(404));

    // Closed, it lets the last one through, at the next problem of the webhook.
    core.set_state(latest.number, "closed").await;
    let fifth = tagged(&common::post_event(&url, SECRET, common::problem_sent(&events[4])).await);
    assert_eq!(fifth, ticket_of(&core, &events[4]).await);
    assert!(!restarted.holding());

    zabbix.clean_up().await;
}

/// One ticket in 5 minutes.
const ONE: Limit = Limit {
    tickets: 1,
    window: Duration::from_secs(300),
};

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn without_the_help_desk_s_own_organization_the_problems_are_held_back_all_the_same_and_the_safety_ticket_opens_once_it_exists()
 {
    let core = Core::start().await;
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
    let mut problems = Vec::new();
    for key in ["first", "second", "third"] {
        problems.push(trapped(&zabbix, &host, key, 3).await);
    }
    let mut events = Vec::new();
    for (item, trigger) in &problems {
        events.push(zabbix.raise(item, trigger, "500").await);
    }
    // The first problem opens its ticket, then the add-on restarts: the ticket counts in the
    // window all the same, which the core's tickets seed.
    let before = zabbix
        .alerts(&core, common::rules(EVERY_PROBLEM))
        .with_limit(ONE);
    let answer = before
        .receive(EventKind::Problem {
            event: events[0].clone(),
        })
        .await
        .unwrap();
    let first: i64 = answer.tags["hd_ticket"].parse().unwrap();
    drop(before);
    let alerts = Arc::new(
        zabbix
            .alerts(&core, common::rules(EVERY_PROBLEM))
            .with_limit(ONE),
    );
    let (listener, url, _) = common::webhook_listener().await;
    common::serve_webhook(listener, alerts.clone(), SECRET);

    for event in &events[1..] {
        let answer = common::post_event(&url, SECRET, common::problem_sent(event)).await;
        assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})), "held back");
    }
    assert!(alerts.holding());
    assert_eq!(
        core.tickets().await,
        [first],
        "no safety ticket without the organization"
    );
    // The reconciliation goes on, recoveries included.
    let recovery = zabbix.recover(&problems[0].0, &events[0], "1").await;
    let round = alerts.reconcile().await.unwrap();
    assert_eq!(
        (round.opened, round.held_back, round.recovered),
        (0, 2, 1),
        "{round:?}"
    );
    assert!(round.holding);
    assert_eq!(
        core.addon_notes(first).await.last().unwrap(),
        &format!(
            "The problem recovered in Zabbix at {}.",
            happened(&zabbix, &recovery).await
        )
    );

    // Once the core has it, the next reconciliation opens the safety ticket, telling it both.
    core.help_desk("Northwind Help Desk");
    let round = alerts.reconcile().await.unwrap();
    assert!(round.holding, "{round:?}");
    let safety = safety_tickets(&core).await;
    assert_eq!(safety.len(), 1, "{safety:?}");
    let ticket = core.ticket(safety[0].number).await;
    assert_eq!(ticket["customer"]["name"], "Northwind Help Desk");
    let description = &core.addon_notes(safety[0].number).await[0];
    assert!(
        description.contains(": 2 problems the rules open were held back."),
        "{description}"
    );

    zabbix.clean_up().await;
}

/// Waits up to `within` for `until` to hold, saying `what` otherwise.
async fn eventually<F: Future<Output = bool>>(
    within: Duration,
    what: &str,
    mut until: impl FnMut() -> F,
) {
    let started = std::time::Instant::now();
    while !until().await {
        assert!(started.elapsed() < within, "{what}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The statuses of the alerts of `media_type` that Zabbix's event `event` set off.
async fn alert_statuses(zabbix: &TestZabbix, event: &str, media_type: &str) -> Vec<String> {
    let alerts: Vec<Value> = zabbix
        .admin
        .call(
            "alert.get",
            json!({"eventids": [event], "mediatypeids": [media_type],
                   "output": ["eventid", "p_eventid", "status"]}),
        )
        .await
        .unwrap();
    alerts
        .iter()
        .map(|a| a["status"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_recovery_the_webhook_received_is_not_handled_again_after_a_restart_and_one_it_missed_is_though_an_agent_touched_the_ticket()
 {
    let core = Core::start().await;
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
    let (received_item, received) = trapped(&zabbix, &host, "received", 4).await;
    let (missed_item, missed) = trapped(&zabbix, &host, "missed", 4).await;

    // Zabbix sends the problems to the webhook through the media type and an action.
    let secret = uuid::Uuid::new_v4().simple().to_string();
    let name = zabbix.name("Help desk");
    let (listener, _, reached) = common::webhook_listener().await;
    let webhook = common::serve_webhook(
        listener,
        zabbix
            .alerts(&core, hd_zabbix::rules::Rules::default_list())
            .with_media_type(&name),
        &secret,
    );
    let media_type = zabbix.media_type(&reached, &secret).await;
    let receiver = zabbix
        .user(
            "helpdesk",
            json!({"medias": [{"mediatypeid": media_type, "sendto": "helpdesk",
                               "active": 0, "severity": 63,
                               "period": "1-7,00:00-24:00"}]}),
        )
        .await;
    zabbix.action(&group, &media_type, &receiver).await;
    let addon = core.addon();
    let mut tickets = Vec::new();
    for (item, trigger) in [(&received_item, &received), (&missed_item, &missed)] {
        let event = zabbix.raise(item, trigger, "500").await;
        let number = ticket_of_within(&addon, &event, Duration::from_secs(90)).await;
        tickets.push((event, number));
    }
    let (received_event, received_ticket) = tickets[0].clone();
    let (missed_event, missed_ticket) = tickets[1].clone();

    // The webhook receives the first recovery: noted and resolved. An agent reopens the ticket.
    let received_recovery = zabbix.recover(&received_item, &received_event, "1").await;
    eventually(
        Duration::from_secs(60),
        "the first ticket resolved",
        || async { core.ticket(received_ticket).await["state"] == "resolved" },
    )
    .await;
    // Zabbix keeps the recovery's alert under the recovery event.
    eventually(
        Duration::from_secs(30),
        "the recovery's alert sent",
        || async { alert_statuses(&zabbix, &received_recovery, &media_type).await == ["1"] },
    )
    .await;
    core.set_state(received_ticket, "open").await;
    let received_notes = core.addon_notes(received_ticket).await.len();

    // The webhook goes down; the second problem recovers, and Zabbix gives up sending it. Then
    // an agent works on its ticket.
    webhook.abort();
    let _ = webhook.await;
    let missed_recovery = zabbix.recover(&missed_item, &missed_event, "1").await;
    eventually(
        Duration::from_secs(120),
        "the second recovery's alert failed",
        || async { alert_statuses(&zabbix, &missed_recovery, &media_type).await == ["2"] },
    )
    .await;
    core.work_on(missed_ticket).await;

    // Started again, the reconciliation handles the missed recovery only.
    let round = zabbix
        .alerts(&core, hd_zabbix::rules::Rules::default_list())
        .with_media_type(&name)
        .reconcile()
        .await
        .unwrap();
    assert_eq!((round.recovered, round.failed), (1, 0), "{round:?}");
    assert_eq!(
        core.addon_notes(missed_ticket).await.last().unwrap(),
        &format!(
            "The problem recovered in Zabbix at {}.",
            happened(&zabbix, &missed_recovery).await
        )
    );
    assert_eq!(
        core.ticket(missed_ticket).await["state"],
        "open",
        "an agent worked on it"
    );
    assert_eq!(
        core.ticket(received_ticket).await["state"],
        "open",
        "the agent reopened it"
    );
    assert_eq!(
        core.addon_notes(received_ticket).await.len(),
        received_notes
    );

    zabbix.clean_up().await;
}

/// The ticket the add-on opened for the problem of `event`, waiting up to `within` for it.
async fn ticket_of_within(addon: &hd_addon_sdk::Client, event: &str, within: Duration) -> i64 {
    let started = std::time::Instant::now();
    loop {
        if let Ok(ticket) = addon.ticket_by_reference(event).await {
            return ticket.number;
        }
        assert!(started.elapsed() < within, "no ticket for problem {event}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// How often the stub Zabbix answered `event.get` for the problem 4213.
static PROBLEM_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A Zabbix with the host SRV-01 in the group Brightfield and its problem 4213, still open the
/// first time it is read and recovered by the event 4220 from then on, as when the problem
/// recovers while the reconciliation opens its ticket.
async fn zabbix_recovering_meanwhile() -> String {
    let app = axum::Router::new().route(
        "/api_jsonrpc.php",
        // The add-on sends application/json-rpc, which axum's Json does not take.
        axum::routing::post(|body: axum::body::Bytes| async move {
            let call: Value = serde_json::from_slice(&body).unwrap();
            let first_event = call["params"]["eventids"][0].as_str();
            let result = match (call["method"].as_str().unwrap_or_default(), first_event) {
                ("hostgroup.get", _) => json!([{"groupid": "22", "name": "Brightfield"}]),
                ("problem.get", _) => {
                    json!([{"eventid": "4213", "suppressed": "0", "cause_eventid": "0"}])
                }
                ("host.get", _) => json!([{"hostid": "10683", "host": "srv-01", "name": "SRV-01",
                    "hostgroups": [{"groupid": "22", "name": "Brightfield"}]}]),
                ("event.get", Some("4213")) => {
                    let reads = PROBLEM_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let recovery = if reads == 0 { "0" } else { "4220" };
                    json!([{"eventid": "4213", "value": "1", "severity": "4",
                            "name": "Mail queue is too long", "clock": "1795801516",
                            "opdata": "", "r_eventid": recovery, "tags": [],
                            "hosts": [{"hostid": "10683", "host": "srv-01", "name": "SRV-01"}]}])
                }
                ("event.get", Some("4220")) => json!([{"clock": "1795801522", "userid": "0"}]),
                _ => json!([]),
            };
            axum::Json(json!({"jsonrpc": "2.0", "result": result, "id": call["id"]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
#[ignore = "needs the test database"]
async fn a_problem_that_recovers_while_the_reconciliation_opens_its_ticket_is_handled_as_its_recovery()
 {
    let core = Core::start().await;
    let brightfield = core.customer("Brightfield").await;
    core.addon()
        .replace_names(&hd_addon_sdk::SystemNames {
            system: "Zabbix".into(),
            names: vec![hd_addon_sdk::SystemName::top("Brightfield")],
        })
        .await
        .unwrap();
    core.reference(brightfield, "Brightfield").await;
    let zabbix = hd_zabbix::zabbix::Zabbix::new(
        &zabbix_recovering_meanwhile().await,
        "zabbix-token",
        Duration::from_secs(5),
    )
    .unwrap();
    let alerts = hd_zabbix::webhook::Alerts::new(
        core.addon(),
        zabbix,
        "Zabbix",
        hd_zabbix::rules::Rules::default_list(),
    );

    // The webhook's recovery came while there was no ticket, so Zabbix took it as sent.
    let round = alerts.reconcile().await.unwrap();
    assert_eq!((round.opened, round.failed), (1, 0), "{round:?}");
    let number = ticket_of(&core, "4213").await;
    assert_eq!(
        core.addon_notes(number).await.last().unwrap(),
        "The problem recovered in Zabbix at 2026-11-27 17:45:22 UTC."
    );
    assert_eq!(core.ticket(number).await["state"], "resolved");
}
