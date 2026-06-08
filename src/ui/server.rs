//! axum HTTP server for `giga ui`. Phase A is a placeholder — one
//! page at `/` and a health endpoint at `/api/health` returning the
//! crate version. Phase B layers the swarm/agent REST API on top.

use anyhow::{Context, Result};
use axum::{routing::get, Json, Router};
use serde::Serialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn serve(bind: String, port: u16) -> Result<()> {
    let app = build_router();
    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    println!("    listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .with_context(|| format!("serving on {addr}"))?;
    println!("==> giga ui stopped");
    Ok(())
}

/// Wait for Ctrl-C, then return so axum's `with_graceful_shutdown`
/// can drain in-flight requests and exit cleanly.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\n  ! Ctrl-C received — shutting down");
}

fn build_router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
}

async fn index() -> &'static str {
    "giga ui — Phase A skeleton. The Svelte frontend lands in Phase F."
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok", version: VERSION })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn index_returns_skeleton_message() {
        let app = build_router();
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("Phase A skeleton"), "got: {text}");
    }

    #[tokio::test]
    async fn health_returns_json_with_version() {
        let app = build_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["version"], VERSION);
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let app = build_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
