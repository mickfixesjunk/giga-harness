//! `giga launch` — spawn one terminal per agent and start each one
//! in their working directory with `claude` (if installed) ready to go.

use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::init;
use crate::terminal::{self, Multiplexer, Pane};

pub fn run(
    config_path: &Path,
    skip_init: bool,
    dry_run: bool,
    only: &[String],
    new_window: bool,
    terminal: &str,
) -> Result<()> {
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

    // If --only was passed, narrow the agent list to that set and
    // error on any name the config doesn't know — typos here are
    // common and silent skips would be worse than a hard failure.
    let agents_iter: Box<dyn Iterator<Item = &_>> = if only.is_empty() {
        Box::new(cfg.agents.iter())
    } else {
        let known: Vec<&str> = cfg.agents.iter().map(|a| a.name.as_str()).collect();
        let unknown: Vec<&str> = only
            .iter()
            .map(String::as_str)
            .filter(|n| !known.contains(n))
            .collect();
        if !unknown.is_empty() {
            anyhow::bail!(
                "--only names unknown agent(s): {} — known agents: {}",
                unknown.join(", "),
                known.join(", "),
            );
        }
        Box::new(
            cfg.agents
                .iter()
                .filter(|a| only.iter().any(|n| n == &a.name)),
        )
    };

    let panes: Vec<Pane> = agents_iter
        .map(|a| {
            let cwd = a.workdir.to_string_lossy().to_string();
            // Per-agent launch_cmd override wins; otherwise pick a
            // default that matches the platform and includes the
            // intro prompt so the agent starts working immediately.
            // Self-identification preamble: gives every reply a `[slug]`
            // prefix so the user can tell at a glance which terminal
            // window they're reading. Reinforced in the agent's CLAUDE.md
            // header so the rule survives session restarts.
            let identity = format!(
                "You are the `{slug}` agent in this giga-harness swarm. EVERY response \
                 you make to the user in this terminal MUST start with `[{slug}]` so the \
                 user knows which agent is talking — this applies to every assistant turn, \
                 not just channel messages. ",
                slug = a.name,
            );
            let agent_intro = if let Some(cr) = &a.code_root {
                format!(
                    "{identity}{intro} Your code root (where all code work happens) is `{cr}` — cd there before editing files.",
                    identity = identity,
                    intro = intro,
                    cr = cr.display(),
                )
            } else {
                format!("{identity}{intro}")
            };
            let cmd = a
                .launch_cmd
                .clone()
                .unwrap_or_else(|| default_cmd(&a.platform, &agent_intro));
            Pane {
                title: a.name.clone(),
                cwd,
                cmd,
                platform: a.platform.clone(),
            }
        })
        .collect();

    let incremental = !only.is_empty();
    let mux = terminal::parse_override(terminal).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown --terminal value `{}` — valid: auto, tmux, mac-terminal, wt, print",
            terminal
        )
    })?;
    let mut tags = Vec::new();
    if incremental {
        tags.push("incremental");
    }
    if new_window {
        tags.push("new-window");
    }
    let tag_str = if tags.is_empty() {
        String::new()
    } else {
        format!(" ({})", tags.join(", "))
    };
    println!("multiplexer: {mux:?}");
    println!("session:     {session}{tag_str}");
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

    terminal::launch(mux, &panes, &session, incremental, new_window)?;
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

/// Platform-appropriate default shell command. Tries `claude -c`
/// first to resume the most-recent session in this cwd; falls back
/// to `claude` (fresh session) if `-c` fails — which it does on the
/// first launch of a brand-new agent, where no prior session exists.
/// (Claude Code's `-c` errors with "No conversation found to
/// continue" rather than starting fresh, so we have to handle that
/// here.)
fn default_cmd(platform: &str, intro: &str) -> String {
    match platform {
        "windows" => {
            // PowerShell. Single-quote the intro and double any inner
            // single quotes (PS's `''` escape). Wrap the resume + new
            // attempts so a `-c` failure falls through to a fresh
            // session with the same intro.
            let ps_intro = intro.replace('\'', "''");
            format!(
                "if (Get-Command claude -ErrorAction SilentlyContinue) {{ \
                   claude -c '{ps_intro}'; \
                   if ($LASTEXITCODE -ne 0) {{ claude '{ps_intro}' }} \
                 }}",
            )
        }
        _ => {
            // POSIX bash. shell_escape gives us a safely-quoted form.
            // Group the resume + new attempts with `{ ... ; }` so the
            // outer `|| true` only fires if claude is missing entirely.
            let sh_intro = shell_escape::unix::escape(intro.into());
            format!(
                "command -v claude >/dev/null && \
                 {{ claude -c {sh_intro} || claude {sh_intro} ; }} || true",
            )
        }
    }
}
