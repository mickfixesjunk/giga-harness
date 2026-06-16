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
//! advance the cursor. A crash between read and commit re-delivers the
//! bytes on restart rather than dropping them. Each consumer keeps its own
//! `last_size` cursor and calls [`read_delta`] / [`read_delta_lossy`].

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, s: &str) {
        fs::write(path, s).unwrap();
    }

    #[test]
    fn read_delta_reads_suffix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("ch.md");
        write(&p, "hello world");
        assert_eq!(read_delta(&p, 6, 11).unwrap(), b"world");
        assert_eq!(read_delta_lossy(&p, 0, 5).unwrap(), "hello");
    }
}
