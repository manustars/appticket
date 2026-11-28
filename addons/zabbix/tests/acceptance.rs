//! The acceptance of the Zabbix add-on, end to end: the `hd-zabbix` binary as an install runs it,
//! with a configuration file and its secrets, between the Zabbix in containers and the real core.
//! Zabbix sends the problem and its recovery to the add-on's webhook itself, through the imported
//! media type and an action; the core tells the add-on that an agent resolved the ticket. Ignored
//! by default, since it needs Zabbix (`HD_TEST_ZABBIX_URL`), the test database server
//! (`DATABASE_URL`) and the core's binary (`cargo build -p hd-core`); CI runs it.

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{Core, TestZabbix};
use serde_json::{Value, json};

/// The add-on's binary, run in a directory of its own with the configuration file it is given
/// and nothing of the test's environment but its secrets; stopped with it.
struct Addon {
    process: Child,
    dir: PathBuf,
}

impl Addon {
    fn start(config: &str, webhook_secret: &str, core: &Core, zabbix: &TestZabbix) -> Addon {
        // Outside the repository, so that no .env of a checkout is read.
        let dir = std::env::temp_dir().join(format!("hd-zabbix-addon-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("webhook-secret"), format!("{webhook_secret}\n")).unwrap();
        std::fs::write(dir.join("zabbix.toml"), config).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_hd-zabbix"));
        command.current_dir(&dir).env_clear();
        for name in ["PATH", "SystemRoot", "SYSTEMROOT", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let process = command
            .env("HD_ZABBIX_CONFIG", dir.join("zabbix.toml"))
            .env("HD_ZABBIX_TOKEN", &core.token)
            .env("HD_ZABBIX_EVENT_SECRET", &core.event_secret)
            .env("HD_ZABBIX_API_TOKEN", &zabbix.api_token)
            .env("HD_ZABBIX_LOG", "info")
            .stdout(std::fs::File::create(dir.join("addon.log")).unwrap())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Addon { process, dir }
    }

    /// What the add-on logged so far, for a failure's message.
    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("addon.log")).unwrap_or_default()
    }

    /// Fails when the add-on has stopped.
    fn running(&mut self) {
        if let Some(status) = self.process.try_wait().unwrap() {
            panic!("the add-on stopped with {status}: {}", self.log());
        }
    }
}

impl Drop for Addon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Waits up to `within` for `until` to answer something, checking that the add-on still runs.
async fn wait_for<T, F: Future<Output = Option<T>>>(
    addon: &mut Addon,
    within: Duration,
    what: &str,
    mut until: impl FnMut() -> F,
) -> T {
    let started = Instant::now();
    loop {
        addon.running();
        if let Some(found) = until().await {
            return found;
        }
        assert!(
            started.elapsed() < within,
            "{what}; the add-on's log:\n{}",
            addon.log()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_problem_raised_in_zabbix_opens_one_ticket_its_recovery_adds_a_note_and_resolving_the_ticket_acknowledges_the_event()
 {
    // The add-on's port, where the core reaches it at 127.0.0.1 and Zabbix's server at the host
    // HD_TEST_ZABBIX_WEBHOOK_HOST names; given to the add-on, which binds it itself.
    let (listener, url, reached) = common::webhook_listener().await;
    let listen = listener.local_addr().unwrap();
    drop(listener);
    let base = url.strip_suffix(hd_zabbix::webhook::PATH).unwrap();
    let core = Core::start_at(base).await;
    let mut zabbix = TestZabbix::connect().await;
    core.asset_kind("Server").await;
    let brightfield = core.customer("Brightfield").await;
    let group = zabbix.group("Brightfield").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let srv = zabbix.name("SRV-01");
    let host = zabbix.host("srv-01", &[&group], json!({"name": srv})).await;
    let key = "queue";
    let item = zabbix
        .item(&host, key, json!({"key_": key, "type": 2, "value_type": 3}))
        .await;
    let technical = zabbix.host_name("srv-01");
    // High: the default rules open a ticket with priority high.
    let trigger = zabbix
        .trigger_with(
            "Mail queue is too long",
            &format!("last(/{technical}/{key})>100"),
            4,
            json!({}),
        )
        .await;

    // Zabbix's side, as the README sets it up: the media type with the webhook's URL and the
    // secret in the global macro, a user with the media, and an action sending problems,
    // recoveries and updates.
    let secret = uuid::Uuid::new_v4().simple().to_string();
    let media_type = zabbix.media_type(&reached, &secret).await;
    let receiver = zabbix
        .user(
            "helpdesk",
            json!({"medias": [{"mediatypeid": media_type, "sendto": "helpdesk",
                               "active": 0, "severity": 63,
                               "period": "1-7,00:00-24:00"}]}),
        )
        .await;
    let action = zabbix.action(&group, &media_type, &receiver).await;

    // The add-on with the default rules.
    let config = format!(
        r#"
[core]
url = "{core_url}"

[zabbix]
url = "{zabbix_url}"

[assets]
default_kind = "Server"

[addon]
listen = "{listen}"

[webhook]
secret_file = "webhook-secret"
media_type = "{media_type_name}"
"#,
        core_url = core.url,
        zabbix_url = common::zabbix_url(),
        media_type_name = zabbix.name("Help desk"),
    );
    let mut addon = Addon::start(&config, &secret, &core, &zabbix);
    // Its first round makes the host an asset of Brightfield.
    wait_for(
        &mut addon,
        Duration::from_secs(60),
        "the host did not become an asset",
        || async {
            let assets = core.assets(brightfield).await;
            assets.iter().any(|a| a.0 == srv).then_some(())
        },
    )
    .await;

    // A problem raised: Zabbix sends it, and the add-on opens one ticket.
    let event = zabbix.raise(&item, &trigger, "500").await;
    let ticket_api = core.addon();
    let number = wait_for(
        &mut addon,
        Duration::from_secs(90),
        &format!("no ticket for problem {event}"),
        || async { ticket_api.ticket_by_reference(&event).await.ok() },
    )
    .await
    .number;
    let ticket = core.ticket(number).await;
    assert_eq!(ticket["customer"]["name"], "Brightfield");
    assert_eq!(ticket["priority"], "high");
    assert_eq!(ticket["title"], format!("{srv}: Mail queue is too long"));
    // Zabbix has sent the problem, and taken the ticket's number into the event's tags.
    wait_for(
        &mut addon,
        Duration::from_secs(30),
        "the event has no tag hd_ticket",
        || async {
            zabbix
                .event_tags(&event)
                .await
                .contains(&("hd_ticket".to_owned(), number.to_string()))
                .then_some(())
        },
    )
    .await;
    assert_eq!(core.tickets().await, [number], "one ticket");

    // An agent works on it, so that the recovery leaves it open.
    core.work_on(number).await;

    // The problem recovers: Zabbix sends the recovery, which adds a note.
    zabbix.recover(&item, &event, "1").await;
    let notes = wait_for(
        &mut addon,
        Duration::from_secs(90),
        "no note of the recovery",
        || async {
            let notes = core.addon_notes(number).await;
            notes
                .last()
                .is_some_and(|n| n.starts_with("The problem recovered in Zabbix at "))
                .then_some(notes)
        },
    )
    .await;
    assert_eq!(
        notes.len(),
        2,
        "the description and the recovery's: {notes:?}"
    );
    assert_eq!(
        core.ticket(number).await["state"],
        "open",
        "an agent has worked on it"
    );

    // The agent resolves it: the add-on acknowledges the event in Zabbix.
    core.set_state(number, "resolved").await;
    core.wait_for_deliveries().await;
    let acknowledged = zabbix.acknowledges(&event).await;
    let [(user, action_bits, message)] = &acknowledged[..] else {
        panic!("one acknowledgement, not {acknowledged:?}");
    };
    assert_eq!(user, "Admin", "the add-on's Zabbix user");
    assert_eq!(*action_bits, 6, "acknowledged with a message");
    assert!(
        message.starts_with(&format!(
            "Ticket {number} was resolved in the help desk by Ada Admin. [help desk change "
        )),
        "{message}"
    );

    // What Zabbix sent: the problem, its recovery and the add-on's acknowledgement, each
    // delivered; still one ticket, resolved, with no note added since.
    wait_for(
        &mut addon,
        Duration::from_secs(90),
        "Zabbix did not deliver the action's alerts",
        || async {
            let sent: Vec<Value> = zabbix
                .admin
                .call(
                    "alert.get",
                    json!({"actionids": [action], "output": ["status", "error"]}),
                )
                .await
                .unwrap();
            (sent.len() >= 3 && sent.iter().all(|a| a["status"] == "1")).then_some(())
        },
    )
    .await;
    assert_eq!(core.tickets().await, [number], "still one ticket");
    assert_eq!(core.ticket(number).await["state"], "resolved");
    assert_eq!(core.addon_notes(number).await.len(), 2);

    drop(addon);
    zabbix.clean_up().await;
}
