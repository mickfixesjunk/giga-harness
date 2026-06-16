//! Install mechanics for `giga upgrade` — the functions that actually
//! run the canonical installer (`install.sh` via bash on Linux/macOS,
//! `install.ps1` via PowerShell on Windows) locally or on a peer host
//! over `giga remote`.

use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

/// URLs for the per-platform installers — hard-coded to this
/// project's GitHub release "latest" endpoint. v0.4.1+ ships with
/// these baked in so `giga upgrade` doesn't need an extra config
/// knob. v0.6.12 split into per-platform: `install.sh` for
/// Linux/macOS, `install.ps1` for native Windows.
const INSTALL_SH_URL: &str =
    "https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh";
const INSTALL_PS1_URL: &str =
    "https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.ps1";

/// Run the canonical installer on this host, dispatched by platform.
///
/// v0.6.12: native Windows builds (`giga.exe`) now invoke
/// `install.ps1` via PowerShell instead of `install.sh` via bash.
/// Pre-fix, `giga upgrade` on Windows ran `bash -c "curl ... | bash"`
/// which either failed outright (no bash on PATH) or — worse — found
/// Git Bash and ran the Linux install.sh, writing giga into a POSIX
/// path that the Windows giga.exe launcher never looks at. Reported
/// on 2026-06-03 after a Windows-host upgrade to v0.6.11.
///
/// Linux/macOS keep the bash + curl + install.sh path unchanged.
///
/// Streams stdout/stderr through to the operator so install progress
/// is visible.
pub(super) fn install_local(dry_run: bool) -> Result<()> {
    if cfg!(target_os = "windows") {
        install_local_windows(dry_run)
    } else {
        install_local_unix(dry_run)
    }
}

fn install_local_unix(dry_run: bool) -> Result<()> {
    if dry_run {
        println!("  [dry-run] would: curl -sSfL {INSTALL_SH_URL} | bash");
        return Ok(());
    }
    let pipeline = format!("curl -sSfL {INSTALL_SH_URL} | bash");
    let status = Command::new("bash")
        .args(["-c", &pipeline])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| "running local install.sh")?;
    if !status.success() {
        return Err(anyhow!(
            "local install failed (exit {})",
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

/// Run install.ps1 on the Windows side from a WSL operator host.
/// Used when the operator is on WSL (cfg!(target_os = "linux")) AND
/// there are Windows-platform agents co-located on the same physical
/// box (a single-host topology where Windows agents share the WSL
/// host's physical machine via WSL interop).
///
/// WSL interop exposes `powershell.exe` on PATH; we invoke it the
/// same way `install_local_windows` does. The PowerShell process
/// runs on the Windows side, downloads install.ps1, and installs
/// giga.exe into the Windows-side install location.
pub(super) fn install_local_windows_via_wsl_interop(dry_run: bool) -> Result<()> {
    let script = format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
         iwr -useb {INSTALL_PS1_URL} | iex"
    );
    if dry_run {
        println!(
            "  [dry-run] would (via WSL interop): powershell.exe -NoProfile -ExecutionPolicy Bypass -Command \"{script}\""
        );
        return Ok(());
    }
    println!("  -> running install.ps1 on Windows side via WSL interop (powershell.exe)");
    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| {
            "invoking powershell.exe via WSL interop — is interop enabled? \
             (check /etc/wsl.conf [interop] generateBinPath=true + \
             `wsl --shutdown` from Windows)"
        })?;
    if !status.success() {
        return Err(anyhow!(
            "Windows-side install.ps1 failed (exit {})",
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

fn install_local_windows(dry_run: bool) -> Result<()> {
    // The canonical Windows one-liner is `iwr -useb <url> | iex`. We
    // run it under powershell.exe with ExecutionPolicy Bypass + a
    // pinned TLS protocol so older PowerShell 5.x boxes can still
    // negotiate HTTPS to github.com. PowerShell 7+ doesn't need the
    // SecurityProtocol nudge but it's a harmless no-op there.
    let script = format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
         iwr -useb {INSTALL_PS1_URL} | iex"
    );
    if dry_run {
        println!("  [dry-run] would: powershell -NoProfile -ExecutionPolicy Bypass -Command \"{script}\"");
        return Ok(());
    }
    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| "running local install.ps1 via powershell.exe")?;
    if !status.success() {
        return Err(anyhow!(
            "local install failed (exit {})",
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

/// Run the canonical installer on a peer host over `giga remote
/// --host`. We re-invoke this same binary so the remote-exec
/// capability check (transport must `supports_remote_exec`) is
/// enforced uniformly with the rest of the `--host` operator
/// commands.
///
/// v0.6.12: dispatches by `peer_platform` so Windows peers get
/// `install.ps1` via `powershell.exe` and Linux/macOS peers get
/// `install.sh` via `bash`. Platform is inferred from the agents
/// configured on the peer host (see `infer_host_platform`).
pub(super) fn install_remote(
    giga_exe: &std::path::Path,
    config: &std::path::Path,
    peer: &str,
    peer_platform: &str,
    dry_run: bool,
) -> Result<()> {
    let (shell_program, shell_args, installer_cmd): (&str, &[&str], String) =
        if peer_platform == "windows" {
            (
                "powershell.exe",
                &["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"],
                format!(
                "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
                 iwr -useb {INSTALL_PS1_URL} | iex"
            ),
            )
        } else {
            (
                "bash",
                &["-c"],
                format!("curl -sSfL {INSTALL_SH_URL} | bash"),
            )
        };
    if dry_run {
        println!(
            "  [dry-run] would: giga remote --host {peer} -- {shell_program} {} '{installer_cmd}'",
            shell_args.join(" "),
        );
        return Ok(());
    }
    let mut args: Vec<String> = vec![
        "remote".into(),
        "--host".into(),
        peer.into(),
        "--config".into(),
        config
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF8 config path"))?
            .into(),
        "--".into(),
        shell_program.into(),
    ];
    for a in shell_args {
        args.push((*a).into());
    }
    args.push(installer_cmd);
    let status = Command::new(giga_exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking giga remote --host {peer} for install"))?;
    if !status.success() {
        return Err(anyhow!(
            "remote install on {peer} failed (exit {})",
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_urls_point_at_this_project_repo() {
        // Guard against accidental URL drift if someone edits the
        // constants. install.sh / install.ps1 are what the README +
        // REMOTE_QUICKSTART point at, so changing the URL silently
        // is bad.
        for url in [INSTALL_SH_URL, INSTALL_PS1_URL] {
            assert!(url.contains("mickfixesjunk/giga-harness"), "{url}");
            assert!(url.contains("/latest/"), "{url}");
        }
        assert!(INSTALL_SH_URL.ends_with("/install.sh"));
        assert!(INSTALL_PS1_URL.ends_with("/install.ps1"));
    }
}
