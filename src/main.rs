//! giga-harness — manual multi-agent coordination harness.
//!
//! See README.md for the design. Subcommands:
//!
//!   giga validate <config>        — schema + cross-check, no side effects
//!   giga init     <config>        — scaffold inbox files + per-agent CLAUDE.md
//!   giga launch   <config>        — spawn one terminal per agent
//!   giga sweep    <config>        — show channel state (who owes whom)
//!   giga post     <channel> ...   — append a properly-formatted message
//!   giga watch    <channel> ...   — long-running inbox watcher

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod config;
mod fs_paths;
mod init;
mod launch;
mod post;
mod sweep;
mod terminal;
mod validate;
mod watch;

#[derive(Parser)]
#[command(
    name = "giga",
    version,
    about = "Manual multi-agent coordination harness",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a config file without touching the filesystem.
    Validate {
        #[arg(value_name = "CONFIG", default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Create inbox files and per-agent CLAUDE.md from a config.
    Init {
        #[arg(value_name = "CONFIG", default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Spawn one terminal per agent (Windows Terminal or tmux).
    Launch {
        #[arg(value_name = "CONFIG", default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Skip `giga init` before launching. Use if you've already
        /// scaffolded and don't want to re-render CLAUDE.md files.
        #[arg(long)]
        skip_init: bool,
        /// Print the launch plan instead of executing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Tabulate every channel's last message + WAITING ON tag.
    Sweep {
        #[arg(value_name = "CONFIG", default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Show only channels where `as` is the one being waited on.
        #[arg(long)]
        owed_by: Option<String>,
    },
    /// Append a properly-formatted message to a channel file.
    Post {
        /// Channel filename (must match a [[channels]] entry) OR an absolute path.
        channel: String,
        /// Your agent name — must match one of the channel's participants.
        #[arg(long, value_name = "AGENT")]
        r#as: String,
        /// Short subject line for the header block.
        #[arg(long)]
        subject: String,
        /// Body — if absent, read from stdin until EOF.
        #[arg(long)]
        body: Option<String>,
        /// Tag the message as waiting on this agent (omit for informational).
        #[arg(long, value_name = "AGENT")]
        waiting_on: Option<String>,
        /// Optional "what's needed" hint for the WAITING ON tag.
        #[arg(long)]
        needs: Option<String>,
        /// Config file — used to resolve a bare channel filename to its absolute path.
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Long-running watcher — emits one stdout line per new message.
    Watch {
        /// Channel path (absolute) or bare filename to resolve via config.
        channel: String,
        /// Your agent name (own messages are filtered out).
        #[arg(long, value_name = "AGENT")]
        r#as: String,
        /// Config file used to resolve a bare channel filename.
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { config } => validate::run(&config),
        Command::Init { config } => init::run(&config),
        Command::Launch { config, skip_init, dry_run } => {
            launch::run(&config, skip_init, dry_run)
        }
        Command::Sweep { config, owed_by } => sweep::run(&config, owed_by.as_deref()),
        Command::Post {
            channel,
            r#as,
            subject,
            body,
            waiting_on,
            needs,
            config,
        } => post::run(post::Args {
            channel,
            me: r#as,
            subject,
            body,
            waiting_on,
            needs,
            config,
        }),
        Command::Watch { channel, r#as, config } => {
            let path = resolve_channel(&channel, &config)?;
            watch::run(&path, &r#as)
        }
    }
}

/// Resolve a channel argument that may be either an absolute path or
/// a bare filename matching a [[channels]] entry in the config.
fn resolve_channel(channel: &str, config: &std::path::Path) -> Result<PathBuf> {
    let as_path = PathBuf::from(channel);
    if as_path.is_absolute() && as_path.exists() {
        return Ok(as_path);
    }
    let cfg = config::Config::load(config)?;
    if let Some(ch) = cfg.channels.iter().find(|c| c.file == channel) {
        return cfg.channel_path(ch);
    }
    // Fallback: if user passed a relative path that exists, use it.
    if as_path.exists() {
        return Ok(as_path);
    }
    Err(anyhow::anyhow!(
        "can't resolve channel `{channel}` — not an absolute path and not in {config:?}"
    ))
}
