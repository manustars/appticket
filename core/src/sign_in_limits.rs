//! Limits on failed sign-ins (docs/domain-rules.md, "Signing in"): repeated failures for an
//! account or from an address are slowed, then refused for a while.

use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// The limits, with the clock they are measured by.
#[derive(Clone)]
pub struct SignInLimits {
    /// Failures within the window after which attempts are slowed.
    failures: u32,
    window: Duration,
    clock: Clock,
}

/// What became of an attempt.
#[derive(Debug, PartialEq)]
pub enum Guarded<T> {
    /// The check passed, with what it returned.
    Passed(T),
    /// The check failed: a wrong password, or an account nobody has.
    Failed,
    /// Too many failures: the check was not run. Retrying makes sense after `retry_after`.
    Refused { retry_after: Duration },
}

impl SignInLimits {
    pub fn new(failures: u32, window: Duration) -> SignInLimits {
        SignInLimits {
            failures,
            window,
            clock: Clock::system(),
        }
    }

    /// Runs `check`, the password check of a sign-in as `account` from `address`, within the
    /// limits. `check` gives `None` when the password is wrong or the account unknown.
    pub async fn guard<T>(
        &self,
        db: &PgPool,
        account: &str,
        address: IpAddr,
        check: impl Future<Output = anyhow::Result<Option<T>>>,
    ) -> anyhow::Result<Guarded<T>> {
        let account = account_key(account);
        let address = address_key(address);
        let (attempt, delay) = match self.reserve(db, &account, &address).await? {
            Reserved::Refused { retry_after } => return Ok(Guarded::Refused { retry_after }),
            Reserved::Allowed { attempt, delay } => (attempt, delay),
        };
        if !delay.is_zero() {
            self.clock.sleep(delay).await;
        }
        let checked = match check.await {
            Ok(checked) => checked,
            Err(error) => {
                // The password could not be checked: that was no guess.
                sqlx::query("DELETE FROM sign_in_failures WHERE id = $1")
                    .bind(attempt)
                    .execute(db)
                    .await?;
                return Err(error);
            }
        };
        match checked {
            Some(value) => {
                // The account's earlier failures were the person's own: the account forgets
                // them. The address keeps them, as they may be someone else's guesses.
                let mut tx = db.begin().await?;
                sqlx::query("DELETE FROM sign_in_failures WHERE id = $1")
                    .bind(attempt)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE sign_in_failures SET account = NULL WHERE account = $1")
                    .bind(&account)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(Guarded::Passed(value))
            }
            None => Ok(Guarded::Failed),
        }
    }

    /// Records the attempt as a failure, unless it is refused, and says how long it waits first.
    /// Attempts for the same account or from the same address take their turn, so that attempts
    /// sent at once cannot all pass before any is counted.
    async fn reserve(&self, db: &PgPool, account: &str, address: &str) -> anyhow::Result<Reserved> {
        let now = self.clock.now();
        let mut tx = db.begin().await?;
        // A refusal rests on failures up to a window before the last one, itself within the
        // window: older ones no longer matter.
        sqlx::query("DELETE FROM sign_in_failures WHERE failed_at <= $1")
            .bind(now - 2 * self.window_span())
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(1, hashtext($1))")
            .bind(account)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(2, hashtext($1))")
            .bind(address)
            .execute(&mut *tx)
            .await?;
        let by_account: Vec<OffsetDateTime> = sqlx::query_scalar(
            "SELECT failed_at FROM sign_in_failures WHERE account = $1
             ORDER BY failed_at DESC LIMIT $2",
        )
        .bind(account)
        .bind(i64::from(self.most_checked()))
        .fetch_all(&mut *tx)
        .await?;
        let by_address: Vec<OffsetDateTime> = sqlx::query_scalar(
            "SELECT failed_at FROM sign_in_failures WHERE address = $1
             ORDER BY failed_at DESC LIMIT $2",
        )
        .bind(address)
        .bind(i64::from(self.most_checked()))
        .fetch_all(&mut *tx)
        .await?;
        let reserved = match self
            .verdict(&by_account, now)
            .stricter(self.verdict(&by_address, now))
        {
            Verdict::Refused { retry_after } => Reserved::Refused { retry_after },
            Verdict::Allowed { delay } => {
                let attempt = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO sign_in_failures (id, account, address, failed_at)
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(attempt)
                .bind(account)
                .bind(address)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                Reserved::Allowed { attempt, delay }
            }
        };
        tx.commit().await?;
        Ok(reserved)
    }

    /// The most failures within a window: those answered at once, then the slowed ones.
    fn most_checked(&self) -> u32 {
        self.failures + SLOWED_ATTEMPTS
    }

    /// What the failures of one account or address, newest first, allow now.
    fn window_span(&self) -> time::Duration {
        time::Duration::try_from(self.window).unwrap_or(time::Duration::MAX)
    }

    fn verdict(&self, newest_first: &[OffsetDateTime], now: OffsetDateTime) -> Verdict {
        let window = self.window_span();
        let Some(&last) = newest_first.first() else {
            return Verdict::Allowed {
                delay: Duration::ZERO,
            };
        };
        // Once the most failures fall within one window, attempts are refused until a whole
        // window has passed since the last of them; then none of them counts any more.
        let up_to_last = newest_first
            .iter()
            .take_while(|&&at| last - at < window)
            .count() as u32;
        if up_to_last >= self.most_checked() && now - last < window {
            let retry_after = (last + window - now).try_into().unwrap_or(Duration::ZERO);
            return Verdict::Refused { retry_after };
        }
        let in_window = newest_first
            .iter()
            .take_while(|&&at| now - at < window)
            .count() as u32;
        let delay = match in_window.checked_sub(self.failures) {
            Some(past) => FIRST_DELAY * 2u32.pow(past),
            None => Duration::ZERO,
        };
        Verdict::Allowed { delay }
    }
}

/// How many attempts past the limit are slowed before attempts are refused.
const SLOWED_ATTEMPTS: u32 = 4;
/// How long the first slowed attempt waits; each one after waits twice as long as the last.
const FIRST_DELAY: Duration = Duration::from_secs(1);

/// What the failures of an account or an address allow.
enum Verdict {
    Allowed { delay: Duration },
    Refused { retry_after: Duration },
}

impl Verdict {
    /// The stricter of the account's and the address's verdicts.
    fn stricter(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::Refused { retry_after: a }, Verdict::Refused { retry_after: b }) => {
                Verdict::Refused {
                    retry_after: a.max(b),
                }
            }
            (refused @ Verdict::Refused { .. }, _) | (_, refused @ Verdict::Refused { .. }) => {
                refused
            }
            (Verdict::Allowed { delay: a }, Verdict::Allowed { delay: b }) => {
                Verdict::Allowed { delay: a.max(b) }
            }
        }
    }
}

enum Reserved {
    /// The attempt is recorded as `attempt`, and waits `delay` before its password is checked.
    Allowed {
        attempt: Uuid,
        delay: Duration,
    },
    Refused {
        retry_after: Duration,
    },
}

/// The account an attempt counts for: what was typed in, trimmed and in lower case, as agents
/// are looked up by their e-mail.
/// Longer than any e-mail, it is kept as its SHA-256, which fits the table's index.
fn account_key(account: &str) -> String {
    let account = account.trim().to_lowercase();
    if account.len() <= 254 {
        return account;
    }
    use sha2::Digest;
    format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(account.as_bytes()))
    )
}

/// The address an attempt counts for: an IPv4 address (also when mapped into IPv6), or the /64
/// network of an IPv6 address, as one client usually holds a whole /64.
fn address_key(address: IpAddr) -> String {
    match address.to_canonical() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => ipnet::Ipv6Net::new_assert(v6, 64).trunc().to_string(),
    }
}

/// The time limits are measured by, and how a slowed attempt waits.
#[derive(Clone)]
pub struct Clock(ClockKind);

#[derive(Clone)]
enum ClockKind {
    System,
    #[cfg(test)]
    Test(std::sync::Arc<std::sync::Mutex<TestTime>>),
}

#[cfg(test)]
struct TestTime {
    now: time::OffsetDateTime,
    sleeps: Vec<Duration>,
}

impl Clock {
    pub fn system() -> Clock {
        Clock(ClockKind::System)
    }

    fn now(&self) -> OffsetDateTime {
        match &self.0 {
            ClockKind::System => OffsetDateTime::now_utc(),
            #[cfg(test)]
            ClockKind::Test(time) => time.lock().unwrap().now,
        }
    }

    async fn sleep(&self, duration: Duration) {
        match &self.0 {
            ClockKind::System => tokio::time::sleep(duration).await,
            #[cfg(test)]
            ClockKind::Test(time) => {
                let mut time = time.lock().unwrap();
                time.now += duration;
                time.sleeps.push(duration);
            }
        }
    }
}

#[cfg(test)]
impl Clock {
    /// A clock that starts at a fixed time and moves only when told to or when an attempt waits;
    /// waiting takes no real time.
    pub fn test() -> Clock {
        Clock(ClockKind::Test(std::sync::Arc::new(std::sync::Mutex::new(
            TestTime {
                now: time::OffsetDateTime::from_unix_timestamp(1_772_442_000).unwrap(),
                sleeps: Vec::new(),
            },
        ))))
    }

    fn test_time(&self) -> std::sync::MutexGuard<'_, TestTime> {
        match &self.0 {
            ClockKind::Test(time) => time.lock().unwrap(),
            ClockKind::System => panic!("not a test clock"),
        }
    }

    pub fn advance(&self, by: Duration) {
        self.test_time().now += by;
    }

    /// How long each slowed attempt waited, in order.
    pub fn sleeps(&self) -> Vec<Duration> {
        self.test_time().sleeps.clone()
    }
}

#[cfg(test)]
impl SignInLimits {
    /// The limits measured by a test clock.
    pub fn for_tests(failures: u32, window: Duration) -> SignInLimits {
        SignInLimits {
            failures,
            window,
            clock: Clock::test(),
        }
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    fn address(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn secs(list: &[u64]) -> Vec<Duration> {
        list.iter().copied().map(Duration::from_secs).collect()
    }

    /// A sign-in with a wrong password.
    async fn wrong(db: &PgPool, limits: &SignInLimits, account: &str, from: &str) -> Guarded<()> {
        limits
            .guard(db, account, address(from), async { Ok(None) })
            .await
            .unwrap()
    }

    /// A sign-in with the right password.
    async fn right(db: &PgPool, limits: &SignInLimits, account: &str, from: &str) -> Guarded<()> {
        limits
            .guard(db, account, address(from), async { Ok(Some(())) })
            .await
            .unwrap()
    }

    /// Guesses sent all at once cannot pass before the ones ahead of them are counted.
    #[sqlx::test]
    async fn attempts_sent_at_once_check_no_more_passwords_than_the_limits_allow(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        let checked = std::sync::atomic::AtomicUsize::new(0);
        let attempts = (0..20).map(|_| {
            limits.guard(&db, "ada@example.com", address("203.0.113.7"), async {
                checked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(None::<()>)
            })
        });
        let outcomes = futures_util::future::join_all(attempts).await;
        let refused = outcomes
            .iter()
            .filter(|o| matches!(o.as_ref().unwrap(), Guarded::Refused { .. }))
            .count();
        assert_eq!(checked.load(std::sync::atomic::Ordering::SeqCst), 9);
        assert_eq!(refused, 11);
    }

    /// A check that could not be made, such as a directory that does not answer, is no guess.
    #[sqlx::test]
    async fn a_check_that_fails_with_an_error_is_not_counted(db: PgPool) {
        let limits = SignInLimits::for_tests(1, 15 * MINUTE);
        let broken = limits
            .guard(&db, "ada@example.com", address("203.0.113.7"), async {
                Err::<Option<()>, _>(anyhow::anyhow!("the directory does not answer"))
            })
            .await;
        assert!(broken.is_err());
        wrong(&db, &limits, "ada@example.com", "203.0.113.7").await;
        // That was the first failure: only the attempt after it waits.
        assert_eq!(limits.clock().sleeps(), secs(&[]));
    }

    /// A single IPv6 client usually holds a whole /64 network; an IPv4 client may connect over
    /// IPv6 as a mapped address.
    #[sqlx::test]
    async fn ipv6_addresses_count_by_their_64_network(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        for n in 1..=9 {
            let from = format!("2001:db8:7:1::{n:x}");
            wrong(&db, &limits, &format!("guess{n}@example.com"), &from).await;
        }
        assert!(matches!(
            wrong(&db, &limits, "ada@example.com", "2001:db8:7:1:ffff::1").await,
            Guarded::Refused { .. }
        ));
        assert_eq!(
            wrong(&db, &limits, "ada@example.com", "2001:db8:7:2::1").await,
            Guarded::Failed
        );

        for n in 1..=9 {
            wrong(&db, &limits, &format!("try{n}@example.com"), "203.0.113.7").await;
        }
        assert!(matches!(
            wrong(&db, &limits, "bo@example.com", "::ffff:203.0.113.7").await,
            Guarded::Refused { .. }
        ));
    }

    /// Whatever is typed in as the e-mail, however long, is counted like any other account.
    #[sqlx::test]
    async fn an_overlong_account_is_counted_too(db: PgPool) {
        let limits = SignInLimits::for_tests(1, 15 * MINUTE);
        // Text that does not compress, as a repeated one would.
        let mut seed: u32 = 27;
        let noise: String = (0..9000)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                char::from(b'a' + ((seed >> 16) % 26) as u8)
            })
            .collect();
        let long = format!("{noise}@Example.com");
        for n in 1..=5 {
            let from = format!("203.0.113.{n}");
            assert_eq!(wrong(&db, &limits, &long, &from).await, Guarded::Failed);
        }
        assert!(matches!(
            wrong(&db, &limits, &long.to_lowercase(), "198.51.100.4").await,
            Guarded::Refused { .. }
        ));
    }

    #[sqlx::test]
    async fn failures_older_than_the_window_are_deleted(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        wrong(&db, &limits, "ada@example.com", "203.0.113.7").await;
        wrong(&db, &limits, "bo@example.com", "198.51.100.4").await;
        // A refusal can rest on failures up to a window before the last one: they are kept for
        // two windows.
        limits.clock().advance(29 * MINUTE);
        wrong(&db, &limits, "cy@example.com", "192.0.2.9").await;
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM sign_in_failures")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(kept, 3);
        limits.clock().advance(MINUTE);
        wrong(&db, &limits, "cy@example.com", "192.0.2.9").await;
        let kept: Vec<String> = sqlx::query_scalar("SELECT account FROM sign_in_failures")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(kept, ["cy@example.com", "cy@example.com"]);
    }

    /// Signing in proves the person knows the account's password, not that nobody at their
    /// address is guessing at others.
    #[sqlx::test]
    async fn signing_in_clears_the_accounts_failures_but_not_the_addresss(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        for _ in 0..4 {
            wrong(&db, &limits, "ada@example.com", "203.0.113.7").await;
        }
        assert_eq!(
            right(&db, &limits, "ada@example.com", "203.0.113.7").await,
            Guarded::Passed(())
        );
        for _ in 0..5 {
            wrong(&db, &limits, "ada@example.com", "198.51.100.4").await;
        }
        assert_eq!(limits.clock().sleeps(), secs(&[]));
        // The address still has its four failures: one more, and the next attempt waits.
        wrong(&db, &limits, "bo@example.com", "203.0.113.7").await;
        wrong(&db, &limits, "bo@example.com", "203.0.113.7").await;
        assert_eq!(limits.clock().sleeps(), secs(&[1]));
    }

    #[sqlx::test]
    async fn failures_past_the_limit_are_slowed_then_refused(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        for _ in 0..9 {
            assert_eq!(
                wrong(&db, &limits, "ada@example.com", "203.0.113.7").await,
                Guarded::Failed
            );
        }
        // The first five are answered at once; each one after waits twice as long as the last.
        assert_eq!(limits.clock().sleeps(), secs(&[1, 2, 4, 8]));
        assert!(matches!(
            wrong(&db, &limits, "ada@example.com", "203.0.113.7").await,
            Guarded::Refused { .. }
        ));
    }

    /// Nine failures, the most within a window with the default limit.
    async fn nine_failures(db: &PgPool, limits: &SignInLimits, account: &str, from: &str) {
        for _ in 0..9 {
            assert_eq!(wrong(db, limits, account, from).await, Guarded::Failed);
        }
    }

    #[sqlx::test]
    async fn a_refused_attempt_does_not_check_the_password(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        nine_failures(&db, &limits, "ada@example.com", "203.0.113.7").await;
        let checked = std::sync::atomic::AtomicBool::new(false);
        let right = limits
            .guard(&db, "ada@example.com", address("203.0.113.7"), async {
                checked.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some("session"))
            })
            .await
            .unwrap();
        // The last failure waited 8 seconds after it was counted; a window from it is left.
        assert_eq!(
            right,
            Guarded::Refused {
                retry_after: Duration::from_secs(15 * 60 - 8)
            }
        );
        assert!(!checked.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[sqlx::test]
    async fn attempts_are_refused_until_a_window_has_passed_since_the_last_failure(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        for _ in 0..5 {
            wrong(&db, &limits, "ada@example.com", "203.0.113.7").await;
        }
        limits.clock().advance(10 * MINUTE);
        for _ in 0..4 {
            wrong(&db, &limits, "ada@example.com", "203.0.113.7").await;
        }
        // The first five failures leave the window, the last four keep it refused.
        limits.clock().advance(6 * MINUTE);
        assert!(matches!(
            wrong(&db, &limits, "ada@example.com", "203.0.113.7").await,
            Guarded::Refused { .. }
        ));
        limits.clock().advance(9 * MINUTE);
        assert_eq!(
            wrong(&db, &limits, "ada@example.com", "203.0.113.7").await,
            Guarded::Failed
        );
        // Nothing counts any more: that failure was answered at once.
        assert_eq!(limits.clock().sleeps(), secs(&[1, 2, 4, 8]));
    }

    /// Accounts are counted whether or not an agent has them, whatever the case and the spaces
    /// around them.
    #[sqlx::test]
    async fn failures_count_for_the_account_from_any_address(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        for n in 0..9 {
            let account = if n % 2 == 0 {
                " Ada@Example.com"
            } else {
                "ada@example.com"
            };
            let from = format!("203.0.113.{}", n + 1);
            assert_eq!(wrong(&db, &limits, account, &from).await, Guarded::Failed);
        }
        assert!(matches!(
            wrong(&db, &limits, "ADA@example.com ", "198.51.100.4").await,
            Guarded::Refused { .. }
        ));
        assert_eq!(
            wrong(&db, &limits, "bo@example.com", "198.51.100.4").await,
            Guarded::Failed
        );
    }

    #[sqlx::test]
    async fn failures_count_for_the_address_for_any_account(db: PgPool) {
        let limits = SignInLimits::for_tests(5, 15 * MINUTE);
        for n in 0..9 {
            let account = format!("guess{n}@example.com");
            assert_eq!(
                wrong(&db, &limits, &account, "203.0.113.7").await,
                Guarded::Failed
            );
        }
        assert!(matches!(
            wrong(&db, &limits, "ada@example.com", "203.0.113.7").await,
            Guarded::Refused { .. }
        ));
        assert_eq!(
            wrong(&db, &limits, "ada@example.com", "198.51.100.4").await,
            Guarded::Failed
        );
    }
}
