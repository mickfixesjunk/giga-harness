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
mod trust;
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
        /// Skip pre-populating Claude Code's per-folder trust state.
        /// By default giga marks every agent workdir as trusted so
        /// claude doesn't prompt on first launch.
        #[arg(long)]
        no_trust: bool,
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
        /// Spawn only the named agents (comma-separated, or repeat the
        /// flag). New tabs join the existing wt window / tmux session
        /// instead of replacing it — use this to add a freshly-defined
        /// agent without disturbing tabs that are already running.
        #[arg(long, value_delimiter = ',', value_name = "AGENT")]
        only: Vec<String>,
        /// Force each new tab into its own fresh wt window (uses
        /// `wt -w new` instead of targeting the project's named window).
        /// Use when the original launch window no longer exists in its
        /// original form — e.g. you've torn agent tabs out into separate
        /// windows you've arranged on screen. tmux has no equivalent.
        #[arg(long)]
        new_window: bool,
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
    ///
    /// Two modes:
    ///   * With <CHANNEL>: legacy single-file watch.
    ///   * Without <CHANNEL>: config-aware multi-channel watch — tracks
    ///     every channel where `--as` is a participant and rereads the
    ///     config periodically so newly-added channels get picked up
    ///     without restarting the watcher.
    Watch {
        /// Channel path (absolute) or bare filename to resolve via config.
        /// If omitted, watches every channel where `--as` participates.
        channel: Option<String>,
        /// Your agent name (own messages are filtered out).
        #[arg(long, value_name = "AGENT")]
        r#as: String,
        /// Config file used to resolve a bare channel filename, or
        /// (in multi-channel mode) to enumerate participating channels.
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { config } => validate::run(&config),
        Command::Init { config, no_trust } => init::run_with(&config, !no_trust),
        Command::Launch { config, skip_init, dry_run, only, new_window } => {
            launch::run(&config, skip_init, dry_run, &only, new_window)
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
        Command::Watch { channel, r#as, config } => match channel {
            Some(c) => {
                let path = resolve_channel(&c, &config)?;
                watch::run_single(&path, &r#as)
            }
            None => watch::run_multi(&config, &r#as),
        },
    }
}

/// Resolve a channel argument that may be either an absolute path or
/// a bare filename matching a [[channels]] entry in the config.
fn resolve_channel(channel: &str, config: &std::path::Path) -> Result<PathBuf> {
    let as_path = PathBuf::from(channel);
    if as_path.is_absolute() && as_path.exists() {
        return Ok(as_path);
    }
    if !config.exists() {
        return Err(anyhow::anyhow!(
            "no config file at {} — pass --config <path>, or place a giga-harness.toml in this directory (a workdir symlink to the project config is the usual fix)",
            config.display(),
        ));
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
        "channel `{channel}` not listed in {} and not a valid path",
        config.display(),
    ))
}
