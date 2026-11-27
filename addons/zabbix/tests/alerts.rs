//! Alerts in, against the Zabbix in containers and the real core: problems raised on trapper
//! items reach the add-on's webhook as Zabbix's media type sends them, and the rules turn them
//! into tickets; recoveries add a note and resolve as the rule says, and updates add a note.
//! Ignored by default, since they need Zabbix (`HD_TEST_ZABBIX_URL`), the test database server
//! (`DATABASE_URL`) and the core's binary (`cargo build -p hd-core`); CI runs them.

mod common;

use std::time::{Duration, Instant};

use common::{Core, TestZabbix};
use hd_addon_sdk::Client;
use hd_zabbix::kinds::Kinds;
use reqwest::StatusCode;
use serde_json::{Value, json};

const SECRET: &str = "a-webhook-secret-of-the-test";

fn server() -> Kinds {
    Kinds {
        mappings: vec![],
        default: "Server".into(),
    }
}

/// A trapper item of the host, with a trigger on it that has a problem while its last value is
/// above 100; answers the item's and the trigger's ids.
async fn trapped(
    zabbix: &TestZabbix,
    host: &str,
    key: &str,
    trigger: &str,
    severity: u8,
    fields: Value,
) -> (String, String) {
    let item = zabbix
        .item(
            host,
            key,
            json!({"key_": key, "type": 2, "value_type": 3,
                   "tags": [{"tag": "component", "value": key}]}),
        )
        .await;
    let technical: Vec<Value> = zabbix
        .admin
        .call("host.get", json!({"output": ["host"], "hostids": [host]}))
        .await
        .unwrap();
    let technical = technical[0]["host"].as_str().unwrap();
    let trigger = zabbix
        .trigger_with(
            trigger,
            &format!("last(/{technical}/{key})>100"),
            severity,
            fields,
        )
        .await;
    (item, trigger)
}

/// The ticket the add-on opened for the problem of `event`, waiting up to `within` for it.
async fn ticket_of(addon: &Client, event: &str, within: Duration) -> Option<i64> {
    let started = Instant::now();
    loop {
        if let Ok(ticket) = addon.ticket_by_reference(event).await {
            return Some(ticket.number);
        }
        if started.elapsed() > within {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

const RULES: &str = r#"
[[alerts.rules]]
name = "Backups"
trigger_contains = "backup"
outcome = "ignore"

[[alerts.rules]]
name = "Mail"
tags = [{ tag = "service", value = "mail" }]
outcome = "open"
priority = "high"
service = "Hosted mail"

[[alerts.rules]]
name = "Disks"
trigger_contains = "disk"
outcome = "open"
priority = "urgent"
service = "Cloud backup"

[[alerts.rules]]
min_severity = "high"
outcome = "open"
priority = "normal"
"#;

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_problem_the_rules_open_makes_one_ticket_and_one_they_ignore_or_of_a_host_not_imported_none()
 {
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    core.asset_kind("Server").await;
    let brightfield = core.customer("Brightfield").await;
    let harbor = core.customer("Harbor Lane").await;
    core.service("Hosted mail", brightfield).await;
    // Brightfield does not use it.
    core.service("Cloud backup", harbor).await;
    let offices = zabbix.group("Brightfield/Offices").await;
    let docks = zabbix.group("Harbor Lane").await;
    let lab = zabbix.group("Lab").await;
    core.reference(brightfield, &zabbix.name("Brightfield/Offices"))
        .await;
    core.reference(harbor, &zabbix.name("Harbor Lane")).await;
    let srv = zabbix.name("SRV-01");
    let host = zabbix
        .host(
            "srv-01",
            &[&offices],
            json!({"name": srv, "tags": [{"tag": "site", "value": "north"}]}),
        )
        .await;
    let unowned = zabbix.host("lab-02", &[&lab], json!({})).await;
    let shared = zabbix.host("db-03", &[&offices, &docks], json!({})).await;
    zabbix.addon(&core, server()).run().await.unwrap();

    let (mail_item, mail) = trapped(
        &zabbix,
        &host,
        "queue",
        "Mail queue of {HOST.NAME} is too long",
        4,
        json!({"tags": [{"tag": "service", "value": "mail"}],
               "opdata": "Queue: {ITEM.LASTVALUE1}"}),
    )
    .await;
    let (disk_item, disk) = trapped(&zabbix, &host, "disk", "Disk almost full", 3, json!({})).await;
    let (backup_item, backup) = trapped(
        &zabbix,
        &host,
        "backup",
        "Nightly backup failed",
        5,
        json!({}),
    )
    .await;
    let (lab_item, lab_trigger) =
        trapped(&zabbix, &unowned, "lab", "Lab is down", 5, json!({})).await;
    let (shared_item, shared_trigger) =
        trapped(&zabbix, &shared, "shared", "Database is down", 5, json!({})).await;

    let (listener, url, _) = common::webhook_listener().await;
    common::serve_webhook(listener, zabbix.alerts(&core, common::rules(RULES)), SECRET);

    // The rule Mail opens a ticket for the customer of the host's asset, linked to it and to the
    // service, and its number comes back as the tag; the same event again gets the same ticket.
    let event = zabbix.raise(&mail_item, &mail, "500").await;
    let (status, answer) = common::post_event(&url, SECRET, common::problem_sent(&event)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let number = answer["tags"]["hd_ticket"]
        .as_str()
        .unwrap_or_else(|| panic!("no ticket in {answer}"))
        .parse::<i64>()
        .unwrap();
    let again = common::post_event(&url, SECRET, common::problem_sent(&event)).await;
    assert_eq!(again, (StatusCode::OK, answer.clone()));
    assert_eq!(core.tickets().await, [number], "one ticket");

    let ticket = core.ticket(number).await;
    assert_eq!(ticket["customer"]["name"], "Brightfield");
    assert_eq!(ticket["requester"], Value::Null);
    assert_eq!(ticket["opened_by_addon"], common::ADDON);
    assert_eq!(ticket["priority"], "high");
    assert_eq!(ticket["state"], "open");
    assert_eq!(
        ticket["title"],
        format!("{srv}: Mail queue of {srv} is too long")
    );
    let assets: Vec<&Value> = ticket["assets"].as_array().unwrap().iter().collect();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0]["name"], srv.as_str());
    assert_eq!(ticket["services"][0]["name"], "Hosted mail");
    let started: Vec<Value> = zabbix
        .admin
        .call(
            "event.get",
            json!({"eventids": [event], "output": ["clock"]}),
        )
        .await
        .unwrap();
    let started = hd_zabbix::alerts::utc(started[0]["clock"].as_str().unwrap().parse().unwrap());
    assert_eq!(
        core.addon_notes(number).await,
        [format!(
            "Zabbix's problem {event} on {srv}, opened by rule 2 (\"Mail\").\n\
             Severity: High\n\
             Started: {started}\n\
             Tags: component=queue, service=mail, site=north\n\
             Operational data: Queue: 500"
        )]
    );

    // A service the customer does not use is left out, and the description says so.
    let event = zabbix.raise(&disk_item, &disk, "500").await;
    let (status, answer) = common::post_event(&url, SECRET, common::problem_sent(&event)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let number = answer["tags"]["hd_ticket"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let ticket = core.ticket(number).await;
    assert_eq!(ticket["priority"], "urgent");
    assert_eq!(ticket["services"], json!([]));
    assert_eq!(ticket["assets"][0]["name"], srv.as_str());
    let description = &core.addon_notes(number).await[0];
    assert!(
        description.ends_with(
            "The rule links the service \"Cloud backup\", which Brightfield does not use: the \
             ticket links no service."
        ),
        "{description}"
    );

    // A problem the rules ignore, and those of a host no customer covers or two do, open none.
    let before = core.tickets().await;
    for (item, trigger) in [
        (&backup_item, &backup),
        (&lab_item, &lab_trigger),
        (&shared_item, &shared_trigger),
    ] {
        let event = zabbix.raise(item, trigger, "500").await;
        let answer = common::post_event(&url, SECRET, common::problem_sent(&event)).await;
        assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})), "{trigger}");
    }
    assert_eq!(core.tickets().await, before);

    zabbix.clean_up().await;
}

/// The rules of the test of recoveries, each by a text of the trigger's name.
const RECOVERY_RULES: &str = r#"
[[alerts.rules]]
name = "Always"
trigger_contains = "always"
outcome = "open"
priority = "normal"
on_recovery = "resolve"

[[alerts.rules]]
name = "Never"
trigger_contains = "never"
outcome = "open"
priority = "normal"
on_recovery = "note"

[[alerts.rules]]
name = "By default"
outcome = "open"
priority = "normal"
"#;

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_recovery_adds_a_note_and_resolves_as_the_rule_says_and_an_update_with_a_message_a_note()
{
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    core.asset_kind("Server").await;
    let brightfield = core.customer("Brightfield").await;
    let offices = zabbix.group("Brightfield").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let host = zabbix
        .host(
            "srv-01",
            &[&offices],
            json!({"name": zabbix.name("SRV-01")}),
        )
        .await;
    let mut problems = Vec::new();
    for (key, name, worked_on) in [
        ("untouched", "Untouched by default", false),
        ("taken", "Taken by default", true),
        ("always", "Resolve always", true),
        ("never", "Resolve never", false),
    ] {
        let (item, trigger) = trapped(&zabbix, &host, key, name, 4, json!({})).await;
        problems.push((item, trigger, worked_on));
    }

    let (listener, url, _) = common::webhook_listener().await;
    common::serve_webhook(
        listener,
        zabbix.alerts(&core, common::rules(RECOVERY_RULES)),
        SECRET,
    );
    let mut opened = Vec::new();
    for (item, trigger, worked_on) in &problems {
        let event = zabbix.raise(item, trigger, "500").await;
        let (status, answer) = common::post_event(&url, SECRET, common::problem_sent(&event)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let number: i64 = answer["tags"]["hd_ticket"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        if *worked_on {
            core.work_on(number).await;
        }
        opened.push((item, event, number));
    }

    // An update with a message from a Zabbix user adds a note; one without a message, or by the
    // add-on's own user (the test's API token is Admin's), does not.
    let (_, event, number) = &opened[0];
    for (user, action, message) in [
        (
            "Olga Operator (olga)",
            "acknowledged and commented",
            "Looking at it",
        ),
        ("Zabbix Administrator (Admin)", "commented", "Ticket noted"),
        (
            "Olga Operator (olga)",
            "acknowledged",
            "{EVENT.UPDATE.MESSAGE}",
        ),
    ] {
        let sent = common::update_sent(event, user, action, message);
        let answer = common::post_event(&url, SECRET, sent).await;
        assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})));
    }
    assert_eq!(
        core.addon_notes(*number).await[1..],
        [
            "Olga Operator (olga) updated the problem in Zabbix (acknowledged and commented): \
          Looking at it"
        ]
    );

    for (item, event, number) in &opened {
        let recovery = zabbix.recover(item, event, "1").await;
        let answer = common::post_event(&url, SECRET, common::recovery_sent(event)).await;
        assert_eq!(answer, (StatusCode::OK, json!({"tags": {}})));
        let recovered: Vec<Value> = zabbix
            .admin
            .call(
                "event.get",
                json!({"eventids": [recovery], "output": ["clock"]}),
            )
            .await
            .unwrap();
        let at = hd_zabbix::alerts::utc(recovered[0]["clock"].as_str().unwrap().parse().unwrap());
        let notes = core.addon_notes(*number).await;
        assert_eq!(
            notes.last().unwrap(),
            &format!("The problem recovered in Zabbix at {at}."),
            "ticket {number}"
        );
    }
    let states: Vec<String> = {
        let mut states = Vec::new();
        for (_, _, number) in &opened {
            states.push(
                core.ticket(*number).await["state"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        states
    };
    assert_eq!(
        states,
        [
            "resolved", // by default, no agent worked on it
            "open",     // by default, an agent worked on it
            "resolved", // resolve, though an agent worked on it
            "open",     // note: never resolved
        ]
    );

    zabbix.clean_up().await;
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_problem_raised_in_zabbix_opens_one_ticket_through_the_media_type_and_its_recovery_adds_a_note()
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
    let (item, trigger) = trapped(
        &zabbix,
        &host,
        "queue",
        "Mail queue is too long",
        4,
        json!({}),
    )
    .await;

    let (listener, _, reached) = common::webhook_listener().await;
    common::serve_webhook(
        listener,
        zabbix.alerts(&core, hd_zabbix::rules::Rules::default_list()),
        SECRET,
    );
    let media_type = zabbix.media_type(&reached, SECRET).await;
    let receiver = zabbix
        .user(
            "helpdesk",
            json!({"medias": [{"mediatypeid": media_type, "sendto": "helpdesk",
                               "active": 0, "severity": 63,
                               "period": "1-7,00:00-24:00"}]}),
        )
        .await;
    zabbix.action(&group, &media_type, &receiver).await;
    let olga = zabbix
        .user("olga", json!({"name": "Olga", "surname": "Operator"}))
        .await;

    let event = zabbix.raise(&item, &trigger, "500").await;
    let addon = core.addon();
    let Some(number) = ticket_of(&addon, &event, Duration::from_secs(90)).await else {
        panic!(
            "no ticket for problem {event}; Zabbix's alerts: {}",
            zabbix.alerts_of(&event).await
        );
    };
    let ticket = core.ticket(number).await;
    assert_eq!(ticket["customer"]["name"], "Brightfield");
    assert_eq!(ticket["priority"], "high", "the default list's rule High");
    assert_eq!(ticket["title"], format!("{srv}: Mail queue is too long"));
    // The ticket's number in the event's tags, from the webhook's answer.
    let started = Instant::now();
    while !zabbix
        .event_tags(&event)
        .await
        .contains(&("hd_ticket".to_owned(), number.to_string()))
    {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the event's tags are {:?}",
            zabbix.event_tags(&event).await
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Olga's comment becomes a note; the add-on's own user's does not.
    let _: Value = zabbix
        .admin
        .call(
            "event.acknowledge",
            json!({"eventids": [event], "action": 4, "message": "Seen by the add-on's user"}),
        )
        .await
        .unwrap();
    let _: Value = zabbix
        .sign_in(&olga)
        .await
        .call(
            "event.acknowledge",
            json!({"eventids": [event], "action": 6, "message": "Looking at it"}),
        )
        .await
        .unwrap();
    let olga_note = format!(
        "Olga Operator ({olga}) updated the problem in Zabbix (acknowledged and commented): \
         Looking at it"
    );
    let started = Instant::now();
    while !core.addon_notes(number).await.contains(&olga_note) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the notes are {:?}; Zabbix's alerts: {}",
            core.addon_notes(number).await,
            zabbix.alerts_of(&event).await
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Its recovery adds a note and resolves it: no agent has worked on it.
    zabbix.recover(&item, &event, "1").await;
    let started = Instant::now();
    loop {
        let notes = core.addon_notes(number).await;
        if notes
            .last()
            .is_some_and(|n| n.starts_with("The problem recovered in Zabbix at "))
        {
            assert_eq!(
                notes.len(),
                3,
                "the description, Olga's and the recovery's: {notes:?}"
            );
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the notes are {notes:?}; Zabbix's alerts: {}",
            zabbix.alerts_of(&event).await
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let started = Instant::now();
    while core.ticket(number).await["state"] != "resolved" {
        assert!(started.elapsed() < Duration::from_secs(10), "not resolved");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(core.tickets().await, [number], "one ticket");

    zabbix.clean_up().await;
}
