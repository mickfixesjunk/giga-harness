//! Command dispatch — maps each parsed `Command` variant to its
//! implementation. Every arm's logic is identical to the historical
//! `main()` match: the same `registry::resolve_config` calls, the same
//! `--host` remote passthrough for Launch/Sweep, the same channel
//! resolution, and the same runtime parse for takeover.

use std::path::PathBuf;

use anyhow::Result;

use crate::cli::Command;
use crate::{
    accounts, claude_operator, config, coordination, mobility, mutate, registry, runtime, scaffold,
    setup, transport, ui, validate,
};

impl Command {
    pub fn run(self) -> Result<()> {
        match self {
            Command::Setup {
                remote_node,
                inbox_dir,
                transport,
                repo,
                dry_run,
            } => {
                if remote_node {
                    crate::transport::setup_remote_node::run(
                        crate::transport::setup_remote_node::Args {
                            inbox_dir,
                            dry_run,
                            transport,
                            repo,
                        },
                    )
                } else {
                    setup::run()
                }
            }
            Command::Validate { config } => {
                let config = registry::resolve_config(config)?;
                validate::run(&config)
            }
            Command::Init { config, no_trust } => scaffold::init::run_with(&config, !no_trust),
            Command::Launch {
                config,
                host,
                skip_init,
                dry_run,
                only,
                new_window,
                terminal,
                stagger_per_agent_seconds,
                ui,
                ui_port,
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
                    if stagger_per_agent_seconds > 0 {
                        remote_args.push("--stagger-per-agent-seconds".to_string());
                        remote_args.push(stagger_per_agent_seconds.to_string());
                    }
                    if ui {
                        remote_args.push("--ui".to_string());
                        remote_args.push("--ui-port".to_string());
                        remote_args.push(ui_port.to_string());
                    }
                    let code = transport::remote::run(transport::remote::Args {
                        host,
                        config,
                        remote_args,
                    })?;
                    std::process::exit(code);
                }
                scaffold::launch::run(scaffold::launch::LaunchArgs {
                    config_path: &config,
                    skip_init,
                    dry_run,
                    only: &only,
                    new_window,
                    terminal: &terminal,
                    stagger_per_agent_seconds,
                    ui,
                    ui_port,
                })
            }
            Command::Hosts { config, available } => {
                // When the user didn't override --config and we can't resolve
                // a default `giga-harness.toml` (not in a swarm dir, no
                // registry hit), fall back to listing every registered swarm
                // instead of erroring with the cryptic "no swarm registered"
                // message. Explicit-but-bad --config still errors loud. The
                // list-all fallback only applies without --available, so the
                // available case keeps surfacing the resolution error.
                match registry::resolve_config_or(config) {
                    registry::Resolved::Found(c) if available => {
                        transport::hosts::run_available(&c)
                    }
                    registry::Resolved::Found(c) => transport::hosts::run(&c),
                    registry::Resolved::DefaultMissing(_) if !available => {
                        transport::hosts::run_list_all()
                    }
                    // --available + unresolvable default: surface the same
                    // loud error the inline logic produced (Err(e) arm).
                    registry::Resolved::DefaultMissing(e) => Err(e),
                    registry::Resolved::ExplicitError(e) => Err(e),
                }
            }
            Command::ClaudeOperator => claude_operator::run(),
            Command::Upgrade {
                config,
                r#as,
                skip_peers,
                skip_broadcast,
                skip_windows,
                dry_run,
                bare,
            } => {
                // v0.6.41: explicit --bare flag skips all swarm-aware
                // machinery (the UI's upgrade button uses this).
                if bare {
                    return mobility::upgrade::run_bare(dry_run);
                }
                // v0.6.30: `giga upgrade` should work CWD-independently —
                // the binary install is system-level, not per-swarm. If
                // config resolution fails (CWD is not under any registered
                // swarm and no explicit --config was passed), fall through
                // to a bare install rather than erroring out. The disarm/
                // rearm dance is only meaningful when a swarm is in scope.
                // Any resolution failure (default or explicit) falls back to
                // bare — preserving the historical `Err(_) => run_bare` arm.
                match registry::resolve_config_or(config) {
                    registry::Resolved::Found(config) => {
                        mobility::upgrade::run(mobility::upgrade::Args {
                            config,
                            as_agent: r#as,
                            skip_peers,
                            skip_broadcast,
                            skip_windows,
                            dry_run,
                        })
                    }
                    registry::Resolved::DefaultMissing(_)
                    | registry::Resolved::ExplicitError(_) => mobility::upgrade::run_bare(dry_run),
                }
            }
            Command::Ui { bind, port } => ui::run(ui::Args { bind, port }),
            Command::Teleport {
                agent,
                to,
                from,
                keep_running,
                dry_run,
                config,
            } => {
                let config = registry::resolve_config(config)?;
                mobility::teleport::run(mobility::teleport::Args {
                    agent,
                    to,
                    from,
                    keep_running,
                    dry_run,
                    config,
                })
            }
            Command::Takeover {
                as_agent,
                to,
                dry_run,
                config,
            } => {
                let config = registry::resolve_config(config)?;
                let to_runtime = runtime::Runtime::parse(&to).ok_or_else(|| {
                    anyhow::anyhow!("unknown --to runtime `{to}` — valid: claude, codex, agy")
                })?;
                mobility::takeover::run(mobility::takeover::Args {
                    config,
                    as_agent,
                    to_runtime,
                    dry_run,
                })
            }
            Command::SetSwarmBoss {
                slug,
                unset,
                no_init,
                config,
            } => {
                let config = registry::resolve_config(config)?;
                mutate::set_swarm_boss::run(mutate::set_swarm_boss::Args {
                    config,
                    slug,
                    unset,
                    no_init,
                })
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
                    let code = transport::remote::run(transport::remote::Args {
                        host,
                        config,
                        remote_args,
                    })?;
                    std::process::exit(code);
                }
                coordination::sweep::run(&config, owed_by.as_deref())
            }
            Command::Post {
                channel,
                channel_flag,
                r#as,
                subject,
                body,
                waiting_on,
                needs,
                config,
                to,
                fyi,
            } => {
                // v0.3.7 Bug 8: resolve channel from positional or --channel flag.
                let channel = match (channel, channel_flag) {
                    (Some(c), None) | (None, Some(c)) => c,
                    (Some(_), Some(_)) => {
                        return Err(anyhow::anyhow!(
                            "channel passed both positionally and via --channel — pick one"
                        ));
                    }
                    (None, None) => {
                        return Err(anyhow::anyhow!(
                            "channel is required — pass it positionally or as --channel <NAME>"
                        ));
                    }
                };
                let config = registry::resolve_config(config)?;
                coordination::post::run(coordination::post::Args {
                    channel,
                    me: r#as,
                    subject,
                    body,
                    waiting_on,
                    needs,
                    config,
                    to,
                    fyi,
                })
            }
            Command::AddAgent {
                name,
                workdir,
                role,
                platform,
                peer,
                bench_scheduler,
                swarm_boss,
                no_broadcast,
                template,
                dry_run,
                code_root,
                host,
                config,
            } => mutate::add_agent::run(mutate::add_agent::Args {
                config,
                name,
                workdir,
                role,
                platform,
                peers: peer,
                bench_scheduler,
                swarm_boss,
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
                mutate::add_channel::run(mutate::add_channel::Args {
                    config,
                    participants,
                    file,
                    dry_run,
                })
            }
            Command::AddHost {
                name,
                tailnet_hostname,
                ssh_user,
                remote_config_dir,
                remote_inbox_dir,
                no_bootstrap,
                dry_run,
                this_host_name,
                config,
            } => {
                let config = registry::resolve_config(config)?;
                mutate::add_host::run(mutate::add_host::Args {
                    config,
                    name,
                    tailnet_hostname,
                    ssh_user,
                    remote_config_dir,
                    remote_inbox_dir,
                    no_bootstrap,
                    dry_run,
                    this_host_name,
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
                    accounts::switch::Op::Setup
                } else if add {
                    accounts::switch::Op::Add
                } else if list {
                    accounts::switch::Op::List
                } else if account.is_some() {
                    accounts::switch::Op::Switch
                } else {
                    accounts::switch::Op::Status
                };
                accounts::switch::run(accounts::switch::Args {
                    runtime,
                    account,
                    op,
                })
            }
            Command::Watch {
                channel,
                r#as,
                config,
                stagger_seconds,
                no_stagger,
                agy,
                codex,
            } => {
                let config = registry::resolve_config(config)?;
                let stagger_override = if no_stagger { Some(0) } else { stagger_seconds };
                // v0.6.0: derive watch mode. clap's conflicts_with enforces
                // --agy and --codex are mutually exclusive; default is Claude.
                let mode = if agy {
                    coordination::watch::WatchMode::Agy
                } else if codex {
                    coordination::watch::WatchMode::Codex
                } else {
                    coordination::watch::WatchMode::Default
                };
                match channel {
                    Some(c) => {
                        let path = resolve_channel(&c, &config)?;
                        coordination::watch::run_single(&path, &r#as, mode)
                    }
                    None => coordination::watch::run_multi(&config, &r#as, stagger_override, mode),
                }
            }
            Command::Merger {
                config,
                once,
                quiet,
            } => {
                let config = registry::resolve_config(config)?;
                coordination::merger::run(&config, once, quiet)
            }
            Command::Sync {
                config,
                once,
                dry_run,
                quiet,
            } => {
                let config = registry::resolve_config(config)?;
                transport::sync::run(transport::sync::Args {
                    config,
                    once,
                    dry_run,
                    quiet,
                })
            }
            Command::Remote {
                host,
                config,
                remote_args,
            } => {
                let config = registry::resolve_config(config)?;
                let code = transport::remote::run(transport::remote::Args {
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
                coordination::codex_channel::run(coordination::codex_channel::Args {
                    me: r#as,
                    channel_dir,
                    config,
                    catch_up,
                    direct_only,
                })
            }
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
