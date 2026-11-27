//! Endpoints for axum (feature `axum`) that check the core's signature before the add-on's
//! handler runs: `POST /events`, which parses each delivery's envelope, and `GET
//! /assets/{reference}`, which answers the live details of an asset. Merge them into one router
//! to serve both.
//!
//! The core waits at most 10 seconds for an event's answer and delivers one event at a time, so a
//! handler that has slow work to do should start it in the background and return at once. It
//! waits 2 seconds for live details, so a handler should answer from what it already knows.

use std::future::Future;

use axum::{
    Json, Router,
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use crate::events::{Envelope, HEADER_SIGNATURE, HEADER_TIMESTAMP};
use crate::live_details::LiveDetails;
use crate::signature::{Verifier, signed_read};

/// What a handler returns when it could not take an event: the core delivers it again later.
/// From a handler of live details: the core shows them as unavailable.
pub type HandlerError = Box<dyn std::error::Error + Send + Sync>;

/// A router with `GET /assets/{reference}`. Each read reaches `handler`, with the reference
/// decoded, only after its signature (over [`signed_read`] of that reference) and timestamp
/// check out. Answers:
///
/// - 200 with the live details the handler gives;
/// - 404 when it gives none: the add-on has no live details of that asset, and agents see none;
/// - 401 when the signature or the timestamp is wrong;
/// - 503 when the handler failed: agents see the details as unavailable.
pub fn live_details_router<H, F>(verifier: Verifier, handler: H) -> Router
where
    H: Fn(String) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Result<Option<LiveDetails>, HandlerError>> + Send + 'static,
{
    Router::new().route(
        "/assets/{reference}",
        get(move |Path(reference): Path<String>, headers: HeaderMap| {
            let verifier = verifier.clone();
            let handler = handler.clone();
            async move {
                let header = |name| {
                    headers
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                };
                if let Err(why) = verifier.verify(
                    header(HEADER_TIMESTAMP),
                    header(HEADER_SIGNATURE),
                    signed_read(&reference).as_bytes(),
                ) {
                    tracing::warn!(%why, "refused a read of live details");
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                let answer: Response = match handler(reference.clone()).await {
                    Ok(Some(details)) => Json(details).into_response(),
                    Ok(None) => StatusCode::NOT_FOUND.into_response(),
                    Err(error) => {
                        tracing::warn!(%reference, error, "could not give live details");
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    }
                };
                answer
            }
        }),
    )
}

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
    use crate::signature::{sign, signed_read};
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

    fn read(reference_path: &str, timestamp: u64, signature: &str) -> Request<Body> {
        Request::get(format!("/assets/{reference_path}"))
            .header(HEADER_TIMESTAMP, timestamp.to_string())
            .header(HEADER_SIGNATURE, signature)
            .body(Body::empty())
            .unwrap()
    }

    /// The core's read reaches the handler with the reference decoded only when it is signed
    /// for that reference; the handler's live details are answered as JSON, none as 404 and a
    /// failure as 503, which the core shows as unavailable.
    #[tokio::test]
    async fn only_signed_reads_reach_the_handler() {
        use crate::live_details::{DetailState, Field, LiveDetails, in_english};
        let verifier = Verifier::new(SECRET).with_clock(|| UNIX_EPOCH + Duration::from_secs(NOW));
        let app = live_details_router(verifier, |reference: String| async move {
            match reference.as_str() {
                "host 10/42" => Ok(Some(LiveDetails {
                    state: DetailState::Ok,
                    fields: vec![Field::new(in_english("Uptime"), "12 days")],
                    links: vec![],
                })),
                "busy" => Err("the monitoring system is down".into()),
                _ => Ok(None),
            }
        });
        let answer = |req: Request<Body>| {
            let app = app.clone();
            async move {
                let response = app.oneshot(req).await.unwrap();
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                (status, body)
            }
        };
        let signed = |reference: &str| sign(SECRET, NOW, signed_read(reference).as_bytes());

        let (status, body) = answer(read("host%2010%2F42", NOW, &signed("host 10/42"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"state": "ok", "fields": [{"label": {"en": "Uptime"},
                "value": "12 days"}], "links": []})
        );
        assert_eq!(
            answer(read("host%2010%2F42", NOW, &signed("host-7")))
                .await
                .0,
            StatusCode::UNAUTHORIZED,
            "signed for another reference"
        );
        assert_eq!(
            answer(read(
                "host%2010%2F42",
                NOW - 600,
                &sign(SECRET, NOW - 600, signed_read("host 10/42").as_bytes())
            ))
            .await
            .0,
            StatusCode::UNAUTHORIZED,
            "stale"
        );
        assert_eq!(
            answer(read("host-7", NOW, &signed("host-7"))).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            answer(read("busy", NOW, &signed("busy"))).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
