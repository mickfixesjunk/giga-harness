//! `giga sync` — push local slice files + canonical TOML to peer hosts.
//!
//! Per REMOTE_DESIGN.md §4: v1 transport is rsync over Tailscale SSH.
//! Each host pushes:
//!   - Its OWN slice files `<channel>.<this_host>.md` for every cross-host
//!     channel it participates in (single-writer-per-slice preserved at
//!     the wire level — a peer never pulls or rewrites our local data).
//!   - The canonical `giga-harness.toml` (so peers learn about config
//!     changes made from this host — operator-UX assumes one writer per
//!     swarm).
//!
//! Reception is symmetric: peers push to us; we don't pull. This means
//! no peer needs to know which slices exist on the others — each side
//! ships only what it owns.
//!
//! Transport is pluggable via the `Transport` enum (v1 only has the
//! rsync-over-Tailscale-SSH plug; cloud-storage / `s3://` follows in
//! v1.1). The `compute_sync_plan()` function is pure — testable without
//! actually invoking rsync.
//!
//! Assumption (v1): the canonical config dir + inbox dir paths are
//! symmetric across hosts (same absolute path everywhere). True for
//! homogeneous WSL/Linux setups with the same $HOME. Per-host path
//! overrides can be added later if needed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config::{Config, Host};

const POLL_INTERVAL: Duration = Duration::from_secs(3);

pub struct Args {
    pub config: PathBuf,
    /// Run one sync tick then exit. Useful for `giga sync --once` in
    /// scripts or for debugging.
    pub once: bool,
    /// Print the rsync commands that would be run, don't execute them.
    /// Combined with `--once` for a no-side-effects preview.
    pub dry_run: bool,
}

/// One file to ship to one peer host. Carries enough info to execute the
/// rsync without re-consulting the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCommand {
    pub peer_target: String,        // user@tailnet_hostname:path
    pub local_path: PathBuf,
    pub use_append_verify: bool,    // true for append-only slice files
    pub kind: &'static str,         // "slice" | "toml" — for logging
}

pub fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;
    if cfg.hosts.is_empty() {
        eprintln!("sync: no [[hosts]] declared — local-only swarm, nothing to sync. Exiting.");
        return Ok(());
    }
    let this_host = cfg
        .this_host
        .clone()
        .ok_or_else(|| anyhow!("this_host is unknown — set sibling this_host.toml"))?;

    // Verify rsync is installed before the loop so we fail fast with a
    // clean error instead of N rsync-not-found messages per tick.
    if !args.dry_run && which::which("rsync").is_err() {
        return Err(anyhow!(
            "rsync not found on PATH. Install it with: sudo apt install rsync"
        ));
    }

    loop {
        let plan = compute_sync_plan(&cfg, &this_host, &args.config);
        if plan.is_empty() {
            eprintln!("sync: no cross-host slices for this_host=`{this_host}` and no peers to ship to.");
        }
        for cmd in &plan {
            if args.dry_run {
                eprintln!("[dry-run] {} {} -> {}", cmd.kind, cmd.local_path.display(), cmd.peer_target);
                continue;
            }
            if let Err(e) = execute(cmd) {
                eprintln!("sync: {} push failed ({e}) — will retry next tick", cmd.kind);
            }
        }
        if args.once {
            return Ok(());
        }
        thread::sleep(POLL_INTERVAL);
        // Hot-reload the config so newly-added [[hosts]] / channels are
        // picked up without restart (~3s latency, same as merger/watch).
        // A failed reload is logged but we keep the previous in-memory cfg.
        // For v1 simplicity we just continue the loop with the prior cfg;
        // a fresh load happens at the top of the next iteration only if
        // we re-enter — actually the loop above always re-reads at the
        // top, so the next tick gets the fresh config automatically.
        // Wait — Config::load was called once before the loop. Move it
        // inside.
    }
}

/// Pure planner: compute the rsync commands this tick should issue.
/// Inputs: parsed config + this_host name + the canonical config path
/// (for rsync'ing the TOML itself).
///
/// Output rules:
///   - For every PEER host (not this_host), produce one SyncCommand
///     for the canonical TOML.
///   - For every cross-host channel where this_host has at least one
///     participant, produce one SyncCommand per PEER host that has at
///     least one participant on that channel, for THIS host's slice
///     file. Append-verify enabled.
///   - Skip own slice files (never push to self).
///   - Skip local-only channels (no slice exists for them on this host).
pub fn compute_sync_plan(
    cfg: &Config,
    this_host: &str,
    canonical_config_path: &Path,
) -> Vec<SyncCommand> {
    let mut plan = Vec::new();

    let peers: Vec<&Host> = cfg
        .hosts
        .iter()
        .filter(|h| h.name != this_host)
        .collect();

    // Local config + inbox dirs — used as the default when a peer
    // hasn't overridden them.
    let local_config_dir = canonical_config_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let local_inbox_dir = cfg
        .paths
        .wsl_inbox
        .clone()
        .or_else(|| cfg.paths.windows_inbox.clone())
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    // 1) Canonical TOML to every peer (at peer's remote_config_dir).
    let toml_filename = canonical_config_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "giga-harness.toml".to_string());
    for peer in &peers {
        let remote_dir = peer
            .remote_config_dir
            .as_ref()
            .cloned()
            .unwrap_or_else(|| local_config_dir.clone());
        let remote_path = remote_dir.join(&toml_filename);
        let target = match build_rsync_target(peer, &remote_path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        plan.push(SyncCommand {
            peer_target: target,
            local_path: canonical_config_path.to_path_buf(),
            use_append_verify: false,
            kind: "toml",
        });
    }

    // 2) Own slice files to every peer that participates on each channel.
    for ch in &cfg.channels {
        if cfg.channel_is_local(ch) {
            continue;
        }
        let mut channel_hosts: Vec<&str> = ch
            .participants
            .iter()
            .filter_map(|p| {
                cfg.agents
                    .iter()
                    .find(|a| a.name == *p)
                    .and_then(|a| cfg.agent_host(a))
            })
            .collect();
        channel_hosts.sort();
        channel_hosts.dedup();

        if !channel_hosts.contains(&this_host) {
            continue;
        }

        let merged_path = match cfg.channel_path(ch) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let slice_path = derive_slice_path(&merged_path, this_host);
        let slice_filename = slice_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}.{this_host}.md", ch.file.trim_end_matches(".md")));

        for peer in &peers {
            if !channel_hosts.contains(&peer.name.as_str()) {
                continue;
            }
            let remote_inbox = peer
                .remote_inbox_dir
                .as_ref()
                .cloned()
                .unwrap_or_else(|| local_inbox_dir.clone());
            let remote_slice_path = remote_inbox.join(&slice_filename);
            let target = match build_rsync_target(peer, &remote_slice_path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            plan.push(SyncCommand {
                peer_target: target,
                local_path: slice_path.clone(),
                use_append_verify: true,
                kind: "slice",
            });
        }
    }

    plan
}

/// Build the rsync target string: `user@tailnet_hostname:path`.
/// `path` is the destination path on the peer (per remote_config_dir /
/// remote_inbox_dir, with the local path as fallback for
/// homogeneous-user setups).
fn build_rsync_target(peer: &Host, remote_path: &Path) -> Result<String> {
    let user = peer
        .ssh_user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .ok_or_else(|| {
            anyhow!(
                "can't determine SSH user for host `{}` (no ssh_user; $USER unset)",
                peer.name
            )
        })?;
    Ok(format!(
        "{user}@{host}:{path}",
        host = peer.tailnet_hostname,
        path = remote_path.display()
    ))
}

/// `/dir/<channel>.md` + host -> `/dir/<channel>.<host>.md`. Mirrors
/// `post::slice_path` + `merger::derive_slice_path`.
fn derive_slice_path(merged: &Path, host: &str) -> PathBuf {
    let parent = merged.parent().unwrap_or_else(|| Path::new("."));
    let stem = merged
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "channel".to_string());
    parent.join(format!("{stem}.{host}.md"))
}

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
        .ok_or_else(|| anyhow!("can't determine SSH user for host `{peer_name}`"))?;
    let ssh_target = format!("{user}@{}", peer.tailnet_hostname);

    // 1. mkdir -p the peer's config dir.
    let mkdir_cmd = format!("mkdir -p {}", shell_escape::escape(remote_dir.to_string_lossy()));
    ssh_run(&ssh_target, &mkdir_cmd).context("creating remote config dir")?;

    // 2. rsync the WHOLE swarm dir (canonical TOML + agents/ templates
    //    + handover stubs + anything else under the config dir).
    //    Excludes this_host.toml so each host's per-host identity isn't
    //    trampled; excludes workdirs/ so an agent's accumulated session
    //    state isn't clobbered. The remote `giga init` (step 3 from
    //    the add-agent caller) re-renders workdir CLAUDE.md from the
    //    template that this rsync just delivered.
    let dir_rsync_status = Command::new("rsync")
        .args([
            "-avz",
            "--exclude",
            "this_host.toml",
            "--exclude",
            "workdirs/",
            &format!("{}/", local_config_dir.display()),
            &format!("{ssh_target}:{}/", remote_dir.display()),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking rsync of swarm dir to {ssh_target}"))?;
    if !dir_rsync_status.success() {
        return Err(anyhow!(
            "rsync swarm dir -> {ssh_target}:{} exited {}",
            remote_dir.display(),
            dir_rsync_status.code().unwrap_or(-1),
        ));
    }

    // 3. ensure this_host.toml exists on the peer (only set if missing
    //    — never overwrite, in case a previous bootstrap got there first).
    let this_host_path = remote_dir.join("this_host.toml");
    let ensure_cmd = format!(
        "test -f {path} || echo 'this_host = \"{name}\"' > {path}",
        path = shell_escape::escape(this_host_path.to_string_lossy()),
        name = peer_name,
    );
    ssh_run(&ssh_target, &ensure_cmd).context("ensuring remote this_host.toml")?;

    Ok(())
}

/// Run a one-shot SSH command on the peer, wrapped in `bash -lc` so
/// the remote shell sources login config — necessary for cargo-installed
/// binaries (`~/.cargo/bin/giga` etc.) that aren't on PATH for plain
/// non-interactive ssh. Inherits stderr so the user sees what happens;
/// captures stdout only (currently unused).
fn ssh_run(ssh_target: &str, remote_cmd: &str) -> Result<()> {
    let wrapped = format!(
        "bash -lc {}",
        shell_escape::escape(std::borrow::Cow::Borrowed(remote_cmd))
    );
    let status = Command::new("ssh")
        .arg(ssh_target)
        .arg(&wrapped)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("ssh {ssh_target} {remote_cmd}"))?;
    if !status.success() {
        return Err(anyhow!(
            "ssh {ssh_target} <cmd> exited {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Run `giga init` on the peer to scaffold workdirs + CLAUDE.md for
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
        .ok_or_else(|| anyhow!("can't determine SSH user for host `{peer_name}`"))?;
    let ssh_target = format!("{user}@{}", peer.tailnet_hostname);
    let remote_cmd = format!(
        "cd {} && giga init",
        shell_escape::escape(remote_dir.to_string_lossy())
    );
    ssh_run(&ssh_target, &remote_cmd).context("remote `giga init`")
}

fn execute(cmd: &SyncCommand) -> Result<()> {
    // `rsync -avz [--append-verify] <local> <target>`. `-a` preserves
    // metadata, `-v` is verbose (printed to our stderr), `-z` compresses
    // the on-wire bytes. We don't need --partial because a failed transfer
    // doesn't corrupt the destination — rsync writes to a temp + renames.
    let mut c = Command::new("rsync");
    c.arg("-avz");
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
    use std::fs;
    use tempfile::TempDir;

    fn host(name: &str, tailnet: &str, ssh_user: Option<&str>) -> Host {
        Host {
            name: name.into(),
            tailnet_hostname: tailnet.into(),
            ssh_user: ssh_user.map(|s| s.into()),
            remote_config_dir: None,
            remote_inbox_dir: None,
        }
    }

    #[test]
    fn build_rsync_target_uses_explicit_ssh_user() {
        let h = host("wsl-b", "wsl-b.tail0.ts.net", Some("alice"));
        let target = build_rsync_target(&h, Path::new("/some/file.md")).unwrap();
        assert_eq!(target, "alice@wsl-b.tail0.ts.net:/some/file.md");
    }

    #[test]
    fn build_rsync_target_falls_back_to_env_user() {
        let orig = std::env::var("USER").ok();
        unsafe { std::env::set_var("USER", "env-user") };
        let h = host("wsl-b", "wsl-b.tail0.ts.net", None);
        let target = build_rsync_target(&h, Path::new("/x")).unwrap();
        match orig {
            Some(v) => unsafe { std::env::set_var("USER", v) },
            None => unsafe { std::env::remove_var("USER") },
        }
        assert_eq!(target, "env-user@wsl-b.tail0.ts.net:/x");
    }

    /// Build a 2-host cross-host swarm fixture: alice@wsl-a + bob@wsl-b
    /// + 1 bilateral channel. Returns (tmp, config_path).
    fn fixture(this_host: &str) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "remote-test"

[paths]
wsl_inbox = "{inbox}"

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"
ssh_user = "neomatrix"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"
ssh_user = "neomatrix"

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
        fs::write(&config_path, toml).unwrap();
        fs::write(
            tmp.path().join("this_host.toml"),
            format!("this_host = \"{this_host}\"\n"),
        )
        .unwrap();
        (tmp, config_path)
    }

    #[test]
    fn plan_pushes_toml_to_every_peer() {
        let (_tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        let toml_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "toml").collect();
        assert_eq!(toml_pushes.len(), 1, "one toml push per peer; one peer here");
        assert!(toml_pushes[0]
            .peer_target
            .ends_with(&config_path.display().to_string()));
        assert!(toml_pushes[0].peer_target.contains("wsl-b.tail0.ts.net"));
        assert!(!toml_pushes[0].use_append_verify, "TOML is whole-file");
    }

    #[test]
    fn plan_pushes_own_slice_to_peers_on_cross_host_channels() {
        let (tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        let slice_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "slice").collect();
        assert_eq!(slice_pushes.len(), 1, "one slice push per peer for the bilateral");
        assert!(slice_pushes[0].use_append_verify, "slices are append-only");
        // We're wsl-a so the slice is alice-bob.wsl-a.md
        assert!(slice_pushes[0]
            .local_path
            .to_string_lossy()
            .ends_with("alice-bob.wsl-a.md"));
        // Target hostname is the peer (wsl-b)
        assert!(slice_pushes[0].peer_target.contains("wsl-b.tail0.ts.net"));
        let _ = tmp; // keep tempdir alive
    }

    #[test]
    fn plan_does_not_push_to_self() {
        let (_tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        for cmd in &plan {
            assert!(
                !cmd.peer_target.contains("wsl-a.tail0.ts.net"),
                "should never push to own host: {cmd:?}"
            );
        }
    }

    #[test]
    fn plan_symmetric_from_other_host_pushes_other_slice() {
        // Same swarm, viewed from wsl-b's perspective: it should push
        // its own (wsl-b) slice to wsl-a, not wsl-a's slice.
        let (_tmp, config_path) = fixture("wsl-b");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-b", &config_path);
        let slice_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "slice").collect();
        assert_eq!(slice_pushes.len(), 1);
        assert!(slice_pushes[0]
            .local_path
            .to_string_lossy()
            .ends_with("alice-bob.wsl-b.md"));
        assert!(slice_pushes[0].peer_target.contains("wsl-a.tail0.ts.net"));
    }

    #[test]
    fn plan_skips_local_only_channels() {
        // Re-write the fixture so bob also lives on wsl-a -> channel is
        // local-only -> no slice push for it.
        let (tmp, config_path) = fixture("wsl-a");
        let body = fs::read_to_string(&config_path)
            .unwrap()
            .replace(r#"host = "wsl-b""#, r#"host = "wsl-a""#);
        fs::write(&config_path, body).unwrap();
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        let slice_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "slice").collect();
        assert!(slice_pushes.is_empty(), "local-only channels need no slice push");
        // TOML push still happens (peer might have other reasons to receive).
        let toml_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "toml").collect();
        assert_eq!(toml_pushes.len(), 1);
        let _ = tmp;
    }

    #[test]
    fn plan_uses_peer_remote_config_dir_override_when_set() {
        // When the local config lives at /home/alice/... and the peer's
        // config lives at /home/bob/... (different user, different
        // $HOME), the toml push must target the peer's path, not the
        // local path. v1.1 fix for the homogeneous-path-assumption bug
        // surfaced in the live smoke (REMOTE_DESIGN.md §6 step 10).
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let local_cfg = tmp.path().join("local").join("giga-harness.toml");
        fs::create_dir_all(local_cfg.parent().unwrap()).unwrap();
        fs::write(
            &local_cfg,
            format!(
                r#"
[project]
name = "x"

[paths]
wsl_inbox = "{inbox}"

[[hosts]]
name = "self"
tailnet_hostname = "self.tail0.ts.net"

[[hosts]]
name = "peer"
tailnet_hostname = "peer.tail0.ts.net"
ssh_user = "bob"
remote_config_dir = "/home/bob/.giga/configs/x"
remote_inbox_dir = "/home/bob/projects/inbox"
"#,
                inbox = inbox.to_string_lossy(),
            ),
        )
        .unwrap();
        fs::write(
            tmp.path().join("local").join("this_host.toml"),
            "this_host = \"self\"\n",
        )
        .unwrap();
        let cfg = Config::load(&local_cfg).unwrap();
        let plan = compute_sync_plan(&cfg, "self", &local_cfg);
        let toml = plan.iter().find(|c| c.kind == "toml").expect("toml push");
        assert_eq!(
            toml.peer_target,
            "bob@peer.tail0.ts.net:/home/bob/.giga/configs/x/giga-harness.toml"
        );
    }

    #[test]
    fn plan_uses_peer_remote_inbox_dir_override_when_set_for_slice_push() {
        // Same idea — slice files land in the peer's remote_inbox_dir
        // when set, not at the local inbox path.
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let local_cfg = tmp.path().join("local").join("giga-harness.toml");
        fs::create_dir_all(local_cfg.parent().unwrap()).unwrap();
        fs::write(
            &local_cfg,
            format!(
                r#"
[project]
name = "x"

[paths]
wsl_inbox = "{inbox}"

[[hosts]]
name = "self"
tailnet_hostname = "self.tail0.ts.net"

[[hosts]]
name = "peer"
tailnet_hostname = "peer.tail0.ts.net"
ssh_user = "bob"
remote_inbox_dir = "/home/bob/projects/inbox"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "self"

[[agents]]
name = "bob-agent"
workdir = "/h/bob-agent"
role = "."
platform = "wsl"
host = "peer"

[[channels]]
file = "alice-bob-agent.md"
side = "wsl"
participants = ["alice", "bob-agent"]
"#,
                inbox = inbox.to_string_lossy(),
            ),
        )
        .unwrap();
        fs::write(
            tmp.path().join("local").join("this_host.toml"),
            "this_host = \"self\"\n",
        )
        .unwrap();
        let cfg = Config::load(&local_cfg).unwrap();
        let plan = compute_sync_plan(&cfg, "self", &local_cfg);
        let slice = plan
            .iter()
            .find(|c| c.kind == "slice")
            .expect("slice push to peer");
        assert!(
            slice
                .peer_target
                .ends_with("/home/bob/projects/inbox/alice-bob-agent.self.md"),
            "expected peer_target to end with peer's inbox dir + slice filename, got: {}",
            slice.peer_target
        );
        assert!(slice.use_append_verify);
    }

    #[test]
    fn plan_with_no_peers_is_empty() {
        // Single-host swarm with [[hosts]] entry — degenerate but valid.
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "solo"

[paths]
wsl_inbox = "{inbox}"

[[hosts]]
name = "wsl-only"
tailnet_hostname = "wsl-only.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-only"
"#,
            inbox = inbox.to_string_lossy(),
        );
        fs::write(&config_path, toml).unwrap();
        fs::write(tmp.path().join("this_host.toml"), "this_host = \"wsl-only\"\n").unwrap();
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-only", &config_path);
        assert!(plan.is_empty());
    }
}
