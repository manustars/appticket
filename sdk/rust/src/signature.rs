//! The signature of an event: `sha256=` and the hex HMAC-SHA-256 of `<hd-timestamp>.<raw body>`,
//! keyed with the event secret the core printed when the add-on was registered. The core signs
//! its reads of live details the same way, over [`signed_read`] in place of a body.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// How far an event's timestamp may be from the add-on's clock, either way.
pub const TOLERANCE: Duration = Duration::from_secs(5 * 60);

const PREFIX: &str = "sha256=";

/// Why an event was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// The `hd-timestamp` header is missing or not a number of seconds.
    BadTimestamp,
    /// The timestamp is more than [`TOLERANCE`] away from the clock: possibly a replay.
    Stale,
    /// The `hd-signature` header is missing, malformed or does not match the body.
    BadSignature,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VerifyError::BadTimestamp => "the event's timestamp is missing or malformed",
            VerifyError::Stale => "the event's timestamp is more than five minutes off",
            VerifyError::BadSignature => "the event's signature does not match",
        })
    }
}

impl std::error::Error for VerifyError {}

fn mac(secret: &str, timestamp: &str, body: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// The `hd-signature` value the core sends for `body` at `timestamp`. Add-ons only need it to
/// test themselves; the core is the one that signs.
pub fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    let tag = mac(secret, &timestamp.to_string(), body)
        .finalize()
        .into_bytes();
    format!("{PREFIX}{}", hex::encode(tag))
}

/// What the core signs, in place of a body, when it asks for the live details of the asset the
/// add-on knows by `reference`: `GET /assets/<reference>`, the reference decoded, as the add-on
/// gave it. Check a read with [`Verifier::verify`] over these bytes.
pub fn signed_read(reference: &str) -> String {
    format!("GET /assets/{reference}")
}

/// Checks an event against its `hd-timestamp` and `hd-signature` headers and its raw body, as
/// seen at `now`. The comparison takes the same time whatever the signature.
pub fn verify_at(
    secret: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
    now: SystemTime,
) -> Result<(), VerifyError> {
    let timestamp = timestamp.trim();
    let sent: u64 = timestamp.parse().map_err(|_| VerifyError::BadTimestamp)?;
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| VerifyError::Stale)?
        .as_secs();
    if now.abs_diff(sent) > TOLERANCE.as_secs() {
        return Err(VerifyError::Stale);
    }
    let tag = signature
        .trim()
        .strip_prefix(PREFIX)
        .and_then(|hex_tag| hex::decode(hex_tag).ok())
        .ok_or(VerifyError::BadSignature)?;
    mac(secret, timestamp, body)
        .verify_slice(&tag)
        .map_err(|_| VerifyError::BadSignature)
}

/// Checks events with one secret and a clock, the system's unless replaced (for tests).
#[derive(Clone)]
pub struct Verifier {
    secret: Arc<str>,
    clock: Arc<dyn Fn() -> SystemTime + Send + Sync>,
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Verifier").finish_non_exhaustive()
    }
}

impl Verifier {
    pub fn new(secret: impl Into<String>) -> Self {
        Verifier {
            secret: secret.into().into(),
            clock: Arc::new(SystemTime::now),
        }
    }

    /// Uses `clock` instead of the system's clock.
    pub fn with_clock(mut self, clock: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    pub fn verify(&self, timestamp: &str, signature: &str, body: &[u8]) -> Result<(), VerifyError> {
        verify_at(&self.secret, timestamp, signature, body, (self.clock)())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "4f0c9e1d2b3a49c8a7d6e5f4c3b2a190e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3";
    const SENT: u64 = 1_795_000_000;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// The core's steps (core/src/addons.rs `signature`), repeated here because the SDK cannot
    /// depend on the core: HMAC-SHA-256 keyed with the secret's bytes, over the decimal
    /// timestamp, a dot and the body, hex-encoded in lowercase after `sha256=`.
    fn core_signature(secret: &str, timestamp: u64, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn a_signature_made_like_the_core_verifies() {
        let body = br#"{"id":"6f1c2c1e-8d2a-4c4f-9a43-0d5b8f6e2a10","kind":"ticket.opened"}"#;
        let signature = core_signature(SECRET, SENT, body);
        assert_eq!(signature, sign(SECRET, SENT, body));
        assert_eq!(signature.len(), 7 + 64);
        assert_eq!(
            verify_at(SECRET, &SENT.to_string(), &signature, body, at(SENT + 3)),
            Ok(())
        );
    }

    #[test]
    fn tampering_is_refused() {
        let body = br#"{"kind":"ticket.opened","data":{"ticket_number":7}}"#;
        let ts = SENT.to_string();
        let good = sign(SECRET, SENT, body);
        let now = at(SENT);
        let refused =
            |secret: &str, ts: &str, sig: &str, body: &[u8]| verify_at(secret, ts, sig, body, now);
        assert_eq!(
            refused(
                SECRET,
                &ts,
                &good,
                br#"{"kind":"ticket.opened","data":{"ticket_number":8}}"#
            ),
            Err(VerifyError::BadSignature),
            "body changed"
        );
        assert_eq!(
            refused(SECRET, &(SENT + 1).to_string(), &good, body),
            Err(VerifyError::BadSignature),
            "timestamp changed"
        );
        assert_eq!(
            refused("another secret", &ts, &good, body),
            Err(VerifyError::BadSignature)
        );
        let mut flipped = good.clone().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] = if flipped[last] == b'0' { b'1' } else { b'0' };
        assert_eq!(
            refused(SECRET, &ts, std::str::from_utf8(&flipped).unwrap(), body),
            Err(VerifyError::BadSignature)
        );
        assert_eq!(
            refused(SECRET, &ts, good.trim_start_matches("sha256="), body),
            Err(VerifyError::BadSignature),
            "prefix missing"
        );
        assert_eq!(
            refused(SECRET, &ts, "sha256=zz", body),
            Err(VerifyError::BadSignature)
        );
        assert_eq!(
            refused(SECRET, &ts, "", body),
            Err(VerifyError::BadSignature)
        );
        assert_eq!(
            refused(SECRET, "soon", &good, body),
            Err(VerifyError::BadTimestamp)
        );
    }

    #[test]
    fn stale_timestamps_are_refused_both_ways() {
        let body = b"{}";
        let ts = SENT.to_string();
        let signature = sign(SECRET, SENT, body);
        let check = |now| verify_at(SECRET, &ts, &signature, body, at(now));
        assert_eq!(check(SENT + 300), Ok(()));
        assert_eq!(check(SENT - 300), Ok(()));
        assert_eq!(check(SENT + 301), Err(VerifyError::Stale), "replayed later");
        assert_eq!(
            check(SENT - 301),
            Err(VerifyError::Stale),
            "from the future"
        );
    }

    #[test]
    fn the_verifier_uses_its_clock() {
        let body = b"{}";
        let signature = sign(SECRET, SENT, body);
        let verifier = Verifier::new(SECRET).with_clock(|| at(SENT + 10));
        assert_eq!(verifier.verify(&SENT.to_string(), &signature, body), Ok(()));
        let late = Verifier::new(SECRET).with_clock(|| at(SENT + 3600));
        assert_eq!(
            late.verify(&SENT.to_string(), &signature, body),
            Err(VerifyError::Stale)
        );
    }
}
