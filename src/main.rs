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

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod add_agent;
mod add_channel;
mod add_host;
mod claude_operator;
mod config;
mod hosts;
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
mod set_swarm_boss;
mod stale_wait;
mod sweep;
mod sync;
mod switch;
mod transport;
mod transports;
mod templates;
mod terminal;
mod runtime;
mod takeover;
mod teleport;
mod trust;
mod upgrade;
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
        /// Bootstrap THIS machine as a remote peer in an existing swarm.
        /// Default `--transport rsync+tailscale` installs Tailscale +
        /// rsync + enables Tailscale SSH. `--transport git` installs
        /// git + rsync + smoke-tests the state repo URL.
        #[arg(long)]
        remote_node: bool,
        /// Override the default inbox directory (~/projects/inbox).
        /// Only used with --remote-node.
        #[arg(long, value_name = "PATH")]
        inbox_dir: Option<PathBuf>,
        /// Which transport plug this peer will use. Default
        /// `rsync+tailscale` preserves v0.2 behavior. `git` activates
        /// the v0.3 git-based state-repo transport.
        #[arg(long, value_name = "KIND", default_value = "rsync+tailscale")]
        transport: String,
        /// State-repo URL for `--transport git`. Required when transport
        /// is git; ignored otherwise.
        #[arg(long, value_name = "URL")]
        repo: Option<String>,
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
    /// Create inbox files and per-agent AGENTS.md from a config.
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
        /// scaffolded and don't want to re-render AGENTS.md files.
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
        /// Sleep this many seconds between starting each agent's CLI.
        /// 0 (default) = launch all at once. For 10+ agent swarms, pass
        /// 5-15s to avoid the TPM-limit storm from N simultaneous
        /// `claude` first turns. The total launch time becomes
        /// roughly `(N-1) * stagger` seconds.
        #[arg(long, value_name = "SECONDS", default_value_t = 0)]
        stagger_per_agent_seconds: u64,
    },
    /// Move an agent from one host to another in the tailnet.
    ///
    /// Updates `agent.host` in the canonical TOML, rsyncs the agent's
    /// workdir from source to target (direct over tailnet SSH;
    /// two-hop fallback via operator), prepends a "you have been
    /// teleported" banner to HANDOVER.md on the target, syncs TOML
    /// to peers, kills the source tmux pane gracefully, and launches
    /// the agent on the target.
    ///
    /// Channel slice files are NOT moved (per-host append logs;
    /// past posts stay in the source's slice forever, still visible
    /// swarm-wide via merge). The agent's `~/.claude/` conversation
    /// history is also per-machine — agent restarts fresh on target
    /// and reads HANDOVER.md (with the teleport banner) for context.
    /// See TELEPORT_DESIGN.md.
    Teleport {
        /// The agent slug to teleport.
        agent: String,
        /// Destination host name (must exist in [[hosts]]).
        #[arg(long, value_name = "HOST")]
        to: String,
        /// Source host name. Optional — defaults to the agent's
        /// current `host` field in the TOML.
        #[arg(long, value_name = "HOST")]
        from: Option<String>,
        /// Don't kill the source tmux pane after the target pane is
        /// up. Operator handles teardown manually after verifying the
        /// target-side agent is healthy.
        #[arg(long)]
        keep_running: bool,
        /// Print every step that would be taken; no side effects.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Flip an agent's runtime in-place: regenerate AGENTS.md with
    /// the new runtime's session-start instructions, append a takeover
    /// block to HANDOVER.md, and print a one-shot prompt the new
    /// agent should follow. Designed for the operator workflow "start
    /// a fresh CLI in the existing workdir and say: use giga to take
    /// over from this <old-runtime> agent" — the new agent runs
    /// `giga takeover` with no flags and giga handles the rest.
    Takeover {
        /// Override the agent slug. By default, takeover auto-detects
        /// the agent by matching cwd to one of the [[agents]].workdir
        /// entries — the new CLI already knows who it is from its
        /// freshly-read AGENTS.md, so the flag is rarely needed.
        #[arg(long = "as", value_name = "SLUG")]
        as_agent: Option<String>,
        /// Target runtime for the takeover. Defaults to `claude`
        /// because Claude is the most common takeover tool, but any
        /// supported runtime works (codex/agy).
        #[arg(long, value_name = "RUNTIME", default_value = "claude")]
        to: String,
        /// Print the plan + the takeover prompt; don't touch TOML,
        /// AGENTS.md, or HANDOVER.md.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Promote an existing agent to `swarm_boss` (or demote with
    /// `--unset`). At most one swarm_boss per host; promotion
    /// requires platform=wsl. After the TOML write, re-runs `giga
    /// init` to regenerate AGENTS.md so the boss section is fresh.
    SetSwarmBoss {
        /// Agent slug to promote (or demote with `--unset`).
        slug: String,
        /// Demote: clear the swarm_boss flag on this agent.
        #[arg(long)]
        unset: bool,
        /// Don't re-run `giga init` after the TOML write. Useful when
        /// chaining commands or inspecting the TOML before scaffold
        /// regeneration.
        #[arg(long)]
        no_init: bool,
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
    },
    /// Install the latest giga binary on this host (and optionally on
    /// every peer), then post a "please re-arm your watcher" broadcast
    /// to all `_*.md` channels so agents pick up the new binary.
    ///
    /// Without flags: updates local + all peers, auto-detects an agent
    /// to post the broadcast as (swarm_boss preferred; falls back to
    /// any local broadcast participant). Pass `--as <agent>` to
    /// override. `--skip-peers` / `--skip-broadcast` for partial runs;
    /// `--dry-run` to preview.
    Upgrade {
        #[arg(long, default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Agent slug to post the rearm broadcast as (must be a
        /// participant of the broadcast channel). Omit to print the
        /// broadcast command for manual run instead.
        #[arg(long, value_name = "AGENT")]
        r#as: Option<String>,
        /// Don't propagate the install to peer hosts.
        #[arg(long)]
        skip_peers: bool,
        /// Don't post the rearm broadcast after upgrade.
        #[arg(long)]
        skip_broadcast: bool,
        /// Print what would happen; don't run install or post.
        #[arg(long)]
        dry_run: bool,
    },
    /// List the swarm's hosts + which agents live on each + whether
    /// this_host matches. Read-only; useful for orientation after
    /// `giga add-host` / `giga add-agent --host` to confirm topology.
    Hosts {
        #[arg(value_name = "CONFIG", default_value = "giga-harness.toml")]
        config: PathBuf,
        /// Show tailnet members that aren't yet registered in this
        /// swarm. Queries `tailscale status` for the roster + diffs
        /// against [[hosts]] entries; surfaces candidates for
        /// `giga add-host`. Falls back to Windows-side Tailscale
        /// from a WSL distro when WSL doesn't have its own tailscale.
        #[arg(long)]
        available: bool,
    },
    /// Operator help for Claude. TTY-aware:
    ///
    ///   * At a terminal: launches `claude --append-system-prompt <doc>`
    ///     so a fresh Claude session boots with the giga operator
    ///     command surface in context. The drop-into-Claude one-shot.
    ///
    ///   * Piped / redirected (e.g. from an agent's Bash tool): just
    ///     prints the doc to stdout. An agent invoking this captures
    ///     the doc into their conversation context.
    ///
    /// Doc source: `CLAUDE_OPERATOR.md` at the repo root, baked into
    /// the binary at compile time. No network. Same content for both
    /// audiences.
    ClaudeOperator,
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
        /// May be passed positionally OR via `--channel <name>`.
        channel: Option<String>,
        /// For broadcast channels (`_*.md`), address this message to a
        /// specific subset of participants. Synthesizes a `[ack: a, b, c]`
        /// subject prefix that the receiver-side watcher honors — only
        /// named agents fire a Monitor notification. Pass as CSV. No-op
        /// on non-broadcast channels. See BROADCAST_FANOUT_DESIGN.md.
        #[arg(long, value_name = "AGENT-CSV", value_delimiter = ',')]
        to: Vec<String>,
        /// Mark this broadcast as informational. Synthesizes a `[fyi]`
        /// subject prefix — receiver-side watchers append to a per-agent
        /// FYI archive instead of firing a Monitor notification (zero
        /// LLM cost). Mutually exclusive with --to.
        #[arg(long, conflicts_with = "to")]
        fyi: bool,
        /// Alias for the positional CHANNEL arg. Either form works;
        /// exactly one must be provided.
        #[arg(long = "channel", value_name = "CHANNEL")]
        channel_flag: Option<String>,
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
        /// author's path form (e.g. `/home/alice/...` or
        /// `C:\Users\Alice\...`); per-host localizers substitute.
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
        /// Set this agent as the swarm_boss. At most one per host;
        /// must be platform=wsl (sync + merger are POSIX-only). The
        /// swarm_boss runs sync + merger Monitors and (when smart-
        /// compaction is enabled) supervises worker agent compaction.
        #[arg(long)]
        swarm_boss: bool,
        /// Skip auto-appending the new slug to broadcast-channel
        /// participants (channels whose `file` starts with `_`).
        #[arg(long)]
        no_broadcast: bool,
        /// Use a custom AGENTS.md template file instead of the
        /// generated minimal stub. The contents are written verbatim
        /// to `agents/<slug>.md`.
        #[arg(long, value_name = "PATH")]
        template: Option<PathBuf>,
        /// Don't write anything; print the planned changes and exit.
        #[arg(long)]
        dry_run: bool,
        /// The directory where this agent actually edits code, separate
        /// from --workdir (the launch context where AGENTS.md lives).
        /// When set, giga injects it into the agent's AGENTS.md and
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
    /// Append a new [[hosts]] entry to the canonical TOML and
    /// (by default) auto-bootstrap the new peer: mkdir + rsync the
    /// canonical TOML + ensure peer has a `this_host.toml`. After
    /// this, run `giga add-agent --host <name> ...` to put agents
    /// on the new host.
    ///
    /// Typical use: after `giga setup --remote-node` on the peer +
    /// noting its tailnet hostname, run this on the operator host
    /// to register the peer in the swarm.
    AddHost {
        /// Slug for the new host (matches [[hosts]].name + agent.host).
        #[arg(long)]
        name: String,
        /// Full tailnet FQDN of the peer (e.g. wsl-b.tail0000.ts.net).
        /// `giga setup --remote-node` on the peer prints this.
        #[arg(long, value_name = "FQDN")]
        tailnet_hostname: String,
        /// SSH user on the peer. Defaults to $USER (homogeneous-user
        /// setup); set when the peer has a different OS user.
        #[arg(long, value_name = "USER")]
        ssh_user: Option<String>,
        /// Absolute path on the peer where the swarm config lives.
        /// Defaults to the local config dir (homogeneous-path setup);
        /// set when the peer's $HOME differs from the operator's.
        #[arg(long, value_name = "PATH")]
        remote_config_dir: Option<PathBuf>,
        /// Absolute path on the peer where the inbox lives. Defaults
        /// to the local inbox path; set when the peer's filesystem
        /// layout differs.
        #[arg(long, value_name = "PATH")]
        remote_inbox_dir: Option<PathBuf>,
        /// Don't auto-push the canonical TOML to the new peer (skip
        /// the SSH/rsync step). Use when the peer isn't reachable yet.
        #[arg(long)]
        no_bootstrap: bool,
        /// Print the planned change without writing.
        #[arg(long)]
        dry_run: bool,
        /// Name to register THIS host as in [[hosts]] during a
        /// first-host migration (local-only → multi-host). Auto-detected
        /// from $HOSTNAME or /etc/hostname when omitted. Ignored when
        /// the swarm already has [[hosts]] entries.
        #[arg(long, value_name = "NAME")]
        this_host_name: Option<String>,
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
        /// Override the per-swarm broadcast stagger value for this
        /// watcher invocation. Precedence: --stagger-seconds > TOML
        /// `[broadcast].stagger_seconds` > 15s default. See
        /// BROADCAST_FANOUT_DESIGN.md for the fanout-limiter rationale.
        #[arg(long, value_name = "N")]
        stagger_seconds: Option<u64>,
        /// Shorthand for `--stagger-seconds 0` — instant broadcast
        /// fanout (no per-slot delay). Use when you've confirmed
        /// rate-limit headroom and want notifications to surface
        /// ASAP. Mutually exclusive with --stagger-seconds.
        #[arg(long, conflicts_with = "stagger_seconds")]
        no_stagger: bool,
        /// Antigravity-runtime mode: force-flush stdout after every
        /// line, and exit 0 the moment a new message arrives that's
        /// `WAITING ON: <this-agent>`. AGY's reactive-wakeup system
        /// fires on the task completion, resuming the agent's
        /// session with the action-worthy event delivered. Implies
        /// `--no-stagger`. Mutually exclusive with `--codex`.
        #[arg(long, conflicts_with = "codex")]
        agy: bool,
        /// Codex-runtime mode: instead of stdout, write JSON
        /// envelopes to `$CODEX_CHANNEL_DIR/inbox/`. The codex CLI
        /// reads from inbox and surfaces envelopes as inbound
        /// messages. Requires `CODEX_CHANNEL_DIR` env var — set
        /// automatically by `giga launch` for codex-runtime agents.
        /// Mutually exclusive with `--agy`.
        #[arg(long)]
        codex: bool,
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
        /// Suppress startup chatter; only emit on errors. Set by the
        /// swarm_boss AGENTS.md Monitor lines so the agent's
        /// notification stream doesn't flood.
        #[arg(long)]
        quiet: bool,
    },
    /// Long-running sync daemon — every ~3s, rsync the canonical
    /// giga-harness.toml + own slice files to each peer host over
    /// Tailscale SSH (per REMOTE_DESIGN.md §4). Re-reads the config
    /// every ~15s so `add-agent` / `add-channel` after launch is
    /// picked up automatically.
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
        /// Suppress per-tick summary lines; only emit on errors and
        /// startup. Set by the swarm_boss AGENTS.md Monitor lines so
        /// the agent's notification stream doesn't flood with "tick
        /// complete: N attempted" every 3 seconds.
        #[arg(long)]
        quiet: bool,
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
            transport,
            repo,
            dry_run,
        } => {
            if remote_node {
                setup_remote_node::run(setup_remote_node::Args {
                    inbox_dir,
                    dry_run,
                    transport,
                    repo,
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
            stagger_per_agent_seconds,
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
                let code = remote::run(remote::Args {
                    host,
                    config,
                    remote_args,
                })?;
                std::process::exit(code);
            }
            launch::run(
                &config,
                skip_init,
                dry_run,
                &only,
                new_window,
                &terminal,
                stagger_per_agent_seconds,
            )
        }
        Command::Hosts { config, available } => {
            // When the user didn't override --config and we can't resolve
            // a default `giga-harness.toml` (not in a swarm dir, no
            // registry hit), fall back to listing every registered swarm
            // instead of erroring with the cryptic "no swarm registered"
            // message. Explicit-but-bad --config still errors loud.
            let was_default = config == PathBuf::from("giga-harness.toml");
            match registry::resolve_config(config) {
                Ok(c) if available => hosts::run_available(&c),
                Ok(c) => hosts::run(&c),
                Err(_) if was_default && !available => hosts::run_list_all(),
                Err(e) => Err(e),
            }
        }
        Command::ClaudeOperator => claude_operator::run(),
        Command::Upgrade {
            config,
            r#as,
            skip_peers,
            skip_broadcast,
            dry_run,
        } => {
            let config = registry::resolve_config(config)?;
            upgrade::run(upgrade::Args {
                config,
                as_agent: r#as,
                skip_peers,
                skip_broadcast,
                dry_run,
            })
        }
        Command::Teleport {
            agent,
            to,
            from,
            keep_running,
            dry_run,
            config,
        } => {
            let config = registry::resolve_config(config)?;
            teleport::run(teleport::Args {
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
                anyhow::anyhow!(
                    "unknown --to runtime `{to}` — valid: claude, codex, agy"
                )
            })?;
            takeover::run(takeover::Args {
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
            set_swarm_boss::run(set_swarm_boss::Args {
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
            post::run(post::Args {
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
        } => add_agent::run(add_agent::Args {
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
            add_channel::run(add_channel::Args {
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
            add_host::run(add_host::Args {
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
            stagger_seconds,
            no_stagger,
            agy,
            codex,
        } => {
            let config = registry::resolve_config(config)?;
            let stagger_override = if no_stagger {
                Some(0)
            } else {
                stagger_seconds
            };
            // v0.6.0: derive watch mode. clap's conflicts_with enforces
            // --agy and --codex are mutually exclusive; default is Claude.
            let mode = if agy {
                watch::WatchMode::Agy
            } else if codex {
                watch::WatchMode::Codex
            } else {
                watch::WatchMode::Default
            };
            match channel {
                Some(c) => {
                    let path = resolve_channel(&c, &config)?;
                    watch::run_single(&path, &r#as, mode)
                }
                None => watch::run_multi(&config, &r#as, stagger_override, mode),
            }
        }
        Command::Merger {
            config,
            once,
            quiet,
        } => {
            let config = registry::resolve_config(config)?;
            merger::run(&config, once, quiet)
        }
        Command::Sync {
            config,
            once,
            dry_run,
            quiet,
        } => {
            let config = registry::resolve_config(config)?;
            sync::run(sync::Args {
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
