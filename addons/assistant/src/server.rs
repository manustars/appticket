//! The `/events` endpoint and the background work behind it.
//!
//! The core waits at most 10 seconds for an answer and delivers one event at a time, while the
//! model can take longer than that. So the endpoint only checks the event, skips one it has
//! already taken, and queues it; workers do the rest.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use hd_addon_sdk::server::{HandlerError, events_router};
use hd_addon_sdk::{Envelope, Verifier};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use uuid::Uuid;

use crate::assistant::{Assistant, Failure};

/// How many event ids are remembered to skip duplicates.
const REMEMBERED: usize = 10_000;
/// How many events may wait for a worker; beyond that the core is asked to retry later.
const QUEUE: usize = 1_000;

/// Event ids already taken, the oldest forgotten first.
#[derive(Default)]
pub struct Seen {
    ids: HashSet<Uuid>,
    order: VecDeque<Uuid>,
}

impl Seen {
    /// Records `id`; false when it was already there.
    pub fn insert(&mut self, id: Uuid) -> bool {
        if !self.ids.insert(id) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > REMEMBERED
            && let Some(old) = self.order.pop_front()
        {
            self.ids.remove(&old);
        }
        true
    }

    fn forget(&mut self, id: Uuid) {
        if self.ids.remove(&id) {
            self.order.retain(|x| *x != id);
        }
    }
}

/// The `/events` router, feeding `queue` with each verified event the first time it arrives.
pub fn events(verifier: Verifier, queue: mpsc::Sender<Envelope>) -> Router {
    let seen: Arc<Mutex<Seen>> = Arc::default();
    events_router(verifier, move |envelope: Envelope| {
        let seen = seen.clone();
        let queue = queue.clone();
        async move {
            let id = envelope.id;
            if !seen.lock().unwrap().insert(id) {
                tracing::debug!(event = %id, "skipping an event already taken");
                return Ok(());
            }
            if let Err(e) = queue.try_send(envelope) {
                seen.lock().unwrap().forget(id);
                return Err::<(), HandlerError>(format!("the queue is full: {e}").into());
            }
            Ok(())
        }
    })
}

/// A queue for [`events`] and the matching receiver for [`work`].
pub fn queue() -> (mpsc::Sender<Envelope>, mpsc::Receiver<Envelope>) {
    mpsc::channel(QUEUE)
}

/// Works on queued events until the queue closes, `workers` at once and in the order they came;
/// with the model's concurrency as `workers`, events never wait for each other out of order. An
/// event that
/// meets an unavailable core or model is tried again after each of `retry_delays`, without
/// holding a worker while it waits; the core does not deliver it again, since it was already
/// answered.
pub async fn work(
    assistant: Arc<Assistant>,
    mut queue: mpsc::Receiver<Envelope>,
    retry_delays: Vec<Duration>,
    workers: usize,
) {
    let slots = Arc::new(Semaphore::new(workers.max(1)));
    let retry_delays = Arc::new(retry_delays);
    while let Some(envelope) = queue.recv().await {
        let permit = slots.clone().acquire_owned().await.expect("never closed");
        let assistant = assistant.clone();
        let retry_delays = retry_delays.clone();
        let slots = slots.clone();
        tokio::spawn(async move {
            handle(&assistant, &envelope, &retry_delays, &slots, permit).await;
        });
    }
}

/// Handles one event with the worker `permit`, logging the outcome. The permit is given back
/// while waiting to try again, and taken from `slots` again afterwards.
async fn handle(
    assistant: &Assistant,
    envelope: &Envelope,
    retry_delays: &[Duration],
    slots: &Arc<Semaphore>,
    permit: OwnedSemaphorePermit,
) {
    let mut permit = Some(permit);
    let mut delays = retry_delays.iter();
    loop {
        let held = match permit.take() {
            Some(p) => p,
            None => slots.clone().acquire_owned().await.expect("never closed"),
        };
        let outcome = assistant.process(envelope).await;
        drop(held);
        match outcome {
            Ok(made) => {
                let kinds: Vec<_> = made.iter().map(|p| p.content.kind()).collect();
                tracing::info!(event = %envelope.id, kind = %envelope.kind, proposals = ?kinds, "event handled");
                return;
            }
            Err(Failure::Transient(why)) => match delays.next() {
                Some(delay) => {
                    tracing::warn!(event = %envelope.id, %why, ?delay, "trying the event again later");
                    tokio::time::sleep(*delay).await;
                }
                None => {
                    tracing::error!(event = %envelope.id, %why, "giving up on the event");
                    return;
                }
            },
            Err(Failure::Permanent(why)) => {
                tracing::error!(event = %envelope.id, %why, "could not handle the event");
                return;
            }
        }
    }
}
