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

    // The intro prompt is what each claude session processes the
    // moment it opens. Generic by design — per-agent behavior lives
    // in each agent's CLAUDE.md (which the prompt references).
    let intro = cfg
        .project
        .launch_intro_prompt
        .as_deref()
        .unwrap_or(DEFAULT_INTRO_PROMPT);

    let panes: Vec<Pane> = cfg
        .agents
        .iter()
        .map(|a| {
            let cwd = a.workdir.to_string_lossy().to_string();
            // Per-agent launch_cmd override wins; otherwise pick a
            // default that matches the platform and includes the
            // intro prompt so the agent starts working immediately.
            let cmd = a
                .launch_cmd
                .clone()
                .unwrap_or_else(|| default_cmd(&a.platform, intro));
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

/// Generic opening prompt sent to every claude session. Each
/// agent's own CLAUDE.md should contain a "Session Start" section
/// with the concrete actions to take (arm watchers, post intro,
/// etc.). Project configs can override via
/// `[project].launch_intro_prompt`.
///
/// We always launch with `claude -c`, which resumes the most-recent
/// session for the agent's cwd if one exists and starts fresh if
/// not. The prompt has to work in both cases — so it tells the
/// agent: if you were mid-task, finish it; otherwise do the
/// Session Start protocol.
const DEFAULT_INTRO_PROMPT: &str =
    "Session start. First, if ./HANDOVER.md exists in cwd, read it — it \
     carries cross-session / cross-machine state (recent decisions, \
     in-flight work, pickup instructions) that your conversation history \
     may not include. Then: if you were in the middle of a task in the \
     previous session (check your most recent assistant message), \
     continue from where you left off. Otherwise, follow the Session \
     Start protocol in CLAUDE.md — arm your inbox watchers, post a \
     one-line introduction on each of your channels announcing you're \
     online, then standby for messages.";

/// Platform-appropriate default shell command. Drops the agent into
/// `claude -c` so a prior session in that cwd gets resumed if one
/// exists (and falls back to a fresh session otherwise).
fn default_cmd(platform: &str, intro: &str) -> String {
    match platform {
        "windows" => {
            // PowerShell. Single-quote the intro and double any inner
            // single quotes (PS's `''` escape).
            let ps_intro = intro.replace('\'', "''");
            format!(
                "if (Get-Command claude -ErrorAction SilentlyContinue) {{ claude -c '{ps_intro}' }}",
            )
        }
        _ => {
            // POSIX bash. shell_escape gives us a safely-quoted form.
            let sh_intro = shell_escape::unix::escape(intro.into());
            format!("command -v claude >/dev/null && claude -c {sh_intro} || true")
        }
    }
}
