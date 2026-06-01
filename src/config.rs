//! TOML config schema for giga-harness.
//!
//! A config describes a project's agent ecosystem: which agents
//! exist, where they work, which channels they participate in,
//! and how the bench-coordination protocol is scoped (single host
//! vs. multi-host).
//!
//! Remote-channels extension (per REMOTE_DESIGN.md):
//! - `[[hosts]]` table enumerates every host in the swarm.
//! - `[[agents]].host` names which host an agent runs on.
//! - `this_host` (the host identity of THIS machine) is loaded from a
//!   sibling `this_host.toml` next to the canonical config so rsync of
//!   the canonical doesn't trample per-host identity.
//!
//! All three additions are backward-compatible: a config with no
//! `[[hosts]]` and no `this_host.toml` behaves exactly as today
//! (local-only mode).
//!
//! See `examples/minimal/giga-harness.toml` for a working example.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub project: Project,
    pub paths: Paths,
    #[serde(default)]
    pub hosts: Vec<Host>,
    /// Pluggable transport selection (v0.3+). When absent, `transport::for_config`
    /// infers: `local` if no `[[hosts]]`, `rsync+tailscale` if hosts present —
    /// preserving v0.2 behavior for un-tagged swarms.
    #[serde(default)]
    pub transport: Option<TransportConfig>,
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    pub bench_protocol: Option<BenchProtocol>,
    /// The host name (matching one of `[[hosts]].name`) that identifies
    /// THIS machine within the swarm. Loaded at `Config::load` time from
    /// a sibling `this_host.toml` next to the canonical config (so rsync
    /// of the canonical config between hosts doesn't trample it).
    ///
    /// `None` means local-only mode — today's behavior. A non-empty
    /// `[[hosts]]` with a `None` `this_host` is degenerate (no slice
    /// suffix to write to; remote operations will refuse) and is flagged
    /// by validation.
    #[serde(skip)]
    pub this_host: Option<String>,
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
    /// Model passed to each spawned claude session via `--model`.
    /// Default: `claude-opus-4-7`. Agents need to follow nuanced
    /// instructions (Monitor TOOL vs Bash, role boundaries, etc.)
    /// and Opus follows those more reliably than Sonnet. Override
    /// per-swarm if you have a reason — e.g. `"claude-sonnet-4-6"`
    /// for cheaper agents.
    #[serde(default = "default_launch_model")]
    pub launch_model: String,
}

fn default_launch_model() -> String {
    "claude-opus-4-7".to_string()
}

#[derive(Debug, Deserialize, Clone)]
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

/// Transport selection for the swarm (v0.3+). `kind` is the active plug
/// name (matches `transport::for_config` dispatch). Per-kind config
/// lives in the matching `[transport.<kind>]` sub-table; only the active
/// kind's section is read.
///
/// See TRANSPORT_DESIGN.md for the full schema.
#[derive(Debug, Deserialize, Clone)]
pub struct TransportConfig {
    pub kind: String,
    #[serde(default)]
    pub git: Option<GitTransportConfig>,
    // Future per-kind sections:
    //   #[serde(default)] pub s3: Option<S3TransportConfig>,
    //   #[serde(default)] pub azure: Option<AzureTransportConfig>,
    //   etc.
}

/// `[transport.git]` config: the shared git repo + optional local clone path.
#[derive(Debug, Deserialize, Clone)]
pub struct GitTransportConfig {
    /// Git remote URL (SSH or HTTPS) of the swarm state repo.
    pub state_repo: String,
    /// Optional override for the local clone location. Defaults to
    /// `~/.giga/swarm-state/<project>/`.
    #[serde(default)]
    pub local_clone_dir: Option<PathBuf>,
}

/// A host in the swarm. Enumerated when the swarm spans more than one
/// physical machine; absent for all-local swarms (today's default).
///
/// `tailnet_hostname` is what `giga remote` / `giga sync` use to reach
/// this host over the tailnet (e.g. `wsl-box-b.tail1234.ts.net`).
///
/// `ssh_user` is the OS user account on this host. Defaults to the
/// caller's `$USER` if omitted — the common case when the same user
/// runs on every box. Set explicitly when hosts have different users
/// (e.g. `neomatrix` on box A, `neo` on box B).
///
/// `remote_config_dir` and `remote_inbox_dir` are absolute paths on
/// THIS host (the one the [[hosts]] entry describes) — used by other
/// hosts when they push to this one. Both default to the local
/// caller's matching path, which works for homogeneous-user setups.
/// Set explicitly when paths differ (e.g. `/home/neomatrix/...` on
/// box A vs `/home/neo/...` on box B).
#[derive(Debug, Deserialize, Clone)]
pub struct Host {
    pub name: String,
    pub tailnet_hostname: String,
    #[serde(default)]
    pub ssh_user: Option<String>,
    #[serde(default)]
    pub remote_config_dir: Option<PathBuf>,
    #[serde(default)]
    pub remote_inbox_dir: Option<PathBuf>,
    /// Per-host override for `[paths]`. Supersedes the global `[paths]`
    /// for THIS host's local operations (init, channel_path) AND for
    /// sync targets pushing to it. Use when peers have asymmetric paths
    /// (different `$HOME`, different Windows user, etc.) — added in
    /// v0.3.2 after morpheus-wsl exposed the homogeneous-path assumption.
    ///
    /// TOML form:
    ///   [[hosts]]
    ///   name = "morpheus-wsl"
    ///   paths.wsl_inbox = "/home/neo/projects/inbox"
    ///   paths.windows_inbox = "/mnt/c/Users/audio/projects/inbox"
    #[serde(default)]
    pub paths: Option<Paths>,
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
    /// Which host this agent runs on (must match a `[[hosts]].name`).
    /// `None` means "this_host" by default — backward-compatible for
    /// all-local swarms where `[[hosts]]` is empty and every agent is
    /// implicitly here. Set explicitly when adding agents on a peer
    /// host (`giga add-agent --host <peer>`).
    #[serde(default)]
    pub host: Option<String>,
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
    /// If `true`, request administrator elevation for this agent's
    /// terminal tab. Windows Terminal only (`wt.exe`); triggers a UAC
    /// prompt at launch time. Ignored on non-Windows platforms.
    #[serde(default)]
    pub admin: bool,
    /// The directory where this agent actually edits code. Separate
    /// from `workdir` (the launch context where CLAUDE.md lives) so
    /// an agent can have an isolated home while working against a
    /// shared codebase. When set, giga injects it into the agent's
    /// CLAUDE.md and the launch intro prompt.
    #[serde(default)]
    pub code_root: Option<PathBuf>,
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

/// Format of the sibling `this_host.toml` file: a single key telling
/// us which `[[hosts]].name` represents THIS machine. Kept separate
/// from the canonical config so rsync of the canonical between hosts
/// doesn't trample per-host identity.
#[derive(Debug, Deserialize)]
struct ThisHostFile {
    this_host: String,
}

/// Look for `this_host.toml` next to the canonical config, parse it,
/// and return the host name. Missing file is OK (local-only mode);
/// parse errors are surfaced.
fn load_this_host(config_path: &Path) -> Result<Option<String>> {
    let Some(parent) = config_path.parent() else {
        return Ok(None);
    };
    let sibling = parent.join("this_host.toml");
    if !sibling.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&sibling)
        .with_context(|| format!("reading {}", sibling.display()))?;
    let parsed: ThisHostFile = toml::from_str(&text)
        .with_context(|| format!("parsing {} (expected `this_host = \"...\"`)", sibling.display()))?;
    Ok(Some(parsed.this_host))
}

impl Config {
    /// Read a config from disk and validate it semantically.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing TOML config at {}", path.display()))?;
        cfg.this_host = load_this_host(path)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Cross-check the config: every channel participant resolves
    /// to an agent, every channel side has its inbox dir defined,
    /// at most one bench scheduler, etc.
    pub fn validate(&self) -> Result<()> {
        let agent_names: std::collections::HashSet<&str> =
            self.agents.iter().map(|a| a.name.as_str()).collect();

        // [[hosts]] uniqueness + agent.host resolution.
        let mut seen_host_names: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for h in &self.hosts {
            if !seen_host_names.insert(h.name.as_str()) {
                return Err(anyhow!(
                    "duplicate [[hosts]] entry `{}` (host names must be unique)",
                    h.name,
                ));
            }
        }

        for a in &self.agents {
            if let Some(host) = &a.host {
                if !seen_host_names.contains(host.as_str()) {
                    return Err(anyhow!(
                        "agent `{}` has host `{}` which isn't in [[hosts]]",
                        a.name,
                        host,
                    ));
                }
            }
        }

        // this_host (if present) must resolve to a hosts entry.
        if let Some(th) = &self.this_host {
            if !seen_host_names.contains(th.as_str()) {
                return Err(anyhow!(
                    "this_host = `{}` isn't in [[hosts]] (check the sibling this_host.toml)",
                    th,
                ));
            }
        }

        // Degenerate combo: [[hosts]] declared but no this_host known.
        // The swarm has remote topology but this machine doesn't know
        // its own identity — slice writes have no suffix to use.
        if !self.hosts.is_empty() && self.this_host.is_none() {
            return Err(anyhow!(
                "[[hosts]] is declared but this_host is unknown (create a sibling this_host.toml with `this_host = \"<host-name>\"`)",
            ));
        }

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

    /// The effective host for an agent: explicit `agent.host` if set,
    /// otherwise `this_host` (the local machine). Returns `None` only
    /// when neither is known — local-only mode pre-remote-channels.
    pub fn agent_host<'a>(&'a self, agent: &'a Agent) -> Option<&'a str> {
        agent
            .host
            .as_deref()
            .or(self.this_host.as_deref())
    }

    /// True when every participant of the channel lives on `this_host`
    /// (or `[[hosts]]` is empty — today's local-only mode). When true,
    /// `post` can take the fast-path direct write to `<channel>.md`
    /// instead of writing to a per-host slice file.
    ///
    /// Unknown participants are treated as remote (conservative) — this
    /// should never happen for a validated config but defensive nonetheless.
    pub fn channel_is_local(&self, ch: &Channel) -> bool {
        if self.hosts.is_empty() {
            return true; // pre-remote-channels world: nothing is "remote"
        }
        let Some(this) = self.this_host.as_deref() else {
            return true; // degenerate but validated config wouldn't reach here
        };
        ch.participants.iter().all(|p| {
            self.agents
                .iter()
                .find(|a| a.name == *p)
                .and_then(|a| self.agent_host(a))
                .map(|h| h == this)
                .unwrap_or(false)
        })
    }

    /// Resolve a channel file to its absolute path on this host,
    /// using the configured inbox dirs. The configured dir may be in
    /// the other side's path form (e.g., windows_inbox = "/mnt/c/..."
    /// for WSL convenience); `to_host_fs` translates it to whatever
    /// the current host's filesystem expects.
    ///
    /// Uses the per-host [paths] override under `[[hosts]]` when this_host
    /// is known and the override is set (v0.3.2+ asymmetric-path
    /// support); falls back to the global `[paths]` otherwise.
    pub fn channel_path(&self, ch: &Channel) -> Result<PathBuf> {
        let dir = self
            .inbox_for_host_side(self.this_host.as_deref(), &ch.side)
            .ok_or_else(|| anyhow!("no inbox path for channel `{}` (side={})", ch.file, ch.side))?;
        Ok(crate::fs_paths::to_host_fs(&dir).join(&ch.file))
    }

    /// Returns the inbox dir to use for a given host + side. Priority:
    ///   1. `[[hosts]].paths.<side>_inbox` (per-host explicit override; v0.3.2+)
    ///   2. `[[hosts]].remote_inbox_dir` (v0.2 back-compat — applies to
    ///      the wsl side only since that's what it was designed for)
    ///   3. global `[paths].<side>_inbox` (homogeneous-path fallback)
    ///
    /// `host_name = None` means "no host context" (legacy local-only
    /// swarm or pre-host-resolution); always uses the global path.
    pub fn inbox_for_host_side(&self, host_name: Option<&str>, side: &str) -> Option<PathBuf> {
        if let Some(name) = host_name {
            if let Some(host) = self.hosts.iter().find(|h| h.name == name) {
                if let Some(host_paths) = &host.paths {
                    let p = match side {
                        "wsl" => host_paths.wsl_inbox.as_ref(),
                        "windows" => host_paths.windows_inbox.as_ref(),
                        _ => None,
                    };
                    if let Some(p) = p {
                        return Some(p.clone());
                    }
                }
                // v0.2 back-compat shim — remote_inbox_dir is a single
                // path with no side distinction; treat it as wsl-side.
                if side == "wsl" {
                    if let Some(p) = &host.remote_inbox_dir {
                        return Some(p.clone());
                    }
                }
            }
        }
        match side {
            "wsl" => self.paths.wsl_inbox.clone(),
            "windows" => self.paths.windows_inbox.clone(),
            _ => None,
        }
    }

    pub fn agent_by_name(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Parse a config from a string (no file I/O). Used by tests so
    /// fixtures can be inline rather than requiring tempfiles for
    /// every scenario. Pure validation only — no path resolution
    /// beyond what's in the string.
    #[cfg(test)]
    pub fn load_str_for_test(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).with_context(|| "parsing inline test TOML")?;
        cfg.validate()?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> &'static str {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"

[[agents]]
name = "b"
workdir = "/h/b"
role = "."
platform = "wsl"

[[channels]]
file = "a-b.md"
side = "wsl"
participants = ["a", "b"]
"#
    }

    #[test]
    fn parses_minimal_config() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert_eq!(cfg.project.name, "t");
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.channels.len(), 1);
    }

    #[test]
    fn rejects_unknown_participant() {
        let body = minimal().replace(
            r#"participants = ["a", "b"]"#,
            r#"participants = ["a", "ghost"]"#,
        );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("ghost"));
        assert!(err.to_string().contains("isn't in [[agents]]"));
    }

    #[test]
    fn rejects_unknown_channel_side() {
        let body = minimal().replace(r#"side = "wsl""#, r#"side = "macos""#);
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("unknown side"));
    }

    #[test]
    fn rejects_windows_channel_without_windows_inbox() {
        let body = minimal().replace(r#"side = "wsl""#, r#"side = "windows""#);
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("windows_inbox"));
    }

    #[test]
    fn rejects_wsl_channel_without_wsl_inbox() {
        let body = r#"
[project]
name = "t"

[paths]
windows_inbox = "/tmp/iw"

[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"

[[agents]]
name = "b"
workdir = "/h/b"
role = "."
platform = "wsl"

[[channels]]
file = "a-b.md"
side = "wsl"
participants = ["a", "b"]
"#;
        let err = Config::load_str_for_test(body).unwrap_err();
        assert!(err.to_string().contains("wsl_inbox"));
    }

    #[test]
    fn rejects_multiple_bench_schedulers() {
        let body = minimal()
            .replace(
                r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
                r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
bench_scheduler = true"#,
            )
            .replace(
                r#"name = "b"
workdir = "/h/b"
role = "."
platform = "wsl""#,
                r#"name = "b"
workdir = "/h/b"
role = "."
platform = "wsl"
bench_scheduler = true"#,
            );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("multiple agents"));
    }

    #[test]
    fn accepts_single_bench_scheduler() {
        let body = minimal().replace(
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
bench_scheduler = true"#,
        );
        let cfg = Config::load_str_for_test(&body).unwrap();
        assert_eq!(cfg.agents.iter().filter(|a| a.bench_scheduler).count(), 1);
    }

    #[test]
    fn rejects_unknown_platform() {
        let body = minimal().replace(
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "linux""#,
        );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("unknown platform"));
    }

    #[test]
    fn channel_path_resolves_wsl_side() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        let ch = &cfg.channels[0];
        let p = cfg.channel_path(ch).unwrap();
        assert!(p.ends_with("a-b.md"));
        assert!(p.starts_with("/tmp/i"));
    }

    #[test]
    fn config_with_no_channels_validates() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.channels.len(), 0);
    }

    #[test]
    fn config_with_no_agents_validates() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        assert_eq!(cfg.agents.len(), 0);
    }

    #[test]
    fn defaults_platform_to_wsl_when_omitted() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "a"
workdir = "/h/a"
role = "."
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        assert_eq!(cfg.agents[0].platform, "wsl");
    }

    #[test]
    fn bench_protocol_defaults_slot_pool_to_this_host() {
        let body = format!("{}\n[bench_protocol]\nscheduler = \"a\"\n", minimal(),);
        let cfg = Config::load_str_for_test(&body).unwrap();
        let bp = cfg.bench_protocol.as_ref().unwrap();
        assert_eq!(bp.scheduler, "a");
        assert_eq!(bp.slot_pool, "this-host");
    }

    #[test]
    fn channel_with_three_participants_validates() {
        let body = minimal().replace(
            r#"[[channels]]
file = "a-b.md"
side = "wsl"
participants = ["a", "b"]"#,
            r#"[[agents]]
name = "c"
workdir = "/h/c"
role = "."
platform = "wsl"

[[channels]]
file = "_all.md"
side = "wsl"
participants = ["a", "b", "c"]"#,
        );
        let cfg = Config::load_str_for_test(&body).unwrap();
        assert_eq!(cfg.channels[0].participants.len(), 3);
    }

    #[test]
    fn code_root_field_is_optional_and_absent_by_default() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert!(
            cfg.agents.iter().all(|a| a.code_root.is_none()),
            "minimal config has no code_root → all agents should deserialize with None",
        );
    }

    #[test]
    fn code_root_field_round_trips() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "a"
workdir = "/h/a"
code_root = "/code/shared"
role = "."
platform = "wsl"
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        assert_eq!(
            cfg.agents[0]
                .code_root
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            Some("/code/shared".to_string()),
        );
    }

    #[test]
    fn multiple_agents_can_share_one_code_root() {
        // Common pattern: each agent has its own isolated workdir but
        // they all edit the same codebase. The schema must allow this.
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
code_root = "/code/proj"
role = "."
platform = "wsl"

[[agents]]
name = "code"
workdir = "/h/code"
code_root = "/code/proj"
role = "."
platform = "wsl"
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        assert_eq!(cfg.agents.len(), 2);
        let roots: Vec<_> = cfg
            .agents
            .iter()
            .filter_map(|a| a.code_root.as_ref())
            .collect();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0], roots[1]);
    }

    // -------------------------------------------------------------------
    // Remote-channels schema tests (per REMOTE_DESIGN.md). The new
    // [[hosts]] / this_host / [[agents]].host fields are all optional;
    // absence reproduces today's local-only behavior exactly.
    // -------------------------------------------------------------------

    fn minimal_two_host() -> String {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0000.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0000.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#
        .to_string()
    }

    #[test]
    fn parses_hosts_table() {
        // load_str_for_test goes through validate(), which would reject
        // a config with [[hosts]] non-empty and this_host=None. Bypass
        // by going direct to toml::from_str + manual validate.
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        assert_eq!(cfg.hosts.len(), 2);
        assert_eq!(cfg.hosts[0].name, "wsl-a");
        assert_eq!(cfg.hosts[1].tailnet_hostname, "wsl-b.tail0000.ts.net");
    }

    #[test]
    fn agent_host_resolves_to_hosts_entry() {
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let alice = cfg.agent_by_name("alice").unwrap();
        let bob = cfg.agent_by_name("bob").unwrap();
        assert_eq!(cfg.agent_host(alice), Some("wsl-a"));
        assert_eq!(cfg.agent_host(bob), Some("wsl-b"));
    }

    #[test]
    fn unknown_agent_host_fails_validation() {
        let body = minimal_two_host().replace(r#"host = "wsl-b""#, r#"host = "ghost-host""#);
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("ghost-host"));
        assert!(err.to_string().contains("isn't in [[hosts]]"));
    }

    #[test]
    fn duplicate_host_names_fail_validation() {
        let body = minimal_two_host().replace(
            r#"name = "wsl-b"
tailnet_hostname = "wsl-b.tail0000.ts.net""#,
            r#"name = "wsl-a"
tailnet_hostname = "wsl-a-dup.tail0000.ts.net""#,
        );
        let cfg: Config = toml::from_str(&body).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate"));
        assert!(err.to_string().contains("wsl-a"));
    }

    #[test]
    fn this_host_must_resolve_to_hosts_entry() {
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("nowhere".into());
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("this_host"));
        assert!(err.to_string().contains("nowhere"));
    }

    #[test]
    fn hosts_declared_but_this_host_missing_is_degenerate() {
        let cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        // this_host left as None
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("this_host"));
        assert!(err.to_string().contains("this_host.toml"));
    }

    #[test]
    fn empty_hosts_section_works_with_no_this_host() {
        // Backward compat: today's local-only configs have no [[hosts]]
        // and no this_host. Must validate cleanly.
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert!(cfg.hosts.is_empty());
        assert!(cfg.this_host.is_none());
        // And channel_is_local returns true (local fast-path)
        let ch = &cfg.channels[0];
        assert!(cfg.channel_is_local(ch));
    }

    #[test]
    fn agent_without_host_field_falls_back_to_this_host() {
        // When [[hosts]] exists but an agent omits `host`, it implicitly
        // belongs to this_host. Lets a config writer skip the redundant
        // host = "wsl-a" on every local agent.
        let body = minimal_two_host().replace(r#"host = "wsl-a""#, "");
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let alice = cfg.agent_by_name("alice").unwrap();
        assert_eq!(alice.host, None);
        assert_eq!(cfg.agent_host(alice), Some("wsl-a")); // resolved via this_host
    }

    #[test]
    fn channel_is_local_when_all_participants_share_this_host() {
        let body = minimal_two_host().replace(
            r#"participants = ["alice", "bob"]"#,
            r#"participants = ["alice"]"#,
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let ch = &cfg.channels[0];
        assert!(cfg.channel_is_local(ch), "alice is on wsl-a (this_host) -> local fast-path");
    }

    #[test]
    fn channel_is_not_local_when_participants_span_hosts() {
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let ch = &cfg.channels[0];
        assert!(!cfg.channel_is_local(ch), "alice@wsl-a + bob@wsl-b spans hosts -> slice path");
    }

    #[test]
    fn channel_is_local_when_this_host_is_the_other_side() {
        // Same config viewed from wsl-b: alice is remote, bob is local.
        // The channel is still cross-host (not local fast-path).
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-b".into());
        cfg.validate().unwrap();
        let ch = &cfg.channels[0];
        assert!(!cfg.channel_is_local(ch));
    }

    #[test]
    fn this_host_file_loaded_from_sibling() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        fs::write(&cfg_path, minimal_two_host()).unwrap();
        fs::write(tmp.path().join("this_host.toml"), "this_host = \"wsl-a\"\n").unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.this_host.as_deref(), Some("wsl-a"));
    }

    #[test]
    fn this_host_file_absent_is_ok_for_local_only_swarm() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        fs::write(&cfg_path, minimal()).unwrap(); // no [[hosts]]
        let cfg = Config::load(&cfg_path).unwrap();
        assert!(cfg.this_host.is_none());
        assert!(cfg.hosts.is_empty());
    }

    #[test]
    fn this_host_file_malformed_surfaces_error() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        fs::write(&cfg_path, minimal_two_host()).unwrap();
        // missing the `this_host` key
        fs::write(tmp.path().join("this_host.toml"), "host = \"wsl-a\"\n").unwrap();
        let err = Config::load(&cfg_path).unwrap_err();
        assert!(err.to_string().contains("this_host.toml"));
    }

    #[test]
    fn host_with_optional_ssh_user() {
        let body = minimal_two_host().replace(
            r#"[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0000.ts.net""#,
            r#"[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0000.ts.net"
ssh_user = "neomatrix""#,
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        assert_eq!(cfg.hosts[0].ssh_user.as_deref(), Some("neomatrix"));
        assert_eq!(cfg.hosts[1].ssh_user, None); // omitted defaults to None
    }

    // -------------------------------------------------------------------
    // v0.3.2: per-host [paths] override (quality finding 1 — morpheus-wsl
    // had different $HOME than operator, init failed on literal path).
    // -------------------------------------------------------------------

    #[test]
    fn inbox_for_host_side_uses_per_host_paths_override() {
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"
paths.wsl_inbox = "/home/neo/projects/inbox"
"#,
            minimal()
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        // wsl-a has no override → falls through to global /tmp/i
        assert_eq!(
            cfg.inbox_for_host_side(Some("wsl-a"), "wsl"),
            Some(PathBuf::from("/tmp/i"))
        );
        // wsl-b has per-host override → uses that
        assert_eq!(
            cfg.inbox_for_host_side(Some("wsl-b"), "wsl"),
            Some(PathBuf::from("/home/neo/projects/inbox"))
        );
    }

    #[test]
    fn inbox_for_host_side_falls_back_to_remote_inbox_dir_v02_compat() {
        // v0.2 swarms used [[hosts]].remote_inbox_dir before the
        // explicit per-host [paths] field existed. Keep working.
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "b.tail0.ts.net"
remote_inbox_dir = "/legacy/path"
"#,
            minimal()
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        assert_eq!(
            cfg.inbox_for_host_side(Some("wsl-b"), "wsl"),
            Some(PathBuf::from("/legacy/path"))
        );
    }

    #[test]
    fn inbox_for_host_side_unknown_host_uses_global() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert_eq!(
            cfg.inbox_for_host_side(Some("ghost"), "wsl"),
            Some(PathBuf::from("/tmp/i"))
        );
    }

    #[test]
    fn inbox_for_host_side_no_host_context_uses_global() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert_eq!(cfg.inbox_for_host_side(None, "wsl"), Some(PathBuf::from("/tmp/i")));
    }

    #[test]
    fn channel_path_uses_per_host_override_when_this_host_set() {
        // The killer test: a peer with asymmetric paths must NOT try to
        // resolve channels against the operator's local-only inbox.
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "b.tail0.ts.net"
paths.wsl_inbox = "/home/neo/projects/inbox"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["a", "b"]
"#,
            minimal().trim_end_matches(|c: char| c == '\n')
        );
        // Add a "b" agent on wsl-b for the channel participants validation.
        let body = body.replace(
            "[[channels]]",
            "[[agents]]\nname = \"b\"\nworkdir = \"/h/b\"\nrole = \".\"\nplatform = \"wsl\"\nhost = \"wsl-b\"\n\n[[channels]]",
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-b".into());
        cfg.validate().unwrap();
        let ch = cfg.channels.iter().find(|c| c.file == "alice-bob.md").unwrap();
        let path = cfg.channel_path(ch).unwrap();
        assert!(
            path.starts_with("/home/neo/projects/inbox"),
            "channel_path on wsl-b should use wsl-b's override, got {}",
            path.display()
        );
    }
}
