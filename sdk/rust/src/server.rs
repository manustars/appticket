//! A `/events` endpoint for axum (feature `axum`): it checks each delivery's signature over the
//! raw body, parses the envelope and hands it to the add-on's handler.
//!
//! The core waits at most 10 seconds for an answer and delivers one event at a time, so a
//! handler that has slow work to do should start it in the background and return at once.

use std::future::Future;

use axum::{
    Router,
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
};

use crate::events::{Envelope, HEADER_SIGNATURE, HEADER_TIMESTAMP};
use crate::signature::Verifier;

/// What a handler returns when it could not take an event: the core delivers it again later.
pub type HandlerError = Box<dyn std::error::Error + Send + Sync>;

/// A router with `POST /events`. Each event reaches `handler` only after its signature and
/// timestamp check out, so its body can be trusted. Answers:
///
/// - 204 when the handler took the event;
/// - 401 when the signature or the timestamp is wrong;
/// - 400 when the body is not an envelope;
/// - 503 when the handler failed, so that the core retries.
pub fn events_router<H, F>(verifier: Verifier, handler: H) -> Router
where
    H: Fn(Envelope) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Result<(), HandlerError>> + Send + 'static,
{
    Router::new().route(
        "/events",
        post(move |headers: HeaderMap, body: Bytes| {
            let verifier = verifier.clone();
            let handler = handler.clone();
            async move {
                let header = |name| {
                    headers
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                };
                if let Err(why) =
                    verifier.verify(header(HEADER_TIMESTAMP), header(HEADER_SIGNATURE), &body)
                {
                    tracing::warn!(%why, "refused an event");
                    return StatusCode::UNAUTHORIZED;
                }
                let envelope: Envelope = match serde_json::from_slice(&body) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        tracing::warn!(%error, "refused an event that is not an envelope");
                        return StatusCode::BAD_REQUEST;
                    }
                };
                let id = envelope.id;
                match handler(envelope).await {
                    Ok(()) => StatusCode::NO_CONTENT,
                    Err(error) => {
                        tracing::warn!(event = %id, error, "could not take an event");
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::sign;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};
    use tower::ServiceExt;

    const SECRET: &str = "a secret shared with the core";
    const NOW: u64 = 1_795_000_000;

    fn request(timestamp: u64, signature: &str, body: &'static str) -> Request<Body> {
        Request::post("/events")
            .header("content-type", "application/json")
            .header(HEADER_TIMESTAMP, timestamp.to_string())
            .header(HEADER_SIGNATURE, signature)
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn only_verified_envelopes_reach_the_handler() {
        let seen: Arc<Mutex<Vec<Envelope>>> = Arc::default();
        let verifier = Verifier::new(SECRET).with_clock(|| UNIX_EPOCH + Duration::from_secs(NOW));
        let app = events_router(verifier, {
            let seen = seen.clone();
            move |envelope| {
                let seen = seen.clone();
                async move {
                    if envelope.kind == "fail" {
                        return Err("busy".into());
                    }
                    seen.lock().unwrap().push(envelope);
                    Ok(())
                }
            }
        });
        let body = r#"{"id":"6f1c2c1e-8d2a-4c4f-9a43-0d5b8f6e2a10","kind":"ticket.opened","created_at":"2026-11-21T09:00:00Z","data":{"ticket_number":3}}"#;
        let status = |req: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(req).await.unwrap().status() }
        };

        assert_eq!(
            status(request(NOW, &sign(SECRET, NOW, body.as_bytes()), body)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(
            status(request(NOW, &sign("wrong", NOW, body.as_bytes()), body)).await,
            StatusCode::UNAUTHORIZED
        );
        let old = NOW - 600;
        assert_eq!(
            status(request(old, &sign(SECRET, old, body.as_bytes()), body)).await,
            StatusCode::UNAUTHORIZED
        );
        let junk = "not json";
        assert_eq!(
            status(request(NOW, &sign(SECRET, NOW, junk.as_bytes()), junk)).await,
            StatusCode::BAD_REQUEST
        );
        let failing = r#"{"id":"6f1c2c1e-8d2a-4c4f-9a43-0d5b8f6e2a11","kind":"fail","created_at":"2026-11-21T09:00:00Z"}"#;
        assert_eq!(
            status(request(
                NOW,
                &sign(SECRET, NOW, failing.as_bytes()),
                failing
            ))
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
