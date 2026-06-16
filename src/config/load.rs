//! The load/parse/path-default pipeline: read TOML from disk,
//! resolve the sibling `this_host` identity, fill in per-host inbox
//! defaults, then validate.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::schema::{Config, ThisHostFile, THIS_HOST_FILE, THIS_HOST_FILE_LEGACY};

impl Config {
    /// Read a config from disk and validate it semantically.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing TOML config at {}", path.display()))?;
        // v0.3.7 Bug 1 fix: callers commonly pass a symlinked path
        // (e.g., workdirs/<agent>/giga-harness.toml -> swarm/giga-harness.toml).
        // sibling lookups (this_host.toml) must resolve relative to the
        // target's directory, NOT the symlink's parent — otherwise the
        // sibling is silently missing and downstream code degrades to
        // "no this_host" with a confusing 0-channels-tracked watcher.
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        cfg.this_host = load_this_host(&canonical)?;
        cfg.source_path = Some(canonical.clone());
        // v0.6.24: auto-default `paths.wsl_inbox` and `paths.windows_inbox`
        // when omitted. Lets new swarms drop the explicit `[paths]` block
        // entirely; both default to a swarm-config-relative location.
        // Existing swarms with explicit values are unchanged (the
        // explicit value wins).
        apply_path_defaults(&mut cfg, &canonical);
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse a config from a string (no file I/O). Used by tests so
    /// fixtures can be inline rather than requiring tempfiles for
    /// every scenario. Pure validation only — no path resolution
    /// beyond what's in the string.
    #[cfg(test)]
    pub fn load_str_for_test(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).with_context(|| "parsing inline test TOML")?;
        cfg.validate()?;
        Ok(cfg)
    }
}

fn load_this_host(config_path: &Path) -> Result<Option<String>> {
    let Some(parent) = config_path.parent() else {
        return Ok(None);
    };
    // v0.3.9+ name wins when both exist.
    let preferred = parent.join(THIS_HOST_FILE);
    let legacy = parent.join(THIS_HOST_FILE_LEGACY);
    let path = if preferred.exists() {
        preferred
    } else if legacy.exists() {
        legacy
    } else {
        return Ok(None);
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: ThisHostFile = toml::from_str(&text).with_context(|| {
        format!(
            "parsing {} (expected `this_host = \"...\"`)",
            path.display()
        )
    })?;
    Ok(Some(parsed.this_host))
}

/// v0.6.24: when the swarm config omits explicit `paths.wsl_inbox`
/// or `paths.windows_inbox`, fill in sensible per-host defaults so
/// the common case doesn't need an explicit `[paths]` block at all.
///
/// Defaults:
///   * `wsl_inbox`    → `<config_dir>/inbox`
///   * `windows_inbox` → `<USERPROFILE>\.giga\configs\<project>\inbox`
///     (resolved via cmd.exe interop on WSL, or `%USERPROFILE%`
///     env-var on native Windows)
///
/// Per-host overrides via `[[hosts]].paths` still win when set.
/// Explicit values in the top-level `[paths]` block also still win;
/// this fn only fills in MISSING values.
fn apply_path_defaults(cfg: &mut Config, canonical_config_path: &std::path::Path) {
    let config_dir = match canonical_config_path.parent() {
        Some(p) => p,
        None => return, // pathological: config has no parent, can't compute defaults
    };
    if cfg.paths.wsl_inbox.is_none() {
        cfg.paths.wsl_inbox = Some(config_dir.join("inbox"));
    }
    if cfg.paths.windows_inbox.is_none() {
        if let Some(profile_win) = resolve_windows_userprofile() {
            // Build the Windows-form path first, then translate to
            // WSL-form (/mnt/c/...) so the stored canonical matches
            // existing-swarm convention. On a native-Windows host,
            // fs_paths::to_host_fs translates back at access sites.
            let win_path = format!(
                "{profile_win}\\.giga\\configs\\{project}\\inbox",
                project = cfg.project.name,
            );
            // On WSL, store the /mnt/c/... form so init's mkdir +
            // the sync rsync paths work without further translation.
            // On native Windows, store the Windows-form (no
            // translation needed; fs_paths handles cross-FS use).
            if cfg!(target_os = "windows") {
                cfg.paths.windows_inbox = Some(PathBuf::from(win_path));
            } else if let Some(wsl_form) = crate::fs_paths::windows_to_wsl(&win_path) {
                cfg.paths.windows_inbox = Some(PathBuf::from(wsl_form));
            }
        }
        // If we can't resolve a Windows userprofile (e.g., pure Linux
        // without WSL interop), leave windows_inbox as None. Pure
        // WSL-only swarms don't need it; mixed-platform swarms on
        // Linux/macOS without interop should set it explicitly.
    }
}

/// Resolve the Windows-side `%USERPROFILE%` path (e.g.,
/// `C:\Users\Alice`) without assuming the host platform.
///
/// On native Windows: reads `USERPROFILE` env var directly.
/// On WSL: shells out to `cmd.exe /c echo %USERPROFILE%` via interop
/// and parses the result, trimming trailing CR/LF that Windows tools
/// add. Returns None if interop is unavailable or fails.
fn resolve_windows_userprofile() -> Option<String> {
    // Reads %USERPROFILE% directly on native Windows, or via cmd.exe
    // interop from a WSL distro. Shared impl in foundation::proc.
    crate::foundation::proc::cmd_exe_echo("USERPROFILE")
}

#[cfg(test)]
mod tests {
    use super::super::*;

    /// v0.3.9 Bug 5b: load_this_host prefers the new `.local.toml`
    /// name. Reader still accepts the legacy `this_host.toml` for
    /// backward compat with v0.3.8 and earlier swarms.
    #[test]
    fn load_prefers_this_host_local_toml_over_legacy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(
            &cfg_path,
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-new"
tailnet_hostname = "host-new.tail0.ts.net"
[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
host = "host-new"
"#,
        )
        .unwrap();
        // Both files present, different values — the new name wins.
        std::fs::write(
            tmp.path().join("this_host.local.toml"),
            "this_host = \"host-new\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-legacy\"\n",
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        // Wait — cfg validation requires this_host to be in [[hosts]].
        // host-legacy isn't in [[hosts]], so if the legacy file won,
        // validation would fail. The fact that load succeeded with
        // host-new in [[hosts]] proves the new name was picked.
        assert_eq!(cfg.this_host.as_deref(), Some("host-new"));
    }

    /// v0.3.9 Bug 5b: legacy `this_host.toml` is still accepted when
    /// `this_host.local.toml` is absent — backward compat for v0.3.8
    /// and earlier swarms that haven't been migrated.
    #[test]
    fn load_falls_back_to_legacy_this_host_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(
            &cfg_path,
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-x"
tailnet_hostname = "host-x.tail0.ts.net"
[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
host = "host-x"
"#,
        )
        .unwrap();
        // Only legacy file present.
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-x\"\n",
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.this_host.as_deref(), Some("host-x"));
    }

    /// v0.6.24: when [paths].wsl_inbox is omitted, default to
    /// <config_dir>/inbox so new swarms can drop the [paths] block
    /// entirely.
    #[test]
    fn load_defaults_wsl_inbox_to_config_dir_inbox_when_omitted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        // NOTE: no [paths] block at all.
        let body = r#"
[project]
name = "t"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#;
        std::fs::write(&cfg_path, body).unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        let expected = tmp.path().canonicalize().unwrap().join("inbox");
        assert_eq!(cfg.paths.wsl_inbox.as_ref().unwrap(), &expected);
    }

    /// v0.6.24: explicit [paths].wsl_inbox always wins over the default.
    /// Critical for existing swarms — they shouldn't get silently
    /// migrated to a new inbox location on first load.
    #[test]
    fn load_explicit_wsl_inbox_overrides_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        let explicit = "/some/explicit/inbox/path";
        let body = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = "{explicit}"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
        );
        std::fs::write(&cfg_path, body).unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(
            cfg.paths.wsl_inbox.as_ref().unwrap(),
            std::path::Path::new(explicit),
        );
    }

    /// v0.3.7 Bug 1 fix: when the config is loaded via a symlink (the
    /// canonical case for agents whose workdir contains a symlink to
    /// the swarm-dir TOML), this_host.toml MUST be found relative to
    /// the symlink's TARGET, not its parent. Pre-fix: agents armed
    /// `giga watch` from their workdir, config reload silently failed
    /// because workdir/this_host.toml didn't exist, watcher tracked 0
    /// channels, looked alive in Monitor but produced no events.
    #[cfg(unix)]
    #[test]
    fn load_resolves_this_host_relative_to_symlink_target() {
        let swarm = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();

        // Real config + this_host.toml live in swarm/
        let real_config = swarm.path().join("giga-harness.toml");
        std::fs::write(
            &real_config,
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"
[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "host-a"
"#,
        )
        .unwrap();
        std::fs::write(
            swarm.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();

        // Workdir has a symlink to the real config.
        let symlink = workdir.path().join("giga-harness.toml");
        std::os::unix::fs::symlink(&real_config, &symlink).unwrap();
        // Crucially, NO this_host.toml in the workdir — only the
        // symlink target's directory has it.
        assert!(!workdir.path().join("this_host.toml").exists());

        // Load via the symlink. Should resolve to swarm/this_host.toml.
        let cfg = Config::load(&symlink).unwrap();
        assert_eq!(
            cfg.this_host.as_deref(),
            Some("host-a"),
            "this_host must be resolved via the symlink target, not its parent"
        );
    }

    #[test]
    fn this_host_file_loaded_from_sibling() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        fs::write(&cfg_path, minimal_two_host()).unwrap();
        fs::write(tmp.path().join("this_host.toml"), "this_host = \"wsl-a\"\n").unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.this_host.as_deref(), Some("wsl-a"));
    }

    #[test]
    fn this_host_file_absent_is_ok_for_local_only_swarm() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        fs::write(&cfg_path, minimal()).unwrap(); // no [[hosts]]
        let cfg = Config::load(&cfg_path).unwrap();
        assert!(cfg.this_host.is_none());
        assert!(cfg.hosts.is_empty());
    }

    #[test]
    fn this_host_file_malformed_surfaces_error() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        fs::write(&cfg_path, minimal_two_host()).unwrap();
        // missing the `this_host` key
        fs::write(tmp.path().join("this_host.toml"), "host = \"wsl-a\"\n").unwrap();
        let err = Config::load(&cfg_path).unwrap_err();
        assert!(err.to_string().contains("this_host.toml"));
    }

    // Shared fixtures for load tests.
    fn minimal() -> &'static str {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"

[[agents]]
name = "b"
workdir = "/h/b"
role = "."
platform = "wsl"

[[channels]]
file = "a-b.md"
side = "wsl"
participants = ["a", "b"]
"#
    }

    fn minimal_two_host() -> String {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0000.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0000.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#
        .to_string()
    }
}
