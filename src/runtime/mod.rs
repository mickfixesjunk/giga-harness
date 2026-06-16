//! v0.6.0: per-runtime support for the swarm.
//!
//! Giga today is Claude Code-specific in three places: launch command
//! defaults, the watcher delivery model (Claude's `Monitor` tool), and
//! the per-agent instruction snippet baked into `AGENTS.md`. v0.6.0
//! abstracts those three behind a `Runtime` enum so swarms can mix
//! Claude / Codex CLI / Antigravity (`agy`) agents on the same channels.
//!
//! Filename is universal — every agent's workdir gets a single
//! `AGENTS.md`, not per-runtime `CLAUDE.md` / `AGENTS.md` / `GEMINI.md`.
//! Modern Claude Code reads `AGENTS.md` alongside `CLAUDE.md` (the
//! cross-runtime convention is consolidating on `AGENTS.md`), and the
//! launch intro prompt explicitly tells the agent to read `AGENTS.md`
//! at session start, so even older Claude versions pick it up.
//!
//! What varies per runtime:
//!   - Launch command default: `claude -c` / `codex` / `agy`
//!   - Session Start snippet in the generated AGENTS.md (Monitor vs
//!     run_command-background vs separate-bridge-pane)
//!   - Watcher mode (default stdout / `--agy` exit-on-WAITING-ON / `--codex` envelope-bridge)
//!   - Pane count per agent on launch (1 for claude/agy; 2 for codex —
//!     the CLI + a separate bridge pane running `giga watch --codex`)

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub mod agy;
pub mod claude;
pub mod codex;

/// Which agent runtime this swarm or this individual agent uses.
/// Default is `Claude` for backward compat with every pre-v0.6.0 swarm.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Anthropic Claude Code CLI. Default. Monitor-tool watcher
    /// integration; busy-lock hooks integrate cleanly.
    Claude,
    /// OpenAI Codex CLI. REPL-shaped, no background-task primitive.
    /// Watcher runs as a separate "bridge" pane via `giga watch --codex`
    /// writing JSON envelopes to `$CODEX_CHANNEL_DIR/inbox/`. The codex
    /// CLI consumes the envelopes.
    Codex,
    /// Antigravity CLI (`agy`). Has a reactive-wakeup background-task
    /// primitive — watcher runs INSIDE the agent's session via the
    /// `run_command` tool with a small `WaitMsBeforeAsync` to detach
    /// (NOT a `background=true` parameter; that isn't in agy's tool
    /// schema), running `giga watch --agy` (exits 0 on WAITING ON me,
    /// which triggers AGY's task-completion wakeup).
    Agy,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime::Claude
    }
}

impl Runtime {
    /// Short stable identifier — used in logs, error messages, and
    /// TOML serialization (matches the `#[serde(rename_all = "lowercase")]`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Runtime::Claude => "claude",
            Runtime::Codex => "codex",
            Runtime::Agy => "agy",
        }
    }

    /// Parse from a TOML string. Used by `parse_runtime_opt` to coerce
    /// optional fields without forcing every caller to depend on serde
    /// internals.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(Runtime::Claude),
            "codex" => Some(Runtime::Codex),
            "agy" | "antigravity" => Some(Runtime::Agy),
            _ => None,
        }
    }

    /// Watcher invocation for this runtime — the command the runtime's
    /// Session Start template tells the agent (or operator pane) to
    /// run. For Claude: stdout-based Monitor. For Agy: --agy mode.
    /// For Codex: --codex mode in a separate pane (the agent's CLI
    /// doesn't see the watcher directly).
    pub fn watcher_invocation(&self, agent_slug: &str) -> String {
        match self {
            Runtime::Claude => format!("giga watch --as {agent_slug}"),
            Runtime::Agy => format!("giga watch --as {agent_slug} --agy"),
            Runtime::Codex => format!("giga watch --as {agent_slug} --codex"),
        }
    }

    /// True when this runtime needs a separate "bridge" tmux pane
    /// alongside the CLI pane. Codex is the only one today — its CLI
    /// has no background-task primitive so the watcher must run in a
    /// sidecar process that drops envelopes into the codex inbox dir.
    pub fn needs_bridge_pane(&self) -> bool {
        matches!(self, Runtime::Codex)
    }

    /// Number of tmux panes this runtime's agent occupies on launch:
    /// 2 for runtimes that need a separate bridge pane (codex), 1
    /// otherwise (claude / agy run the watcher in-session). The launcher
    /// branches on `needs_bridge_pane` directly today; this convenience
    /// accessor is consumed in a later phase.
    #[allow(dead_code)]
    pub fn pane_count(&self) -> u8 {
        if self.needs_bridge_pane() {
            2
        } else {
            1
        }
    }

    /// The instruction snippet for this runtime's `AGENTS.md` Session
    /// Start section. Pulled from `templates/runtimes/<runtime>.md` at
    /// compile time via `include_str!`. The snippet is text that gets
    /// rendered with `{{AGENT}}` replaced by the agent's slug.
    pub fn session_start_snippet(&self) -> &'static str {
        match self {
            Runtime::Claude => claude::SESSION_START,
            Runtime::Codex => codex::SESSION_START,
            Runtime::Agy => agy::SESSION_START,
        }
    }

    /// Default opening prompt sent to this runtime's CLI on `giga
    /// launch`. Pulled from `templates/runtimes/<runtime>-intro.md` at
    /// compile time. Per-project override lives at
    /// `[project].launch_intro_prompt` in TOML — when set, that wins
    /// for all agents regardless of runtime.
    ///
    /// IMPORTANT: these strings end up single-quoted on a shell command
    /// line (wt.exe → wsl.exe → bash hop). Backticks in the file would
    /// survive single-quoting and get shell-evaluated as command
    /// substitution, corrupting the prompt the agent actually sees.
    /// Keep the intro files plain prose — no code spans, no fences.
    pub fn launch_intro_prompt(&self) -> &'static str {
        match self {
            Runtime::Claude => claude::INTRO,
            Runtime::Codex => codex::INTRO,
            Runtime::Agy => agy::INTRO,
        }
    }

    /// Locate the most-recent CLI session log for this runtime that
    /// corresponds to `workdir`. Best-effort: returns None if the
    /// runtime doesn't keep per-cwd logs, we can't resolve the home
    /// directory, or we can't find the conventional path.
    pub fn session_log(&self, workdir: &Path) -> Option<PathBuf> {
        let home = crate::foundation::dirs::home_dir()?;
        match self {
            Runtime::Claude => claude::session_log(&home, workdir),
            Runtime::Codex => codex::session_log(&home, workdir),
            Runtime::Agy => agy::session_log(&home, workdir),
        }
    }
}

/// Return the most-recently-modified `*.jsonl` under `dir`, or None
/// if the dir doesn't exist or contains no jsonl files. Shared by the
/// per-runtime session-log locators.
pub(crate) fn most_recent_jsonl(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, p));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_canonical_names() {
        assert_eq!(Runtime::parse("claude"), Some(Runtime::Claude));
        assert_eq!(Runtime::parse("codex"), Some(Runtime::Codex));
        assert_eq!(Runtime::parse("agy"), Some(Runtime::Agy));
    }

    #[test]
    fn parse_accepts_antigravity_alias_for_agy() {
        assert_eq!(Runtime::parse("antigravity"), Some(Runtime::Agy));
        assert_eq!(Runtime::parse("Antigravity"), Some(Runtime::Agy));
    }

    #[test]
    fn parse_is_case_insensitive_and_trim_tolerant() {
        assert_eq!(Runtime::parse("  CLAUDE  "), Some(Runtime::Claude));
        assert_eq!(Runtime::parse("Codex"), Some(Runtime::Codex));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(Runtime::parse("gemini"), None);
        assert_eq!(Runtime::parse(""), None);
    }

    #[test]
    fn default_is_claude_for_backward_compat() {
        assert_eq!(Runtime::default(), Runtime::Claude);
    }

    #[test]
    fn as_str_matches_serde_lowercase_convention() {
        assert_eq!(Runtime::Claude.as_str(), "claude");
        assert_eq!(Runtime::Codex.as_str(), "codex");
        assert_eq!(Runtime::Agy.as_str(), "agy");
    }

    #[test]
    fn needs_bridge_pane_only_codex() {
        assert!(!Runtime::Claude.needs_bridge_pane());
        assert!(!Runtime::Agy.needs_bridge_pane());
        assert!(Runtime::Codex.needs_bridge_pane());
    }

    #[test]
    fn watcher_invocation_includes_runtime_flag() {
        assert_eq!(
            Runtime::Claude.watcher_invocation("alice"),
            "giga watch --as alice"
        );
        assert_eq!(
            Runtime::Agy.watcher_invocation("alice"),
            "giga watch --as alice --agy"
        );
        assert_eq!(
            Runtime::Codex.watcher_invocation("alice"),
            "giga watch --as alice --codex"
        );
    }

    #[test]
    fn session_start_snippet_is_non_empty_per_runtime() {
        // Just confirm the include_str! pointed at real files. If any
        // snippet path is wrong this fails at compile time, but the
        // test guards against accidentally emptying the file.
        for r in [Runtime::Claude, Runtime::Codex, Runtime::Agy] {
            let body = r.session_start_snippet();
            assert!(
                !body.trim().is_empty(),
                "{} snippet must not be empty",
                r.as_str()
            );
            assert!(
                body.contains("{{AGENT}}"),
                "{} snippet must use {{AGENT}} placeholder",
                r.as_str()
            );
        }
    }

    #[test]
    fn agy_snippet_uses_correct_tool_signatures() {
        // Regression test for v0.6.11 — agy's AGENTS.md previously
        // documented two fictitious things that crash the real Agy
        // CLI on use:
        //   1. `giga sweep --as <slug>` — there is no --as flag on
        //      sweep; the real flag is --owed-by <slug>.
        //   2. `run_command("...", background=true)` — Agy's actual
        //      run_command tool doesn't take a `background` parameter;
        //      detachment is via `WaitMsBeforeAsync`.
        // An operator's coder agent caught both bugs on first session.
        let body = Runtime::Agy.session_start_snippet();
        assert!(
            !body.contains("sweep --as"),
            "agy snippet still recommends `sweep --as <slug>` (no such flag — use --owed-by):\n{body}",
        );
        assert!(
            !body.contains("background=true") || body.contains("not a supported parameter"),
            "agy snippet recommends background=true to run_command (Agy's schema doesn't have that):\n{body}",
        );
        assert!(
            body.contains("--owed-by"),
            "agy snippet must reference the correct sweep flag --owed-by:\n{body}",
        );
        assert!(
            body.contains("WaitMsBeforeAsync"),
            "agy snippet must reference WaitMsBeforeAsync for run_command backgrounding:\n{body}",
        );
    }

    #[test]
    fn launch_intro_prompt_is_runtime_specific_and_safe() {
        let claude = Runtime::Claude.launch_intro_prompt();
        let codex = Runtime::Codex.launch_intro_prompt();
        let agy = Runtime::Agy.launch_intro_prompt();
        // Distinct strings — no accidental sharing of file paths.
        assert_ne!(claude, codex);
        assert_ne!(claude, agy);
        assert_ne!(codex, agy);
        // Filename consolidation (v0.6.0): never CLAUDE.md.
        for (name, s) in [("claude", claude), ("codex", codex), ("agy", agy)] {
            assert!(!s.trim().is_empty(), "{name} intro must not be empty");
            assert!(
                !s.contains("CLAUDE.md"),
                "{name} intro references CLAUDE.md (should be AGENTS.md):\n{s}",
            );
            assert!(
                s.contains("AGENTS.md"),
                "{name} intro should reference AGENTS.md:\n{s}",
            );
            // Backticks survive single-quoting on the wt → wsl → bash
            // hop and get shell-evaluated. Lock this out at the source.
            assert!(
                !s.contains('`'),
                "{name} intro contains a backtick — will be shell-evaluated:\n{s}",
            );
        }
        // Claude-only tools must not leak into other runtimes' intros.
        assert!(
            !codex.contains("Monitor TOOL") && !codex.contains("Bash tool"),
            "codex intro must not reference Claude's Monitor/Bash tools:\n{codex}",
        );
        assert!(
            !agy.contains("Monitor TOOL") && !agy.contains("Bash tool"),
            "agy intro must not reference Claude's Monitor/Bash tools:\n{agy}",
        );
        // Runtime-specific guidance present.
        assert!(
            codex.contains("bridge"),
            "codex intro should mention bridge pane:\n{codex}"
        );
        assert!(
            agy.contains("run_command"),
            "agy intro should mention run_command:\n{agy}"
        );
        // v0.6.10 regression guard: every intro must tell the agent
        // AGENTS.md lives in CWD (relative ./AGENTS.md). Burned on
        // agy/coder 2026-06-02 — agy searched the entire filesystem
        // with find / grep because the intro only said "follow Session
        // Start in AGENTS.md" without saying where AGENTS.md is.
        for (name, s) in [("claude", claude), ("codex", codex), ("agy", agy)] {
            assert!(
                s.contains("./AGENTS.md") || s.contains("cwd"),
                "{name} intro must say AGENTS.md is in cwd:\n{s}",
            );
        }
    }

    /// v0.6.29 regression guard for codex pane-only-output discipline.
    /// Codex CLI built-in slash commands (`/review`, `/diff`, etc.)
    /// produce output in the pane only; agents don't naturally know
    /// to relay that output to the swarm channel via `giga post`.
    /// The codex runtime AGENTS.md snippet must explicitly bind these
    /// commands to a follow-up `giga post`. Burned on superdeduper
    /// 2026-06-07: codex-review's PR #171 and #176 verdicts sat in
    /// the pane for 1-1.5hr each, requiring manual nudges from
    /// design to trigger the post.
    #[test]
    fn codex_snippet_binds_builtin_commands_to_giga_post() {
        let body = Runtime::Codex.session_start_snippet();
        assert!(
            body.contains("/review"),
            "codex snippet must call out /review specifically (most common pane-only failure mode):\n{body}",
        );
        assert!(
            body.contains("pane only") || body.contains("pane-only"),
            "codex snippet must explain the pane-only failure mode:\n{body}",
        );
        assert!(
            body.contains("giga post"),
            "codex snippet must instruct the follow-up giga post:\n{body}",
        );
    }

    /// v0.6.28 regression guard for the VM-reboot Monitor revival
    /// hole: a WSL VM reboot kills every Monitor process but Claude
    /// Code preserves the agent's conversation history, so the agent
    /// boots looking mid-task and skips Session Start, never re-arming
    /// Monitor and idling silently. Claude's intro must instruct the
    /// agent to call TaskList BEFORE deciding to resume — if no
    /// Monitor entries surface, every Monitor is dead and must be
    /// re-armed silently before any other work.
    #[test]
    fn claude_intro_guards_vm_reboot_monitor_revival() {
        let claude = Runtime::Claude.launch_intro_prompt();
        assert!(
            claude.contains("TaskList"),
            "Claude intro must reference TaskList check for VM-reboot recovery:\n{claude}",
        );
        assert!(
            claude.contains("silently"),
            "Claude intro must instruct silent re-arm so the agent doesn't \
             announce the re-arm into the conversation:\n{claude}",
        );
    }
}
