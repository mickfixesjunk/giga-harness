//! Host-filesystem path translation.
//!
//! Configs are shared between sides of a multi-host setup. Whoever
//! authored the file picked one form per path — e.g.,
//! `C:\Users\Alice\windows-agent` for a Windows-platform agent's
//! workdir, or `/mnt/c/Users/Alice` for the windows_inbox path used
//! by both sides. That string is meaningful in its authored form,
//! but a process running on the *other* side can't open it via the
//! filesystem without translation. Linux syscalls treat `\` as a
//! regular character (so a naive create_dir_all on `C:\foo` makes a
//! literally-named directory), and Windows syscalls don't know
//! what `/mnt/c` means.
//!
//! This module translates a config-form path into a path the host
//! filesystem can actually use:
//!   - Linux/WSL host + `C:\...` → `/mnt/c/...`
//!   - Windows host + `/mnt/c/...` → `C:\...`
//!   - Anything else (already in host form) is left alone.

use std::path::{Path, PathBuf};

/// Translate a config-form path into a host-FS path.
pub fn to_host_fs(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if cfg!(unix) {
        if let Some(translated) = windows_to_wsl(&s) {
            return PathBuf::from(translated);
        }
    } else if cfg!(windows) {
        if let Some(translated) = wsl_to_windows(&s) {
            return PathBuf::from(translated);
        }
    }
    p.to_path_buf()
}

/// If `s` looks like a Windows drive path (`X:\...` or `X:/...`),
/// return the `/mnt/<x>/...` form. Otherwise return None.
fn windows_to_wsl(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let drive = bytes[0];
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    if bytes[1] != b':' {
        return None;
    }
    if bytes[2] != b'\\' && bytes[2] != b'/' {
        return None;
    }
    let drive_lower = (drive | 0x20) as char;
    // Skip the drive letter + colon; keep the separator so we don't
    // collapse "C:\foo" into "/mnt/cfoo".
    let rest = s[2..].replace('\\', "/");
    Some(format!("/mnt/{}{}", drive_lower, rest))
}

/// If `s` looks like a WSL drive-mount path (`/mnt/<x>/...`),
/// return the `X:\...` form (uppercased drive, backslash
/// separators). Otherwise return None. Accepts `/mnt/c` with no
/// trailing slash → `C:\`.
fn wsl_to_windows(s: &str) -> Option<String> {
    let rest = s.strip_prefix("/mnt/")?;
    let mut chars = rest.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    let drive_upper = drive.to_ascii_uppercase();
    // After the drive letter we expect either end-of-string or '/'.
    let tail = match chars.next() {
        None => return Some(format!("{drive_upper}:\\")),
        Some('/') => chars.as_str(),
        _ => return None,
    };
    Some(format!("{drive_upper}:\\{}", tail.replace('/', "\\")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_windows_path() {
        assert_eq!(
            windows_to_wsl("C:\\Users\\Alice\\windows-agent").as_deref(),
            Some("/mnt/c/Users/Alice/windows-agent"),
        );
    }

    #[test]
    fn forward_slash_windows_path() {
        assert_eq!(
            windows_to_wsl("D:/projects/x").as_deref(),
            Some("/mnt/d/projects/x"),
        );
    }

    #[test]
    fn passes_linux_path_through() {
        assert_eq!(windows_to_wsl("/home/alice/x"), None);
    }

    #[test]
    fn passes_relative_through() {
        assert_eq!(windows_to_wsl("relative/path"), None);
    }

    #[test]
    fn translates_wsl_mount_path() {
        assert_eq!(
            wsl_to_windows("/mnt/c/Users/alice/projects/myproj").as_deref(),
            Some("C:\\Users\\alice\\projects\\myproj"),
        );
    }

    #[test]
    fn translates_wsl_mount_drive_only() {
        assert_eq!(wsl_to_windows("/mnt/d").as_deref(), Some("D:\\"),);
    }

    #[test]
    fn rejects_non_mount_unix_path() {
        assert!(wsl_to_windows("/home/alice/x").is_none());
        assert!(wsl_to_windows("/mnt/").is_none());
        assert!(wsl_to_windows("/mnt").is_none());
    }

    #[test]
    fn windows_to_wsl_lowercases_drive() {
        assert_eq!(windows_to_wsl("E:\\foo").as_deref(), Some("/mnt/e/foo"),);
    }

    #[test]
    fn wsl_to_windows_uppercases_drive() {
        assert_eq!(wsl_to_windows("/mnt/z/foo").as_deref(), Some("Z:\\foo"),);
    }

    #[test]
    fn windows_to_wsl_rejects_too_short() {
        assert!(windows_to_wsl("C").is_none());
        assert!(windows_to_wsl("C:").is_none());
        assert!(windows_to_wsl("").is_none());
    }

    #[test]
    fn windows_to_wsl_rejects_missing_separator() {
        // "C:foo" — no separator between colon and rest.
        assert!(windows_to_wsl("C:foo").is_none());
    }

    #[test]
    fn windows_to_wsl_rejects_non_letter_drive() {
        assert!(windows_to_wsl("1:\\foo").is_none());
        assert!(windows_to_wsl(":\\foo").is_none());
    }

    #[test]
    fn wsl_to_windows_rejects_non_letter_drive() {
        assert!(wsl_to_windows("/mnt/1/foo").is_none());
    }

    #[test]
    fn round_trip_windows_to_wsl_to_windows() {
        let orig = "C:\\Users\\alice\\project";
        let wsl_form = windows_to_wsl(orig).unwrap();
        let back = wsl_to_windows(&wsl_form).unwrap();
        assert_eq!(back, orig);
    }

    #[test]
    fn windows_to_wsl_preserves_deep_path() {
        assert_eq!(
            windows_to_wsl("C:\\a\\b\\c\\d\\e\\f\\g.txt").as_deref(),
            Some("/mnt/c/a/b/c/d/e/f/g.txt"),
        );
    }

    #[test]
    fn to_host_fs_passes_through_on_unrecognized() {
        use std::path::Path;
        let p = Path::new("/home/alice/something");
        // On Unix this returns the same; on Windows it would translate.
        // Either way we don't crash and don't mangle non-matching paths.
        let out = to_host_fs(p);
        // No mnt-prefix garbage was inserted:
        assert!(
            out.to_string_lossy().starts_with("/home/alice/")
                || out.to_string_lossy().starts_with("\\\\")
        );
    }
}
