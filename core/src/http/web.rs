//! The agents' UI, built by `npm run build` into `web/dist`, served next to the API when
//! `HD_WEB_DIR` points at it. The UI routes with `#`, so the server only needs its files, with
//! `index.html` for any other path.

use std::convert::Infallible;
use std::path::Path;

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use tower::{ServiceExt, service_fn};
use tower_http::services::{ServeDir, ServeFile};

use super::ApiError;

/// Build assets carry a content hash in their name: they never change under the same name.
const LONG_CACHE: &str = "public, max-age=31536000, immutable";
/// Everything else, `index.html` first, is checked again on every load.
const NO_CACHE: &str = "no-cache";

fn with_cache(response: Response, value: &'static str) -> Response {
    let mut response = response;
    if response.status().is_success() {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }
    response
}

/// `api` plus the UI built into `dir`: hashed files under `/assets/` (404 when missing), other
/// files of `dir`, and `index.html` for any other path. Unknown `/api` paths stay JSON errors.
pub fn with_web(api: Router, dir: &Path) -> Router {
    let hashed = ServeDir::new(dir.join("assets"));
    let assets = service_fn(move |request: Request| {
        let hashed = hashed.clone();
        async move {
            let response = hashed.oneshot(request).await?;
            Ok::<_, Infallible>(with_cache(response.map(Body::new), LONG_CACHE))
        }
    });
    let files = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
    let fallback = service_fn(move |request: Request| {
        let files = files.clone();
        async move {
            let path = request.uri().path();
            if path == "/api" || path.starts_with("/api/") {
                return Ok::<_, Infallible>(ApiError::NotFound("API route").into_response());
            }
            let response = files.oneshot(request).await?;
            Ok(with_cache(response.map(Body::new), NO_CACHE))
        }
    });
    api.nest_service("/assets", assets)
        .fallback_service(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{AppState, router};
    use axum::http::StatusCode;
    use http_body_util::BodyExt;
    use sqlx::PgPool;

    /// A built UI in a fresh directory: `index.html` and one hashed asset.
    fn built_ui() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hd-web-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><title>Help desk</title>",
        )
        .unwrap();
        std::fs::write(dir.join("assets/index-3f2a9c.js"), "console.log('ui')").unwrap();
        dir
    }

    async fn get(app: &Router, path: &str) -> (StatusCode, Option<String>, String) {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let cache = response
            .headers()
            .get(header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap().to_owned());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, cache, String::from_utf8_lossy(&body).into_owned())
    }

    #[sqlx::test]
    async fn the_ui_is_served_next_to_the_api(db: PgPool) {
        let dir = built_ui();
        let app = with_web(
            router(AppState {
                sender: None,
                ..AppState::for_tests(&db)
            }),
            &dir,
        );

        let (status, cache, body) = get(&app, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache.as_deref(), Some(NO_CACHE));
        assert!(body.contains("<title>Help desk</title>"));

        let (status, cache, body) = get(&app, "/assets/index-3f2a9c.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache.as_deref(), Some(LONG_CACHE));
        assert_eq!(body, "console.log('ui')");

        // A path the UI does not have still opens the UI.
        let (status, cache, body) = get(&app, "/tickets/12").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache.as_deref(), Some(NO_CACHE));
        assert!(body.contains("<title>Help desk</title>"));

        // A missing asset is missing, and nothing marks the 404 as cacheable forever.
        let (status, cache, _) = get(&app, "/assets/index-000000.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(cache, None);

        // The API keeps answering for itself, unknown paths included.
        let (status, _, body) = get(&app, "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\""));
        let (status, _, body) = get(&app, "/api/no-such-thing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, r#"{"error":"API route not found."}"#);
        let (status, _, _) = get(&app, "/api/openapi.json").await;
        assert_eq!(status, StatusCode::OK);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
