//! Subprocess substrate: run-and-check, shell escaping.
//!
//! The harness shells out a lot — rsync, ssh, git, tmux, `cmd.exe`, and
//! `giga` itself. Roughly a dozen call sites each hand-rolled the same
//! `Command` → set stdio → `.status()`/`.output()` → map exit code into an
//! `anyhow` error dance. These helpers are that dance, once.

use std::borrow::Cow;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

/// Run `cmd` to completion, erroring (with `what` as context) if it
/// exits non-zero. stdin is nulled and stdout is suppressed; stderr is
/// inherited so the operator sees the child's diagnostics. This is the
/// "fire a side-effecting command and check it worked" path.
pub fn run_checked(cmd: &mut Command, what: &str) -> Result<()> {
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking {what}"))?;
    if !status.success() {
        return Err(anyhow!("{what} exited {}", status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// POSIX shell-escape `s` for safe interpolation into a remote command
/// string (the `bash -lc '<cmd>'` wrapping used over ssh). Thin wrapper
/// over `shell_escape` so the `Cow::Borrowed` incantation lives in one
/// place.
pub fn sh_escape(s: &str) -> Cow<'_, str> {
    shell_escape::unix::escape(Cow::Borrowed(s))
}

/// Read a Windows environment variable's value, working from either
/// native Windows or a WSL distro with cmd.exe interop. On Windows this
/// reads the process environment directly; on Unix it shells to
/// `cmd.exe /c echo %VAR%` and trims the CRLF. Returns `None` when the
/// variable is unset (cmd.exe echoes the literal `%VAR%` in that case,
/// which is guarded against).
pub fn cmd_exe_echo(var: &str) -> Option<String> {
    if cfg!(target_os = "windows") {
        return std::env::var(var).ok();
    }
    let out = Command::new("cmd.exe")
        .args(["/c", &format!("echo %{var}%")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    if s.is_empty() || s == format!("%{var}%") {
        return None;
    }
    Some(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn run_checked_ok_on_success() {
        assert!(run_checked(&mut Command::new("true"), "true").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn run_checked_errors_on_failure() {
        let err = run_checked(&mut Command::new("false"), "false").unwrap_err();
        assert!(err.to_string().contains("false exited"));
    }

    #[test]
    fn sh_escape_quotes_spaces_and_specials() {
        assert_eq!(sh_escape("plain").as_ref(), "plain");
        assert_eq!(sh_escape("a b").as_ref(), "'a b'");
        assert_eq!(sh_escape("a'b").as_ref(), r#"'a'\''b'"#);
    }
}
