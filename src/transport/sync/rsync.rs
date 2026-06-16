//! The rsync executor: turn the planner's `SyncCommand`s into actual
//! `rsync` invocations over Tailscale SSH, plus the per-tick sweep
//! (`tick_once`) that the `RsyncTailscaleTransport::tick` adapter calls.

use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::config::{Config, Host};

// SSH timeout options + the rsync `-e ssh …` arg builder live in
// foundation::ssh (shared with remote + teleport so a dead tailnet
// fails in ~10s instead of hanging ~2min).
use crate::foundation::ssh::rsync_ssh_e_arg;

use super::plan::{cfg_canonical_path, compute_sync_plan, SyncCommand};

/// Single sync sweep — extracted from the daemon loop so the
/// `RsyncTailscaleTransport::tick` adapter can call it without
/// reimplementing the planner + executor. Idempotent.
pub fn tick_once(cfg: &Config, this_host: &str, dry_run: bool, quiet: bool) -> Result<()> {
    let canonical = cfg_canonical_path(cfg)?;
    let plan = compute_sync_plan(cfg, this_host, &canonical);
    if plan.is_empty() {
        // v0.3.6: silent under --quiet — this prints every tick under
        // normal mode but Monitor-hosted daemons don't need it.
        if !quiet {
            eprintln!(
                "sync: no cross-host slices for this_host=`{this_host}` and no peers to ship to."
            );
        }
        return Ok(());
    }
    let mut ok = 0usize;
    let mut failed = 0usize;
    for cmd in &plan {
        if dry_run {
            eprintln!(
                "[dry-run] {} {} -> {}",
                cmd.kind,
                cmd.local_path.display(),
                cmd.peer_target
            );
            continue;
        }
        match execute(cmd) {
            Ok(()) => ok += 1,
            Err(e) => {
                // Errors always emit even under --quiet — that's the
                // critical signal a swarm_boss agent needs to notice.
                eprintln!(
                    "sync: {} push failed ({e}) — will retry next tick",
                    cmd.kind
                );
                failed += 1;
            }
        }
    }
    // v0.3.4 fix for quality finding 10: print a summary line after every
    // sync tick that actually had work to do. Pre-fix: `giga sync --once`
    // produced no output on success (rsync's -v output went to inherited
    // stderr but was often easy to miss in a CI/scripted invocation), so
    // the operator couldn't tell "pushed cleanly" from "silently no-op'd
    // because of an mtime gap". Suppressed in dry-run (the dry-run lines
    // already enumerate the would-be work).
    //
    // v0.3.6 (SWARM_BOSS_DESIGN.md): also suppressed under --quiet so
    // Monitor-hosted daemons don't flood the swarm_boss agent's
    // notification stream with "tick complete" every 3 seconds. Errors
    // still emit (printed above).
    if !dry_run && !quiet {
        let attempted = plan.len();
        eprintln!(
            "sync: tick complete — {attempted} attempted ({ok} ok, {failed} failed) for this_host=`{this_host}`"
        );
    }
    Ok(())
}

/// Build the rsync target string: `user@tailnet_hostname:path`.
/// `path` must already be a forward-slash string — the peer is always
/// Linux/WSL. Callers compute it via `foundation::paths::unix_join`
/// which normalizes backslashes that a Windows operator's
/// `PathBuf::display()` would emit.
pub(crate) fn build_rsync_target(peer: &Host, remote_path: &str) -> Result<String> {
    let user = peer
        .ssh_user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok()) // Windows operator fallback
        .ok_or_else(|| {
            anyhow!(
                "can't determine SSH user for host `{}` (no ssh_user; $USER and $USERNAME both unset)",
                peer.name
            )
        })?;
    Ok(format!(
        "{user}@{host}:{remote_path}",
        host = peer.tailnet_hostname,
    ))
}

fn execute(cmd: &SyncCommand) -> Result<()> {
    // v0.4.3 Bug 12: skip cleanly when the local file doesn't exist
    // yet. For slice files this is the common "no posts on this
    // channel from this host yet" state — pre-fix, rsync fired
    // anyway, failed with exit 23 ("No such file or directory"),
    // and the loop logged a `push failed` warning every tick. The
    // A field report (2026-06-02) tied a Monitor-reported daemon
    // exit (exit 144) to this cascade. Silent
    // no-op until the source file materializes is the right shape:
    // sync's contract is "push what exists", not "complain about
    // what doesn't".
    if !cmd.local_path.exists() {
        return Ok(());
    }
    // `rsync -avz [--append-verify] <local> <target>`. `-a` preserves
    // metadata, `-v` is verbose (printed to our stderr), `-z` compresses
    // the on-wire bytes. We don't need --partial because a failed transfer
    // doesn't corrupt the destination — rsync writes to a temp + renames.
    let mut c = Command::new("rsync");
    c.arg("-avz");
    // v0.6.15: bound rsync's SSH so a dead tailnet returns Err in
    // ~10s instead of wedging the tick for minutes.
    let ssh_e = rsync_ssh_e_arg();
    c.arg("-e");
    c.arg(&ssh_e);
    if cmd.use_append_verify {
        c.arg("--append-verify");
    }
    c.arg(&cmd.local_path);
    c.arg(&cmd.peer_target);
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let status = c
        .status()
        .with_context(|| format!("running rsync for {}", cmd.peer_target))?;
    if !status.success() {
        return Err(anyhow!(
            "rsync exit {} for {} -> {}",
            status.code().unwrap_or(-1),
            cmd.local_path.display(),
            cmd.peer_target
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn host(name: &str, tailnet: &str, ssh_user: Option<&str>) -> Host {
        Host {
            name: name.into(),
            tailnet_hostname: tailnet.into(),
            ssh_user: ssh_user.map(|s| s.into()),
            remote_config_dir: None,
            remote_inbox_dir: None,
            paths: None,
        }
    }

    /// SSH timeout options must include a ConnectTimeout — without it
    /// a dead tailnet wedges the rsync invocation for the OS-default
    /// TCP timeout (~2min/attempt). That was the original symptom.
    #[test]
    fn rsync_ssh_e_arg_includes_connect_timeout() {
        let arg = rsync_ssh_e_arg();
        assert!(
            arg.contains("ConnectTimeout"),
            "missing ConnectTimeout: {arg}"
        );
        assert!(
            arg.contains("ServerAliveInterval"),
            "missing ServerAliveInterval: {arg}"
        );
        // First token must be `ssh` because rsync's -e parses this as
        // a command line; the rest are flags for that command.
        assert!(arg.starts_with("ssh "), "must start with `ssh`: {arg}");
    }

    // SSH_TIMEOUT_OPTS shape is tested in foundation::ssh.

    #[test]
    fn build_rsync_target_uses_explicit_ssh_user() {
        let h = host("wsl-b", "wsl-b.tail0.ts.net", Some("alice"));
        let target = build_rsync_target(&h, "/some/file.md").unwrap();
        assert_eq!(target, "alice@wsl-b.tail0.ts.net:/some/file.md");
    }

    #[test]
    fn build_rsync_target_falls_back_to_env_user() {
        let orig = std::env::var("USER").ok();
        unsafe { std::env::set_var("USER", "env-user") };
        let h = host("wsl-b", "wsl-b.tail0.ts.net", None);
        let target = build_rsync_target(&h, "/x").unwrap();
        match orig {
            Some(v) => unsafe { std::env::set_var("USER", v) },
            None => unsafe { std::env::remove_var("USER") },
        }
        assert_eq!(target, "env-user@wsl-b.tail0.ts.net:/x");
    }

    // forward-slash normalization of remote paths is now exercised by
    // foundation::paths::unix_join tests (the planner calls unix_join
    // directly; sync no longer carries a private remote_join copy).

    /// v0.3.6 S7 (SWARM_BOSS_DESIGN.md §5): --quiet suppresses the
    /// per-tick "tick complete" summary. `quiet` is now a plain
    /// parameter on `tick_once` (was a process-global atomic), so this
    /// just confirms the quiet path runs cleanly. We can't easily
    /// capture process-level stderr in a unit test; the behavioral
    /// guarantee — that `quiet` only gates the summary println, never a
    /// `Result` / error line — is now structural: the error eprintln in
    /// the loop body is unconditional, while `quiet` only short-circuits
    /// the two summary printlns.
    #[test]
    fn quiet_mode_suppresses_per_tick_summary() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "remote-test"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"
ssh_user = "alice"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"
ssh_user = "alice"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
        );
        std::fs::write(&config_path, toml).unwrap();
        std::fs::write(tmp.path().join("this_host.toml"), "this_host = \"wsl-a\"\n").unwrap();
        let cfg = Config::load(&config_path).unwrap();
        // quiet=true, dry_run=true should produce no `tick complete`
        // line and run cleanly.
        tick_once(&cfg, "wsl-a", true, true).unwrap();
    }

    /// v0.4.3 Bug 12: execute() returns Ok WITHOUT shelling to rsync
    /// when the local source file doesn't exist. This is the common
    /// pre-first-post state on a freshly-added channel — the slice
    /// file doesn't materialize until the local agent posts. Pre-fix
    /// rsync fired and failed (exit 23 / no such file), logged as a
    /// recurring "push failed" warning every 3 seconds.
    #[test]
    fn execute_silently_skips_when_local_path_missing() {
        let cmd = SyncCommand {
            peer_target: "user@host:/remote/path".to_string(),
            local_path: PathBuf::from("/this/path/does/not/exist/__giga_test"),
            use_append_verify: false,
            kind: "slice",
        };
        // Must return Ok without panicking — and without invoking
        // rsync (we'd see ERROR_NOT_FOUND or exit 23 if it did).
        execute(&cmd).expect("nonexistent local_path must be a silent no-op");
    }
}
