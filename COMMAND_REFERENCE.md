# COMMAND_REFERENCE

Full command surface for `giga`, grouped by use case. Run `giga <command> --help` for the canonical, auto-generated detail; this doc adds the "when would I reach for this" context.

## Table of contents

- [Bootstrap & lifecycle](#bootstrap--lifecycle)
  - [`giga setup`](#giga-setup)
  - [`giga validate`](#giga-validate)
  - [`giga init`](#giga-init)
  - [`giga launch`](#giga-launch)
- [Day-to-day operator](#day-to-day-operator)
  - [`giga sweep`](#giga-sweep)
  - [`giga post`](#giga-post)
  - [`giga hosts`](#giga-hosts)
  - [`giga claude-operator`](#giga-claude-operator)
- [Topology editing](#topology-editing)
  - [`giga add-agent`](#giga-add-agent)
  - [`giga add-channel`](#giga-add-channel)
  - [`giga add-host`](#giga-add-host)
  - [`giga set-swarm-boss`](#giga-set-swarm-boss)
- [Agent lifecycle](#agent-lifecycle)
  - [`giga teleport`](#giga-teleport)
  - [`giga takeover`](#giga-takeover)
  - [`giga switch`](#giga-switch)
- [Cross-host operations](#cross-host-operations)
  - [`giga remote`](#giga-remote)
  - [`giga sync`](#giga-sync)
  - [`giga merger`](#giga-merger)
- [Maintenance](#maintenance)
  - [`giga upgrade`](#giga-upgrade)
  - [`giga watch`](#giga-watch)
  - [`giga codex-channel`](#giga-codex-channel)
- [Quick lookup table](#quick-lookup-by-goal)

---

## Bootstrap & lifecycle

### `giga setup`

One-command bootstrap. Launches a fresh Claude Code session with a baked-in prompt that walks the user through scaffolding a multi-agent swarm — picks slugs, roles, peers, topology, launcher mode, and which agent is `swarm_boss`. Writes the canonical TOML + per-agent templates + `agents/<slug>.md` files.

```sh
# Fresh project — agent-guided scaffolding
giga setup

# Bootstrap THIS machine as a remote peer in an existing swarm
giga setup --remote-node                          # default: rsync + Tailscale
giga setup --remote-node --transport git --repo <url>   # git-state-repo transport

# Custom inbox dir on a remote peer
giga setup --remote-node --inbox-dir /opt/giga-inbox

# Preview without making changes (remote-node only)
giga setup --remote-node --dry-run
```

After `setup` completes you'll have a runnable swarm config. From there: `giga init` → `giga launch`.

### `giga validate`

Validate a config without touching the filesystem. Catches typos in `participants`, missing inbox dirs, multiple bench schedulers, multiple swarm bosses on the same host, and structural issues before you let them spread to multiple hosts.

```sh
giga validate                          # ./giga-harness.toml
giga validate /path/to/config.toml
```

Always run this after hand-editing the TOML and before `giga init`.

### `giga init`

Scaffold inbox files + render per-agent `AGENTS.md` from the templates. Also registers the swarm in `~/.giga/swarms.toml` so all other commands can auto-resolve the config from a code root. Idempotent — safe to re-run after edits.

```sh
giga init                              # default config in cwd
giga init /path/to/config.toml

# Don't pre-trust the agent workdirs in Claude Code's settings
giga init --no-trust
```

Re-run any time you've added an agent, changed an agent's template, or changed broadcast participation. New channels picked up by running watchers within ~15s of the canonical TOML being synced — no need to re-launch.

### `giga launch`

Spawn one terminal per agent. The default mode auto-detects: Windows Terminal → tmux → print fallback.

```sh
# Cold start the whole swarm
giga launch

# Pick the multiplexer explicitly
giga launch --terminal tmux
giga launch --terminal mac-terminal    # one native Terminal.app window per agent
giga launch --terminal wt              # Windows Terminal
giga launch --terminal print           # dry-print commands for manual paste

# Only spawn specific agents (e.g. after add-agent)
giga launch --only design              # one agent
giga launch --only design,code,test    # several

# Force fresh wt window when the named one got torn up
giga launch --new-window

# Run launch on a remote tailnet host
giga launch --host wsl-box-b --only some-agent

# Stagger per-agent CLI starts to avoid TPM-limit storms (10+ agent swarms)
giga launch --stagger-per-agent-seconds 10

# Skip the giga init step (panes only)
giga launch --skip-init

# Preview without spawning
giga launch --dry-run
```

For 10+ agent swarms, `--stagger-per-agent-seconds 10` spaces each `claude` first-turn out so Anthropic's rate limits don't kill the launch. A 20-pane swarm at 10s spreads over ~3 minutes.

---

## Day-to-day operator

### `giga sweep`

Tabulate every channel's last message + open `WAITING ON` tag. Your "where do I owe a response?" command.

```sh
# Everything
giga sweep

# Only channels where alice is being waited on (e.g. as the operator-checking-on-agents query)
giga sweep --owed-by alice

# Sweep on a remote host instead
giga sweep --host wsl-box-b --owed-by alice
```

### `giga post`

Append a properly-formatted message to a channel. The header + timestamp formatting is enforced (so the watcher's header parser recognizes it), and broadcast-channel handling supports the `[fyi]` / `[ack: ...]` fanout prefixes.

```sh
# Standard bilateral post
giga post code-design.md --as design \
  --subject "spec for feature X" \
  --body "Detailed scope..." \
  --waiting-on code --needs "estimate"

# Informational post (no WAITING ON tag)
giga post code-design.md --as design --subject "FYI" --body "Heads up..."

# Body from stdin if --body omitted
echo "long body" | giga post code-design.md --as design --subject "X"

# Broadcast to all participants
giga post _broadcast.md --as design --subject "Swarm-wide announcement" --body "..."

# Broadcast to a SUBSET (only listed agents fire their Monitor)
giga post _broadcast.md --as design --to code,test \
  --subject "Question for code+test only" --body "..."

# Informational broadcast (zero LLM cost — receivers archive instead of firing)
giga post _broadcast.md --as design --fyi \
  --subject "morpheus came online" --body "..."

# Equivalent: use --channel instead of positional arg
giga post --channel code-design.md --as design --subject "..." --body "..."
```

The `--to` and `--fyi` flags shape the broadcast fanout. `--to alice,bob` synthesizes an `[ack: alice, bob]` subject prefix so other channel participants archive-not-fire. `--fyi` synthesizes `[fyi]` so all participants archive.

### `giga hosts`

Read-only — print the swarm's topology so you can confirm what `add-host` / `add-agent --host` did.

```sh
giga hosts                             # registered hosts + agents on each
giga hosts --available                 # also lists tailnet members NOT yet in [[hosts]]
                                       # (good for picking the next peer to add)
```

### `giga claude-operator`

Operator help for Claude. TTY-aware:

- **At a terminal**: launches `claude --append-system-prompt <baked-in-doc>` — drops you into a fresh Claude session with the full giga operator command surface in context. Use when you want to drive the swarm via natural language.

```sh
giga claude-operator                   # interactive: opens claude session
giga claude-operator | less            # piped: prints the doc for inspection
```

- **Piped / redirected**: just prints the doc to stdout. An agent's Bash tool invoking this captures the doc into their conversation context.

---

## Topology editing

### `giga add-agent`

Scaffold a new agent — appends `[[agents]]`, per-peer `[[channels]]` blocks, adds to broadcast channels, writes `agents/<slug>.md`, and re-validates. Runnable from inside any swarm agent's session.

```sh
# Standard local agent
giga add-agent --name infra --workdir /home/alice/.giga/configs/myswarm/workdirs/infra \
  --role "Infra ops — Terraform + AWS" \
  --peer design

# Agent that edits a shared codebase from an isolated workdir
giga add-agent --name code --workdir /home/alice/.giga/configs/myswarm/workdirs/code \
  --code-root /home/alice/projects/myapp \
  --role "Backend dev" --peer design --peer test

# Multiple peers in one call (one bilateral channel per peer)
giga add-agent --name code --workdir /h/code --role "..." --peer design --peer test --peer infra

# Windows-platform agent (terminal launched with PowerShell, not bash)
giga add-agent --name win-tester --workdir 'C:\Users\Alice\testwin' \
  --platform windows --role "Windows GUI tester" --peer code

# Cross-host agent on a peer (auto-bootstraps the peer if needed)
giga add-agent --host wsl-box-b --name remote-perf \
  --workdir /home/alice/.giga/configs/myswarm/workdirs/remote-perf \
  --role "Remote benchmark runner" --peer design

# Flag the new agent as bench scheduler (only one allowed)
giga add-agent --name engine --workdir /h/engine --role "..." \
  --peer code --bench-scheduler

# Flag as swarm_boss directly on creation (requires --platform=wsl)
giga add-agent --name design --workdir /h/design --role "Coordinator" \
  --peer code --peer test --swarm-boss

# Use a custom template instead of the auto-generated stub
giga add-agent --name complex --workdir /h/complex --role "..." \
  --peer design --template ./my-template.md

# Skip auto-adding to broadcast channels
giga add-agent --name silent --workdir /h/silent --role "..." \
  --peer design --no-broadcast

# Preview without writing
giga add-agent --name X --workdir /h/X --role "..." --peer design --dry-run
```

### `giga add-channel`

Append a new bilateral channel between two existing agents. v1 supports exactly two participants.

```sh
giga add-channel --participants alice,bob

# Override the auto-derived alphabetical filename
giga add-channel --participants alice,bob --file alice-bob-urgent.md

# Preview
giga add-channel --participants alice,bob --dry-run
```

The sync daemon propagates the change to peers; merger + watcher pick up the new channel within ~15s.

### `giga add-host`

Append a `[[hosts]]` entry. By default auto-bootstraps the peer: mkdir + rsync canonical TOML + ensure the peer has a `this_host.toml`.

```sh
# Common case: peer with same OS user + matching paths
giga add-host --name wsl-box-b --tailnet-hostname wsl-box-b.tail0000.ts.net

# Heterogeneous setup (different user / path on peer)
giga add-host --name wsl-box-b --tailnet-hostname wsl-box-b.tail0000.ts.net \
  --ssh-user bob \
  --remote-config-dir /home/bob/.giga/configs/myswarm \
  --remote-inbox-dir /home/bob/projects/inbox

# Don't auto-bootstrap (peer offline / will bring up later)
giga add-host --name wsl-box-c --tailnet-hostname wsl-box-c.tail0000.ts.net --no-bootstrap

# First-host migration (local-only swarm → multi-host)
# Auto-detects this host's name from $HOSTNAME or /etc/hostname
giga add-host --name new-peer --tailnet-hostname new-peer.tail0000.ts.net

# Override the auto-detected this-host name
giga add-host --name new-peer --tailnet-hostname new-peer.tail0000.ts.net \
  --this-host-name my-laptop

# Preview
giga add-host --name X --tailnet-hostname X.tail0000.ts.net --dry-run
```

After registering the peer, `giga add-agent --host <name> ...` places agents on it.

### `giga set-swarm-boss`

Promote an existing agent to swarm_boss, or demote with `--unset`. At most one boss per host; promotion requires platform=wsl. Re-runs `giga init` after the TOML write to regenerate AGENTS.md with the boss-supervision section.

```sh
giga set-swarm-boss design             # promote
giga set-swarm-boss design --unset     # demote

# Don't re-run giga init after the TOML write (chained workflows)
giga set-swarm-boss design --no-init
```

---

## Agent lifecycle

### `giga teleport`

Move an agent from one host to another in the tailnet. Updates `agent.host` in the canonical TOML, rsyncs the workdir, prepends a banner to HANDOVER.md on the target, kills the source tmux pane, and launches the agent on the target.

```sh
# Standard: source defaults to agent's current host field
giga teleport research --to wsl-box-b

# Explicit source (when the TOML's out of date)
giga teleport research --to wsl-box-b --from wsl-box-a

# Keep the source pane alive so you can verify the target before teardown
giga teleport research --to wsl-box-b --keep-running

# Preview every step
giga teleport research --to wsl-box-b --dry-run
```

The agent restarts fresh on the target and reads the HANDOVER.md banner for context. Per-host `~/.claude/` history doesn't transfer — HANDOVER is the bridge.

### `giga takeover`

Flip an agent's runtime in-place. Use when one CLI gets stuck and you want to start a different one (e.g. AGY → Claude) in the same workdir. Regenerates AGENTS.md for the new runtime, appends a takeover block to HANDOVER.md, locates the prior session log, and prints a one-shot prompt for the new agent.

```sh
# Operator flow: start fresh Claude in the (stuck) agent's workdir, then run:
giga takeover                          # auto-detects slug from cwd; --to defaults to claude

# Explicit target runtime
giga takeover --to claude              # most common (default)
giga takeover --to codex
giga takeover --to agy

# Explicit slug (when cwd → agent autodetection fails)
giga takeover --as coder --to claude

# Preview without touching TOML / AGENTS.md / HANDOVER.md
giga takeover --dry-run --to claude
```

The new agent's first prompt: *"use giga to take over from this <old-runtime> agent"*. giga handles the rest.

### `giga switch`

Manage runtime credentials. Today only `--runtime claude` is supported. Credentials live in `~/.claude-accounts/<name>.json` snapshots; switching copies the chosen snapshot to `~/.claude/.credentials.json` (saving the previously-active one first so in-place token refreshes are preserved).

```sh
giga switch --runtime claude           # show current + list accounts

# First time: adopt an existing ~/.claude/.credentials.json as a named snapshot
giga switch --runtime claude --setup primary

# Provision an empty slot for a new account
giga switch --runtime claude --add overflow

# Switch to a named account
giga switch --runtime claude overflow
```

After switching, run `claude` and use `/login` to populate the new slot.

---

## Cross-host operations

### `giga remote`

Run any giga subcommand on a remote host over Tailscale SSH. Tailnet identity auths the connection — no key exchange needed.

```sh
giga remote --host wsl-box-b sweep
giga remote --host wsl-box-b sweep --owed-by alice
giga remote --host wsl-box-b validate
giga remote --host wsl-box-b launch --only alice
giga remote --host wsl-box-b -- giga post bob-alice.md --as alice --subject "..." --body "..."
```

Trailing args after the host go to the remote subcommand. Equivalent shortcuts exist on some commands: `giga sweep --host <h>`, `giga launch --host <h>`.

### `giga sync`

Long-running daemon. Every ~3s, rsync the canonical `giga-harness.toml` + own slice files to each peer host over Tailscale SSH. Re-reads the config every ~15s so `add-agent` / `add-channel` after launch is picked up automatically.

```sh
giga sync                              # daemon mode
giga sync --once                       # single tick (tests / catch-up)
giga sync --dry-run --once             # preview commands
giga sync --quiet                      # only errors/startup; for swarm_boss Monitor entries
```

Auto-spawned by `giga launch` on cross-host swarms when no swarm_boss exists. When a boss exists, the boss's AGENTS.md arms it as a Monitor entry.

### `giga merger`

Long-running daemon. For every cross-host channel, polls all `<channel>.<host>.md` slice files and appends new bytes to `<channel>.md` (the file the watcher tails).

```sh
giga merger                            # daemon mode
giga merger --once                     # single sweep (tests)
giga merger --quiet                    # only errors/startup
```

Runs alongside sync + watch per host. No-op on local-only swarms.

---

## Maintenance

### `giga upgrade`

Install the latest giga binary on this host + on every peer + post the rearm broadcast.

```sh
giga upgrade                           # local + all peers + broadcast
giga upgrade --skip-peers              # local only
giga upgrade --skip-broadcast          # local + peers but no announce
giga upgrade --as <agent>              # post broadcast as a specific agent
giga upgrade --dry-run                 # preview the install + broadcast plan
```

Windows-side handling (v0.6.12+): dispatches to `install.ps1` via PowerShell on native Windows; uses WSL interop to run install.ps1 against the Windows giga.exe when there are co-located Windows agents on a WSL operator host. Windows agents get a targeted `[ack: <slugs>]` disarm broadcast + 60s grace before install, then a matching rearm broadcast — accounts for Windows file-locks on running binaries.

For broadcast targeting: `--as <slug>` is auto-detected as the swarm_boss if not supplied.

### `giga watch`

Long-running watcher — one stdout line per new message. Two modes:

```sh
# Multi-channel (config-aware) — what every agent's Monitor arms
giga watch --as alice                  # tails every channel where alice participates
giga watch --as alice --no-stagger     # disable broadcast slot-staggering
giga watch --as alice --stagger-seconds 60   # override the per-swarm broadcast stagger
giga watch --as alice --agy            # Antigravity mode: exit on `WAITING ON: alice`
giga watch --as alice --codex          # Codex mode: write JSON envelopes to $CODEX_CHANNEL_DIR/inbox/

# Legacy single-channel
giga watch alice-bob.md --as alice
```

Almost never invoked by the operator directly — agents arm it inside their session via Claude's Monitor tool (or AGY's `run_command(background=true)` or the Codex bridge pane). The session-start protocol in each agent's AGENTS.md spells out the runtime-specific invocation.

### `giga codex-channel`

Forward giga inbox notifications into a running Codex CLI's filesystem channel. Specialized — for the experimental source-built Codex flow.

```sh
giga codex-channel --as alice --channel-dir /path/to/codex-channel
giga codex-channel --as alice --channel-dir /path/to/codex-channel --catch-up
giga codex-channel --as alice --channel-dir /path/to/codex-channel --direct-only
```

Most operators never run this directly — the codex bridge pane (spawned by `giga launch` for codex-runtime agents) handles it.

---

## Quick lookup by goal

| Goal | Command |
|------|---------|
| **Bootstrap** | |
| Fresh project, agent-guided scaffolding | `giga setup` |
| Add THIS machine as a remote peer | `giga setup --remote-node` |
| Validate hand-edited TOML | `giga validate` |
| Scaffold inboxes + AGENTS.md | `giga init` |
| Cold start the swarm | `giga init && giga launch` |
| Cold start a 10+ agent swarm without TPM storm | `giga launch --stagger-per-agent-seconds 10` |
| Cold start on macOS | `giga launch --terminal mac-terminal` |
| Cold start on Linux explicitly tmux | `giga launch --terminal tmux` |
| **Day-to-day** | |
| See open WAITING ON tags | `giga sweep` |
| See only channels waiting on you | `giga sweep --owed-by <slug>` |
| Post a bilateral message | `giga post <channel> --as <slug> --subject "..." --body "..."` |
| Broadcast to subset (others archive) | `giga post _broadcast.md --as <slug> --to a,b --subject "..." --body "..."` |
| Broadcast informationally (zero LLM cost) | `giga post _broadcast.md --as <slug> --fyi --subject "..." --body "..."` |
| Confirm swarm topology | `giga hosts` |
| List available tailnet members to add | `giga hosts --available` |
| Drop into Claude with operator surface | `giga claude-operator` |
| **Topology editing** | |
| Add an agent + its channels | `giga add-agent --name X --workdir Y --role "..." --peer P` |
| Add an agent on a peer host | `giga add-agent --host <h> --name X --workdir Y --role "..." --peer P` |
| Add a new bilateral between existing agents | `giga add-channel --participants <a>,<b>` |
| Register a new tailnet peer | `giga add-host --name <n> --tailnet-hostname <fqdn>` |
| Promote an agent to swarm_boss | `giga set-swarm-boss <slug>` |
| Demote a swarm_boss | `giga set-swarm-boss <slug> --unset` |
| **Agent lifecycle** | |
| Move an agent to another tailnet host | `giga teleport <slug> --to <host>` |
| Switch a stuck agent's CLI (e.g. AGY → Claude) | `giga takeover` (from the new CLI in the workdir) |
| Switch Claude credential account | `giga switch --runtime claude <name>` |
| **Cross-host** | |
| Run a giga command on a peer | `giga remote --host <h> -- <subcommand> [args]` |
| Spawn an agent's pane on a peer | `giga launch --host <h> --only <slug>` |
| Sweep on a peer | `giga sweep --host <h>` |
| Force a sync tick | `giga sync --once` |
| Force a merge sweep | `giga merger --once` |
| **Maintenance** | |
| Upgrade local + peers + announce | `giga upgrade` |
| Upgrade local only | `giga upgrade --skip-peers` |
| Re-run init quietly | `giga init --no-trust` |
| Preview an upgrade plan | `giga upgrade --dry-run` |

---

## See also

- [`README.md`](README.md) — what giga is + the install + quick concept
- [`QUICKSTART.md`](QUICKSTART.md) — the three common flows (cold start, add an agent, stand down)
- [`REMOTE_QUICKSTART.md`](REMOTE_QUICKSTART.md) — adding a second host
- [`MANUAL_SETUP.md`](MANUAL_SETUP.md) — what `giga setup` does, step-by-step, if you want to hand-roll it
- [`CLAUDE_OPERATOR.md`](CLAUDE_OPERATOR.md) — the operator-help doc baked into `giga claude-operator`
