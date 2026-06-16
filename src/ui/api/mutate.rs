//! Mutating (POST) handlers for the dashboard + their request DTOs.
//! These shell out to the same `giga` binary via [`run_giga`] (or, for
//! kill/post, hit tmux / the post machinery directly).

use crate::config::Config;
use crate::registry;
use axum::extract::{Path as AxumPath, Query};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::dto::{internal, not_found, ExecResult, PostError};

#[derive(Debug, Deserialize)]
pub struct ArchiveBody {
    pub archived: bool,
}

#[derive(Debug, Serialize)]
pub struct ArchiveResult {
    pub swarm: String,
    pub archived: bool,
    /// True when the flag flipped; false when it was already in
    /// the requested state.
    pub changed: bool,
}

/// `POST /api/swarms/:name/archive` with body `{archived: bool}` —
/// flips the registry entry's archived flag. The configs and
/// channel files stay on disk untouched; only the UI's default
/// filtering changes. Unarchive by passing `{archived: false}`.
pub async fn set_swarm_archived(
    AxumPath(name): AxumPath<String>,
    Json(body): Json<ArchiveBody>,
) -> Result<Json<ArchiveResult>, (axum::http::StatusCode, Json<PostError>)> {
    match registry::set_archived(&name, body.archived) {
        Ok(changed) => Ok(Json(ArchiveResult {
            swarm: name,
            archived: body.archived,
            changed,
        })),
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("is not registered") {
                Err(not_found(&msg))
            } else {
                Err(internal(&msg))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PostBody {
    pub r#as: String,
    pub subject: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub waiting_on: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PostResponse {
    pub swarm: String,
    pub file: String,
    pub posted_as: String,
}

/// `POST /api/swarms/:name/channels/:file` — append a post to a
/// channel via the same machinery `giga post` uses. Validates
/// channel + sender against the swarm config; the underlying
/// `post::run` enforces participant rules + slice routing.
///
/// v0.6.40 v2 ship gate: no auth in v1, so any tailnet client can
/// post. Acceptable when the server is localhost-only (the v1
/// default); operators who --bind 0.0.0.0 take responsibility for
/// the tailnet trust model.
pub async fn post_to_channel(
    AxumPath((name, file)): AxumPath<(String, String)>,
    Json(body): Json<PostBody>,
) -> Result<Json<PostResponse>, (axum::http::StatusCode, Json<PostError>)> {
    let reg = registry::load().map_err(|e| internal(&format!("registry load: {e:#}")))?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| not_found("swarm not found"))?;
    // post::run validates the channel + participant itself; we still
    // pre-check that the channel is declared in this swarm so
    // arbitrary file appends are blocked even before post::run is
    // reached.
    let cfg = Config::load(&entry.config).map_err(|e| internal(&format!("config load: {e:#}")))?;
    let _ = cfg
        .channels
        .iter()
        .find(|c| c.file == file)
        .ok_or_else(|| not_found("channel not in swarm config"))?;

    // Build post::Args and delegate to the canonical implementation.
    // Sync I/O inside an async handler is fine here — the append is
    // a few syscalls; not worth a spawn_blocking dance.
    let args = crate::coordination::post::Args {
        channel: file.clone(),
        me: body.r#as.clone(),
        subject: body.subject.clone(),
        body: body.body.clone(),
        waiting_on: body.waiting_on.clone(),
        needs: None,
        config: entry.config.clone(),
        to: Vec::new(),
        fyi: false,
    };
    crate::coordination::post::run(args).map_err(|e| {
        let msg = format!("{e:#}");
        // Participant/waiting-on validation errors are the user's
        // fault, not 500s — surface as 400 with the original error.
        if msg.contains("is not a participant") || msg.contains("WAITING ON target") {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(PostError { error: msg }),
            )
        } else {
            internal(&msg)
        }
    })?;

    Ok(Json(PostResponse {
        swarm: name,
        file,
        posted_as: body.r#as,
    }))
}

/// `POST /api/swarms/:name/validate` — shell out to `giga validate
/// --config <swarm.toml>` and return its stdout/stderr/exit-code.
/// Read-only operation (validate doesn't mutate anything); kept as
/// POST so the frontend keeps cache controllers from caching the
/// trigger.
pub async fn validate_swarm(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<ExecResult>, (axum::http::StatusCode, Json<PostError>)> {
    let reg = registry::load().map_err(|e| internal(&format!("registry load: {e:#}")))?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| not_found("swarm not found"))?;
    // `giga validate` takes CONFIG as a positional argument, not
    // `--config` (unlike `giga post` / `giga launch`). Easy gotcha
    // caught during the smoke test.
    let config_str = entry.config.display().to_string();
    let out = run_giga(&["validate", &config_str])
        .map_err(|e| internal(&format!("spawn giga validate: {e:#}")))?;
    Ok(Json(out))
}

#[derive(Debug, Deserialize, Default)]
pub struct UpgradeQuery {
    /// When true, runs `giga upgrade --dry-run` so the operator can
    /// preview without actually installing.
    #[serde(default)]
    pub dry_run: bool,
}

/// `POST /api/upgrade` — shell out to `giga upgrade --bare`
/// (system-level binary install only; no swarm-aware disarm/rearm
/// dance). Pass `?dry_run=true` to preview without installing.
///
/// This intentionally runs `--bare`; the dance for Windows
/// peer-host watchers is deferred to v2.1 once the multi-swarm
/// iteration story is settled.
pub async fn run_upgrade(
    Query(q): Query<UpgradeQuery>,
) -> Result<Json<ExecResult>, (axum::http::StatusCode, Json<PostError>)> {
    let mut args = vec!["upgrade", "--bare"];
    if q.dry_run {
        args.push("--dry-run");
    }
    let out = run_giga(&args).map_err(|e| internal(&format!("spawn giga upgrade: {e:#}")))?;
    Ok(Json(out))
}

/// Invokes the same `giga` binary that's currently running (so the
/// child sees the same dependencies + behavior). Captures stdout +
/// stderr; never inherits them.
fn run_giga(argv: &[&str]) -> Result<ExecResult, std::io::Error> {
    let exe = crate::foundation::self_invoke::giga_binary();
    let out = std::process::Command::new(&exe).args(argv).output()?;
    Ok(ExecResult {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        ok: out.status.success(),
    })
}

#[derive(Debug, Deserialize, Default)]
pub struct LaunchQuery {
    /// Stagger between agent spawns in seconds. Default 0 (no
    /// stagger); pass 5-15 for 10+ agent swarms to avoid the TPM
    /// limit storm.
    #[serde(default)]
    pub stagger: u64,
    /// When true, also re-run `giga init` before launching. Default
    /// false — re-init can overwrite local AGENTS.md edits.
    #[serde(default)]
    pub init: bool,
}

/// `POST /api/swarms/:name/launch` — shells out to `giga launch`
/// for this swarm. Defaults to --skip-init + --terminal tmux to
/// keep the call deterministic; pass `?init=true` to re-render
/// AGENTS.md before launching. Synchronous; the request returns
/// when `giga launch` exits (can take a while for staggered
/// launches).
pub async fn launch_swarm(
    AxumPath(name): AxumPath<String>,
    Query(q): Query<LaunchQuery>,
) -> Result<Json<ExecResult>, (axum::http::StatusCode, Json<PostError>)> {
    let reg = registry::load().map_err(|e| internal(&format!("registry load: {e:#}")))?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| not_found("swarm not found"))?;
    let config_str = entry.config.display().to_string();
    let stagger_str = q.stagger.to_string();
    let mut argv: Vec<&str> = vec!["launch", "--config", &config_str, "--terminal", "tmux"];
    if !q.init {
        argv.push("--skip-init");
    }
    if q.stagger > 0 {
        argv.push("--stagger-per-agent-seconds");
        argv.push(&stagger_str);
    }
    let out = run_giga(&argv).map_err(|e| internal(&format!("spawn giga launch: {e:#}")))?;
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct AddAgentBody {
    pub name: String,
    pub workdir: String,
    pub role: String,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub peers: Vec<String>,
    #[serde(default)]
    pub bench_scheduler: bool,
    #[serde(default)]
    pub swarm_boss: bool,
    #[serde(default)]
    pub no_broadcast: bool,
    #[serde(default)]
    pub code_root: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// `POST /api/swarms/:name/agents` — shells out to
/// `giga add-agent` with the body's fields mapped to flags.
/// Returns the underlying command output. Dry-run mode lets the
/// operator preview the TOML / channel-file changes before
/// committing.
pub async fn add_agent(
    AxumPath(name): AxumPath<String>,
    Json(body): Json<AddAgentBody>,
) -> Result<Json<ExecResult>, (axum::http::StatusCode, Json<PostError>)> {
    let reg = registry::load().map_err(|e| internal(&format!("registry load: {e:#}")))?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| not_found("swarm not found"))?;
    let config_str = entry.config.display().to_string();
    // Validate slug shape early — giga's own check is later in the
    // pipeline but we'd rather not spawn a subprocess for an
    // obviously-bad name.
    if body.name.is_empty()
        || !body
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(PostError {
                error: "agent name must be a non-empty [a-zA-Z0-9_-]+ slug".to_string(),
            }),
        ));
    }
    let mut argv: Vec<String> = vec![
        "add-agent".into(),
        "--config".into(),
        config_str,
        "--name".into(),
        body.name.clone(),
        "--workdir".into(),
        body.workdir.clone(),
        "--role".into(),
        body.role.clone(),
    ];
    if let Some(p) = body.platform.as_deref() {
        argv.push("--platform".into());
        argv.push(p.to_string());
    }
    if let Some(h) = body.host.as_deref() {
        argv.push("--host".into());
        argv.push(h.to_string());
    }
    if let Some(cr) = body.code_root.as_deref() {
        argv.push("--code-root".into());
        argv.push(cr.to_string());
    }
    for peer in &body.peers {
        argv.push("--peer".into());
        argv.push(peer.clone());
    }
    if body.bench_scheduler {
        argv.push("--bench-scheduler".into());
    }
    if body.swarm_boss {
        argv.push("--swarm-boss".into());
    }
    if body.no_broadcast {
        argv.push("--no-broadcast".into());
    }
    if body.dry_run {
        argv.push("--dry-run".into());
    }
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out =
        run_giga(&argv_refs).map_err(|e| internal(&format!("spawn giga add-agent: {e:#}")))?;
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct AddChannelBody {
    /// Bilateral channel — must be exactly two participants per
    /// add-channel's v1 contract.
    pub participants: Vec<String>,
    /// Override the auto-derived `<a>-<b>.md` filename. Rarely
    /// needed; lets the operator name a channel something other
    /// than its participants (e.g. `_broadcast.md`).
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// `POST /api/swarms/:name/channels` — shells out to
/// `giga add-channel --participants a,b` for the swarm.
pub async fn add_channel(
    AxumPath(name): AxumPath<String>,
    Json(body): Json<AddChannelBody>,
) -> Result<Json<ExecResult>, (axum::http::StatusCode, Json<PostError>)> {
    let reg = registry::load().map_err(|e| internal(&format!("registry load: {e:#}")))?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| not_found("swarm not found"))?;
    if body.participants.len() != 2 {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(PostError {
                error: "bilateral channels only — pass exactly two participants".to_string(),
            }),
        ));
    }
    let config_str = entry.config.display().to_string();
    let participants_str = body.participants.join(",");
    let mut argv = vec![
        "add-channel",
        "--config",
        &config_str,
        "--participants",
        &participants_str,
    ];
    if let Some(f) = body.file.as_deref() {
        argv.push("--file");
        argv.push(f);
    }
    if body.dry_run {
        argv.push("--dry-run");
    }
    let out = run_giga(&argv).map_err(|e| internal(&format!("spawn giga add-channel: {e:#}")))?;
    Ok(Json(out))
}

/// `POST /api/swarms/:name/kill` — shells out to
/// `tmux kill-session -t giga-<swarm>`. Returns the tmux exit
/// code so the operator can tell whether the session existed.
/// No-op (exit 1) when the session is already gone.
pub async fn kill_swarm(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<ExecResult>, (axum::http::StatusCode, Json<PostError>)> {
    // Verify the swarm exists in the registry before touching tmux,
    // so a typo in `name` doesn't accidentally hit a similarly-named
    // session belonging to another tool.
    let reg = registry::load().map_err(|e| internal(&format!("registry load: {e:#}")))?;
    let _entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| not_found("swarm not found"))?;
    let session = format!("giga-{name}");
    let out = std::process::Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .output()
        .map_err(|e| internal(&format!("spawn tmux: {e}")))?;
    Ok(Json(ExecResult {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        ok: out.status.success(),
    }))
}
