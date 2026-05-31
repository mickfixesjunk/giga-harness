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

use anyhow::Result;

use crate::config::Config;

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
