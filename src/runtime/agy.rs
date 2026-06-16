//! Antigravity CLI (`agy`) runtime: AGENTS.md Session Start snippet,
//! launch intro prompt, and prior-session-log location.

use std::path::{Path, PathBuf};

/// AGENTS.md Session Start snippet for Agy agents. Rendered with
/// `{{AGENT}}` replaced by the agent's slug.
pub const SESSION_START: &str = include_str!("../../templates/runtimes/agy.md");

/// Default opening prompt sent to the Agy CLI on `giga launch`.
pub const INTRO: &str = include_str!("../../templates/runtimes/agy-intro.md");

/// Agy (Antigravity / Gemini CLI) keeps a global rolling history at
/// `~/.gemini/antigravity-cli/history.jsonl`. There's no per-workdir
/// subdir today (verified against the coder agent's session on
/// 2026-06-03). We point at the global file; the new agent can grep
/// for cwd-relevant lines.
pub fn session_log(home: &Path, _workdir: &Path) -> Option<PathBuf> {
    let p = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("history.jsonl");
    p.exists().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn locate_agy_session_finds_global_history() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();
        let agy_dir = tmp_home.path().join(".gemini").join("antigravity-cli");
        fs::create_dir_all(&agy_dir).unwrap();
        let hist = agy_dir.join("history.jsonl");
        let mut f = fs::File::create(&hist).unwrap();
        writeln!(f, r#"{{"event":"hi"}}"#).unwrap();
        let picked = session_log(tmp_home.path(), workdir.path()).unwrap();
        assert_eq!(picked, hist);
    }
}
