//! The `check` command: which rule a problem meets and with what outcome, or that none does. The
//! problem is given by its event id in Zabbix, or by its severity, host, tags and trigger name.
//! The host's groups come from Zabbix and its customer from the external references customers
//! have, read from the core with the add-on's token as a round of the add-on reads them; nothing
//! is written anywhere.

use std::collections::HashMap;

use anyhow::Context;
use hd_addon_sdk::Client;

use crate::config::Config;
use crate::coverage::{Coverage, Placement};
use crate::problems::{self, HostWithGroups};
use crate::rules::{Matched, OpenTicket, Outcome, Problem, Recovery, Rules, Severity};
use crate::zabbix::Zabbix;

/// The problem to check, as the command line gives it.
#[derive(Debug, Clone, clap::Args)]
#[command(group(clap::ArgGroup::new("problem").required(true).args(["event", "host"])))]
pub struct CheckArgs {
    /// The problem's event id in Zabbix, such as 4213.
    #[arg(long, value_name = "ID")]
    pub event: Option<String>,
    /// The host, by its technical or its visible name.
    #[arg(long, requires = "severity", conflicts_with = "event")]
    pub host: Option<String>,
    /// The problem's severity: not classified, information, warning, average, high or disaster.
    #[arg(long, value_parser = parse_severity, requires = "host", conflicts_with = "event")]
    pub severity: Option<Severity>,
    /// An event tag, as NAME or NAME=VALUE; once for each tag.
    #[arg(long = "tag", value_name = "NAME[=VALUE]", value_parser = parse_tag, conflicts_with = "event")]
    pub tags: Vec<(String, String)>,
    /// The trigger's name, with its macros resolved, as the problem shows it.
    #[arg(long, value_name = "NAME", conflicts_with = "event")]
    pub trigger: Option<String>,
}

fn parse_severity(written: &str) -> Result<Severity, String> {
    Severity::from_name(written).ok_or_else(|| {
        format!(
            "{written:?} is not a severity of Zabbix: give not classified, information, \
             warning, average, high or disaster"
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

/// Runs the check and answers what to print.
pub async fn run(config: &Config, args: CheckArgs) -> anyhow::Result<String> {
    let zabbix = Zabbix::new(
        &config.zabbix_url,
        &config.zabbix_token,
        config.zabbix_timeout,
    )?;
    let core = Client::new(&config.core_url, &config.core_token)?;

    let (event, hosts, severity, tags, trigger) = match &args.event {
        Some(id) => {
            let event = problems::problem_event(&zabbix, id.trim()).await?;
            let hosts = problems::hosts_by_id(&zabbix, &event.host_ids).await?;
            if hosts.is_empty() {
                anyhow::bail!(
                    "the trigger of event {} is on no host the add-on's user can read",
                    event.id
                );
            }
            let (severity, tags, name) = (event.severity, event.tags.clone(), event.name.clone());
            (Some(event.id), hosts, severity, tags, name)
        }
        None => {
            let host = args.host.as_deref().unwrap_or_default();
            let host = problems::host_by_name(&zabbix, host.trim()).await?;
            let severity = args.severity.context("give the problem's severity")?;
            let trigger = args.trigger.clone().unwrap_or_default();
            (None, vec![host], severity, args.tags.clone(), trigger)
        }
    };

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

    let mut reports = Vec::with_capacity(hosts.len());
    for host in &hosts {
        let import = match coverage.place(host.groups.iter().map(|g| g.id.as_str())) {
            Placement::Customer(id) => Import::Customer(names[&id].to_owned()),
            Placement::Uncovered => Import::Uncovered,
            Placement::Offboarded => Import::Offboarded,
            Placement::Conflict(_) => Import::Conflict,
        };
        let problem = Problem {
            severity,
            customer: match &import {
                Import::Customer(name) => Some(name.clone()),
                _ => None,
            },
            host_groups: host.groups.iter().map(|g| g.name.clone()).collect(),
            tags: tags.clone(),
            trigger: trigger.clone(),
        };
        reports.push(report(
            event.as_deref(),
            host,
            &import,
            &problem,
            &config.rules,
        ));
    }
    Ok(reports.join("\n"))
}

/// `"a"`, `"a" and "b"`, `"a", "b" and "c"`.
fn quoted_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("{i:?}")).collect();
    match quoted.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// What `check` prints of one host of the problem: whether the host is imported, the problem as
/// the rules see it, and the rule that decides it with its outcome, or that none does.
pub fn report(
    event: Option<&str>,
    host: &HostWithGroups,
    import: &Import,
    problem: &Problem,
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
    let tags = match problem.tags.as_slice() {
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
    let event = event.map_or(String::new(), |id| format!("event {id}, "));
    let decision = match rules.decide(problem) {
        None => "No rule matches: the alert is ignored.".to_owned(),
        Some(matched) => format!("{} matches: {}.", label(&matched, rules), outcome(&matched)),
    };
    format!(
        "Host {:?}, {in_groups}: {imported}\nProblem: {event}severity {}, trigger {:?}, {tags}.\n\
         {decision}\n",
        host.name, problem.severity, problem.trigger
    )
}

fn label(matched: &Matched<'_>, rules: &Rules) -> String {
    let named = match &matched.rule.name {
        Some(name) => format!("Rule {} ({name:?})", matched.position),
        None => format!("Rule {}", matched.position),
    };
    if rules.is_default() {
        format!("{named} of the default list, the configuration having no rules,")
    } else {
        named
    }
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
            id: "10683".into(),
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

    fn problem(customer: Option<&str>, groups: &[&str]) -> Problem {
        Problem {
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
                &problem(Some("Brightfield"), &groups),
                &rules
            ),
            "Host \"SRV-01\", in the host groups \"Brightfield/Offices\" and \"Linux servers\": \
             an asset of Brightfield.\n\
             Problem: event 4213, severity high, trigger \"Mail queue is too long\", the tags \
             \"scope\" and \"service=mail\".\n\
             Rule 2 (\"Mail\") matches: a ticket opened with priority high, linking the service \
             \"Hosted mail\" when the customer uses it; on recovery, a note, and the ticket left \
             unresolved; once an agent resolves the ticket, the problem closed in Zabbix, when \
             its trigger allows it.\n"
        );

        let backup = Problem {
            trigger: "Backup failed".into(),
            tags: vec![],
            ..problem(Some("Brightfield"), &groups[..1])
        };
        assert_eq!(
            report(None, &host(&groups[..1]), &brightfield, &backup, &rules),
            "Host \"SRV-01\", in the host group \"Brightfield/Offices\": an asset of \
             Brightfield.\n\
             Problem: severity high, trigger \"Backup failed\", no tags.\n\
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
                &problem(None, &["Lab"]),
                &rules
            ),
            "Host \"SRV-01\", in the host group \"Lab\": not imported, since no customer covers \
             its groups: its alerts open no ticket.\n\
             Problem: severity high, trigger \"Mail queue is too long\", the tags \
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
            let printed = report(None, &host(&[]), &import, &problem(None, &[]), &rules);
            assert!(
                printed.starts_with(&format!("Host \"SRV-01\", in no host group: {sentence}")),
                "{printed}"
            );
        }
    }

    #[test]
    fn the_report_says_when_the_default_list_decides() {
        let printed = report(
            None,
            &host(&["Brightfield"]),
            &Import::Customer("Brightfield".into()),
            &problem(Some("Brightfield"), &["Brightfield"]),
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
