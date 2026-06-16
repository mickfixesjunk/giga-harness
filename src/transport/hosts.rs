//! `giga hosts` — list the swarm's hosts + which agents live on each.
//!
//! Pure read: parses the canonical TOML + `this_host.toml`, prints a
//! tree. Useful for operator orientation ("which boxes am I dealing
//! with?") and for verifying after `giga add-host` / `add-agent --host`
//! that the swarm topology is what you intended.
//!
//! For all-local swarms (no `[[hosts]]`), prints a short notice +
//! lists the agents under a synthetic "(local)" header.

use std::path::Path;

use anyhow::{Context, Result};

use crate::foundation::tailscale::{self, TailnetNode};

use crate::config::Config;
use crate::registry;

/// `giga hosts` with no specific config to drill into — list all
/// registered swarms (one line each) instead of cryptically failing
/// with "no swarm registered for this directory." Operator can then
/// pass `--config <path>` (or the swarm name's config path) to get
/// detail.
pub fn run_list_all() -> Result<()> {
    let reg = registry::load()?;
    if reg.entries.is_empty() {
        println!("(no swarms registered yet — run `giga setup` to create one)");
        return Ok(());
    }
    println!("registered swarms ({}):", reg.entries.len());
    for entry in &reg.entries {
        println!();
        println!("  {}", entry.name);
        println!("    config:     {}", entry.config.display());
        if entry.code_roots.is_empty() {
            println!("    code_roots: (none)");
        } else {
            println!("    code_roots:");
            for r in &entry.code_roots {
                println!("      - {}", r.display());
            }
        }
        // Try to load + count agents/hosts/channels — best-effort, skip on parse fail.
        if let Ok(cfg) = Config::load(&entry.config) {
            let host_count = cfg.hosts.len();
            let agent_count = cfg.agents.len();
            let channel_count = cfg.channels.len();
            print!(
                "    summary:    {} agent(s), {} channel(s)",
                agent_count, channel_count
            );
            if host_count > 0 {
                print!(", {} host(s) (multi-host)", host_count);
            } else {
                print!(" (local-only)");
            }
            println!();
        }
    }
    println!();
    println!("for detail on one swarm: `giga hosts <config-path>`");
    Ok(())
}

pub fn run(config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    let this_host = cfg.this_host.as_deref();

    let mut out = String::new();
    out.push_str(&format!("swarm: {}", cfg.project.name));
    if let Some(th) = this_host {
        out.push_str(&format!(" (this_host: {th})"));
    }
    out.push('\n');

    if cfg.hosts.is_empty() {
        // Legacy local-only swarm — no [[hosts]]; list agents under a
        // synthetic header so the output shape is consistent.
        out.push_str("\n(local-only swarm — no [[hosts]] declared)\n");
        out.push_str("\nagents:\n");
        if cfg.agents.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for a in &cfg.agents {
                out.push_str(&format!(
                    "  - {} ({}, workdir: {})\n",
                    a.name,
                    a.platform,
                    a.workdir.display()
                ));
            }
        }
    } else {
        for h in &cfg.hosts {
            let is_this = this_host == Some(h.name.as_str());
            let ssh_user = h.ssh_user.as_deref().unwrap_or("(defaults to $USER)");
            out.push('\n');
            out.push_str(&format!("  {}", h.name));
            if is_this {
                out.push_str("   <-- this_host");
            }
            out.push('\n');
            out.push_str(&format!("    tailnet:  {}\n", h.tailnet_hostname));
            out.push_str(&format!("    ssh user: {ssh_user}\n"));
            if let Some(p) = &h.remote_config_dir {
                out.push_str(&format!("    config:   {}\n", p.display()));
            }
            if let Some(p) = &h.remote_inbox_dir {
                out.push_str(&format!("    inbox:    {}\n", p.display()));
            }
            // Agents on this host.
            let mine: Vec<&_> = cfg
                .agents
                .iter()
                .filter(|a| cfg.agent_host(a) == Some(h.name.as_str()))
                .collect();
            if mine.is_empty() {
                out.push_str("    agents:   (none yet)\n");
            } else {
                out.push_str("    agents:\n");
                for a in mine {
                    out.push_str(&format!("      - {} ({})\n", a.name, a.platform));
                }
            }
        }
    }

    // Channels summary at the bottom.
    out.push_str(&format!("\nchannels: {}\n", cfg.channels.len()));
    let cross_host = cfg
        .channels
        .iter()
        .filter(|c| !cfg.channel_is_local(c))
        .count();
    if !cfg.hosts.is_empty() {
        out.push_str(&format!(
            "  {} cross-host (slice-and-merge), {} local-only (fast-path)\n",
            cross_host,
            cfg.channels.len() - cross_host,
        ));
    }

    print!("{out}");
    Ok(())
}

/// `giga hosts --available`: shows registered swarm hosts + tailnet
/// members NOT yet registered (candidates for `giga add-host`).
/// Queries `tailscale status --json` for the roster.
pub fn run_available(config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    let registered: std::collections::HashSet<String> = cfg
        .hosts
        .iter()
        .map(|h| h.tailnet_hostname.trim_end_matches('.').to_string())
        .collect();

    println!("swarm: {}", cfg.project.name);
    if let Some(th) = &cfg.this_host {
        println!("this_host: {th}");
    }
    println!();
    println!("Registered hosts in swarm ({}):", cfg.hosts.len());
    if cfg.hosts.is_empty() {
        println!("  (none — single-host swarm; every tailnet member below is a candidate)");
    } else {
        for h in &cfg.hosts {
            println!("  {:<14} {}", h.name, h.tailnet_hostname);
        }
    }

    let roster = tailscale::roster().context(
        "couldn't query Tailscale — install the CLI or run from a WSL distro \
         on a host with Windows-side Tailscale",
    )?;
    let unregistered: Vec<&TailnetNode> = roster
        .iter()
        .filter(|n| !registered.contains(&n.dns_name))
        .collect();

    println!();
    if unregistered.is_empty() {
        println!(
            "All {} tailnet member(s) are registered in this swarm.",
            roster.len()
        );
    } else {
        println!(
            "Tailnet members NOT registered in this swarm ({}):",
            unregistered.len()
        );
        // Two columns: hostname + dns_name + OS.
        for n in &unregistered {
            println!("  {:<14} {:<42} {}", n.host_name, n.dns_name, n.os);
        }
        println!();
        println!(
            "To add one: giga add-host --name <slug> --tailnet-hostname <FQDN> --ssh-user <user>"
        );
    }

    Ok(())
}

// Tailnet roster parsing lives in foundation::tailscale (one parser,
// shared with setup_remote_node).
