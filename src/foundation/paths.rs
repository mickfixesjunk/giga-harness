//! Path string normalization and ancestor walking.
//!
//! Two recurring needs across the harness:
//!
//!   * **Forward-slash normalization** for anything that crosses an SSH
//!     boundary. Peers are always Linux/WSL, but the operator host may be
//!     Windows where `PathBuf::join` emits `\`. A backslash in a remote
//!     path silently breaks rsync/ssh targets, so every wire-facing path
//!     is forced to `/` via [`to_unix`] / [`unix_join`].

use std::path::Path;

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
}
