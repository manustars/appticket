//! What the mail loops last did, kept in memory for the agents who view the state of the mail.
//! It starts empty when the core starts.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// The last outcome of reading or sending mail.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct Activity {
    /// The server's host and port; never the credentials.
    pub server: String,
    /// Reading: the last read that worked. Sending: the last message sent.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_success_at: Option<OffsetDateTime>,
    /// The last failure since the core started, with any credentials taken out.
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_error_at: Option<OffsetDateTime>,
}

/// Shared between a mail loop, which records, and the API, which reads.
#[derive(Debug, Clone)]
pub struct Tracker {
    activity: Arc<Mutex<Activity>>,
    /// Text that never appears in a recorded error, such as the mailbox's password.
    secrets: Vec<String>,
}

impl Tracker {
    pub fn new(server: String, secrets: Vec<String>) -> Tracker {
        Tracker {
            activity: Arc::new(Mutex::new(Activity {
                server,
                ..Activity::default()
            })),
            secrets: secrets.into_iter().filter(|s| !s.is_empty()).collect(),
        }
    }

    pub fn succeeded(&self) {
        if let Ok(mut activity) = self.activity.lock() {
            activity.last_success_at = Some(OffsetDateTime::now_utc());
        }
    }

    pub fn failed(&self, error: &anyhow::Error) {
        let mut text = format!("{error:#}");
        for secret in &self.secrets {
            text = text.replace(secret.as_str(), "***");
        }
        if let Ok(mut activity) = self.activity.lock() {
            activity.last_error = Some(text);
            activity.last_error_at = Some(OffsetDateTime::now_utc());
        }
    }

    pub fn snapshot(&self) -> Activity {
        self.activity
            .lock()
            .map(|activity| activity.clone())
            .unwrap_or_default()
    }
}

/// The trackers of the loops this core runs; a missing one is not configured.
#[derive(Debug, Clone, Default)]
pub struct MailStatus {
    pub reading: Option<Tracker>,
    pub sending: Option<Tracker>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_recorded_without_secrets() {
        let tracker = Tracker::new(
            "mail.example.com:993".into(),
            vec!["s3cret".into(), String::new()],
        );
        tracker.failed(&anyhow::anyhow!("login as helpdesk with s3cret refused"));
        let activity = tracker.snapshot();
        assert_eq!(
            activity.last_error.as_deref(),
            Some("login as helpdesk with *** refused")
        );
        assert!(activity.last_error_at.is_some() && activity.last_success_at.is_none());
        tracker.succeeded();
        assert!(tracker.snapshot().last_success_at.is_some());
    }
}
