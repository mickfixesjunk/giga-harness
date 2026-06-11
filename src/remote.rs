//! `giga remote --host <host> <subcommand>` — SSH passthrough primitive.
//!
//! The operator UX lets the operator drive everything
//! from a single host. `giga remote` is the underlying primitive: it looks
//! up the named host in `[[hosts]]`, shells out to `ssh <user>@<tailnet_hostname>`,
//! invokes `giga <args>` on the remote side from the same canonical config
//! directory, and streams stdout/stderr/stdin back transparently while
//! propagating the remote's exit code.
//!
//! Higher-level subcommands (`giga add-agent --host B ...`, `giga sweep --host B`,
//! `giga launch --host B`) are sugar over this primitive — implemented in
//! step 6.
//!
//! Authentication: this just shells to plain `ssh`. With Tailscale SSH
//! enabled on the remote (via `tailscale set --ssh` per setup-remote-peer.sh),
//! the connection auths via tailnet identity automatically — no keypair or
//! authorized_keys file involved. If Tailscale SSH is off, it falls through
//! to the host's regular sshd (which needs key trust). Either way this
//! module doesn't care; the plumbing is handled below the ssh-binary line.
//!
//! Assumption (v1): the canonical config dir has the SAME absolute path on
//! every host. For homogeneous WSL/Linux setups (same OS user, same HOME)
//! this is reliably true; per-host overrides can come later if needed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::config::{Config, Host};

pub struct Args {
    pub host: String,
    pub remote_args: Vec<String>,
    pub config: PathBuf,
}

pub fn run(args: Args) -> Result<i32> {
    if args.remote_args.is_empty() {
        return Err(anyhow!(
            "missing subcommand. Usage: `giga remote --host <host> <subcommand> [args...]`"
        ));
    }
    let cfg = Config::load(&args.config)?;
    let transport = crate::transport::for_config(&cfg)?;
    if !transport.supports_remote_exec() {
        return Err(anyhow!(
            "transport `{}` doesn't support --host commands. \
             Run the giga command directly on the peer, or switch to a transport \
             that supports remote exec (e.g. rsync+tailscale).",
            transport.name()
        ));
    }
    transport.run_remote(&cfg, &args.host, &args.remote_args)
}

/// Shared SSH-passthrough implementation used by the
/// `RsyncTailscaleTransport::run_remote` plug adapter. Extracted so
/// the plug doesn't need to know about `Args` / clap shapes.
pub fn run_passthrough(cfg: &Config, peer: &str, args: &[String]) -> Result<i32> {
    let host = lookup_host(cfg, peer)?;
    let target = build_ssh_target(host)?;
    // Use the peer's remote_config_dir override if set; otherwise fall
    // back to whatever the local config's parent dir would be by
    // convention. The transport layer doesn't carry the operator's
    // current --config arg, so we look the swarm up in the registry to
    // find where the canonical config lives locally.
    let local_config_dir =
        registry_config_dir(&cfg.project.name).unwrap_or_else(|| std::path::PathBuf::from("."));
    let remote_dir = host.remote_config_dir.clone().unwrap_or(local_config_dir);
    let remote_cmd = build_remote_command(&remote_dir, args);
    let wrapped = format!(
        "bash -lc {}",
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(remote_cmd.as_str()))
    );
    let status = Command::new("ssh")
        .arg(&target)
        .arg(&wrapped)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking ssh to {target}"))?;
    Ok(status.code().unwrap_or(255))
}

fn registry_config_dir(project: &str) -> Option<std::path::PathBuf> {
    crate::registry::load()
        .ok()?
        .entries
        .into_iter()
        .find(|e| e.name == project)
        .and_then(|e| e.config.parent().map(|p| p.to_path_buf()))
}

/// Look up a host by name, returning a clear error listing the valid
/// options when the name doesn't match. Pure — testable without ssh.
fn lookup_host<'a>(cfg: &'a Config, name: &str) -> Result<&'a Host> {
    cfg.hosts.iter().find(|h| h.name == name).ok_or_else(|| {
        let known: Vec<&str> = cfg.hosts.iter().map(|h| h.name.as_str()).collect();
        if known.is_empty() {
            anyhow!(
                "no [[hosts]] declared in this swarm — `giga remote` requires at least one peer host. \
                 Add a [[hosts]] entry to your giga-harness.toml first."
            )
        } else {
            anyhow!(
                "unknown host `{name}` — expected one of: {}",
                known.join(", "),
            )
        }
    })
}

/// Build the SSH target string (`user@tailnet_hostname`). The user comes
/// from `host.ssh_user`, falling back to `$USER` for the common
/// homogeneous-user case. Pure — testable.
fn build_ssh_target(host: &Host) -> Result<String> {
    let user = host
        .ssh_user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok()) // Windows operator fallback
        .ok_or_else(|| {
            anyhow!(
                "can't determine SSH user for host `{}` (host has no ssh_user; $USER and $USERNAME both unset)",
                host.name
            )
        })?;
    Ok(format!("{user}@{}", host.tailnet_hostname))
}

/// Build the shell command to send over ssh: `cd <config-dir> && giga <args>`.
/// All values are shell-escaped to be safe against spaces or special chars
/// in paths or subcommand arguments. The config dir is normalized to
/// forward slashes (the peer is always Linux/WSL — `PathBuf::display()`
/// on a Windows operator would emit `\` which the remote shell rejects).
/// Pure — testable without ssh.
fn build_remote_command(config_dir: &Path, remote_args: &[String]) -> String {
    let escaped_args: Vec<String> = remote_args
        .iter()
        .map(|a| shell_escape::unix::escape(a.into()).into_owned())
        .collect();
    let dir_unix = config_dir.display().to_string().replace('\\', "/");
    let escaped_dir =
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(dir_unix.as_str())).into_owned();
    format!("cd {escaped_dir} && giga {}", escaped_args.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn build_ssh_target_uses_explicit_user_when_set() {
        let h = host("wsl-b", "wsl-b.tail0.ts.net", Some("alice"));
        assert_eq!(build_ssh_target(&h).unwrap(), "alice@wsl-b.tail0.ts.net");
    }

    #[test]
    fn build_ssh_target_falls_back_to_env_user() {
        // Save + restore $USER around this test so we don't break others.
        let orig = std::env::var("USER").ok();
        unsafe { std::env::set_var("USER", "from-env-user") };
        let h = host("wsl-b", "wsl-b.tail0.ts.net", None);
        let target = build_ssh_target(&h).unwrap();
        // Restore before any assert so failure doesn't leak the override.
        match orig {
            Some(v) => unsafe { std::env::set_var("USER", v) },
            None => unsafe { std::env::remove_var("USER") },
        }
        assert_eq!(target, "from-env-user@wsl-b.tail0.ts.net");
    }

    #[test]
    fn build_remote_command_quotes_paths_and_args() {
        let cmd = build_remote_command(
            Path::new("/home/alice/.giga/configs/remote-test"),
            &[
                "sweep".to_string(),
                "--owed-by".to_string(),
                "test-a".to_string(),
            ],
        );
        // Basic shape: cd <quoted-path> && giga sweep --owed-by test-a
        assert!(cmd.starts_with("cd "));
        assert!(cmd.contains("/home/alice/.giga/configs/remote-test"));
        assert!(cmd.contains(" && giga sweep --owed-by test-a"));
    }

    #[test]
    fn build_remote_command_escapes_args_with_spaces() {
        let cmd = build_remote_command(
            Path::new("/tmp/swarm"),
            &[
                "post".to_string(),
                "ch.md".to_string(),
                "--subject".to_string(),
                "subject with spaces".to_string(),
            ],
        );
        // The subject argument should be quoted so the remote shell
        // doesn't tokenize it into multiple args.
        assert!(
            cmd.contains("'subject with spaces'"),
            "expected single-quoted subject in: {cmd}",
        );
    }

    #[test]
    fn build_remote_command_handles_path_with_spaces() {
        let cmd = build_remote_command(
            Path::new("/home/alice/my swarms/test"),
            &["sweep".to_string()],
        );
        assert!(
            cmd.contains("'/home/alice/my swarms/test'"),
            "expected single-quoted path in: {cmd}",
        );
    }

    fn make_cfg_with_hosts(host_names: &[&str]) -> Config {
        let hosts_toml: String = host_names
            .iter()
            .map(|n| {
                format!("[[hosts]]\nname = \"{n}\"\ntailnet_hostname = \"{n}.tail0.ts.net\"\n")
            })
            .collect();
        let body =
            format!("[project]\nname = \"t\"\n[paths]\nwsl_inbox = \"/tmp/i\"\n{hosts_toml}");
        let mut cfg: Config = toml::from_str(&body).unwrap();
        // Pretend we are the first host so validate() passes.
        if let Some(first) = host_names.first() {
            cfg.this_host = Some((*first).into());
        }
        cfg.validate().unwrap();
        cfg
    }

    #[test]
    fn lookup_host_finds_known() {
        let cfg = make_cfg_with_hosts(&["wsl-a", "wsl-b"]);
        let h = lookup_host(&cfg, "wsl-b").unwrap();
        assert_eq!(h.name, "wsl-b");
    }

    #[test]
    fn lookup_host_unknown_lists_known_options() {
        let cfg = make_cfg_with_hosts(&["wsl-a", "wsl-b"]);
        let err = lookup_host(&cfg, "ghost").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ghost"));
        assert!(msg.contains("wsl-a"));
        assert!(msg.contains("wsl-b"));
    }

    #[test]
    fn lookup_host_with_empty_hosts_gives_setup_hint() {
        // Build a config with NO [[hosts]] — pre-remote-channels world.
        let body = "[project]\nname = \"t\"\n[paths]\nwsl_inbox = \"/tmp/i\"\n";
        let cfg: Config = toml::from_str(body).unwrap();
        cfg.validate().unwrap();
        let err = lookup_host(&cfg, "anyhost").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no [[hosts]] declared"));
        assert!(msg.contains("giga-harness.toml"));
    }
}
