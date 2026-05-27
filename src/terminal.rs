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
    /// Request UAC elevation for this tab (Windows Terminal only).
    pub admin: bool,
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
    // Compose wt.exe invocations — one tab per agent.
    //
    // Admin panes get a SEPARATE wt.exe call with `-w new`. This is
    // necessary because wt.exe silently drops `--admin` when it
    // attaches to an existing non-elevated WT window. Forcing `-w new`
    // makes WT open a fresh window for those tabs, which triggers the
    // UAC prompt correctly. Non-admin panes go into the named session
    // window as before.
    let exe = if which("wt.exe").is_ok() {
        "wt.exe"
    } else {
        "wt"
    };

    // Temp dir for WSL launch scripts. The agent identity prompt
    // contains backtick-wrapped slugs (e.g. `design`) that are safe
    // inside bash single-quoted strings but get treated as command
    // substitution if any layer in the wt.exe→wsl.exe chain
    // re-processes the argument under double-quote semantics — which
    // it does. The slug command fails silently, the name disappears,
    // and the agent hears "You are the  agent." Passing a plain
    // script path instead has no metacharacters; the quoting gauntlet
    // can't corrupt it. Same rationale as launch_mac_terminal.
    let tmpdir = std::env::temp_dir().join(format!(
        "giga-launch-{}-{}",
        session_name,
        std::process::id()
    ));

    let regular: Vec<&Pane> = panes.iter().filter(|p| !p.admin).collect();
    let admin: Vec<&Pane> = panes.iter().filter(|p| p.admin).collect();

    // Regular (non-admin) panes: attach to or create the named window.
    if !regular.is_empty() {
        let mut cmd = Command::new(exe);
        if new_window {
            cmd.arg("-w").arg("new");
        } else {
            cmd.arg("--window").arg(session_name);
        }
        for (i, pane) in regular.iter().enumerate() {
            if i > 0 {
                cmd.arg(";");
            }
            append_tab_args(&mut cmd, pane, &tmpdir)?;
        }
        let status = cmd.status().context("spawning Windows Terminal (regular tabs)")?;
        if !status.success() {
            anyhow::bail!("wt.exe exited with status {status}");
        }
    }

    // Admin panes: force a new window so --admin triggers UAC.
    if !admin.is_empty() {
        let mut cmd = Command::new(exe);
        cmd.arg("-w").arg("new");
        for (i, pane) in admin.iter().enumerate() {
            if i > 0 {
                cmd.arg(";");
            }
            cmd.arg("new-tab")
                .arg("--title")
                .arg(&pane.title)
                .arg("--suppressApplicationTitle")
                .arg("--admin");
            append_windows_tab_cmd(&mut cmd, pane);
        }
        let status = cmd.status().context("spawning Windows Terminal (admin tabs)")?;
        if !status.success() {
            anyhow::bail!("wt.exe exited with status {status} (admin tabs)");
        }
    }

    Ok(())
}

// Appends new-tab args for a single non-admin pane to an in-progress
// wt.exe command. Handles wsl vs windows platform branching.
fn append_tab_args(cmd: &mut Command, pane: &Pane, tmpdir: &std::path::Path) -> Result<()> {
    cmd.arg("new-tab")
        .arg("--title")
        .arg(&pane.title)
        .arg("--suppressApplicationTitle");
    debug_assert!(!pane.admin);

    if pane.platform == "windows" {
        append_windows_tab_cmd(cmd, pane);
    } else {
        // Write the spawn body to a temp script and pass wsl.exe just
        // the path — no shell metacharacters in the wt.exe command
        // line, no quoting corruption.
        //
        // `-li`: login + interactive so PATH includes ~/.local/bin and
        // the user's ~/.bashrc additions are applied.
        fs::create_dir_all(tmpdir)
            .with_context(|| format!("creating launch script dir {}", tmpdir.display()))?;
        let script_path = tmpdir.join(format!("{}.sh", sanitize_for_filename(&pane.title)));
        let body = format!(
            "#!/bin/bash\nprintf '\\033]0;{name}\\007'\ncd {cwd} && {cmd}\nexec bash\n",
            name = pane.title,
            cwd = shell_escape::unix::escape(pane.cwd.as_str().into()),
            cmd = pane.cmd,
        );
        fs::write(&script_path, &body)
            .with_context(|| format!("writing launch script {}", script_path.display()))?;
        chmod_executable(&script_path)?;
        cmd.arg("wsl.exe")
            .arg("bash")
            .arg("-li")
            .arg(script_path.to_string_lossy().as_ref());
    }
    Ok(())
}

// Appends the powershell.exe invocation for a Windows-platform pane.
// Shared between the regular and admin wt.exe call paths.
fn append_windows_tab_cmd(cmd: &mut Command, pane: &Pane) {
    // Rebuild $env:Path from Machine + User registry so tools installed
    // via the User scope (giga.exe, most user-scoped installers) resolve.
    // wt.exe parses `;` as a tab separator even inside quoted args —
    // escape_wt_semicolons converts inner `;` to `\;` so wt passes them
    // through to PowerShell intact.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_override_accepts_canonical_names() {
        assert_eq!(parse_override("tmux"), Some(Multiplexer::Tmux));
        assert_eq!(parse_override("wt"), Some(Multiplexer::WindowsTerminal));
        assert_eq!(
            parse_override("mac-terminal"),
            Some(Multiplexer::MacTerminal)
        );
        assert_eq!(parse_override("print"), Some(Multiplexer::None));
    }

    #[test]
    fn parse_override_accepts_aliases() {
        // `windows-terminal` is the long-form alias for `wt`.
        assert_eq!(
            parse_override("windows-terminal"),
            Some(Multiplexer::WindowsTerminal)
        );
        // `mac` is the short alias for `mac-terminal`.
        assert_eq!(parse_override("mac"), Some(Multiplexer::MacTerminal));
        // `none` is the alias for `print`.
        assert_eq!(parse_override("none"), Some(Multiplexer::None));
    }

    #[test]
    fn parse_override_auto_returns_detect_result() {
        // `auto` defers to `detect()`. We can't assert which variant
        // comes back (depends on what's installed on the test host),
        // but it should always return Some.
        assert!(parse_override("auto").is_some());
    }

    #[test]
    fn parse_override_rejects_unknown_value() {
        assert_eq!(parse_override("kitty"), None);
        assert_eq!(parse_override(""), None);
        assert_eq!(parse_override("TMUX"), None, "case-sensitive");
    }

    #[test]
    fn sanitize_filename_passes_through_slugs() {
        assert_eq!(sanitize_for_filename("design"), "design");
        assert_eq!(sanitize_for_filename("code-review"), "code-review");
        assert_eq!(sanitize_for_filename("agent_42"), "agent_42");
    }

    #[test]
    fn sanitize_filename_replaces_unsafe_chars() {
        // Path separators, spaces, shell metachars — all become `_`.
        assert_eq!(sanitize_for_filename("a/b"), "a_b");
        assert_eq!(sanitize_for_filename("a b"), "a_b");
        assert_eq!(sanitize_for_filename("a;b"), "a_b");
        assert_eq!(sanitize_for_filename("a$b"), "a_b");
        assert_eq!(sanitize_for_filename("../etc"), "___etc");
    }
}
