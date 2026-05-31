//! `giga add-host` — append a `[[hosts]]` entry to the canonical TOML
//! and (by default) auto-bootstrap the new peer.
//!
//! Per REMOTE_DESIGN.md: a host's `[[hosts]]` entry tells the rest of
//! the swarm how to reach it (tailnet hostname, SSH user, where its
//! filesystem stores the config + inbox). Without this entry, you
//! can't `giga add-agent --host <host>` or `giga remote --host <host>`
//! against the peer. v1 of remote-channels asked the user to edit
//! `[[hosts]]` by hand; this subcommand wraps that edit + the
//! one-time peer bootstrap (push TOML + ensure this_host.toml) in
//! one command.
//!
//! Typical flow:
//!
//!   # On peer first: bootstrap the host itself (Tailscale, rsync, etc.)
//!   giga setup --remote-node
//!
//!   # On operator: register the peer in the swarm + push the TOML to it
//!   giga add-host --name wsl-b \
//!                 --tailnet-hostname wsl-b.tail0.ts.net \
//!                 --ssh-user neo \
//!                 --remote-config-dir /home/neo/.giga/configs/<swarm>
//!
//!   # Then add agents on it:
//!   giga add-agent --host wsl-b --name foo --peer bar
//!
//! Refuses to add a duplicate-named host; use `giga validate` or edit
//! the TOML manually if you need to update an existing entry.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use toml_edit::{value, DocumentMut, Table};

use crate::add_agent::ensure_array_of_tables;
use crate::config::Config;
use crate::sync;

pub struct Args {
    pub config: PathBuf,
    pub name: String,
    pub tailnet_hostname: String,
    pub ssh_user: Option<String>,
    pub remote_config_dir: Option<PathBuf>,
    pub remote_inbox_dir: Option<PathBuf>,
    /// Skip the auto-push of the canonical TOML to the new peer
    /// (the part that needs the peer to already be reachable over SSH).
    /// Use when you're adding a host in advance of bringing it online.
    pub no_bootstrap: bool,
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;

    if cfg.hosts.iter().any(|h| h.name == args.name) {
        return Err(anyhow!(
            "host `{}` already exists in [[hosts]] — pick a different --name, \
             or remove the existing entry manually if you mean to replace it",
            args.name,
        ));
    }

    if args.dry_run {
        println!("dry-run: would add host");
        println!("  name:                {}", args.name);
        println!("  tailnet_hostname:    {}", args.tailnet_hostname);
        if let Some(u) = &args.ssh_user {
            println!("  ssh_user:            {u}");
        }
        if let Some(p) = &args.remote_config_dir {
            println!("  remote_config_dir:   {}", p.display());
        }
        if let Some(p) = &args.remote_inbox_dir {
            println!("  remote_inbox_dir:    {}", p.display());
        }
        if !args.no_bootstrap {
            println!("  + would also bootstrap (mkdir + rsync TOML + ensure this_host.toml on peer)");
        }
        return Ok(());
    }

    // Edit TOML preserving comments + formatting.
    let original = fs::read_to_string(&args.config)
        .with_context(|| format!("reading {}", args.config.display()))?;
    let mut doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parsing {} as TOML", args.config.display()))?;
    append_host(&mut doc, &args)?;
    fs::write(&args.config, doc.to_string())
        .with_context(|| format!("writing {}", args.config.display()))?;

    // Reload + revalidate. Catches "host name not in [[hosts]]" type
    // breakage if something went wrong in the toml_edit serialization.
    let revalidated = Config::load(&args.config).with_context(|| {
        format!(
            "added host `{}` but post-edit validation failed — config is in an unexpected state",
            args.name
        )
    })?;

    println!("added host `{}` to {}", args.name, args.config.display());
    println!("  tailnet_hostname = {}", args.tailnet_hostname);
    if let Some(u) = &args.ssh_user {
        println!("  ssh_user = {u}");
    }

    // Auto-bootstrap unless opted out.
    if args.no_bootstrap {
        println!();
        println!("(--no-bootstrap: skipping peer push; run `giga sync --once` later or use add-agent --host {} to trigger it)", args.name);
    } else {
        println!();
        println!("bootstrapping `{}` (mkdir + rsync TOML + ensure this_host.toml)...", args.name);
        match sync::bootstrap_peer(&revalidated, &args.name, &args.config) {
            Ok(()) => println!("  + bootstrap complete"),
            Err(e) => {
                eprintln!("  ! bootstrap failed: {e:#}");
                eprintln!("    The local TOML edit is correct; the peer just isn't synced yet.");
                eprintln!("    Re-run `giga sync --once` once the peer is reachable to recover.");
            }
        }
    }

    println!();
    println!("next:");
    println!(
        "  giga add-agent --host {} --name <slug> --peer <local-agent> --role \"...\"",
        args.name
    );
    Ok(())
}

fn append_host(doc: &mut DocumentMut, args: &Args) -> Result<()> {
    let hosts = ensure_array_of_tables(doc, "hosts")?;
    let mut block = Table::new();
    block["name"] = value(args.name.as_str());
    block["tailnet_hostname"] = value(args.tailnet_hostname.as_str());
    if let Some(u) = &args.ssh_user {
        block["ssh_user"] = value(u.as_str());
    }
    if let Some(p) = &args.remote_config_dir {
        block["remote_config_dir"] = value(p.to_string_lossy().into_owned());
    }
    if let Some(p) = &args.remote_inbox_dir {
        block["remote_inbox_dir"] = value(p.to_string_lossy().into_owned());
    }
    hosts.push(block);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_cfg(tmp: &TempDir, body: &str) -> PathBuf {
        let p = tmp.path().join("giga-harness.toml");
        fs::write(&p, body).unwrap();
        p
    }

    fn base_cfg() -> &'static str {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "host-a"
"#
    }

    fn this_host_a(tmp: &TempDir) {
        fs::write(tmp.path().join("this_host.toml"), "this_host = \"host-a\"\n").unwrap();
    }

    fn args(config: PathBuf, name: &str) -> Args {
        Args {
            config,
            name: name.into(),
            tailnet_hostname: format!("{name}.tail0.ts.net"),
            ssh_user: None,
            remote_config_dir: None,
            remote_inbox_dir: None,
            no_bootstrap: true,
            dry_run: false,
        }
    }

    #[test]
    fn appends_new_host_to_toml() {
        let tmp = TempDir::new().unwrap();
        let p = write_cfg(&tmp, base_cfg());
        this_host_a(&tmp);
        run(args(p.clone(), "host-b")).unwrap();
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.hosts.len(), 2);
        assert!(cfg.hosts.iter().any(|h| h.name == "host-b"));
    }

    #[test]
    fn refuses_duplicate_name() {
        let tmp = TempDir::new().unwrap();
        let p = write_cfg(&tmp, base_cfg());
        this_host_a(&tmp);
        let err = run(args(p, "host-a")).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn dry_run_does_not_modify_file() {
        let tmp = TempDir::new().unwrap();
        let p = write_cfg(&tmp, base_cfg());
        this_host_a(&tmp);
        let before = fs::read_to_string(&p).unwrap();
        let mut a = args(p.clone(), "host-b");
        a.dry_run = true;
        run(a).unwrap();
        let after = fs::read_to_string(&p).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn writes_optional_fields_when_set() {
        let tmp = TempDir::new().unwrap();
        let p = write_cfg(&tmp, base_cfg());
        this_host_a(&tmp);
        let mut a = args(p.clone(), "host-b");
        a.ssh_user = Some("bob".into());
        a.remote_config_dir = Some(PathBuf::from("/home/bob/.giga/configs/t"));
        a.remote_inbox_dir = Some(PathBuf::from("/home/bob/inbox"));
        run(a).unwrap();
        let cfg = Config::load(&p).unwrap();
        let b = cfg.hosts.iter().find(|h| h.name == "host-b").unwrap();
        assert_eq!(b.ssh_user.as_deref(), Some("bob"));
        assert_eq!(
            b.remote_config_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
            Some("/home/bob/.giga/configs/t".into())
        );
        assert_eq!(
            b.remote_inbox_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
            Some("/home/bob/inbox".into())
        );
    }

    #[test]
    fn omits_optional_fields_when_unset() {
        let tmp = TempDir::new().unwrap();
        let p = write_cfg(&tmp, base_cfg());
        this_host_a(&tmp);
        run(args(p.clone(), "host-b")).unwrap();
        let body = fs::read_to_string(&p).unwrap();
        // The new [[hosts]] block should NOT carry empty ssh_user / etc.
        // We rely on the absence of "ssh_user" in the host-b block.
        let host_b_start = body.find(r#"name = "host-b""#).unwrap();
        let after = &body[host_b_start..];
        assert!(
            !after[..200.min(after.len())].contains("ssh_user"),
            "host-b block should NOT have ssh_user when args.ssh_user was None: {after}"
        );
    }
}
