//! The canonical locked append.
//!
//! Cross-host channels are dual-written: `post` appends a frame to the
//! merged file AND its slice, and the `merger` appends peer slices into
//! the same merged file. Two writers means torn frames are possible —
//! POSIX `O_APPEND` is atomic only up to `PIPE_BUF` (4KB) and a ~10KB
//! report can interleave. An exclusive file lock around the append
//! serializes writers so frames never tear.
//!
//! Promoted out of `post.rs` so every appender (post, merger, and the
//! watcher's FYI archive — which previously appended UNLOCKED) routes
//! through one implementation.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Append `bytes` to `path` under an exclusive file lock that works on
/// both POSIX and Windows.
///
/// Windows subtlety (v0.4.4): `OpenOptions::append(true)` opens with
/// `FILE_APPEND_DATA` only, which `LockFileEx` rejects with
/// `ERROR_ACCESS_DENIED`. Opening `read(true).write(true).create(true)`
/// maps to `GENERIC_READ | GENERIC_WRITE` (POSIX `O_RDWR | O_CREAT`),
/// which the lock accepts; we then `seek(End)` explicitly inside the
/// locked region (no `O_APPEND`, so the kernel won't seek for us, but the
/// lock serializes seek+write atomically across processes).
///
/// If the open or the lock acquire fails for some other reason, falls
/// back to a plain `O_APPEND` write for resilience.
pub fn append_with_lock(path: &Path, bytes: &[u8]) -> Result<()> {
    let open_result = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path);
    let mut f = match open_result {
        Ok(f) => f,
        Err(_) => return append_plain(path, bytes),
    };
    if f.lock().is_err() {
        eprintln!(
            "append: couldn't acquire exclusive lock on {} despite read+write open — \
             proceeding without lock (rare; investigate if persistent)",
            path.display(),
        );
        drop(f);
        return append_plain(path, bytes);
    }
    let seek_result = f.seek(SeekFrom::End(0));
    if let Err(e) = seek_result {
        let _ = f.unlock();
        return Err(anyhow!("seek to end of {} failed: {e}", path.display()));
    }
    let write_result = f.write_all(bytes);
    let _ = f.unlock();
    write_result.with_context(|| format!("writing to {}", path.display()))?;
    Ok(())
}

/// Fallback append used when the read+write open or the lock acquire
/// fails. Plain `O_APPEND`; on POSIX this preserves kernel-side atomicity
/// for writes up to `PIPE_BUF`.
pub fn append_plain(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("writing to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn append_creates_then_appends() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        append_with_lock(&p, b"one\n").unwrap();
        append_with_lock(&p, b"two\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\ntwo\n");
    }

    #[test]
    fn append_releases_lock_cleanly() {
        // After the call returns, the file must NOT be left locked — a
        // subsequent open+lock must succeed without contention. (v0.4.4
        // Bug 11: the read+write open is what makes LockFileEx work on
        // Windows; this guards that the lock is also released.)
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("under-lock.md");
        append_with_lock(&p, b"first\n").unwrap();
        append_with_lock(&p, b"second\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "first\nsecond\n");
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&p)
            .unwrap();
        f.lock()
            .expect("file must be unlocked after append_with_lock returns");
    }

    #[test]
    fn append_plain_creates_then_appends() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        append_plain(&p, b"a").unwrap();
        append_plain(&p, b"b").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "ab");
    }

    #[test]
    fn concurrent_appends_do_not_tear() {
        // Each thread appends a distinct >PIPE_BUF block many times; with
        // the lock, no block is ever interleaved with another.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        let block_a = "A".repeat(8192);
        let block_b = "B".repeat(8192);
        let pa = p.clone();
        let pb = p.clone();
        let ba = block_a.clone();
        let bb = block_b.clone();
        let ta = std::thread::spawn(move || {
            for _ in 0..20 {
                append_with_lock(&pa, ba.as_bytes()).unwrap();
            }
        });
        let tb = std::thread::spawn(move || {
            for _ in 0..20 {
                append_with_lock(&pb, bb.as_bytes()).unwrap();
            }
        });
        ta.join().unwrap();
        tb.join().unwrap();
        let content = fs::read_to_string(&p).unwrap();
        // Every 8192-run is homogeneous: split into 8192-char chunks and
        // assert each is all-A or all-B (no interleaving inside a block).
        assert_eq!(content.len(), 8192 * 40);
        for chunk in content.as_bytes().chunks(8192) {
            let first = chunk[0];
            assert!(chunk.iter().all(|&b| b == first), "torn block detected");
        }
    }
}
