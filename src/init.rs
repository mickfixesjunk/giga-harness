//! `giga init` — scaffold inbox files and per-agent CLAUDE.md from a config.
//!
//! Idempotent: re-running against an existing config is safe. Inbox
//! files that already exist keep their content (only the header gets
//! re-written if missing). CLAUDE.md files are always re-rendered
//! from the template so config changes propagate.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Config, Agent};
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

    println!("project: {}", cfg.project.name);
    println!("agents:  {}", cfg.agents.len());
    println!("channels:{}", cfg.channels.len());

    // Ensure inbox dirs exist
    if let Some(p) = &cfg.paths.wsl_inbox {
        fs::create_dir_all(p).with_context(|| format!("mkdir -p {}", p.display()))?;
    }
    if let Some(p) = &cfg.paths.windows_inbox {
        fs::create_dir_all(p).with_context(|| format!("mkdir -p {}", p.display()))?;
    }

    // Create channel files with convention headers if absent.
    for ch in &cfg.channels {
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
    // its workdir copy. The configs repo is the round-trip
    // checkpoint; the workdir copy is the agent's live append log.
    for agent in &cfg.agents {
        let host_workdir = to_host_fs(&agent.workdir);
        fs::create_dir_all(&host_workdir)
            .with_context(|| format!("mkdir -p agent workdir {}", host_workdir.display()))?;
        let claudemd_path = host_workdir.join("CLAUDE.md");
        let body = render_agent_claudemd(&cfg, agent, config_dir)?;
        fs::write(&claudemd_path, body)
            .with_context(|| format!("write {}", claudemd_path.display()))?;
        println!("  [gen]  {}", claudemd_path.display());

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
            Ok(n) => println!("  [trust] marked {} agent workdir(s) as trusted in Claude Code", n),
            Err(e) => eprintln!("  [trust] warning: couldn't pre-populate trust — {}", e),
        }
    }

    println!("\nginit OK — {} channels + {} agent CLAUDE.md files in place", cfg.channels.len(), cfg.agents.len());
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
    s.push_str(&format!(
        "    Monitor(\n      description: \"{} watcher\",\n      persistent: true,\n      command: \"giga watch {} --as <your-name>\"\n    )\n\n",
        ch.participants.join(" ↔ "),
        ch.file,
    ));
    s.push_str("Replace `<your-name>` with whichever participant you are.\n");
    s.push_str("Stop with TaskStop when you no longer want events.\n");
    s
}

fn render_agent_claudemd(
    cfg: &Config,
    agent: &Agent,
    config_dir: &Path,
) -> Result<String> {
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
    s.push_str(&format!("**Working directory:** `{}`\n\n", agent.workdir.display()));
    s.push_str(&format!("## Project pipeline\n\n_(from {} config)_\n\n", cfg.project.name));

    // Channels this agent watches.
    let mine: Vec<&crate::config::Channel> = cfg
        .channels
        .iter()
        .filter(|ch| ch.participants.iter().any(|p| p == &agent.name))
        .collect();
    if !mine.is_empty() {
        s.push_str("## Channels you watch\n\n");
        s.push_str("Arm at session start:\n\n```\n");
        for ch in &mine {
            let p = cfg.channel_path(ch).unwrap_or_else(|_| ch.file.clone().into());
            s.push_str(&format!(
                "Monitor(persistent: true, command: \"giga watch {} --as {}\")\n",
                p.display(),
                agent.name,
            ));
        }
        s.push_str("```\n\n");
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

    s.push_str(&format!(
        "## Convention\n\nEvery channel message ends with either:\n\n* `WAITING ON: <agent> (<what's needed>)` — if a reply is expected.\n* `Informational, no response required.` — otherwise.\n\nAmbiguous closings stall the pipeline. Use the tag.\n\n",
    ));

    Ok(prepend_header(&s, agent))
}

fn prepend_header(body: &str, agent: &Agent) -> String {
    format!(
        "<!--\n  Generated by giga-harness. The source template lives in the\n  configs repo (giga-harness-configs). Edits to THIS file in the\n  agent's workdir will be overwritten on the next `giga init` or\n  `giga launch`. To persist, modify the source template.\n  Agent: {}\n-->\n\n{}",
        agent.name, body,
    )
}
