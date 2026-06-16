//! SSH invocation over the tailnet.
//!
//! Every ssh/rsync call carries [`SSH_TIMEOUT_OPTS`] so a dead tailnet
//! peer fails in ~10s instead of wedging for the OS-default TCP timeout
//! (~2min/attempt). The sync daemon already did this; `remote` and
//! `teleport` historically did not and could hang — routing them through
//! [`ssh_exec`] closes that gap.

use std::process::Command;

use anyhow::Result;

use super::proc;

/// SSH options applied to every rsync/ssh invocation so a dead tailnet
/// returns an error quickly rather than hanging:
///
/// - `ConnectTimeout=10` — fail fast on the initial TCP handshake.
/// - `ServerAliveInterval=10` + `ServerAliveCountMax=3` — drop a stalled
///   connection after ~30s of silence rather than hanging forever.
pub const SSH_TIMEOUT_OPTS: &[&str] = &[
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=10",
    "-o",
    "ServerAliveCountMax=3",
];

/// The `-e ssh ...` string rsync uses to invoke ssh, with the timeout
/// options embedded. rsync's `-e` takes a single space-separated string.
pub fn rsync_ssh_e_arg() -> String {
    let mut s = String::from("ssh");
    for opt in SSH_TIMEOUT_OPTS {
        s.push(' ');
        s.push_str(opt);
    }
    s
}

/// Run a one-shot command on `target` (`user@host`), wrapped in
/// `bash -lc` so the remote shell sources login config — necessary for
/// cargo-installed binaries (`~/.cargo/bin/giga`) that aren't on a plain
/// non-interactive ssh's PATH. Carries [`SSH_TIMEOUT_OPTS`]. stdout is
/// suppressed, stderr inherited; errors on a non-zero remote exit.
pub fn ssh_exec(target: &str, remote_cmd: &str) -> Result<()> {
    let wrapped = format!("bash -lc {}", proc::sh_escape(remote_cmd));
    let mut cmd = Command::new("ssh");
    cmd.args(SSH_TIMEOUT_OPTS).arg(target).arg(&wrapped);
    proc::run_checked(&mut cmd, &format!("ssh {target} <cmd>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_opts_include_connect_timeout() {
        assert!(SSH_TIMEOUT_OPTS
            .iter()
            .any(|a| a.contains("ConnectTimeout")));
    }

    #[test]
    fn rsync_e_arg_embeds_opts() {
        let e = rsync_ssh_e_arg();
        assert!(e.starts_with("ssh "));
        assert!(e.contains("-o ConnectTimeout=10"));
        assert!(e.contains("ServerAliveInterval=10"));
        assert!(e.contains("ServerAliveCountMax=3"));
    }
}
