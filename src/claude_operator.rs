//! `giga claude-operator` — dual-mode operator help for Claude.
//!
//! TTY-aware behavior in one subcommand (no `--print` flag needed):
//!
//!   Stdout is a TTY (operator at a terminal):
//!     Launches `claude --append-system-prompt <DOC>` so the human-driven
//!     Claude session boots with the giga operator command surface
//!     loaded as system prompt. Drop-into-Claude one-shot.
//!
//!   Stdout is NOT a TTY (agent's Bash tool, redirect, pipe):
//!     Prints the doc to stdout. An agent invoking this via Bash
//!     captures the text into their conversation context — same effect
//!     as the operator, just for an already-running Claude session.
//!
//! The doc is `templates/CLAUDE_OPERATOR.md`, baked into the binary at
//! compile time with `include_str!`. No network, no stale-URL risk,
//! version-locked to the giga binary.
//!
//! Usage on the operator host:
//!   giga claude-operator                  # drop into Claude with the doc loaded
//!   giga claude-operator | less           # just view the doc
//!   giga claude-operator > op.md          # save the doc to a file
//!
//! Usage by an agent (via Bash tool):
//!   giga claude-operator                  # stdout captured -> doc enters context

use std::io::IsTerminal;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

const DOC: &str = include_str!("../templates/CLAUDE_OPERATOR.md");

pub fn run() -> Result<()> {
    if std::io::stdout().is_terminal() {
        // Operator path: launch Claude with the doc as a system prompt
        // suffix. Inherits stdin/stdout/stderr so the interactive REPL
        // works normally; the doc just shapes what Claude knows.
        if which::which("claude").is_err() {
            // Don't fail silently if claude isn't installed — that's
            // the common confusion mode for a first-time operator.
            return Err(anyhow!(
                "claude CLI not found on PATH. Install Claude Code first \
                 (https://claude.com/claude-code), then retry. \
                 (If you just want the doc text, pipe to anything: \
                 `giga claude-operator | less`)"
            ));
        }
        let status = Command::new("claude")
            .arg("--append-system-prompt")
            .arg(DOC)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("invoking claude")?;
        std::process::exit(status.code().unwrap_or(0));
    } else {
        // Agent path (or anything non-interactive): print the raw doc.
        print!("{DOC}");
        Ok(())
    }
}
