//! Claude Code runtime: AGENTS.md Session Start snippet, launch intro
//! prompt, and prior-session-log location.

use std::path::{Path, PathBuf};

/// AGENTS.md Session Start snippet for Claude agents. Rendered with
/// `{{AGENT}}` replaced by the agent's slug.
pub const SESSION_START: &str = include_str!("../../templates/runtimes/claude.md");

/// Default opening prompt sent to the Claude CLI on `giga launch`.
pub const INTRO: &str = include_str!("../../templates/runtimes/claude-intro.md");

/// Claude Code stores sessions under `~/.claude/projects/<encoded>/`
/// where `<encoded>` is the workdir absolute path with BOTH `/` and
/// `.` replaced by `-` (verified empirically against
/// `/home/alice/.giga/configs/.../giga`, which Claude encodes as
/// `-home-alice--giga-configs-...-giga` — the leading `/` becomes
/// a leading `-`, and the `.` in `.giga` becomes the second `-` of
/// the `--giga` sequence). Each session is one `<uuid>.jsonl`. We
/// return the most-recently-modified file under that dir.
pub fn session_log(home: &Path, workdir: &Path) -> Option<PathBuf> {
    let canon = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let encoded: String = canon
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let dir = home.join(".claude").join("projects").join(&encoded);
    super::most_recent_jsonl(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn locate_claude_session_finds_most_recent_jsonl() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();
        let canon = workdir.path().canonicalize().unwrap();
        // Claude encodes both `/` AND `.` to `-`. The tmpdir path
        // typically contains neither `.` nor anything weird, but
        // construct the encoding the same way the locator does so
        // the test exercises the actual path resolution.
        let encoded: String = canon
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        let proj_dir = tmp_home
            .path()
            .join(".claude")
            .join("projects")
            .join(&encoded);
        fs::create_dir_all(&proj_dir).unwrap();
        // Two session files, write second one later so its mtime is
        // newer; the locator should pick it.
        let older = proj_dir.join("aaa.jsonl");
        let newer = proj_dir.join("bbb.jsonl");
        fs::write(&older, "{}\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&newer, "{}\n").unwrap();
        let picked = session_log(tmp_home.path(), workdir.path()).unwrap();
        assert_eq!(picked, newer);
    }

    /// Regression test for the encoding rule. Tempdirs typically
    /// don't have `.` in the path, so the basic test above can pass
    /// even with a buggy encoder. This one explicitly exercises the
    /// `.` → `-` rule by constructing a workdir under a dotdir.
    ///
    /// v0.6.27: gated to unix-only. Windows TempDirs canonicalize to
    /// the `\\?\` extended-path prefix which has its own normalization
    /// rules — leading `\\?\C:\...\-tmpXXX\-giga\...` doesn't preserve
    /// the dot-prefix the way Linux `/tmp/.../.giga/...` does. The
    /// `.giga` workdir convention is a WSL/Linux artifact anyway; the
    /// underlying encoder doesn't need a Windows code path for it.
    #[cfg(unix)]
    #[test]
    fn locate_claude_session_handles_dotdirs_in_workdir() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        // Build a workdir under a `.giga` subdir of a tempdir, mirror
        // of the real giga-harness layout.
        let parent = tempfile::TempDir::new().unwrap();
        let workdir = parent.path().join(".giga").join("workdirs").join("alice");
        fs::create_dir_all(&workdir).unwrap();
        let canon = workdir.canonicalize().unwrap();
        let encoded: String = canon
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        // Must contain the `--giga` double-dash signature (`/.giga` → `--giga`).
        assert!(
            encoded.contains("--giga"),
            "encoding lost `.` -> `-`: {encoded}"
        );
        let proj_dir = tmp_home
            .path()
            .join(".claude")
            .join("projects")
            .join(&encoded);
        fs::create_dir_all(&proj_dir).unwrap();
        let session = proj_dir.join("x.jsonl");
        fs::write(&session, "{}\n").unwrap();
        let picked = session_log(tmp_home.path(), &workdir).unwrap();
        assert_eq!(picked, session);
    }

    #[test]
    fn locate_claude_session_returns_none_when_no_jsonl() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();
        // No ~/.claude/projects/<encoded>/ created at all.
        assert!(session_log(tmp_home.path(), workdir.path()).is_none());
    }
}
