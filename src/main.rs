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

mod add_agent;
mod add_channel;
mod config;
mod codex_channel;
mod cursor;
mod fs_paths;
mod init;
mod launch;
mod merger;
mod post;
mod registry;
mod remote;
mod setup;
mod setup_remote_node;
mod sweep;
mod sync;
mod switch;
mod templates;
mod terminal;
mod trust;
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
    /// One-command bootstrap: launches a Claude Code session that walks
    /// the user through scaffolding a multi-agent swarm. No external
    /// docs or paste-prompts required — everything's baked in.
    ///
    /// `--remote-node` instead bootstraps THIS machine as a remote peer
    /// in an EXISTING swarm: installs rsync + Tailscale, runs
    /// `tailscale up` (interactive), enables Tailscale SSH, creates the
    /// inbox dir. Run on a bare WSL host you want to add as a swarm
    /// member; then go to your operator host and
    /// `giga add-agent --host <this-node> ...`.
    Setup {
        /// Bootstrap THIS machine as a remote peer in an existing swarm
        /// (Tailscale + SSH + rsync + inbox dir). Implies that the
        /// operator-side scaffolding (giga init, giga setup as a fresh
        /// swarm, etc.) is NOT what you want.
        #[arg(long)]
        remote_node: bool,
        /// Override the default inbox directory (~/projects/inbox).
        /// Only used with --remote-node.
        #[arg(long, value_name = "PATH")]
        inbox_dir: Option<PathBuf>,
        /// Print what would happen without making changes. Only used
        /// with --remote-node.
        #[arg(long)]
        dry_run: bool,
    },
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
        /// Run launch on a remote host instead of locally. Equivalent to
        /// `giga remote --host <HOST> launch [args]`. Tailnet identity
        /// auths the connection.
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
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
        /// Which terminal multiplexer / launcher to use. `auto` (default)
        /// detects: wt.exe > tmux > print. Use `mac-terminal` on macOS to
        /// open one native Terminal.app window per agent. Other values:
        /// `tmux`, `wt`, `print`.
        #[arg(long, value_name = "MODE", default_value = "auto")]
        terminal: String,
    },
    /// Tabulate every channel's last message + WAITING ON tag.
    Sweep {
        #[arg(value_name = "CONFIG", default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Show only channels where `as` is the one being waited on.
        #[arg(long)]
        owed_by: Option<String>,
        /// Run sweep on a remote host instead of locally. Equivalent to
        /// `giga remote --host <HOST> sweep [args]`.
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
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
    /// Scaffold a new agent into the canonical config + write the
    /// template. Appends [[agents]] + per-peer [[channels]] blocks,
    /// adds the slug to any broadcast channel (`_*.md`), and writes
    /// `agents/<slug>.md`. Re-validates after.
    ///
    /// Designed to be runnable from any swarm agent's session — they
    /// can add new agents on the user's behalf without hand-editing
    /// TOML. Launch is a separate step the user owns.
    AddAgent {
        /// Agent slug (kebab-case). Becomes part of channel filenames
        /// and is what `--as <slug>` expects.
        #[arg(long, value_name = "SLUG")]
        name: String,
        /// Absolute workdir on the agent's target OS. Use the canonical
        /// author's path form (e.g. `/home/neo/...` or
        /// `C:\Users\Audio\...`); per-host localizers substitute.
        #[arg(long)]
        workdir: String,
        /// One-line role description; goes in `[[agents]] role = "..."`
        /// and into the generated template's header.
        #[arg(long)]
        role: String,
        /// `wsl` (default) or `windows`.
        #[arg(long, default_value = "wsl")]
        platform: String,
        /// Peer agent (repeatable). One bilateral [[channels]] block
        /// is appended per peer; alphabetical filename convention
        /// (e.g. `alice-charlie.md`). Side is auto-derived from peer
        /// platforms — windows if either side is windows-platform.
        #[arg(long, value_name = "AGENT")]
        peer: Vec<String>,
        /// Set this agent as the bench scheduler. Fails if another
        /// agent already holds the role.
        #[arg(long)]
        bench_scheduler: bool,
        /// Skip auto-appending the new slug to broadcast-channel
        /// participants (channels whose `file` starts with `_`).
        #[arg(long)]
        no_broadcast: bool,
        /// Use a custom CLAUDE.md template file instead of the
        /// generated minimal stub. The contents are written verbatim
        /// to `agents/<slug>.md`.
        #[arg(long, value_name = "PATH")]
        template: Option<PathBuf>,
        /// Don't write anything; print the planned changes and exit.
        #[arg(long)]
        dry_run: bool,
        /// The directory where this agent actually edits code, separate
        /// from --workdir (the launch context where CLAUDE.md lives).
        /// When set, giga injects it into the agent's CLAUDE.md and
        /// the launch intro prompt.
        #[arg(long, value_name = "PATH")]
        code_root: Option<String>,
        /// Host this agent lives on (must match a `[[hosts]].name`).
        /// Sets the agent's `host` field in the TOML so cross-host
        /// routing works. After scaffolding, run
        /// `giga launch --host <HOST> --only <NEW-AGENT>` to bring up
        /// the terminal on the peer.
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
        /// Config file to edit.
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Append a new bilateral channel between two existing agents.
    /// Updates the canonical giga-harness.toml; the `giga sync` daemon
    /// propagates the change to peers. The merger + watcher pick up
    /// the new channel within ~15s (auto-discovery reload window).
    AddChannel {
        /// Participant agent names, comma-separated. v1 supports
        /// bilateral channels only — exactly two participants.
        #[arg(long, value_delimiter = ',', value_name = "AGENT")]
        participants: Vec<String>,
        /// Override the auto-derived filename (sorted-alphabetical
        /// `<a>-<b>.md`). Rarely needed.
        #[arg(long)]
        file: Option<String>,
        /// Print the planned change without writing.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Manage which runtime account is active. Today only `--runtime claude`
    /// is supported. Credentials live in `~/.claude-accounts/<name>.json`
    /// snapshots; switching copies the chosen snapshot into
    /// `~/.claude/.credentials.json` (saving the previously-active one
    /// back first so any in-place token refreshes are preserved).
    ///
    /// Examples:
    ///   giga switch --runtime claude                  # show current + list
    ///   giga switch --runtime claude --setup primary  # one-time bootstrap
    ///   giga switch --runtime claude --add overflow   # provision empty slot
    ///   giga switch --runtime claude overflow         # switch to overflow
    Switch {
        /// Which agent runtime's credentials to manage. Only `claude` today.
        #[arg(long, value_name = "RUNTIME")]
        runtime: String,
        /// Account name. Required by --setup / --add and for a switch
        /// (positional). Omit with --list / no flags to see current state.
        account: Option<String>,
        /// List known accounts and exit.
        #[arg(long, conflicts_with_all = ["setup", "add"])]
        list: bool,
        /// One-time: adopt the existing ~/.claude/.credentials.json as
        /// a named snapshot.
        #[arg(long, conflicts_with_all = ["list", "add"])]
        setup: bool,
        /// Provision an empty credential slot. Populate by switching
        /// to it and running `claude` / going through /login.
        #[arg(long, conflicts_with_all = ["list", "setup"])]
        add: bool,
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
    /// Long-running merger daemon — for every cross-host channel,
    /// poll all <channel>.<host>.md slice files and append new bytes
    /// to <channel>.md (the file the watcher tails).
    ///
    /// Runs alongside `giga watch` + `giga sync` per host. No-op when
    /// the swarm has no [[hosts]] (today's local-only swarms).
    Merger {
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Run a single merge sweep and exit (useful in tests + scripted
        /// catch-up scenarios).
        #[arg(long)]
        once: bool,
    },
    /// Long-running sync daemon — every ~3s, rsync the canonical
    /// giga-harness.toml + own slice files to each peer host over
    /// Tailscale SSH (per REMOTE_DESIGN.md §4).
    ///
    /// Runs alongside `giga watch` + `giga merger` per host. No-op when
    /// the swarm has no [[hosts]] (today's local-only swarms).
    Sync {
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Run a single sync tick and exit (useful in scripts + tests).
        #[arg(long)]
        once: bool,
        /// Print the rsync commands that would be issued; don't execute.
        /// Combine with --once for a no-side-effects preview.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run a giga subcommand on a remote host over SSH. Looks up the
    /// host in `[[hosts]]`, shells to `ssh <user>@<tailnet_hostname>`,
    /// runs `giga <args>` from the same canonical config dir on that
    /// host, and propagates stdout/stderr/exit-code transparently.
    ///
    /// With Tailscale SSH enabled on the remote (per setup-remote-peer.sh),
    /// auth is automatic via tailnet identity — no key exchange.
    ///
    /// Example: `giga remote --host wsl-box-b sweep`
    Remote {
        /// Host name (must match a `[[hosts]].name` entry).
        #[arg(long, value_name = "HOST")]
        host: String,
        /// Local config file used to look up `[[hosts]]` + the canonical
        /// config dir to cd into on the remote.
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Subcommand + args to invoke on the remote host. Captured as
        /// trailing args so flags like `--owed-by` go to the remote
        /// subcommand, not to `giga remote` itself.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "ARGS")]
        remote_args: Vec<String>,
    },
    /// Forward giga inbox notifications into a running Codex filesystem channel.
    CodexChannel {
        /// Agent name to watch as.
        #[arg(long, value_name = "AGENT")]
        r#as: String,
        /// Codex channel directory used by the experimental source-built Codex.
        #[arg(long, value_name = "DIR")]
        channel_dir: PathBuf,
        /// Start from stored cursors (or byte 0) instead of current EOF.
        #[arg(long)]
        catch_up: bool,
        /// Skip broadcast channels such as `_broadcast.md`.
        #[arg(long)]
        direct_only: bool,
        /// Config file used to enumerate participating channels.
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup {
            remote_node,
            inbox_dir,
            dry_run,
        } => {
            if remote_node {
                setup_remote_node::run(setup_remote_node::Args {
                    inbox_dir,
                    dry_run,
                })
            } else {
                setup::run()
            }
        }
        Command::Validate { config } => {
            let config = registry::resolve_config(config)?;
            validate::run(&config)
        }
        Command::Init { config, no_trust } => init::run_with(&config, !no_trust),
        Command::Launch {
            config,
            host,
            skip_init,
            dry_run,
            only,
            new_window,
            terminal,
        } => {
            let config = registry::resolve_config(config)?;
            if let Some(host) = host {
                let mut remote_args = vec!["launch".to_string()];
                if skip_init {
                    remote_args.push("--skip-init".to_string());
                }
                if dry_run {
                    remote_args.push("--dry-run".to_string());
                }
                if !only.is_empty() {
                    remote_args.push("--only".to_string());
                    remote_args.push(only.join(","));
                }
                if new_window {
                    remote_args.push("--new-window".to_string());
                }
                remote_args.push("--terminal".to_string());
                remote_args.push(terminal);
                let code = remote::run(remote::Args {
                    host,
                    config,
                    remote_args,
                })?;
                std::process::exit(code);
            }
            launch::run(&config, skip_init, dry_run, &only, new_window, &terminal)
        }
        Command::Sweep {
            config,
            owed_by,
            host,
        } => {
            let config = registry::resolve_config(config)?;
            if let Some(host) = host {
                let mut remote_args = vec!["sweep".to_string()];
                if let Some(o) = &owed_by {
                    remote_args.push("--owed-by".to_string());
                    remote_args.push(o.clone());
                }
                let code = remote::run(remote::Args {
                    host,
                    config,
                    remote_args,
                })?;
                std::process::exit(code);
            }
            sweep::run(&config, owed_by.as_deref())
        }
        Command::Post {
            channel,
            r#as,
            subject,
            body,
            waiting_on,
            needs,
            config,
        } => {
            let config = registry::resolve_config(config)?;
            post::run(post::Args {
                channel,
                me: r#as,
                subject,
                body,
                waiting_on,
                needs,
                config,
            })
        }
        Command::AddAgent {
            name,
            workdir,
            role,
            platform,
            peer,
            bench_scheduler,
            no_broadcast,
            template,
            dry_run,
            code_root,
            host,
            config,
        } => add_agent::run(add_agent::Args {
            config,
            name,
            workdir,
            role,
            platform,
            peers: peer,
            bench_scheduler,
            no_broadcast,
            template,
            dry_run,
            code_root,
            host,
        }),
        Command::AddChannel {
            participants,
            file,
            dry_run,
            config,
        } => {
            let config = registry::resolve_config(config)?;
            add_channel::run(add_channel::Args {
                config,
                participants,
                file,
                dry_run,
            })
        }
        Command::Switch {
            runtime,
            account,
            list,
            setup,
            add,
        } => {
            let op = if setup {
                switch::Op::Setup
            } else if add {
                switch::Op::Add
            } else if list {
                switch::Op::List
            } else if account.is_some() {
                switch::Op::Switch
            } else {
                switch::Op::Status
            };
            switch::run(switch::Args {
                runtime,
                account,
                op,
            })
        }
        Command::Watch {
            channel,
            r#as,
            config,
        } => {
            let config = registry::resolve_config(config)?;
            match channel {
                Some(c) => {
                    let path = resolve_channel(&c, &config)?;
                    watch::run_single(&path, &r#as)
                }
                None => watch::run_multi(&config, &r#as),
            }
        }
        Command::Merger { config, once } => {
            let config = registry::resolve_config(config)?;
            merger::run(&config, once)
        }
        Command::Sync {
            config,
            once,
            dry_run,
        } => {
            let config = registry::resolve_config(config)?;
            sync::run(sync::Args {
                config,
                once,
                dry_run,
            })
        }
        Command::Remote {
            host,
            config,
            remote_args,
        } => {
            let config = registry::resolve_config(config)?;
            let code = remote::run(remote::Args {
                host,
                config,
                remote_args,
            })?;
            std::process::exit(code);
        }
        Command::CodexChannel {
            r#as,
            channel_dir,
            catch_up,
            direct_only,
            config,
        } => {
            let config = registry::resolve_config(config)?;
            codex_channel::run(codex_channel::Args {
                me: r#as,
                channel_dir,
                config,
                catch_up,
                direct_only,
            })
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
    if !config.exists() {
        return Err(anyhow::anyhow!(
            "no config file at {} — pass --config <path>, or place a giga-harness.toml in this directory (a workdir symlink to the project config is the usual fix)",
            config.display(),
        ));
    }
    let cfg = config::Config::load(config)?;
    // Accept bare names without `.md` — channel files in config always
    // carry the suffix, but users (and agents) commonly drop it.
    let with_md = if channel.ends_with(".md") {
        None
    } else {
        Some(format!("{channel}.md"))
    };
    if let Some(ch) = cfg
        .channels
        .iter()
        .find(|c| c.file == channel || with_md.as_deref().map(|m| c.file == m).unwrap_or(false))
    {
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
