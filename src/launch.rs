//! `giga launch` — spawn one terminal per agent and start each one
//! in their working directory with `claude` (if installed) ready to go.

use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::init;
use crate::terminal::{self, Multiplexer, Pane};

pub fn run(config_path: &Path, skip_init: bool, dry_run: bool) -> Result<()> {
    if !skip_init {
        init::run(config_path)?;
        println!();
    }

    let cfg = Config::load(config_path)?;
    let session = format!("giga-{}", cfg.project.name);

    let panes: Vec<Pane> = cfg
        .agents
        .iter()
        .map(|a| {
            let cwd = a.workdir.to_string_lossy().to_string();
            // Default cmd: drop into claude if available, else bash.
            // `claude` (Claude Code CLI) auto-loads CLAUDE.md from cwd.
            let cmd = "command -v claude >/dev/null && claude || bash".to_string();
            Pane {
                title: a.name.clone(),
                cwd,
                cmd,
                platform: a.platform.clone(),
            }
        })
        .collect();

    let mux = terminal::detect();
    println!("multiplexer: {mux:?}");
    println!("session:     {session}");
    println!("panes:       {}", panes.len());
    for p in &panes {
        println!("  - {} ({}) — cwd={}", p.title, p.platform, p.cwd);
    }

    if dry_run {
        println!("\n(dry-run — not spawning)");
        return Ok(());
    }

    if mux == Multiplexer::None {
        eprintln!("\nwarning: no multiplexer available — printing commands instead");
    }

    terminal::launch(mux, &panes, &session)?;
    Ok(())
}
