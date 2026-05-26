//! Cross-platform terminal multiplexer detection + spawn.
//!
//! Strategy (in priority order, auto-detected):
//!   1. Windows Terminal (`wt.exe`) — best UX on Windows; one window
//!      with N tabs, mixed wsl/windows panes via `-p` profiles.
//!   2. tmux — Linux fallback; one session, N windows.
//!   3. None — fall back to printing the per-agent commands so the
//!      user can paste them into separate terminals manually.
//!
//! `MacTerminal` opens one Terminal.app window per agent via
//! `osascript`. Opt-in only (`giga launch --terminal mac-terminal`);
//! never auto-detected so existing tmux users on macOS keep their
//! current behavior.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use which::which;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    WindowsTerminal,
    Tmux,
    MacTerminal,
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

/// Parse a `--terminal` flag value. `auto` means use `detect()`.
/// Returns None for unknown values so the caller can surface a
/// helpful error.
pub fn parse_override(s: &str) -> Option<Multiplexer> {
    match s {
        "auto" => Some(detect()),
        "wt" | "windows-terminal" => Some(Multiplexer::WindowsTerminal),
        "tmux" => Some(Multiplexer::Tmux),
        "mac-terminal" | "mac" => Some(Multiplexer::MacTerminal),
        "print" | "none" => Some(Multiplexer::None),
        _ => None,
    }
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

pub fn launch(
    mux: Multiplexer,
    panes: &[Pane],
    session_name: &str,
    incremental: bool,
    new_window: bool,
) -> Result<()> {
    match mux {
        // wt.exe's `--window <name>` flag already does the right thing
        // for the default case: reuse the existing window with that
        // name (adds tabs) or create one if absent. `new_window`
        // overrides that with `-w new` to force a fresh wt window —
        // matters when the original launch window has been torn up
        // (tabs dragged into separate windows) and the name no longer
        // points anywhere useful. The incremental distinction only
        // matters for tmux.
        Multiplexer::WindowsTerminal => launch_wt(panes, session_name, new_window),
        Multiplexer::Tmux => launch_tmux(panes, session_name, incremental),
        Multiplexer::MacTerminal => launch_mac_terminal(panes, session_name),
        Multiplexer::None => launch_print(panes),
    }
}

/// Escape every `;` in `s` as `\;` so wt.exe doesn't treat it as
/// a tab separator. Already-escaped `\;` is left as-is.
fn escape_wt_semicolons(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut prev_backslash = false;
    for ch in s.chars() {
        if ch == ';' && !prev_backslash {
            out.push_str("\\;");
        } else {
            out.push(ch);
        }
        prev_backslash = ch == '\\';
    }
    out
}

fn launch_wt(panes: &[Pane], session_name: &str, new_window: bool) -> Result<()> {
    // Compose a single `wt.exe` invocation that opens one window
    // with one tab per agent.
    //
    // Layout we use:
    //   wt.exe new-tab --title <t1> -p <profile> --suppressApplicationTitle wsl.exe -d Ubuntu bash -lc "cd <cwd> && <cmd>; bash" ;
    //       new-tab --title <t2> ...
    //
    // For windows-side agents we use the default PowerShell profile.
    let exe = if which("wt.exe").is_ok() {
        "wt.exe"
    } else {
        "wt"
    };
    let mut cmd = Command::new(exe);
    // `-w new` forces a fresh wt window every time; `--window <name>`
    // reuses an existing window with that name (or creates one).
    if new_window {
        cmd.arg("-w").arg("new");
    } else {
        cmd.arg("--window").arg(session_name);
    }

    for (i, pane) in panes.iter().enumerate() {
        if i > 0 {
            cmd.arg(";");
        }
        cmd.arg("new-tab")
            .arg("--title")
            .arg(&pane.title)
            .arg("--suppressApplicationTitle");

        // wt.exe parses `;` as its tab separator even inside quoted
        // args, so any inner `;` (PowerShell statement separator,
        // bash command separator) gets eaten and severs the
        // commandline. The documented workaround is `\;` — wt
        // un-escapes it to a literal `;` and passes the rest through
        // to the spawned shell as one command. Build the inner
        // spawn command first, then escape every `;` in one shot so
        // user-supplied `launch_cmd` strings are covered too.
        if pane.platform == "windows" {
            // Rebuild $env:Path from the Machine + User registry
            // entries. Without this, the spawned PowerShell inherits
            // the Path that wt.exe got from the WSL giga process —
            // which doesn't include Windows-side User PATH entries
            // (e.g. %LOCALAPPDATA%\Programs\giga\). Result: tools
            // installed via [Environment]::SetEnvironmentVariable
            // 'User' (giga.exe, plus most user-scoped installers)
            // wouldn't resolve.
            //
            // `$Host.UI.RawUI.WindowTitle` sets the wt window/tab
            // title to the agent name — visible in the OS chrome,
            // not just the wt tab strip.
            let spawn = format!(
                "$Host.UI.RawUI.WindowTitle = '{title}'; \
                 $env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User'); \
                 Set-Location -LiteralPath '{cwd}'; {cmd}",
                title = pane.title.replace('\'', "''"),
                cwd = pane.cwd.replace('\'', "''"),
                cmd = pane.cmd,
            );
            cmd.arg("powershell.exe")
                .arg("-NoExit")
                .arg("-Command")
                .arg(escape_wt_semicolons(&spawn));
        } else {
            // OSC 0 sets the terminal window title to the agent name.
            let spawn = format!(
                "printf '\\033]0;{name}\\007' ; cd {cwd} && {cmd} ; exec bash",
                name = pane.title,
                cwd = shell_escape::unix::escape(pane.cwd.as_str().into()),
                cmd = pane.cmd,
            );
            // `-lic`: login + interactive + command. The interactive
            // flag forces ~/.bashrc to fully process (Ubuntu's default
            // ~/.bashrc returns early when non-interactive, before any
            // PATH exports). Without -i, claude installs under
            // ~/.local/bin / ~/.npm-global/bin / etc. won't be found.
            cmd.arg("wsl.exe")
                .arg("bash")
                .arg("-lic")
                .arg(escape_wt_semicolons(&spawn));
        }
    }

    let status = cmd.status().context("spawning Windows Terminal")?;
    if !status.success() {
        anyhow::bail!("wt.exe exited with status {status}");
    }
    Ok(())
}

fn launch_tmux(panes: &[Pane], session_name: &str, incremental: bool) -> Result<()> {
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

    for pane in panes.iter() {
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

fn launch_print(panes: &[Pane]) -> Result<()> {
    println!("\nNo terminal multiplexer detected (wt.exe / tmux).");
    println!("Run each of these in its own terminal:\n");
    for p in panes {
        println!("# {} ({})", p.title, p.platform);
        println!("cd {} && {}\n", p.cwd, p.cmd);
    }
    Ok(())
}

/// macOS-native launcher: one Terminal.app window per agent. Driven
/// via `osascript`. For each agent we write a tiny `cd <cwd> && <cmd>`
/// bash script to a temp dir and tell Terminal to `do script` it —
/// the file indirection sidesteps every layer of AppleScript+shell
/// quoting that would otherwise have to escape the intro prompt's
/// apostrophes, semicolons, and so on.
fn launch_mac_terminal(panes: &[Pane], session_name: &str) -> Result<()> {
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

    for pane in panes {
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

/// Replace anything that isn't `[A-Za-z0-9_-]` with `_`. Agent names
/// are kebab-case slugs already, but defensive.
fn sanitize_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(unix)]
fn chmod_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("chmod 755 {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn chmod_executable(_path: &Path) -> Result<()> {
    Ok(())
}
