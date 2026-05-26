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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Entry {
    pub name: String,
    pub config: PathBuf,
    #[serde(default)]
    pub code_roots: Vec<PathBuf>,
}

/// `~/.giga/swarms.toml` — absolute path. Created on first upsert.
pub fn path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME env var not set"))?;
    Ok(home.join(".giga").join("swarms.toml"))
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
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(reg).context("serialize registry")?;
    // Atomic-ish: write to .tmp then rename.
    let tmp = p.with_extension("toml.tmp");
    fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &p).with_context(|| format!("rename {} → {}", tmp.display(), p.display()))?;
    Ok(())
}

/// Insert or update an entry for `name` with the given config path and
/// code roots. Idempotent. Returns true if anything changed.
pub fn upsert(name: &str, config: &Path, code_roots: &[PathBuf]) -> Result<bool> {
    let mut reg = load()?;
    let new_entry = Entry {
        name: name.to_string(),
        config: config.to_path_buf(),
        code_roots: code_roots.to_vec(),
    };
    let mut changed = false;
    if let Some(existing) = reg.entries.iter_mut().find(|e| e.name == name) {
        if existing.config != new_entry.config || existing.code_roots != new_entry.code_roots {
            *existing = new_entry;
            changed = true;
        }
    } else {
        reg.entries.push(new_entry);
        changed = true;
    }
    if changed {
        save(&reg)?;
    }
    Ok(changed)
}

/// Given a starting directory (typically cwd), walk up parent dirs
/// looking for a swarm whose `code_roots` contains `start` or one of
/// its ancestors. Returns the config path of the first match.
///
/// Stale entries (config file missing) are skipped — keeps the
/// registry self-healing without a separate gc command.
pub fn find_by_cwd(start: &Path) -> Result<Option<PathBuf>> {
    let reg = load()?;
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut cursor: &Path = &start;
    loop {
        for entry in &reg.entries {
            for root in &entry.code_roots {
                let canon = root.canonicalize().unwrap_or_else(|_| root.clone());
                if canon == cursor {
                    if entry.config.exists() {
                        return Ok(Some(entry.config.clone()));
                    }
                }
            }
        }
        match cursor.parent() {
            Some(p) => cursor = p,
            None => return Ok(None),
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
    if let Some(found) = find_by_cwd(&cwd)? {
        return Ok(found);
    }
    // No config in cwd, no swarm registered for this directory or any
    // of its ancestors. Most likely: the user is in a project dir and
    // hasn't bootstrapped a swarm yet.
    anyhow::bail!(
        "no giga-harness.toml in {} and no swarm registered for this directory or any \
         ancestor.\n\n\
         If you haven't set up a swarm here yet, run:\n    \
         giga setup\n\n\
         If you have one elsewhere, either cd to its config dir or pass --config <path>.",
        cwd.display(),
    );
}
