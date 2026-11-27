//! The `check` command: which rule an alert meets and with what outcome, or that none does. The
//! alert is given by Zabbix's event id, or by its severity, host, tags and trigger name. The
//! host's groups come from Zabbix and its customer from the external references customers have,
//! read from the core with the add-on's token as a round of the add-on reads them; nothing is
//! written anywhere.

use std::collections::HashMap;

use anyhow::Context;
use hd_addon_sdk::Client;

use crate::config::Config;
use crate::coverage::{Coverage, Placement};
use crate::rules::{Alert, Matched, OpenTicket, Outcome, Recovery, Rules, Severity};
use crate::text::list;
use crate::zabbix::{Event, HostWithGroups, Zabbix};

/// The alert to check, as the command line gives it.
#[derive(Debug, Clone, clap::Args)]
#[command(group(clap::ArgGroup::new("alert").required(true).args(["event", "host"])))]
pub struct CheckArgs {
    /// Zabbix's event id of the alert, such as 4213.
    #[arg(long, value_name = "ID")]
    pub event: Option<String>,
    /// The host, by its technical or its visible name.
    #[arg(long, requires = "severity", conflicts_with = "event")]
    pub host: Option<String>,
    #[arg(
        long,
        help = format!("The alert's severity: {}", Severity::names()),
        value_parser = parse_severity,
        requires = "host",
        conflicts_with = "event"
    )]
    pub severity: Option<Severity>,
    /// A tag of the alert, as NAME or NAME=VALUE; once for each tag.
    #[arg(long = "tag", value_name = "NAME[=VALUE]", value_parser = parse_tag, conflicts_with = "event")]
    pub tags: Vec<(String, String)>,
    /// The trigger's name, with its macros resolved, as Zabbix shows the problem.
    #[arg(long, value_name = "NAME", conflicts_with = "event")]
    pub trigger: Option<String>,
}

fn parse_severity(written: &str) -> Result<Severity, String> {
    Severity::from_name(written).ok_or_else(|| {
        format!(
            "{written:?} is not a severity of Zabbix: give {}",
            Severity::names()
        )
    })
}

/// A tag as `NAME` or `NAME=VALUE`; the value is everything after the first `=`.
fn parse_tag(written: &str) -> Result<(String, String), String> {
    let (name, value) = written.split_once('=').unwrap_or((written, ""));
    if name.trim().is_empty() {
        return Err(format!("the tag {written:?} has no name"));
    }
    Ok((name.trim().to_owned(), value.trim().to_owned()))
}

/// Whether the host is an asset of a customer, whose alerts can open its tickets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Import {
    /// An asset of this customer, by name.
    Customer(String),
    /// No customer covers its groups.
    Uncovered,
    /// An offboarded customer covers its groups.
    Offboarded,
    /// Two customers or more cover its groups.
    Conflict,
}

/// The alert as Zabbix or the command line gives it, before its hosts' customers are known.
struct Given {
    /// Zabbix's event id, when the alert is given by it.
    event: Option<String>,
    hosts: Vec<HostWithGroups>,
    severity: Severity,
    tags: Vec<(String, String)>,
    trigger: String,
}

impl Given {
    async fn read(zabbix: &Zabbix, args: CheckArgs) -> anyhow::Result<Given> {
        match args.event {
            Some(id) => {
                let Event {
                    id,
                    severity,
                    name,
                    tags,
                    host_ids,
                } = zabbix.problem_event(id.trim()).await?;
                let hosts = zabbix.hosts_by_id(&host_ids).await?;
                if hosts.is_empty() {
                    anyhow::bail!(
                        "the trigger of Zabbix's event {id} is on no host the add-on's user can \
                         read"
                    );
                }
                Ok(Given {
                    event: Some(id),
                    hosts,
                    severity,
                    tags,
                    trigger: name,
                })
            }
            None => {
                let host = args.host.context("give the host")?;
                Ok(Given {
                    event: None,
                    hosts: vec![zabbix.host_by_name(host.trim()).await?],
                    severity: args.severity.context("give the alert's severity")?,
                    tags: args.tags,
                    trigger: args.trigger.unwrap_or_default(),
                })
            }
        }
    }
}

/// Runs the check and answers what to print.
pub async fn run(config: &Config, args: CheckArgs) -> anyhow::Result<String> {
    let zabbix = Zabbix::new(
        &config.zabbix_url,
        &config.zabbix_token,
        config.zabbix_timeout,
    )?;
    let core = Client::new(&config.core_url, &config.core_token)?;
    let given = Given::read(&zabbix, args).await?;

    let groups = zabbix.host_groups().await?;
    let customers = core
        .customers()
        .await
        .context("reading the customers from the core")?;
    let offboarded = core
        .offboarded_references()
        .await
        .context("reading the offboarded customers' external references from the core")?;
    let coverage = Coverage::new(&config.system, &groups, &customers, &offboarded);
    let names: HashMap<_, _> = customers.iter().map(|c| (c.id, c.name.as_str())).collect();

    let mut reports = Vec::with_capacity(given.hosts.len());
    for host in &given.hosts {
        let import = match coverage.place(host.groups.iter().map(|g| g.id.as_str())) {
            Placement::Customer(id) => Import::Customer(names[&id].to_owned()),
            Placement::Uncovered => Import::Uncovered,
            Placement::Offboarded => Import::Offboarded,
            Placement::Conflict(_) => Import::Conflict,
        };
        let alert = Alert {
            severity: given.severity,
            customer: match &import {
                Import::Customer(name) => Some(name.clone()),
                _ => None,
            },
            host_groups: host.groups.iter().map(|g| g.name.clone()).collect(),
            tags: given.tags.clone(),
            trigger: given.trigger.clone(),
        };
        reports.push(report(
            given.event.as_deref(),
            host,
            &import,
            &alert,
            &config.rules,
        ));
    }
    Ok(reports.join("\n"))
}

/// `"a"`, `"a" and "b"`, `"a", "b" and "c"`.
fn quoted_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("{i:?}")).collect();
    list(&quoted, "and")
}

/// What `check` prints of one host of the alert: whether the host is imported, the alert as the
/// rules see it, and the rule that decides it with its outcome, or that none does.
pub fn report(
    event: Option<&str>,
    host: &HostWithGroups,
    import: &Import,
    alert: &Alert,
    rules: &Rules,
) -> String {
    let groups: Vec<String> = host.groups.iter().map(|g| g.name.clone()).collect();
    let in_groups = match groups.len() {
        0 => "in no host group".to_owned(),
        1 => format!("in the host group {}", quoted_list(&groups)),
        _ => format!("in the host groups {}", quoted_list(&groups)),
    };
    let imported = match import {
        Import::Customer(name) => format!("an asset of {name}."),
        Import::Uncovered => {
            "not imported, since no customer covers its groups: its alerts open no ticket."
                .to_owned()
        }
        Import::Offboarded => "not imported, since an offboarded customer covers its groups: \
                               its alerts open no ticket."
            .to_owned(),
        Import::Conflict => "not imported, since two customers or more cover its groups: its \
                             alerts open no ticket."
            .to_owned(),
    };
    let tags = match alert.tags.as_slice() {
        [] => "no tags".to_owned(),
        tags => {
            // In name order: Zabbix gives them in none in particular.
            let mut written: Vec<String> = tags
                .iter()
                .map(|(tag, value)| match value.as_str() {
                    "" => tag.clone(),
                    value => format!("{tag}={value}"),
                })
                .collect();
            written.sort();
            format!("the tags {}", quoted_list(&written))
        }
    };
    let event = event.map_or(String::new(), |id| format!("Zabbix's event {id}, "));
    let decision = match rules.decide(alert) {
        None => "No rule matches: the alert is ignored.".to_owned(),
        Some(matched) => {
            let matches = match import {
                Import::Customer(_) => "matches",
                _ => "would match once the host is imported, an asset of a customer",
            };
            format!(
                "{} {matches}: {}.",
                rule_named(&matched, rules),
                outcome(&matched)
            )
        }
    };
    format!(
        "Host {:?}, {in_groups}: {imported}\nAlert: {event}severity {}, trigger {:?}, {tags}.\n\
         {decision}\n",
        host.name, alert.severity, alert.trigger
    )
}

/// The rule, starting a sentence: `Rule 2 ("Mail")`, with the default list named.
fn rule_named(matched: &Matched<'_>, rules: &Rules) -> String {
    let label = matched.label();
    let mut named = label[..1].to_uppercase() + &label[1..];
    if rules.is_default() {
        named.push_str(" of the default list, the configuration having no rules,");
    }
    named
}

fn outcome(matched: &Matched<'_>) -> String {
    let OpenTicket {
        priority,
        service,
        on_recovery,
        close_in_zabbix,
    } = match &matched.rule.outcome {
        Outcome::Ignore => return "the alert is ignored".to_owned(),
        Outcome::Open(ticket) => ticket,
    };
    let service = match service {
        Some(service) => format!(", linking the service {service:?} when the customer uses it"),
        None => String::new(),
    };
    let recovery = match on_recovery {
        Recovery::ResolveIfNotWorkedOn => {
            "a note, and the ticket resolved only when no agent has worked on it"
        }
        Recovery::Resolve => "a note, and the ticket resolved",
        Recovery::Note => "a note, and the ticket left unresolved",
    };
    let close = if *close_in_zabbix {
        "the problem closed in Zabbix, when its trigger allows it"
    } else {
        "the problem left open in Zabbix"
    };
    format!(
        "a ticket opened with priority {}{service}; on recovery, {recovery}; once an agent \
         resolves the ticket, {close}",
        priority.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::HostGroup;

    fn host(groups: &[&str]) -> HostWithGroups {
        HostWithGroups {
            name: "SRV-01".into(),
            groups: groups
                .iter()
                .enumerate()
                .map(|(i, name)| HostGroup {
                    id: (20 + i).to_string(),
                    name: (*name).to_owned(),
                })
                .collect(),
        }
    }

    fn rules(toml: &str) -> Rules {
        let mut table: toml::Table = toml::from_str(toml).unwrap();
        let Some(toml::Value::Array(rules)) = table.remove("rules") else {
            panic!("no [[rules]] in {toml}")
        };
        Rules::read(rules).unwrap()
    }

    fn alert(customer: Option<&str>, groups: &[&str]) -> Alert {
        Alert {
            severity: Severity::High,
            customer: customer.map(str::to_owned),
            host_groups: groups.iter().map(|g| (*g).to_owned()).collect(),
            tags: vec![
                ("service".into(), "mail".into()),
                ("scope".into(), "".into()),
            ],
            trigger: "Mail queue is too long".into(),
        }
    }

    #[test]
    fn the_report_names_the_rule_that_matches_and_its_outcome() {
        let rules = rules(
            r#"
[[rules]]
name = "Backups"
trigger_contains = "backup"
outcome = "ignore"

[[rules]]
name = "Mail"
customer = "Brightfield"
tags = [{ tag = "service", value = "mail" }]
outcome = "open"
priority = "high"
service = "Hosted mail"
on_recovery = "note"
close_in_zabbix = true
"#,
        );
        let groups = ["Brightfield/Offices", "Linux servers"];
        let brightfield = Import::Customer("Brightfield".into());
        assert_eq!(
            report(
                Some("4213"),
                &host(&groups),
                &brightfield,
                &alert(Some("Brightfield"), &groups),
                &rules
            ),
            "Host \"SRV-01\", in the host groups \"Brightfield/Offices\" and \"Linux servers\": \
             an asset of Brightfield.\n\
             Alert: Zabbix's event 4213, severity high, trigger \"Mail queue is too long\", the tags \
             \"scope\" and \"service=mail\".\n\
             Rule 2 (\"Mail\") matches: a ticket opened with priority high, linking the service \
             \"Hosted mail\" when the customer uses it; on recovery, a note, and the ticket left \
             unresolved; once an agent resolves the ticket, the problem closed in Zabbix, when \
             its trigger allows it.\n"
        );

        let backup = Alert {
            trigger: "Backup failed".into(),
            tags: vec![],
            ..alert(Some("Brightfield"), &groups[..1])
        };
        assert_eq!(
            report(None, &host(&groups[..1]), &brightfield, &backup, &rules),
            "Host \"SRV-01\", in the host group \"Brightfield/Offices\": an asset of \
             Brightfield.\n\
             Alert: severity high, trigger \"Backup failed\", no tags.\n\
             Rule 1 (\"Backups\") matches: the alert is ignored.\n"
        );
    }

    #[test]
    fn the_report_says_when_no_rule_matches_and_when_the_host_is_not_imported() {
        let rules =
            rules("[[rules]]\ncustomer = \"Brightfield\"\noutcome = \"open\"\npriority = \"low\"");
        assert_eq!(
            report(
                None,
                &host(&["Lab"]),
                &Import::Uncovered,
                &alert(None, &["Lab"]),
                &rules
            ),
            "Host \"SRV-01\", in the host group \"Lab\": not imported, since no customer covers \
             its groups: its alerts open no ticket.\n\
             Alert: severity high, trigger \"Mail queue is too long\", the tags \
             \"scope\" and \"service=mail\".\n\
             No rule matches: the alert is ignored.\n"
        );
        for (import, sentence) in [
            (
                Import::Offboarded,
                "not imported, since an offboarded customer covers its groups",
            ),
            (
                Import::Conflict,
                "not imported, since two customers or more cover its groups",
            ),
        ] {
            let printed = report(None, &host(&[]), &import, &alert(None, &[]), &rules);
            assert!(
                printed.starts_with(&format!("Host \"SRV-01\", in no host group: {sentence}")),
                "{printed}"
            );
        }

        // A rule that holds without a customer would decide only once the host is imported.
        let any_high = super::tests::rules(
            "[[rules]]\nmin_severity = \"high\"\noutcome = \"open\"\npriority = \"low\"",
        );
        let printed = report(
            None,
            &host(&["Lab"]),
            &Import::Uncovered,
            &alert(None, &["Lab"]),
            &any_high,
        );
        assert!(
            printed.ends_with(
                "\nRule 1 would match once the host is imported, an asset of a customer: a \
                 ticket opened with priority low; on recovery, a note, and the ticket resolved \
                 only when no agent has worked on it; once an agent resolves the ticket, the \
                 problem left open in Zabbix.\n"
            ),
            "{printed}"
        );
    }

    #[test]
    fn the_report_says_when_the_default_list_decides() {
        let printed = report(
            None,
            &host(&["Brightfield"]),
            &Import::Customer("Brightfield".into()),
            &alert(Some("Brightfield"), &["Brightfield"]),
            &Rules::default_list(),
        );
        assert!(
            printed.ends_with(
                "Rule 2 (\"High\") of the default list, the configuration having no rules, \
                 matches: a ticket opened with priority high; on recovery, a note, and the \
                 ticket resolved only when no agent has worked on it; once an agent resolves \
                 the ticket, the problem left open in Zabbix.\n"
            ),
            "{printed}"
        );
    }

    #[test]
    fn a_tag_is_given_as_its_name_or_its_name_and_value() {
        assert_eq!(parse_tag("scope"), Ok(("scope".into(), "".into())));
        assert_eq!(
            parse_tag("service = mail"),
            Ok(("service".into(), "mail".into()))
        );
        assert_eq!(
            parse_tag("url=https://example.com/?a=b"),
            Ok(("url".into(), "https://example.com/?a=b".into()))
        );
        assert!(parse_tag("=mail").is_err());
        assert_eq!(
            parse_severity("Not classified"),
            Ok(Severity::NotClassified)
        );
        assert!(parse_severity("critical").is_err());
    }
}
