//! Host-filesystem path translation.
//!
//! Configs use the same path form an end-user would type for that
//! agent's *target* OS — e.g., `C:\Users\Audio\sdd-testwin` for a
//! Windows-platform agent, even when giga itself is running on
//! Linux/WSL. That string is meaningful for the spawn command we
//! hand to wt + PowerShell, but it's not a valid Linux filesystem
//! path. Linux syscalls treat `\` as a regular character, so a naive
//! `fs::create_dir_all` would create a literally-named directory
//! containing backslashes.
//!
//! This module translates a config-form path into a path the host
//! filesystem can actually use. On Linux/WSL we map `C:\path\to`
//! into `/mnt/c/path/to`. On native Windows we leave it alone.

use std::path::{Path, PathBuf};

/// Translate a config-form path into a host-FS path.
pub fn to_host_fs(p: &Path) -> PathBuf {
    if cfg!(unix) {
        let s = p.to_string_lossy();
        if let Some(translated) = wsl_translate(&s) {
            return PathBuf::from(translated);
        }
    }
    p.to_path_buf()
}

/// If `s` looks like a Windows drive path (`X:\...` or `X:/...`),
/// return the `/mnt/<x>/...` form. Otherwise return None.
fn wsl_translate(s: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_windows_path() {
        assert_eq!(
            wsl_translate("C:\\Users\\Audio\\sdd-testwin").as_deref(),
            Some("/mnt/c/Users/Audio/sdd-testwin"),
        );
    }

    #[test]
    fn forward_slash_windows_path() {
        assert_eq!(
            wsl_translate("D:/projects/x").as_deref(),
            Some("/mnt/d/projects/x"),
        );
    }

    #[test]
    fn passes_linux_path_through() {
        assert_eq!(wsl_translate("/home/neo/x"), None);
    }

    #[test]
    fn passes_relative_through() {
        assert_eq!(wsl_translate("relative/path"), None);
    }
}
