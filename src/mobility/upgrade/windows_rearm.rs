//! Windows agent disarm/rearm dance for `giga upgrade`.
//!
//! Windows file-locks running `.exe`s, so a Windows agent holding
//! `giga.exe` (via a running inbox watcher / daemon) blocks
//! `install.ps1` from overwriting it. Before installing we post a
//! targeted `[ack: <slugs>]` broadcast asking those agents to
//! TaskStop their watcher (release the lock) and schedule a delayed
//! re-arm; after installing we post the matching re-arm message. Used
//! for both local (WSL-interop co-located) and cross-host Windows
//! agents.

use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

/// Post a targeted `[ack: <windows-slugs>]` broadcast asking the
/// Windows agents on `peer` to TaskStop their watchers + schedule a
/// re-arm. Then sleep `operator_wait_secs` operator-side to give them
/// a chance to act before running install.ps1. The `[ack:]` prefix
/// already filters fanout in the watcher (see watch.rs:347) so
/// non-Windows agents on the same channel are unaffected.
///
/// v0.6.23: operator-side wait and agent-side rearm delay are now
/// SEPARATE values. Pre-v0.6.23 they were conflated as a single
/// 60s constant, which had two problems: (a) the operator wait was
/// longer than needed (~15s field-validated), bloating the
/// end-to-end upgrade time, and (b) the agent's rearm delay must
/// EXCEED operator wait + install duration to avoid the new watcher
/// loading a half-written giga.exe.
#[allow(clippy::too_many_arguments)]
pub(super) fn windows_pre_install_disarm(
    giga_exe: &std::path::Path,
    config: &std::path::Path,
    posting_agent: &str,
    peer: &str,
    windows_agents: &[String],
    broadcast_channels: &[&str],
    operator_wait_secs: u64,
    agent_rearm_delay_secs: u64,
    dry_run: bool,
) -> Result<()> {
    let ack_list = windows_agents.join(",");
    let subject = format!(
        "[ack: {ack_list}] giga upgrade incoming on `{peer}` — disarm your watcher and rearm it in {agent_rearm_delay_secs}s"
    );
    let body = format!(
        "giga.exe on host `{peer}` is about to be upgraded via install.ps1, \
         which requires the binary to be unlocked (Windows file-locks running \
         exes). Please do BOTH of these now, in order:\n\
         \n\
         1. TaskStop your giga inbox watcher (release the giga.exe file lock).\n\
         \n\
         2. BEFORE going idle, schedule yourself to re-arm the watcher in \
         {agent_rearm_delay_secs} seconds using your runtime's scheduler \
         (Claude Code: CronCreate; agy: schedule). Without this scheduled \
         re-arm you will stay deaf to inbox events until manually nudged — \
         the rearm broadcast lands in the channel during the deaf window \
         and your dead watcher can't surface it.\n\
         \n\
         The {agent_rearm_delay_secs}s delay covers the operator's wait \
         (~{operator_wait_secs}s for you to TaskStop) PLUS the install.ps1 \
         download + write time PLUS a safety buffer. Re-arming sooner risks \
         loading a half-written giga.exe. When you re-arm after the full \
         delay, your new watcher loads the freshly-installed giga.exe and \
         auto-replays any messages that landed during the deaf window \
         (including the operator's post-install rearm broadcast). No other \
         action needed.\n\
         \n\
         (Non-Windows agents on this channel can ignore this — the [ack:] \
         prefix targets only the Windows slugs listed.)"
    );
    println!(
        "  -> Windows pre-install disarm: targeting agents [{ack_list}] on {} channel(s)",
        broadcast_channels.len(),
    );
    for ch in broadcast_channels {
        if dry_run {
            println!("    [dry-run] would post disarm to {ch}");
            continue;
        }
        if let Err(e) = post_with_subject_body(giga_exe, config, posting_agent, ch, &subject, &body)
        {
            eprintln!("    ! disarm post to {ch} failed ({e:#})");
        }
    }
    if dry_run {
        println!("    [dry-run] would sleep {operator_wait_secs}s for watchers to TaskStop (then run install)");
    } else {
        println!(
            "  -> sleeping {operator_wait_secs}s for watchers to TaskStop + release giga.exe lock"
        );
        std::thread::sleep(std::time::Duration::from_secs(operator_wait_secs));
    }
    Ok(())
}

/// Post the matching `[ack: <windows-slugs>]` re-arm message after
/// install.ps1 finishes on `peer`. The Windows agents see this on
/// their next turn and re-Monitor with the freshly-installed giga.exe.
pub(super) fn windows_post_install_rearm(
    giga_exe: &std::path::Path,
    config: &std::path::Path,
    posting_agent: &str,
    peer: &str,
    windows_agents: &[String],
    broadcast_channels: &[&str],
    dry_run: bool,
) -> Result<()> {
    let ack_list = windows_agents.join(",");
    let subject =
        format!("[ack: {ack_list}] giga.exe upgraded on `{peer}` — please re-arm your watcher");
    let body = format!(
        "install.ps1 completed on host `{peer}`. Please re-arm your inbox \
         watcher with the standard Monitor invocation from your AGENTS.md — \
         it will load the freshly-installed giga.exe. Confirm with \
         `giga --version` if you want to verify the new build."
    );
    println!(
        "  -> Windows post-install rearm: notifying agents [{ack_list}] on {} channel(s)",
        broadcast_channels.len(),
    );
    for ch in broadcast_channels {
        if dry_run {
            println!("    [dry-run] would post rearm to {ch}");
            continue;
        }
        if let Err(e) = post_with_subject_body(giga_exe, config, posting_agent, ch, &subject, &body)
        {
            eprintln!("    ! rearm post to {ch} failed ({e:#})");
        }
    }
    Ok(())
}

/// Re-invoke giga post with a custom subject + body. Used by the
/// Windows disarm/rearm pair (post_rearm hard-codes the
/// `[giga-rearm]` silent-execve subject; these messages need
/// different subjects and bodies).
fn post_with_subject_body(
    giga_exe: &std::path::Path,
    config: &std::path::Path,
    as_agent: &str,
    channel: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    let status = Command::new(giga_exe)
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
