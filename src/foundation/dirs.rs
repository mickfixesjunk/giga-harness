//! Home- and `~/.giga`-directory resolution.
//!
//! Native Windows shells (PowerShell, cmd.exe) set `%USERPROFILE%` but
//! often not `$HOME`. Without the fallback, anything keyed off the home
//! directory — read cursors, the busy-lock gate, the swarm registry —
//! silently disappears on Windows agents. Linux always has `$HOME`, so
//! the fallback is a no-op there.

use std::path::PathBuf;

/// The user's home directory: `$HOME`, falling back to `%USERPROFILE%`.
/// Returns `None` only when neither is set (callers decide whether that
/// is a soft skip or a hard error).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The `~/.giga` state directory derived from [`home_dir`].
pub fn giga_home() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".giga"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The env is process-global; these tests mutate and restore it and
    // must not run concurrently with each other (cargo runs tests in
    // the same binary on separate threads, so we keep each assertion
    // self-contained and restore what we touch).

    #[test]
    fn home_dir_prefers_home_then_userprofile() {
        let saved_home = std::env::var_os("HOME");
        let saved_up = std::env::var_os("USERPROFILE");

        std::env::set_var("HOME", "/home/neo");
        std::env::remove_var("USERPROFILE");
        assert_eq!(home_dir(), Some(PathBuf::from("/home/neo")));

        std::env::remove_var("HOME");
        std::env::set_var("USERPROFILE", r"C:\Users\Neo");
        assert_eq!(home_dir(), Some(PathBuf::from(r"C:\Users\Neo")));

        // restore
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_up {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
    }

    #[test]
    fn giga_home_appends_dot_giga() {
        let saved_home = std::env::var_os("HOME");
        let saved_up = std::env::var_os("USERPROFILE");

        std::env::set_var("HOME", "/home/neo");
        std::env::remove_var("USERPROFILE");
        assert_eq!(giga_home(), Some(PathBuf::from("/home/neo/.giga")));

        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_up {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
    }
}
