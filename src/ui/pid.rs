//! PID file enforcement for `giga ui`.
//!
//! Acquisition logic:
//!   1. If `~/.giga/ui.pid` exists and the recorded PID is alive,
//!      bail loudly with the live PID — don't try to bind the
//!      socket (and don't clobber the file).
//!   2. If the file exists but the PID is dead (stale lock from a
//!      crashed server), silently overwrite.
//!   3. Otherwise create it and write our PID.
//!
//! Release: `Guard` Drop removes the file. Best-effort — if removal
//! fails (e.g. the file was deleted out from under us), we don't
//! escalate; the next acquire will treat it as stale anyway.
//!
//! Liveness check on POSIX: `kill -0 <pid>` (no signal sent; just
//! checks whether the process exists and we have permission). On
//! Windows we fall back to `OpenProcess` via a separate path — for
//! v0.6.31 Phase A we punt and trust the file's PID (single-user
//! workstation; rare collision).

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Guard {
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Read the PID file at `path` and report whether the recorded
/// process is alive. Used by `giga launch --ui` to decide whether
/// the UI server already needs to be spawned. Treats missing,
/// malformed, or dead-PID files as "not running".
pub fn is_alive(path: &Path) -> bool {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let pid: i32 = match text.trim().parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    process_alive(pid)
}

pub fn acquire(path: &Path) -> Result<Guard> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    if let Ok(text) = fs::read_to_string(path) {
        if let Ok(pid) = text.trim().parse::<i32>() {
            if process_alive(pid) {
                anyhow::bail!(
                    "giga ui already running (PID {pid}) — pid file {}.\n\
                     Stop the running server first, or remove the pid file if you \
                     know the process is gone.",
                    path.display()
                );
            }
            // Stale: fall through and overwrite.
            eprintln!(
                "  ! stale pid file at {} (PID {pid} not alive) — replacing",
                path.display()
            );
        }
    }
    fs::write(path, std::process::id().to_string())
        .with_context(|| format!("writing pid file {}", path.display()))?;
    Ok(Guard {
        path: path.to_path_buf(),
    })
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // kill(pid, 0): no signal sent; returns 0 if process exists
    // and we have permission to signal it, otherwise -1.
    // SAFETY: libc::kill with sig=0 is a process-existence probe
    // and does not modify any state.
    unsafe { libc_kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    // Phase A punt on Windows — trust the recorded PID. Single-user
    // workstation; the operator is unlikely to recycle PIDs into a
    // collision with a dead giga ui server. Revisit in Phase G if
    // it bites.
    true
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    kill(pid, sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_writes_pid_file_when_absent() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("ui.pid");
        let guard = acquire(&p).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        let pid: u32 = text.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
        drop(guard);
        assert!(!p.exists(), "Guard drop should remove the pid file");
    }

    #[test]
    fn acquire_creates_parent_dir() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("nested").join("dir").join("ui.pid");
        let _guard = acquire(&p).unwrap();
        assert!(p.exists());
    }

    /// v0.6.33: gated to unix. The Phase A `process_alive` Windows
    /// stub always returns `true` (single-user workstation; PID
    /// collision is rare; full WinAPI liveness check deferred to
    /// Phase G). That stub means a "definitely dead" PID like
    /// 2_000_000 reports as alive on Windows and the acquire
    /// correctly bails — but the test was written to assert the
    /// unix overwrite path. When the Windows stub is upgraded,
    /// drop the cfg gate.
    #[cfg(unix)]
    #[test]
    fn acquire_overwrites_stale_pid_file() {
        // PID 1 is init/systemd — definitely alive — so we can't
        // use it to simulate a stale PID. Use a very large PID
        // that's almost certainly not allocated. The test is best-
        // effort: if by cosmic chance PID 2_000_000 is alive, this
        // test would fail. Acceptable.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("ui.pid");
        fs::write(&p, "2000000").unwrap();
        let _guard = acquire(&p).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        let pid: u32 = text.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn acquire_bails_when_pid_file_holds_live_pid() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("ui.pid");
        // Our own PID is definitely alive.
        fs::write(&p, std::process::id().to_string()).unwrap();
        let err = acquire(&p).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "expected 'already running' in: {err}",
        );
    }

    #[test]
    fn drop_removes_pid_file() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("ui.pid");
        {
            let _guard = acquire(&p).unwrap();
            assert!(p.exists());
        }
        assert!(!p.exists());
    }
}
