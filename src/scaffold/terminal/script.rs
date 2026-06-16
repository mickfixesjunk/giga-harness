//! Shared helpers used by more than one terminal backend: the
//! stagger sleep, the launch-script filename sanitizer, and the
//! chmod-executable helper for the temp `.sh` scripts that the wt and
//! mac backends hand to wsl.exe / Terminal.app respectively.

use std::fs;
use std::path::Path;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;

/// v0.6.19: sleep `seconds` if non-zero. Pulled out so each
/// multiplexer doesn't reimplement the "skip if zero" guard.
pub(super) fn stagger_sleep(seconds: u64) {
    if seconds > 0 {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
}

/// Replace anything that isn't `[A-Za-z0-9_-]` with `_`. Agent names
/// are kebab-case slugs already, but defensive.
pub(super) fn sanitize_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(unix)]
pub(super) fn chmod_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("chmod 755 {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn chmod_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.6.19: stagger_sleep is a no-op when seconds=0 (returns
    /// immediately). The actual sleep behavior at seconds>0 is
    /// trusted to the stdlib; we just verify the fast-path.
    #[test]
    fn stagger_sleep_is_immediate_when_zero() {
        let start = std::time::Instant::now();
        stagger_sleep(0);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "stagger_sleep(0) should return immediately, took {elapsed:?}",
        );
    }

    /// v0.6.19: at seconds=1 there IS a measurable sleep. Confirms
    /// the non-zero path actually sleeps (so a refactor that drops
    /// the call gets caught).
    #[test]
    fn stagger_sleep_actually_sleeps_when_nonzero() {
        let start = std::time::Instant::now();
        stagger_sleep(1);
        let elapsed = start.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(900),
            "stagger_sleep(1) should sleep ~1s, took {elapsed:?}",
        );
    }

    #[test]
    fn sanitize_filename_passes_through_slugs() {
        assert_eq!(sanitize_for_filename("design"), "design");
        assert_eq!(sanitize_for_filename("code-review"), "code-review");
        assert_eq!(sanitize_for_filename("agent_42"), "agent_42");
    }

    #[test]
    fn sanitize_filename_replaces_unsafe_chars() {
        // Path separators, spaces, shell metachars — all become `_`.
        assert_eq!(sanitize_for_filename("a/b"), "a_b");
        assert_eq!(sanitize_for_filename("a b"), "a_b");
        assert_eq!(sanitize_for_filename("a;b"), "a_b");
        assert_eq!(sanitize_for_filename("a$b"), "a_b");
        assert_eq!(sanitize_for_filename("../etc"), "___etc");
    }
}
