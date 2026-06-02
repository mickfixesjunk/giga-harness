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

use serde::Deserialize;

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
    /// primitive — watcher runs INSIDE the agent's session via
    /// `run_command(background=true)` with `giga watch --agy` (exits 0
    /// on WAITING ON me, which triggers AGY's task-completion wakeup).
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

    /// Default launch command for this runtime. The agent's tmux pane
    /// runs this when the agent is spawned by `giga launch`. Operators
    /// can override per-agent via `agent.launch_cmd` in TOML.
    ///
    /// Returns the COMMAND string only — not platform-wrapped. The
    /// launch module wraps it in `bash -lc` / `powershell` / etc. as
    /// appropriate for the agent's platform.
    pub fn default_launch_cmd(&self, intro: &str, model: &str) -> String {
        let sh_intro = shell_escape::unix::escape(std::borrow::Cow::Borrowed(intro));
        let sh_model = shell_escape::unix::escape(std::borrow::Cow::Borrowed(model));
        match self {
            Runtime::Claude => format!(
                "command -v claude >/dev/null && \
                 {{ claude -c --model {sh_model} {sh_intro} || claude --model {sh_model} {sh_intro} ; }} || true",
            ),
            Runtime::Codex => {
                // codex CLI doesn't take a --model flag the same way;
                // it reads its own profile. We pass the intro on stdin
                // (codex reads stdin for the initial prompt) but mostly
                // rely on AGENTS.md for the agent's instructions. The
                // intro is appended as a single startup message.
                format!(
                    "command -v codex >/dev/null && \
                     echo {sh_intro} | codex || true",
                )
            }
            Runtime::Agy => {
                // agy is the antigravity CLI. Similar shape to codex —
                // we pass the intro as a startup prompt. Specifics may
                // need adjustment once we live-test against agy.
                format!(
                    "command -v agy >/dev/null && \
                     echo {sh_intro} | agy || true",
                )
            }
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

    /// The instruction snippet for this runtime's `AGENTS.md` Session
    /// Start section. Pulled from `templates/runtimes/<runtime>.md` at
    /// compile time via `include_str!`. The snippet is text that gets
    /// rendered with `{{AGENT}}` replaced by the agent's slug.
    pub fn session_start_snippet(&self) -> &'static str {
        match self {
            Runtime::Claude => include_str!("../templates/runtimes/claude.md"),
            Runtime::Codex => include_str!("../templates/runtimes/codex.md"),
            Runtime::Agy => include_str!("../templates/runtimes/agy.md"),
        }
    }
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
        assert_eq!(Runtime::Claude.watcher_invocation("alice"), "giga watch --as alice");
        assert_eq!(Runtime::Agy.watcher_invocation("alice"), "giga watch --as alice --agy");
        assert_eq!(Runtime::Codex.watcher_invocation("alice"), "giga watch --as alice --codex");
    }

    #[test]
    fn session_start_snippet_is_non_empty_per_runtime() {
        // Just confirm the include_str! pointed at real files. If any
        // snippet path is wrong this fails at compile time, but the
        // test guards against accidentally emptying the file.
        for r in [Runtime::Claude, Runtime::Codex, Runtime::Agy] {
            let body = r.session_start_snippet();
            assert!(!body.trim().is_empty(), "{} snippet must not be empty", r.as_str());
            assert!(body.contains("{{AGENT}}"), "{} snippet must use {{AGENT}} placeholder", r.as_str());
        }
    }
}
