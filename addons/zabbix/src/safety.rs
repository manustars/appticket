//! The safety limit (ADR 0010): when the add-on would open more than a number of tickets in a
//! few minutes, counting those of the webhook and of the reconciliation together, it stops
//! opening tickets, opens one urgent ticket for the help desk's own organization, the safety
//! ticket, and holds back the problems the rules open until an agent resolves or closes it.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hd_addon_sdk::NewTicket;
use hd_addon_sdk::api::{Priority, TicketState, TicketSummary};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::alerts::utc;
use crate::claims::lock;
use crate::references::TicketReference;

/// How many tickets the add-on opens at most in how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub tickets: usize,
    pub window: Duration,
}

impl Default for Limit {
    /// 20 tickets in 5 minutes.
    fn default() -> Self {
        Limit {
            tickets: 20,
            window: Duration::from_secs(5 * 60),
        }
    }
}

impl Limit {
    /// The window in whole minutes, as the configuration gives it.
    pub fn minutes(self) -> u64 {
        self.window.as_secs() / 60
    }
}

/// The add-on's wait after it reached the limit: since when, the safety ticket once opened, and
/// the problems held back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trip {
    /// When the wait started, to the second.
    pub since: OffsetDateTime,
    /// The safety ticket's number, once the core opened it.
    pub ticket: Option<i64>,
    /// The event ids of the problems held back during this run of the add-on.
    pub held: BTreeSet<String>,
    /// How many held back problems the safety ticket last said.
    pub told: usize,
}

impl Trip {
    /// The ticket reference of its safety ticket, such as `safety-limit:2026-11-27T09:00:00Z`:
    /// one per wait, so that a later wait opens a ticket of its own.
    pub fn reference(&self) -> String {
        TicketReference::Safety(self.since).to_string()
    }
}

/// Whether a problem the rules open gets its ticket now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Open it, then say with [`Gate::opened`] whether the core opened a ticket; [`admit`] does
    /// it with a [`Slot`].
    Open,
    /// Hold it back: the add-on waits. `tripped` when this problem started the wait, so that the
    /// safety ticket is to be opened.
    HeldBack { tripped: bool },
}

/// The safety limit's state, between the webhook and the reconciliation.
#[derive(Debug)]
pub struct Gate {
    limit: Limit,
    /// When the tickets of the window were opened, oldest first.
    opened: VecDeque<Instant>,
    /// Problems admitted whose ticket is being opened.
    pending: usize,
    trip: Option<Trip>,
    /// When the latest trip started, so that the next starts later.
    latest: Option<OffsetDateTime>,
}

impl Gate {
    pub fn new(limit: Limit) -> Gate {
        Gate {
            limit,
            opened: VecDeque::new(),
            pending: 0,
            trip: None,
            latest: None,
        }
    }

    pub fn limit(&self) -> Limit {
        self.limit
    }

    /// The wait, while the add-on holds back the problems.
    pub fn holding(&self) -> Option<&Trip> {
        self.trip.as_ref()
    }

    /// Counts in the window tickets opened before the add-on started, at these times, so that a
    /// restart does not start it empty.
    pub fn seed(&mut self, opened: impl IntoIterator<Item = Instant>) {
        let mut opened: Vec<Instant> = opened.into_iter().chain(self.opened.drain(..)).collect();
        opened.sort();
        self.opened = opened.into();
    }

    /// Whether the problem of `event`, which the rules open and which has no ticket, gets one
    /// now, at `now` (`wall` on the clock): while the add-on waits it is held back, and so is the
    /// one that would open more than the limit's tickets in its window, which starts the wait.
    pub fn admit(&mut self, event: &str, now: Instant, wall: OffsetDateTime) -> Admission {
        if let Some(trip) = &mut self.trip {
            trip.held.insert(event.to_owned());
            return Admission::HeldBack { tripped: false };
        }
        while self
            .opened
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= self.limit.window)
        {
            self.opened.pop_front();
        }
        if self.opened.len() + self.pending < self.limit.tickets {
            self.pending += 1;
            return Admission::Open;
        }
        let mut since = wall
            .to_offset(time::UtcOffset::UTC)
            .replace_nanosecond(0)
            .unwrap_or(wall);
        if let Some(latest) = self.latest
            && since <= latest
        {
            since = latest + Duration::from_secs(1);
        }
        self.latest = Some(since);
        self.trip = Some(Trip {
            since,
            ticket: None,
            held: BTreeSet::from([event.to_owned()]),
            told: 0,
        });
        Admission::HeldBack { tripped: true }
    }

    /// The end of an [`Admission::Open`]: `created` when the core opened a ticket at `now`,
    /// which counts in the window; not when it failed or answered one opened before.
    pub fn opened(&mut self, now: Instant, created: bool) {
        self.pending = self.pending.saturating_sub(1);
        if created {
            self.opened.push_back(now);
        }
    }

    /// The safety ticket of the wait that started at `since` is `number`.
    pub fn safety_ticket(&mut self, since: OffsetDateTime, number: i64) {
        if let Some(trip) = &mut self.trip
            && trip.since == since
        {
            trip.ticket = Some(number);
        }
    }

    /// The add-on waits, since `since`, for the safety ticket `number`, which it found open at
    /// startup.
    pub fn resume(&mut self, since: OffsetDateTime, number: i64) {
        self.latest = Some(self.latest.map_or(since, |latest| latest.max(since)));
        self.trip = Some(Trip {
            since,
            ticket: Some(number),
            held: BTreeSet::new(),
            told: 0,
        });
    }

    /// An agent resolved or closed the safety ticket of the wait that started at `since`: the
    /// add-on opens tickets again, counted afresh.
    pub fn end_wait(&mut self, since: OffsetDateTime) {
        if self.trip.as_ref().is_some_and(|trip| trip.since == since) {
            self.trip = None;
            self.opened.clear();
        }
    }

    /// The safety ticket and how many problems were held back, when more than it last said.
    pub fn untold(&self) -> Option<(i64, usize)> {
        let trip = self.trip.as_ref()?;
        let number = trip.ticket?;
        (trip.held.len() > trip.told).then_some((number, trip.held.len()))
    }

    /// The safety ticket of the wait that started at `since` now says that `count` problems were
    /// held back.
    pub fn told(&mut self, since: OffsetDateTime, count: usize) {
        if let Some(trip) = &mut self.trip
            && trip.since == since
        {
            trip.told = trip.told.max(count);
        }
    }
}

/// A place in the window for a problem whose ticket is being opened: given back when dropped
/// before [`Slot::opened`], such as when the webhook's deadline drops the opening.
#[derive(Debug)]
pub struct Slot<'a> {
    gate: &'a Mutex<Gate>,
    done: bool,
}

impl Slot<'_> {
    /// The opening ended now: `created` when the core opened a ticket.
    pub fn opened(mut self, created: bool) {
        self.done = true;
        lock(self.gate).opened(Instant::now(), created);
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if !self.done {
            lock(self.gate).opened(Instant::now(), false);
        }
    }
}

/// [`Gate::admit`] through its lock, with the place of an admitted problem held by a [`Slot`];
/// otherwise whether this problem started the wait.
pub fn admit<'a>(
    gate: &'a Mutex<Gate>,
    event: &str,
    now: Instant,
    wall: OffsetDateTime,
) -> Result<Slot<'a>, bool> {
    match lock(gate).admit(event, now, wall) {
        Admission::Open => Ok(Slot { gate, done: false }),
        Admission::HeldBack { tripped } => Err(tripped),
    }
}

/// The wait a restarted add-on goes on with: the one of the latest safety ticket among those it
/// opened that are not closed, when that ticket is still open or waiting; as its start and the
/// ticket's number.
pub fn held_since(opened: &[TicketSummary]) -> Option<(OffsetDateTime, i64)> {
    let (since, latest) = opened
        .iter()
        .filter_map(|t| Some((TicketReference::read(t.reference.as_deref()?)?.safety()?, t)))
        .max_by_key(|(since, _)| *since)?;
    matches!(latest.state, TicketState::Open | TicketState::Waiting)
        .then_some((since, latest.number))
}

/// When the tickets of problems among those the add-on opened that are not closed may have been
/// opened within the window before `now` (`wall` on the clock), to seed it at startup. The
/// add-on reads only when each last changed, which is never before it was opened: a ticket
/// changed in the window counts, even one opened before it.
pub fn recently_opened(
    opened: &[TicketSummary],
    window: Duration,
    now: Instant,
    wall: OffsetDateTime,
) -> Vec<Instant> {
    opened
        .iter()
        .filter(|t| {
            t.reference
                .as_deref()
                .and_then(TicketReference::read)
                .is_some_and(|r| r.problem().is_some())
        })
        .filter_map(|t| {
            let ago = Duration::try_from(wall - t.updated_at).unwrap_or(Duration::ZERO);
            (ago < window).then(|| now.checked_sub(ago).unwrap_or(now))
        })
        .collect()
}

/// The safety ticket of `trip`: urgent, for the help desk's own organization `help_desk`, saying
/// how many problems of `system` (such as `Zabbix`) were held back and since when, with the wait's
/// reference, so that it is opened once.
pub fn safety_ticket(system: &str, limit: Limit, trip: &Trip, help_desk: Uuid) -> NewTicket {
    let minutes = limit.minutes();
    NewTicket {
        customer_id: help_desk,
        title: format!(
            "{system}'s alerts are held back: more than {} tickets in {minutes} minutes",
            limit.tickets
        ),
        description: Some(format!(
            "The {system} add-on would have opened more than {tickets} tickets in {minutes} \
             minutes, and stopped opening tickets at {since}: {held} held back.\n\
             While this ticket is open or waiting, {system}'s problems open no ticket; their \
             recoveries and updates still add notes to the tickets already opened, and resolve \
             them as the rules say.\n\
             Resolve or close this ticket to let the add-on open tickets again: it then opens \
             those of the problems still active that the rules open, under the same limit.",
            tickets = limit.tickets,
            since = utc(trip.since.unix_timestamp()),
            held = problems(trip.held.len()),
        )),
        priority: Some(Priority::Urgent),
        asset_ids: vec![],
        service_ids: vec![],
        reference: Some(trip.reference()),
    }
}

/// The note that tells the safety ticket how many problems were held back since `since`.
pub fn held_back_note(since: OffsetDateTime, count: usize) -> String {
    format!(
        "Since {}, {} held back.",
        utc(since.unix_timestamp()),
        problems(count)
    )
}

/// `1 problem the rules open was`, `3 problems the rules open were`.
fn problems(count: usize) -> String {
    match count {
        1 => "1 problem the rules open was".to_owned(),
        n => format!("{n} problems the rules open were"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_795_770_000 + seconds).unwrap()
    }

    fn small() -> Gate {
        Gate::new(Limit {
            tickets: 2,
            window: Duration::from_secs(300),
        })
    }

    /// Admits a problem at `seconds` after `start` and opens its ticket at once.
    fn open(gate: &mut Gate, start: Instant, seconds: u64, event: &str) -> Admission {
        let now = start + Duration::from_secs(seconds);
        let admitted = gate.admit(event, now, at(seconds as i64));
        if admitted == Admission::Open {
            gate.opened(now, true);
        }
        admitted
    }

    #[test]
    fn the_default_is_20_tickets_in_5_minutes() {
        assert_eq!(
            Limit::default(),
            Limit {
                tickets: 20,
                window: Duration::from_secs(300)
            }
        );
    }

    #[test]
    fn one_ticket_more_than_the_limit_in_its_window_starts_the_wait_and_later_ones_are_held_back() {
        let start = Instant::now();
        let mut gate = small();
        assert_eq!(open(&mut gate, start, 0, "101"), Admission::Open);
        assert_eq!(open(&mut gate, start, 10, "102"), Admission::Open);
        assert_eq!(
            open(&mut gate, start, 20, "103"),
            Admission::HeldBack { tripped: true }
        );
        let trip = gate.holding().unwrap();
        assert_eq!(trip.since, at(20));
        assert_eq!(trip.held, BTreeSet::from(["103".to_owned()]));
        // Even once the window has passed, the add-on waits for the safety ticket.
        assert_eq!(
            open(&mut gate, start, 900, "104"),
            Admission::HeldBack { tripped: false }
        );
        assert_eq!(gate.holding().unwrap().held.len(), 2);
    }

    #[test]
    fn tickets_older_than_the_window_no_longer_count() {
        let start = Instant::now();
        let mut gate = small();
        assert_eq!(open(&mut gate, start, 0, "101"), Admission::Open);
        assert_eq!(open(&mut gate, start, 200, "102"), Admission::Open);
        // 300 seconds after the first: it has left the window.
        assert_eq!(open(&mut gate, start, 300, "103"), Admission::Open);
        assert_eq!(
            open(&mut gate, start, 301, "104"),
            Admission::HeldBack { tripped: true }
        );
    }

    #[test]
    fn only_tickets_the_core_opened_count_and_those_being_opened_hold_their_place() {
        let start = Instant::now();
        let mut gate = small();
        // Two at once: both admitted, a third waits for neither.
        assert_eq!(gate.admit("101", start, at(0)), Admission::Open);
        assert_eq!(gate.admit("102", start, at(0)), Admission::Open);
        assert_eq!(
            gate.admit("103", start, at(0)),
            Admission::HeldBack { tripped: true }
        );
        let mut gate = small();
        assert_eq!(gate.admit("101", start, at(0)), Admission::Open);
        // The core answered a ticket opened before, or failed: no ticket counts.
        gate.opened(start, false);
        assert_eq!(gate.admit("102", start, at(0)), Admission::Open);
        gate.opened(start, false);
        for event in ["103", "104"] {
            assert_eq!(open(&mut gate, start, 1, event), Admission::Open);
        }
        assert_eq!(
            open(&mut gate, start, 2, "105"),
            Admission::HeldBack { tripped: true }
        );
    }

    #[test]
    fn the_wait_ends_with_its_safety_ticket_and_tickets_are_then_counted_afresh() {
        let start = Instant::now();
        let mut gate = small();
        open(&mut gate, start, 0, "101");
        open(&mut gate, start, 1, "102");
        assert_eq!(
            open(&mut gate, start, 2, "103"),
            Admission::HeldBack { tripped: true }
        );
        let first = gate.holding().unwrap().since;
        // The end of another wait changes nothing.
        gate.end_wait(at(-60));
        assert!(gate.holding().is_some());
        gate.end_wait(first);
        assert_eq!(gate.holding(), None);
        // Within the same window: the tickets before the wait no longer count.
        assert_eq!(open(&mut gate, start, 3, "103"), Admission::Open);
        assert_eq!(open(&mut gate, start, 3, "104"), Admission::Open);
        assert_eq!(
            open(&mut gate, start, 3, "105"),
            Admission::HeldBack { tripped: true }
        );
        // In the same second as the first wait, by the clock: still a wait of its own.
        let mut gate = small();
        for (seconds, event) in [(0, "1"), (0, "2")] {
            open(&mut gate, start, seconds, event);
        }
        gate.admit("3", start, at(2));
        gate.end_wait(at(2));
        for event in ["4", "5"] {
            assert_eq!(gate.admit(event, start, at(2)), Admission::Open);
            gate.opened(start, true);
        }
        gate.admit("6", start, at(2));
        assert_eq!(gate.holding().unwrap().since, at(3));
        assert_ne!(
            TicketReference::Safety(at(2)).to_string(),
            gate.holding().unwrap().reference()
        );
    }

    #[test]
    fn a_wait_found_at_startup_holds_back_every_problem() {
        let start = Instant::now();
        let mut gate = small();
        gate.resume(at(-3600), 42);
        assert_eq!(
            open(&mut gate, start, 0, "101"),
            Admission::HeldBack { tripped: false }
        );
        let trip = gate.holding().unwrap();
        assert_eq!((trip.since, trip.ticket), (at(-3600), Some(42)));
        gate.end_wait(at(-3600));
        assert_eq!(open(&mut gate, start, 1, "101"), Admission::Open);
    }

    #[test]
    fn the_safety_ticket_is_told_how_many_were_held_back_when_more_were() {
        let start = Instant::now();
        let mut gate = small();
        open(&mut gate, start, 0, "101");
        open(&mut gate, start, 1, "102");
        open(&mut gate, start, 2, "103");
        assert_eq!(gate.untold(), None, "no safety ticket yet");
        let since = gate.holding().unwrap().since;
        gate.safety_ticket(at(-1), 7);
        assert_eq!(gate.untold(), None, "another wait's");
        gate.safety_ticket(since, 7);
        assert_eq!(gate.untold(), Some((7, 1)));
        gate.told(since, 1);
        assert_eq!(gate.untold(), None);
        // The same problem again is the same one.
        open(&mut gate, start, 3, "103");
        assert_eq!(gate.untold(), None);
        open(&mut gate, start, 3, "104");
        open(&mut gate, start, 3, "105");
        assert_eq!(gate.untold(), Some((7, 3)), "until told");
        assert_eq!(gate.untold(), Some((7, 3)));
        gate.told(at(-1), 3);
        assert_eq!(gate.untold(), Some((7, 3)), "another wait's");
        gate.told(since, 3);
        assert_eq!(gate.untold(), None);
    }

    fn summary(number: i64, state: TicketState, reference: Option<&str>) -> TicketSummary {
        TicketSummary {
            number,
            title: "A ticket".into(),
            state,
            priority: Priority::Normal,
            customer_id: Uuid::from_u128(1),
            requester_email: None,
            reference: reference.map(str::to_owned),
            updated_at: at(0),
        }
    }

    #[test]
    fn a_restarted_add_on_waits_for_the_latest_safety_ticket_while_it_is_open_or_waiting() {
        use TicketState::*;
        let older = TicketReference::Safety(at(-7200)).to_string();
        let latest = TicketReference::Safety(at(-60)).to_string();
        for (state, waits) in [
            (Open, true),
            (Waiting, true),
            (Resolved, false),
            (Closed, false),
        ] {
            let opened = [
                summary(5, Open, Some("4213")),
                summary(3, Open, Some(&older)),
                summary(9, state, Some(&latest)),
                summary(6, Open, None),
            ];
            assert_eq!(
                held_since(&opened),
                waits.then_some((at(-60), 9)),
                "{state:?}"
            );
        }
        assert_eq!(
            held_since(&[summary(5, Open, Some("4213")), summary(6, Waiting, None)]),
            None
        );
    }

    #[test]
    fn the_safety_ticket_is_urgent_for_the_help_desk_and_says_how_many_and_since_when() {
        let help_desk = Uuid::from_u128(9);
        let trip = Trip {
            since: OffsetDateTime::from_unix_timestamp(1_795_770_000).unwrap(),
            ticket: None,
            held: BTreeSet::from(["103".to_owned()]),
            told: 0,
        };
        let ticket = safety_ticket("Zabbix", Limit::default(), &trip, help_desk);
        assert_eq!(
            ticket,
            NewTicket {
                customer_id: help_desk,
                title: "Zabbix's alerts are held back: more than 20 tickets in 5 minutes".into(),
                description: Some(
                    "The Zabbix add-on would have opened more than 20 tickets in 5 minutes, and \
                     stopped opening tickets at 2026-11-27 09:00:00 UTC: 1 problem the rules \
                     open was held back.\n\
                     While this ticket is open or waiting, Zabbix's problems open no ticket; \
                     their recoveries and updates still add notes to the tickets already \
                     opened, and resolve them as the rules say.\n\
                     Resolve or close this ticket to let the add-on open tickets again: it then \
                     opens those of the problems still active that the rules open, under the \
                     same limit."
                        .into()
                ),
                priority: Some(Priority::Urgent),
                asset_ids: vec![],
                service_ids: vec![],
                reference: Some("safety-limit:2026-11-27T09:00:00Z".into()),
            }
        );
        assert_eq!(
            held_back_note(trip.since, 3),
            "Since 2026-11-27 09:00:00 UTC, 3 problems the rules open were held back."
        );
    }

    #[test]
    fn the_window_in_minutes() {
        assert_eq!(Limit::default().minutes(), 5);
    }

    #[tokio::test]
    async fn an_opening_dropped_mid_way_gives_its_place_back() {
        let start = Instant::now();
        let gate = Mutex::new(small());
        let opening = async {
            let slot = admit(&gate, "101", start, at(0)).unwrap();
            // The core never answers.
            std::future::pending::<()>().await;
            slot.opened(true);
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(20), opening)
                .await
                .is_err()
        );
        // Both places are free: two problems get theirs, a third starts the wait.
        let first = admit(&gate, "102", start, at(1)).unwrap();
        let second = admit(&gate, "103", start, at(1)).unwrap();
        assert!(admit(&gate, "104", start, at(1)).unwrap_err(), "tripped");
        first.opened(true);
        second.opened(false);
        assert_eq!(lock(&gate).holding().unwrap().held.len(), 1);
    }

    #[test]
    fn a_slot_counts_the_ticket_the_core_opened() {
        let start = Instant::now();
        let gate = Mutex::new(small());
        admit(&gate, "101", start, at(0)).unwrap().opened(true);
        admit(&gate, "102", start, at(0)).unwrap().opened(false);
        admit(&gate, "103", start, at(0)).unwrap().opened(true);
        assert!(
            admit(&gate, "104", start, at(0)).unwrap_err(),
            "two opened: tripped"
        );
    }

    #[test]
    fn tickets_opened_before_a_restart_count_in_the_window_until_it_passes() {
        let start = Instant::now();
        let mut gate = small();
        gate.seed([start, start + Duration::from_secs(100)]);
        assert_eq!(
            open(&mut gate, start, 120, "101"),
            Admission::HeldBack { tripped: true }
        );
        let mut gate = small();
        gate.seed([start]);
        assert_eq!(open(&mut gate, start, 120, "101"), Admission::Open);
        assert_eq!(
            open(&mut gate, start, 130, "102"),
            Admission::HeldBack { tripped: true }
        );
        let mut gate = small();
        gate.seed([start, start + Duration::from_secs(10)]);
        // The first has left the window.
        assert_eq!(open(&mut gate, start, 300, "101"), Admission::Open);
    }

    #[test]
    fn the_tickets_of_problems_changed_within_the_window_may_have_been_opened_in_it() {
        let now = Instant::now() + Duration::from_secs(3600);
        let wall = at(0);
        let changed = |number: i64, reference: Option<&str>, seconds_ago: i64| TicketSummary {
            updated_at: at(-seconds_ago),
            ..summary(number, TicketState::Open, reference)
        };
        let safety = TicketReference::Safety(at(-10)).to_string();
        let opened = [
            changed(1, Some("4213"), 10),
            changed(2, Some("4214"), 299),
            changed(3, Some("4215"), 300),
            changed(4, Some(&safety), 10),
            changed(5, None, 10),
            changed(6, Some("4216"), -5),
        ];
        assert_eq!(
            recently_opened(&opened, Duration::from_secs(300), now, wall),
            [
                now - Duration::from_secs(10),
                now - Duration::from_secs(299),
                now
            ],
            "neither the one changed 5 minutes ago, the safety ticket, nor one without a \
             reference; one the core's clock puts ahead counts as now"
        );
    }
}
