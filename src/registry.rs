//! Cross-swarm registry at `~/.giga/swarms.toml`.
//!
//! Lets the user run `giga launch` (or `validate`/`sweep`/`watch`) from
//! anywhere under a swarm's code root without remembering where its
//! config lives. `giga init` upserts each swarm into the registry; the
//! resolver walks up from cwd looking for a matching `code_roots` entry.
//!
//! Format:
//!
//! ```toml
//! [[swarms]]
//! name = "giga-mac-branch"
//! config = "/Users/me/giga-configs/giga-mac-branch/giga-harness.toml"
//! code_roots = ["/Users/me/code/giga-harness"]
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Registry {
    #[serde(default, rename = "swarms")]
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Entry {
    pub name: String,
    pub config: PathBuf,
    #[serde(default)]
    pub code_roots: Vec<PathBuf>,
    /// v0.6.49: archived swarms stay in the registry (so their
    /// configs + history are preserved and they can be brought
    /// back via un-archive) but are hidden from the UI's default
    /// swarm list. `serde(default)` keeps old swarms.toml files
    /// readable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}

/// `~/.giga/swarms.toml` — absolute path. Created on first upsert.
/// Falls back to `%USERPROFILE%` for native Windows shells (PowerShell,
/// cmd.exe) which don't set `$HOME` — mirrors `cursor::giga_home()`.
pub fn path() -> Result<PathBuf> {
    let giga_home = crate::foundation::dirs::giga_home()
        .ok_or_else(|| anyhow::anyhow!("neither $HOME nor %USERPROFILE% is set"))?;
    Ok(giga_home.join("swarms.toml"))
}

pub fn load() -> Result<Registry> {
    let p = path()?;
    if !p.exists() {
        return Ok(Registry::default());
    }
    let text = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    let reg: Registry =
        toml::from_str(&text).with_context(|| format!("parse {} as registry TOML", p.display()))?;
    Ok(reg)
}

pub fn save(reg: &Registry) -> Result<()> {
    let p = path()?;
    let text = toml::to_string_pretty(reg).context("serialize registry")?;
    crate::foundation::atomic_io::atomic_write(&p, text.as_bytes())
}

/// Insert or update an entry for `name` with the given config path and
/// code roots. Idempotent. Returns true if anything changed.
pub fn upsert(name: &str, config: &Path, code_roots: &[PathBuf]) -> Result<bool> {
    let mut reg = load()?;
    let changed = upsert_in(&mut reg, name, config, code_roots);
    if changed {
        save(&reg)?;
    }
    Ok(changed)
}

/// Pure upsert helper — operates on an in-memory Registry. Returns
/// true if `reg` was modified. Separated from `upsert` so unit tests
/// can exercise the merge logic without touching `~/.giga`.
pub fn upsert_in(reg: &mut Registry, name: &str, config: &Path, code_roots: &[PathBuf]) -> bool {
    if let Some(existing) = reg.entries.iter_mut().find(|e| e.name == name) {
        if existing.config != config || existing.code_roots != code_roots {
            existing.config = config.to_path_buf();
            existing.code_roots = code_roots.to_vec();
            // Preserve `archived` — upserting (e.g. via `giga init`)
            // shouldn't silently un-archive a swarm.
            return true;
        }
        return false;
    }
    reg.entries.push(Entry {
        name: name.to_string(),
        config: config.to_path_buf(),
        code_roots: code_roots.to_vec(),
        archived: false,
    });
    true
}

/// v0.6.49: flip the `archived` flag on a registered swarm. Returns
/// `Ok(true)` when the flag changed (write happened),
/// `Ok(false)` when it was already in the requested state.
/// Returns Err if the named swarm is not registered.
pub fn set_archived(name: &str, archived: bool) -> Result<bool> {
    let mut reg = load()?;
    let entry = reg
        .entries
        .iter_mut()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow::anyhow!("swarm `{name}` is not registered"))?;
    if entry.archived == archived {
        return Ok(false);
    }
    entry.archived = archived;
    save(&reg)?;
    Ok(true)
}

/// Given a starting directory (typically cwd), walk up parent dirs
/// looking for a swarm whose `code_roots` contains `start` or one of
/// its ancestors. Returns the config path of the first match.
///
/// Stale entries (config file missing) are skipped — keeps the
/// registry self-healing without a separate gc command.
pub fn find_by_cwd(start: &Path) -> Result<Option<PathBuf>> {
    let reg = load()?;
    Ok(find_match(&reg, start))
}

/// Pure lookup helper — given an already-loaded Registry, walk up
/// from `start` looking for a code_root match. Separated from
/// `find_by_cwd` so unit tests don't have to touch `~/.giga`.
pub fn find_match(reg: &Registry, start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut cursor: &Path = &start;
    loop {
        for entry in &reg.entries {
            for root in &entry.code_roots {
                let canon = root.canonicalize().unwrap_or_else(|_| root.clone());
                if canon == cursor && entry.config.exists() {
                    return Some(entry.config.clone());
                }
            }
        }
        match cursor.parent() {
            Some(p) => cursor = p,
            None => return None,
        }
    }
}

/// Used by command dispatch: if `provided` exists as-is, return it.
/// Otherwise try the registry. If that also fails AND the user was
/// relying on the default (`giga-harness.toml`), surface a helpful
/// error pointing them to `giga setup` instead of letting the
/// downstream "file not found" leak through. Explicit paths that
/// don't exist are still user errors and pass through unchanged.
pub fn resolve_config(provided: PathBuf) -> Result<PathBuf> {
    if provided.exists() {
        return Ok(provided);
    }
    let default_name = std::path::Path::new("giga-harness.toml");
    if provided != default_name {
        return Ok(provided);
    }
    let cwd = std::env::current_dir().context("getting cwd for registry lookup")?;
    // Walk up looking for an ancestral `giga-harness.toml`. This
    // matters for agents whose workdir lives under the config dir
    // (the canonical layout under `~/.giga/configs/<swarm>/workdirs/<agent>/`)
    // — the registry only indexes code_roots, but the config file
    // itself is sitting two levels up from the workdir. Without this
    // walk, `giga watch --as <slug>` from a workdir fails even
    // though the config is right there.
    {
        let canon_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
        let mut cursor: &Path = &canon_cwd;
        loop {
            let candidate = cursor.join("giga-harness.toml");
            if candidate.exists() {
                return Ok(candidate);
            }
            match cursor.parent() {
                Some(p) => cursor = p,
                None => break,
            }
        }
    }
    if let Some(found) = find_by_cwd(&cwd)? {
        return Ok(found);
    }
    // No config in cwd or any ancestor, no swarm registered for this
    // directory or any ancestor. Most likely: the user is in a project
    // dir and hasn't bootstrapped a swarm yet.
    anyhow::bail!(
        "no giga-harness.toml in {} and no swarm registered for this directory or any \
         ancestor.\n\n\
         If you haven't set up a swarm here yet, run:\n    \
         giga setup\n\n\
         If you have one elsewhere, either cd to its config dir or pass --config <path>.",
        cwd.display(),
    );
}

/// Outcome of [`resolve_config_or`] — lets dispatch arms with a
/// fallback path (e.g. `giga hosts` listing all swarms, `giga upgrade`
/// doing a bare install) read cleanly without re-deriving the
/// "was the user relying on the default `giga-harness.toml`?" check.
pub enum Resolved {
    /// `resolve_config` succeeded; here's the config path.
    Found(PathBuf),
    /// Resolution failed AND the user was relying on the default
    /// `giga-harness.toml`. Callers with a no-swarm fallback (list-all,
    /// bare install) should take it here; the carried error is what
    /// `resolve_config` would have surfaced if no fallback is wanted
    /// (e.g. `giga hosts --available`).
    DefaultMissing(anyhow::Error),
    /// Resolution failed for an explicitly-provided (non-default) path.
    /// This is a user error and should surface loud, not be swallowed
    /// by a fallback.
    ExplicitError(anyhow::Error),
}

/// Resolve `provided` like [`resolve_config`], but classify the
/// failure so dispatch arms can decide whether to take a no-swarm
/// fallback. Mirrors the old inline `was_default` checks: failure on
/// the default name → [`Resolved::DefaultMissing`]; failure on an
/// explicit path → [`Resolved::ExplicitError`].
pub fn resolve_config_or(provided: PathBuf) -> Resolved {
    let was_default = provided == PathBuf::from("giga-harness.toml");
    match resolve_config(provided) {
        Ok(c) => Resolved::Found(c),
        Err(e) if was_default => Resolved::DefaultMissing(e),
        Err(e) => Resolved::ExplicitError(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a Registry containing one entry pointing at a config file
    /// that actually exists on disk. Tests that need `find_match` to
    /// succeed must use real paths because the lookup checks
    /// `entry.config.exists()`.
    fn registry_with_real_config(name: &str, code_root: &Path) -> (TempDir, Registry) {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("giga-harness.toml");
        fs::write(&cfg, "[project]\nname = \"x\"\n[paths]\n").unwrap();
        let reg = Registry {
            entries: vec![Entry {
                name: name.to_string(),
                config: cfg,
                code_roots: vec![code_root.to_path_buf()],
                archived: false,
            }],
        };
        (tmp, reg)
    }

    #[test]
    fn upsert_in_appends_when_name_absent() {
        let mut reg = Registry::default();
        let changed = upsert_in(
            &mut reg,
            "alice",
            Path::new("/x/y.toml"),
            &[PathBuf::from("/code/a")],
        );
        assert!(changed);
        assert_eq!(reg.entries.len(), 1);
        assert_eq!(reg.entries[0].name, "alice");
        assert_eq!(reg.entries[0].config, PathBuf::from("/x/y.toml"));
    }

    #[test]
    fn upsert_in_updates_when_changed() {
        let mut reg = Registry {
            entries: vec![Entry {
                name: "alice".into(),
                config: PathBuf::from("/old.toml"),
                code_roots: vec![PathBuf::from("/code/a")],
                archived: false,
            }],
        };
        let changed = upsert_in(
            &mut reg,
            "alice",
            Path::new("/new.toml"),
            &[PathBuf::from("/code/a")],
        );
        assert!(changed);
        assert_eq!(reg.entries.len(), 1, "should not duplicate");
        assert_eq!(reg.entries[0].config, PathBuf::from("/new.toml"));
    }

    #[test]
    fn upsert_in_returns_false_when_unchanged() {
        let mut reg = Registry {
            entries: vec![Entry {
                name: "alice".into(),
                config: PathBuf::from("/x.toml"),
                code_roots: vec![PathBuf::from("/code/a")],
                archived: false,
            }],
        };
        let changed = upsert_in(
            &mut reg,
            "alice",
            Path::new("/x.toml"),
            &[PathBuf::from("/code/a")],
        );
        assert!(!changed, "identical upsert should report no change");
    }

    #[test]
    fn upsert_in_detects_code_root_changes() {
        let mut reg = Registry {
            entries: vec![Entry {
                name: "alice".into(),
                config: PathBuf::from("/x.toml"),
                code_roots: vec![PathBuf::from("/code/a")],
                archived: false,
            }],
        };
        let changed = upsert_in(
            &mut reg,
            "alice",
            Path::new("/x.toml"),
            &[PathBuf::from("/code/a"), PathBuf::from("/code/b")],
        );
        assert!(changed);
        assert_eq!(reg.entries[0].code_roots.len(), 2);
    }

    #[test]
    fn find_match_returns_exact_code_root() {
        let code_root = TempDir::new().unwrap();
        let (_tmp, reg) = registry_with_real_config("alice", code_root.path());
        let found = find_match(&reg, code_root.path()).expect("should match");
        assert!(found.ends_with("giga-harness.toml"));
    }

    #[test]
    fn find_match_walks_up_to_parent() {
        let code_root = TempDir::new().unwrap();
        let nested = code_root.path().join("src").join("submodule");
        fs::create_dir_all(&nested).unwrap();
        let (_tmp, reg) = registry_with_real_config("alice", code_root.path());
        let found = find_match(&reg, &nested).expect("should walk up and match parent");
        assert!(found.ends_with("giga-harness.toml"));
    }

    #[test]
    fn find_match_returns_none_when_no_entry_matches() {
        let code_root = TempDir::new().unwrap();
        let unrelated = TempDir::new().unwrap();
        let (_tmp, reg) = registry_with_real_config("alice", code_root.path());
        assert!(find_match(&reg, unrelated.path()).is_none());
    }

    #[test]
    fn find_match_skips_stale_entries_with_missing_config() {
        // Entry points at a config that doesn't exist — find_match must
        // treat it as if the entry weren't there (self-healing).
        let code_root = TempDir::new().unwrap();
        let reg = Registry {
            entries: vec![Entry {
                name: "ghost".into(),
                config: PathBuf::from("/nonexistent/giga-harness.toml"),
                code_roots: vec![code_root.path().to_path_buf()],
                archived: false,
            }],
        };
        assert!(find_match(&reg, code_root.path()).is_none());
    }

    #[test]
    fn find_match_returns_first_match_when_multiple_swarms_overlap() {
        let code_root = TempDir::new().unwrap();
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let cfg1 = tmp1.path().join("giga-harness.toml");
        let cfg2 = tmp2.path().join("giga-harness.toml");
        fs::write(&cfg1, "").unwrap();
        fs::write(&cfg2, "").unwrap();
        let reg = Registry {
            entries: vec![
                Entry {
                    name: "first".into(),
                    config: cfg1.clone(),
                    code_roots: vec![code_root.path().to_path_buf()],
                    archived: false,
                },
                Entry {
                    name: "second".into(),
                    config: cfg2,
                    code_roots: vec![code_root.path().to_path_buf()],
                    archived: false,
                },
            ],
        };
        let found = find_match(&reg, code_root.path()).expect("should match the first one");
        assert_eq!(found, cfg1);
    }

    #[test]
    fn registry_roundtrips_through_toml() {
        let original = Registry {
            entries: vec![Entry {
                name: "alice".into(),
                config: PathBuf::from("/some/config.toml"),
                code_roots: vec![PathBuf::from("/code/a"), PathBuf::from("/code/b")],
                archived: false,
            }],
        };
        let serialized = toml::to_string_pretty(&original).unwrap();
        let parsed: Registry = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].name, "alice");
        assert_eq!(parsed.entries[0].config, original.entries[0].config);
        assert_eq!(parsed.entries[0].code_roots, original.entries[0].code_roots);
    }

    #[test]
    fn registry_default_is_empty() {
        let reg = Registry::default();
        assert_eq!(reg.entries.len(), 0);
    }

    #[test]
    fn resolve_config_returns_existing_path_unchanged() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("custom.toml");
        fs::write(&cfg, "").unwrap();
        let result = resolve_config(cfg.clone()).unwrap();
        assert_eq!(result, cfg);
    }

    #[test]
    fn resolve_config_passes_through_nonexistent_explicit_path() {
        // When the user passes an explicit path that doesn't exist
        // (anything other than the default `giga-harness.toml`), the
        // resolver must NOT consult the registry — explicit paths are
        // user errors and should surface directly. Pre-existing
        // behavior; locked in here.
        let explicit = PathBuf::from("/definitely/does/not/exist/custom-name.toml");
        let result = resolve_config(explicit.clone()).unwrap();
        assert_eq!(result, explicit);
    }
}
