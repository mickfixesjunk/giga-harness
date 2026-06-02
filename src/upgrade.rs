//! `giga upgrade` — install latest binary locally, optionally on peers,
//! and post a broadcast asking agents to re-arm their inbox watcher.
//!
//! Why this exists. The natural upgrade flow is:
//!   1. `curl install.sh | bash` on every host
//!   2. Tell every agent "please TaskStop + re-arm your watcher" so
//!      they pick up the new binary (running watchers are the old
//!      pre-upgrade binary in-process)
//!
//! For a single-host swarm that's two commands. For a multi-host swarm
//! it's 1 + N + 1. `giga upgrade` collapses both into one operator
//! command, with optional opt-outs for the broadcast and/or peer
//! propagation.
//!
//! Safety:
//! * The local install runs the SAME `install.sh` an operator would
//!   run manually — `curl -sSfL <release URL> | bash`. The URL is
//!   hard-coded to this project's own GitHub release endpoint; no
//!   indirection.
//! * Overwriting the running binary is safe on Linux/macOS (open file
//!   handles keep the old binary mapped; subsequent invocations see
//!   the new inode). On Windows the in-place overwrite may fail; the
//!   install.ps1 path is the operator's recourse there.
//! * Bootstrap post-failure (peer install failed; broadcast failed) is
//!   non-fatal — local install already succeeded, peers/agents can be
//!   re-prodded manually.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::config::{self, Config};

/// URL operator install.sh — hard-coded to this project's GitHub
/// release "latest" endpoint. v0.4.1+ ships with this baked in so
/// `giga upgrade` doesn't need an extra config knob.
const INSTALL_URL: &str =
    "https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh";

pub struct Args {
    pub config: PathBuf,
    /// Agent slug to post the rearm broadcast AS. Must be a
    /// participant of the broadcast channel(s) the post will hit.
    /// Required for the broadcast step; if omitted, upgrade prints
    /// the broadcast command for the operator to run manually.
    pub as_agent: Option<String>,
    /// Skip propagating the install to peer hosts. Use when peers are
    /// offline or you want to update them on a different cadence.
    pub skip_peers: bool,
    /// Skip the rearm broadcast. Use when you've already prodded
    /// agents another way, or want a silent operator-only update.
    pub skip_broadcast: bool,
    /// Print what would happen; don't run install or post.
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;

    // --- 1. local install ---------------------------------------------
    println!("==> upgrading giga on local host");
    install_local(args.dry_run)?;

    // --- 2. peer installs ---------------------------------------------
    let peers: Vec<&str> = if args.skip_peers {
        Vec::new()
    } else {
        cfg.hosts
            .iter()
            .filter(|h| Some(h.name.as_str()) != cfg.this_host.as_deref())
            .map(|h| h.name.as_str())
            .collect()
    };
    if !peers.is_empty() {
        println!("\n==> upgrading giga on {} peer host(s)", peers.len());
        for peer in &peers {
            match install_remote(&args.config, peer, args.dry_run) {
                Ok(()) => println!("  + {peer}: upgraded"),
                Err(e) => eprintln!("  ! {peer}: upgrade failed ({e:#}) — run install on that host manually"),
            }
        }
    } else if args.skip_peers {
        println!("\n(--skip-peers: not propagating install)");
    } else if !cfg.hosts.is_empty() {
        println!("\n(no peer hosts found in [[hosts]] — local-only install)");
    }

    // --- 3. rearm broadcast -------------------------------------------
    if args.skip_broadcast {
        println!("\n(--skip-broadcast: not posting rearm message)");
        return Ok(());
    }
    let broadcast_channels: Vec<&str> = cfg
        .channels
        .iter()
        .filter(|c| config::is_broadcast_channel(&c.file))
        .map(|c| c.file.as_str())
        .collect();
    if broadcast_channels.is_empty() {
        println!("\n(no broadcast channels found — skipping rearm post)");
        return Ok(());
    }

    let posting_agent = match args.as_agent.as_deref() {
        Some(slug) => slug.to_string(),
        None => {
            print_manual_broadcast_command(&broadcast_channels);
            return Ok(());
        }
    };

    println!(
        "\n==> posting rearm broadcast as `{posting_agent}` to {} channel(s)",
        broadcast_channels.len(),
    );
    for ch in &broadcast_channels {
        if args.dry_run {
            println!("  [dry-run] would post to {ch}");
            continue;
        }
        match post_rearm(&args.config, &posting_agent, ch) {
            Ok(()) => println!("  + posted to {ch}"),
            Err(e) => eprintln!("  ! {ch}: post failed ({e:#})"),
        }
    }
    println!("\nupgrade complete.");
    Ok(())
}

/// Run the canonical install.sh on this host. Streams stdout/stderr
/// through to the operator so the install progress is visible.
fn install_local(dry_run: bool) -> Result<()> {
    if dry_run {
        println!("  [dry-run] would: curl -sSfL {INSTALL_URL} | bash");
        return Ok(());
    }
    // `bash -c 'curl ... | bash'` so the pipe lives inside the child.
    let pipeline = format!("curl -sSfL {INSTALL_URL} | bash");
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

/// Run the canonical install.sh on a peer over `giga remote --host`.
/// We re-invoke this same binary so the remote-exec capability check
/// (transport must `supports_remote_exec`) is enforced uniformly with
/// the rest of the `--host` operator commands.
fn install_remote(config: &std::path::Path, peer: &str, dry_run: bool) -> Result<()> {
    let cmd = format!("curl -sSfL {INSTALL_URL} | bash");
    if dry_run {
        println!("  [dry-run] would: giga remote --host {peer} -- bash -c '{cmd}'");
        return Ok(());
    }
    let status = Command::new(std::env::current_exe()?)
        .args([
            "remote",
            "--host",
            peer,
            "--config",
            config
                .to_str()
                .ok_or_else(|| anyhow!("non-UTF8 config path"))?,
            "--",
            "bash",
            "-c",
            &cmd,
        ])
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

/// Post the canonical "please re-arm your watcher" message to one
/// broadcast channel. Re-invokes this same binary so the post goes
/// through the standard `giga post` validation + dual-write path.
fn post_rearm(config: &std::path::Path, as_agent: &str, channel: &str) -> Result<()> {
    let subject = "giga upgraded — please re-arm your inbox watcher";
    let body = "giga has been upgraded to the latest release on all hosts. \
                Please TaskStop your `giga inbox watcher` Monitor — the \
                current one is still running the pre-upgrade binary in-process \
                — then re-arm by re-issuing the Monitor TOOL call from your \
                CLAUDE.md. ~5 seconds; no pending notifications are lost \
                (your cursor persists across watcher restarts).";
    let status = Command::new(std::env::current_exe()?)
        .args([
            "post",
            channel,
            "--as",
            as_agent,
            "--subject",
            subject,
            "--body",
            body,
            "--config",
            config
                .to_str()
                .ok_or_else(|| anyhow!("non-UTF8 config path"))?,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking giga post on {channel}"))?;
    if !status.success() {
        return Err(anyhow!(
            "giga post on {channel} failed (exit {})",
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

/// Print a copy-paste broadcast command for the operator to run when
/// --as wasn't supplied. Helps them pick a participant slug without
/// re-running upgrade.
fn print_manual_broadcast_command(channels: &[&str]) {
    println!();
    println!("(--as not provided; rearm broadcast skipped)");
    println!("To prompt agents to re-arm, run one of:");
    for ch in channels {
        println!(
            "  giga post {ch} --as <participant-slug> \\\n    --subject \"giga upgraded — please re-arm your inbox watcher\" \\\n    --body \"giga has been upgraded; TaskStop your watcher and re-arm from CLAUDE.md.\""
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Implementation tests are intentionally light — `install_local`
    // and `install_remote` shell out to curl/bash/ssh which can't be
    // unit-tested without a full network + ssh fixture. The dry-run
    // paths give operators a safe preview; smoke-testing via
    // `giga upgrade --dry-run` is the right CI for this subcommand.
    //
    // What we CAN unit-test: the broadcast-channel enumeration logic
    // is just `cfg.channels.iter().filter(is_broadcast_channel)`,
    // already covered by config::tests::is_broadcast_channel_matches_underscore_prefix.

    #[test]
    fn install_url_points_at_this_project_repo() {
        // Guard against accidental URL drift if someone edits the
        // constant. install.sh is what the README + REMOTE_QUICKSTART
        // point at, so changing the URL silently is bad.
        assert!(INSTALL_URL.contains("mickfixesjunk/giga-harness"));
        assert!(INSTALL_URL.ends_with("/install.sh"));
        assert!(INSTALL_URL.contains("/latest/"));
    }
}
