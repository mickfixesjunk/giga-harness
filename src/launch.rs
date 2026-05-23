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
            // Per-agent override wins; otherwise pick a default that
            // matches the shell we're about to spawn in.
            let cmd = a.launch_cmd.clone().unwrap_or_else(|| default_cmd(&a.platform));
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

/// Platform-appropriate default shell command. The Claude Code CLI
/// (`claude`) auto-loads `CLAUDE.md` from cwd, so if it's installed
/// we drop the agent straight into it. Otherwise fall back to an
/// interactive shell.
fn default_cmd(platform: &str) -> String {
    match platform {
        "windows" => {
            // PowerShell 5.1+ syntax. `Get-Command -ea SilentlyContinue`
            // returns $null if claude isn't on PATH, which the `if`
            // treats as falsy.
            "if (Get-Command claude -ErrorAction SilentlyContinue) { claude }".to_string()
        }
        _ => {
            // POSIX bash. `command -v` is portable; `exec bash` keeps
            // the shell open after claude exits so the agent can take
            // over manually if needed.
            "command -v claude >/dev/null && claude || true".to_string()
        }
    }
}
