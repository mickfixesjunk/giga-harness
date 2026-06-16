//! Path string normalization and ancestor walking.
//!
//! Two recurring needs across the harness:
//!
//!   * **Forward-slash normalization** for anything that crosses an SSH
//!     boundary. Peers are always Linux/WSL, but the operator host may be
//!     Windows where `PathBuf::join` emits `\`. A backslash in a remote
//!     path silently breaks rsync/ssh targets, so every wire-facing path
//!     is forced to `/` via [`to_unix`] / [`unix_join`].
//!   * **Config-dir derivation + ancestor walking** so a command run from
//!     anywhere under a code root can locate its swarm config.

use std::path::{Path, PathBuf};

/// Normalize a path to a forward-slash string, regardless of host OS.
/// A Windows-built `PathBuf` ("C:\\a\\b") becomes "C:/a/b".
pub fn to_unix(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Join `name` onto `dir` using a forward slash, normalizing `dir` to
/// unix separators and collapsing any trailing slash first. This is the
/// path form to hand to ssh/rsync — never `PathBuf::join`, which would
/// emit `\` on a Windows operator host.
pub fn unix_join(dir: &Path, name: &str) -> String {
    let dir_str = to_unix(dir);
    let trimmed = dir_str.trim_end_matches('/');
    format!("{trimmed}/{name}")
}

/// The directory containing a config file: canonicalize the config path
/// (so a symlinked workdir config resolves against its real sibling
/// directory) then take the parent. Falls back to the un-canonicalized
/// path when canonicalization fails (e.g. the file doesn't exist yet).
pub fn config_dir(config_path: &Path) -> PathBuf {
    let canonical = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());
    canonical
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Walk `start` and each of its ancestors, calling `f` on every directory
/// from the deepest upward, returning the first `Some` it yields. `start`
/// is canonicalized first (falling back to as-is) so relative invocations
/// resolve consistently. Used to locate a swarm config / code-root marker
/// from an arbitrary working directory.
pub fn walk_up<T>(start: &Path, mut f: impl FnMut(&Path) -> Option<T>) -> Option<T> {
    let canonical = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut cursor: &Path = &canonical;
    loop {
        if let Some(found) = f(cursor) {
            return Some(found);
        }
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_unix_rewrites_backslashes() {
        assert_eq!(
            to_unix(Path::new(r"C:\Users\bob\inbox")),
            "C:/Users/bob/inbox"
        );
        assert_eq!(to_unix(Path::new("/home/bob/x")), "/home/bob/x");
    }

    #[test]
    fn unix_join_uses_forward_slashes() {
        assert_eq!(
            unix_join(Path::new("/home/bob/.giga/configs/x"), "giga-harness.toml"),
            "/home/bob/.giga/configs/x/giga-harness.toml"
        );
    }

    #[test]
    fn unix_join_trims_trailing_slash() {
        assert_eq!(
            unix_join(Path::new("/home/bob/.giga/configs/x/"), "f.md"),
            "/home/bob/.giga/configs/x/f.md"
        );
    }

    #[test]
    fn unix_join_normalizes_windows_built_dir() {
        assert_eq!(
            unix_join(Path::new(r"C:\Users\bob\inbox"), "ch.md"),
            "C:/Users/bob/inbox/ch.md"
        );
    }

    #[test]
    fn config_dir_is_parent_of_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg, "x").unwrap();
        let dir = config_dir(&cfg);
        // canonicalized parent equals the canonicalized tmp dir
        assert_eq!(dir, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn walk_up_finds_marker_in_ancestor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let marker = tmp.path().join("MARKER");
        std::fs::write(&marker, "x").unwrap();
        let found = walk_up(&nested, |dir| {
            let m = dir.join("MARKER");
            m.exists().then_some(m)
        });
        assert_eq!(
            found,
            Some(tmp.path().canonicalize().unwrap().join("MARKER"))
        );
    }

    #[test]
    fn walk_up_returns_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let found = walk_up(tmp.path(), |dir| dir.join("NOPE").exists().then_some(()));
        assert_eq!(found, None);
    }
}
