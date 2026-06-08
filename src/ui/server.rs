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
        .route("/api/swarms/{name}/channels/{file}", get(api::get_channel_tail))
        .route("/api/processes", get(api::list_processes))
        .route("/ws/channels/{swarm}/{file}", get(ws::ws_channel))
        .with_state(AppState::new())
}

/// Phase B index — server-rendered HTML listing the registered
/// swarms. Replaced wholesale by the Svelte SPA in Phase F.
async fn index() -> Html<String> {
    let swarms = match crate::registry::load() {
        Ok(r) => r.entries,
        Err(_) => Vec::new(),
    };
    let body = render_index(&swarms);
    Html(body)
}

fn render_index(swarms: &[crate::registry::Entry]) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html>\n");
    out.push_str("<html><head><meta charset=\"utf-8\"><title>giga ui</title>\n");
    out.push_str(
        "<style>\
         body{font-family:system-ui,sans-serif;max-width:960px;margin:2rem auto;padding:0 1rem;color:#222}\
         h1{margin-bottom:.25rem}\
         small{color:#666}\
         table{border-collapse:collapse;width:100%;margin-top:1rem}\
         th,td{padding:.5rem .75rem;border-bottom:1px solid #eee;text-align:left}\
         th{background:#f6f6f6;font-weight:600}\
         a{color:#1a4ed4;text-decoration:none}a:hover{text-decoration:underline}\
         code{background:#f4f4f4;padding:0 .25rem;border-radius:3px}\
         .empty{padding:2rem;background:#fafafa;border:1px dashed #ccc;border-radius:6px;text-align:center;color:#666}\
         </style>",
    );
    out.push_str("</head><body>\n");
    out.push_str("<h1>giga ui</h1>\n");
    out.push_str(&format!(
        "<small>v{} — Phase B (server-rendered placeholder; Svelte frontend lands in Phase F)</small>\n",
        VERSION
    ));
    if swarms.is_empty() {
        out.push_str("<div class=\"empty\">No swarms registered. Run <code>giga setup</code> to create one, or <code>giga init</code> in an existing swarm dir to register it.</div>\n");
    } else {
        out.push_str("<table>\n<thead><tr><th>Swarm</th><th>Config</th><th>JSON</th></tr></thead><tbody>\n");
        for entry in swarms {
            out.push_str(&format!(
                "<tr><td><strong>{}</strong></td><td><code>{}</code></td><td><a href=\"/api/swarms/{}\">/api/swarms/{}</a></td></tr>\n",
                escape_html(&entry.name),
                escape_html(&entry.config.display().to_string()),
                escape_html(&entry.name),
                escape_html(&entry.name),
            ));
        }
        out.push_str("</tbody></table>\n");
    }
    out.push_str("<p><small>API: <a href=\"/api/swarms\">/api/swarms</a> · <a href=\"/api/health\">/api/health</a></small></p>\n");
    out.push_str("</body></html>\n");
    out
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
    async fn index_returns_html_with_giga_ui_heading() {
        let app = build_router();
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("<h1>giga ui</h1>"), "got: {text}");
        // Should link to the JSON API regardless of swarm state.
        assert!(text.contains("/api/swarms"), "expected API link in: {text}");
    }

    #[test]
    fn render_index_renders_empty_state_when_no_swarms() {
        let html = render_index(&[]);
        assert!(html.contains("No swarms registered"), "got: {html}");
    }

    #[test]
    fn render_index_lists_each_swarm_with_link_to_detail() {
        let entries = vec![
            crate::registry::Entry {
                name: "superdeduper".to_string(),
                config: std::path::PathBuf::from("/home/x/.giga/configs/superdeduper/giga-harness.toml"),
                code_roots: vec![],
            },
            crate::registry::Entry {
                name: "gpa".to_string(),
                config: std::path::PathBuf::from("/home/x/.giga/configs/gpa/giga-harness.toml"),
                code_roots: vec![],
            },
        ];
        let html = render_index(&entries);
        assert!(html.contains("superdeduper"));
        assert!(html.contains("gpa"));
        assert!(html.contains("/api/swarms/superdeduper"));
        assert!(html.contains("/api/swarms/gpa"));
    }

    #[test]
    fn escape_html_neutralizes_metacharacters() {
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
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
