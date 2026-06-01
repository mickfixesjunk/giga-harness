//! `giga init` — scaffold inbox files and per-agent CLAUDE.md from a config.
//!
//! Idempotent: re-running against an existing config is safe. Inbox
//! files that already exist keep their content (only the header gets
//! re-written if missing). CLAUDE.md files are always re-rendered
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
    let config_dir = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent dir"))?;
    let abs_config = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());

    // Host-aware filtering: when this_host is set (cross-host swarm), only
    // scaffold local-host artifacts — agents whose host matches this_host,
    // and channels with at least one participant on this_host. Without
    // this we'd try to mkdir + write CLAUDE.md to agent workdirs that
    // belong on a different physical machine (e.g. /home/neo/... when
    // we're on a box with user `neomatrix`). For legacy local-only
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
    let local_channels: Vec<&crate::config::Channel> = if let Some(this) = cfg.this_host.as_deref() {
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
    // the operator's literal path. Falls back to global [paths] when
    // no per-host override is set (legacy behavior preserved).
    let this_host = cfg.this_host.as_deref();
    if let Some(p) = cfg.inbox_for_host_side(this_host, "wsl") {
        fs::create_dir_all(&p).with_context(|| format!("mkdir -p {}", p.display()))?;
    }
    if let Some(p) = cfg.inbox_for_host_side(this_host, "windows") {
        fs::create_dir_all(&p).with_context(|| format!("mkdir -p {}", p.display()))?;
    }

    // Create channel files with convention headers if absent.
    for ch in &local_channels {
        let path = cfg.channel_path(ch)?;
        if path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            println!("  [keep] {}", path.display());
            continue;
        }
        let header = render_channel_header(&cfg, ch);
        fs::write(&path, header).with_context(|| format!("write {}", path.display()))?;
        println!("  [new]  {}", path.display());
    }

    // Generate per-agent CLAUDE.md in the agent's workdir. The
    // workdir comes from the config in its agent-side form (e.g.,
    // `C:\Users\Audio\sdd-testwin` for Windows-platform agents on a
    // Linux/WSL host); translate to a host-FS path before touching
    // the filesystem so we don't end up with literal-backslash dirs.
    //
    // Also: if the agent has a CLAUDE.md template at
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
        let claudemd_path = host_workdir.join("CLAUDE.md");
        let body = render_agent_claudemd(&cfg, agent, config_dir)?;
        fs::write(&claudemd_path, body)
            .with_context(|| format!("write {}", claudemd_path.display()))?;
        println!("  [gen]  {}", claudemd_path.display());

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

    println!(
        "\nginit OK — {} channels + {} agent CLAUDE.md files in place",
        local_channels.len(),
        local_agents.len(),
    );
    println!("next: `giga launch <config>` to open the terminals");
    Ok(())
}

/// Given an agent's CLAUDE.md template path (e.g.,
/// `agents/superdeduper.md`), return the sibling handover path
/// (`agents/superdeduper.handover.md`). The file may or may not
/// exist; the caller checks before copying.
fn handover_template_for(claudemd: &Path) -> PathBuf {
    let stem = claudemd
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = claudemd.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}.handover.md"))
}

fn render_channel_header(cfg: &Config, ch: &crate::config::Channel) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# {} shared inbox\n\nProject: {}\n",
        ch.participants.join(" ↔ "),
        cfg.project.name,
    ));
    if let Some(purpose) = &ch.purpose {
        s.push_str(&format!("Purpose: {purpose}\n"));
    }
    s.push_str(
        "\n## Convention\n\n\
         Append-only. Each message gets a header block:\n\n\
         ```\n\
         ===\n\
         [<sender>] <subject> — <UTC ISO-8601 timestamp>\n\
         ===\n\n\
         <body>\n\n\
         WAITING ON: <agent-name> (<what's needed>)   ← OR\n\
         (Informational, no response required.)\n\
         ===\n\
         ```\n\n\
         Read the full file before replying. Don't edit anyone else's\n\
         messages — post corrections as new messages.\n\n",
    );
    s.push_str("## Self-arm your watcher (do this once per session)\n\n");
    s.push_str(
        "    Monitor(\n      description: \"giga inbox watcher\",\n      persistent: true,\n      command: \"giga watch --as <your-name>\"\n    )\n\n",
    );
    s.push_str("Replace `<your-name>` with whichever participant you are.\n");
    s.push_str("This single watcher tracks every channel you participate in via giga-harness.toml — not just this one. New channels added later are picked up automatically (~15s).\n");
    s.push_str("Stop with TaskStop when you no longer want events.\n");
    s
}

fn render_agent_claudemd(cfg: &Config, agent: &Agent, config_dir: &Path) -> Result<String> {
    // If the agent has an explicit template, use it verbatim (the
    // template author owns the content). Otherwise auto-generate.
    if let Some(tpl) = &agent.claudemd_template {
        let abs = if tpl.is_absolute() {
            tpl.clone()
        } else {
            config_dir.join(tpl)
        };
        let body = fs::read_to_string(&abs)
            .with_context(|| format!("reading agent CLAUDE.md template {}", abs.display()))?;
        return Ok(prepend_header(&body, agent));
    }

    // Auto-generated minimal CLAUDE.md.
    let mut s = String::new();
    s.push_str(&format!("# {} agent\n\n", agent.name));
    s.push_str(&format!("**Role:** {}\n\n", agent.role));
    s.push_str(&format!(
        "**Working directory:** `{}`\n\n",
        agent.workdir.display()
    ));
    s.push_str(&format!(
        "## Project pipeline\n\n_(from {} config)_\n\n",
        cfg.project.name
    ));

    // Channels this agent watches — auto-discovered at runtime by a
    // single config-aware watcher.
    let mine: Vec<&crate::config::Channel> = cfg
        .channels
        .iter()
        .filter(|ch| ch.participants.iter().any(|p| p == &agent.name))
        .collect();
    if !mine.is_empty() {
        let channel_list = mine
            .iter()
            .map(|c| format!("`{}`", c.file))
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str("## Session Start (do this first, every session)\n\n");
        s.push_str(&crate::templates::WATCHER.replace("{{AGENT}}", &agent.name));
        s.push_str(&format!(
            "\nYour channels: {channel_list}.\n\n",
        ));
    }

    // Bench-coord pointer.
    if let Some(bp) = &cfg.bench_protocol {
        if agent.bench_scheduler {
            s.push_str("## Bench coordination\n\nYou are the bench scheduler. ");
        } else {
            s.push_str("## Bench coordination\n\n");
        }
        s.push_str(&format!(
            "Slot pool: {}. Scheduler: `{}`. Before any CPU/IO-heavy work, post `bench-request` on the channel with the scheduler and wait for `bench-clear`. Standing-clearance — release with `bench-done`.\n\n",
            bp.slot_pool, bp.scheduler,
        ));
    }

    s.push_str("## Convention\n\n");
    s.push_str(crate::templates::CONVENTION.trim_end());
    s.push_str("\n\n");

    Ok(prepend_header(&s, agent))
}

fn prepend_header(body: &str, agent: &Agent) -> String {
    let mut out = format!(
        "<!--\n  Generated by giga-harness from this swarm's agent template\n  (its `claudemd_template`, or a built-in default if none is set).\n  Edits to THIS workdir copy are overwritten on the next `giga init`\n  or `giga launch` — to persist changes, edit the source template.\n  Agent: {}\n-->\n\n",
        agent.name,
    );
    out.push_str(&format!(
        "> **You are the `{slug}` agent.** Every response you make to the user in \
         this terminal MUST start with `[{slug}]` so the user can tell at a glance \
         which agent is talking. This applies to every assistant turn, not just \
         channel messages.\n\n",
        slug = agent.name,
    ));
    if let Some(cr) = &agent.code_root {
        out.push_str(&format!(
            "> **Code root:** `{}` \\\n> All code work (edits, builds, tests) happens here. `cd` to this directory before touching project files. Your workdir (`{}`) is only your launch context and CLAUDE.md home.\n\n",
            cr.display(),
            agent.workdir.display(),
        ));
    }
    out.push_str(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn config_with_one_agent(code_root: Option<&str>) -> Config {
        let cr_line = code_root
            .map(|p| format!("code_root = \"{p}\"\n"))
            .unwrap_or_default();
        let body = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
{cr_line}role = "scope owner"
platform = "wsl"
"#,
        );
        Config::load_str_for_test(&body).unwrap()
    }

    #[test]
    fn claudemd_always_contains_identity_callout() {
        let cfg = config_with_one_agent(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path()).unwrap();
        assert!(
            body.contains("You are the `design` agent"),
            "identity callout missing — agent won't self-identify in replies"
        );
        assert!(
            body.contains("[design]"),
            "reply-prefix instruction missing — agent won't prefix its replies"
        );
    }

    #[test]
    fn claudemd_contains_code_root_callout_when_set() {
        let cfg = config_with_one_agent(Some("/code/myproj"));
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path()).unwrap();
        assert!(
            body.contains("Code root:") && body.contains("/code/myproj"),
            "code_root callout missing or path wrong:\n{}",
            body,
        );
    }

    #[test]
    fn claudemd_omits_code_root_callout_when_unset() {
        let cfg = config_with_one_agent(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path()).unwrap();
        assert!(
            !body.contains("Code root:"),
            "code_root callout should not appear when field is unset",
        );
    }

    #[test]
    fn claudemd_preserves_template_body_under_callout_header() {
        // When the agent has a custom template, prepend_header injects
        // the callouts at the top but must preserve the template body
        // verbatim below them. (Templates are user-authored and should
        // never be silently modified.)
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        let tpl_path = agents_dir.join("design.md");
        let tpl_body = "# my custom template\n\nCustom body content the user wrote.\n";
        fs::write(&tpl_path, tpl_body).unwrap();

        let cfg_text = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"
claudemd_template = "agents/design.md"
"#;
        let cfg = Config::load_str_for_test(cfg_text).unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path()).unwrap();
        assert!(body.contains(tpl_body), "custom template body was modified");
        assert!(
            body.contains("You are the `design` agent"),
            "identity callout still injected for custom templates",
        );
    }

    #[test]
    fn claudemd_lists_channels_the_agent_participates_in() {
        // Auto-generated CLAUDE.md (no template) lists the agent's
        // channels so the watcher arming command is self-documenting.
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"

[[agents]]
name = "code"
workdir = "/h/code"
role = "."
platform = "wsl"

[[channels]]
file = "code-design.md"
side = "wsl"
participants = ["code", "design"]
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let claudemd = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path()).unwrap();
        assert!(claudemd.contains("code-design.md"));
        assert!(claudemd.contains("giga watch --as design"));
    }
}
