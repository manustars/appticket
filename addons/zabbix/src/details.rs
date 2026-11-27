//! The live details of a host, which the core asks for as `GET <url>/assets/{reference}` whenever
//! agents look at the asset of the host: its availability, its open problems, its main interface
//! and its OS, with a state and links to the host and its problems in Zabbix's frontend.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hd_addon_sdk::live_details::Label;
use hd_addon_sdk::{DetailState, Field, Link, LiveDetails, Verifier};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::rules::Severity;
use crate::sync::host_of;
use crate::zabbix::Zabbix;

/// How long the add-on takes at most to answer a read: Zabbix's answers or an error, within the
/// 2 seconds the core waits ([`TIMEOUT`](hd_addon_sdk::live_details::TIMEOUT)), less a margin
/// for the reads the core makes of the add-on before and for the answer's way back.
pub const DEADLINE: Duration =
    hd_addon_sdk::live_details::TIMEOUT.saturating_sub(Duration::from_millis(500));
const _: () = assert!(
    !DEADLINE.is_zero(),
    "the core's wait leaves the add-on no time"
);

/// The live details of the hosts of one Zabbix, read from it on each of the core's reads.
pub struct Details {
    zabbix: Zabbix,
    /// The frontend's base URL, for the links.
    frontend: String,
}

impl Details {
    /// The details from `zabbix`, whose calls should take at most [`DEADLINE`], with links into
    /// the frontend at `frontend_url`, given without a final `/`.
    pub fn new(zabbix: Zabbix, frontend_url: &str) -> Details {
        Details {
            zabbix,
            frontend: frontend_url.to_owned(),
        }
    }

    /// The live details of the host of the add-on's `reference`, `None` for a host Zabbix does
    /// not have (or the token's user cannot read); an error when Zabbix does not answer them
    /// within [`DEADLINE`].
    pub async fn of(&self, reference: &str) -> anyhow::Result<Option<LiveDetails>> {
        let host = host_of(reference);
        if host.is_empty() || !host.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(None);
        }
        tokio::time::timeout(DEADLINE, self.read(host))
            .await
            .with_context(|| format!("Zabbix did not answer within {DEADLINE:?}"))?
    }

    /// The host, its enabled items and its open problems, asked at once.
    async fn read(&self, host: &str) -> anyhow::Result<Option<LiveDetails>> {
        let (hosts, items, problems) = tokio::try_join!(
            self.zabbix.call::<Vec<RawHost>>(
                "host.get",
                json!({
                    "output": ["hostid", "status", "active_available"],
                    "hostids": [host],
                    "selectInterfaces":
                        ["interfaceid", "type", "main", "useip", "ip", "dns", "available"],
                    "selectInventory": ["os"],
                }),
            ),
            self.zabbix.call::<Vec<RawItem>>(
                "item.get",
                json!({
                    "output": ["interfaceid", "type"],
                    "hostids": [host],
                    // Only those that count for availability, however many the host has.
                    "filter": {
                        "status": item::ENABLED,
                        "type": item::POLLING.iter().chain([&item::ACTIVE_CHECK]).collect::<Vec<_>>(),
                    },
                }),
            ),
            // As Zabbix's problem view shows them by default: neither suppressed nor symptoms.
            self.zabbix.call::<Vec<RawProblem>>(
                "problem.get",
                json!({
                    "output": ["eventid", "severity"],
                    "hostids": [host],
                    "suppressed": false,
                    "symptom": false,
                }),
            ),
        )?;
        Ok(hosts
            .first()
            .map(|h| describe(&self.frontend, h, &items, &problems)))
    }
}

/// `GET /assets/{reference}`, answering the core's signed reads with [`Details::of`]: 404 for a
/// host Zabbix does not have, 503 when Zabbix does not answer in time.
pub fn router(details: Arc<Details>, verifier: Verifier) -> axum::Router {
    hd_addon_sdk::server::live_details_router(verifier, move |reference: String| {
        let details = details.clone();
        async move {
            details
                .of(&reference)
                .await
                .map_err(|e| format!("{e:#}").into())
        }
    })
}

/// Zabbix's codes of item types and states (`item.type`, `item.status`), as its API writes them.
mod item {
    /// An enabled item.
    pub const ENABLED: &str = "0";
    pub const AGENT: &str = "0";
    /// An active agent check, which uses no interface.
    pub const ACTIVE_CHECK: &str = "7";
    pub const IPMI: &str = "12";
    pub const JMX: &str = "16";
    pub const SNMP: &str = "20";
    /// The types that poll through an interface.
    pub const POLLING: [&str; 4] = [AGENT, IPMI, JMX, SNMP];
}

/// `host.status` of a host Zabbix monitors; `1` is disabled.
const MONITORED: &str = "0";

/// The availability of an interface or of the active checks (`0`, `1` and `2` in Zabbix's API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
enum Reach {
    #[serde(rename = "1")]
    Available,
    #[serde(rename = "2")]
    NotAvailable,
    /// `0`: not polled yet, or its result unknown.
    #[default]
    #[serde(other)]
    Unknown,
}

/// The type of an interface (`1` to `4` in Zabbix's API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum InterfaceType {
    #[serde(rename = "1")]
    Agent,
    #[serde(rename = "2")]
    Snmp,
    #[serde(rename = "3")]
    Ipmi,
    #[serde(rename = "4")]
    Jmx,
    #[serde(other)]
    Other,
}

/// A yes-or-no field, which Zabbix's API writes `1` or `0`.
fn flag<'de, D: serde::Deserializer<'de>>(value: D) -> Result<bool, D::Error> {
    Ok(String::deserialize(value)? == "1")
}

/// A host as `host.get` answers it with the fields the live details need.
#[derive(Debug, Deserialize)]
struct RawHost {
    hostid: String,
    /// [`MONITORED`], or disabled.
    status: String,
    /// The availability of the agent's active checks.
    #[serde(default)]
    active_available: Reach,
    #[serde(default)]
    interfaces: Vec<RawInterface>,
    /// An object with the fields asked for, or an empty array when the inventory is off.
    #[serde(default)]
    inventory: Value,
}

#[derive(Debug, Deserialize)]
struct RawInterface {
    interfaceid: String,
    #[serde(rename = "type")]
    kind: InterfaceType,
    /// The default interface of its type.
    #[serde(deserialize_with = "flag")]
    main: bool,
    /// Zabbix connects through the IP, else through the DNS name.
    #[serde(deserialize_with = "flag")]
    useip: bool,
    ip: String,
    dns: String,
    available: Reach,
}

/// An enabled item of the host, as `item.get` answers it.
#[derive(Debug, Deserialize)]
struct RawItem {
    /// `0` for items that use no interface.
    #[serde(default)]
    interfaceid: String,
    /// One of the codes in [`item`].
    #[serde(rename = "type")]
    kind: String,
}

/// An open problem of the host, as `problem.get` answers it.
#[derive(Debug, Deserialize)]
struct RawProblem {
    /// 0 not classified to 5 disaster.
    severity: String,
}

fn label(en: &str, it: &str) -> Label {
    Label::from([
        ("en".to_owned(), en.to_owned()),
        ("it".to_owned(), it.to_owned()),
    ])
}

/// The availability of a host as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Some of what counts is not available, the rest is or is unknown.
    PartlyAvailable,
    NotAvailable,
    Unknown,
    /// Not monitored: Zabbix does not poll it.
    Disabled,
}

impl Availability {
    /// As Zabbix's frontend words it.
    fn value(self) -> &'static str {
        match self {
            Availability::Available => "Available",
            Availability::PartlyAvailable => "Partly available",
            Availability::NotAvailable => "Not available",
            Availability::Unknown => "Unknown",
            Availability::Disabled => "Disabled",
        }
    }
}

/// The availability of `host`, whose enabled items are `items`. Zabbix 7.0 keeps it per
/// interface, and the availability of active agent checks on the host; its frontend colours each
/// interface type by the interfaces of that type an enabled item uses (all of them when none is
/// used), the active checks counting with the agent's when the host has an enabled one. The same
/// rule is applied here to all of them together: all available is available, all not available
/// is not available, some not available is partly available, and otherwise any unknown is
/// unknown. A host with none of them is unknown, and a disabled host is disabled.
fn availability(host: &RawHost, items: &[RawItem]) -> Availability {
    if host.status != MONITORED {
        return Availability::Disabled;
    }
    let used: std::collections::HashSet<&str> = items
        .iter()
        .filter(|i| item::POLLING.contains(&i.kind.as_str()))
        .map(|i| i.interfaceid.as_str())
        .collect();
    let mut counted: Vec<Reach> = host
        .interfaces
        .iter()
        .filter(|i| used.contains(i.interfaceid.as_str()))
        .map(|i| i.available)
        .collect();
    if items.iter().any(|i| i.kind == item::ACTIVE_CHECK) {
        counted.push(host.active_available);
    }
    if counted.is_empty() {
        counted = host.interfaces.iter().map(|i| i.available).collect();
    }
    let not_available = counted
        .iter()
        .filter(|a| **a == Reach::NotAvailable)
        .count();
    if counted.is_empty() {
        Availability::Unknown
    } else if not_available == counted.len() {
        Availability::NotAvailable
    } else if not_available > 0 {
        Availability::PartlyAvailable
    } else if counted.iter().all(|a| *a == Reach::Available) {
        Availability::Available
    } else {
        Availability::Unknown
    }
}

/// A severity as Zabbix's frontend names it by default.
fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::NotClassified => "Not classified",
        Severity::Information => "Information",
        Severity::Warning => "Warning",
        Severity::Average => "Average",
        Severity::High => "High",
        Severity::Disaster => "Disaster",
    }
}

/// Interface types in the order Zabbix's frontend picks the one it shows.
const INTERFACE_PRIORITY: [InterfaceType; 4] = [
    InterfaceType::Agent,
    InterfaceType::Snmp,
    InterfaceType::Jmx,
    InterfaceType::Ipmi,
];

/// The address of the host's main interface: the default one of the first type in
/// [`INTERFACE_PRIORITY`] it has, as its IP or DNS name, whichever Zabbix connects through.
fn main_interface(host: &RawHost) -> Option<&str> {
    let interface = INTERFACE_PRIORITY.iter().find_map(|kind| {
        let of_kind = || host.interfaces.iter().filter(move |i| i.kind == *kind);
        of_kind().find(|i| i.main).or_else(|| of_kind().next())
    })?;
    let address = if interface.useip {
        &interface.ip
    } else {
        &interface.dns
    };
    Some(address.trim()).filter(|a| !a.is_empty())
}

/// The live details of `host`, whose enabled items are `items` and open problems `problems`,
/// with links into the frontend at `frontend`.
fn describe(
    frontend: &str,
    host: &RawHost,
    items: &[RawItem],
    problems: &[RawProblem],
) -> LiveDetails {
    let availability = availability(host, items);
    let highest = problems
        .iter()
        .filter_map(|p| p.severity.parse().ok().and_then(Severity::from_number))
        .max();
    let state = if availability == Availability::NotAvailable
        || highest.is_some_and(|s| s >= Severity::High)
    {
        DetailState::Problem
    } else if availability != Availability::Available || !problems.is_empty() {
        DetailState::Warning
    } else {
        DetailState::Ok
    };

    let mut fields = vec![
        Field::new(label("Availability", "Disponibilità"), availability.value()),
        Field::new(
            label("Open problems", "Problemi aperti"),
            problems.len().to_string(),
        ),
    ];
    if let Some(highest) = highest {
        fields.push(Field::new(
            label("Highest severity", "Gravità massima"),
            severity_name(highest),
        ));
    }
    if let Some(address) = main_interface(host) {
        fields.push(Field::new(
            label("Main interface", "Interfaccia principale"),
            address,
        ));
    }
    if let Some(os) = host
        .inventory
        .get("os")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|os| !os.is_empty())
    {
        fields.push(Field::new(label("OS", "Sistema operativo"), os));
    }

    let host_id = &host.hostid;
    LiveDetails {
        state,
        fields,
        links: vec![
            Link::new(
                label("Host in Zabbix", "Host in Zabbix"),
                format!("{frontend}/hostinventories.php?hostid={host_id}"),
            ),
            Link::new(
                label("Problems in Zabbix", "Problemi in Zabbix"),
                format!(
                    "{frontend}/zabbix.php?action=problem.view&filter_set=1&hostids%5B%5D={host_id}"
                ),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(value: Value) -> RawHost {
        serde_json::from_value(value).unwrap()
    }

    fn items(value: Value) -> Vec<RawItem> {
        serde_json::from_value(value).unwrap()
    }

    fn problems(value: Value) -> Vec<RawProblem> {
        serde_json::from_value(value).unwrap()
    }

    fn fields(details: &LiveDetails) -> Vec<(String, String, String)> {
        details
            .fields
            .iter()
            .map(|f| {
                (
                    f.label["en"].clone(),
                    f.label["it"].clone(),
                    f.value.clone(),
                )
            })
            .collect()
    }

    /// A host as Zabbix 7.0 answers `host.get` with an agent interface (the main one, through its
    /// IP) and an SNMP one, the inventory's OS, items using the agent interface and two open
    /// problems.
    #[test]
    fn a_host_s_details_have_its_availability_problems_interface_os_and_links() {
        let srv = host(json!({
            "hostid": "10683", "status": "0", "active_available": "0",
            "interfaces": [
                {"interfaceid": "31", "type": "2", "main": "1", "useip": "0",
                 "ip": "", "dns": "switch.brightfield.example.com", "available": "0"},
                {"interfaceid": "30", "type": "1", "main": "1", "useip": "1",
                 "ip": "192.0.2.10", "dns": "srv-01.brightfield.example.com", "available": "1"}
            ],
            "inventory": {"os": " Linux 6.1 "}
        }));
        let details = describe(
            "https://zabbix.example.com",
            &srv,
            &items(json!([{"itemid": "5", "interfaceid": "30", "type": "0"}])),
            &problems(json!([
                {"eventid": "901", "severity": "2"},
                {"eventid": "902", "severity": "3"}
            ])),
        );
        assert_eq!(details.state, DetailState::Warning, "an Average problem");
        assert_eq!(
            fields(&details),
            [
                (
                    "Availability".into(),
                    "Disponibilità".into(),
                    "Available".into()
                ),
                ("Open problems".into(), "Problemi aperti".into(), "2".into()),
                (
                    "Highest severity".into(),
                    "Gravità massima".into(),
                    "Average".into()
                ),
                (
                    "Main interface".into(),
                    "Interfaccia principale".into(),
                    "192.0.2.10".into()
                ),
                ("OS".into(), "Sistema operativo".into(), "Linux 6.1".into()),
            ]
        );
        assert_eq!(
            details
                .links
                .iter()
                .map(|l| (
                    l.label["en"].as_str(),
                    l.label["it"].as_str(),
                    l.url.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "Host in Zabbix",
                    "Host in Zabbix",
                    "https://zabbix.example.com/hostinventories.php?hostid=10683"
                ),
                (
                    "Problems in Zabbix",
                    "Problemi in Zabbix",
                    "https://zabbix.example.com/zabbix.php?action=problem.view&filter_set=1&hostids%5B%5D=10683"
                ),
            ]
        );
    }

    fn interface(id: &str, kind: &str, available: &str) -> Value {
        json!({"interfaceid": id, "type": kind, "main": "1", "useip": "1",
               "ip": format!("198.51.100.{id}"), "dns": "", "available": available})
    }

    /// The availability, and the state with no problem, of a monitored host with these
    /// interfaces, active checks availability and enabled items `(interface, type)`.
    fn availability_of(
        interfaces: &[Value],
        active: &str,
        enabled: &[(&str, &str)],
    ) -> (String, DetailState) {
        let host = host(
            json!({"hostid": "1", "status": "0", "active_available": active,
                               "interfaces": interfaces, "inventory": []}),
        );
        let items: Vec<RawItem> = enabled
            .iter()
            .map(|(interface, kind)| RawItem {
                interfaceid: interface.to_string(),
                kind: kind.to_string(),
            })
            .collect();
        let details = describe("https://zabbix.example.com", &host, &items, &[]);
        (details.fields[0].value.clone(), details.state)
    }

    /// A monitored host's interfaces, the availability of its active checks and its enabled
    /// items, with the availability and state it should have.
    struct Case {
        what: &'static str,
        interfaces: Vec<Value>,
        active: &'static str,
        /// `(interface, item type)`.
        enabled: Vec<(&'static str, &'static str)>,
        availability: &'static str,
        state: DetailState,
    }

    #[test]
    fn the_availability_counts_the_interfaces_enabled_items_use_and_the_active_checks() {
        use DetailState::*;
        let cases = [
            Case {
                what: "no interface",
                interfaces: vec![],
                active: "0",
                enabled: vec![],
                availability: "Unknown",
                state: Warning,
            },
            Case {
                what: "polled and answering",
                interfaces: vec![interface("1", "1", "1")],
                active: "0",
                enabled: vec![("1", "0")],
                availability: "Available",
                state: Ok,
            },
            Case {
                what: "an SNMP interface no item uses is left out",
                interfaces: vec![interface("1", "1", "1"), interface("2", "2", "0")],
                active: "0",
                enabled: vec![("1", "0")],
                availability: "Available",
                state: Ok,
            },
            Case {
                what: "the only polled interface does not answer",
                interfaces: vec![interface("1", "1", "2"), interface("2", "2", "0")],
                active: "0",
                enabled: vec![("1", "0")],
                availability: "Not available",
                state: Problem,
            },
            Case {
                what: "one of two polled interfaces does not answer",
                interfaces: vec![interface("1", "1", "1"), interface("2", "2", "2")],
                active: "0",
                enabled: vec![("1", "0"), ("2", "20")],
                availability: "Partly available",
                state: Warning,
            },
            Case {
                what: "one polled interface does not answer, the other is unknown",
                interfaces: vec![interface("1", "1", "2"), interface("2", "2", "0")],
                active: "0",
                enabled: vec![("1", "0"), ("2", "20")],
                availability: "Partly available",
                state: Warning,
            },
            Case {
                what: "no enabled item: every interface counts",
                interfaces: vec![interface("1", "1", "1"), interface("2", "4", "0")],
                active: "0",
                enabled: vec![],
                availability: "Unknown",
                state: Warning,
            },
            Case {
                what: "not polled yet",
                interfaces: vec![interface("1", "1", "0")],
                active: "0",
                enabled: vec![("1", "0")],
                availability: "Unknown",
                state: Warning,
            },
            Case {
                what: "active checks only, whose heartbeat stopped",
                interfaces: vec![],
                active: "2",
                enabled: vec![("0", "7")],
                availability: "Not available",
                state: Problem,
            },
            Case {
                what: "active checks answering beside an agent interface no item uses",
                interfaces: vec![interface("1", "1", "0")],
                active: "1",
                enabled: vec![("0", "7")],
                availability: "Available",
                state: Ok,
            },
            Case {
                what: "the active checks' availability without an active check is left out",
                interfaces: vec![interface("1", "1", "1")],
                active: "2",
                enabled: vec![("1", "0"), ("0", "2")],
                availability: "Available",
                state: Ok,
            },
        ];
        for case in cases {
            assert_eq!(
                availability_of(&case.interfaces, case.active, &case.enabled),
                (case.availability.to_owned(), case.state),
                "{}",
                case.what
            );
        }
    }

    #[test]
    fn a_disabled_host_is_a_warning_and_a_high_or_disaster_problem_a_problem() {
        let disabled = host(
            json!({"hostid": "7", "status": "1", "active_available": "0",
                                   "interfaces": [interface("1", "1", "0")], "inventory": []}),
        );
        let details = describe("https://zabbix.example.com", &disabled, &[], &[]);
        assert_eq!(
            (details.state, details.fields[0].value.as_str()),
            (DetailState::Warning, "Disabled")
        );

        let answering = host(
            json!({"hostid": "7", "status": "0", "active_available": "0",
                                    "interfaces": [interface("1", "1", "1")], "inventory": []}),
        );
        let polled = items(json!([{"interfaceid": "1", "type": "0"}]));
        for (severities, state, highest) in [
            (vec!["0"], DetailState::Warning, "Not classified"),
            (vec!["1", "3"], DetailState::Warning, "Average"),
            (vec!["2", "4"], DetailState::Problem, "High"),
            (vec!["5", "1"], DetailState::Problem, "Disaster"),
        ] {
            let open: Vec<RawProblem> = severities
                .iter()
                .map(|s| RawProblem {
                    severity: s.to_string(),
                })
                .collect();
            let details = describe("https://zabbix.example.com", &answering, &polled, &open);
            assert_eq!(details.state, state, "{severities:?}");
            assert_eq!(
                fields(&details)[1..3]
                    .iter()
                    .map(|(en, _, value)| (en.as_str(), value.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("Open problems", open.len().to_string().as_str()),
                    ("Highest severity", highest)
                ],
                "{severities:?}"
            );
        }
    }

    /// The main interface is the default one of the agent, else SNMP, JMX and IPMI, by the DNS
    /// name when Zabbix connects through it; without interfaces, inventory or OS, the fields are
    /// left out.
    #[test]
    fn the_main_interface_is_the_one_zabbix_s_frontend_shows() {
        let printer = host(json!({
            "hostid": "12", "status": "0", "active_available": "0",
            "interfaces": [
                {"interfaceid": "3", "type": "3", "main": "1", "useip": "1",
                 "ip": "203.0.113.3", "dns": "", "available": "0"},
                {"interfaceid": "4", "type": "4", "main": "1", "useip": "0",
                 "ip": "203.0.113.4", "dns": "printer.harbor.example.com", "available": "0"}
            ],
            "inventory": {"os": ""}
        }));
        let shown = fields(&describe("https://zabbix.example.com", &printer, &[], &[]));
        assert_eq!(
            shown
                .iter()
                .map(|(en, _, value)| (en.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Availability", "Unknown"),
                ("Open problems", "0"),
                ("Main interface", "printer.harbor.example.com"),
            ]
        );
        let bare = host(json!({"hostid": "13", "status": "0", "interfaces": [], "inventory": []}));
        assert_eq!(
            fields(&describe("https://zabbix.example.com", &bare, &[], &[])).len(),
            2
        );
    }
}
