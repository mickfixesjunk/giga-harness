//! Re-invoking the `giga` binary as a subprocess.
//!
//! Several commands shell out to `giga` itself — `teleport` runs
//! `giga sync`/`giga init` on a peer, `upgrade` re-posts a rearm
//! broadcast, the UI runs `giga validate`/`launch`/…, and the watcher
//! self-rearms by re-exec'ing. They all need to resolve "which binary am
//! I" the same way; this is that resolution, once.

use std::path::{Path, PathBuf};

/// Resolve the path to *this* `giga` binary for re-invocation:
/// `current_exe()`, falling back to the bare name `giga` (which the
/// child's PATH lookup can still resolve).
pub fn giga_binary() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("giga"))
}

/// Resolve the giga binary *after a self-overwriting install*.
///
/// `install.sh` unlinks the running binary, so `current_exe()` (which
/// reads `/proc/self/exe`) then resolves to a `(deleted)` path that
/// fails with ENOENT on spawn. Prefer `which("giga")` (whatever PATH now
/// points at — the freshly-installed binary); else the captured
/// `previous` path if it still exists (install wrote in place); else the
/// bare name. `dry_run` short-circuits to `previous` (no install ran).
pub fn fresh_giga_binary(dry_run: bool, previous: &Path) -> PathBuf {
    if dry_run {
        return previous.to_path_buf();
    }
    if let Ok(p) = which::which("giga") {
        return p;
    }
    if previous.exists() {
        return previous.to_path_buf();
    }
    PathBuf::from("giga")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_dry_run_returns_previous() {
        let prev = PathBuf::from("/some/captured/giga");
        assert_eq!(fresh_giga_binary(true, &prev), prev);
    }

    #[test]
    fn fresh_falls_back_to_previous_when_path_lookup_fails() {
        // A real on-disk file standing in for the captured previous
        // binary. If a real `giga` is on PATH the which() branch wins
        // (also fine); otherwise the existing `previous` is returned —
        // never the bare name when `previous` exists.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let resolved = fresh_giga_binary(false, tmp.path());
        assert!(resolved == which::which("giga").unwrap_or_else(|_| tmp.path().to_path_buf()));
    }
}
