//! Crash-safe file writes: write to a sibling temp file, fsync, then
//! `rename` into place. The rename is atomic on POSIX and Windows, so a
//! reader never observes a half-written file and a crash mid-write leaves
//! the previous contents intact. Used for the swarm registry, swapped
//! credentials, HANDOVER.md banners, and codex envelopes — anything where
//! a torn write would corrupt durable state.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// The temp sibling path for `path`: the full filename with a
/// `.giga-tmp` suffix appended (not an extension replacement, so
/// `swarms.toml` → `swarms.toml.giga-tmp`).
fn temp_sibling(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".giga-tmp");
    std::path::PathBuf::from(name)
}

/// Atomically write `contents` to `path`, creating parent dirs as needed.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    write_inner(path, contents, None)
}

/// Atomically write `contents` to `path` with POSIX mode `mode` (e.g.
/// `0o600` for a credentials file). The mode is applied to the temp file
/// *before* the rename so the final file is never briefly world-readable.
/// On non-Unix the mode is ignored.
pub fn atomic_write_mode(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    write_inner(path, contents, Some(mode))
}

fn write_inner(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
    }
    let tmp = temp_sibling(path);
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(contents)
            .with_context(|| format!("writing {}", tmp.display()))?;
        // Best-effort durability before the rename.
        f.sync_all().ok();
    }
    if let Some(mode) = mode {
        set_mode(&tmp, mode)?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Atomically prepend `prefix` to `path`, creating the file (and parent
/// dirs) if it doesn't exist. Reads the current contents, then writes
/// `prefix ++ existing` via [`atomic_write`].
pub fn atomic_prepend(path: &Path, prefix: &str) -> Result<()> {
    let existing = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    atomic_write(path, format!("{prefix}{existing}").as_bytes())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perm.set_mode(mode);
    fs::set_permissions(path, perm)
        .with_context(|| format!("chmod {} -> {:o}", path.display(), mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    // Windows has no POSIX mode bits; ACLs are managed separately.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("sub/dir/file.toml");
        atomic_write(&p, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
        // no temp file left behind
        assert!(!temp_sibling(&p).exists());
    }

    #[test]
    fn write_overwrites_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("f");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");
    }

    #[test]
    fn prepend_creates_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("HANDOVER.md");
        atomic_prepend(&p, "BANNER\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "BANNER\n");
    }

    #[test]
    fn prepend_prepends_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("HANDOVER.md");
        atomic_write(&p, b"old body").unwrap();
        atomic_prepend(&p, "NEW\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "NEW\nold body");
    }

    #[cfg(unix)]
    #[test]
    fn write_mode_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("creds.json");
        atomic_write_mode(&p, b"{}", 0o600).unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
