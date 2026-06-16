//! Cross-host slice-file naming.
//!
//! On a multi-host swarm each channel `<channel>.md` is fed by per-host
//! **slice** files `<channel>.<host>.md`. Each host appends only to its
//! own slice (single-writer, conflict-free); the merger folds peers'
//! slices into the watched merged file. Three call sites independently
//! re-derived this name (`post`, `merger`, `sync`); this is the one
//! definition. Pure — no filesystem access, so it is trivially testable.

use std::path::{Path, PathBuf};

/// Given a merged channel path `/dir/<channel>.md` and a host slug,
/// return the slice path `/dir/<channel>.<host>.md`.
///
/// The host suffix is inserted before the `.md` extension. Channel names
/// that themselves contain dots are handled by splitting on the final
/// extension only (via [`Path::file_stem`]).
pub fn slice_path(merged: &Path, host: &str) -> PathBuf {
    let parent = merged.parent().unwrap_or_else(|| Path::new("."));
    let stem = merged
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "channel".to_string());
    parent.join(format!("{stem}.{host}.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_host_before_md_extension() {
        assert_eq!(
            slice_path(Path::new("/dir/design-code.md"), "wsl-a"),
            PathBuf::from("/dir/design-code.wsl-a.md")
        );
    }

    #[test]
    fn handles_channel_name_with_dots() {
        assert_eq!(
            slice_path(Path::new("/dir/a.b.c.md"), "h"),
            PathBuf::from("/dir/a.b.c.h.md")
        );
    }

    #[test]
    fn preserves_inbox_dir() {
        assert_eq!(
            slice_path(Path::new("/x/y/inbox/_broadcast.md"), "host-b"),
            PathBuf::from("/x/y/inbox/_broadcast.host-b.md")
        );
    }

    #[test]
    fn relative_path_keeps_relative() {
        // A bare filename's parent is "" (not None), so no "./" is
        // prepended — matches the channel paths produced in practice.
        assert_eq!(
            slice_path(Path::new("design-code.md"), "wsl-a"),
            PathBuf::from("design-code.wsl-a.md")
        );
    }
}
