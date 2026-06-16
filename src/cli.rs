//! CLI schema for giga-harness.
//!
//! The `Cli` struct + `Command` enum below ARE the `--help` surface:
//! every `#[command(...)]` / `#[arg(...)]` attribute and every
//! doc-comment is rendered verbatim by clap into the per-subcommand
//! help text, which is a compatibility contract. Do not reword,
//! reorder, or reformat them.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "giga",
    version,
    about = "Manual multi-agent coordination harness",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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
        /// v0.6.38 Phase H: also spawn the `giga ui` dashboard as a
        /// pane in the launch session. Idempotent: skipped silently
        /// when the server is already running (per ~/.giga/ui.pid).
        #[arg(long)]
        ui: bool,
        /// Port for the auto-spawned `giga ui` pane. Default 7878.
        /// Ignored when `--ui` is not set.
        #[arg(long, value_name = "PORT", default_value_t = 7878)]
        ui_port: u16,
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
        /// Skip all Windows-related upgrade work. Suppresses the
        /// WSL→Windows interop install.ps1 call (local co-located
        /// Windows agents), targeted disarm/rearm broadcasts for
        /// Windows agents, and install on Windows peer hosts. Linux
        /// peers + the POSIX-side install proceed normally. Use when
        /// upgrading only the POSIX side of a mixed-platform swarm.
        #[arg(long)]
        skip_windows: bool,
        /// Print what would happen; don't run install or post.
        #[arg(long)]
        dry_run: bool,
        /// v0.6.41: bare install — skip the swarm-aware machinery
        /// (Windows agent disarm/rearm broadcast, peer-host
        /// install) and just update the local binary. Equivalent
        /// to running `giga upgrade` from a no-swarm directory.
        /// Used by the UI's "upgrade" button.
        #[arg(long)]
        bare: bool,
    },
    /// Launch the browser-based dashboard for managing every
    /// registered swarm on this machine.
    ///
    /// Phase A skeleton (v0.6.31): boots an axum server on
    /// 127.0.0.1:7878 (default) with a placeholder page + health
    /// endpoint. Single-instance enforced via ~/.giga/ui.pid.
    /// Ctrl-C to stop.
    ///
    /// Read-only swarm + channel APIs land in Phase B-E; Svelte
    /// frontend lands in Phase F. See ./UI_DESIGN.md in the giga
    /// workdir for the full design + plan.
    Ui {
        /// Address to bind. Defaults to localhost-only. Pass
        /// 0.0.0.0 to expose on the network (no auth in Phase A —
        /// don't do this on untrusted networks).
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// TCP port. Default 7878.
        #[arg(long, default_value_t = 7878)]
        port: u16,
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
    /// Doc source: `templates/CLAUDE_OPERATOR.md`, baked into the binary at
    /// compile time. No network. Same content for both audiences.
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
    /// swarm dir (canonical TOML + agents/ templates, excluding
    /// *.local.toml + workdirs/) + ensure peer has a `this_host.toml`.
    /// After this, run `giga add-agent --host <name> ...` to put
    /// agents on the new host.
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
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
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
