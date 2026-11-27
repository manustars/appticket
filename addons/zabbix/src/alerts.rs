//! Alerts in (ADR 0010): what the add-on does with the problem, recovery and update events that
//! Zabbix's webhook media type sends it. A problem the rules open becomes one ticket of the
//! customer of the host's asset, with the problem's event id as the ticket reference; its
//! recovery adds a note and resolves the ticket as the rule says; an update with a message from a
//! Zabbix user other than the add-on's own adds a note.

use hd_addon_sdk::api::TicketState;
use hd_addon_sdk::{NewTicket, api::Priority};
use serde::Deserialize;
use uuid::Uuid;

use crate::details::severity_name;
use crate::rules::{Matched, Recovery};
use crate::zabbix::{Event, HostWithGroups};

/// The tag of Zabbix's event that the webhook's answer gives the ticket's number in.
pub const TICKET_TAG: &str = "hd_ticket";

/// What the media type sends, its parameters as Zabbix resolved them: a macro Zabbix does not
/// resolve for the kind of message, such as `{EVENT.UPDATE.MESSAGE}` in a problem's, comes as it
/// is written.
#[derive(Debug, Clone, Deserialize)]
pub struct Payload {
    /// `{EVENT.ID}`: the problem's event id, also in its recovery's and its updates' messages.
    pub event_id: String,
    /// `{EVENT.VALUE}`: 1 for a problem, 0 for a recovery.
    pub event_value: String,
    /// `{EVENT.UPDATE.STATUS}`: 1 for an update.
    pub event_update_status: String,
    /// `{EVENT.SOURCE}`: 0 for a trigger's event.
    pub event_source: String,
    /// `{USER.FULLNAME}`: who updated the problem, as `Name Surname (username)`.
    #[serde(default)]
    pub user: String,
    /// `{EVENT.UPDATE.ACTION}`: what the update did, such as `acknowledged and commented`.
    #[serde(default)]
    pub action: String,
    /// `{EVENT.UPDATE.MESSAGE}`.
    #[serde(default)]
    pub message: String,
}

/// An event of a trigger's problem, as the add-on acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Problem {
        event: String,
    },
    Recovery {
        event: String,
    },
    Update {
        event: String,
        /// Who made it, as Zabbix names them; none when Zabbix did not say.
        user: Option<String>,
        /// What it did; none when Zabbix did not say.
        action: Option<String>,
        /// Its message; none when it has none.
        message: Option<String>,
    },
}

impl Notice {
    /// The notice a payload gives; none for an event that is not a trigger's, which opens no
    /// ticket. A payload that is no event of Zabbix's is refused with a sentence.
    pub fn read(payload: &Payload) -> Result<Option<Notice>, String> {
        match resolved(&payload.event_source) {
            Some(TRIGGER_SOURCE) => {}
            Some(_) => return Ok(None),
            None => {
                return Err(format!(
                    "the event's source {:?} is not Zabbix's: send {{EVENT.SOURCE}} as \
                     event_source",
                    payload.event_source
                ));
            }
        }
        let event = resolved(&payload.event_id)
            .ok_or("the event has no id: send {EVENT.ID} as event_id")?;
        if !event.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("the event's id {event:?} is not a number"));
        }
        let event = event.to_owned();
        if resolved(&payload.event_update_status) == Some("1") {
            return Ok(Some(Notice::Update {
                event,
                user: resolved(&payload.user).map(str::to_owned),
                action: resolved(&payload.action).map(str::to_owned),
                message: resolved(&payload.message).map(str::to_owned),
            }));
        }
        match resolved(&payload.event_value) {
            Some("1") => Ok(Some(Notice::Problem { event })),
            Some("0") => Ok(Some(Notice::Recovery { event })),
            _ => Err(format!(
                "the event's value {:?} is neither 1, a problem, nor 0, a recovery: send \
                 {{EVENT.VALUE}} as event_value",
                payload.event_value
            )),
        }
    }
}

/// The value of a parameter: none when blank, or when Zabbix left its macro unresolved.
fn resolved(value: &str) -> Option<&str> {
    let value = value.trim();
    let unresolved = value.starts_with('{') && value.ends_with('}') && !value.contains(' ');
    (!value.is_empty() && !unresolved).then_some(value)
}

/// `{EVENT.SOURCE}` of a trigger's event; the others are discovery, autoregistration, internal
/// and services.
const TRIGGER_SOURCE: &str = "0";

/// Whether the user Zabbix names as `{USER.FULLNAME}` (`Name Surname (username)`) is the one with
/// this username, as Zabbix spells both from the same user.
pub fn made_by(user: &str, username: &str) -> bool {
    let user = user.trim();
    let named = user
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once('('))
        .map_or(user, |(_, name)| name);
    named.trim() == username.trim()
}

/// The note an update adds: none without a message, or when the add-on's own Zabbix user,
/// `own_username`, made it.
pub fn update_note(
    user: Option<&str>,
    action: Option<&str>,
    message: Option<&str>,
    own_username: &str,
) -> Option<String> {
    let message = message?;
    if user.is_some_and(|user| made_by(user, own_username)) {
        return None;
    }
    let who = user.unwrap_or("A Zabbix user");
    Some(match action {
        Some(action) => format!("{who} updated the problem in Zabbix ({action}): {message}"),
        None => format!("{who} updated the problem in Zabbix: {message}"),
    })
}

/// The note a recovery adds, with the time of the recovery when Zabbix gives it.
pub fn recovery_note(recovered_at: Option<i64>) -> String {
    match recovered_at {
        Some(clock) => format!("The problem recovered in Zabbix at {}.", utc(clock)),
        None => "The problem recovered in Zabbix.".to_owned(),
    }
}

/// Whether the recovery resolves the ticket: only an open or waiting one, as the rule says.
pub fn resolves(on_recovery: Recovery, state: TicketState, worked_on_by_agent: bool) -> bool {
    let resolvable = matches!(state, TicketState::Open | TicketState::Waiting);
    resolvable
        && match on_recovery {
            Recovery::ResolveIfNotWorkedOn => !worked_on_by_agent,
            Recovery::Resolve => true,
            Recovery::Note => false,
        }
}

/// A time of Zabbix's, in seconds since the Unix epoch, as the notes write it:
/// `2026-11-27 09:00:00 UTC`.
pub fn utc(clock: i64) -> String {
    let format =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
    time::OffsetDateTime::from_unix_timestamp(clock)
        .ok()
        .and_then(|at| at.format(&format).ok())
        .unwrap_or_else(|| format!("{clock} seconds after 1970-01-01 00:00:00 UTC"))
}

/// The service a rule's ticket links, by name, among those its customer uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Service {
    /// The rule names none.
    None,
    /// The customer uses it: this one.
    Used(Uuid),
    /// The customer does not use the service of this name.
    NotUsed(String),
}

/// What the ticket of a problem is made of.
#[derive(Debug, Clone)]
pub struct Opening<'a> {
    pub event: &'a Event,
    pub host: &'a HostWithGroups,
    pub customer: Uuid,
    pub customer_name: &'a str,
    /// The asset of the host for the customer; none when the catalog does not have it yet.
    pub asset: Option<Uuid>,
    pub rule: &'a Matched<'a>,
    pub priority: Priority,
    pub service: Service,
}

/// The ticket of a problem: for the host's customer, without a requester, linked to the host's
/// asset, with the rule's priority and service, titled with the host and the trigger's name, and
/// described with the problem's severity, start, tags and operational data. Its reference is the
/// problem's event id, so that opening it again opens no second ticket.
pub fn new_ticket(opening: &Opening<'_>) -> NewTicket {
    let Opening {
        event,
        host,
        customer,
        customer_name,
        asset,
        rule,
        priority,
        service,
    } = opening;
    let mut tags: Vec<String> = event
        .tags
        .iter()
        .map(|(tag, value)| match value.as_str() {
            "" => tag.clone(),
            value => format!("{tag}={value}"),
        })
        .collect();
    // In name order: Zabbix gives them in none in particular.
    tags.sort();
    let mut lines = vec![
        format!(
            "Zabbix's problem {} on {}, opened by {}.",
            event.id,
            host.name,
            rule.label()
        ),
        format!("Severity: {}", severity_name(event.severity)),
        format!("Started: {}", utc(event.clock)),
        format!(
            "Tags: {}",
            if tags.is_empty() {
                "none".to_owned()
            } else {
                tags.join(", ")
            }
        ),
        format!(
            "Operational data: {}",
            match event.operational_data.as_str() {
                "" => "none",
                data => data,
            }
        ),
    ];
    if let Service::NotUsed(name) = service {
        lines.push(format!(
            "The rule links the service {name:?}, which {customer_name} does not use: the ticket \
             links no service."
        ));
    }
    if asset.is_none() {
        lines.push(format!(
            "The catalog has no asset of {} for {customer_name} yet: the ticket links no asset.",
            host.name
        ));
    }
    NewTicket {
        customer_id: *customer,
        title: format!("{}: {}", host.name, event.name),
        description: Some(lines.join("\n")),
        priority: Some(*priority),
        asset_ids: asset.iter().copied().collect(),
        service_ids: match service {
            Service::Used(id) => vec![*id],
            _ => vec![],
        },
        reference: Some(event.id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::HostGroup;
    use crate::rules::{Conditions, OpenTicket, Outcome, Rule, Severity};

    /// A payload as Zabbix 7.0 sends it, from the media type's parameters.
    fn payload(json: serde_json::Value) -> Payload {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_problem_its_recovery_and_its_updates_are_told_apart() {
        // As Zabbix 7.0 resolves the media type's macros in each kind of message.
        let problem = payload(serde_json::json!({
            "event_id": "26", "event_value": "1", "event_update_status": "0",
            "event_source": "0", "user": "{USER.FULLNAME}",
            "action": "{EVENT.UPDATE.ACTION}", "message": "{EVENT.UPDATE.MESSAGE}"}));
        assert_eq!(
            Notice::read(&problem),
            Ok(Some(Notice::Problem { event: "26".into() }))
        );
        let recovery = Payload {
            event_value: "0".into(),
            ..problem.clone()
        };
        assert_eq!(
            Notice::read(&recovery),
            Ok(Some(Notice::Recovery { event: "26".into() }))
        );
        let update = payload(serde_json::json!({
            "event_id": "26", "event_value": "1", "event_update_status": "1",
            "event_source": "0", "user": "Olga Operator (olga)",
            "action": "acknowledged and commented", "message": " Looking at it "}));
        assert_eq!(
            Notice::read(&update),
            Ok(Some(Notice::Update {
                event: "26".into(),
                user: Some("Olga Operator (olga)".into()),
                action: Some("acknowledged and commented".into()),
                message: Some("Looking at it".into()),
            }))
        );
        // An update of a problem that has recovered is still an update.
        let late = Payload {
            event_value: "0".into(),
            message: "{EVENT.UPDATE.MESSAGE}".into(),
            ..update.clone()
        };
        assert_eq!(
            Notice::read(&late),
            Ok(Some(Notice::Update {
                event: "26".into(),
                user: Some("Olga Operator (olga)".into()),
                action: Some("acknowledged and commented".into()),
                message: None,
            }))
        );
    }

    #[test]
    fn an_event_that_is_not_a_trigger_s_is_left_alone_and_one_that_is_no_event_refused() {
        let problem = payload(serde_json::json!({
            "event_id": "26", "event_value": "1", "event_update_status": "0",
            "event_source": "0"}));
        // An event of discovery, autoregistration, internal or services.
        for source in ["1", "2", "3", "4"] {
            let other = Payload {
                event_source: source.into(),
                ..problem.clone()
            };
            assert_eq!(Notice::read(&other), Ok(None), "{source}");
        }
        for (change, sentence) in [
            (
                Payload {
                    event_id: "{EVENT.ID}".into(),
                    ..problem.clone()
                },
                "the event has no id: send {EVENT.ID} as event_id",
            ),
            (
                Payload {
                    event_id: "26; DROP".into(),
                    ..problem.clone()
                },
                "the event's id \"26; DROP\" is not a number",
            ),
            (
                Payload {
                    event_value: "{EVENT.VALUE}".into(),
                    ..problem.clone()
                },
                "the event's value \"{EVENT.VALUE}\" is neither 1, a problem, nor 0, a \
                 recovery: send {EVENT.VALUE} as event_value",
            ),
            (
                Payload {
                    event_source: "{EVENT.SOURCE}".into(),
                    ..problem.clone()
                },
                "the event's source \"{EVENT.SOURCE}\" is not Zabbix's: send {EVENT.SOURCE} as \
                 event_source",
            ),
        ] {
            assert_eq!(Notice::read(&change), Err(sentence.to_owned()));
        }
    }

    #[test]
    fn an_update_is_the_add_on_s_own_when_its_zabbix_user_made_it() {
        assert!(made_by("Zabbix Administrator (Admin)", "Admin"));
        // Zabbix spells the username the same way in both.
        assert!(!made_by("Zabbix Administrator (Admin)", "admin"));
        assert!(made_by(" (hd-addon) ", "hd-addon"));
        assert!(made_by("hd-addon", "hd-addon"));
        assert!(!made_by("Olga Operator (olga)", "Admin"));
        assert!(!made_by("Olga (Admin) Operator (olga)", "Admin"));
        assert!(!made_by("Olga Operator (xadmin)", "admin"));
        assert!(!made_by("Olga Operator (olga)", "olga Operator"));
    }

    #[test]
    fn an_update_with_a_message_adds_a_note_unless_the_add_on_made_it() {
        assert_eq!(
            update_note(
                Some("Olga Operator (olga)"),
                Some("acknowledged and commented"),
                Some("Looking at it"),
                "hd-addon"
            )
            .as_deref(),
            Some(
                "Olga Operator (olga) updated the problem in Zabbix (acknowledged and commented): \
                 Looking at it"
            )
        );
        assert_eq!(
            update_note(None, None, Some("Rebooted"), "hd-addon").as_deref(),
            Some("A Zabbix user updated the problem in Zabbix: Rebooted")
        );
        assert_eq!(
            update_note(
                Some("Olga Operator (olga)"),
                Some("acknowledged"),
                None,
                "hd-addon"
            ),
            None,
            "no message"
        );
        assert_eq!(
            update_note(
                Some("Help desk (hd-addon)"),
                Some("commented"),
                Some("Ticket 42 resolved"),
                "hd-addon"
            ),
            None,
            "the add-on's own"
        );
    }

    #[test]
    fn a_recovery_resolves_only_an_open_or_waiting_ticket_as_its_rule_says() {
        use Recovery::*;
        use TicketState::*;
        for (recovery, state, worked_on, resolved) in [
            (ResolveIfNotWorkedOn, Open, false, true),
            (ResolveIfNotWorkedOn, Waiting, false, true),
            (ResolveIfNotWorkedOn, Open, true, false),
            (ResolveIfNotWorkedOn, Waiting, true, false),
            (Resolve, Open, true, true),
            (Resolve, Waiting, false, true),
            (Note, Open, false, false),
            (Note, Waiting, true, false),
        ] {
            assert_eq!(
                resolves(recovery, state, worked_on),
                resolved,
                "{recovery:?} {state:?} worked on: {worked_on}"
            );
        }
        for recovery in [ResolveIfNotWorkedOn, Resolve, Note] {
            for state in [Resolved, Closed] {
                for worked_on in [false, true] {
                    assert!(
                        !resolves(recovery, state, worked_on),
                        "{recovery:?} {state:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn times_are_written_in_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(1_795_801_516), "2026-11-27 17:45:16 UTC");
        assert_eq!(
            recovery_note(Some(1_795_801_522)),
            "The problem recovered in Zabbix at 2026-11-27 17:45:22 UTC."
        );
        assert_eq!(recovery_note(None), "The problem recovered in Zabbix.");
    }

    fn event() -> Event {
        Event {
            id: "4213".into(),
            severity: Severity::High,
            name: "Mail queue is too long".into(),
            tags: vec![
                ("service".into(), "mail".into()),
                ("scope".into(), String::new()),
            ],
            host_ids: vec!["10683".into()],
            clock: 1_795_801_516,
            operational_data: "Queue: 1200".into(),
            recovery_id: None,
        }
    }

    fn host() -> HostWithGroups {
        HostWithGroups {
            id: "10683".into(),
            name: "SRV-01".into(),
            groups: vec![HostGroup {
                id: "22".into(),
                name: "Brightfield/Offices".into(),
            }],
        }
    }

    fn open_rule(name: Option<&str>) -> Rule {
        Rule {
            name: name.map(str::to_owned),
            conditions: Conditions::default(),
            outcome: Outcome::Open(OpenTicket {
                priority: Priority::High,
                service: Some("Hosted mail".into()),
                on_recovery: Recovery::default(),
                close_in_zabbix: false,
            }),
        }
    }

    #[test]
    fn a_problem_s_ticket_has_the_customer_asset_priority_service_title_and_description() {
        let customer = Uuid::from_u128(1);
        let asset = Uuid::from_u128(2);
        let service = Uuid::from_u128(3);
        let rule = open_rule(Some("Mail"));
        let matched = Matched {
            position: 2,
            rule: &rule,
        };
        let event = event();
        let host = host();
        let opening = Opening {
            event: &event,
            host: &host,
            customer,
            customer_name: "Brightfield",
            asset: Some(asset),
            rule: &matched,
            priority: Priority::High,
            service: Service::Used(service),
        };
        assert_eq!(
            new_ticket(&opening),
            NewTicket {
                customer_id: customer,
                title: "SRV-01: Mail queue is too long".into(),
                description: Some(
                    "Zabbix's problem 4213 on SRV-01, opened by rule 2 (\"Mail\").\n\
                     Severity: High\n\
                     Started: 2026-11-27 17:45:16 UTC\n\
                     Tags: scope, service=mail\n\
                     Operational data: Queue: 1200"
                        .into()
                ),
                priority: Some(Priority::High),
                asset_ids: vec![asset],
                service_ids: vec![service],
                reference: Some("4213".into()),
            }
        );

        // A service the customer does not use, no asset yet, no tags nor operational data.
        let bare = Event {
            tags: vec![],
            operational_data: String::new(),
            ..event.clone()
        };
        let unnamed = open_rule(None);
        let first = Matched {
            position: 1,
            rule: &unnamed,
        };
        let ticket = new_ticket(&Opening {
            event: &bare,
            asset: None,
            rule: &first,
            service: Service::NotUsed("Hosted mail".into()),
            priority: Priority::Urgent,
            ..opening.clone()
        });
        assert_eq!(ticket.priority, Some(Priority::Urgent));
        assert!(ticket.asset_ids.is_empty() && ticket.service_ids.is_empty());
        assert_eq!(
            ticket.description.as_deref(),
            Some(
                "Zabbix's problem 4213 on SRV-01, opened by rule 1.\n\
                 Severity: High\n\
                 Started: 2026-11-27 17:45:16 UTC\n\
                 Tags: none\n\
                 Operational data: none\n\
                 The rule links the service \"Hosted mail\", which Brightfield does not use: the \
                 ticket links no service.\n\
                 The catalog has no asset of SRV-01 for Brightfield yet: the ticket links no \
                 asset."
            )
        );
    }
}
