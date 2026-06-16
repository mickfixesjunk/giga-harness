//! `giga set-swarm-boss <slug> [--unset]` — promote an existing
//! agent to swarm_boss, or demote one.
//!
//! Modifies the canonical `giga-harness.toml` in-place via `toml_edit`
//! so comments and formatting survive. Validation matches the config-
//! load path (`src/config.rs:707-738`): at most one swarm_boss per
//! host; must be platform=wsl. After the TOML write, re-runs `giga
//! init` so the boss's AGENTS.md picks up the supervision section.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use toml_edit::value;

use crate::config::edit::edit_then_validate_with_rollback;
use crate::config::Config;

pub struct Args {
    pub config: PathBuf,
    /// Agent slug to promote or demote.
    pub slug: String,
    /// Demote: set `swarm_boss = false` (and remove the field if
    /// false would be the default anyway). Without this flag, the
    /// agent is promoted to swarm_boss=true.
    pub unset: bool,
    /// Skip the `giga init` regen step. Useful when chaining commands
    /// or when the operator wants to inspect the TOML before scaffold
    /// regeneration.
    pub no_init: bool,
}

pub fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;
    let target = cfg
        .agents
        .iter()
        .find(|a| a.name == args.slug)
        .ok_or_else(|| {
            anyhow!(
                "agent `{}` not found in {}. Known: {}",
                args.slug,
                args.config.display(),
                cfg.agents
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        })?;

    if args.unset {
        if !target.swarm_boss {
            println!("`{}` is not a swarm_boss — nothing to do.", args.slug);
            return Ok(());
        }
        // Demote: no constraint check, this can only relax state.
        update_agent_swarm_boss_in_toml(&args.config, &args.slug, false)?;
        println!("✓ demoted `{}` (swarm_boss = false)", args.slug);
    } else {
        // Promote: validate.
        if target.swarm_boss {
            println!("`{}` is already swarm_boss — nothing to do.", args.slug);
            return Ok(());
        }
        if target.platform != "wsl" {
            return Err(anyhow!(
                "agent `{}` has platform `{}` — swarm_boss must be platform=wsl \
                 (sync + merger are POSIX-only)",
                args.slug,
                target.platform,
            ));
        }
        // At most one swarm_boss per host. host = target.host (None for
        // local-only swarms).
        let target_host = target.host.as_deref();
        let collision = cfg.agents.iter().find(|a| {
            a.name != args.slug
                && a.swarm_boss
                && match (a.host.as_deref(), target_host) {
                    (Some(h1), Some(h2)) => h1 == h2,
                    (None, None) => true,
                    _ => false,
                }
        });
        if let Some(other) = collision {
            return Err(anyhow!(
                "host `{}` already has a swarm_boss (`{}`). At most one per host — \
                 demote with `giga set-swarm-boss {} --unset` first.",
                target_host.unwrap_or("<local>"),
                other.name,
                other.name,
            ));
        }
        update_agent_swarm_boss_in_toml(&args.config, &args.slug, true)?;
        println!("✓ promoted `{}` to swarm_boss", args.slug);
    }

    // Regenerate AGENTS.md so the boss section is freshly injected
    // (or freshly removed on --unset).
    if !args.no_init {
        let canonical = std::fs::canonicalize(&args.config).unwrap_or(args.config.clone());
        println!();
        crate::scaffold::init::run(&canonical)?;
    } else {
        println!("(skipped `giga init` — run it manually to regenerate AGENTS.md)");
    }

    Ok(())
}

/// Edit the canonical TOML in-place: set or clear `swarm_boss` on
/// `[[agents]]` where name=slug. Routes through the shared rollback
/// helper (preserves comments + formatting, reload+validates, restores
/// the original on a would-be-invalid result). Mirrors
/// `teleport::update_toml_agent_host` and
/// `takeover::update_agent_runtime_in_toml`.
fn update_agent_swarm_boss_in_toml(
    config: &std::path::Path,
    slug: &str,
    promote: bool,
) -> Result<()> {
    edit_then_validate_with_rollback(config, |doc| {
        let agents = doc
            .get_mut("agents")
            .and_then(|i| i.as_array_of_tables_mut())
            .ok_or_else(|| anyhow!("[[agents]] not found in TOML"))?;
        let mut updated = false;
        for entry in agents.iter_mut() {
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                if name == slug {
                    if promote {
                        entry["swarm_boss"] = value(true);
                    } else {
                        // Demote: prefer removing the key entirely (so
                        // the TOML reads as "default" rather than
                        // explicit false), matching how bench_scheduler
                        // and other bool fields are written.
                        entry.remove("swarm_boss");
                    }
                    updated = true;
                    break;
                }
            }
        }
        if !updated {
            return Err(anyhow!(
                "agent `{slug}` not found in [[agents]] (TOML may have been edited concurrently)"
            ));
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_cfg(tmp: &std::path::Path, body: &str) -> PathBuf {
        let cfg_path = tmp.join("giga-harness.toml");
        fs::write(&cfg_path, body).unwrap();
        cfg_path
    }

    #[test]
    fn promote_writes_swarm_boss_true_and_preserves_comments() {
        let tmp = tempfile::TempDir::new().unwrap();
        let original = "# top comment\n\
                       [project]\n\
                       name = \"t\"\n\
                       \n\
                       [paths]\n\
                       wsl_inbox = \"/tmp/i\"\n\
                       \n\
                       # before alice\n\
                       [[agents]]\n\
                       name = \"alice\"\n\
                       workdir = \"/h/alice\"\n\
                       role = \"r\"\n\
                       platform = \"wsl\"\n";
        let cfg_path = write_cfg(tmp.path(), original);
        update_agent_swarm_boss_in_toml(&cfg_path, "alice", true).unwrap();
        let after = fs::read_to_string(&cfg_path).unwrap();
        assert!(after.contains("# top comment"));
        assert!(after.contains("# before alice"));
        assert!(after.contains("swarm_boss = true"));
    }

    #[test]
    fn demote_removes_swarm_boss_key_entirely() {
        let tmp = tempfile::TempDir::new().unwrap();
        let original = "[project]\nname = \"t\"\n\
                       [paths]\nwsl_inbox = \"/tmp/i\"\n\
                       [[agents]]\nname = \"alice\"\nworkdir = \"/h/alice\"\n\
                       role = \"r\"\nplatform = \"wsl\"\nswarm_boss = true\n";
        let cfg_path = write_cfg(tmp.path(), original);
        update_agent_swarm_boss_in_toml(&cfg_path, "alice", false).unwrap();
        let after = fs::read_to_string(&cfg_path).unwrap();
        assert!(
            !after.contains("swarm_boss"),
            "demote should remove the key, got:\n{after}",
        );
    }

    #[test]
    fn update_errors_when_slug_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let original = "[project]\nname = \"t\"\n\
                       [paths]\nwsl_inbox = \"/tmp/i\"\n\
                       [[agents]]\nname = \"alice\"\nworkdir = \"/h/alice\"\n\
                       role = \"r\"\nplatform = \"wsl\"\n";
        let cfg_path = write_cfg(tmp.path(), original);
        let err = update_agent_swarm_boss_in_toml(&cfg_path, "bob", true).unwrap_err();
        assert!(format!("{err}").contains("not found"), "{err}");
    }
}
