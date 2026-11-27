//! The add-on's ticket references: a problem's event id names the ticket of that problem, and
//! `safety-limit:<time>` the safety ticket of the wait that started then.

use std::fmt;

use time::OffsetDateTime;

/// What a ticket reference of the add-on names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketReference {
    /// The ticket of the problem with this event id, all digits.
    Problem(String),
    /// The safety ticket of the wait that started at this time, to the second.
    Safety(OffsetDateTime),
}

const SAFETY_PREFIX: &str = "safety-limit:";

const SINCE: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

impl TicketReference {
    /// The reference a ticket was opened with; none for one the add-on does not give.
    pub fn read(reference: &str) -> Option<TicketReference> {
        if !reference.is_empty() && reference.bytes().all(|b| b.is_ascii_digit()) {
            return Some(TicketReference::Problem(reference.to_owned()));
        }
        let since = reference.strip_prefix(SAFETY_PREFIX)?;
        time::PrimitiveDateTime::parse(since, SINCE)
            .ok()
            .map(|at| TicketReference::Safety(at.assume_utc()))
    }

    /// The event id of the problem it names, when it names one.
    pub fn problem(&self) -> Option<&str> {
        match self {
            TicketReference::Problem(event) => Some(event),
            TicketReference::Safety(_) => None,
        }
    }

    /// When the wait of the safety ticket it names started, when it names one.
    pub fn safety(&self) -> Option<OffsetDateTime> {
        match self {
            TicketReference::Safety(since) => Some(*since),
            TicketReference::Problem(_) => None,
        }
    }
}

impl fmt::Display for TicketReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TicketReference::Problem(event) => f.write_str(event),
            TicketReference::Safety(since) => {
                let since = since.to_offset(time::UtcOffset::UTC);
                match since.format(SINCE) {
                    Ok(at) => write!(f, "{SAFETY_PREFIX}{at}"),
                    Err(_) => write!(f, "{SAFETY_PREFIX}{}", since.unix_timestamp()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_problem_s_reference_is_its_event_id_and_a_safety_ticket_s_its_time() {
        let since = OffsetDateTime::from_unix_timestamp(1_795_770_000).unwrap();
        let safety = TicketReference::Safety(since);
        assert_eq!(safety.to_string(), "safety-limit:2026-11-27T09:00:00Z");
        assert_eq!(
            TicketReference::read("safety-limit:2026-11-27T09:00:00Z"),
            Some(safety.clone())
        );
        assert_eq!(safety.safety(), Some(since));
        assert_eq!(safety.problem(), None);
        let problem = TicketReference::read("4213").unwrap();
        assert_eq!(problem, TicketReference::Problem("4213".into()));
        assert_eq!((problem.problem(), problem.safety()), (Some("4213"), None));
        assert_eq!(problem.to_string(), "4213");
        for other in [
            "",
            "42 13",
            "-4",
            "10084@x",
            "safety-limit:",
            "safety-limit:yesterday",
        ] {
            assert_eq!(TicketReference::read(other), None, "{other}");
        }
    }
}
