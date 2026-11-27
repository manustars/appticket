//! The rules that decide what an alert does (ADR 0010): an ordered list in the configuration
//! file, `[[alerts.rules]]`, where the first rule whose conditions all hold decides. A condition
//! left out matches anything; an alert no rule matches is ignored. Without rules, the default
//! list opens Disaster with priority urgent and High with priority high, and ignores the rest.

use hd_addon_sdk::api::Priority;
use serde::Deserialize;

/// A problem as the rules see it, already resolved from Zabbix and the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    /// The customer the host is an asset of, by name; none when no customer covers the host, or
    /// two do.
    pub customer: Option<String>,
    /// The names of the host's groups, such as `Brightfield/Offices`.
    pub host_groups: Vec<String>,
    /// The event's tags, as name and value.
    pub tags: Vec<(String, String)>,
    /// The trigger's name, with its macros resolved, as the event has it.
    pub trigger: String,
}

/// Zabbix's severities, from the lowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    NotClassified,
    Information,
    Warning,
    Average,
    High,
    Disaster,
}

impl Severity {
    /// From the lowest to the highest.
    pub const ALL: [Severity; 6] = [
        Severity::NotClassified,
        Severity::Information,
        Severity::Warning,
        Severity::Average,
        Severity::High,
        Severity::Disaster,
    ];

    /// The severity of Zabbix's number for it, 0 (not classified) to 5 (disaster).
    pub fn from_number(number: u8) -> Option<Severity> {
        Severity::ALL.get(usize::from(number)).copied()
    }

    /// Its name as the rules write it, such as `not classified`.
    pub fn name(self) -> &'static str {
        match self {
            Severity::NotClassified => "not classified",
            Severity::Information => "information",
            Severity::Warning => "warning",
            Severity::Average => "average",
            Severity::High => "high",
            Severity::Disaster => "disaster",
        }
    }

    /// The severity of this name, ignoring case and surrounding whitespace.
    pub fn from_name(name: &str) -> Option<Severity> {
        let name = name.trim().to_lowercase();
        Severity::ALL.into_iter().find(|s| s.name() == name)
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Which customer's hosts a rule is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The customer the host is an asset of, by name, compared ignoring case.
    Customer(String),
    /// A host group of the host, by name, or a group nested in it by `/`, compared ignoring case.
    HostGroup(String),
}

/// An event tag a rule asks for, by name, with the value when one is given; compared ignoring
/// case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCondition {
    pub tag: String,
    pub value: Option<String>,
}

/// What must hold of a problem for a rule to match: all of them; one left out matches anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conditions {
    /// The problem's severity is this one or higher.
    pub min_severity: Option<Severity>,
    pub scope: Option<Scope>,
    /// Each of them is among the event's tags.
    pub tags: Vec<TagCondition>,
    /// The trigger's name contains this text, ignoring case.
    pub trigger_contains: Option<String>,
}

/// What the add-on does when a problem recovers, on the ticket its rule opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Recovery {
    /// A note, and resolving the ticket only when no agent has worked on it.
    #[default]
    ResolveIfNotWorkedOn,
    /// A note, and resolving the ticket in any case.
    Resolve,
    /// A note, never resolving the ticket.
    Note,
}

impl Recovery {
    const ALL: [Recovery; 3] = [
        Recovery::ResolveIfNotWorkedOn,
        Recovery::Resolve,
        Recovery::Note,
    ];

    /// Its name as the rules write it.
    pub fn name(self) -> &'static str {
        match self {
            Recovery::ResolveIfNotWorkedOn => "resolve_if_not_worked_on",
            Recovery::Resolve => "resolve",
            Recovery::Note => "note",
        }
    }
}

/// A ticket a rule opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTicket {
    pub priority: Priority,
    /// A service, by name, which the ticket links when the customer uses it.
    pub service: Option<String>,
    pub on_recovery: Recovery,
    /// Whether the add-on closes the problem in Zabbix when an agent resolves the ticket.
    pub close_in_zabbix: bool,
}

/// What a rule does with the alerts it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ignore,
    Open(OpenTicket),
}

/// A rule of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The name the configuration gives it, for those who read the logs and `check`.
    pub name: Option<String>,
    pub conditions: Conditions,
    pub outcome: Outcome,
}

/// The rules, in their order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    rules: Vec<Rule>,
    /// The configuration has no rules: these are the default list.
    default: bool,
}

/// The rule that decides a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Matched<'a> {
    /// Its position in the list, from 1.
    pub position: usize,
    pub rule: &'a Rule,
}

static IGNORE: Outcome = Outcome::Ignore;

impl Rules {
    /// The list of the configuration in its order; the default list when it is empty.
    pub fn new(rules: Vec<Rule>) -> Rules {
        if rules.is_empty() {
            return Rules::default_list();
        }
        Rules {
            rules,
            default: false,
        }
    }

    /// The list without rules in the configuration: Disaster opens a ticket with priority
    /// urgent, High one with priority high, and the rest is ignored.
    pub fn default_list() -> Rules {
        let open = |name: &str, severity: Severity, priority: Priority| Rule {
            name: Some(name.to_owned()),
            conditions: Conditions {
                min_severity: Some(severity),
                ..Conditions::default()
            },
            outcome: Outcome::Open(OpenTicket {
                priority,
                service: None,
                on_recovery: Recovery::default(),
                close_in_zabbix: false,
            }),
        };
        Rules {
            rules: vec![
                open("Disaster", Severity::Disaster, Priority::Urgent),
                open("High", Severity::High, Priority::High),
            ],
            default: true,
        }
    }

    /// Whether these are the default list, the configuration having none.
    pub fn is_default(&self) -> bool {
        self.default
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// The first rule whose conditions all hold of `problem`; none when no rule matches, and the
    /// alert is then ignored.
    pub fn decide(&self, problem: &Problem) -> Option<Matched<'_>> {
        self.rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.conditions.hold(problem))
            .map(|(i, rule)| Matched {
                position: i + 1,
                rule,
            })
    }

    /// What the alert of `problem` does: the outcome of the rule that decides it, else ignored.
    pub fn outcome(&self, problem: &Problem) -> &Outcome {
        self.decide(problem).map_or(&IGNORE, |m| &m.rule.outcome)
    }
}

/// The same text, ignoring case and surrounding whitespace.
fn same(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

impl Conditions {
    fn hold(&self, problem: &Problem) -> bool {
        let severity = self.min_severity.is_none_or(|min| problem.severity >= min);
        let scope = match &self.scope {
            None => true,
            Some(Scope::Customer(name)) => {
                problem.customer.as_deref().is_some_and(|c| same(c, name))
            }
            Some(Scope::HostGroup(name)) => {
                let name = name.trim().to_lowercase();
                let nested = format!("{name}/");
                problem.host_groups.iter().any(|group| {
                    let group = group.trim().to_lowercase();
                    group == name || group.starts_with(&nested)
                })
            }
        };
        let tags = self.tags.iter().all(|wanted| {
            problem.tags.iter().any(|(tag, value)| {
                same(tag, &wanted.tag) && wanted.value.as_deref().is_none_or(|v| same(value, v))
            })
        });
        let trigger = self.trigger_contains.as_deref().is_none_or(|text| {
            problem
                .trigger
                .to_lowercase()
                .contains(&text.trim().to_lowercase())
        });
        severity && scope && tags && trigger
    }
}

/// A rule as the configuration file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    min_severity: Option<String>,
    #[serde(default)]
    customer: Option<String>,
    #[serde(default)]
    host_group: Option<String>,
    #[serde(default)]
    tags: Vec<RawTag>,
    #[serde(default)]
    trigger_contains: Option<String>,
    outcome: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    service: Option<String>,
    #[serde(default)]
    on_recovery: Option<String>,
    #[serde(default)]
    close_in_zabbix: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTag {
    tag: String,
    #[serde(default)]
    value: Option<String>,
}

/// The names of `values`, as a sentence lists them: `a, b or c`.
fn one_of(values: &[&str]) -> String {
    match values {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

impl Rules {
    /// The rules as the configuration file writes them, each a table of `[[alerts.rules]]`; the
    /// default list when there are none. A rule that does not fit is refused with a sentence
    /// naming it by its position and its name.
    pub fn read(written: Vec<toml::Value>) -> Result<Rules, String> {
        let mut rules = Vec::with_capacity(written.len());
        let mut names: Vec<(usize, String)> = Vec::new();
        for (i, value) in written.into_iter().enumerate() {
            let position = i + 1;
            let named = value
                .get("name")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned);
            let label = match &named {
                Some(name) => format!("rule {position} ({name:?})"),
                None => format!("rule {position}"),
            };
            let rule = RawRule::deserialize(value)
                .map_err(|e| e.message().trim().to_owned())
                .and_then(Rule::try_from)
                .map_err(|problem| format!("alerts.rules: {label}: {problem}"))?;
            if let Some(name) = &rule.name {
                if let Some((other, _)) = names.iter().find(|(_, n)| same(n, name)) {
                    return Err(format!(
                        "alerts.rules: {label}: rule {other} has that name too: give each rule \
                         a name of its own"
                    ));
                }
                names.push((position, name.clone()));
            }
            rules.push(rule);
        }
        Ok(Rules::new(rules))
    }
}

impl TryFrom<RawRule> for Rule {
    type Error = String;

    fn try_from(raw: RawRule) -> Result<Rule, String> {
        let filled = |s: Option<String>, what: &str| match s {
            Some(s) if s.trim().is_empty() => Err(format!("its {what} is blank")),
            other => Ok(other.map(|s| s.trim().to_owned())),
        };
        let name = filled(raw.name, "name")?;
        let min_severity = match filled(raw.min_severity, "min_severity")? {
            None => None,
            Some(written) => Some(Severity::from_name(&written).ok_or_else(|| {
                let names: Vec<&str> = Severity::ALL.iter().map(|s| s.name()).collect();
                format!(
                    "min_severity is {written:?}, not a severity of Zabbix: give {}",
                    one_of(&names)
                )
            })?),
        };
        let scope = match (
            filled(raw.customer, "customer")?,
            filled(raw.host_group, "host_group")?,
        ) {
            (Some(_), Some(_)) => {
                return Err("it names both a customer and a host group: give one".into());
            }
            (Some(customer), None) => Some(Scope::Customer(customer)),
            (None, Some(group)) => Some(Scope::HostGroup(group)),
            (None, None) => None,
        };
        let mut tags = Vec::with_capacity(raw.tags.len());
        for tag in raw.tags {
            tags.push(TagCondition {
                tag: filled(Some(tag.tag), "tag's name")?.unwrap_or_default(),
                value: tag.value.map(|v| v.trim().to_owned()),
            });
        }
        let conditions = Conditions {
            min_severity,
            scope,
            tags,
            trigger_contains: filled(raw.trigger_contains, "trigger_contains")?,
        };

        let priority = filled(raw.priority, "priority")?;
        let service = filled(raw.service, "service")?;
        let on_recovery = filled(raw.on_recovery, "on_recovery")?;
        let outcome = match raw.outcome.trim().to_lowercase().as_str() {
            "ignore" => {
                let given: Vec<&str> = [
                    ("priority", priority.is_some()),
                    ("service", service.is_some()),
                    ("on_recovery", on_recovery.is_some()),
                    ("close_in_zabbix", raw.close_in_zabbix.is_some()),
                ]
                .into_iter()
                .filter_map(|(setting, given)| given.then_some(setting))
                .collect();
                if !given.is_empty() {
                    let (verb, pronoun) = match given.len() {
                        1 => ("means", "it"),
                        _ => ("mean", "them"),
                    };
                    return Err(format!(
                        "it ignores the alert, so {} {verb} nothing: remove {pronoun}, or open a \
                         ticket",
                        one_of(&given).replace(" or ", " and ")
                    ));
                }
                Outcome::Ignore
            }
            "open" => {
                let priority = priority.ok_or(
                    "it opens a ticket without a priority: give one of low, normal, high or urgent",
                )?;
                let priority = parse_priority(&priority).ok_or_else(|| {
                    format!("priority is {priority:?}: give low, normal, high or urgent")
                })?;
                let on_recovery = match on_recovery {
                    None => Recovery::default(),
                    Some(written) => Recovery::ALL
                        .into_iter()
                        .find(|r| same(r.name(), &written))
                        .ok_or_else(|| {
                            let names: Vec<&str> = Recovery::ALL.iter().map(|r| r.name()).collect();
                            format!("on_recovery is {written:?}: give {}", one_of(&names))
                        })?,
                };
                Outcome::Open(OpenTicket {
                    priority,
                    service,
                    on_recovery,
                    close_in_zabbix: raw.close_in_zabbix.unwrap_or(false),
                })
            }
            _ => {
                return Err(format!(
                    "outcome is {:?}: give open or ignore",
                    raw.outcome.trim()
                ));
            }
        };
        Ok(Rule {
            name,
            conditions,
            outcome,
        })
    }
}

fn parse_priority(written: &str) -> Option<Priority> {
    [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Urgent,
    ]
    .into_iter()
    .find(|p| same(p.as_str(), written))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules of `[[rules]]` tables in `toml`.
    fn rules(toml: &str) -> Rules {
        Rules::read(written(toml)).unwrap()
    }

    fn written(toml: &str) -> Vec<toml::Value> {
        let mut table: toml::Table = toml::from_str(toml).unwrap();
        match table.remove("rules") {
            Some(toml::Value::Array(rules)) => rules,
            other => panic!("no [[rules]] in {toml}: {other:?}"),
        }
    }

    /// A problem of this severity, of no customer, without groups or tags, named `trigger`.
    fn problem(severity: Severity, trigger: &str) -> Problem {
        Problem {
            severity,
            customer: None,
            host_groups: vec![],
            tags: vec![],
            trigger: trigger.to_owned(),
        }
    }

    /// The position of the rule that decides `problem`, from 1.
    fn decided(rules: &Rules, problem: &Problem) -> Option<usize> {
        rules.decide(problem).map(|m| m.position)
    }

    #[test]
    fn the_first_rule_that_matches_decides_and_none_matching_ignores_the_alert() {
        let rules = rules(
            r#"
[[rules]]
trigger_contains = "backup"
outcome = "ignore"

[[rules]]
min_severity = "high"
outcome = "open"
priority = "urgent"

[[rules]]
min_severity = "warning"
outcome = "open"
priority = "normal"
"#,
        );
        // Both the first and the second hold: the first decides.
        assert_eq!(
            decided(&rules, &problem(Severity::Disaster, "Backup failed")),
            Some(1)
        );
        assert_eq!(
            decided(&rules, &problem(Severity::High, "Disk is full")),
            Some(2)
        );
        assert_eq!(
            decided(&rules, &problem(Severity::Warning, "Disk is full")),
            Some(3)
        );
        assert_eq!(
            decided(&rules, &problem(Severity::Information, "Disk is full")),
            None
        );
        assert_eq!(
            rules.outcome(&problem(Severity::Information, "x")),
            &Outcome::Ignore
        );
    }

    /// Whether the one rule written in `toml` matches `problem`.
    fn matches(toml: &str, problem: &Problem) -> bool {
        let rules = rules(&format!("[[rules]]\noutcome = \"ignore\"\n{toml}"));
        decided(&rules, problem).is_some()
    }

    #[test]
    fn a_rule_without_conditions_matches_any_problem() {
        for severity in Severity::ALL {
            assert!(matches("", &problem(severity, "")), "{severity}");
        }
    }

    #[test]
    fn a_minimum_severity_matches_it_and_the_ones_above() {
        let at_least = |min: &str| -> Vec<&str> {
            Severity::ALL
                .into_iter()
                .filter(|s| matches(&format!("min_severity = {min:?}"), &problem(*s, "")))
                .map(Severity::name)
                .collect()
        };
        assert_eq!(
            at_least("not classified"),
            [
                "not classified",
                "information",
                "warning",
                "average",
                "high",
                "disaster"
            ]
        );
        assert_eq!(
            at_least("information"),
            ["information", "warning", "average", "high", "disaster"]
        );
        assert_eq!(
            at_least("Warning"),
            ["warning", "average", "high", "disaster"]
        );
        assert_eq!(at_least("average"), ["average", "high", "disaster"]);
        assert_eq!(at_least("HIGH"), ["high", "disaster"]);
        assert_eq!(at_least(" disaster "), ["disaster"]);
    }

    #[test]
    fn a_customer_matches_by_name_ignoring_case_and_never_a_host_of_no_customer() {
        let rule = "customer = \"Brightfield\"";
        let of = |customer: Option<&str>| Problem {
            customer: customer.map(str::to_owned),
            ..problem(Severity::High, "")
        };
        assert!(matches(rule, &of(Some("Brightfield"))));
        assert!(matches(rule, &of(Some("BRIGHTFIELD"))));
        assert!(!matches(rule, &of(Some("Brightfield Logistics"))));
        assert!(!matches(rule, &of(Some("Harbor Lane"))));
        assert!(!matches(rule, &of(None)));
    }

    #[test]
    fn a_host_group_matches_it_and_the_groups_nested_in_it() {
        let rule = "host_group = \"Brightfield/Offices\"";
        let in_groups = |groups: &[&str]| Problem {
            host_groups: groups.iter().map(|g| (*g).to_owned()).collect(),
            ..problem(Severity::High, "")
        };
        assert!(matches(rule, &in_groups(&["Brightfield/Offices"])));
        assert!(matches(rule, &in_groups(&["brightfield/offices"])));
        assert!(matches(
            rule,
            &in_groups(&["Linux servers", "Brightfield/Offices/Floor 2"])
        ));
        // Nested by name, also where Zabbix has no group between them.
        assert!(matches(
            rule,
            &in_groups(&["Brightfield/Offices/Floor 2/Rack"])
        ));
        assert!(!matches(rule, &in_groups(&["Brightfield"])));
        assert!(!matches(rule, &in_groups(&["Brightfield/Offices East"])));
        assert!(!matches(rule, &in_groups(&["Harbor/Brightfield/Offices"])));
        assert!(!matches(rule, &in_groups(&[])));
    }

    #[test]
    fn every_tag_of_a_rule_must_be_present_with_its_value_when_it_gives_one() {
        let rule = r#"tags = [{ tag = "service", value = "mail" }, { tag = "scope" }]"#;
        let tagged = |tags: &[(&str, &str)]| Problem {
            tags: tags
                .iter()
                .map(|(t, v)| ((*t).to_owned(), (*v).to_owned()))
                .collect(),
            ..problem(Severity::High, "")
        };
        assert!(matches(
            rule,
            &tagged(&[("service", "mail"), ("scope", "availability")])
        ));
        assert!(matches(
            rule,
            &tagged(&[("Scope", ""), ("other", "x"), ("SERVICE", "Mail")])
        ));
        // A tag twice, one of them with the value asked for.
        assert!(matches(
            rule,
            &tagged(&[("service", "web"), ("service", "mail"), ("scope", "x")])
        ));
        assert!(!matches(rule, &tagged(&[("service", "mail")])));
        assert!(!matches(
            rule,
            &tagged(&[("service", "web"), ("scope", "x")])
        ));
        assert!(!matches(
            rule,
            &tagged(&[("service", "mailbox"), ("scope", "x")])
        ));
        assert!(!matches(rule, &tagged(&[])));
    }

    #[test]
    fn a_trigger_name_matches_when_it_contains_the_text_ignoring_case() {
        let rule = "trigger_contains = \"disk space\"";
        assert!(matches(
            rule,
            &problem(Severity::High, "Low disk space on /var")
        ));
        assert!(matches(rule, &problem(Severity::High, "DISK SPACE is low")));
        assert!(!matches(rule, &problem(Severity::High, "Disk is full")));
        assert!(!matches(rule, &problem(Severity::High, "")));
    }

    #[test]
    fn a_rule_matches_only_when_all_its_conditions_hold() {
        let rule = r#"
min_severity = "average"
customer = "Brightfield"
tags = [{ tag = "service", value = "mail" }]
trigger_contains = "queue"
"#;
        let all = Problem {
            severity: Severity::Average,
            customer: Some("Brightfield".into()),
            host_groups: vec!["Brightfield".into()],
            tags: vec![("service".into(), "mail".into())],
            trigger: "Mail queue is too long".into(),
        };
        assert!(matches(rule, &all));
        let but = |change: fn(&mut Problem)| {
            let mut problem = all.clone();
            change(&mut problem);
            problem
        };
        assert!(!matches(rule, &but(|p| p.severity = Severity::Warning)));
        assert!(!matches(
            rule,
            &but(|p| p.customer = Some("Harbor Lane".into()))
        ));
        assert!(!matches(rule, &but(|p| p.tags.clear())));
        assert!(!matches(
            rule,
            &but(|p| p.trigger = "Mail server is down".into())
        ));
    }

    /// The outcome of the one rule written in `toml`.
    fn outcome_of(toml: &str) -> Outcome {
        rules(&format!("[[rules]]\n{toml}")).rules()[0]
            .outcome
            .clone()
    }

    #[test]
    fn a_rule_opens_a_ticket_with_a_priority_a_service_and_what_to_do_on_recovery() {
        assert_eq!(outcome_of("outcome = \"ignore\""), Outcome::Ignore);
        assert_eq!(
            outcome_of("outcome = \"open\"\npriority = \"low\""),
            Outcome::Open(OpenTicket {
                priority: Priority::Low,
                service: None,
                on_recovery: Recovery::ResolveIfNotWorkedOn,
                close_in_zabbix: false,
            })
        );
        assert_eq!(
            outcome_of(
                r#"outcome = "Open"
priority = "Urgent"
service = " Hosted mail "
on_recovery = "resolve"
close_in_zabbix = true"#
            ),
            Outcome::Open(OpenTicket {
                priority: Priority::Urgent,
                service: Some("Hosted mail".into()),
                on_recovery: Recovery::Resolve,
                close_in_zabbix: true,
            })
        );
        for (written, priority, recovery) in [
            ("normal", Priority::Normal, "note"),
            ("high", Priority::High, "resolve_if_not_worked_on"),
        ] {
            let outcome = outcome_of(&format!(
                "outcome = \"open\"\npriority = {written:?}\non_recovery = {recovery:?}"
            ));
            let Outcome::Open(ticket) = outcome else {
                panic!("{written}: {outcome:?}")
            };
            assert_eq!(ticket.priority, priority);
            assert_eq!(ticket.on_recovery.name(), recovery);
        }
    }

    #[test]
    fn without_rules_the_default_list_opens_disaster_as_urgent_and_high_as_high() {
        let rules = Rules::read(vec![]).unwrap();
        assert!(rules.is_default());
        let decided: Vec<(&str, Option<Priority>)> = Severity::ALL
            .into_iter()
            .map(|severity| {
                let opened = match rules.outcome(&problem(severity, "Anything")) {
                    Outcome::Open(ticket) => {
                        assert_eq!(ticket.service, None);
                        assert_eq!(ticket.on_recovery, Recovery::ResolveIfNotWorkedOn);
                        assert!(!ticket.close_in_zabbix);
                        Some(ticket.priority)
                    }
                    Outcome::Ignore => None,
                };
                (severity.name(), opened)
            })
            .collect();
        assert_eq!(
            decided,
            [
                ("not classified", None),
                ("information", None),
                ("warning", None),
                ("average", None),
                ("high", Some(Priority::High)),
                ("disaster", Some(Priority::Urgent)),
            ]
        );
        let names: Vec<Option<&str>> = rules.rules().iter().map(|r| r.name.as_deref()).collect();
        assert_eq!(names, [Some("Disaster"), Some("High")]);

        // A list of the configuration replaces it whole.
        let own =
            super::tests::rules("[[rules]]\nmin_severity = \"warning\"\noutcome = \"ignore\"");
        assert!(!own.is_default());
        assert_eq!(
            own.outcome(&problem(Severity::Disaster, "")),
            &Outcome::Ignore
        );
    }

    #[test]
    fn a_rule_that_does_not_fit_is_refused_with_a_sentence_naming_it() {
        let refused = |toml: &str| -> String { Rules::read(written(toml)).unwrap_err() };
        let first_fits = "[[rules]]\noutcome = \"ignore\"\n\n[[rules]]\n";
        for (rule, sentence) in [
            (
                "name = \"Mail\"\nmin_severity = \"critical\"\noutcome = \"ignore\"",
                "alerts.rules: rule 2 (\"Mail\"): min_severity is \"critical\", not a severity \
                 of Zabbix: give not classified, information, warning, average, high or disaster",
            ),
            (
                "outcome = \"open\"",
                "alerts.rules: rule 2: it opens a ticket without a priority: give one of low, \
                 normal, high or urgent",
            ),
            (
                "outcome = \"open\"\npriority = \"p1\"",
                "alerts.rules: rule 2: priority is \"p1\": give low, normal, high or urgent",
            ),
            (
                "outcome = \"open\"\npriority = \"high\"\non_recovery = \"close\"",
                "alerts.rules: rule 2: on_recovery is \"close\": give resolve_if_not_worked_on, \
                 resolve or note",
            ),
            (
                "outcome = \"ignore\"\npriority = \"high\"\nclose_in_zabbix = false",
                "alerts.rules: rule 2: it ignores the alert, so priority and close_in_zabbix \
                 mean nothing: remove them, or open a ticket",
            ),
            (
                "outcome = \"ignore\"\nservice = \"Hosted mail\"",
                "alerts.rules: rule 2: it ignores the alert, so service means nothing: remove \
                 it, or open a ticket",
            ),
            (
                "outcome = \"page\"",
                "alerts.rules: rule 2: outcome is \"page\": give open or ignore",
            ),
            (
                "name = \"Both\"\ncustomer = \"Brightfield\"\nhost_group = \"Brightfield\"\n\
                 outcome = \"ignore\"",
                "alerts.rules: rule 2 (\"Both\"): it names both a customer and a host group: \
                 give one",
            ),
            (
                "customer = \" \"\noutcome = \"ignore\"",
                "alerts.rules: rule 2: its customer is blank",
            ),
            (
                "tags = [{ tag = \"\" }]\noutcome = \"ignore\"",
                "alerts.rules: rule 2: its tag's name is blank",
            ),
            (
                "trigger_contains = \"\"\noutcome = \"ignore\"",
                "alerts.rules: rule 2: its trigger_contains is blank",
            ),
            (
                "name = \"ignore\"\noutcome = \"ignore\"\n\n[[rules]]\nname = \"Ignore\"\n\
                 outcome = \"ignore\"",
                "alerts.rules: rule 3 (\"Ignore\"): rule 2 has that name too: give each rule a \
                 name of its own",
            ),
        ] {
            assert_eq!(refused(&format!("{first_fits}{rule}")), sentence, "{rule}");
        }
        // What the file's own reading finds, with the setting it is about.
        for (rule, words) in [
            (
                "min_severty = \"high\"\noutcome = \"ignore\"",
                "min_severty",
            ),
            ("min_severity = \"high\"", "outcome"),
            ("tags = [{ name = \"x\" }]\noutcome = \"ignore\"", "name"),
            ("close_in_zabbix = \"yes\"\noutcome = \"ignore\"", "bool"),
        ] {
            let sentence = refused(&format!("{first_fits}name = \"Odd\"\n{rule}"));
            assert!(
                sentence.starts_with("alerts.rules: rule 2 (\"Odd\"): ")
                    && sentence.contains(words),
                "{sentence}"
            );
        }
    }
}
