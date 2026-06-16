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

    // NOTE: these tests deliberately do NOT mutate the process-global
    // HOME/USERPROFILE env vars. The cargo test runner runs tests in
    // parallel threads of one process, and many tests across the crate
    // (registry, cursor, trust) read HOME — a set_var/remove_var here
    // races with them and flakes. We instead assert the *relationships*
    // against whatever the ambient env is.

    #[test]
    fn giga_home_is_home_dir_plus_dot_giga() {
        // Whatever home_dir() resolves to (ambient env), giga_home() is
        // exactly that with `.giga` appended — and they're both-Some or
        // both-None together.
        match home_dir() {
            Some(h) => assert_eq!(giga_home(), Some(h.join(".giga"))),
            None => assert_eq!(giga_home(), None),
        }
    }

    #[test]
    fn giga_home_ends_in_dot_giga_when_resolvable() {
        if let Some(g) = giga_home() {
            assert!(g.ends_with(".giga"), "got {}", g.display());
        }
    }
}
