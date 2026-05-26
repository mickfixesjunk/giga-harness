//! Pre-populate Claude Code's per-project trust state so launched
//! agents don't get a "do you trust this folder?" prompt on first
//! run.
//!
//! Claude Code stores trust in `~/.claude.json` under
//! `projects.<absolute-path>.hasTrustDialogAccepted`. WSL-platform
//! agents use the WSL-side file (`$HOME/.claude.json`). Windows-
//! platform agents use the Windows-side file, accessible from WSL
//! at `/mnt/c/Users/<user>/.claude.json` — we derive the username
//! from the agent's workdir.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Agent, Config};
use crate::fs_paths;

/// Mark every agent workdir in `cfg` as trusted in the appropriate
/// Claude Code config file. Returns the number of entries
/// added or updated.
pub fn pre_trust(cfg: &Config) -> Result<usize> {
    let mut buckets: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();

    for agent in &cfg.agents {
        let (config_path, project_key) = trust_target(agent)?;
        buckets.entry(config_path).or_default().push(project_key);
        // Also trust code_root so the agent doesn't hit a trust prompt
        // when it cd's there to do actual code work.
        if let Some(cr) = &agent.code_root {
            let home = dirs_home()?;
            let claude_json = home.join(".claude.json");
            let key = fs_paths::to_host_fs(cr).to_string_lossy().to_string();
            buckets.entry(claude_json).or_default().push(key);
        }
    }

    let mut total = 0;
    for (config_path, keys) in buckets {
        total += update_claude_json(&config_path, &keys)
            .with_context(|| format!("updating {}", config_path.display()))?;
    }
    Ok(total)
}

/// Resolve the `~/.claude.json` path Claude would read for this
/// agent, plus the exact project-key string Claude uses (matches
/// what claude saw as its cwd).
fn trust_target(agent: &Agent) -> Result<(PathBuf, String)> {
    if agent.platform == "windows" {
        // Workdir form: "C:\Users\<user>\..." — extract the user, then
        // the Windows-side .claude.json lives at C:\Users\<user>\.claude.json,
        // which from WSL is /mnt/c/Users/<user>/.claude.json.
        let workdir_str = agent.workdir.to_string_lossy().to_string();
        let user = extract_windows_user(&workdir_str).ok_or_else(|| {
            anyhow::anyhow!(
                "agent `{}` is platform=windows but workdir `{}` isn't an absolute Windows path under C:\\Users\\<user>",
                agent.name,
                workdir_str,
            )
        })?;
        let config_path = if cfg!(unix) {
            PathBuf::from(format!("/mnt/c/Users/{user}/.claude.json"))
        } else {
            PathBuf::from(format!("C:\\Users\\{user}\\.claude.json"))
        };
        // Claude on Windows stores the project key as the Windows
        // path it saw at launch (forward slashes or backslashes
        // depending on version; we use the same form the workdir is
        // written in).
        Ok((config_path, workdir_str))
    } else {
        // WSL/Linux agent. Trust file is $HOME/.claude.json. Project
        // key is the absolute WSL path Claude will see as cwd.
        let home = dirs_home()?;
        let config_path = home.join(".claude.json");
        let key = fs_paths::to_host_fs(&agent.workdir)
            .to_string_lossy()
            .to_string();
        Ok((config_path, key))
    }
}

fn dirs_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME env var not set"))
}

fn extract_windows_user(workdir: &str) -> Option<String> {
    // Accept both `C:\Users\<user>\...` and `C:/Users/<user>/...`.
    let normalized = workdir.replace('\\', "/");
    let prefix = "C:/Users/";
    let lower = normalized.to_ascii_lowercase();
    let lower_prefix = prefix.to_ascii_lowercase();
    if !lower.starts_with(&lower_prefix) {
        return None;
    }
    let rest = &normalized[prefix.len()..];
    let user = rest.split('/').next()?;
    if user.is_empty() {
        None
    } else {
        Some(user.to_string())
    }
}

/// Open a Claude Code config file (creating a minimal one if
/// absent), set `projects[key].hasTrustDialogAccepted = true` for
/// every key in `keys`, save. Returns number of project entries
/// touched.
fn update_claude_json(path: &Path, keys: &[String]) -> Result<usize> {
    let mut root: serde_json::Value = if path.exists() {
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parse {} (expected JSON)", path.display()))?
    } else {
        // Create parent dir if needed.
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        serde_json::json!({})
    };

    // Ensure top-level is an object.
    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object", path.display());
    }

    // Ensure `projects` is an object.
    let projects = root
        .as_object_mut()
        .unwrap()
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    if !projects.is_object() {
        anyhow::bail!("{} has non-object `projects`", path.display());
    }
    let projects = projects.as_object_mut().unwrap();

    let mut touched = 0;
    for key in keys {
        let entry = projects
            .entry(key.clone())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            // Shouldn't normally happen — replace with object.
            *entry = serde_json::json!({});
        }
        let obj = entry.as_object_mut().unwrap();
        let was = obj.get("hasTrustDialogAccepted").cloned();
        obj.insert(
            "hasTrustDialogAccepted".to_string(),
            serde_json::Value::Bool(true),
        );
        if was != Some(serde_json::Value::Bool(true)) {
            touched += 1;
        }
    }

    let serialized =
        serde_json::to_string_pretty(&root).context("serialize updated claude.json")?;
    fs::write(path, serialized).with_context(|| format!("write {}", path.display()))?;
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn extract_user_from_windows_path() {
        assert_eq!(
            extract_windows_user("C:\\Users\\alice\\projects\\x"),
            Some("alice".to_string()),
        );
        assert_eq!(
            extract_windows_user("c:/users/bob/foo"),
            Some("bob".to_string()),
        );
        assert_eq!(extract_windows_user("/home/alice/x"), None);
    }

    #[test]
    fn update_claude_json_creates_file_when_absent() {
        // When .claude.json doesn't exist, pre_trust must create it
        // with the right shape. Without this guarantee, a brand-new
        // install would error on first init.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent").join(".claude.json");
        let touched = update_claude_json(&path, &["/Users/me/workdir".to_string()]).unwrap();
        assert_eq!(touched, 1);
        assert!(path.exists());
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["projects"]["/Users/me/workdir"]["hasTrustDialogAccepted"],
            serde_json::Value::Bool(true),
        );
    }

    #[test]
    fn update_claude_json_writes_multiple_keys_in_one_pass() {
        // The code_root pre-trust I added pushes additional keys into
        // the same call. This locks in that N keys all land correctly.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".claude.json");
        let keys = vec![
            "/Users/me/workdirs/design".to_string(),
            "/Users/me/code/myproj".to_string(),
            "/Users/me/workdirs/code".to_string(),
        ];
        let touched = update_claude_json(&path, &keys).unwrap();
        assert_eq!(touched, 3);
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for k in &keys {
            assert_eq!(
                written["projects"][k]["hasTrustDialogAccepted"],
                serde_json::Value::Bool(true),
                "key `{k}` was not trusted",
            );
        }
    }

    #[test]
    fn update_claude_json_is_idempotent() {
        // Running pre_trust twice in a row should not report any
        // touches the second time — important because `giga init`
        // (which triggers pre_trust) is idempotent by design.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".claude.json");
        let key = vec!["/Users/me/workdir".to_string()];
        let first = update_claude_json(&path, &key).unwrap();
        let second = update_claude_json(&path, &key).unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 0, "second pass should report 0 touches");
    }

    #[test]
    fn update_claude_json_preserves_unrelated_fields() {
        // ~/.claude.json holds far more than just trust state. The
        // pre_trust path must touch the projects[key].hasTrustDialogAccepted
        // and nothing else.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".claude.json");
        fs::write(
            &path,
            serde_json::to_string(&serde_json::json!({
                "userId": "abc-123",
                "projects": {
                    "/some/other/dir": {
                        "hasTrustDialogAccepted": true,
                        "lastSessionAt": "2026-01-01"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        update_claude_json(&path, &["/Users/me/new-workdir".to_string()]).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // Original fields survive:
        assert_eq!(written["userId"], "abc-123");
        assert_eq!(
            written["projects"]["/some/other/dir"]["lastSessionAt"],
            "2026-01-01",
        );
        // New key was added:
        assert_eq!(
            written["projects"]["/Users/me/new-workdir"]["hasTrustDialogAccepted"],
            serde_json::Value::Bool(true),
        );
    }
}
