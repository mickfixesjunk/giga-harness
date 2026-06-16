//! axum HTTP server for `giga ui`. Phase A is a placeholder — one
//! page at `/` and a health endpoint at `/api/health` returning the
//! crate version. Phase B layers the swarm/agent REST API on top.

use crate::ui::api;
use crate::ui::state::AppState;
use crate::ui::ws;
use anyhow::{Context, Result};
use axum::{response::Html, routing::get, Json, Router};
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
        .route("/api/swarms", get(api::list_swarms))
        .route("/api/swarms/{name}", get(api::get_swarm))
        .route(
            "/api/swarms/{name}/channels/{file}",
            get(api::get_channel_tail).post(api::post_to_channel),
        )
        .route("/api/swarms/{name}/timeline", get(api::get_swarm_timeline))
        .route(
            "/api/swarms/{name}/archive",
            axum::routing::post(api::set_swarm_archived),
        )
        .route("/api/processes", get(api::list_processes))
        .route("/assets/giga-icon.png", get(serve_icon))
        .route(
            "/api/swarms/{name}/validate",
            axum::routing::post(api::validate_swarm),
        )
        .route(
            "/api/swarms/{name}/launch",
            axum::routing::post(api::launch_swarm),
        )
        .route(
            "/api/swarms/{name}/kill",
            axum::routing::post(api::kill_swarm),
        )
        .route(
            "/api/swarms/{name}/agents",
            axum::routing::post(api::add_agent),
        )
        .route(
            "/api/swarms/{name}/channels",
            axum::routing::post(api::add_channel),
        )
        .route(
            "/api/swarms/{swarm}/agents/{agent}/log",
            get(api::get_agent_log),
        )
        .route("/api/upgrade", axum::routing::post(api::run_upgrade))
        .route("/ws/channels/{swarm}/{file}", get(ws::ws_channel))
        .with_state(AppState::new())
}

/// v0.6.37 Phase F: the dashboard HTML is a single self-contained
/// page (HTML + CSS + vanilla JS) at templates/ui/dashboard.html,
/// embedded via include_str! so the binary still ships as one
/// artifact. The page does its own fetching against the JSON
/// API + WebSocket and renders client-side. UI_DESIGN.md originally
/// scoped a Svelte SPA — pivoted to no-build vanilla JS for the
/// initial ship so end users don't need Node to build giga.
/// Reasonable to revisit if/when interactions get more complex.
const DASHBOARD_HTML: &str = include_str!("../../templates/ui/dashboard.html");

/// v0.6.49: brand icon shown in the header. ~47KB, 256x171 PNG.
/// Embedded via include_bytes! so no separate asset shipping
/// is required.
const ICON_PNG: &[u8] = include_bytes!("../../assets/giga-icon.png");

async fn serve_icon() -> impl axum::response::IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "image/png")], ICON_PNG)
}

async fn index() -> Html<String> {
    // Inject the running version into the page header so the user
    // can confirm at a glance which binary they're hitting.
    let body = DASHBOARD_HTML.replace("__VERSION__", VERSION);
    Html(body)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: VERSION,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn index_returns_dashboard_html() {
        let app = build_router();
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        // Embedded dashboard markers.
        assert!(text.contains("<title>giga ui</title>"), "missing title");
        assert!(
            !text.contains("__VERSION__"),
            "version placeholder should be substituted"
        );
        assert!(
            text.contains(VERSION),
            "dashboard should embed the running version"
        );
        // The dashboard wires onto the JSON API.
        assert!(
            text.contains("/api/swarms"),
            "dashboard should reference /api/swarms"
        );
        assert!(
            text.contains("/ws/channels/"),
            "dashboard should reference the WS endpoint"
        );
    }

    #[test]
    fn dashboard_html_contains_no_unreplaced_version_placeholder_when_built() {
        // The constant DASHBOARD_HTML itself still has __VERSION__;
        // it's substituted at request time. Sanity-check the
        // placeholder is present in the template so the substitution
        // has something to act on.
        assert!(
            DASHBOARD_HTML.contains("__VERSION__"),
            "template lost its version placeholder"
        );
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
            .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
