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
//! * The local install runs the SAME canonical installer an operator
//!   would run manually — `install.sh` via bash on Linux/macOS,
//!   `install.ps1` via PowerShell on Windows (v0.6.12). URLs are
//!   hard-coded to this project's own GitHub release endpoint; no
//!   indirection.
//! * Overwriting the running binary is safe on Linux/macOS (open file
//!   handles keep the old binary mapped; subsequent invocations see
//!   the new inode). On Windows the in-place overwrite of a running
//!   `giga.exe` fails with sharing-violation — agents holding the
//!   binary (watchers, daemons) need to be TaskStop'd before upgrade,
//!   as the disarm/rearm flow handles automatically (v0.6.14+).
//! * Bootstrap post-failure (peer install failed; broadcast failed) is
//!   non-fatal — local install already succeeded, peers/agents can be
//!   re-prodded manually.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::config::{self, Config};

/// URLs for the per-platform installers — hard-coded to this
/// project's GitHub release "latest" endpoint. v0.4.1+ ships with
/// these baked in so `giga upgrade` doesn't need an extra config
/// knob. v0.6.12 split into per-platform: `install.sh` for
/// Linux/macOS, `install.ps1` for native Windows.
const INSTALL_SH_URL: &str =
    "https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh";
const INSTALL_PS1_URL: &str =
    "https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.ps1";

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
    // v0.4.5 bug fix: canonicalize the config path before passing it
    // to subprocesses (giga remote, giga post). Pre-fix: when the
    // operator ran `giga upgrade` from a non-swarm-dir CWD with the
    // default `giga-harness.toml`, the subprocess inherited the
    // operator's CWD and couldn't resolve the relative config path —
    // the post step failed with "config not found". design saw this
    // exact failure on 2026-06-02 after a 0.4.2 → 0.4.4 upgrade.
    let abs_config = std::fs::canonicalize(&args.config).unwrap_or(args.config.clone());

    // --- 0. resolve broadcast machinery up front so the local +
    // peer paths can both use it for the Windows disarm/rearm dance.
    let broadcast_channels: Vec<&str> = cfg
        .channels
        .iter()
        .filter(|c| config::is_broadcast_channel(&c.file))
        .map(|c| c.file.as_str())
        .collect();
    let posting_agent_early = match args.as_agent.clone() {
        Some(slug) => Some(slug),
        None => resolve_default_posting_agent(&cfg, &broadcast_channels),
    };

    // v0.6.14: local Windows agents (co-located on the operator host
    // via WSL interop — the single-host topology where Windows
    // agents live on the same physical box as the WSL operator).
    // They hold giga.exe locked just like remote-peer Windows
    // agents do, so we need the same disarm/rearm dance.
    //
    // For a swarm with no [[hosts]], "local Windows agents" = all
    // Windows-platform agents (single-host topology, every agent is
    // co-located with the operator). With [[hosts]], we filter to
    // this_host.
    let local_windows_agents: Vec<String> = match cfg.this_host.as_deref() {
        Some(th) => windows_agents_on_host(&cfg, th),
        None => cfg
            .agents
            .iter()
            .filter(|a| a.platform == "windows")
            .map(|a| a.name.clone())
            .collect(),
    };
    let has_local_windows = !local_windows_agents.is_empty();
    let local_host_label = cfg.this_host.clone().unwrap_or_else(|| "local".to_string());

    // --- 1. local install ---------------------------------------------
    println!("==> upgrading giga on local host");

    // 1a. Pre-install disarm for local Windows agents (if any). The
    //     dance is the same as the cross-host case — disarm/wait so
    //     the Windows-side install.ps1 can overwrite the locked .exe.
    if has_local_windows
        && !broadcast_channels.is_empty()
        && !args.skip_broadcast
    {
        match &posting_agent_early {
            Some(poster) => {
                if let Err(e) = windows_pre_install_disarm(
                    &abs_config,
                    poster,
                    &local_host_label,
                    &local_windows_agents,
                    &broadcast_channels,
                    WINDOWS_DISARM_GRACE_SECS,
                    args.dry_run,
                ) {
                    eprintln!(
                        "  ! local: pre-install disarm post failed ({e:#}) \
                         — local install.ps1 may fail if Windows watchers \
                         are still holding giga.exe"
                    );
                }
            }
            None => eprintln!(
                "  ! local: no posting agent resolved; skipping \
                 Windows disarm broadcast — Windows agents may still \
                 hold giga.exe locked when install.ps1 runs"
            ),
        }
    } else if has_local_windows && args.skip_broadcast {
        eprintln!(
            "  ! local: --skip-broadcast set with Windows agents \
             present; TaskStop their watchers manually before \
             install.ps1 to avoid sharing-violation"
        );
    }

    install_local(args.dry_run)?;

    // 1b. From WSL with co-located Windows agents, ALSO run
    //     install.ps1 via WSL interop so the Windows giga.exe gets
    //     refreshed alongside the WSL giga binary. On native Windows
    //     `install_local` already ran install.ps1 (v0.6.12 dispatch);
    //     on macOS / Linux without Windows agents this is a no-op.
    if has_local_windows && cfg!(target_os = "linux") {
        if let Err(e) = install_local_windows_via_wsl_interop(args.dry_run) {
            eprintln!(
                "  ! local: install.ps1 via WSL interop failed ({e:#}) \
                 — Windows-side giga.exe NOT upgraded. Run install.ps1 \
                 from a Windows shell manually."
            );
        }
    }

    // 1c. Post-install rearm broadcast for local Windows agents.
    if has_local_windows
        && !broadcast_channels.is_empty()
        && !args.skip_broadcast
    {
        if let Some(poster) = &posting_agent_early {
            if let Err(e) = windows_post_install_rearm(
                &abs_config,
                poster,
                &local_host_label,
                &local_windows_agents,
                &broadcast_channels,
                args.dry_run,
            ) {
                eprintln!(
                    "  ! local: post-install rearm post failed ({e:#}) \
                     — agents can re-arm manually from AGENTS.md"
                );
            }
        }
    }

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
    // v0.6.14: cross-host Windows peers get the same disarm/rearm
    // dance as the local Windows agents handled above. broadcast_channels
    // + posting_agent_early were resolved in step 0.
    if !peers.is_empty() {
        println!("\n==> upgrading giga on {} peer host(s)", peers.len());
        for peer in &peers {
            let peer_platform = infer_host_platform(&cfg, peer);
            let windows_agents = windows_agents_on_host(&cfg, peer);

            // Pre-install disarm for Windows peers — only meaningful
            // when we have agents to address AND a posting agent.
            if peer_platform == "windows"
                && !windows_agents.is_empty()
                && !broadcast_channels.is_empty()
                && !args.skip_broadcast
            {
                match &posting_agent_early {
                    Some(poster) => {
                        if let Err(e) = windows_pre_install_disarm(
                            &abs_config,
                            poster,
                            peer,
                            &windows_agents,
                            &broadcast_channels,
                            WINDOWS_DISARM_GRACE_SECS,
                            args.dry_run,
                        ) {
                            eprintln!(
                                "  ! {peer}: pre-install disarm post failed ({e:#}) \
                                 — continuing with install but giga.exe lock may block it"
                            );
                        }
                    }
                    None => eprintln!(
                        "  ! {peer}: no posting agent resolved; \
                         skipping Windows pre-install disarm — \
                         install.ps1 will likely fail if watchers are running"
                    ),
                }
            } else if peer_platform == "windows"
                && !windows_agents.is_empty()
                && args.skip_broadcast
            {
                eprintln!(
                    "  ! {peer}: --skip-broadcast set; you must manually \
                     TaskStop the watchers on Windows agents before install.ps1 \
                     can succeed (file lock)"
                );
            }

            match install_remote(&abs_config, peer, peer_platform, args.dry_run) {
                Ok(()) => println!("  + {peer}: upgraded ({peer_platform})"),
                Err(e) => eprintln!("  ! {peer}: upgrade failed ({e:#}) — run install on that host manually"),
            }

            // Post-install re-arm for Windows peers — targeted at
            // their agents so they pick up the new binary. The
            // generic final rearm broadcast below ALSO covers them
            // (silent execve for Linux, redundant text for Windows)
            // but the targeted message is what actually closes the
            // loop on Windows.
            if peer_platform == "windows"
                && !windows_agents.is_empty()
                && !broadcast_channels.is_empty()
                && !args.skip_broadcast
            {
                if let Some(poster) = &posting_agent_early {
                    if let Err(e) = windows_post_install_rearm(
                        &abs_config,
                        poster,
                        peer,
                        &windows_agents,
                        &broadcast_channels,
                        args.dry_run,
                    ) {
                        eprintln!(
                            "  ! {peer}: post-install rearm post failed ({e:#}) \
                             — agents can re-arm manually from AGENTS.md"
                        );
                    }
                }
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
    if broadcast_channels.is_empty() {
        println!("\n(no broadcast channels found — skipping rearm post)");
        return Ok(());
    }

    // v0.4.3 (Bug 74): auto-detect a posting agent when --as not
    // supplied. Priority:
    //   1. Explicit --as flag (operator's choice; never overridden)
    //   2. swarm_boss agent on this_host (the canonical orchestrator)
    //   3. Any agent on this_host that participates in the first
    //      broadcast channel (best-effort fallback)
    //   4. Print the manual command if nothing resolves.
    // Lets `giga upgrade` "just work" without the operator needing
    // to know which slug to pass.
    let posting_agent = match posting_agent_early.clone() {
        Some(slug) => {
            if args.as_agent.is_none() {
                println!("\n(auto-detected --as `{slug}` — pass --as explicitly to override)");
            }
            slug
        }
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
        match post_rearm(&abs_config, &posting_agent, ch) {
            Ok(()) => println!("  + posted to {ch}"),
            Err(e) => eprintln!("  ! {ch}: post failed ({e:#})"),
        }
    }
    println!("\nupgrade complete.");
    Ok(())
}

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
fn install_local(dry_run: bool) -> Result<()> {
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
fn install_local_windows_via_wsl_interop(dry_run: bool) -> Result<()> {
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
fn install_remote(
    config: &std::path::Path,
    peer: &str,
    peer_platform: &str,
    dry_run: bool,
) -> Result<()> {
    let (shell_program, shell_args, installer_cmd): (&str, &[&str], String) = if peer_platform
        == "windows"
    {
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
    let status = Command::new(std::env::current_exe()?)
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

/// v0.6.14: how long the operator waits after posting the
/// pre-install disarm broadcast for Windows agents to TaskStop their
/// watchers + release the giga.exe file lock. Sized for "next agent
/// turn fires + agent acts" round-trip. If install.ps1 still fails
/// with a sharing violation, the agent didn't act in time — the
/// operator can retry or extend.
const WINDOWS_DISARM_GRACE_SECS: u64 = 60;

/// List the agent slugs configured on `host` whose platform is
/// `windows`. Drives the [ack: ...] addressing for the pre-install
/// disarm + post-install rearm targeted broadcasts.
fn windows_agents_on_host(cfg: &Config, host: &str) -> Vec<String> {
    cfg.agents
        .iter()
        .filter(|a| a.host.as_deref() == Some(host) && a.platform == "windows")
        .map(|a| a.name.clone())
        .collect()
}

/// Post a targeted `[ack: <windows-slugs>]` broadcast asking the
/// Windows agents on `peer` to TaskStop their watchers + stand by
/// while install.ps1 runs. Then sleep `grace_secs` operator-side to
/// give them a chance to act. The `[ack:]` prefix already filters
/// fanout in the watcher (see watch.rs:347) so non-Windows agents on
/// the same channel are unaffected.
#[allow(clippy::too_many_arguments)]
fn windows_pre_install_disarm(
    config: &std::path::Path,
    posting_agent: &str,
    peer: &str,
    windows_agents: &[String],
    broadcast_channels: &[&str],
    grace_secs: u64,
    dry_run: bool,
) -> Result<()> {
    let ack_list = windows_agents.join(",");
    let subject = format!(
        "[ack: {ack_list}] giga upgrade incoming on `{peer}` — TaskStop your watcher and stand by"
    );
    let body = format!(
        "giga.exe on host `{peer}` is about to be upgraded via install.ps1, \
         which requires the binary to be unlocked (Windows file-locks running \
         exes). Please TaskStop your giga inbox watcher and stand by for \
         ~{grace_secs} seconds. After install completes you'll get an [ack: ...] \
         re-arm message; until then no need to do anything else. \
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
        if let Err(e) = post_with_subject_body(config, posting_agent, ch, &subject, &body) {
            eprintln!("    ! disarm post to {ch} failed ({e:#})");
        }
    }
    if dry_run {
        println!("    [dry-run] would sleep {grace_secs}s for watchers to release lock");
    } else {
        println!("  -> sleeping {grace_secs}s for watchers to TaskStop + release giga.exe lock");
        std::thread::sleep(std::time::Duration::from_secs(grace_secs));
    }
    Ok(())
}

/// Post the matching `[ack: <windows-slugs>]` re-arm message after
/// install.ps1 finishes on `peer`. The Windows agents see this on
/// their next turn and re-Monitor with the freshly-installed giga.exe.
fn windows_post_install_rearm(
    config: &std::path::Path,
    posting_agent: &str,
    peer: &str,
    windows_agents: &[String],
    broadcast_channels: &[&str],
    dry_run: bool,
) -> Result<()> {
    let ack_list = windows_agents.join(",");
    let subject = format!(
        "[ack: {ack_list}] giga.exe upgraded on `{peer}` — please re-arm your watcher"
    );
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
        if let Err(e) = post_with_subject_body(config, posting_agent, ch, &subject, &body) {
            eprintln!("    ! rearm post to {ch} failed ({e:#})");
        }
    }
    Ok(())
}

/// Re-invoke giga post with a custom subject + body. Used by the
/// Windows disarm/rearm pair (post_rearm above hard-codes the
/// `[giga-rearm]` silent-execve subject; these messages need
/// different subjects and bodies).
fn post_with_subject_body(
    config: &std::path::Path,
    as_agent: &str,
    channel: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
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

/// Infer a host's platform from the agents configured on it.
/// Heuristic: if any agent on the host has `platform = "windows"`,
/// the host is Windows; otherwise it's POSIX (wsl / linux / macos).
/// Real swarms are platform-homogeneous per host today; mixed-host
/// scenarios would need a Host.platform field, which we can add when
/// the case shows up.
fn infer_host_platform(cfg: &Config, host: &str) -> &'static str {
    let any_windows = cfg
        .agents
        .iter()
        .any(|a| a.host.as_deref() == Some(host) && a.platform == "windows");
    if any_windows {
        "windows"
    } else {
        "unix"
    }
}

/// Post the canonical "please re-arm your watcher" message to one
/// broadcast channel. Re-invokes this same binary so the post goes
/// through the standard `giga post` validation + dual-write path.
///
/// v0.6.3: subject uses the `[giga-rearm]` prefix. v0.6.3+ watchers
/// detect this and self-rearm via in-place execve — no agent turn
/// triggered, no API call. Pre-v0.6.3 watchers don't recognize the
/// prefix and fall back to the `All` branch (wake-up rearm), so
/// behavior is backward-compatible during the v0.6.2 → v0.6.3
/// transition. From the FIRST upgrade onto a v0.6.3+ swarm onward,
/// rearm broadcasts are silent end-to-end.
fn post_rearm(config: &std::path::Path, as_agent: &str, channel: &str) -> Result<()> {
    let subject = "[giga-rearm] giga upgraded — please re-arm your inbox watcher";
    let body = "giga has been upgraded to the latest release on all hosts. \
                v0.6.3+ watchers self-rearm silently via in-place execve on \
                this `[giga-rearm]` broadcast — no agent turn triggered, no \
                API call. Pre-v0.6.3 watchers see this as an ordinary \
                broadcast and need to TaskStop + re-Monitor manually \
                (one final time; after this upgrade lands, future upgrades \
                are silent).";
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

/// v0.4.3 (Bug 74): pick a default agent to post the rearm broadcast
/// AS when the operator didn't supply --as. Prefers the swarm_boss
/// agent on this_host; falls back to any local agent that
/// participates in the first broadcast channel. Returns None if
/// nothing reasonable is in scope — caller prints the manual
/// command in that case.
fn resolve_default_posting_agent(cfg: &Config, broadcast_channels: &[&str]) -> Option<String> {
    let this_host = cfg.this_host.as_deref();
    // 1. swarm_boss on this_host (if multi-host). The canonical
    //    orchestrator and the agent whose AGENTS.md is set up to
    //    react to such operational broadcasts.
    let boss = cfg.agents.iter().find(|a| {
        a.swarm_boss
            && match this_host {
                Some(this) => a.host.as_deref() == Some(this) || a.host.is_none(),
                None => a.host.is_none(),
            }
    });
    if let Some(b) = boss {
        return Some(b.name.clone());
    }
    // 2. First local agent that participates in the first broadcast
    //    channel. Resolves the "no swarm_boss flagged" case (e.g.,
    //    swarms that use tmux daemons instead).
    let first_channel = broadcast_channels.first()?;
    let channel = cfg.channels.iter().find(|c| &c.file.as_str() == first_channel)?;
    for participant_name in &channel.participants {
        let agent = cfg.agents.iter().find(|a| &a.name == participant_name);
        if let Some(a) = agent {
            let is_local = match this_host {
                Some(this) => a.host.as_deref() == Some(this) || a.host.is_none(),
                None => true,
            };
            if is_local {
                return Some(a.name.clone());
            }
        }
    }
    None
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
            "  giga post {ch} --as <participant-slug> \\\n    --subject \"giga upgraded — please re-arm your inbox watcher\" \\\n    --body \"giga has been upgraded; TaskStop your watcher and re-arm from AGENTS.md.\""
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

    /// v0.4.3 (Bug 74): swarm_boss agent is preferred when --as not
    /// supplied. Makes "just say 'upgrade giga'" work end-to-end.
    #[test]
    fn resolve_default_posting_agent_prefers_swarm_boss() {
        let cfg = Config::load_str_for_test(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"
swarm_boss = true
[[agents]]
name = "code"
workdir = "/h/code"
role = "."
platform = "wsl"
[[channels]]
file = "_broadcast.md"
side = "wsl"
participants = ["design", "code"]
"#,
        )
        .unwrap();
        let picked = resolve_default_posting_agent(&cfg, &["_broadcast.md"]);
        assert_eq!(picked.as_deref(), Some("design"));
    }

    /// v0.4.3: when no swarm_boss, fall back to any participant of
    /// the broadcast channel (local first when this_host is set).
    #[test]
    fn resolve_default_posting_agent_falls_back_to_first_broadcast_participant() {
        let cfg = Config::load_str_for_test(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"
[[agents]]
name = "code"
workdir = "/h/code"
role = "."
platform = "wsl"
[[channels]]
file = "_broadcast.md"
side = "wsl"
participants = ["design", "code"]
"#,
        )
        .unwrap();
        let picked = resolve_default_posting_agent(&cfg, &["_broadcast.md"]);
        // First participant of _broadcast.md = "design"
        assert_eq!(picked.as_deref(), Some("design"));
    }

    /// v0.4.3: returns None when there are no agents at all (so the
    /// caller falls through to the manual-command print path).
    #[test]
    fn resolve_default_posting_agent_returns_none_when_no_match() {
        let cfg = Config::load_str_for_test(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
"#,
        )
        .unwrap();
        // No broadcast channels passed in → nothing to fall back to.
        let picked = resolve_default_posting_agent(&cfg, &[]);
        assert!(picked.is_none());
    }

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

    /// v0.6.12 regression guard. Pre-fix `giga upgrade` on Windows
    /// run the bash/install.sh path instead of powershell/install.ps1,
    /// which either failed (no bash on PATH) or — worse — wrote the
    /// Linux binary into a POSIX path that giga.exe never looks at.
    /// Helper: write a config + this_host.toml to a tempdir and load.
    fn load_with_this_host(cfg_text: &str, this_host: &str) -> Config {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, cfg_text).unwrap();
        std::fs::write(
            tmp.path().join("this_host.toml"),
            format!("this_host = \"{this_host}\"\n"),
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        // Hold tempdir for the lifetime of the test via leak — small
        // and the test process exits right after.
        std::mem::forget(tmp);
        cfg
    }

    #[test]
    fn infer_host_platform_returns_windows_when_any_agent_is_windows() {
        let cfg = load_with_this_host(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"
[[hosts]]
name = "local-wsl"
tailnet_hostname = "local-wsl.tail0.ts.net"
[[agents]]
name = "win-agent-1"
workdir = "C:\\win-agent-1"
role = "."
platform = "windows"
host = "host-b"
"#,
            "local-wsl",
        );
        assert_eq!(infer_host_platform(&cfg, "host-b"), "windows");
    }

    #[test]
    fn infer_host_platform_returns_unix_for_wsl_only_host() {
        let cfg = load_with_this_host(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"
[[hosts]]
name = "local-wsl"
tailnet_hostname = "local-wsl.tail0.ts.net"
[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"
host = "host-a"
"#,
            "local-wsl",
        );
        assert_eq!(infer_host_platform(&cfg, "host-a"), "unix");
    }

    #[test]
    fn windows_agents_on_host_filters_by_host_and_platform() {
        let cfg = load_with_this_host(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"
[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"
[[hosts]]
name = "local"
tailnet_hostname = "local.tail0.ts.net"
[[agents]]
name = "win-agent-1"
workdir = "C:\\win-agent-1"
role = "."
platform = "windows"
host = "host-b"
[[agents]]
name = "win-agent-2"
workdir = "C:\\win-agent-2"
role = "."
platform = "windows"
host = "host-b"
[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"
host = "host-a"
"#,
            "local",
        );
        let mut windows_hosts = windows_agents_on_host(&cfg, "host-b");
        windows_hosts.sort();
        assert_eq!(windows_hosts, vec!["win-agent-1".to_string(), "win-agent-2".to_string()]);
        // host-a is not Windows → empty.
        assert!(windows_agents_on_host(&cfg, "host-a").is_empty());
        // local host with no agents at all → empty.
        assert!(windows_agents_on_host(&cfg, "local").is_empty());
    }

    #[test]
    fn infer_host_platform_returns_unix_when_host_has_no_agents() {
        // Defensive default — an empty host slot shouldn't make us
        // try to PowerShell-install. Fall back to bash/install.sh.
        let cfg = load_with_this_host(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "empty-host"
tailnet_hostname = "empty.tail0.ts.net"
[[hosts]]
name = "local-wsl"
tailnet_hostname = "local-wsl.tail0.ts.net"
"#,
            "local-wsl",
        );
        assert_eq!(infer_host_platform(&cfg, "empty-host"), "unix");
    }
}
