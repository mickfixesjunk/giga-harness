//! Shared request/response DTOs + error helpers used across the read
//! and mutate handler modules.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PostError {
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub ok: bool,
}

pub(super) fn not_found(msg: &str) -> (axum::http::StatusCode, axum::Json<PostError>) {
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(PostError {
            error: msg.to_string(),
        }),
    )
}

pub(super) fn internal(msg: &str) -> (axum::http::StatusCode, axum::Json<PostError>) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(PostError {
            error: msg.to_string(),
        }),
    )
}
