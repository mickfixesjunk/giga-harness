//! Cross-platform terminal multiplexer detection + spawn.
//!
//! Strategy (in priority order):
//!   1. Windows Terminal (`wt.exe`) — best UX on Windows; one window
//!      with N tabs, mixed wsl/windows panes via `-p` profiles.
//!   2. tmux — Linux fallback; one session, N windows.
//!   3. None — fall back to printing the per-agent commands so the
//!      user can paste them into separate terminals manually.

use std::process::Command;

use anyhow::{Context, Result};
use which::which;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    WindowsTerminal,
    Tmux,
    None,
}

pub fn detect() -> Multiplexer {
    // Inside WSL, `wt.exe` is on PATH via Windows interop.
    if which("wt.exe").is_ok() || which("wt").is_ok() {
        return Multiplexer::WindowsTerminal;
    }
    if which("tmux").is_ok() {
        return Multiplexer::Tmux;
    }
    Multiplexer::None
}

pub struct Pane {
    pub title: String,
    /// Working directory before the command runs.
    pub cwd: String,
    /// Shell command to execute. Already shell-escaped where needed.
    pub cmd: String,
    /// "wsl" or "windows" — affects which wt profile we pick.
    pub platform: String,
}

pub fn launch(mux: Multiplexer, panes: &[Pane], session_name: &str) -> Result<()> {
    match mux {
        Multiplexer::WindowsTerminal => launch_wt(panes, session_name),
        Multiplexer::Tmux => launch_tmux(panes, session_name),
        Multiplexer::None => launch_print(panes),
    }
}

fn launch_wt(panes: &[Pane], session_name: &str) -> Result<()> {
    // Compose a single `wt.exe` invocation that opens one window
    // with one tab per agent.
    //
    // Layout we use:
    //   wt.exe new-tab --title <t1> -p <profile> --suppressApplicationTitle wsl.exe -d Ubuntu bash -lc "cd <cwd> && <cmd>; bash" ;
    //       new-tab --title <t2> ...
    //
    // For windows-side agents we use the default PowerShell profile.
    let exe = if which("wt.exe").is_ok() { "wt.exe" } else { "wt" };
    let mut cmd = Command::new(exe);
    cmd.arg("--window").arg(session_name);

    for (i, pane) in panes.iter().enumerate() {
        if i > 0 {
            cmd.arg(";");
        }
        cmd.arg("new-tab")
            .arg("--title")
            .arg(&pane.title)
            .arg("--suppressApplicationTitle");

        if pane.platform == "windows" {
            // Default profile, run powershell with command.
            cmd.arg("powershell.exe")
                .arg("-NoExit")
                .arg("-Command")
                .arg(format!(
                    "Set-Location -LiteralPath '{}'; {}",
                    pane.cwd.replace('\'', "''"),
                    pane.cmd,
                ));
        } else {
            // WSL profile. Use `wsl.exe ... bash -lc` so the agent inherits a login shell.
            cmd.arg("wsl.exe")
                .arg("bash")
                .arg("-lc")
                .arg(format!(
                    "cd {} && {} ; exec bash",
                    shell_escape::unix::escape(pane.cwd.as_str().into()),
                    pane.cmd,
                ));
        }
    }

    let status = cmd.status().context("spawning Windows Terminal")?;
    if !status.success() {
        anyhow::bail!("wt.exe exited with status {status}");
    }
    Ok(())
}

fn launch_tmux(panes: &[Pane], session_name: &str) -> Result<()> {
    // Kill any prior session with this name (idempotent re-launch).
    let _ = Command::new("tmux").args(["kill-session", "-t", session_name]).status();

    for (i, pane) in panes.iter().enumerate() {
        let inner = format!(
            "cd {} && {} ; exec bash",
            shell_escape::unix::escape(pane.cwd.as_str().into()),
            pane.cmd,
        );
        if i == 0 {
            let status = Command::new("tmux")
                .args(["new-session", "-d", "-s", session_name, "-n", &pane.title, "bash", "-lc", &inner])
                .status()
                .context("starting tmux session")?;
            if !status.success() {
                anyhow::bail!("tmux new-session failed");
            }
        } else {
            let status = Command::new("tmux")
                .args(["new-window", "-t", session_name, "-n", &pane.title, "bash", "-lc", &inner])
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

fn launch_print(panes: &[Pane]) -> Result<()> {
    println!("\nNo terminal multiplexer detected (wt.exe / tmux).");
    println!("Run each of these in its own terminal:\n");
    for p in panes {
        println!("# {} ({})", p.title, p.platform);
        println!("cd {} && {}\n", p.cwd, p.cmd);
    }
    Ok(())
}
