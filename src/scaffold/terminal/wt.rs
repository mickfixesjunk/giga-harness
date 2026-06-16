//! Windows Terminal (`wt.exe`) backend.

use std::fs;
use std::process::Command;

use anyhow::{Context, Result};
use which::which;

use super::script::{chmod_executable, sanitize_for_filename, stagger_sleep};
use super::{LaunchSession, Pane, TerminalBackend};

pub struct WindowsTerminal;

impl TerminalBackend for WindowsTerminal {
    fn name(&self) -> &'static str {
        "windows-terminal"
    }

    fn launch(&self, panes: &[Pane], session: &LaunchSession) -> Result<()> {
        launch_wt(
            panes,
            &session.session_name,
            session.new_window,
            session.stagger_seconds,
        )
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

fn launch_wt(
    panes: &[Pane],
    session_name: &str,
    new_window: bool,
    stagger_seconds: u64,
) -> Result<()> {
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
    //
    // v0.6.19: when stagger_seconds > 0, issue ONE wt.exe call per
    // pane (always with `--window <session>` so they all land in the
    // same window) with a sleep between calls. This staggers when
    // each `claude` first turn fires, avoiding the TPM-limit storm
    // for large swarms. When stagger_seconds == 0 (default), keep
    // the single big invocation — preserves the snappier UX for
    // small swarms.
    if !regular.is_empty() {
        if stagger_seconds == 0 {
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
            wt_spawn_or_explain(cmd)?;
        } else {
            // Per-pane invocations. First call may use `-w new`, but
            // we need a stable window name for subsequent calls to
            // attach to — so even in new_window mode we name the
            // window after `session_name-stagger-<pid>` and use that
            // for ALL calls. `--window <name>` creates if absent,
            // attaches if present.
            let window_name = if new_window {
                format!("{session_name}-stagger-{}", std::process::id())
            } else {
                session_name.to_string()
            };
            for (i, pane) in regular.iter().enumerate() {
                if i > 0 {
                    stagger_sleep(stagger_seconds);
                }
                let mut cmd = Command::new(exe);
                cmd.arg("--window").arg(&window_name);
                append_tab_args(&mut cmd, pane, &tmpdir)?;
                wt_spawn_or_explain(cmd)?;
            }
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
        let status = cmd
            .status()
            .context("spawning Windows Terminal (admin tabs)")?;
        if !status.success() {
            anyhow::bail!("wt.exe exited with status {status} (admin tabs)");
        }
    }

    Ok(())
}

// v0.6.5: when `wt.exe` exec fails with ENOEXEC (Exec format
// error / os error 8), the WindowsApps AppExecutionAlias stub for
// wt.exe is probably broken or absent — common on freshly-installed
// WSL distros where Windows Terminal isn't installed but the alias
// stub exists with zero bytes. Surface the fallback before the
// cryptic os-error reaches the operator.
//
// v0.6.19: extracted as a helper so the single-invocation path and
// the per-pane staggered path both use the same friendly error
// handling.
fn wt_spawn_or_explain(mut cmd: Command) -> Result<()> {
    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            if e.raw_os_error() == Some(8) {
                return Err(anyhow::anyhow!(
                    "wt.exe spawn failed with ENOEXEC (os error 8) — \
                     the Windows Terminal alias stub at /mnt/c/Users/.../WindowsApps/wt.exe \
                     is likely a 0-byte AppExecutionAlias that isn't a real binary. \
                     Either install Windows Terminal from the Microsoft Store, OR run \
                     `giga launch --terminal tmux` to bypass wt entirely."
                ));
            }
            return Err(anyhow::Error::new(e).context("spawning Windows Terminal"));
        }
    };
    if !status.success() {
        anyhow::bail!("wt.exe exited with status {status}");
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
