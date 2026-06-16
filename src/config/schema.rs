//! TOML config schema types for giga-harness.
//!
//! Pure data: every `#[derive(Deserialize)]` struct, its `Default`
//! impl, and the `default_*` serde helper fns. Behavior (load,
//! validate, resolve, broadcast) lives in the sibling modules.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub project: Project,
    /// v0.6.24: optional now. When omitted, inbox paths auto-default
    /// via `apply_path_defaults` — wsl_inbox → `<config_dir>/inbox`,
    /// windows_inbox → `<USERPROFILE>\.giga\configs\<project>\inbox`
    /// (resolved at load time). Explicit values still win.
    #[serde(default)]
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
    /// v0.4.0: configuration for the broadcast-channel fanout limiter.
    /// When absent, sensible defaults apply (15s stagger, "all"
    /// recipients). See BROADCAST_FANOUT_DESIGN.md.
    #[serde(default)]
    pub broadcast: BroadcastConfig,
    /// v0.6.16: configuration for the watcher's stale-wait detection.
    /// When absent, defaults apply (30min threshold, scan at arm time).
    #[serde(default)]
    pub watch: WatchConfig,
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
    /// Absolute path of the canonical TOML file this Config was loaded
    /// from. Populated by `Config::load`. Used by sync to derive the
    /// rsync source path so it doesn't fall back to CWD-relative
    /// `giga-harness.toml`. `None` only when the config was parsed from
    /// an inline string (tests).
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
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
    /// v0.6.0: agent runtime for this swarm. "claude" (default; today's
    /// behavior), "codex", or "agy". Each agent can override via
    /// [[agents]].runtime. See src/runtime.rs.
    #[serde(default)]
    pub runtime: Option<crate::runtime::Runtime>,
}

fn default_launch_model() -> String {
    "claude-opus-4-7".to_string()
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Paths {
    /// Where WSL-side inbox files live (e.g. `/home/alice/projects/inbox`).
    /// Optional — only required if any channel has `side = "wsl"`.
    /// v0.6.24: auto-defaults to `<config_dir>/inbox` at load time
    /// when omitted (see apply_path_defaults).
    #[serde(default)]
    pub wsl_inbox: Option<PathBuf>,
    /// Where Windows-side inbox files live (e.g. `C:/Users/Alice` —
    /// note forward slashes for cross-platform parsing). Optional —
    /// only required if any channel has `side = "windows"`.
    /// v0.6.24: auto-defaults to `<USERPROFILE>\.giga\configs\<project>\inbox`
    /// at load time when omitted (resolved via cmd.exe interop on WSL
    /// or USERPROFILE env on native Windows).
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
/// (e.g. `alice` on box A, `bob` on box B).
///
/// `remote_config_dir` and `remote_inbox_dir` are absolute paths on
/// THIS host (the one the [[hosts]] entry describes) — used by other
/// hosts when they push to this one. Both default to the local
/// caller's matching path, which works for homogeneous-user setups.
/// Set explicitly when paths differ (e.g. `/home/alice/...` on
/// box A vs `/home/bob/...` on box B).
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
    /// v0.3.2 after a peer host exposed the homogeneous-path assumption.
    ///
    /// TOML form:
    ///   [[hosts]]
    ///   name = "host-b"
    ///   paths.wsl_inbox = "/home/alice/projects/inbox"
    ///   paths.windows_inbox = "/mnt/c/Users/Alice/projects/inbox"
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
    /// v0.3.6: when true, this agent hosts the per-host coordination
    /// daemons (sync + merger) via Monitor entries in its CLAUDE.md
    /// instead of as separate tmux panes. At most one per host.
    /// `giga launch` skips the tmux daemon panes for hosts where a
    /// swarm_boss agent exists. See SWARM_BOSS_DESIGN.md.
    #[serde(default)]
    pub swarm_boss: bool,
    /// v0.6.0: per-agent runtime override. When set, this agent uses
    /// the specified runtime ("claude", "codex", "agy") regardless of
    /// the project-level [project].runtime. When None, falls back to
    /// the project default (which itself defaults to "claude"). Lets
    /// mixed-runtime swarms coexist on the same channels.
    #[serde(default)]
    pub runtime: Option<crate::runtime::Runtime>,
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
    /// v0.6.16: per-channel override for the stale-wait threshold.
    /// When set, this channel's pending WAITING ON: <me> tags are
    /// surfaced at arm time only if older than this many minutes.
    /// When unset, falls back to `[watch].stale_wait_threshold_minutes`.
    /// Use for channels with bursty/short-fuse traffic (low override)
    /// or batch-style channels where 30min isn't long enough yet
    /// (high override).
    #[serde(default)]
    pub stale_wait_threshold_minutes: Option<u64>,
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

/// v0.4.0: broadcast fanout limiter config. See BROADCAST_FANOUT_DESIGN.md.
///
/// Controls how watchers handle notifications on `_*.md` channels (the
/// broadcast convention). Pre-v0.4.0 every broadcast woke every
/// participating agent within a single 3s poll tick → synchronous
/// LLM-turn storm and per-account Anthropic TPM rate-limit risk.
#[derive(Debug, Deserialize, Clone)]
pub struct BroadcastConfig {
    /// Per-slot delay in seconds for staggered fanout on `_*.md`
    /// channels. 0 disables (today's behavior; instant fan-out to
    /// everyone). The watcher computes a stable slot per agent via
    /// alphabetical ordering of the recipient list and delays the
    /// Monitor notification by `slot × stagger_seconds`. Worst-case
    /// fanout window for a swarm with N recipients = N × stagger.
    #[serde(default = "default_broadcast_stagger")]
    pub stagger_seconds: u64,
    /// Treat broadcasts without a `[fyi]` / `[ack: ...]` / `[all]`
    /// subject prefix as `[all]`. Set to `"named-only"` to enforce
    /// explicit addressing (no prefix = post error; future use).
    /// Today only `"all"` is wired through; the field exists so the
    /// schema is forward-compatible.
    #[serde(default = "default_broadcast_recipients")]
    pub default_recipients: String,
}

impl Default for BroadcastConfig {
    fn default() -> Self {
        Self {
            stagger_seconds: default_broadcast_stagger(),
            default_recipients: default_broadcast_recipients(),
        }
    }
}

fn default_broadcast_stagger() -> u64 {
    // v0.6.2: bumped 15 → 30. Halves peak TPM during broadcast
    // fanout (relevant for `giga upgrade` rearm and other swarm-wide
    // pings). 19 agents × 30s = 9.5-min worst-case fanout — slow for
    // urgent traffic but the safety/UX tradeoff favors not blowing
    // Anthropic per-account rate-limits during rearm storms.
    30
}

fn default_broadcast_recipients() -> String {
    "all".to_string()
}

/// v0.6.16: watcher behavior config — currently houses the stale-wait
/// detection threshold (see `src/stale_wait.rs`). When the watcher
/// arms, it scans each tracked channel for unresolved `WAITING ON:
/// <me>` tags older than `stale_wait_threshold_minutes` and emits one
/// notification per finding. This turns the silent-wedge failure mode
/// (sender posts, receiver compacts/misses, both stay quiet by
/// protocol) into a self-healing event stream.
#[derive(Debug, Deserialize, Clone)]
pub struct WatchConfig {
    /// How many minutes old an unresolved `WAITING ON: <me>` tag must
    /// be before the watcher surfaces it. Per-channel override via
    /// `[[channels]].stale_wait_threshold_minutes`. Default 30 minutes —
    /// chosen as the floor where a missed reply stops being "they're
    /// typing" and starts being "they wedged".
    #[serde(default = "default_stale_wait_threshold")]
    pub stale_wait_threshold_minutes: u64,
    /// v0.6.17: how often the watcher RE-scans for stale waits after
    /// the initial arm-time scan. Cheap (local file read + parse per
    /// channel) so a tight cadence is fine. The watcher dedupes by
    /// (channel, sender, tag-timestamp) so a single stale wait fires
    /// at most ONE Monitor notification per supersede — zero LLM-
    /// turn cost beyond first detection. Set to 0 to disable
    /// periodic re-scan (arm-time scan still runs).
    ///
    /// Default 60s — catches the "agent alive but missed the
    /// original Monitor notification" + "agent restarted after a
    /// mid-turn API kill" cases without flooding the operator's
    /// stderr.
    #[serde(default = "default_stale_wait_recheck")]
    pub stale_wait_recheck_seconds: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            stale_wait_threshold_minutes: default_stale_wait_threshold(),
            stale_wait_recheck_seconds: default_stale_wait_recheck(),
        }
    }
}

fn default_stale_wait_threshold() -> u64 {
    30
}

fn default_stale_wait_recheck() -> u64 {
    60
}

/// Format of the sibling `this_host.toml` file: a single key telling
/// us which `[[hosts]].name` represents THIS machine. Kept separate
/// from the canonical config so rsync of the canonical between hosts
/// doesn't trample per-host identity.
#[derive(Debug, Deserialize)]
pub(super) struct ThisHostFile {
    pub(super) this_host: String,
}

/// Look for the per-host identity file next to the canonical config,
/// parse it, and return the host name. Missing file is OK (local-only
/// mode); parse errors are surfaced.
///
/// v0.3.9 Bug 5b: prefer `this_host.local.toml` (the v0.3.9+ name).
/// The `.local.toml` suffix is a convention meaning "host-private,
/// never rsync between machines" — chosen so a bare `rsync -av` of
/// the swarm dir between hosts doesn't overwrite the peer's identity
/// (the bug user-agent hit). Fall back to legacy `this_host.toml`
/// for backward compat with v0.3.8 and earlier swarms.
pub const THIS_HOST_FILE: &str = "this_host.local.toml";
pub const THIS_HOST_FILE_LEGACY: &str = "this_host.toml";

#[cfg(test)]
mod tests {
    use super::super::*;

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

    #[test]
    fn broadcast_config_defaults_when_section_missing() {
        // v0.6.2: default bumped 15 → 30 to halve peak TPM during
        // broadcast fanout.
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert_eq!(cfg.broadcast.stagger_seconds, 30);
        assert_eq!(cfg.broadcast.default_recipients, "all");
    }

    #[test]
    fn broadcast_config_overrides_via_toml() {
        let body = format!(
            "{}\n[broadcast]\nstagger_seconds = 5\ndefault_recipients = \"all\"\n",
            minimal()
        );
        let cfg = Config::load_str_for_test(&body).unwrap();
        assert_eq!(cfg.broadcast.stagger_seconds, 5);
    }
}
