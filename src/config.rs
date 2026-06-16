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

/// v0.4.0: parsed shape of a broadcast subject's leading prefix. Used
/// by `watch.rs` to decide what to do with a notification on a `_*.md`
/// channel. See BROADCAST_FANOUT_DESIGN.md §3.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastPrefix {
    /// `[fyi]` — informational. Watcher logs to per-agent archive
    /// instead of firing Monitor notification (zero LLM cost).
    Fyi,
    /// `[ack: a, b, c]` — fire only for agents in the list.
    Ack(Vec<String>),
    /// `[all]` or no prefix — fire for every participant (with
    /// staggered fanout from `BroadcastConfig.stagger_seconds`).
    All,
    /// v0.6.3: `[giga-rearm]` — silent watcher-rebinary signal.
    /// Watcher writes its cursor past this message, then POSIX-execve's
    /// itself with the same args. New binary loads from disk; Monitor
    /// task sees no exit; agent's Claude session is never woken.
    /// Zero API calls swarm-wide. `giga upgrade` posts with this
    /// prefix as of v0.6.3. Pre-v0.6.3 watchers parse this as None →
    /// fall back to `All` (wake-up rearm) — backward compat for the
    /// first upgrade ONTO v0.6.3.
    GigaRearm,
}

/// Parse the leading broadcast prefix out of a subject line. Tolerant
/// of whitespace; case-insensitive on the prefix tag. Returns `None`
/// for the unprefixed case (caller treats as `All` when
/// `default_recipients = "all"`). The prefix may appear AFTER the
/// existing `[<agent> YYYY-MM-DD HH:MM PST]` convention header — the
/// parser scans past the timestamp-shaped first prefix when present.
pub fn parse_broadcast_prefix(subject: &str) -> Option<BroadcastPrefix> {
    let rest = strip_timestamp_prefix(subject.trim_start());
    let rest = rest.trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let end = rest.find(']')?;
    let inside = rest[1..end].trim();
    if inside.is_empty() {
        return None;
    }
    let lower = inside.to_ascii_lowercase();
    if lower == "fyi" {
        return Some(BroadcastPrefix::Fyi);
    }
    if lower == "all" {
        return Some(BroadcastPrefix::All);
    }
    if lower == "giga-rearm" {
        return Some(BroadcastPrefix::GigaRearm);
    }
    // `[ack: a, b, c]` form. Split on first `:`.
    if let Some(colon) = inside.find(':') {
        let tag = inside[..colon].trim().to_ascii_lowercase();
        if tag == "ack" {
            let recipients: Vec<String> = inside[colon + 1..]
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return Some(BroadcastPrefix::Ack(recipients));
        }
    }
    None
}

/// If the subject begins with `[<word> YYYY-MM-DD HH:MM <TZ>]`, return
/// the slice AFTER that prefix. Otherwise return the input. This is
/// the convention header agents already use; broadcast prefixes
/// appear AFTER it, so the parser needs to skip past.
fn strip_timestamp_prefix(s: &str) -> &str {
    if !s.starts_with('[') {
        return s;
    }
    let Some(end) = s.find(']') else {
        return s;
    };
    let inside = &s[1..end];
    // Heuristic: contains a date-like `YYYY-MM-DD` substring AND
    // doesn't look like one of our broadcast tags. Cheap + good enough.
    let looks_like_timestamp = inside.chars().filter(|c| *c == '-').count() >= 2
        && inside.chars().any(|c| c.is_ascii_digit());
    let lower = inside.trim().to_ascii_lowercase();
    let is_broadcast_tag = lower == "fyi"
        || lower == "all"
        || lower == "giga-rearm"
        || lower.starts_with("ack:")
        || lower.starts_with("ack ");
    if looks_like_timestamp && !is_broadcast_tag {
        return &s[end + 1..];
    }
    s
}

/// True for channel filenames that match the broadcast convention
/// (`_*.md`). Used by `watch.rs` to decide whether broadcast-specific
/// fanout handling applies.
pub fn is_broadcast_channel(filename: &str) -> bool {
    filename.starts_with('_') && filename.ends_with(".md")
}

/// Compute the stable per-agent fanout delay slot for a broadcast.
/// Slot = position of `this_agent` in the alphabetically-sorted
/// recipient list. Same agent always gets the same slot (deterministic
/// across watcher restarts). See BROADCAST_FANOUT_DESIGN.md §3.2.
pub fn fanout_delay_seconds(this_agent: &str, recipients: &[&str], stagger_seconds: u64) -> u64 {
    if stagger_seconds == 0 {
        return 0;
    }
    let mut sorted: Vec<&str> = recipients.to_vec();
    sorted.sort();
    let slot = sorted.iter().position(|a| *a == this_agent).unwrap_or(0) as u64;
    slot * stagger_seconds
}

/// Format of the sibling `this_host.toml` file: a single key telling
/// us which `[[hosts]].name` represents THIS machine. Kept separate
/// from the canonical config so rsync of the canonical between hosts
/// doesn't trample per-host identity.
#[derive(Debug, Deserialize)]
struct ThisHostFile {
    this_host: String,
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

fn load_this_host(config_path: &Path) -> Result<Option<String>> {
    let Some(parent) = config_path.parent() else {
        return Ok(None);
    };
    // v0.3.9+ name wins when both exist.
    let preferred = parent.join(THIS_HOST_FILE);
    let legacy = parent.join(THIS_HOST_FILE_LEGACY);
    let path = if preferred.exists() {
        preferred
    } else if legacy.exists() {
        legacy
    } else {
        return Ok(None);
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: ThisHostFile = toml::from_str(&text).with_context(|| {
        format!(
            "parsing {} (expected `this_host = \"...\"`)",
            path.display()
        )
    })?;
    Ok(Some(parsed.this_host))
}

/// v0.6.24: when the swarm config omits explicit `paths.wsl_inbox`
/// or `paths.windows_inbox`, fill in sensible per-host defaults so
/// the common case doesn't need an explicit `[paths]` block at all.
///
/// Defaults:
///   * `wsl_inbox`    → `<config_dir>/inbox`
///   * `windows_inbox` → `<USERPROFILE>\.giga\configs\<project>\inbox`
///     (resolved via cmd.exe interop on WSL, or `%USERPROFILE%`
///     env-var on native Windows)
///
/// Per-host overrides via `[[hosts]].paths` still win when set.
/// Explicit values in the top-level `[paths]` block also still win;
/// this fn only fills in MISSING values.
fn apply_path_defaults(cfg: &mut Config, canonical_config_path: &std::path::Path) {
    let config_dir = match canonical_config_path.parent() {
        Some(p) => p,
        None => return, // pathological: config has no parent, can't compute defaults
    };
    if cfg.paths.wsl_inbox.is_none() {
        cfg.paths.wsl_inbox = Some(config_dir.join("inbox"));
    }
    if cfg.paths.windows_inbox.is_none() {
        if let Some(profile_win) = resolve_windows_userprofile() {
            // Build the Windows-form path first, then translate to
            // WSL-form (/mnt/c/...) so the stored canonical matches
            // existing-swarm convention. On a native-Windows host,
            // fs_paths::to_host_fs translates back at access sites.
            let win_path = format!(
                "{profile_win}\\.giga\\configs\\{project}\\inbox",
                project = cfg.project.name,
            );
            // On WSL, store the /mnt/c/... form so init's mkdir +
            // the sync rsync paths work without further translation.
            // On native Windows, store the Windows-form (no
            // translation needed; fs_paths handles cross-FS use).
            if cfg!(target_os = "windows") {
                cfg.paths.windows_inbox = Some(PathBuf::from(win_path));
            } else if let Some(wsl_form) = crate::fs_paths::windows_to_wsl(&win_path) {
                cfg.paths.windows_inbox = Some(PathBuf::from(wsl_form));
            }
        }
        // If we can't resolve a Windows userprofile (e.g., pure Linux
        // without WSL interop), leave windows_inbox as None. Pure
        // WSL-only swarms don't need it; mixed-platform swarms on
        // Linux/macOS without interop should set it explicitly.
    }
}

/// Resolve the Windows-side `%USERPROFILE%` path (e.g.,
/// `C:\Users\Alice`) without assuming the host platform.
///
/// On native Windows: reads `USERPROFILE` env var directly.
/// On WSL: shells out to `cmd.exe /c echo %USERPROFILE%` via interop
/// and parses the result, trimming trailing CR/LF that Windows tools
/// add. Returns None if interop is unavailable or fails.
fn resolve_windows_userprofile() -> Option<String> {
    // Reads %USERPROFILE% directly on native Windows, or via cmd.exe
    // interop from a WSL distro. Shared impl in foundation::proc.
    crate::foundation::proc::cmd_exe_echo("USERPROFILE")
}

impl Config {
    /// Read a config from disk and validate it semantically.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing TOML config at {}", path.display()))?;
        // v0.3.7 Bug 1 fix: callers commonly pass a symlinked path
        // (e.g., workdirs/<agent>/giga-harness.toml -> swarm/giga-harness.toml).
        // sibling lookups (this_host.toml) must resolve relative to the
        // target's directory, NOT the symlink's parent — otherwise the
        // sibling is silently missing and downstream code degrades to
        // "no this_host" with a confusing 0-channels-tracked watcher.
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        cfg.this_host = load_this_host(&canonical)?;
        cfg.source_path = Some(canonical.clone());
        // v0.6.24: auto-default `paths.wsl_inbox` and `paths.windows_inbox`
        // when omitted. Lets new swarms drop the explicit `[paths]` block
        // entirely; both default to a swarm-config-relative location.
        // Existing swarms with explicit values are unchanged (the
        // explicit value wins).
        apply_path_defaults(&mut cfg, &canonical);
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
        let mut seen_host_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
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

        // v0.3.8 Bug 4 fix: when [[hosts]] is non-empty, every agent
        // MUST have an explicit `host = "..."` field. Pre-fix:
        // `agent_host()` falls back to this_host for host-less agents,
        // which means the SAME canonical TOML reads differently on each
        // host — agents collapse to "wherever I am running", silently
        // misrouting channels. The user's bootstrap report (Bug 4)
        // showed `giga hosts` reporting all 5 agents as local on each
        // of 2 hosts. Require explicit-host now; remediation is to
        // add `host = "<host-name>"` to each agent block.
        if !self.hosts.is_empty() {
            let host_names: std::collections::HashSet<&str> =
                self.hosts.iter().map(|h| h.name.as_str()).collect();
            let unhosted: Vec<&str> = self
                .agents
                .iter()
                .filter(|a| a.host.is_none())
                .map(|a| a.name.as_str())
                .collect();
            if !unhosted.is_empty() {
                return Err(anyhow!(
                    "agents missing `host = \"...\"` in a multi-host swarm: {:?}. \
                     When [[hosts]] is non-empty, every [[agents]] block must declare \
                     which host it runs on (one of: {:?}). The pre-v0.3.8 fallback to \
                     this_host silently misrouted channels on the peer.",
                    unhosted,
                    host_names.iter().copied().collect::<Vec<_>>(),
                ));
            }
        }

        // v0.3.6: at most one swarm_boss per host. Bucketed by the
        // resolved host (agent.host or this_host fallback). See
        // SWARM_BOSS_DESIGN.md §3.2.
        let mut bosses_per_host: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for a in &self.agents {
            if !a.swarm_boss {
                continue;
            }
            if a.platform != "wsl" {
                return Err(anyhow!(
                    "agent `{}` is flagged swarm_boss but has platform `{}` — \
                     sync + merger are POSIX-only; swarm_boss must be platform=\"wsl\"",
                    a.name,
                    a.platform,
                ));
            }
            // Resolve host: explicit `agent.host`, else this_host, else
            // "<local>" sentinel (legacy local-only swarm).
            let host_key = a
                .host
                .as_deref()
                .or(self.this_host.as_deref())
                .unwrap_or("<local>");
            bosses_per_host
                .entry(host_key)
                .or_default()
                .push(a.name.as_str());
        }
        for (host, names) in &bosses_per_host {
            if names.len() > 1 {
                return Err(anyhow!(
                    "multiple agents flagged as swarm_boss on host `{}`: {:?}. At most one per host.",
                    host,
                    names,
                ));
            }
        }

        Ok(())
    }

    /// The effective host for an agent: explicit `agent.host` if set,
    /// otherwise `this_host` (the local machine). Returns `None` only
    /// when neither is known — local-only mode pre-remote-channels.
    pub fn agent_host<'a>(&'a self, agent: &'a Agent) -> Option<&'a str> {
        agent.host.as_deref().or(self.this_host.as_deref())
    }

    /// v0.6.0: resolve which runtime an agent uses. Priority:
    /// explicit `agent.runtime` → `project.runtime` → `Runtime::Claude`
    /// (backward compat default).
    pub fn agent_runtime(&self, agent: &Agent) -> crate::runtime::Runtime {
        agent.runtime.or(self.project.runtime).unwrap_or_default()
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

    /// v0.3.9 Bug 5b: load_this_host prefers the new `.local.toml`
    /// name. Reader still accepts the legacy `this_host.toml` for
    /// backward compat with v0.3.8 and earlier swarms.
    #[test]
    fn load_prefers_this_host_local_toml_over_legacy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(
            &cfg_path,
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-new"
tailnet_hostname = "host-new.tail0.ts.net"
[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
host = "host-new"
"#,
        )
        .unwrap();
        // Both files present, different values — the new name wins.
        std::fs::write(
            tmp.path().join("this_host.local.toml"),
            "this_host = \"host-new\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-legacy\"\n",
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        // Wait — cfg validation requires this_host to be in [[hosts]].
        // host-legacy isn't in [[hosts]], so if the legacy file won,
        // validation would fail. The fact that load succeeded with
        // host-new in [[hosts]] proves the new name was picked.
        assert_eq!(cfg.this_host.as_deref(), Some("host-new"));
    }

    /// v0.3.9 Bug 5b: legacy `this_host.toml` is still accepted when
    /// `this_host.local.toml` is absent — backward compat for v0.3.8
    /// and earlier swarms that haven't been migrated.
    #[test]
    fn load_falls_back_to_legacy_this_host_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(
            &cfg_path,
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-x"
tailnet_hostname = "host-x.tail0.ts.net"
[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
host = "host-x"
"#,
        )
        .unwrap();
        // Only legacy file present.
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-x\"\n",
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.this_host.as_deref(), Some("host-x"));
    }

    /// v0.3.7 Bug 1 fix: when the config is loaded via a symlink (the
    /// canonical case for agents whose workdir contains a symlink to
    /// the swarm-dir TOML), this_host.toml MUST be found relative to
    /// the symlink's TARGET, not its parent. Pre-fix: agents armed
    /// v0.6.24: when [paths].wsl_inbox is omitted, default to
    /// <config_dir>/inbox so new swarms can drop the [paths] block
    /// entirely.
    #[test]
    fn load_defaults_wsl_inbox_to_config_dir_inbox_when_omitted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        // NOTE: no [paths] block at all.
        let body = r#"
[project]
name = "t"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#;
        std::fs::write(&cfg_path, body).unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        let expected = tmp.path().canonicalize().unwrap().join("inbox");
        assert_eq!(cfg.paths.wsl_inbox.as_ref().unwrap(), &expected);
    }

    /// v0.6.24: explicit [paths].wsl_inbox always wins over the default.
    /// Critical for existing swarms — they shouldn't get silently
    /// migrated to a new inbox location on first load.
    #[test]
    fn load_explicit_wsl_inbox_overrides_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        let explicit = "/some/explicit/inbox/path";
        let body = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = "{explicit}"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
        );
        std::fs::write(&cfg_path, body).unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(
            cfg.paths.wsl_inbox.as_ref().unwrap(),
            std::path::Path::new(explicit),
        );
    }

    /// `giga watch` from their workdir, config reload silently failed
    /// because workdir/this_host.toml didn't exist, watcher tracked 0
    /// channels, looked alive in Monitor but produced no events.
    #[cfg(unix)]
    #[test]
    fn load_resolves_this_host_relative_to_symlink_target() {
        let swarm = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();

        // Real config + this_host.toml live in swarm/
        let real_config = swarm.path().join("giga-harness.toml");
        std::fs::write(
            &real_config,
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"
[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "host-a"
"#,
        )
        .unwrap();
        std::fs::write(
            swarm.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();

        // Workdir has a symlink to the real config.
        let symlink = workdir.path().join("giga-harness.toml");
        std::os::unix::fs::symlink(&real_config, &symlink).unwrap();
        // Crucially, NO this_host.toml in the workdir — only the
        // symlink target's directory has it.
        assert!(!workdir.path().join("this_host.toml").exists());

        // Load via the symlink. Should resolve to swarm/this_host.toml.
        let cfg = Config::load(&symlink).unwrap();
        assert_eq!(
            cfg.this_host.as_deref(),
            Some("host-a"),
            "this_host must be resolved via the symlink target, not its parent"
        );
    }

    /// v0.3.6 S1 (SWARM_BOSS_DESIGN.md): two agents both flagged
    /// swarm_boss on the same host is a validation error. Mirrors
    /// the bench_scheduler-uniqueness rule.
    #[test]
    fn rejects_multiple_swarm_bosses_on_same_host() {
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
swarm_boss = true"#,
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
swarm_boss = true"#,
            );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err
            .to_string()
            .contains("multiple agents flagged as swarm_boss"));
    }

    /// v0.3.6 S2: one boss per host across a multi-host swarm is OK.
    #[test]
    fn accepts_one_swarm_boss_per_host_on_multi_host_swarm() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "boss-a"
workdir = "/h/boss-a"
role = "."
platform = "wsl"
host = "host-a"
swarm_boss = true

[[agents]]
name = "boss-b"
workdir = "/h/boss-b"
role = "."
platform = "wsl"
host = "host-b"
swarm_boss = true
"#;
        // Need a this_host or validation rejects [[hosts]] without it.
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, body).unwrap();
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.agents.iter().filter(|a| a.swarm_boss).count(), 2);
    }

    #[test]
    fn rejects_swarm_boss_on_windows_platform() {
        let body = minimal().replace(
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "windows"
swarm_boss = true"#,
        );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(
            err.to_string().contains("swarm_boss") && err.to_string().contains("POSIX-only"),
            "got: {err}",
        );
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

    /// v0.3.8 Bug 4 fix (inverted from the v0.2 fallback test): when
    /// [[hosts]] is non-empty, agents MUST have an explicit `host =`
    /// field. The old fallback to this_host silently misrouted
    /// channels because the SAME canonical TOML resolved differently
    /// on each host. Validation now rejects the missing-host case.
    #[test]
    fn agent_without_host_field_in_multi_host_swarm_is_rejected() {
        let body = minimal_two_host().replace(r#"host = "wsl-a""#, "");
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        let err = cfg.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("missing `host") && msg.contains("alice"),
            "expected multi-host explicit-host error; got: {msg}"
        );
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
        assert!(
            cfg.channel_is_local(ch),
            "alice is on wsl-a (this_host) -> local fast-path"
        );
    }

    #[test]
    fn channel_is_not_local_when_participants_span_hosts() {
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let ch = &cfg.channels[0];
        assert!(
            !cfg.channel_is_local(ch),
            "alice@wsl-a + bob@wsl-b spans hosts -> slice path"
        );
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
ssh_user = "alice""#,
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        assert_eq!(cfg.hosts[0].ssh_user.as_deref(), Some("alice"));
        assert_eq!(cfg.hosts[1].ssh_user, None); // omitted defaults to None
    }

    // -------------------------------------------------------------------
    // v0.3.2: per-host [paths] override (quality finding 1 — a peer host
    // had different $HOME than operator, init failed on literal path).
    // -------------------------------------------------------------------

    #[test]
    fn inbox_for_host_side_uses_per_host_paths_override() {
        // v0.3.8: agents need explicit host= in multi-host swarms.
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"
paths.wsl_inbox = "/home/alice/projects/inbox"
"#,
            minimal()
                .replace(
                    "platform = \"wsl\"\n\n[[agents]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[agents]]",
                )
                .replace(
                    "platform = \"wsl\"\n\n[[channels]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[channels]]",
                )
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
            Some(PathBuf::from("/home/alice/projects/inbox"))
        );
    }

    #[test]
    fn inbox_for_host_side_falls_back_to_remote_inbox_dir_v02_compat() {
        // v0.2 swarms used [[hosts]].remote_inbox_dir before the
        // explicit per-host [paths] field existed. Keep working.
        // v0.3.8: agents need explicit host=.
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
                .replace(
                    "platform = \"wsl\"\n\n[[agents]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[agents]]",
                )
                .replace(
                    "platform = \"wsl\"\n\n[[channels]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[channels]]",
                )
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
        assert_eq!(
            cfg.inbox_for_host_side(None, "wsl"),
            Some(PathBuf::from("/tmp/i"))
        );
    }

    #[test]
    fn channel_path_uses_per_host_override_when_this_host_set() {
        // The killer test: a peer with asymmetric paths must NOT try to
        // resolve channels against the operator's local-only inbox.
        // v0.3.8: add explicit host= to the agents from minimal().
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "b.tail0.ts.net"
paths.wsl_inbox = "/home/alice/projects/inbox"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["a", "b"]
"#,
            minimal()
                .trim_end_matches(|c: char| c == '\n')
                .replace(
                    "platform = \"wsl\"\n\n[[agents]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[agents]]",
                )
                .replace(
                    "platform = \"wsl\"\n\n[[channels]]",
                    "platform = \"wsl\"\nhost = \"wsl-b\"\n\n[[channels]]",
                )
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-b".into());
        cfg.validate().unwrap();
        let ch = cfg
            .channels
            .iter()
            .find(|c| c.file == "alice-bob.md")
            .unwrap();
        let path = cfg.channel_path(ch).unwrap();
        assert!(
            path.starts_with("/home/alice/projects/inbox"),
            "channel_path on wsl-b should use wsl-b's override, got {}",
            path.display()
        );
    }

    // ----- v0.4.0 broadcast fanout: parser + slot computation ---------

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

    #[test]
    fn broadcast_config_stagger_zero_disables_fanout_delay() {
        assert_eq!(fanout_delay_seconds("alice", &["alice", "bob"], 0), 0);
        assert_eq!(fanout_delay_seconds("bob", &["alice", "bob"], 0), 0);
    }

    #[test]
    /// v0.6.3: `[giga-rearm]` triggers the silent watcher self-rearm
    /// path. parse_broadcast_prefix returns the new variant so
    /// watch.rs can dispatch on it.
    #[test]
    fn parse_broadcast_prefix_recognizes_giga_rearm() {
        assert_eq!(
            parse_broadcast_prefix("[giga-rearm] giga upgraded"),
            Some(BroadcastPrefix::GigaRearm)
        );
        assert_eq!(
            parse_broadcast_prefix("[GIGA-REARM] case insensitive"),
            Some(BroadcastPrefix::GigaRearm)
        );
        // After timestamp wrapper.
        assert_eq!(
            parse_broadcast_prefix("[design 2026-06-02 12:00 PST] [giga-rearm] please"),
            Some(BroadcastPrefix::GigaRearm),
        );
    }

    #[test]
    fn parse_broadcast_prefix_recognizes_fyi() {
        assert_eq!(
            parse_broadcast_prefix("[fyi] host-c came online"),
            Some(BroadcastPrefix::Fyi)
        );
        assert_eq!(
            parse_broadcast_prefix("[FYI] case insensitive"),
            Some(BroadcastPrefix::Fyi)
        );
        assert_eq!(
            parse_broadcast_prefix("  [ fyi ]  whitespace tolerant"),
            Some(BroadcastPrefix::Fyi)
        );
    }

    #[test]
    fn parse_broadcast_prefix_recognizes_ack_list() {
        let parsed = parse_broadcast_prefix("[ack: alice, bob, carol] cleanup nudge");
        match parsed {
            Some(BroadcastPrefix::Ack(list)) => {
                assert_eq!(list, vec!["alice", "bob", "carol"]);
            }
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn parse_broadcast_prefix_recognizes_all() {
        assert_eq!(
            parse_broadcast_prefix("[all] hello everyone"),
            Some(BroadcastPrefix::All)
        );
    }

    #[test]
    fn parse_broadcast_prefix_returns_none_for_unprefixed() {
        assert_eq!(parse_broadcast_prefix("plain subject no brackets"), None);
        assert_eq!(parse_broadcast_prefix("[unknown-tag] something"), None);
    }

    #[test]
    fn parse_broadcast_prefix_skips_timestamp_header() {
        // The convention from CLAUDE.md is "[<agent> YYYY-MM-DD HH:MM PST]".
        // The parser must skip past that to find the broadcast prefix.
        let parsed =
            parse_broadcast_prefix("[design 2026-06-01 12:00 PST] [ack: alice] cleanup nudge");
        match parsed {
            Some(BroadcastPrefix::Ack(list)) => assert_eq!(list, vec!["alice"]),
            other => panic!("expected Ack after timestamp prefix, got {other:?}"),
        }
    }

    #[test]
    fn parse_broadcast_prefix_handles_fyi_after_timestamp() {
        assert_eq!(
            parse_broadcast_prefix("[design 2026-06-01 12:00 PST] [fyi] foo"),
            Some(BroadcastPrefix::Fyi),
        );
    }

    #[test]
    fn parse_broadcast_prefix_empty_ack_list_yields_empty_vec() {
        let parsed = parse_broadcast_prefix("[ack: ] empty list");
        match parsed {
            Some(BroadcastPrefix::Ack(list)) => assert!(list.is_empty()),
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn is_broadcast_channel_matches_underscore_prefix() {
        assert!(is_broadcast_channel("_broadcast.md"));
        assert!(is_broadcast_channel("_announcements.md"));
        assert!(!is_broadcast_channel("alice-bob.md"));
        assert!(!is_broadcast_channel("broadcast.md"));
        assert!(!is_broadcast_channel("_broadcast.txt"));
    }

    #[test]
    fn fanout_delay_assigns_stable_slots() {
        let agents = ["bob", "alice", "carol"];
        // Sorted: alice (0), bob (1), carol (2). Stagger 10s.
        assert_eq!(fanout_delay_seconds("alice", &agents, 10), 0);
        assert_eq!(fanout_delay_seconds("bob", &agents, 10), 10);
        assert_eq!(fanout_delay_seconds("carol", &agents, 10), 20);
    }

    #[test]
    fn fanout_delay_for_unknown_agent_defaults_to_zero_slot() {
        let agents = ["alice", "bob"];
        // Unknown agent gets slot 0 (no delay) — conservative; the
        // caller already filtered the recipient list.
        assert_eq!(fanout_delay_seconds("eve", &agents, 10), 0);
    }
}
