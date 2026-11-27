//! The `check` command against the Zabbix in containers and the real core: an alert raised on a
//! trapper item, given by Zabbix's event id or by its fields, meets the rule of the configuration
//! file it should. Ignored by default, since it needs Zabbix (`HD_TEST_ZABBIX_URL`), the test
//! database server (`DATABASE_URL`) and the core's binary (`cargo build -p hd-core`); CI runs it.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Core, TestZabbix};
use serde_json::{Value, json};

const RULES: &str = r#"
[[alerts.rules]]
name = "Backups"
trigger_contains = "backup"
outcome = "ignore"

[[alerts.rules]]
name = "Mail"
customer = "Brightfield"
min_severity = "average"
tags = [{ tag = "service", value = "mail" }]
outcome = "open"
priority = "high"
service = "Hosted mail"
"#;

/// Runs `hd-zabbix check` with these arguments, as an install runs it: in a folder of its own,
/// with only the configuration file and the two tokens. Answers what it printed, or its error.
fn check(dir: &Path, core: &Core, zabbix: &TestZabbix, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hd-zabbix"));
    command.arg("check").args(args).current_dir(dir).env_clear();
    for name in ["PATH", "SystemRoot", "SYSTEMROOT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command
        .env("HD_ZABBIX_CONFIG", dir.join("zabbix.toml"))
        .env("HD_ZABBIX_TOKEN", &core.token)
        .env("HD_ZABBIX_API_TOKEN", &zabbix.api_token)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(stderr)
    }
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn check_tells_the_rule_an_alert_of_zabbix_meets_by_event_id_or_by_its_fields() {
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    let brightfield = core.customer("Brightfield").await;
    zabbix.group("Brightfield").await;
    let offices = zabbix.group("Brightfield/Offices").await;
    let lab = zabbix.group("Lab").await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let technical = format!("{}-srv-01", zabbix.prefix);
    let visible = zabbix.name("SRV-01");
    let host = zabbix
        .host(
            "srv-01",
            &[&offices],
            json!({"name": visible, "tags": [{"tag": "site", "value": "north"}]}),
        )
        .await;
    let item: Value = zabbix
        .admin
        .call(
            "item.create",
            json!({"hostid": host, "name": "Mail queue", "key_": "hd.queue",
                   "type": 2, "value_type": 3,
                   "tags": [{"tag": "component", "value": "queue"}]}),
        )
        .await
        .unwrap();
    let item = item["itemids"][0].as_str().unwrap().to_owned();
    // A macro in the name: the event has it resolved.
    let trigger: Value = zabbix
        .admin
        .call(
            "trigger.create",
            json!({"description": "Mail queue of {HOST.NAME} is too long",
                   "expression": format!("last(/{technical}/hd.queue)>100"),
                   "priority": 4,
                   "tags": [{"tag": "service", "value": "mail"}]}),
        )
        .await
        .unwrap();
    let trigger = trigger["triggerids"][0].as_str().unwrap().to_owned();
    zabbix.host("lab-02", &[&lab], json!({})).await;

    // Zabbix takes values for the item once its server has read the new configuration.
    let started = Instant::now();
    let event = loop {
        let pushed: Value = zabbix
            .admin
            .call("history.push", json!([{"itemid": item, "value": 500}]))
            .await
            .unwrap();
        let events: Vec<Value> = zabbix
            .admin
            .call(
                "event.get",
                json!({"objectids": [trigger], "value": 1, "output": ["eventid"]}),
            )
            .await
            .unwrap();
        if let Some(event) = events.first() {
            break event["eventid"].as_str().unwrap().to_owned();
        }
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "no problem after pushing values: {pushed}"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    };

    let dir = std::env::temp_dir().join(format!("hd-zabbix-check-{}", zabbix.prefix));
    std::fs::create_dir_all(&dir).unwrap();
    let config = format!(
        "[core]\nurl = \"{}\"\n\n[zabbix]\nurl = \"{}\"\n\n[assets]\ndefault_kind = \"Server\"\n",
        core.url,
        common::zabbix_url()
    );
    std::fs::write(dir.join("zabbix.toml"), format!("{config}{RULES}")).unwrap();
    let p = |name: &str| zabbix.name(name);

    // By event id: severity, tags (the trigger's, the item's and the host's) and name come from
    // Zabbix, the customer from the external reference of the group the host's group is nested
    // in.
    assert_eq!(
        check(&dir, &core, &zabbix, &["--event", &event]).unwrap(),
        format!(
            "Host \"{visible}\", in the host group \"{}\": an asset of Brightfield.\n\
             Alert: Zabbix's event {event}, severity high, trigger \"Mail queue of {visible} is too \
             long\", the tags \"component=queue\", \"service=mail\" and \"site=north\".\n\
             Rule 2 (\"Mail\") matches: a ticket opened with priority high, linking the service \
             \"Hosted mail\" when the customer uses it; on recovery, a note, and the ticket \
             resolved only when no agent has worked on it; once an agent resolves or closes the \
             ticket, the problem left open in Zabbix.\n",
            p("Brightfield/Offices")
        )
    );

    // By its fields, the host by its technical or its visible name.
    let by_fields = check(
        &dir,
        &core,
        &zabbix,
        &[
            "--host",
            &technical,
            "--severity",
            "warning",
            "--tag",
            "service=mail",
            "--trigger",
            "Mail queue is too long",
        ],
    )
    .unwrap();
    assert!(
        by_fields.ends_with(
            "Alert: severity warning, trigger \"Mail queue is too long\", the tags \
             \"service=mail\".\nNo rule matches: the alert is ignored.\n"
        ),
        "{by_fields}"
    );
    let backup = check(
        &dir,
        &core,
        &zabbix,
        &[
            "--host",
            &visible,
            "--severity",
            "disaster",
            "--trigger",
            "Backup failed",
        ],
    )
    .unwrap();
    assert!(
        backup.ends_with("Rule 1 (\"Backups\") matches: the alert is ignored.\n"),
        "{backup}"
    );

    // A host of no customer's group: the rules still tell, and the report says it opens nothing.
    let lab_check = check(
        &dir,
        &core,
        &zabbix,
        &[
            "--host",
            &format!("{}-lab-02", zabbix.prefix),
            "--severity",
            "high",
            "--trigger",
            "Backup failed",
        ],
    )
    .unwrap();
    assert!(
        lab_check.starts_with(&format!(
            "Host \"{}-lab-02\", in the host group \"{}\": not imported, since no customer \
             covers its groups: its alerts open no ticket.\n",
            zabbix.prefix,
            p("Lab")
        )),
        "{lab_check}"
    );
    assert!(
        lab_check.ends_with(
            "\nRule 1 (\"Backups\") would match once the host is imported, an asset of a \
             customer: the alert is ignored.\n"
        ),
        "{lab_check}"
    );

    // Without rules, the default list decides.
    std::fs::write(dir.join("zabbix.toml"), &config).unwrap();
    let default = check(&dir, &core, &zabbix, &["--event", &event]).unwrap();
    assert!(
        default.contains(
            "Rule 2 (\"High\") of the default list, the configuration having no rules, \
             matches: a ticket opened with priority high;"
        ),
        "{default}"
    );

    // What does not exist is refused with a sentence.
    let missing = check(
        &dir,
        &core,
        &zabbix,
        &["--host", &p("nowhere"), "--severity", "high"],
    )
    .unwrap_err();
    assert!(
        missing.contains(&format!(
            "Zabbix has no host named \"{}\" that the add-on's user can read",
            p("nowhere")
        )),
        "{missing}"
    );
    let unknown = check(&dir, &core, &zabbix, &["--event", "999999999"]).unwrap_err();
    assert!(
        unknown.contains("Zabbix has no event 999999999 of a trigger's problem"),
        "{unknown}"
    );
    // An invalid rule stops it, as it stops the add-on.
    std::fs::write(
        dir.join("zabbix.toml"),
        format!("{config}{}", RULES.replace("\"average\"", "\"severe\"")),
    )
    .unwrap();
    let invalid = check(&dir, &core, &zabbix, &["--event", &event]).unwrap_err();
    assert!(
        invalid.contains("alerts.rules: rule 2 (\"Mail\"): min_severity is \"severe\""),
        "{invalid}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
    zabbix.clean_up().await;
}
