//! Per-agent read cursors at `~/.giga/cursors/<agent>/<channel>.pos`.
//!
//! Each cursor file holds a single ASCII decimal — the byte offset up
//! to which that agent has already seen messages in that channel file.
//! The watcher writes the cursor after every notification it delivers;
//! `giga catchup` reads from the cursor to EOF and then advances it.
//!
//! Cursor files that don't exist yet are treated as "unknown" — callers
//! that want a default fall back to EOF (don't replay old messages as
//! live notifications) or to 0 (read the full file for catchup).

use std::fs;
use std::path::{Path, PathBuf};

/// Path to the cursor file for `(agent, channel_filename)`.
/// e.g. `~/.giga/cursors/code/design-code.md.pos`
pub fn cursor_path(giga_home: &Path, agent: &str, channel_filename: &str) -> PathBuf {
    giga_home
        .join("cursors")
        .join(agent)
        .join(format!("{channel_filename}.pos"))
}

/// Read the stored byte offset. Returns `None` if no cursor file exists
/// (caller decides whether to fall back to 0 or EOF).
pub fn read(giga_home: &Path, agent: &str, channel_filename: &str) -> Option<u64> {
    let s = fs::read_to_string(cursor_path(giga_home, agent, channel_filename)).ok()?;
    s.trim().parse::<u64>().ok()
}

/// Write `offset` to the cursor file, creating parent dirs as needed.
/// Errors are silently swallowed — a failed cursor write must never
/// crash the watcher or the catchup command.
pub fn write(giga_home: &Path, agent: &str, channel_filename: &str, offset: u64) {
    let path = cursor_path(giga_home, agent, channel_filename);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, offset.to_string());
}

/// The ~/.giga directory derived from $HOME, falling back to %USERPROFILE%.
/// Native Windows often has USERPROFILE but no HOME, so without the fallback
/// giga_home() returns None there — which silently disables cursors AND the
/// busy-lock gate on Windows agents. Linux always has HOME, so the fallback
/// is a no-op there.
pub fn giga_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".giga"))
}

// ---------------------------------------------------------------------------
// Merge cursors (per-channel-per-slice-host) — used by `giga merger` to
// remember how many bytes of each peer's slice file we've already appended
// to the local merged <channel>.md. Keyed by (channel, slice_host) not by
// (agent, channel) like the watch cursors above — different consumers.
// ---------------------------------------------------------------------------

/// Path to the merge cursor for `(channel, slice_host)`.
/// e.g. `~/.giga/merge-cursors/design-code.md/wsl-a.pos`
pub fn merge_cursor_path(giga_home: &Path, channel: &str, slice_host: &str) -> PathBuf {
    giga_home
        .join("merge-cursors")
        .join(channel)
        .join(format!("{slice_host}.pos"))
}

/// Read the stored merge offset. Returns `None` when the cursor file
/// doesn't exist (caller decides whether to fall back to 0 or EOF).
pub fn read_merge(giga_home: &Path, channel: &str, slice_host: &str) -> Option<u64> {
    let s = fs::read_to_string(merge_cursor_path(giga_home, channel, slice_host)).ok()?;
    s.trim().parse::<u64>().ok()
}

/// Write `offset` to the merge cursor, creating parent dirs as needed.
/// Errors are silently swallowed (same policy as the watch cursors) — a
/// failed cursor write must never crash the merger.
pub fn write_merge(giga_home: &Path, channel: &str, slice_host: &str, offset: u64) {
    let path = merge_cursor_path(giga_home, channel, slice_host);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, offset.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "code", "design-code.md", 1234);
        assert_eq!(read(tmp.path(), "code", "design-code.md"), Some(1234));
    }

    #[test]
    fn missing_cursor_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read(tmp.path(), "code", "design-code.md"), None);
    }

    #[test]
    fn write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "some-agent", "some-channel.md", 42);
        assert!(cursor_path(tmp.path(), "some-agent", "some-channel.md").exists());
    }

    #[test]
    fn overwrite_updates_value() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "test", "ch.md", 100);
        write(tmp.path(), "test", "ch.md", 999);
        assert_eq!(read(tmp.path(), "test", "ch.md"), Some(999));
    }

    #[test]
    fn merge_cursor_round_trip() {
        let tmp = TempDir::new().unwrap();
        write_merge(tmp.path(), "alice-bob.md", "wsl-a", 4242);
        assert_eq!(read_merge(tmp.path(), "alice-bob.md", "wsl-a"), Some(4242));
    }

    #[test]
    fn merge_cursor_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_merge(tmp.path(), "alice-bob.md", "wsl-a"), None);
    }

    #[test]
    fn merge_cursor_per_channel_per_slice_host_isolation() {
        let tmp = TempDir::new().unwrap();
        write_merge(tmp.path(), "ch-1.md", "wsl-a", 100);
        write_merge(tmp.path(), "ch-1.md", "wsl-b", 200);
        write_merge(tmp.path(), "ch-2.md", "wsl-a", 300);
        assert_eq!(read_merge(tmp.path(), "ch-1.md", "wsl-a"), Some(100));
        assert_eq!(read_merge(tmp.path(), "ch-1.md", "wsl-b"), Some(200));
        assert_eq!(read_merge(tmp.path(), "ch-2.md", "wsl-a"), Some(300));
    }

    #[test]
    fn merge_cursor_path_layout() {
        let tmp = TempDir::new().unwrap();
        let p = merge_cursor_path(tmp.path(), "design-code.md", "wsl-a");
        assert!(p.ends_with("merge-cursors/design-code.md/wsl-a.pos"));
    }
}
