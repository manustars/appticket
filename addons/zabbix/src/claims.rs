//! The recoveries the add-on handled, or is handling, since it started, so that the webhook and
//! the reconciliation handle each once within a run. A claim given up without being finished,
//! such as when the webhook's deadline drops the handling, leaves no trace: the recovery can be
//! handled again.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// Locks `mutex`, also after a holder panicked: each change under these locks is one step, so the
/// state stays whole.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The recoveries handled or being handled, by the problem's event id.
#[derive(Debug, Default)]
pub struct Claims {
    recoveries: Mutex<HashMap<String, Handling>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handling {
    Underway,
    Done,
}

/// What claiming a recovery found.
#[derive(Debug)]
pub enum Claimed<'a> {
    /// Nobody has handled it: handle it, then [`Claim::finish`].
    Now(Claim<'a>),
    /// Another handling is under way.
    Underway,
    /// It was handled.
    Done,
}

impl Claims {
    /// Claims the recovery of the problem of `event`.
    pub fn claim(&self, event: &str) -> Claimed<'_> {
        let mut recoveries = lock(&self.recoveries);
        match recoveries.get(event) {
            Some(Handling::Underway) => Claimed::Underway,
            Some(Handling::Done) => Claimed::Done,
            None => {
                recoveries.insert(event.to_owned(), Handling::Underway);
                Claimed::Now(Claim {
                    claims: self,
                    event: event.to_owned(),
                    finished: false,
                })
            }
        }
    }
}

/// A recovery being handled; given up when dropped unfinished.
#[derive(Debug)]
pub struct Claim<'a> {
    claims: &'a Claims,
    event: String,
    finished: bool,
}

impl Claim<'_> {
    /// The recovery was handled.
    pub fn finish(mut self) {
        self.finished = true;
        lock(&self.claims.recoveries).insert(self.event.clone(), Handling::Done);
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if !self.finished {
            lock(&self.claims.recoveries).remove(&self.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_recovery_is_handled_once_and_another_handling_waits_for_it() {
        let claims = Claims::default();
        let Claimed::Now(claim) = claims.claim("4213") else {
            panic!("the first claim");
        };
        assert!(matches!(claims.claim("4213"), Claimed::Underway));
        assert!(matches!(claims.claim("4214"), Claimed::Now(_)));
        claim.finish();
        assert!(matches!(claims.claim("4213"), Claimed::Done));
    }

    #[test]
    fn a_claim_given_up_lets_the_recovery_be_handled_again() {
        let claims = Claims::default();
        let Claimed::Now(claim) = claims.claim("4213") else {
            panic!("the first claim");
        };
        drop(claim);
        assert!(matches!(claims.claim("4213"), Claimed::Now(_)));
    }

    #[tokio::test]
    async fn a_handling_dropped_mid_way_gives_its_claim_up() {
        let claims = Claims::default();
        let handling = async {
            let Claimed::Now(claim) = claims.claim("4213") else {
                panic!("the first claim");
            };
            // Zabbix or the core never answers.
            std::future::pending::<()>().await;
            claim.finish();
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(20), handling)
                .await
                .is_err()
        );
        assert!(matches!(claims.claim("4213"), Claimed::Now(_)));
    }
}
