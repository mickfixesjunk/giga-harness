//! `giga post` — append a properly-formatted message to an inbox channel.
//!
//! Enforces the convention so agents can't accidentally drop the
//! header block or forget the WAITING ON / informational tag.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::config::Config;

pub struct Args {
    pub channel: String,
    pub me: String,
    pub subject: String,
    pub body: Option<String>,
    pub waiting_on: Option<String>,
    pub needs: Option<String>,
    pub config: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let cfg_opt = Config::load(&args.config).ok();

    let path = resolve(&args.channel, cfg_opt.as_ref(), &args.config)?;

    // Validate sender is a participant on this channel (when we have a config).
    if let Some(cfg) = &cfg_opt {
        if let Some(ch) = cfg.channels.iter().find(|c| c.file == args.channel || cfg.channel_path(c).map(|p| p == path).unwrap_or(false)) {
            if !ch.participants.contains(&args.me) {
                return Err(anyhow!(
                    "`{}` is not a participant of channel `{}` (participants: {:?})",
                    args.me, ch.file, ch.participants
                ));
            }
            if let Some(target) = &args.waiting_on {
                if !ch.participants.contains(target) {
                    return Err(anyhow!(
                        "WAITING ON target `{}` is not a participant of channel `{}`",
                        target, ch.file
                    ));
                }
            }
        }
    }

    let body = match args.body {
        Some(b) => b,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).context("reading body from stdin")?;
            s
        }
    };

    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let footer = match (&args.waiting_on, &args.needs) {
        (Some(who), Some(needs)) => format!("WAITING ON: {who} ({needs})"),
        (Some(who), None) => format!("WAITING ON: {who}"),
        (None, _) => "(Informational, no response required.)".to_string(),
    };

    let block = format!(
        "\n\n===\n[{}] {} — {}\n===\n\n{}\n\n{}\n===\n",
        args.me, args.subject, ts, body.trim_end(), footer,
    );

    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    f.write_all(block.as_bytes())
        .with_context(|| format!("writing to {}", path.display()))?;

    println!("posted to {} ({} bytes)", path.display(), block.len());
    Ok(())
}

fn resolve(channel: &str, cfg: Option<&Config>, config_path: &Path) -> Result<PathBuf> {
    let as_path = Path::new(channel);
    if as_path.is_absolute() && as_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return Ok(as_path.to_path_buf());
    }
    if let Some(cfg) = cfg {
        if let Some(ch) = cfg.channels.iter().find(|c| c.file == channel) {
            return cfg.channel_path(ch);
        }
    }
    if as_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return Ok(as_path.to_path_buf());
    }
    if !config_path.exists() {
        return Err(anyhow!(
            "no config file at {} — pass --config <path>, or place a giga-harness.toml in this directory (a workdir symlink to the project config is the usual fix)",
            config_path.display(),
        ));
    }
    Err(anyhow!(
        "channel `{channel}` not listed in {} and not a valid path",
        config_path.display(),
    ))
}
