//! `giga validate` — config sanity check, no side effects.

use std::path::Path;

use anyhow::Result;

use crate::config::Config;

pub fn run(path: &Path) -> Result<()> {
    let cfg = Config::load(path)?;
    println!("ok: {} ({}) — {} agents, {} channels",
        path.display(),
        cfg.project.name,
        cfg.agents.len(),
        cfg.channels.len(),
    );
    if let Some(bp) = &cfg.bench_protocol {
        println!("    bench scheduler: {} (slot pool: {})", bp.scheduler, bp.slot_pool);
    }
    for ch in &cfg.channels {
        let p = cfg.channel_path(ch)?;
        let status = if p.exists() { "exists" } else { "absent — `giga init` will create it" };
        println!("    [{}] {} ({})", ch.side, p.display(), status);
    }
    Ok(())
}
