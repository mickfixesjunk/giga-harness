//! REST handlers for the dashboard.
//!
//! Phase B (v0.6.32):
//!   * `GET /api/swarms` — list every registered swarm with a
//!     summary (agent count, channel count, last activity).
//!   * `GET /api/swarms/:name` — full detail (agents + channels).
//!
//! Stateless: each request reloads `~/.giga/swarms.toml` and the
//! per-swarm `giga-harness.toml`. Caching is a future optimization.

use crate::config::{Agent, Channel, Config};
use crate::registry;
use crate::ui::channel as post_parser;
use crate::ui::process;
use axum::extract::{Path as AxumPath, Query};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct SwarmSummary {
    pub name: String,
    pub config_path: PathBuf,
    pub agent_count: usize,
    pub channel_count: usize,
    /// RFC3339 mtime of the most-recently-modified channel file in
    /// this swarm's inbox. `None` when nothing matches or the
    /// inbox dir is missing.
    pub last_activity_iso: Option<String>,
    /// When set, the swarm's config could not be loaded; agent and
    /// channel counts are zero and the operator can use this to
    /// diagnose. The other fields fall back to defaults.
    pub load_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SwarmDetail {
    pub name: String,
    pub config_path: PathBuf,
    pub description: Option<String>,
    pub project_runtime: Option<String>,
    pub launch_model: String,
    pub agents: Vec<AgentDto>,
    pub channels: Vec<ChannelDto>,
}

#[derive(Debug, Serialize)]
pub struct AgentDto {
    pub name: String,
    /// Effective runtime — agent override > project default.
    pub runtime: String,
    pub workdir: PathBuf,
    pub code_root: Option<PathBuf>,
    pub host: Option<String>,
    pub platform: String,
    pub role: String,
    pub bench_scheduler: bool,
    pub swarm_boss: bool,
    /// v0.6.36 Phase E: live process status derived from
    /// `tmux list-windows` + `ps`. `None` means we couldn't even
    /// query — render as "unknown" in the UI rather than guessing
    /// alive/dead.
    pub process_status: Option<AgentProcessStatus>,
}

#[derive(Debug, Serialize)]
pub struct AgentProcessStatus {
    /// True when this agent has a tmux window named after its slug
    /// in this swarm's `giga-<swarm>` session. For codex agents the
    /// match includes the `-bridge` / `-cli` suffix.
    pub tmux_alive: bool,
    /// True when a `giga watch --as <slug>` process is in `ps` —
    /// either a standalone watcher or the codex bridge sidecar.
    pub watcher_alive: bool,
}

#[derive(Debug, Serialize)]
pub struct ChannelDto {
    pub file: String,
    pub side: String,
    pub participants: Vec<String>,
    pub purpose: Option<String>,
}

pub async fn list_swarms() -> Json<Vec<SwarmSummary>> {
    let entries = match registry::load() {
        Ok(reg) => reg.entries,
        Err(_) => Vec::new(),
    };
    Json(entries.iter().map(summarize_swarm).collect())
}

pub async fn get_swarm(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<SwarmDetail>, StatusCode> {
    let reg = registry::load().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or(StatusCode::NOT_FOUND)?;
    let cfg = Config::load(&entry.config).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(detail_from(entry, &cfg)))
}

#[derive(Debug, Deserialize, Default)]
pub struct TailQuery {
    /// How many posts to return (oldest-newest order). Default 50,
    /// capped at 500 to keep responses bounded.
    pub n: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ChannelTail {
    pub swarm: String,
    pub file: String,
    /// Posts in chronological order (oldest first). Same shape as
    /// post_parser::Post.
    pub posts: Vec<post_parser::Post>,
    /// Total number of posts in the file before truncation. `None`
    /// when the file is missing (channel listed in config but inbox
    /// file not yet created).
    pub total: Option<usize>,
}

pub async fn get_channel_tail(
    AxumPath((name, file)): AxumPath<(String, String)>,
    Query(q): Query<TailQuery>,
) -> Result<Json<ChannelTail>, StatusCode> {
    let reg = registry::load().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or(StatusCode::NOT_FOUND)?;
    let cfg = Config::load(&entry.config).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Security: only files listed in the swarm's [[channels]] are
    // readable. Prevents path-traversal via `../etc/passwd` and
    // accidental disclosure of non-channel files.
    let channel_meta = cfg
        .channels
        .iter()
        .find(|c| c.file == file)
        .ok_or(StatusCode::NOT_FOUND)?;

    let inbox_dir = inbox_dir_for(&cfg, channel_meta).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let path = inbox_dir.join(&file);

    let n = q.n.unwrap_or(50).min(500);

    let (posts, total) = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed = post_parser::parse(&text);
            let total = parsed.len();
            let start = total.saturating_sub(n);
            (parsed[start..].to_vec(), Some(total))
        }
        Err(_) => (Vec::new(), None),
    };

    Ok(Json(ChannelTail {
        swarm: name,
        file,
        posts,
        total,
    }))
}

/// Pick the inbox directory for a channel based on its `side`
/// (`"wsl"` or `"windows"`). Falls back to `wsl_inbox` for unknown
/// sides — caller already validated `channel_meta` so this never
/// surfaces an attacker-controlled side.
fn inbox_dir_for(cfg: &Config, ch: &Channel) -> Option<PathBuf> {
    match ch.side.as_str() {
        "windows" => cfg.paths.windows_inbox.clone(),
        _ => cfg.paths.wsl_inbox.clone(),
    }
}

/// `GET /api/processes` — machine-wide tmux sessions + every
/// `giga watch` Monitor watcher currently running. Useful for
/// cross-swarm orientation ("what's actually live right now?").
pub async fn list_processes() -> Json<process::ProcessSnapshot> {
    Json(process::snapshot())
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

#[derive(Debug, Serialize)]
pub struct PostError {
    pub error: String,
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
    let cfg = Config::load(&entry.config)
        .map_err(|e| internal(&format!("config load: {e:#}")))?;
    let _ = cfg
        .channels
        .iter()
        .find(|c| c.file == file)
        .ok_or_else(|| not_found("channel not in swarm config"))?;

    // Build post::Args and delegate to the canonical implementation.
    // Sync I/O inside an async handler is fine here — the append is
    // a few syscalls; not worth a spawn_blocking dance.
    let args = crate::post::Args {
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
    crate::post::run(args).map_err(|e| {
        let msg = format!("{e:#}");
        // Participant/waiting-on validation errors are the user's
        // fault, not 500s — surface as 400 with the original error.
        if msg.contains("is not a participant") || msg.contains("WAITING ON target") {
            (axum::http::StatusCode::BAD_REQUEST, Json(PostError { error: msg }))
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

fn not_found(msg: &str) -> (axum::http::StatusCode, Json<PostError>) {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(PostError { error: msg.to_string() }),
    )
}

fn internal(msg: &str) -> (axum::http::StatusCode, Json<PostError>) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(PostError { error: msg.to_string() }),
    )
}

#[derive(Debug, Serialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub ok: bool,
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
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("giga"));
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

fn summarize_swarm(entry: &registry::Entry) -> SwarmSummary {
    match Config::load(&entry.config) {
        Ok(cfg) => SwarmSummary {
            name: entry.name.clone(),
            config_path: entry.config.clone(),
            agent_count: cfg.agents.len(),
            channel_count: cfg.channels.len(),
            last_activity_iso: last_activity(&cfg),
            load_error: None,
        },
        Err(e) => SwarmSummary {
            name: entry.name.clone(),
            config_path: entry.config.clone(),
            agent_count: 0,
            channel_count: 0,
            last_activity_iso: None,
            load_error: Some(format!("{e:#}")),
        },
    }
}

fn detail_from(entry: &registry::Entry, cfg: &Config) -> SwarmDetail {
    let snapshot = process::snapshot();
    let session_name = format!("giga-{}", entry.name);
    let tmux_window_names: Vec<String> = snapshot
        .tmux
        .iter()
        .find(|s| s.name == session_name)
        .map(|s| s.windows.iter().map(|w| w.name.clone()).collect())
        .unwrap_or_default();
    let watcher_agents: Vec<String> = snapshot
        .watchers
        .iter()
        .map(|w| w.agent.clone())
        .collect();
    SwarmDetail {
        name: entry.name.clone(),
        config_path: entry.config.clone(),
        description: cfg.project.description.clone(),
        project_runtime: cfg.project.runtime.as_ref().map(|r| r.as_str().to_string()),
        launch_model: cfg.project.launch_model.clone(),
        agents: cfg
            .agents
            .iter()
            .map(|a| agent_dto(a, cfg, &tmux_window_names, &watcher_agents))
            .collect(),
        channels: cfg.channels.iter().map(channel_dto).collect(),
    }
}

fn agent_dto(
    a: &Agent,
    cfg: &Config,
    tmux_window_names: &[String],
    watcher_agents: &[String],
) -> AgentDto {
    AgentDto {
        name: a.name.clone(),
        runtime: cfg.agent_runtime(a).as_str().to_string(),
        workdir: a.workdir.clone(),
        code_root: a.code_root.clone(),
        host: a.host.clone(),
        platform: a.platform.clone(),
        role: a.role.clone(),
        bench_scheduler: a.bench_scheduler,
        swarm_boss: a.swarm_boss,
        process_status: Some(AgentProcessStatus {
            tmux_alive: agent_has_tmux_window(&a.name, tmux_window_names),
            watcher_alive: watcher_agents.iter().any(|w| w == &a.name),
        }),
    }
}

/// Match an agent slug to its tmux window. Single-pane agents have a
/// window named after the slug verbatim; codex agents have
/// `<slug>-bridge` + `<slug>-cli`. Treat the agent as alive if any
/// matching window exists.
fn agent_has_tmux_window(slug: &str, window_names: &[String]) -> bool {
    let bridge = format!("{slug}-bridge");
    let cli = format!("{slug}-cli");
    window_names
        .iter()
        .any(|n| n == slug || n == &bridge || n == &cli)
}

fn channel_dto(c: &Channel) -> ChannelDto {
    ChannelDto {
        file: c.file.clone(),
        side: c.side.clone(),
        participants: c.participants.clone(),
        purpose: c.purpose.clone(),
    }
}

/// Newest mtime among the `*.md` files in this swarm's WSL inbox,
/// formatted as RFC3339. Returns `None` when the inbox path isn't
/// set or the dir can't be read.
fn last_activity(cfg: &Config) -> Option<String> {
    let inbox = cfg.paths.wsl_inbox.as_ref()?;
    newest_md_mtime(inbox).map(|t| {
        let dt: chrono::DateTime<chrono::Utc> = t.into();
        dt.to_rfc3339()
    })
}

fn newest_md_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut latest: Option<std::time::SystemTime> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if latest.map_or(true, |l| mtime > l) {
            latest = Some(mtime);
        }
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn newest_md_mtime_returns_none_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        assert!(newest_md_mtime(&missing).is_none());
    }

    #[test]
    fn newest_md_mtime_returns_none_when_no_md_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("not-md.txt"), "x").unwrap();
        assert!(newest_md_mtime(tmp.path()).is_none());
    }

    #[test]
    fn newest_md_mtime_picks_most_recent() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a.md");
        let b = tmp.path().join("b.md");
        fs::write(&a, "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&b, "second").unwrap();
        let latest = newest_md_mtime(tmp.path()).unwrap();
        let b_mtime = fs::metadata(&b).unwrap().modified().unwrap();
        assert_eq!(latest, b_mtime);
    }

    #[test]
    fn newest_md_mtime_ignores_non_md_extensions() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.md"), "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // .txt should be skipped even though it's newer.
        fs::write(tmp.path().join("zzz.txt"), "newer").unwrap();
        let latest = newest_md_mtime(tmp.path()).unwrap();
        let a_mtime = fs::metadata(tmp.path().join("a.md")).unwrap().modified().unwrap();
        assert_eq!(latest, a_mtime);
    }

    #[test]
    fn summarize_swarm_reports_load_error_when_config_missing() {
        // Entry pointing at a non-existent config. The summarizer
        // must NOT panic — it should return a summary with the
        // load_error populated.
        let entry = registry::Entry {
            name: "missing".to_string(),
            config: PathBuf::from("/nonexistent/giga-harness.toml"),
            code_roots: vec![],
        };
        let summary = summarize_swarm(&entry);
        assert_eq!(summary.name, "missing");
        assert_eq!(summary.agent_count, 0);
        assert_eq!(summary.channel_count, 0);
        assert!(summary.load_error.is_some(), "expected load_error for missing config");
    }
}
