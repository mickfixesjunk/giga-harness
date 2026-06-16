//! One-shot peer bootstrap + remote `giga init`, invoked by
//! `giga add-host` / `giga add-agent --host` so a TOML change
//! propagates to a peer immediately instead of waiting for the next
//! sync tick.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::config::Config;
use crate::foundation::paths::{to_unix, unix_join};
use crate::foundation::ssh::{rsync_ssh_e_arg, ssh_exec};

/// One-shot bootstrap of a peer host after the operator-side
/// `add-agent --host <peer>` (or any TOML change that should propagate
/// immediately rather than waiting for the next sync tick):
///
///   1. mkdir -p the peer's remote_config_dir (so the rsync target
///      exists — rsync doesn't create grandparent dirs by default)
///   2. rsync the canonical giga-harness.toml to the peer
///   3. if the peer has no this_host.toml yet, create one with
///      `this_host = "<peer-name>"` (idempotent — won't overwrite an
///      existing one a previous run set up)
///
/// Best-effort: errors are returned so the caller can decide whether
/// to surface them or just warn + carry on. Used by add-agent in the
/// cross-host case.
pub fn bootstrap_peer(cfg: &Config, peer_name: &str, canonical_config_path: &Path) -> Result<()> {
    let peer = cfg
        .hosts
        .iter()
        .find(|h| h.name == peer_name)
        .ok_or_else(|| anyhow!("unknown peer host `{peer_name}`"))?;

    let local_config_dir = canonical_config_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let remote_dir = peer
        .remote_config_dir
        .clone()
        .unwrap_or_else(|| local_config_dir.clone());
    let user = peer
        .ssh_user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .ok_or_else(|| anyhow!("can't determine SSH user for host `{peer_name}`"))?;
    let ssh_target = format!("{user}@{}", peer.tailnet_hostname);

    // 1. mkdir -p the peer's config dir. Normalize separators so a
    //    Windows operator's `\`-laden PathBuf doesn't end up in the
    //    remote shell command.
    let remote_dir_unix = to_unix(&remote_dir);
    let mkdir_cmd = format!(
        "mkdir -p {}",
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(remote_dir_unix.as_str()))
    );
    ssh_exec(&ssh_target, &mkdir_cmd).context("creating remote config dir")?;

    // 2. rsync the WHOLE swarm dir (canonical TOML + agents/ templates
    //    + handover stubs + anything else under the config dir).
    //    Excludes `*.local.toml` (v0.3.9 convention) so each host's
    //    per-host identity files aren't trampled; excludes workdirs/
    //    so an agent's accumulated session state isn't clobbered. The
    //    remote `giga init` (step 3 from the add-agent caller)
    //    re-renders workdir AGENTS.md from the template that this
    //    rsync just delivered. Legacy `this_host.toml` is excluded
    //    explicitly too for swarms that haven't been migrated yet.
    let ssh_e = rsync_ssh_e_arg();
    let dir_rsync_status = Command::new("rsync")
        .args([
            "-avz",
            "-e",
            &ssh_e,
            "--exclude",
            "*.local.toml",
            "--exclude",
            "this_host.toml",
            "--exclude",
            "workdirs/",
            &format!("{}/", local_config_dir.display()),
            &format!("{ssh_target}:{remote_dir_unix}/"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking rsync of swarm dir to {ssh_target}"))?;
    if !dir_rsync_status.success() {
        return Err(anyhow!(
            "rsync swarm dir -> {ssh_target}:{remote_dir_unix} exited {}",
            dir_rsync_status.code().unwrap_or(-1),
        ));
    }

    // 3. ensure the peer has a per-host identity file. v0.3.9: write the
    //    new `this_host.local.toml` name. Idempotent — only set if neither
    //    the new nor the legacy `this_host.toml` exists yet.
    let this_host_path = unix_join(&remote_dir, crate::config::THIS_HOST_FILE);
    let legacy_path = unix_join(&remote_dir, crate::config::THIS_HOST_FILE_LEGACY);
    let escaped_new =
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(this_host_path.as_str()));
    let escaped_legacy =
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(legacy_path.as_str()));
    let ensure_cmd = format!(
        "test -f {escaped_new} || test -f {escaped_legacy} || echo 'this_host = \"{peer_name}\"' > {escaped_new}",
    );
    ssh_exec(&ssh_target, &ensure_cmd).context("ensuring remote this_host identity file")?;

    Ok(())
}

/// Run `giga init` on the peer to scaffold workdirs + AGENTS.md for
/// agents whose host matches the peer (init is host-aware as of v1.1).
/// Best-effort: callers warn on failure rather than blocking local
/// success.
pub fn run_remote_giga_init(
    cfg: &Config,
    peer_name: &str,
    canonical_config_path: &Path,
) -> Result<()> {
    let peer = cfg
        .hosts
        .iter()
        .find(|h| h.name == peer_name)
        .ok_or_else(|| anyhow!("unknown peer host `{peer_name}`"))?;
    let local_config_dir = canonical_config_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let remote_dir = peer
        .remote_config_dir
        .clone()
        .unwrap_or_else(|| local_config_dir.clone());
    let user = peer
        .ssh_user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .ok_or_else(|| anyhow!("can't determine SSH user for host `{peer_name}`"))?;
    let ssh_target = format!("{user}@{}", peer.tailnet_hostname);
    let remote_dir_unix = to_unix(&remote_dir);
    let remote_cmd = format!(
        "cd {} && giga init",
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(remote_dir_unix.as_str()))
    );
    ssh_exec(&ssh_target, &remote_cmd).context("remote `giga init`")
}
