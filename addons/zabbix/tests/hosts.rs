//! The add-on against the Zabbix in containers and the real core: host groups become the name list
//! agents pick external references from, and the hosts of the groups customers have become their
//! assets. Ignored by default, since they need Zabbix (`HD_TEST_ZABBIX_URL`), the test database
//! server (`DATABASE_URL`) and the core's binary (`cargo build -p hd-core`); CI runs them.

mod common;

use common::{ADDON, Core, TestZabbix};
use hd_zabbix::kinds::{KindMapping, Kinds};
use hd_zabbix::sync::Round;
use serde_json::{Value, json};

fn kinds(template: &str) -> Kinds {
    Kinds {
        mappings: vec![
            KindMapping {
                template: Some(template.into()),
                tag: None,
                value: None,
                kind: "Server".into(),
            },
            KindMapping {
                template: None,
                tag: Some("device".into()),
                value: Some("printer".into()),
                kind: "Printer".into(),
            },
        ],
        default: "Network device".into(),
    }
}

/// The names of the test's own groups in the add-on's name list, with the one each is nested in.
async fn listed(core: &Core, prefix: &str) -> Vec<(String, Option<String>)> {
    let lists = core.get("/api/external-references/names").await;
    let list = lists
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["addon"] == ADDON)
        .unwrap_or_else(|| panic!("no list of {ADDON} in {lists}"));
    assert_eq!(list["system"], "Zabbix");
    list["names"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["name"].as_str().unwrap().starts_with(prefix))
        .map(|n| {
            (
                n["name"].as_str().unwrap().to_owned(),
                n["parent"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn the_hosts_of_a_mapped_group_and_its_subgroups_become_assets_of_the_customer() {
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    let brightfield = core.customer("Brightfield").await;
    let harbor = core.customer("Harbor Lane").await;
    let old_quay = core.customer("Old Quay").await;
    for kind in ["Server", "Printer", "Network device"] {
        core.asset_kind(kind).await;
    }
    let bf = zabbix.group("Brightfield").await;
    let offices = zabbix.group("Brightfield/Offices").await;
    // Brightfield/Docks does not exist: Crane is nested in Brightfield.
    let crane = zabbix.group("Brightfield/Docks/Crane").await;
    let archive = zabbix.group("Brightfield/Archive").await;
    let unclaimed = zabbix.group("Unclaimed").await;
    let template = zabbix.template("Linux servers").await;
    zabbix
        .host(
            "srv-01",
            &[&offices],
            json!({"name": format!("{} SRV-01", zabbix.prefix),
                   "templates": [{"templateid": template}],
                   "inventory_mode": 0, "inventory": {"serialno_a": "SN-0042"}}),
        )
        .await;
    zabbix
        .host(
            "printer-02",
            &[&bf],
            json!({"tags": [{"tag": "device", "value": "printer"}], "inventory_mode": -1}),
        )
        .await;
    zabbix.host("crane-03", &[&crane], json!({})).await;
    zabbix.host("archive-04", &[&archive], json!({})).await;
    zabbix.host("lab-05", &[&unclaimed], json!({})).await;
    let addon = zabbix.addon(&core, kinds(&zabbix.name("Linux servers")));

    // The first round lists the groups; no customer has one yet, so nothing is imported.
    let round = addon.run().await.unwrap();
    assert_eq!((round.hosts, round.refused), (0, 0), "{round:?}");
    let p = |name: &str| zabbix.name(name);
    assert_eq!(
        listed(&core, &zabbix.prefix).await,
        [
            (p("Brightfield"), None),
            (p("Brightfield/Archive"), Some(p("Brightfield"))),
            (p("Brightfield/Docks/Crane"), Some(p("Brightfield"))),
            (p("Brightfield/Offices"), Some(p("Brightfield"))),
            (p("Unclaimed"), None),
        ]
    );

    // Agents pick the groups from the list.
    core.reference(brightfield, &p("Brightfield")).await;
    core.reference(harbor, &p("Brightfield/Docks/Crane")).await;
    core.reference(old_quay, &p("Brightfield/Archive")).await;
    let (status, json) = core
        .call("POST", &format!("/api/customers/{old_quay}/offboard"), None)
        .await;
    assert!(status.is_success(), "{json}");

    let round = addon.run().await.unwrap();
    assert_eq!(
        round,
        Round {
            names: round.names,
            hosts: 3,
            created: 3,
            ..Round::default()
        }
    );
    let zabbix_asset = |name: &str, kind: &str, serial: Option<&str>| {
        (
            name.to_owned(),
            kind.to_owned(),
            serial.map(str::to_owned),
            Some(ADDON.to_owned()),
            false,
        )
    };
    assert_eq!(
        core.assets(brightfield).await,
        [
            zabbix_asset(&p("SRV-01"), "Server", Some("SN-0042")),
            zabbix_asset(&format!("{}-printer-02", zabbix.prefix), "Printer", None),
        ]
    );
    assert_eq!(
        core.assets(harbor).await,
        [zabbix_asset(
            &format!("{}-crane-03", zabbix.prefix),
            "Network device",
            None
        )]
    );
    // An offboarded customer's group, inside Brightfield's, is imported for neither.
    assert_eq!(core.assets(old_quay).await, []);
    // The group no customer has shows in the add-on's state.
    let state = core.state().await;
    let uncovered: Vec<&Value> = state["names"]["uncovered"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["name"].as_str().unwrap().starts_with(&zabbix.prefix))
        .collect();
    assert_eq!(
        uncovered,
        [&json!({"name": p("Unclaimed"), "parent": null, "depth": 0})]
    );
    assert_eq!(state["conflicts"]["conflicts"], json!([]));

    // Sent again, nothing changes.
    let round = addon.run().await.unwrap();
    assert_eq!(
        (round.created, round.updated, round.retired),
        (0, 3, 0),
        "{round:?}"
    );
    assert_eq!(core.assets(brightfield).await.len(), 2);
    zabbix.clean_up().await;
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_host_two_customers_cover_is_a_conflict_and_hosts_that_leave_are_retired() {
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    let brightfield = core.customer("Brightfield").await;
    let harbor = core.customer("Harbor Lane").await;
    core.asset_kind("Network device").await;
    let bf = zabbix.group("Brightfield").await;
    let lab = zabbix.group("Brightfield Lab").await;
    let hl = zabbix.group("Harbor Lane").await;
    let shared = zabbix.host("db-01", &[&bf], json!({})).await;
    let moving = zabbix.host("web-02", &[&bf], json!({})).await;
    let removed = zabbix.host("app-03", &[&bf], json!({})).await;
    zabbix.host("bench-04", &[&lab], json!({})).await;
    let addon = zabbix.addon(&core, kinds("none"));
    addon.run().await.unwrap();
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let lab_reference = core
        .reference(brightfield, &zabbix.name("Brightfield Lab"))
        .await;
    core.reference(harbor, &zabbix.name("Harbor Lane")).await;
    let round = addon.run().await.unwrap();
    assert_eq!(round.created, 4, "{round:?}");

    // db-01 joins a group of Harbor Lane's too, web-02 moves there, app-03 leaves Zabbix and
    // Brightfield no longer has the lab.
    zabbix.move_host(&shared, &[&bf, &hl]).await;
    zabbix.move_host(&moving, &[&hl]).await;
    zabbix.delete_host(&removed).await;
    let (status, json) = core
        .call(
            "DELETE",
            &format!("/api/external-references/{lab_reference}"),
            None,
        )
        .await;
    assert!(status.is_success(), "{json}");

    let round = addon.run().await.unwrap();
    assert_eq!(
        (
            round.hosts,
            round.created,
            round.retired,
            round.conflicts,
            round.refused
        ),
        (1, 1, 4, 1, 0),
        "{round:?}"
    );
    let named = |host: &str| format!("{}-{host}", zabbix.prefix);
    let asset = |host: &str, retired: bool| {
        (
            named(host),
            "Network device".to_owned(),
            None,
            Some(ADDON.to_owned()),
            retired,
        )
    };
    assert_eq!(
        core.assets(brightfield).await,
        [
            asset("app-03", true),
            asset("bench-04", true),
            asset("db-01", true),
            asset("web-02", true),
        ]
    );
    assert_eq!(core.assets(harbor).await, [asset("web-02", false)]);
    let state = core.state().await;
    assert_eq!(
        state["conflicts"]["conflicts"],
        json!([{"sentence": {
            "en": format!(
                "The host {} is in host groups of different customers: {} (Brightfield), {} \
                 (Harbor Lane). It is not imported: leave it in the groups of one customer.",
                named("db-01"), zabbix.name("Brightfield"), zabbix.name("Harbor Lane")
            ),
            "it": format!(
                "L'host {} è in gruppi di host di clienti diversi: {} (Brightfield), {} \
                 (Harbor Lane). Non viene importato: lasciarlo nei gruppi di un solo cliente.",
                named("db-01"), zabbix.name("Brightfield"), zabbix.name("Harbor Lane")
            ),
        }}])
    );

    // Back in one customer's groups, the host's asset comes back and the conflict goes.
    zabbix.move_host(&shared, &[&bf]).await;
    let round = addon.run().await.unwrap();
    assert_eq!((round.updated, round.conflicts), (2, 0), "{round:?}");
    assert!(
        core.assets(brightfield)
            .await
            .contains(&asset("db-01", false))
    );
    assert_eq!(core.state().await["conflicts"]["conflicts"], json!([]));
    zabbix.clean_up().await;
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_host_matching_an_asset_the_customer_has_is_proposed_as_that_asset() {
    let core = Core::start().await;
    let mut zabbix = TestZabbix::connect().await;
    let brightfield = core.customer("Brightfield").await;
    let laptop = core.asset_kind("Laptop").await;
    core.asset_kind("Network device").await;
    let by_hand = |name: &str, serial: Option<&str>| json!({"kind_id": laptop, "name": name, "serial_number": serial});
    let assets = format!("/api/customers/{brightfield}/assets");
    let entered = core
        .create(&assets, by_hand("PC-0077", Some("SN-0077")))
        .await;
    core.create(&assets, by_hand(&zabbix.name("NAS"), None))
        .await;
    let bf = zabbix.group("Brightfield").await;
    let by_serial = zabbix
        .host(
            "pc-77",
            &[&bf],
            json!({"inventory_mode": 0, "inventory": {"serialno_a": "sn-0077"}}),
        )
        .await;
    zabbix
        .host("nas", &[&bf], json!({"name": zabbix.name("nas")}))
        .await;
    let addon = zabbix.addon(&core, kinds("none"));
    addon.run().await.unwrap();
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;

    for _ in 0..2 {
        let round = addon.run().await.unwrap();
        assert_eq!((round.created, round.proposed), (0, 2), "{round:?}");
    }
    let proposals = core
        .get(&format!(
            "/api/customers/{brightfield}/asset-link-proposals"
        ))
        .await;
    let mut matched: Vec<(String, String)> = proposals
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            assert_eq!(p["addon"], ADDON);
            (
                p["asset_name"].as_str().unwrap().to_owned(),
                p["matched_by"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    matched.sort();
    assert_eq!(
        matched,
        [
            ("PC-0077".to_owned(), "serial_number".to_owned()),
            (zabbix.name("NAS"), "name".to_owned()),
        ]
    );
    assert_eq!(core.assets(brightfield).await.len(), 2, "no second asset");

    // Accepted, the add-on is linked to the asset and leaves it as it is; once the host leaves
    // Zabbix, it unlinks itself and the asset stays.
    let proposal = proposals
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["asset_name"] == "PC-0077")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, json) = core
        .call(
            "POST",
            &format!("/api/asset-link-proposals/{proposal}/accept"),
            None,
        )
        .await;
    assert!(status.is_success(), "{json}");
    let round = addon.run().await.unwrap();
    assert_eq!((round.proposed, round.refused), (1, 0), "{round:?}");
    let linked = |assets: &Value| -> Vec<String> {
        assets
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == json!(entered))
            .unwrap()["linked_addons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["addon"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    assert_eq!(linked(&core.get(&assets).await), [ADDON]);

    zabbix.delete_host(&by_serial).await;
    let round = addon.run().await.unwrap();
    assert_eq!((round.unlinked, round.retired), (1, 0), "{round:?}");
    let after = core.get(&assets).await;
    assert!(linked(&after).is_empty(), "{after}");
    assert!(
        core.assets(brightfield).await.iter().all(|a| !a.4),
        "nothing retired"
    );
    zabbix.clean_up().await;
}
