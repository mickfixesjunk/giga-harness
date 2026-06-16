//! Codex CLI runtime: AGENTS.md Session Start snippet, launch intro
//! prompt, and prior-session-log location.

use std::path::{Path, PathBuf};

/// AGENTS.md Session Start snippet for Codex agents. Rendered with
/// `{{AGENT}}` replaced by the agent's slug.
pub const SESSION_START: &str = include_str!("../../templates/runtimes/codex.md");

/// Default opening prompt sent to the Codex CLI on `giga launch`.
pub const INTRO: &str = include_str!("../../templates/runtimes/codex-intro.md");

/// Codex stores sessions under (best-effort guesses) `~/.codex/sessions/`
/// or `~/.codex/projects/<encoded>/`. We return the most-recent file in
/// whichever exists. The exact convention may need correction as Codex
/// evolves.
pub fn session_log(home: &Path, workdir: &Path) -> Option<PathBuf> {
    let canon = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let encoded = canon.to_string_lossy().replace('/', "-");
    for candidate in [
        home.join(".codex").join("projects").join(&encoded),
        home.join(".codex").join("sessions"),
    ] {
        if let Some(p) = super::most_recent_jsonl(&candidate) {
            return Some(p);
        }
    }
    None
}
