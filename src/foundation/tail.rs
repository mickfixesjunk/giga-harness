//! Byte-cursor file tailing — the no-loss / no-double-deliver primitive.
//!
//! The watcher, the merger, and the codex bridge all tail an append-only
//! channel file the same way: remember how many bytes you've consumed
//! (`last_size`), stat the file, and read only the new suffix. Each had
//! its own copy of `read_delta` and its own `last_size` field and its own
//! `POLL_INTERVAL`/`RELOAD_EVERY_N_TICKS` constants. This is the one
//! implementation.
//!
//! The no-loss guarantee lives in the read/commit split: read the pending
//! bytes, hand them off (emit / append / persist a cursor), and only THEN
//! [`Tailer::commit`]. A crash between read and commit re-delivers the
//! bytes on restart rather than dropping them.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the daemons stat their files.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Config-reload cadence, in ticks (so newly-added channels are picked up
/// without restarting the daemon).
pub const RELOAD_EVERY_N_TICKS: u64 = 5;

/// Read bytes `[from, to)` of `path`. The cursor invariant guarantees
/// `to <= file len` and `from <= to`, so `read_exact` is sound.
pub fn read_delta(path: &Path, from: u64, to: u64) -> io::Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; (to - from) as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// [`read_delta`] decoded lossily to a `String` (for consumers that emit
/// text rather than re-append raw bytes).
pub fn read_delta_lossy(path: &Path, from: u64, to: u64) -> io::Result<String> {
    Ok(String::from_utf8_lossy(&read_delta(path, from, to)?).into_owned())
}

/// A per-file byte cursor.
#[derive(Debug, Clone)]
pub struct Tailer {
    path: PathBuf,
    last_size: u64,
}

impl Tailer {
    /// Tail `path` from the beginning (replays existing content).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_size: 0,
        }
    }

    /// Tail `path` starting at byte `start` (e.g. current EOF, so only
    /// future appends surface; or a persisted cursor for crash recovery).
    pub fn starting_at(path: impl Into<PathBuf>, start: u64) -> Self {
        Self {
            path: path.into(),
            last_size: start,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn last_size(&self) -> u64 {
        self.last_size
    }

    /// Current on-disk size, or 0 if the file can't be stat'd.
    fn current_size(&self) -> u64 {
        fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    /// Reconcile truncation/rotation: if the file shrank below the cursor,
    /// drop the cursor to the current size so we never compute a negative
    /// delta. Returns the (possibly-reset) current size.
    fn sync_truncation(&mut self) -> u64 {
        let cur = self.current_size();
        if cur < self.last_size {
            self.last_size = cur;
        }
        cur
    }

    /// Read the bytes appended since the cursor WITHOUT advancing it.
    /// Returns `(current_size, bytes)`, or `None` when there's nothing
    /// new. The caller advances with [`commit`](Self::commit) once the
    /// bytes are durably handled — this is the no-loss seam.
    pub fn pending(&mut self) -> io::Result<Option<(u64, Vec<u8>)>> {
        let cur = self.sync_truncation();
        if cur <= self.last_size {
            return Ok(None);
        }
        let bytes = read_delta(&self.path, self.last_size, cur)?;
        Ok(Some((cur, bytes)))
    }

    /// Advance the cursor to `cur` (typically the value returned by
    /// [`pending`](Self::pending) once its bytes have been handled).
    pub fn commit(&mut self, cur: u64) {
        self.last_size = cur;
    }

    /// Read the new bytes and advance the cursor in one step. For
    /// consumers that handle the delta inline and don't need the deferred
    /// crash-safe commit (the merger and codex bridge).
    pub fn poll(&mut self) -> io::Result<Option<Vec<u8>>> {
        match self.pending()? {
            Some((cur, bytes)) => {
                self.commit(cur);
                Ok(Some(bytes))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, s: &str) {
        fs::write(path, s).unwrap();
    }
    fn append(path: &Path, s: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn read_delta_reads_suffix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        write(&p, "hello world");
        assert_eq!(read_delta(&p, 6, 11).unwrap(), b"world");
        assert_eq!(read_delta_lossy(&p, 0, 5).unwrap(), "hello");
    }

    #[test]
    fn poll_returns_only_new_bytes_and_advances() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        write(&p, "aaa");
        let mut t = Tailer::new(&p);
        assert_eq!(t.poll().unwrap().as_deref(), Some(&b"aaa"[..]));
        assert_eq!(t.last_size(), 3);
        // nothing new
        assert!(t.poll().unwrap().is_none());
        append(&p, "bbb");
        assert_eq!(t.poll().unwrap().as_deref(), Some(&b"bbb"[..]));
        assert_eq!(t.last_size(), 6);
    }

    #[test]
    fn starting_at_eof_skips_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        write(&p, "old content");
        let start = fs::metadata(&p).unwrap().len();
        let mut t = Tailer::starting_at(&p, start);
        assert!(t.poll().unwrap().is_none()); // existing skipped
        append(&p, "NEW");
        assert_eq!(t.poll().unwrap().as_deref(), Some(&b"NEW"[..]));
    }

    #[test]
    fn pending_does_not_advance_until_commit() {
        // The no-loss seam: a crash before commit re-delivers.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        write(&p, "frame");
        let mut t = Tailer::new(&p);
        let (cur, bytes) = t.pending().unwrap().unwrap();
        assert_eq!(bytes, b"frame");
        // simulate a crash before commit: a fresh tailer at the SAME
        // (un-advanced) cursor re-reads the same bytes.
        let mut recovered = Tailer::starting_at(&p, t.last_size());
        assert_eq!(recovered.poll().unwrap().as_deref(), Some(&b"frame"[..]));
        // now commit the original and confirm it won't re-deliver.
        t.commit(cur);
        assert!(t.poll().unwrap().is_none());
    }

    #[test]
    fn truncation_resets_cursor_then_delivers_subsequent_appends() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        write(&p, "0123456789");
        let mut t = Tailer::new(&p);
        let _ = t.poll().unwrap();
        assert_eq!(t.last_size(), 10);
        // File rotated/truncated to a shorter length. Matching the
        // merger's long-standing behavior, the cursor resets DOWN to the
        // current size (channels are append-only; this prevents a negative
        // delta) — content rewritten at/below the reset is not re-read,
        // but appends beyond it are.
        write(&p, "new");
        assert!(t.poll().unwrap().is_none());
        assert_eq!(t.last_size(), 3);
        append(&p, "XYZ");
        assert_eq!(t.poll().unwrap().as_deref(), Some(&b"XYZ"[..]));
        assert_eq!(t.last_size(), 6);
    }

    #[test]
    fn missing_file_polls_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("does-not-exist.md");
        let mut t = Tailer::new(&p);
        assert!(t.poll().unwrap().is_none());
    }
}
