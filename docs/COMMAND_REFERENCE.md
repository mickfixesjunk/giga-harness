# COMMAND_REFERENCE

The authoritative, exhaustive command reference for `giga` (v0.6.54) — every subcommand, every flag, every default. Flag spellings and defaults match the binary's `--help` output verbatim. Run `giga <command> --help` for the same detail straight from the binary; this doc groups the 22 subcommands logically and adds the "when would I reach for this" context plus the per-flag caveats that bite people.

## The `--config` / `[CONFIG]` flag (shared)

Almost every subcommand takes the swarm config, always defaulting to `giga-harness.toml`:

- Commands that take it as a **positional** `[CONFIG]` arg: `validate`, `init`, `launch`, `hosts`, `sweep`.
- Commands that take it as a **flag** `--config <CONFIG>`: `post`, `watch`, `sync`, `merger`, `remote`, `teleport`, `takeover`, `set-swarm-boss`, `add-agent`, `add-channel`, `add-host`, `upgrade`, `codex-channel`. The default is `giga-harness.toml` for every one of them.
- Commands that take **no** config at all: `setup`, `claude-operator`, `switch`, `ui`.

Most commands resolve the config CWD-independently: if `giga-harness.toml` isn't in cwd, giga walks up the directory tree and then consults the swarm registry in `~/.giga/swarms.toml` (matching against registered `code_roots`). The exceptions are `init` and `add-agent`, which use the path **literally** — run them from the swarm directory or pass an explicit `--config`. To keep the per-command tables scannable, `--config <CONFIG>` (default `giga-harness.toml`) is documented here once and noted per command only where its behavior differs.

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
- [Dashboard](#dashboard)
  - [`giga ui`](#giga-ui)
- [Quick lookup table](#quick-lookup-by-goal)

---

## Bootstrap & lifecycle

### `giga setup`

One-command bootstrap. Launches a fresh Claude Code session with a baked-in prompt that walks the user through scaffolding a multi-agent swarm — picks slugs, roles, peers, topology, launcher mode, and which agent is `swarm_boss`. Writes the canonical TOML + per-agent templates + `agents/<slug>.md` files. **`--remote-node` repurposes the command** to bootstrap THIS machine as a remote peer in an existing swarm instead of guiding a fresh setup.

**Usage:** `giga setup [OPTIONS]`

| Flag | Default | Notes |
|------|---------|-------|
| `--remote-node` | off | Bootstrap THIS machine as a remote peer (installs rsync + Tailscale, runs `tailscale up`, enables Tailscale SSH, creates the inbox dir). WSL-only. |
| `--inbox-dir <PATH>` | `~/projects/inbox` | Override the remote-peer inbox dir. Only used with `--remote-node`. |
| `--transport <KIND>` | `rsync+tailscale` | Which transport plug this peer uses. Alternative: `git`. Only used with `--remote-node`. |
| `--repo <URL>` | none | State-repo URL. **Required when `--transport git`; ignored otherwise.** |
| `--dry-run` | off | Print what would happen without changing anything. Only used with `--remote-node`. |

```sh
# Fresh project — agent-guided scaffolding
giga setup

# Bootstrap THIS machine as a remote peer in an existing swarm
giga setup --remote-node                                  # default: rsync + Tailscale
giga setup --remote-node --transport git --repo <url>     # git-state-repo transport (--repo required)

# Custom inbox dir on a remote peer
giga setup --remote-node --inbox-dir /opt/giga-inbox

# Preview without making changes (remote-node only)
giga setup --remote-node --dry-run
```

**Notable behavior:** the guided (no `--remote-node`) path requires `claude` on PATH (it bails with an install link otherwise) and does **no scaffolding itself** — the spawned Claude session is what writes the config. If you close that session before it finishes, nothing is created. After `setup` completes you'll have a runnable swarm config; from there it's `giga init` → `giga launch`.

### `giga validate`

Validate a config without touching the filesystem. Catches typos in `participants`, missing inbox dirs, multiple bench schedulers, multiple swarm bosses on the same host, and structural issues before you let them spread to multiple hosts. Also warns about orphan channel files in the inbox dirs (warnings only — `validate` still exits OK).

**Usage:** `giga validate [CONFIG]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[CONFIG]` | `giga-harness.toml` | Config file to validate (positional). |

```sh
giga validate                          # ./giga-harness.toml
giga validate /path/to/config.toml
```

Always run this after hand-editing the TOML and before `giga init`.

### `giga init`

Scaffold inbox files + render per-agent `AGENTS.md` from the templates. Also registers the swarm in `~/.giga/swarms.toml` so all other commands can auto-resolve the config from a code root (`init` is the only command that creates/updates registry *entries*; the `giga ui` archive toggle is the only other writer, and it just flips an existing entry's `archived` flag). Idempotent — safe to re-run after edits.

**Usage:** `giga init [OPTIONS] [CONFIG]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[CONFIG]` | `giga-harness.toml` | Config file (positional). **Used literally** — does NOT resolve via the registry, so run from the swarm dir or pass the path. |
| `--no-trust` | off | Skip pre-populating Claude Code's per-folder trust state. By default giga marks every agent workdir (and any `code_root`) as trusted so `claude` doesn't prompt on first launch. |

```sh
giga init                              # default config in cwd
giga init /path/to/config.toml

# Don't pre-trust the agent workdirs in Claude Code's settings
giga init --no-trust
```

Re-run any time you've added an agent, changed an agent's template, or changed broadcast participation. AGENTS.md is **always overwritten** on init/launch, so persistent changes belong in the source `claudemd_template`, not the workdir copy. New channels are picked up by running watchers within ~15s of the canonical TOML being synced — no need to re-launch.

### `giga launch`

Spawn one terminal per agent. The default `--terminal auto` mode auto-detects in order: `wt.exe` → `tmux` → `print` fallback. Runs `giga init` first unless `--skip-init`.

**Usage:** `giga launch [OPTIONS] [CONFIG]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[CONFIG]` | `giga-harness.toml` | Config file (positional). |
| `--host <HOST>` | none | Run launch on a remote host instead of locally. Equivalent to `giga remote --host <HOST> launch [args]`. |
| `--skip-init` | off | Skip the `giga init` step before launching (panes only). |
| `--dry-run` | off | Print the launch plan instead of spawning. |
| `--only <AGENT>` | all agents | Spawn only the named agents (CSV, or repeat the flag). Joins the existing wt window / tmux session instead of replacing it. |
| `--new-window` | off | Force each new tab into its own fresh wt window (`wt -w new`). **wt.exe only** — tmux has no equivalent. |
| `--terminal <MODE>` | `auto` | Launcher to use. `auto` detects `wt.exe` > `tmux` > `print`. Other values: `tmux`, `mac-terminal` (one native Terminal.app window per agent), `wt`, `print`. |
| `--stagger-per-agent-seconds <SECONDS>` | `0` | Sleep N s between starting each agent's CLI. `0` = all at once; total spread ≈ `(N-1) × stagger`. Use 5–15s for 10+ agent swarms to avoid the TPM-limit storm. |
| `--ui` | off | Also spawn a `giga ui` dashboard pane in the launch session. Idempotent — skipped silently if the server is already running (per `~/.giga/ui.pid`). |
| `--ui-port <PORT>` | `7878` | Port for the auto-spawned `giga ui` pane. **Ignored unless `--ui` is set.** |

```sh
# Cold start the whole swarm
giga launch

# Pick the launcher explicitly
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

**Notable behavior:** for 10+ agent swarms, `--stagger-per-agent-seconds 10` spaces each `claude` first-turn out so Anthropic's rate limits don't kill the launch — a 20-pane swarm at 10s spreads over ~3 minutes. On a cross-host swarm with no swarm_boss, a full launch also spawns `giga sync` / `giga merger` daemon panes; when a boss exists, the boss arms those daemons via Monitor entries instead.

---

## Day-to-day operator

### `giga sweep`

Tabulate every channel's last message + open `WAITING ON` tag. Your "where do I owe a response?" command. Read-only.

**Usage:** `giga sweep [OPTIONS] [CONFIG]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[CONFIG]` | `giga-harness.toml` | Config file (positional). |
| `--owed-by <OWED_BY>` | none | Show only channels where `<agent>` is the one being waited on. |
| `--host <HOST>` | none | Run sweep on a remote host instead of locally. Equivalent to `giga remote --host <HOST> sweep [args]`. |

```sh
# Everything
giga sweep

# Only channels where alice is being waited on
giga sweep --owed-by alice

# Sweep on a remote host instead
giga sweep --host wsl-box-b --owed-by alice
```

### `giga post`

Append a properly-formatted message to a channel. The header + timestamp formatting is enforced (so the watcher's header parser recognizes it), and broadcast-channel handling supports the `[fyi]` / `[ack: ...]` fanout prefixes.

**Usage:** `giga post [OPTIONS] --as <AGENT> --subject <SUBJECT> [CHANNEL]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[CHANNEL]` | — | Channel filename (matches a `[[channels]]` entry, `.md` optional) OR an absolute path. Passed positionally OR via `--channel`. |
| `--channel <CHANNEL>` | — | Alias for the positional CHANNEL. **Exactly one of positional / `--channel` is required** (both → error; neither → error). |
| `--as <AGENT>` | **required** | Your agent name — must match one of the channel's participants. |
| `--subject <SUBJECT>` | **required** | Short subject line for the header block. |
| `--body <BODY>` | stdin | Message body. If absent, read from **stdin until EOF**. |
| `--waiting-on <AGENT>` | none | Tag the message `WAITING ON: <agent>`. Omit for an informational post. |
| `--needs <NEEDS>` | none | "What's needed" hint appended to the WAITING ON tag. **Only honored alongside `--waiting-on`** (ignored otherwise). |
| `--to <AGENT-CSV>` | none | Broadcast (`_*.md`) only: address a subset of participants. Synthesizes an `[ack: a, b, c]` prefix so only those agents fire a notification. CSV. No-op on non-broadcast channels. **Mutually exclusive with `--fyi`.** |
| `--fyi` | off | Broadcast (`_*.md`) only: mark informational. Synthesizes a `[fyi]` prefix so receivers archive instead of firing a notification (zero LLM cost). No-op on non-broadcast channels. **Mutually exclusive with `--to`.** |

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

**Notable behavior:** writes under an exclusive cross-platform file lock. On a cross-host channel, the message is dual-written (own slice first, then the merged file); a failed merged write still returns OK with a stderr warning, since the slice is canonical and peers receive it via sync. `--to alice,bob` synthesizes `[ack: alice, bob]` so non-addressed participants archive instead of firing; `--fyi` synthesizes `[fyi]` so **all** participants archive.

### `giga hosts`

Read-only — print the swarm's topology so you can confirm what `add-host` / `add-agent --host` did.

**Usage:** `giga hosts [OPTIONS] [CONFIG]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[CONFIG]` | `giga-harness.toml` | Config file (positional). |
| `--available` | off | Also list tailnet members not yet registered in `[[hosts]]` (queries `tailscale status`; falls back to Windows-side Tailscale from a WSL distro). Surfaces candidates for `giga add-host`. |

```sh
giga hosts                             # registered hosts + agents on each
giga hosts --available                 # also lists tailnet members NOT yet in [[hosts]]
                                       # (good for picking the next peer to add)
```

**Notable behavior:** when the default config can't be resolved and `--available` is not passed, `hosts` falls back to listing every registered swarm. A bad **explicit** `--config` still errors loudly.

### `giga claude-operator`

Operator help for Claude. TTY-aware. **No flags, no config.**

**Usage:** `giga claude-operator`

- **At a terminal:** requires `claude` on PATH; launches `claude --append-system-prompt <baked-in-doc>` — drops you into a fresh Claude session with the full giga operator command surface in context. Use when you want to drive the swarm via natural language.
- **Piped / redirected:** just prints the doc to stdout. An agent's Bash tool invoking this captures the doc into its conversation context.

```sh
giga claude-operator                   # interactive: opens claude session
giga claude-operator | less            # piped: prints the doc for inspection
```

The doc source is `templates/CLAUDE_OPERATOR.md`, baked into the binary at compile time via `include_str!`. No network.

---

## Topology editing

### `giga add-agent`

Scaffold a new agent — appends `[[agents]]`, per-peer `[[channels]]` blocks, adds the slug to broadcast channels, writes `agents/<slug>.md`, and re-validates. Runnable from inside any swarm agent's session.

**Usage:** `giga add-agent [OPTIONS] --name <SLUG> --workdir <WORKDIR> --role <ROLE>`

| Flag | Default | Notes |
|------|---------|-------|
| `--name <SLUG>` | **required** | Agent slug (kebab-case). Becomes part of channel filenames and is what `--as <slug>` expects. |
| `--workdir <WORKDIR>` | **required** | Absolute workdir in the canonical author's path form (e.g. `/home/alice/...` or `C:\Users\Alice\...`). A literal leading `~` is rejected. |
| `--role <ROLE>` | **required** | One-line role description. |
| `--platform <PLATFORM>` | `wsl` | Target OS. Only `wsl` or `windows` are valid. |
| `--peer <AGENT>` | none | Peer agent. **Repeatable — NOT comma-delimited** (unlike `--participants`/`--to`/`--only`, which take CSV). One bilateral `[[channels]]` block per peer; side auto-derived (windows if either side is windows). |
| `--bench-scheduler` | off | Set this agent as the bench scheduler. Fails if another agent already holds the role (one per project). |
| `--swarm-boss` | off | Set this agent as the swarm_boss. At most one per host; requires `--platform=wsl`. |
| `--no-broadcast` | off | Skip auto-appending the new slug to broadcast (`_*.md`) channel participants. |
| `--template <PATH>` | generated stub | Use a custom AGENTS.md template file; written verbatim to `agents/<slug>.md`. |
| `--dry-run` | off | Print the planned changes and exit; write nothing. |
| `--code-root <PATH>` | none | The dir where the agent actually edits code, distinct from `--workdir` (the launch context). Injected into AGENTS.md + the intro; a literal leading `~` is rejected. |
| `--host <HOST>` | local | Host this agent lives on (must match a `[[hosts]].name`). When non-local, triggers best-effort peer bootstrap + remote `giga init`. |
| `--config <CONFIG>` | `giga-harness.toml` | **Used literally** (not resolved via the registry). |

```sh
# Standard local agent
giga add-agent --name infra --workdir /home/alice/.giga/configs/myswarm/workdirs/infra \
  --role "Infra ops — Terraform + AWS" \
  --peer design

# Agent that edits a shared codebase from an isolated workdir
giga add-agent --name code --workdir /home/alice/.giga/configs/myswarm/workdirs/code \
  --code-root /home/alice/projects/myapp \
  --role "Backend dev" --peer design --peer test

# Multiple peers in one call — REPEAT --peer (no CSV form)
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

**Usage:** `giga add-channel [OPTIONS]`

| Flag | Default | Notes |
|------|---------|-------|
| `--participants <AGENT>` | **required** | Participant agent names, comma-separated. v1 is bilateral only — **exactly two** participants. |
| `--file <FILE>` | sorted `<a>-<b>.md` | Override the auto-derived alphabetical filename. Rarely needed. |
| `--dry-run` | off | Print the planned change without writing. |

```sh
giga add-channel --participants alice,bob

# Override the auto-derived alphabetical filename
giga add-channel --participants alice,bob --file alice-bob-urgent.md

# Preview
giga add-channel --participants alice,bob --dry-run
```

**Notable behavior:** refuses a duplicate filename. On a local-only swarm it prints `(local-only swarm — no sync needed)`; on a multi-host swarm the `giga sync` daemon propagates the change to peers and the merger + watcher pick up the new channel within ~15s.

### `giga add-host`

Append a `[[hosts]]` entry. By default auto-bootstraps the peer: mkdir + rsync the canonical TOML + ensure the peer has a `this_host.toml`.

**Usage:** `giga add-host [OPTIONS] --name <NAME> --tailnet-hostname <FQDN>`

| Flag | Default | Notes |
|------|---------|-------|
| `--name <NAME>` | **required** | Slug for the new host (matches `[[hosts]].name` + `agent.host`). Duplicate rejected. |
| `--tailnet-hostname <FQDN>` | **required** | Full tailnet FQDN of the peer (e.g. `wsl-b.tail0000.ts.net`). `giga setup --remote-node` prints this. |
| `--ssh-user <USER>` | `$USER` | SSH user on the peer. Set when the peer has a different OS user. |
| `--remote-config-dir <PATH>` | local config dir | Absolute path on the peer where the swarm config lives. Set when the peer's `$HOME` differs. |
| `--remote-inbox-dir <PATH>` | local inbox path | Absolute path on the peer where the inbox lives. Set when the peer's layout differs. |
| `--no-bootstrap` | off | Don't auto-push the canonical TOML to the peer (skip the SSH/rsync step). Use when the peer isn't reachable yet. |
| `--dry-run` | off | Print the planned change without writing. |
| `--this-host-name <NAME>` | `$HOSTNAME` / `/etc/hostname` | Name to register THIS host as during a first-host migration (local-only → multi-host). Ignored when `[[hosts]]` already has entries. |

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

After registering the peer, `giga add-agent --host <name> ...` places agents on it. On a first-host migration giga also registers the LOCAL host (with a **placeholder** FQDN equal to its name — hand-edit if the real FQDN differs) and rolls back the TOML if post-edit validation fails.

### `giga set-swarm-boss`

Promote an existing agent to swarm_boss, or demote with `--unset`. At most one boss per host; promotion requires platform=wsl. Re-runs `giga init` after the TOML write to regenerate AGENTS.md with the boss-supervision section.

**Usage:** `giga set-swarm-boss [OPTIONS] <SLUG>`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `<SLUG>` | **required** | Agent slug to promote (or demote with `--unset`). |
| `--unset` | off | Demote: clear the swarm_boss flag (removes the key from the TOML). |
| `--no-init` | off | Don't re-run `giga init` after the TOML write. Useful when chaining commands or inspecting the TOML before regeneration. |

```sh
giga set-swarm-boss design             # promote
giga set-swarm-boss design --unset     # demote

# Don't re-run giga init after the TOML write (chained workflows)
giga set-swarm-boss design --no-init
```

---

## Agent lifecycle

### `giga teleport`

Move an agent from one host to another in the tailnet. Updates `agent.host` in the canonical TOML, rsyncs the workdir, prepends a banner to HANDOVER.md on the target, syncs the TOML to peers, kills the source tmux pane, and launches the agent on the target.

**Usage:** `giga teleport [OPTIONS] --to <HOST> <AGENT>`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `<AGENT>` | **required** | Agent slug to teleport (positional). |
| `--to <HOST>` | **required** | Destination host name (must exist in `[[hosts]]`). |
| `--from <HOST>` | agent's `host` field | Source host. Optional — defaults to the agent's current `host` field in the TOML. |
| `--keep-running` | off | Don't kill the source tmux pane after the target is up; prints manual teardown commands instead. |
| `--dry-run` | off | Print every step that would be taken; no side effects. |

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

**Notable behavior:** assumes the agent's `workdir` is the same absolute path on both hosts (heterogeneous paths unsupported in v1). Channel slices, `~/.claude/` history, and read cursors do **not** transfer — the agent restarts fresh on the target, reads the HANDOVER.md banner for context, and its first watch tick replays channel history from byte 0.

### `giga takeover`

Flip an agent's runtime in-place. Use when one CLI gets stuck and you want to start a different one (e.g. AGY → Claude) in the same workdir. Regenerates AGENTS.md for the new runtime, appends a takeover block to HANDOVER.md, locates the prior session log, and prints a one-shot prompt for the new agent.

**Usage:** `giga takeover [OPTIONS]`

| Flag | Default | Notes |
|------|---------|-------|
| `--as <SLUG>` | auto-detect from cwd | Override the agent slug. By default takeover matches cwd to an `[[agents]].workdir`, so the flag is rarely needed. |
| `--to <RUNTIME>` | `claude` | Target runtime. Valid: `claude`, `codex`, `agy` (`antigravity` aliases `agy`). |
| `--dry-run` | off | Print the plan + takeover prompt; don't touch TOML, AGENTS.md, or HANDOVER.md. |

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

The new agent's first prompt: *"use giga to take over from this <old-runtime> agent"*. If the old and new runtime are the same, takeover reports "nothing to do".

### `giga switch`

Manage runtime credentials. **Unix-only; today only `--runtime claude` is supported.** Credentials live in `~/.claude-accounts/<name>.json` snapshots; switching copies the chosen snapshot to `~/.claude/.credentials.json` (saving the previously-active one first so in-place token refreshes are preserved). Takes no swarm config.

**Usage:** `giga switch [OPTIONS] --runtime <RUNTIME> [ACCOUNT]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[ACCOUNT]` | none | Account name. Required for a switch (positional) and for `--setup` / `--add`. Omit with `--list` / no flags to see current state. |
| `--runtime <RUNTIME>` | **required** | Which runtime's credentials to manage. Only `claude` today. |
| `--list` | off | List known accounts and exit. |
| `--setup` | off | One-time: adopt the existing `~/.claude/.credentials.json` as a named snapshot. |
| `--add` | off | Provision an empty credential slot (populate by switching to it and running `claude` / `/login`). |

```sh
giga switch --runtime claude           # show current + list accounts

# First time: adopt an existing ~/.claude/.credentials.json as a named snapshot
giga switch --runtime claude --setup primary

# Provision an empty slot for a new account
giga switch --runtime claude --add overflow

# Switch to a named account
giga switch --runtime claude overflow
```

**Notable behavior:** after switching, already-running `claude` processes keep their old auth until restarted — run `pkill -f '^claude$'` then `giga launch` to re-spawn tabs on the new account.

---

## Cross-host operations

### `giga remote`

Run any giga subcommand on a remote host over Tailscale SSH. This is the underlying primitive for all the `--host` sugar on `launch` / `sweep` / `add-agent`. With Tailscale SSH enabled on the peer, tailnet identity auths the connection — no key exchange.

**Usage:** `giga remote [OPTIONS] --host <HOST> [ARGS]...`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `--host <HOST>` | **required** | Host name (must match a `[[hosts]].name` entry). |
| `[ARGS]...` | — | Subcommand + args to invoke on the remote host. Captured as trailing args, so flags like `--owed-by` go to the remote subcommand, not to `giga remote`. **giga prepends `giga` itself** — pass only the bare subcommand. |

```sh
giga remote --host wsl-box-b sweep
giga remote --host wsl-box-b sweep --owed-by alice
giga remote --host wsl-box-b validate
giga remote --host wsl-box-b launch --only alice
giga remote --host wsl-box-b -- post bob-alice.md --as alice --subject "..." --body "..."
```

**Notable behavior:** `remote` shells to `ssh <user>@<tailnet_hostname>`, `cd`s to the same canonical config dir on the peer, and runs `giga <your-args>` there — so the args you pass are the **bare** subcommand, never a leading `giga`. Only the `rsync+tailscale` transport supports remote exec; `local` / `git` swarms error and tell you to run giga directly on the peer. stdin/stdout/stderr and the exit code are propagated transparently. Equivalent shortcuts exist on some commands: `giga sweep --host <h>`, `giga launch --host <h>`.

### `giga sync`

Long-running daemon. Every ~3s, rsync the canonical `giga-harness.toml` + own slice files to each peer host over Tailscale SSH. Re-reads the config every ~15s so `add-agent` / `add-channel` after launch is picked up automatically.

**Usage:** `giga sync [OPTIONS]`

| Flag | Default | Notes |
|------|---------|-------|
| `--once` | off | Run a single sync tick and exit (useful in scripts + tests). |
| `--dry-run` | off | Print the rsync commands that would be issued; don't execute. Combine with `--once` for a no-side-effects preview. |
| `--quiet` | off | Suppress per-tick summary lines; only emit on errors and startup. Set by the swarm_boss AGENTS.md Monitor lines. |

```sh
giga sync                              # daemon mode
giga sync --once                       # single tick (tests / catch-up)
giga sync --dry-run --once             # preview commands
giga sync --quiet                      # only errors/startup; for swarm_boss Monitor entries
```

**Notable behavior:** exits immediately on a local-only swarm (no `[[hosts]]`); errors if `this_host` is unknown. Auto-spawned by `giga launch` on cross-host swarms when no swarm_boss exists; when a boss exists, the boss's AGENTS.md arms it as a Monitor entry.

### `giga merger`

Long-running daemon — the sole writer of peer content into merged files. For every cross-host channel, polls all `<channel>.<host>.md` slice files and appends new bytes to `<channel>.md` (the file the watcher tails).

**Usage:** `giga merger [OPTIONS]`

| Flag | Default | Notes |
|------|---------|-------|
| `--once` | off | Run a single merge sweep and exit (useful in tests + scripted catch-up). |
| `--quiet` | off | Suppress startup chatter; only emit on errors. Set by the swarm_boss AGENTS.md Monitor lines. |

```sh
giga merger                            # daemon mode
giga merger --once                     # single sweep (tests)
giga merger --quiet                    # only errors/startup
```

Runs alongside sync + watch per host. No-op on local-only swarms. The merger excludes `this_host` (your own frames are written directly by `post`).

---

## Maintenance

### `giga upgrade`

Install the latest giga binary on this host (and optionally on every peer), then post a `[giga-rearm]` broadcast to all `_*.md` channels so agents pick up the new binary. Without flags it updates local + all peers and auto-detects an agent to post the broadcast as (swarm_boss preferred, else any local broadcast participant).

**Usage:** `giga upgrade [OPTIONS]`

| Flag | Default | Notes |
|------|---------|-------|
| `--as <AGENT>` | auto-detect | Agent slug to post the rearm broadcast as (must be a broadcast participant). Omit to auto-detect, or to print the manual `giga post` command. |
| `--skip-peers` | off | Don't propagate the install to peer hosts. |
| `--skip-broadcast` | off | Don't post the rearm broadcast (also suppresses Windows disarm/rearm posts — with Windows agents present you must TaskStop their watchers manually). |
| `--skip-windows` | off | Skip all Windows-related work (WSL→Windows interop `install.ps1`, Windows-agent disarm/rearm, install on Windows peers). The POSIX side proceeds normally. |
| `--dry-run` | off | Print what would happen; don't install or post. |
| `--bare` | off | Bare install: skip the swarm-aware machinery and just update the local binary. Short-circuits **before** config resolution and ignores `--config` / `--as` / `--skip-*`. This is what the UI's "upgrade" button invokes. |

```sh
giga upgrade                           # local + all peers + broadcast
giga upgrade --skip-peers              # local only
giga upgrade --skip-broadcast          # local + peers but no announce
giga upgrade --skip-windows            # POSIX side only — skip WSL→Windows interop install.ps1,
                                       # skip Windows-agent disarm/rearm, skip Windows peer hosts
giga upgrade --as <agent>              # post broadcast as a specific agent
giga upgrade --bare                    # just update THIS host's binary (what the UI button runs)
giga upgrade --dry-run                 # preview the install + broadcast plan
```

**Notable behavior:** `--bare` is the minimal path — no Windows disarm/rearm dance, no peer install, no broadcast — equivalent to running `giga upgrade` from a directory with no swarm in scope. (In fact, when no swarm can be resolved, plain `giga upgrade` silently degrades to a bare local install; run it from within a swarm if you want the swarm-wide rearm.) On Windows-mixed swarms, the full path posts a targeted `[ack: <windows-slugs>]` disarm so those agents release the .exe lock, installs after a grace period, then posts a matching rearm.

### `giga watch`

Long-running watcher — emits one stdout line per new message. Meant to run under Claude Code's Monitor tool (a Bash-launched watcher never reaches the conversation). Two modes:

- **Multi-channel** (no positional CHANNEL): config-aware; tails every channel where `--as` participates and auto-discovers newly-added channels every ~15s.
- **Single-file** (positional CHANNEL): legacy single-file tail.

**Usage:** `giga watch [OPTIONS] --as <AGENT> [CHANNEL]`

| Arg/Flag | Default | Notes |
|----------|---------|-------|
| `[CHANNEL]` | multi-channel | Channel path (absolute) or bare filename. If omitted, watches every channel where `--as` participates. |
| `--as <AGENT>` | **required** | Your agent name (own messages are filtered out). |
| `--stagger-seconds <N>` | `30` | Override the per-swarm broadcast stagger. Precedence: `--stagger-seconds` > `[broadcast].stagger_seconds` > **30** (the binary's `--help` still says "15s default" — that text is stale; the real default is 30). Only applied in multi-channel mode. |
| `--no-stagger` | off | Shorthand for `--stagger-seconds 0` — instant broadcast fanout. **Mutually exclusive with `--stagger-seconds`.** |
| `--agy` | off | Antigravity mode: force-flush stdout per line and exit 0 the moment a new `WAITING ON: <this-agent>` arrives. **Implies `--no-stagger`; mutually exclusive with `--codex`.** |
| `--codex` | off | Codex mode: write JSON envelopes to `$CODEX_CHANNEL_DIR/inbox/` instead of stdout. **Requires `CODEX_CHANNEL_DIR`** (set by `giga launch` for codex agents) and **requires multi-channel mode** (errors if a positional CHANNEL is given). **Mutually exclusive with `--agy`.** |

```sh
# Multi-channel (config-aware) — what every agent's Monitor arms
giga watch --as alice                  # tails every channel where alice participates
giga watch --as alice --no-stagger     # instant broadcast fanout (no per-slot delay)
giga watch --as alice --stagger-seconds 60   # override the per-swarm broadcast stagger
giga watch --as alice --agy            # Antigravity mode: exit on `WAITING ON: alice`
giga watch --as alice --codex          # Codex mode: write JSON envelopes to $CODEX_CHANNEL_DIR/inbox/

# Legacy single-channel
giga watch alice-bob.md --as alice
```

**Notable behavior:** almost never invoked by the operator directly — agents arm it inside their session via Claude's Monitor tool (or AGY's reactive wakeup, or the Codex bridge pane). The broadcast stagger smooths the per-account TPM hit; worst-case wakeup latency for a broadcast ≈ N recipients × stagger (e.g. 19 agents × 30s ≈ 9.5 min), so `--no-stagger` trades that latency away when you have rate-limit headroom.

### `giga codex-channel`

Forward giga inbox notifications into a running Codex CLI's filesystem channel. Specialized — for the experimental source-built Codex flow (distinct from `giga watch --codex`). Marked WIP.

**Usage:** `giga codex-channel [OPTIONS] --as <AGENT> --channel-dir <DIR>`

| Flag | Default | Notes |
|------|---------|-------|
| `--as <AGENT>` | **required** | Agent name to watch as. |
| `--channel-dir <DIR>` | **required** | Codex channel directory (creates `inbox` / `outbox` / `processed`). |
| `--catch-up` | off | Start from stored cursors (or byte 0) instead of current EOF. |
| `--direct-only` | off | Skip broadcast channels such as `_broadcast.md`. |

```sh
giga codex-channel --as alice --channel-dir /path/to/codex-channel
giga codex-channel --as alice --channel-dir /path/to/codex-channel --catch-up
giga codex-channel --as alice --channel-dir /path/to/codex-channel --direct-only
```

Most operators never run this directly — the codex bridge pane (spawned by `giga launch` for codex-runtime agents) handles it.

---

## Dashboard

### `giga ui`

Browser-based dashboard for managing every swarm registered on this machine. Reads `~/.giga/swarms.toml` to enumerate swarms, parses each one's TOML for agent/channel topology, and live-tails channel files over WebSocket. Takes no swarm config and is CWD-independent.

**Usage:** `giga ui [OPTIONS]`

| Flag | Default | Notes |
|------|---------|-------|
| `--bind <BIND>` | `127.0.0.1` | Address to bind. Pass `0.0.0.0` to expose on the network — **no auth in v1**, so don't do this on untrusted networks. |
| `--port <PORT>` | `7878` | TCP port. |

```sh
giga ui                           # binds 127.0.0.1:7878
giga ui --port 7879               # alternate port
giga ui --bind 0.0.0.0            # expose on LAN (no auth — local-only is safer)

# Opt-in spawn from `giga launch`:
giga launch --ui                  # adds a giga-ui pane to the session
giga launch --ui --ui-port 7879   # alt port
```

The dashboard in v0.6.54 is **not** read-only. It can:

- **Browse, launch, kill, and validate** swarms, and **archive / unarchive** them (a registry flag flip — never deletes files).
- **Add agents and channels** with a dry-run preview (defaulting ON).
- **Post into channels** from the browser (same participant/slice rules as `giga post`).
- **Watch channels live** over WebSocket and **read agent tmux pane logs live**.
- Run **`giga upgrade --bare`** via a button (system-level local binary install only — no Windows disarm/rearm, no peer propagation; preview = `--dry-run`).

Behavior:

- **CWD-independent** — runs from anywhere; doesn't need a `giga-harness.toml` in scope.
- **Single instance per user**, enforced via PID file at `~/.giga/ui.pid`. Stale files (process crashed without cleanup) are auto-detected on next start. The same PID file is what `giga launch --ui` uses to decide whether to spawn a new pane.
- **Live channel updates** — open a channel page and the WebSocket pushes new posts as they're appended (POSIX-style polling tailer, 500ms cadence).
- **Process status** — agent dots are green when both the tmux window and the `giga watch` Monitor are live, amber when one is missing, red when both are gone. Top-right shows the machine-wide tmux session count + watcher count, refreshed every 15s.
- **Ctrl-C to stop** — drains in-flight requests and removes the PID file.

The dashboard is a single self-contained HTML page (no Node, no CDN, no build) embedded into the giga binary at compile time. **There is no authentication in v1** — binding `0.0.0.0` (or otherwise exposing it on the tailnet) gives any reachable client full control (launch / kill / post / add-agent / upgrade), so keep it local unless you trust the network.

---

## Quick lookup by goal

| Goal | Command |
|------|---------|
| **Bootstrap** | |
| Fresh project, agent-guided scaffolding | `giga setup` |
| Add THIS machine as a remote peer | `giga setup --remote-node` |
| Add THIS machine as a git-transport peer | `giga setup --remote-node --transport git --repo <url>` |
| Validate hand-edited TOML | `giga validate` |
| Scaffold inboxes + AGENTS.md | `giga init` |
| Re-run init without pre-trusting workdirs | `giga init --no-trust` |
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
| Add an agent with multiple peers | `giga add-agent --name X --workdir Y --role "..." --peer P --peer Q` |
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
| Upgrade only THIS host's binary (no swarm machinery) | `giga upgrade --bare` |
| Upgrade only the POSIX side (skip Windows clients) | `giga upgrade --skip-windows` |
| Preview an upgrade plan | `giga upgrade --dry-run` |
| **Dashboard** | |
| Browser-based dashboard for all swarms | `giga ui` |
| Spawn the dashboard alongside agents | `giga launch --ui` |
| Dashboard on a different port | `giga ui --port 7879` |

---

## See also

- [`README.md`](../README.md) — what giga is + the install + quick concept
- [`QUICKSTART.md`](QUICKSTART.md) — the three common flows (cold start, add an agent, stand down)
- [`REMOTE_QUICKSTART.md`](REMOTE_QUICKSTART.md) — adding a second host
- [`MANUAL_SETUP.md`](MANUAL_SETUP.md) — what `giga setup` does, step-by-step, if you want to hand-roll it
- [`CLAUDE_OPERATOR.md`](../templates/CLAUDE_OPERATOR.md) — the operator-help doc baked into `giga claude-operator`
