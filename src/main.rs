//! giga-harness — manual multi-agent coordination harness.
//!
//! See README.md for the design. Subcommands:
//!
//!   giga validate <config>        — schema + cross-check, no side effects
//!   giga init     <config>        — scaffold inbox files + per-agent AGENTS.md
//!   giga launch   <config>        — spawn one terminal per agent
//!   giga sweep    <config>        — show channel state (who owes whom)
//!   giga post     <channel> ...   — append a properly-formatted message
//!   giga watch    <channel> ...   — long-running inbox watcher

use anyhow::Result;
use clap::Parser;

mod accounts;
mod claude_operator;
mod cli;
mod config;
mod coordination;
mod dispatch;
mod foundation;
mod fs_paths;
mod mobility;
mod mutate;
mod registry;
mod runtime;
mod scaffold;
mod setup;
mod transport;
mod trust;
mod ui;
mod validate;

fn main() -> Result<()> {
    cli::Cli::parse().command.run()
}
