//! Holding back problems under the safety limit, between the [`Gate`] and the core: the add-on
//! admits each problem it would open a ticket for, opens the safety ticket when the limit is
//! reached, reads it to know when an agent has resolved or closed it, and at startup picks up the
//! wait and the window where the add-on left them.

use std::sync::Mutex;
use std::time::Instant;

use anyhow::Context;
use hd_addon_sdk::Client;
use hd_addon_sdk::api::TicketState;
use time::OffsetDateTime;
use tokio::sync::{Notify, OnceCell};

use crate::alerts::utc;
use crate::claims::lock;
use crate::safety::{self, Gate, Limit, Slot};

/// The safety limit of one add-on, for the webhook and the reconciliation together.
pub struct Hold {
    core: Client,
    /// The system's name, such as `Zabbix`, for the safety ticket.
    system: String,
    gate: Mutex<Gate>,
    /// Read once, before the first ticket: the wait and the window the add-on left.
    resumed: OnceCell<()>,
    /// Told when a problem of the webhook found the wait ended, so that the reconciliation opens
    /// the tickets held back without waiting for its interval.
    pub ended: Notify,
}

impl std::fmt::Debug for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hold")
            .field("gate", &self.gate)
            .finish_non_exhaustive()
    }
}

impl Hold {
    pub fn new(core: Client, system: impl Into<String>, limit: Limit) -> Hold {
        Hold {
            core,
            system: system.into(),
            gate: Mutex::new(Gate::new(limit)),
            resumed: OnceCell::new(),
            ended: Notify::new(),
        }
    }

    pub fn limit(&self) -> Limit {
        lock(&self.gate).limit()
    }

    /// Whether the add-on holds back the problems until an agent resolves or closes a safety
    /// ticket.
    pub fn holding(&self) -> bool {
        lock(&self.gate).holding().is_some()
    }

    /// Reads, once, what the add-on left when it stopped: the wait for the latest safety ticket
    /// it opened, while that is open or waiting, and the tickets it may have opened within the
    /// window.
    pub async fn resume(&self) -> anyhow::Result<()> {
        self.resumed
            .get_or_try_init(|| async {
                let opened = self
                    .core
                    .opened_tickets()
                    .await
                    .context("reading the tickets the add-on opened, for the safety limit")?;
                let mut gate = lock(&self.gate);
                let window = gate.limit().window;
                gate.seed(safety::recently_opened(
                    &opened,
                    window,
                    Instant::now(),
                    OffsetDateTime::now_utc(),
                ));
                if let Some((since, number)) = safety::held_since(&opened) {
                    tracing::warn!(ticket = number, since = %utc(since.unix_timestamp()),
                        "holding back the problems until an agent resolves or closes the safety \
                         ticket");
                    gate.resume(since, number);
                }
                anyhow::Ok(())
            })
            .await?;
        Ok(())
    }

    /// While the add-on holds back the problems, reads the safety ticket by its reference and
    /// ends the wait once an agent has resolved or closed it; answers whether it ended. Opens
    /// the safety ticket when the core has none yet.
    pub async fn check(&self) -> anyhow::Result<bool> {
        let Some((since, reference)) = lock(&self.gate).holding().map(|t| (t.since, t.reference()))
        else {
            return Ok(false);
        };
        let ticket = match self.core.ticket_by_reference(&reference).await {
            Ok(ticket) => ticket,
            Err(e) if e.status() == Some(404) => {
                self.open_safety_ticket().await;
                return Ok(false);
            }
            Err(e) => return Err(e).context("reading the safety ticket"),
        };
        let mut gate = lock(&self.gate);
        gate.safety_ticket(since, ticket.number);
        if !matches!(ticket.state, TicketState::Resolved | TicketState::Closed) {
            return Ok(false);
        }
        gate.end_wait(since);
        tracing::info!(ticket = ticket.number, state = ?ticket.state,
            "the safety ticket is no longer open: opening tickets again");
        Ok(true)
    }

    /// Whether the problem of `event` gets its ticket now: a [`Slot`] to open it with, or none
    /// when it is held back. While the add-on holds back the problems, `read_safety_ticket` reads
    /// the safety ticket first, since an agent may have resolved or closed it; a problem held
    /// back opens the safety ticket, when the core has none yet.
    pub async fn admit(
        &self,
        event: &str,
        read_safety_ticket: bool,
    ) -> anyhow::Result<Option<Slot<'_>>> {
        self.resume().await?;
        if read_safety_ticket && self.holding() && self.check().await? {
            self.ended.notify_one();
        }
        let tripped =
            match safety::admit(&self.gate, event, Instant::now(), OffsetDateTime::now_utc()) {
                Ok(slot) => return Ok(Some(slot)),
                Err(tripped) => tripped,
            };
        if tripped {
            let limit = self.limit();
            tracing::warn!(event = %event, tickets = limit.tickets, minutes = limit.minutes(),
                "the safety limit is reached: no more tickets until an agent resolves or closes \
                 the safety ticket");
        } else {
            tracing::info!(event = %event,
                "holding back the problems until an agent resolves or closes the safety ticket: \
                 no ticket");
        }
        self.open_safety_ticket().await;
        Ok(None)
    }

    /// Opens the safety ticket of the wait, when the core has none yet: urgent, for the help
    /// desk's own organization. A failure is logged: the problems stay held back all the same,
    /// and the next problem or reconciliation tries again.
    async fn open_safety_ticket(&self) {
        if let Err(e) = self.try_open_safety_ticket().await {
            tracing::error!(
                error = format!("{e:#}"),
                "could not open the safety ticket: trying again at the next problem and round"
            );
        }
    }

    async fn try_open_safety_ticket(&self) -> anyhow::Result<()> {
        let Some(trip) = lock(&self.gate).holding().cloned() else {
            return Ok(());
        };
        if trip.ticket.is_some() {
            return Ok(());
        }
        let customers = self
            .core
            .customers()
            .await
            .context("reading the customers, for the help desk's own organization")?;
        let help_desk = customers.iter().find(|c| c.is_help_desk).context(
            "the core lists no help desk's own organization, which the safety ticket is for",
        )?;
        let ticket = safety::safety_ticket(&self.system, self.limit(), &trip, help_desk.id);
        let opened = self
            .core
            .open_ticket(&ticket)
            .await
            .context("opening the safety ticket")?;
        let mut gate = lock(&self.gate);
        gate.safety_ticket(trip.since, opened.number);
        gate.told(trip.since, trip.held.len());
        drop(gate);
        tracing::warn!(
            ticket = opened.number,
            created = opened.created,
            "the safety ticket is open: an agent resolves or closes it to let the add-on open \
             tickets again"
        );
        Ok(())
    }

    /// Tells the safety ticket how many problems were held back, when more were since it last
    /// said.
    pub async fn tell_held_back(&self) -> anyhow::Result<()> {
        let Some((since, (number, count))) = ({
            let gate = lock(&self.gate);
            gate.holding().map(|t| t.since).zip(gate.untold())
        }) else {
            return Ok(());
        };
        self.core
            .add_note(number, &safety::held_back_note(since, count))
            .await
            .context("telling the safety ticket how many problems were held back")?;
        lock(&self.gate).told(since, count);
        Ok(())
    }
}
