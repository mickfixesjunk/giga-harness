//! TOML config schema for giga-harness.
//!
//! A config describes a project's agent ecosystem: which agents
//! exist, where they work, which channels they participate in,
//! and how the bench-coordination protocol is scoped (single host
//! vs. multi-host).
//!
//! See `examples/minimal/giga-harness.toml` for a working example.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub project: Project,
    pub paths: Paths,
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    pub bench_protocol: Option<BenchProtocol>,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Opening prompt passed to each claude session at launch.
    /// Designed to be referenced from per-agent CLAUDE.md (e.g.,
    /// "Follow the Session Start protocol in CLAUDE.md") so the
    /// concrete actions live in the per-agent doc. If absent,
    /// giga uses a generic default.
    #[serde(default)]
    pub launch_intro_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Paths {
    /// Where WSL-side inbox files live (e.g. `/home/neo/projects/inbox`).
    /// Optional — only required if any channel has `side = "wsl"`.
    #[serde(default)]
    pub wsl_inbox: Option<PathBuf>,
    /// Where Windows-side inbox files live (e.g. `C:/Users/Audio` —
    /// note forward slashes for cross-platform parsing). Optional —
    /// only required if any channel has `side = "windows"`.
    #[serde(default)]
    pub windows_inbox: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Agent {
    pub name: String,
    pub workdir: PathBuf,
    pub role: String,
    /// "wsl" (default) or "windows". Controls how the launcher
    /// spawns this agent's terminal.
    #[serde(default = "default_platform")]
    pub platform: String,
    /// If `true`, this agent is the bench scheduler — every
    /// CPU/IO-heavy operation clears through them. Exactly one
    /// agent per host should be the scheduler.
    #[serde(default)]
    pub bench_scheduler: bool,
    /// Path to a CLAUDE.md template (relative to the config file's
    /// directory). If absent, giga generates a minimal one.
    #[serde(default)]
    pub claudemd_template: Option<PathBuf>,
    /// Override the shell command spawned in this agent's terminal.
    /// If absent, giga picks a platform-appropriate default that
    /// drops into the Claude Code CLI when available.
    #[serde(default)]
    pub launch_cmd: Option<String>,
}

fn default_platform() -> String {
    "wsl".to_string()
}

#[derive(Debug, Deserialize)]
pub struct Channel {
    /// Filename only — directory comes from `paths.<side>_inbox`.
    pub file: String,
    /// "wsl" or "windows" — picks which inbox dir the file lives in.
    pub side: String,
    /// Names of the agents (from `[[agents]]`) that talk on this
    /// channel. Almost always 2; can be more for broadcast channels.
    pub participants: Vec<String>,
    #[serde(default)]
    pub purpose: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BenchProtocol {
    /// Name of the agent that schedules bench slots.
    pub scheduler: String,
    /// "this-host" — all participating agents share one slot pool
    /// (legacy single-host setup). "per-host" — each host has its
    /// own scheduler+slot pool (multi-host setup).
    #[serde(default = "default_slot_pool")]
    pub slot_pool: String,
}

fn default_slot_pool() -> String {
    "this-host".to_string()
}

impl Config {
    /// Read a config from disk and validate it semantically.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing TOML config at {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Cross-check the config: every channel participant resolves
    /// to an agent, every channel side has its inbox dir defined,
    /// at most one bench scheduler, etc.
    pub fn validate(&self) -> Result<()> {
        let agent_names: std::collections::HashSet<&str> =
            self.agents.iter().map(|a| a.name.as_str()).collect();

        for ch in &self.channels {
            for p in &ch.participants {
                if !agent_names.contains(p.as_str()) {
                    return Err(anyhow!(
                        "channel `{}` lists participant `{}` which isn't in [[agents]]",
                        ch.file,
                        p,
                    ));
                }
            }
            match ch.side.as_str() {
                "wsl" => {
                    if self.paths.wsl_inbox.is_none() {
                        return Err(anyhow!(
                            "channel `{}` is side=wsl but paths.wsl_inbox is not set",
                            ch.file,
                        ));
                    }
                }
                "windows" => {
                    if self.paths.windows_inbox.is_none() {
                        return Err(anyhow!(
                            "channel `{}` is side=windows but paths.windows_inbox is not set",
                            ch.file,
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "channel `{}` has unknown side `{}` (want \"wsl\" or \"windows\")",
                        ch.file,
                        other,
                    ));
                }
            }
        }

        let schedulers: Vec<&str> = self
            .agents
            .iter()
            .filter(|a| a.bench_scheduler)
            .map(|a| a.name.as_str())
            .collect();
        if schedulers.len() > 1 {
            return Err(anyhow!(
                "multiple agents flagged as bench_scheduler: {:?}. Only one per host.",
                schedulers,
            ));
        }

        for a in &self.agents {
            if a.platform != "wsl" && a.platform != "windows" {
                return Err(anyhow!(
                    "agent `{}` has unknown platform `{}` (want \"wsl\" or \"windows\")",
                    a.name,
                    a.platform,
                ));
            }
        }

        Ok(())
    }

    /// Resolve a channel file to its absolute path on this host,
    /// using the configured inbox dirs.
    pub fn channel_path(&self, ch: &Channel) -> Result<PathBuf> {
        let dir = match ch.side.as_str() {
            "wsl" => self
                .paths
                .wsl_inbox
                .as_ref()
                .ok_or_else(|| anyhow!("paths.wsl_inbox not set"))?,
            "windows" => self
                .paths
                .windows_inbox
                .as_ref()
                .ok_or_else(|| anyhow!("paths.windows_inbox not set"))?,
            other => return Err(anyhow!("unknown channel side `{}`", other)),
        };
        Ok(dir.join(&ch.file))
    }

    pub fn agent_by_name(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.name == name)
    }
}
