//! tmux backend — one session, N windows.

use std::process::Command;

use anyhow::{Context, Result};

use super::script::stagger_sleep;
use super::{LaunchSession, Pane, TerminalBackend};

pub struct Tmux;

impl TerminalBackend for Tmux {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn launch(&self, panes: &[Pane], session: &LaunchSession) -> Result<()> {
        launch_tmux(
            panes,
            &session.session_name,
            session.incremental,
            session.stagger_seconds,
        )
    }
}

fn launch_tmux(
    panes: &[Pane],
    session_name: &str,
    incremental: bool,
    stagger_seconds: u64,
) -> Result<()> {
    // When incremental (--only), attach to an existing session if one
    // is alive and add windows to it; otherwise create a new session.
    // When not incremental (full launch), preserve the historical
    // behavior of killing any prior session for a clean rebuild.
    let session_alive = Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !incremental {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session_name])
            .status();
    }

    let mut create_session = !incremental || !session_alive;

    for (i, pane) in panes.iter().enumerate() {
        if i > 0 {
            stagger_sleep(stagger_seconds);
        }
        // OSC 0 sets the terminal window title to the agent name.
        // tmux will pass it through to the outer terminal when
        // `set-titles on` (enabled below for new sessions), so the
        // macOS Terminal/iTerm window chrome reflects the active
        // tmux window's agent name.
        let inner = format!(
            "printf '\\033]0;{name}\\007' ; cd {cwd} && {cmd} ; exec bash",
            name = pane.title,
            cwd = shell_escape::unix::escape(pane.cwd.as_str().into()),
            cmd = pane.cmd,
        );
        if create_session {
            let status = Command::new("tmux")
                .args([
                    "new-session",
                    "-d",
                    "-s",
                    session_name,
                    "-n",
                    &pane.title,
                    "bash",
                    "-lc",
                    &inner,
                ])
                .status()
                .context("starting tmux session")?;
            if !status.success() {
                anyhow::bail!("tmux new-session failed");
            }
            // Tell tmux to forward window-name changes to the outer
            // terminal's title bar. Without this, the macOS Terminal
            // window keeps whatever title it started with even when
            // you switch tmux windows.
            let _ = Command::new("tmux")
                .args(["set-option", "-t", session_name, "set-titles", "on"])
                .status();
            let _ = Command::new("tmux")
                .args(["set-option", "-t", session_name, "set-titles-string", "#W"])
                .status();
            create_session = false;
        } else {
            let status = Command::new("tmux")
                .args([
                    "new-window",
                    "-t",
                    session_name,
                    "-n",
                    &pane.title,
                    "bash",
                    "-lc",
                    &inner,
                ])
                .status()
                .context("opening tmux window")?;
            if !status.success() {
                anyhow::bail!("tmux new-window failed");
            }
        }
    }

    println!("tmux session ready — attach with:");
    println!("    tmux attach -t {session_name}");
    Ok(())
}
