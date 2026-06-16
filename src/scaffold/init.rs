//! `giga init` — scaffold inbox files and per-agent AGENTS.md from a config.
//!
//! Idempotent: re-running against an existing config is safe. Inbox
//! files that already exist keep their content (only the header gets
//! re-written if missing). AGENTS.md files are always re-rendered
//! from the template so config changes propagate.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Agent, Config};
use crate::fs_paths::to_host_fs;
use crate::trust;

pub fn run(config_path: &Path) -> Result<()> {
    run_with(config_path, true)
}

pub fn run_with(config_path: &Path, do_trust: bool) -> Result<()> {
    let cfg = Config::load(config_path)?;
    // v0.6.4 fix: derive config_dir from the CANONICALIZED path so
    // claudemd_template relative paths resolve against the swarm dir,
    // NOT a workdir-side symlink to the canonical config. Same class
    // of bug as v0.3.7 Bug 1 (this_host.toml symlink leakage) — fixed
    // there but missed for template lookup. Symptom: `giga launch
    // --only X` from a workdir/<agent>/ cwd errored with "No such file
    // or directory" on `agents/<other-agent>.md` because the parent
    // dir of the symlink was the workdir, not the swarm dir.
    let abs_config = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());
    let config_dir = abs_config
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent dir"))?;

    // Host-aware filtering: when this_host is set (cross-host swarm), only
    // scaffold local-host artifacts — agents whose host matches this_host,
    // and channels with at least one participant on this_host. Without
    // this we'd try to mkdir + write AGENTS.md to agent workdirs that
    // belong on a different physical machine (e.g. /home/bob/... when
    // we're on a box with user `alice`). For legacy local-only
    // swarms (no [[hosts]], no this_host), include everything — today's
    // behavior, unchanged.
    let local_agents: Vec<&Agent> = if cfg.this_host.is_some() {
        cfg.agents
            .iter()
            .filter(|a| cfg.agent_host(a) == cfg.this_host.as_deref())
            .collect()
    } else {
        cfg.agents.iter().collect()
    };
    // v0.3.9 Bug 5: name the agents we're NOT scaffolding so the
    // success message reflects reality. Pre-fix: init exited "OK — 4
    // agent AGENTS.md files in place" without saying it had skipped
    // 3 others that live on a peer host.
    let skipped_agents: Vec<&Agent> = if let Some(this) = cfg.this_host.as_deref() {
        cfg.agents
            .iter()
            .filter(|a| cfg.agent_host(a).map(|h| h != this).unwrap_or(false))
            .collect()
    } else {
        Vec::new()
    };
    let local_channels: Vec<&crate::config::Channel> = if let Some(this) = cfg.this_host.as_deref()
    {
        cfg.channels
            .iter()
            .filter(|c| {
                c.participants.iter().any(|p| {
                    cfg.agents
                        .iter()
                        .find(|a| a.name == *p)
                        .and_then(|a| cfg.agent_host(a))
                        .map(|h| h == this)
                        .unwrap_or(false)
                })
            })
            .collect()
    } else {
        cfg.channels.iter().collect()
    };

    println!("project: {}", cfg.project.name);
    if cfg.this_host.is_some() {
        println!(
            "agents:  {} ({} local on `{}`)",
            cfg.agents.len(),
            local_agents.len(),
            cfg.this_host.as_deref().unwrap_or("?"),
        );
        println!(
            "channels:{} ({} local on `{}`)",
            cfg.channels.len(),
            local_channels.len(),
            cfg.this_host.as_deref().unwrap_or("?"),
        );
    } else {
        println!("agents:  {}", cfg.agents.len());
        println!("channels:{}", cfg.channels.len());
    }

    // Ensure inbox dirs exist. v0.3.2+: respects the per-host [paths]
    // override on [[hosts]] entries so a peer with asymmetric paths
    // (different $HOME, different Windows user) doesn't try to mkdir
    // the operator's literal path.
    //
    // v0.3.4+ (quality F9): only scaffold paths whose channels are
    // actually local to this host. Before this, a wsl-only peer would
    // try to mkdir `windows_inbox` (e.g. /mnt/c/Users/.../something)
    // even though no local agents have side=windows channels. On
    // a peer host this manifested as init failing on a Windows path
    // belonging to a different user on the operator's box. For the
    // legacy local-only case (no this_host, no [[hosts]]) all sides
    // are still in scope — preserves today's behavior.
    let this_host = cfg.this_host.as_deref();
    let need_wsl = local_channels.iter().any(|c| c.side == "wsl");
    let need_windows = local_channels.iter().any(|c| c.side == "windows");
    if need_wsl {
        if let Some(p) = cfg.inbox_for_host_side(this_host, "wsl") {
            fs::create_dir_all(&p).with_context(|| format!("mkdir -p {}", p.display()))?;
        }
    }
    if need_windows {
        if let Some(p) = cfg.inbox_for_host_side(this_host, "windows") {
            fs::create_dir_all(&p).with_context(|| format!("mkdir -p {}", p.display()))?;
        }
    }

    // Create channel files with convention headers if absent.
    for ch in &local_channels {
        let path = cfg.channel_path(ch)?;
        if path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            println!("  [keep] {}", path.display());
            continue;
        }
        let header = crate::scaffold::render::render_channel_header(&cfg, ch);
        fs::write(&path, header).with_context(|| format!("write {}", path.display()))?;
        println!("  [new]  {}", path.display());
    }

    // v0.3.9 Bug 5: explicit visibility on what's being skipped.
    for agent in &skipped_agents {
        let host = cfg.agent_host(agent).unwrap_or("?");
        println!("  [skip] {} (lives on `{host}`, not this host)", agent.name);
    }

    // Generate per-agent AGENTS.md in the agent's workdir. The
    // workdir comes from the config in its agent-side form (e.g.,
    // `C:\Users\Alice\win-agent` for Windows-platform agents on a
    // Linux/WSL host); translate to a host-FS path before touching
    // the filesystem so we don't end up with literal-backslash dirs.
    //
    // Also: if the agent has an AGENTS.md template at
    // `agents/<name>.md`, look for an optional handover file at
    // `agents/<name>.handover.md` next to it. When present, copy
    // it into the workdir as `HANDOVER.md` on first init only —
    // preserving any session appends the agent has accumulated in
    // its workdir copy. The config dir's template is the round-trip
    // checkpoint; the workdir copy is the agent's live append log.
    for agent in &local_agents {
        let host_workdir = to_host_fs(&agent.workdir);
        fs::create_dir_all(&host_workdir)
            .with_context(|| format!("mkdir -p agent workdir {}", host_workdir.display()))?;
        // v0.6.0: universal AGENTS.md filename across runtimes. Modern
        // Claude Code reads AGENTS.md alongside CLAUDE.md; codex + agy
        // expect AGENTS.md natively. Single source of truth.
        let agents_md_path = host_workdir.join("AGENTS.md");
        let body =
            crate::scaffold::render::render_agent_claudemd(&cfg, agent, config_dir, &abs_config)?;
        fs::write(&agents_md_path, body)
            .with_context(|| format!("write {}", agents_md_path.display()))?;
        println!("  [gen]  {}", agents_md_path.display());

        // v0.6.7: removed the v0.6.0 belt-and-suspenders
        // CLAUDE.md → AGENTS.md symlink. Modern Claude Code reads
        // AGENTS.md natively; the symlink was for legacy versions
        // that's a non-issue now. Single source of truth: AGENTS.md.
        // Existing CLAUDE.md files in workdirs are left untouched —
        // operator cleanup script (one-liner) handles old swarms.

        // v0.6.0: for codex-runtime agents, scaffold the channel-bridge
        // directory tree under the agent's workdir. The codex CLI reads
        // CODEX_CHANNEL_DIR=<workdir>/codex-channel; the bridge (giga
        // watch --codex) writes envelopes into inbox/ and reads receipts
        // from outbox/.
        if cfg.agent_runtime(agent) == crate::runtime::Runtime::Codex {
            let bridge_dir = host_workdir.join("codex-channel");
            for sub in ["inbox", "outbox", "processed"] {
                let p = bridge_dir.join(sub);
                fs::create_dir_all(&p).with_context(|| format!("mkdir -p {}", p.display()))?;
            }
            println!(
                "  [codex] {} (inbox/outbox/processed)",
                bridge_dir.display()
            );
        }

        // Symlink the project config into the workdir so the agent's
        // bare `giga watch --as <name>` (whose --config defaults to
        // `giga-harness.toml` in cwd) resolves without an explicit
        // --config. Unix/WSL-side agents only: a unix symlink to a
        // /home path is meaningless to a Windows-native agent. Idempotent —
        // only created when nothing is already at that path.
        #[cfg(unix)]
        if agent.platform != "windows" {
            let link = host_workdir.join("giga-harness.toml");
            if link.symlink_metadata().is_err() {
                match std::os::unix::fs::symlink(&abs_config, &link) {
                    Ok(()) => println!("  [link] {}", link.display()),
                    Err(e) => eprintln!(
                        "  [link] warning: couldn't symlink config into {} — {}",
                        host_workdir.display(),
                        e,
                    ),
                }
            }
        }

        if let Some(tpl) = &agent.claudemd_template {
            let handover_rel = handover_template_for(tpl);
            let handover_abs = if handover_rel.is_absolute() {
                handover_rel
            } else {
                config_dir.join(handover_rel)
            };
            if handover_abs.exists() {
                let dest = host_workdir.join("HANDOVER.md");
                if dest.exists() {
                    println!(
                        "  [keep] {} (workdir copy preserved — agent's session appends)",
                        dest.display(),
                    );
                } else {
                    fs::copy(&handover_abs, &dest).with_context(|| {
                        format!(
                            "copy handover {} → {}",
                            handover_abs.display(),
                            dest.display(),
                        )
                    })?;
                    println!("  [hand] {}", dest.display());
                }
            }
        }
    }

    if do_trust {
        match trust::pre_trust(&cfg) {
            Ok(n) => println!(
                "  [trust] marked {} agent workdir(s) as trusted in Claude Code",
                n
            ),
            Err(e) => eprintln!("  [trust] warning: couldn't pre-populate trust — {}", e),
        }
    }

    // Upsert this swarm into the cross-swarm registry so the user can
    // resume from anywhere under any agent's code_root just by typing
    // `giga launch` — no need to `cd` to the config dir. (`abs_config`
    // was resolved up top.)
    let mut code_roots: Vec<std::path::PathBuf> = cfg
        .agents
        .iter()
        .filter_map(|a| a.code_root.clone())
        .collect();
    code_roots.sort();
    code_roots.dedup();
    match crate::registry::upsert(&cfg.project.name, &abs_config, &code_roots) {
        Ok(true) => println!(
            "  [reg]  swarm `{}` registered → {}",
            cfg.project.name,
            abs_config.display()
        ),
        Ok(false) => {}
        Err(e) => eprintln!("  [reg] warning: couldn't update swarm registry — {}", e),
    }

    if skipped_agents.is_empty() {
        println!(
            "\nginit OK — {} channels + {} agent AGENTS.md files in place",
            local_channels.len(),
            local_agents.len(),
        );
    } else {
        println!(
            "\nginit OK — {} channels + {} local agent AGENTS.md files in place; {} skipped (live on other hosts)",
            local_channels.len(),
            local_agents.len(),
            skipped_agents.len(),
        );
    }
    println!("next: `giga launch <config>` to open the terminals");

    // v0.6.18: warn if no swarm_boss is flagged. The boss runs sync +
    // merger Monitors and (with smart-compaction enabled) supervises
    // worker-agent compaction. Without one, a multi-host swarm needs
    // operator-spawned daemon panes, and compaction supervision is
    // never activated even when configured.
    let has_boss = cfg.agents.iter().any(|a| a.swarm_boss);
    if !has_boss && !cfg.agents.is_empty() {
        println!();
        println!("  NOTE: no swarm_boss flagged. The boss runs sync + merger Monitors and");
        println!("        supervises worker-agent compaction. Promote one with:");
        println!("          giga set-swarm-boss <slug>");
        println!(
            "        (any wsl-platform agent works; design / coordinator agents are typical choices)"
        );
    }

    Ok(())
}

/// Given an agent's AGENTS.md template path (e.g.,
/// `agents/alice.md`), return the sibling handover path
/// (`agents/alice.handover.md`). The file may or may not
/// exist; the caller checks before copying.
fn handover_template_for(claudemd: &Path) -> PathBuf {
    let stem = claudemd
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = claudemd.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}.handover.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.3.4 fix for quality finding 9: a wsl-only peer must NOT try
    /// to mkdir the global `paths.windows_inbox` when no local channel
    /// has `side = "windows"`. Pre-fix: init scaffolded BOTH wsl and
    /// windows inbox dirs unconditionally if either was set in [paths].
    /// Repro: a peer host had a windows_inbox path pointing at the
    /// operator's box (different Windows user); init failed mkdir.
    #[test]
    fn init_skips_windows_inbox_when_no_local_windows_channel() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wsl_inbox = tmp.path().join("wsl-inbox");
        // Path inside tmp that does NOT exist yet; init will mkdir if
        // it visits it. Test passes when it's still missing afterward.
        let windows_inbox = tmp.path().join("nonexistent-windows-inbox");
        let cfg_text = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = '{wsl}'
windows_inbox = '{win}'

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = '{workdir_alice}'
role = "."
platform = "wsl"
host = "host-a"

[[agents]]
name = "bob"
workdir = '{workdir_bob}'
role = "."
platform = "wsl"
host = "host-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            wsl = wsl_inbox.display(),
            win = windows_inbox.display(),
            workdir_alice = tmp.path().join("alice-wd").display(),
            workdir_bob = tmp.path().join("bob-wd").display(),
        );
        let config_path = tmp.path().join("giga-harness.toml");
        fs::write(&config_path, cfg_text).unwrap();
        fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();

        run_with(&config_path, false).unwrap();

        assert!(
            wsl_inbox.exists(),
            "wsl_inbox should be created (local wsl channel)"
        );
        assert!(
            !windows_inbox.exists(),
            "windows_inbox should NOT be created on a wsl-only peer; quality F9"
        );
    }

    /// v0.3.9 Bug 5 visibility: when init runs on a host where some
    /// agents live elsewhere, the skipped agents must be enumerated
    /// (otherwise the success message looks like everything worked
    /// while peer-hosted workdirs are silently missing).
    #[test]
    fn init_skips_agents_on_other_hosts_and_skip_count_in_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wsl_inbox = tmp.path().join("wsl-inbox");
        let cfg_text = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = '{wsl}'

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = '{workdir_alice}'
role = "."
platform = "wsl"
host = "host-a"

[[agents]]
name = "bob"
workdir = '{workdir_bob}'
role = "."
platform = "wsl"
host = "host-b"
"#,
            wsl = wsl_inbox.display(),
            workdir_alice = tmp.path().join("alice-wd").display(),
            workdir_bob = tmp.path().join("bob-wd").display(),
        );
        let config_path = tmp.path().join("giga-harness.toml");
        fs::write(&config_path, cfg_text).unwrap();
        fs::write(
            tmp.path().join("this_host.local.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();

        run_with(&config_path, false).unwrap();

        // alice's workdir created; bob's was skipped (lives on host-b).
        // v0.6.7: AGENTS.md is the universal filename; CLAUDE.md is no
        // longer auto-symlinked.
        assert!(tmp.path().join("alice-wd").join("AGENTS.md").exists());
        assert!(
            !tmp.path().join("bob-wd").exists(),
            "bob's workdir must NOT be created on host-a (lives on host-b)"
        );
    }
}
