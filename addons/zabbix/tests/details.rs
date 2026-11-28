//! The live details of the assets of Zabbix's hosts, as agents read them through the real core
//! from the add-on, against the Zabbix in containers. Ignored by default, since they need Zabbix
//! (`HD_TEST_ZABBIX_URL`), the test database server (`DATABASE_URL`) and the core's binary
//! (`cargo build -p hd-core`); CI runs them.

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{Core, TestZabbix, live_details, zabbix_url};
use hd_addon_sdk::api::{SuppliedAsset, SuppliedDetails};
use hd_addon_sdk::events::{HEADER_SIGNATURE, HEADER_TIMESTAMP};
use hd_addon_sdk::signature::{sign, signed_read};
use hd_zabbix::details::{DEADLINE, Details};
use hd_zabbix::kinds::Kinds;
use hd_zabbix::zabbix::Zabbix;
use serde_json::json;

fn servers() -> Kinds {
    Kinds {
        mappings: vec![],
        default: "Server".into(),
    }
}

fn field(en: &str, it: &str, value: &str) -> (String, String, String) {
    (en.into(), it.into(), value.into())
}

#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn an_asset_of_a_host_shows_its_availability_problems_interface_and_os() {
    let (listener, url) = common::listener().await;
    let core = Core::start_at(&url).await;
    let mut zabbix = TestZabbix::connect().await;
    common::serve(listener, zabbix.details(), &core.event_secret);
    let brightfield = core.customer("Brightfield").await;
    core.asset_kind("Server").await;
    let group = zabbix.group("Brightfield").await;
    let host = zabbix
        .host(
            "srv-01",
            &[&group],
            json!({"interfaces": [{"type": 1, "main": 1, "useip": 1, "ip": "192.0.2.10",
                                   "dns": "srv-01.brightfield.example.com", "port": "10050"}],
                   "inventory_mode": 0, "inventory": {"os": "Debian GNU/Linux 12"}}),
        )
        .await;
    // A trapper item, which uses no interface: Zabbix polls nothing of the host.
    let errors = zabbix
        .item(
            &host,
            "Disk errors",
            json!({"key_": "disk.errors", "type": 2, "value_type": 3}),
        )
        .await;
    zabbix
        .trigger(
            "Disk errors on srv-01",
            &format!("last(/{}/disk.errors)>0", zabbix.host_name("srv-01")),
            4,
        )
        .await;
    core.reference(brightfield, &zabbix.name("Brightfield"))
        .await;
    let round = zabbix.addon(&core, servers()).run().await.unwrap();
    assert_eq!((round.created, round.refused), (1, 0), "{round:?}");

    let links = vec![
        (
            "Host in Zabbix".to_owned(),
            format!("{}/hostinventories.php?hostid={host}", zabbix_url()),
        ),
        (
            "Problems in Zabbix".to_owned(),
            format!(
                "{}/zabbix.php?action=problem.view&filter_set=1&hostids%5B%5D={host}",
                zabbix_url()
            ),
        ),
    ];
    // No item polls the interface, whose availability Zabbix does not know.
    assert_eq!(
        live_details(&core, brightfield).await,
        [Some((
            "warning".to_owned(),
            vec![
                field("Availability", "Disponibilità", "Unknown"),
                field("Open problems", "Problemi aperti", "0"),
                field("Main interface", "Interfaccia principale", "192.0.2.10"),
                field("OS", "Sistema operativo", "Debian GNU/Linux 12"),
            ],
            links.clone(),
        ))]
    );

    zabbix
        .push_until_problems(&errors, "3", &host, |problems| problems.len() == 1)
        .await;
    assert_eq!(
        live_details(&core, brightfield).await,
        [Some((
            "problem".to_owned(),
            vec![
                field("Availability", "Disponibilità", "Unknown"),
                field("Open problems", "Problemi aperti", "1"),
                field("Highest severity", "Gravità massima", "High"),
                field("Main interface", "Interfaccia principale", "192.0.2.10"),
                field("OS", "Sistema operativo", "Debian GNU/Linux 12"),
            ],
            links,
        ))]
    );

    // A host Zabbix does not have, and a reference that is not one of the add-on's.
    for reference in [format!("999999999@{brightfield}"), "srv-01".to_owned()] {
        assert_eq!(
            read(&url, &core.event_secret, &reference).await,
            reqwest::StatusCode::NOT_FOUND,
            "{reference}"
        );
    }
    zabbix.clean_up().await;
}

/// An asset the customer had, which agents linked the host to, shows the host's live details as
/// those of an asset the add-on supplied do.
#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn an_asset_the_add_on_is_linked_to_shows_the_host_s_details() {
    let (listener, url) = common::listener().await;
    let core = Core::start_at(&url).await;
    let mut zabbix = TestZabbix::connect().await;
    common::serve(listener, zabbix.details(), &core.event_secret);
    let old_quay = core.customer("Old Quay").await;
    let server = core.asset_kind("Server").await;
    let by_hand = core
        .create(
            &format!("/api/customers/{old_quay}/assets"),
            json!({"kind_id": server, "name": "Quay storage", "serial_number": "SN-7710"}),
        )
        .await;
    let group = zabbix.group("Old Quay").await;
    let host = zabbix
        .host(
            "nas-01",
            &[&group],
            json!({"interfaces": [{"type": 1, "main": 1, "useip": 0, "ip": "",
                                   "dns": "nas-01.oldquay.example.com", "port": "10050"}],
                   "inventory_mode": 0,
                   "inventory": {"serialno_a": "SN-7710", "os": "FreeBSD 14"}}),
        )
        .await;
    core.reference(old_quay, &zabbix.name("Old Quay")).await;
    let addon = zabbix.addon(&core, servers());
    let round = addon.run().await.unwrap();
    assert_eq!((round.proposed, round.refused), (1, 0), "{round:?}");
    let proposals = core
        .get(&format!("/api/customers/{old_quay}/asset-link-proposals"))
        .await;
    let proposal = proposals[0]["id"].as_str().unwrap();
    let (status, json) = core
        .call(
            "POST",
            &format!("/api/asset-link-proposals/{proposal}/accept"),
            None,
        )
        .await;
    assert!(status.is_success(), "{json}");

    let read = core
        .get(&format!("/api/customers/{old_quay}/assets/live-details"))
        .await;
    assert_eq!(read[0]["asset_id"], json!(by_hand), "{read}");
    assert_eq!(
        live_details(&core, old_quay).await,
        [Some((
            "warning".to_owned(),
            vec![
                field("Availability", "Disponibilità", "Unknown"),
                field("Open problems", "Problemi aperti", "0"),
                field(
                    "Main interface",
                    "Interfaccia principale",
                    "nas-01.oldquay.example.com"
                ),
                field("OS", "Sistema operativo", "FreeBSD 14"),
            ],
            vec![
                (
                    "Host in Zabbix".to_owned(),
                    format!("{}/hostinventories.php?hostid={host}", zabbix_url()),
                ),
                (
                    "Problems in Zabbix".to_owned(),
                    format!(
                        "{}/zabbix.php?action=problem.view&filter_set=1&hostids%5B%5D={host}",
                        zabbix_url()
                    ),
                ),
            ],
        ))]
    );
    zabbix.clean_up().await;
}

/// Zabbix marks an interface not available once its polls have failed for a while (45 seconds by
/// default, after it learned of the item): this test waits for it.
#[tokio::test]
#[ignore = "needs the Zabbix in containers and the test database"]
async fn a_host_zabbix_cannot_reach_is_not_available_and_a_problem() {
    let (listener, url) = common::listener().await;
    let core = Core::start_at(&url).await;
    let mut zabbix = TestZabbix::connect().await;
    common::serve(listener, zabbix.details(), &core.event_secret);
    let harbor = core.customer("Harbor Lane").await;
    core.asset_kind("Server").await;
    let group = zabbix.group("Harbor Lane").await;
    // Nothing answers at that address: the server's polls fail.
    let host = zabbix
        .host(
            "gw-01",
            &[&group],
            json!({"interfaces": [{"type": 1, "main": 1, "useip": 0, "ip": "",
                                   "dns": "gw-01.harbor.invalid", "port": "10050"}]}),
        )
        .await;
    let interfaces: Vec<serde_json::Value> = zabbix
        .admin
        .call(
            "hostinterface.get",
            json!({"output": ["interfaceid"], "hostids": [host]}),
        )
        .await
        .unwrap();
    zabbix
        .item(
            &host,
            "Agent ping",
            json!({"key_": "agent.ping", "type": 0, "value_type": 3, "delay": "5s",
                   "interfaceid": interfaces[0]["interfaceid"]}),
        )
        .await;
    core.reference(harbor, &zabbix.name("Harbor Lane")).await;
    zabbix.addon(&core, servers()).run().await.unwrap();

    let started = Instant::now();
    while zabbix.availability(&host).await != ["2"] {
        assert!(
            started.elapsed() < Duration::from_secs(240),
            "Zabbix still has the interface as {:?}",
            zabbix.availability(&host).await
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let read = live_details(&core, harbor).await;
    let (state, fields, _) = read[0].clone().expect("live details");
    assert_eq!(state, "problem");
    assert_eq!(
        fields[..1],
        [field("Availability", "Disponibilità", "Not available")]
    );
    assert!(
        fields.contains(&field(
            "Main interface",
            "Interfaccia principale",
            "gw-01.harbor.invalid"
        )),
        "{fields:?}"
    );
    zabbix.clean_up().await;
}

/// The status the add-on at `url` answers to a read of `reference` signed as the core signs it.
fn read(
    url: &str,
    event_secret: &str,
    reference: &str,
) -> impl Future<Output = reqwest::StatusCode> + use<> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let request = hd_addon_sdk::http_client(Duration::from_secs(10))
        .unwrap()
        .get(format!(
            "{url}/assets/{}",
            url::form_urlencoded::byte_serialize(reference.as_bytes()).collect::<String>()
        ))
        .header(HEADER_TIMESTAMP, now.to_string())
        .header(
            HEADER_SIGNATURE,
            sign(event_secret, now, signed_read(reference).as_bytes()),
        );
    async move { request.send().await.unwrap().status() }
}

/// A Zabbix that takes connections and never answers.
async fn silent_zabbix() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let (connection, _) = listener.accept().await.unwrap();
            held.push(connection);
        }
    });
    url
}

/// The add-on answers the core with an error before the core's 2 seconds run out, every read on
/// its own: nothing waits for the reads before it.
#[tokio::test]
#[ignore = "needs the test database"]
async fn when_zabbix_does_not_answer_the_details_are_unavailable_in_time() {
    let silent = silent_zabbix().await;
    let (listener, url) = common::listener().await;
    let core = Core::start_at(&url).await;
    common::serve(
        listener,
        Details::new(
            Zabbix::new(&silent, "a-token", DEADLINE).unwrap(),
            "https://zabbix.example.com",
        ),
        &core.event_secret,
    );
    let old_quay = core.customer("Old Quay").await;
    core.asset_kind("Server").await;
    let references: Vec<String> = ["10701", "10702", "10703"]
        .iter()
        .map(|host| format!("{host}@{old_quay}"))
        .collect();
    for reference in &references {
        core.addon()
            .supply_asset(&SuppliedAsset {
                reference: reference.clone(),
                customer_id: old_quay,
                details: SuppliedDetails {
                    kind: "Server".into(),
                    name: format!("srv-{}", &reference[..5]),
                    serial_number: None,
                    site: None,
                },
            })
            .await
            .unwrap();
    }

    // Read by the core: unavailable, within its 2 seconds.
    let started = Instant::now();
    let unavailable = live_details(&core, old_quay).await;
    let took = started.elapsed();
    assert_eq!(unavailable, [None, None, None]);
    assert!(took < Duration::from_millis(2500), "took {took:?}");

    // Read straight from the add-on, all at once: each answers 503 by its deadline.
    let started = Instant::now();
    let reads: Vec<_> = references
        .iter()
        .map(|reference| tokio::spawn(read(&url, &core.event_secret, reference)))
        .collect();
    for read in reads {
        assert_eq!(
            read.await.unwrap(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        );
    }
    let took = started.elapsed();
    assert!(
        took >= DEADLINE && took < hd_addon_sdk::live_details::TIMEOUT,
        "took {took:?}"
    );
}
