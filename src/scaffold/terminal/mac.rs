//! macOS-native backend — one Terminal.app window per agent.

use std::fs;
use std::process::Command;

use anyhow::{Context, Result};
use which::which;

use super::script::{chmod_executable, sanitize_for_filename, stagger_sleep};
use super::{LaunchSession, Pane, TerminalBackend};

pub struct MacTerminal;

impl TerminalBackend for MacTerminal {
    fn name(&self) -> &'static str {
        "mac-terminal"
    }

    fn launch(&self, panes: &[Pane], session: &LaunchSession) -> Result<()> {
        launch_mac_terminal(panes, &session.session_name, session.stagger_seconds)
    }
}

/// macOS-native launcher: one Terminal.app window per agent. Driven
/// via `osascript`. For each agent we write a tiny `cd <cwd> && <cmd>`
/// bash script to a temp dir and tell Terminal to `do script` it —
/// the file indirection sidesteps every layer of AppleScript+shell
/// quoting that would otherwise have to escape the intro prompt's
/// apostrophes, semicolons, and so on.
fn launch_mac_terminal(panes: &[Pane], session_name: &str, stagger_seconds: u64) -> Result<()> {
    if which("osascript").is_err() {
        anyhow::bail!("--terminal mac-terminal requires `osascript` (only available on macOS)");
    }

    // One temp dir per launch invocation. Using std::process::id keeps
    // concurrent giga launches from clobbering each other's scripts.
    let tmpdir = std::env::temp_dir().join(format!(
        "giga-launch-{}-{}",
        session_name,
        std::process::id()
    ));
    fs::create_dir_all(&tmpdir)
        .with_context(|| format!("creating launch script dir {}", tmpdir.display()))?;

    println!("opening {} Terminal.app window(s)...", panes.len());

    for (i, pane) in panes.iter().enumerate() {
        if i > 0 {
            stagger_sleep(stagger_seconds);
        }
        let script_path = tmpdir.join(format!("{}.sh", sanitize_for_filename(&pane.title)));
        // OSC 0 escape sequence sets the window title in Terminal.app
        // (and any other xterm-compatible terminal). The title persists
        // even if the agent's command exits; the user sees the agent
        // slug as the window/tab title at a glance.
        let body = format!(
            "#!/bin/bash\n# giga agent: {name}\nprintf '\\033]0;{name}\\007'\ncd {cwd} && {cmd}\n",
            name = pane.title,
            cwd = shell_escape::unix::escape(pane.cwd.as_str().into()),
            cmd = pane.cmd,
        );
        fs::write(&script_path, body)
            .with_context(|| format!("writing launch script {}", script_path.display()))?;
        chmod_executable(&script_path)?;

        // Open the script in a new Terminal.app window. `do script`
        // without a `in window <id>` clause defaults to a new window,
        // which is exactly what we want — one window per agent.
        // `activate` brings Terminal to the foreground so the user
        // sees it.
        let applescript = format!(
            "tell application \"Terminal\"\n    activate\n    do script \"{}\"\nend tell",
            script_path.display(),
        );
        let status = Command::new("osascript")
            .arg("-e")
            .arg(&applescript)
            .status()
            .context("invoking osascript")?;
        if !status.success() {
            anyhow::bail!("osascript failed for agent {}", pane.title);
        }
        println!("  - {} → {}", pane.title, script_path.display());
    }
    Ok(())
}
