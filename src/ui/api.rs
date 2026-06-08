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
use axum::extract::Path as AxumPath;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
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
    SwarmDetail {
        name: entry.name.clone(),
        config_path: entry.config.clone(),
        description: cfg.project.description.clone(),
        project_runtime: cfg.project.runtime.as_ref().map(|r| r.as_str().to_string()),
        launch_model: cfg.project.launch_model.clone(),
        agents: cfg.agents.iter().map(|a| agent_dto(a, cfg)).collect(),
        channels: cfg.channels.iter().map(channel_dto).collect(),
    }
}

fn agent_dto(a: &Agent, cfg: &Config) -> AgentDto {
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
    }
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
