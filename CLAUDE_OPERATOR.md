# giga-harness operator reference (for Claude)

You are operating a `giga-harness` swarm — N parallel AI agents that coordinate via append-only Markdown channel files. This doc teaches you the command surface so you can do swarm-ops without me dictating every flag.

## Mental model (60 seconds)

- A **swarm** is one TOML file (`giga-harness.toml`) describing **agents** + **channels**. Lives at `~/.giga/configs/<name>/`.
- An **agent** is a Claude (or other) session with its own workdir + `CLAUDE.md`. Each agent runs a `giga watch --as <slug>` Monitor that tails every channel they participate in.
- A **channel** is an append-only `.md` file in the inbox dir. Messages are header-blocks: `===\n[<sender>] <subject> — <UTC ts>\n===\n\n<body>\n\n<footer>\n===`. The footer is either `WAITING ON: <agent> (<what>)` or `(Informational, no response required.)`.
- A swarm can be **single-host** (default — all agents on one machine, polling watcher on local files) or **multi-host** (agents on multiple machines on a tailnet — per-host slice files `<channel>.<host>.md` rsync'd via Tailscale SSH; a local merger appends incoming peer bytes to the watched merged file). The watcher doesn't know or care which case is which.
- In multi-host mode, `giga post` on a cross-host channel **dual-writes** the frame to the slice (for sync to ship to peers) AND to the merged file (so local watchers see it immediately, independent of merger liveness). Adding one remote agent doesn't disrupt local-to-local comms.
- **Strict validation in multi-host swarms:** every `[[agents]]` block must declare `host = "<name>"` explicitly. The pre-strict-validation fallback to `this_host` silently misrouted channels.
- **`this_host.local.toml`:** per-host identity file. The `*.local.toml` suffix is the convention for "host-private, never rsync between hosts" files. The legacy `this_host.toml` name is also accepted at load.
- **Broadcast fanout limiter:** posts on `_*.md` channels are staggered + filterable to prevent N-agent LLM wake-up storms (Anthropic per-account TPM rate-limit protection). Sender sugar: `giga post --to alice,bob` (only named agents wake) and `giga post --fyi` (zero LLM cost; archived to `~/.giga/fyi-archive.<agent>.log` per receiver). Default 15s stagger between recipient slots; override via `[broadcast].stagger_seconds` in TOML or `giga watch --stagger-seconds N` / `--no-stagger`.
- **`giga upgrade`:** one-shot operator command that installs the latest giga binary on this host, propagates to every peer over `giga remote`, and posts a "please re-arm your watcher" broadcast on every `_*.md` channel. Auto-detects `--as` (swarm_boss first; falls back to any local broadcast participant). Flags: `--skip-peers`, `--skip-broadcast`, `--dry-run`.
- **swarm_boss agent (optional):** one agent per host can be flagged `swarm_boss = true` to host the sync + merger daemons as `Monitor` entries in its CLAUDE.md instead of as separate tmux panes. Three Monitors total instead of one. Daemons die with the agent's session — pick a long-lived agent.

## Core commands (10 you'll use 90% of the time)

| Command | What it does |
|---|---|
| `giga validate [config]` | TOML cross-check. No side effects. Run before/after edits. |
| `giga init [config]` | Scaffold inbox files + per-agent CLAUDE.md (idempotent). Host-aware in multi-host swarms. |
| `giga launch [config] [--host H] [--only A,B]` | Open terminals (tmux/wt/Terminal.app). `--host` SSHes to peer + launches there. `--only` brings up specific agents incrementally. Auto-spawns `giga sync` + `giga merger` panes on multi-host swarms. |
| `giga post <channel> --as <agent> --subject ... --body ...` | Append a properly-formatted message. Add `--waiting-on <peer>` for non-informational. |
| `giga sweep [config] [--host H]` | Last-message-per-channel + open `WAITING ON` tags. `--host` runs on the peer. |
| `giga watch --as <agent>` | Long-running notification stream. Run under Claude Code's `Monitor` tool with `persistent: true`. Not via Bash — Monitor delivers events into the agent's conversation; Bash background doesn't. |
| `giga add-agent --name X --workdir Y --role "..." --peer Z [--host H]` | Scaffold a new agent — appends `[[agents]]` + per-peer `[[channels]]` + writes a stub template. `--host` makes it cross-host: auto-bootstraps the peer + scaffolds the workdir remotely. |
| `giga add-channel --participants A,B` | Append a new bilateral channel between two existing agents. |
| `giga add-host --name N --tailnet-hostname FQDN [--ssh-user U] [--remote-config-dir P] [--this-host-name M]` | Register a new tailnet peer + auto-bootstrap. First-host migration: when this is the FIRST host being added (local-only → multi-host), atomically also registers the LOCAL host (defaults to `$HOSTNAME`; override with `--this-host-name`), sets `host = "<local>"` on every existing agent, and writes `this_host.local.toml`. |
| `giga remote --host H -- <subcommand> [args]` | Run any giga subcommand on a peer over SSH. **Put `--config` before the `--` if you need it.** Backbone of all `--host` flags. |
| `giga hosts` | List the swarm's hosts + which agents live where + which one is `this_host`. Pure read. |

## The `--host <H>` pattern

Every multi-host operation is sugar over `giga remote --host H -- <subcommand>`. The `--host` flag on `add-agent`, `sweep`, `launch` exists so you don't have to type `giga remote` manually. Knowing the long form helps when you need to run a subcommand without a dedicated `--host` flag (e.g., `giga remote --host wsl-b -- init`).

## Recipes (copy-pasteable)

### 1. Spin up a new agent on this host

```sh
giga add-agent --name <slug> \
               --workdir ~/.giga/configs/<swarm>/workdirs/<slug> \
               --role "what this agent does" \
               --peer <existing-agent>
# Then bring up the tab:
giga launch --only <slug>
```

### 2. Spin up a new agent on a peer host (one shot)

```sh
# Assumes peer already registered via `giga add-host` and reachable.
giga add-agent --host <peer-name> \
               --name <slug> \
               --workdir /home/<peer-user>/.giga/configs/<swarm>/workdirs/<slug> \
               --role "..." \
               --peer <existing-local-agent>
# add-agent auto-pushes TOML + runs `giga init` on the peer. Then:
giga launch --host <peer-name> --only <slug>
```

### 3. Add a new peer host to the swarm

```sh
# 1) On the new host (assumed giga binary installed there):
giga setup --remote-node       # installs tailscale + rsync + tailnet auth

# 2) On operator host:
giga add-host --name <peer> \
              --tailnet-hostname <peer>.tail....ts.net \
              --ssh-user <peer-user> \
              --remote-config-dir /home/<peer-user>/.giga/configs/<swarm>
```

### 4. Send a message between two agents (programmatic, not via Claude)

```sh
giga post <channel-name> --as <sender> \
                         --subject "<short>" \
                         --body "<the message>" \
                         [--waiting-on <recipient>]
```

If `--waiting-on` is omitted, the footer is `(Informational, no response required.)`. Convention: end every substantive message with one or the other so `giga sweep` is meaningful.

### 5. See what's in flight + who's waiting

```sh
giga sweep                        # this swarm
giga sweep --owed-by <agent>      # filter to channels where <agent> owes a reply
giga sweep --host <peer>          # run sweep on a peer (SSHs there)
giga hosts                        # who lives where
```

### 6. Upgrade the binary across the whole swarm (v0.4.1)

```sh
giga upgrade --as design          # installs latest on this host + all peers,
                                  # posts rearm broadcast on every _*.md
giga upgrade --dry-run --as design   # preview without running anything
giga upgrade --skip-peers --skip-broadcast   # local-only silent update
```

Agents see the broadcast, do `TaskStop` on their `giga inbox watcher` Monitor, re-arm from `CLAUDE.md`, new binary loaded. Broadcast itself uses v0.4.0 stagger smoothing automatically.

### 7. Teleport an agent between hosts

```sh
giga teleport research --to trinity-wsl                  # full one-shot
giga teleport research --to trinity-wsl --dry-run        # preview steps
giga teleport research --to trinity-wsl --keep-running   # don't kill source pane
giga teleport research --to trinity-wsl --from morpheus-wsl  # explicit source override
```

Moves the agent's workdir over tailnet SSH (direct A→B; two-hop via operator fallback). Prepends a banner to HANDOVER.md so the agent knows it was moved when it boots on the new host. Kills the source tmux pane gracefully (SIGTERM + 5s + kill). Past channel slice contributions stay where they were (append-only history); future posts go to the new host's slice. Conversation history doesn't transfer — HANDOVER.md is the migration vehicle.

### 8. Address a broadcast to a subset

```sh
# Only A and B get a notification; other participants see the message in the
# channel file but their watchers stay silent.
giga post _broadcast --as design --to alice,bob \
  --subject "scope question" --body "..."

# FYI — no Monitor notification fires for ANYONE. Archived to
# ~/.giga/fyi-archive.<agent>.log per receiver.
giga post _broadcast --as design --fyi \
  --subject "morpheus came online" --body "..."
```

## Sharp edges (what to know)

1. **`giga remote --host H -- <sub> --flag X`** — flags AFTER the `--` go to the remote subcommand; flags before `--` go to giga remote. Without the `--`, clap captures `--config` as giga remote's own arg and the remote sub sees the wrong (or no) config.
2. **Channel files are append-only by convention.** Never edit or delete a slice file (`<channel>.<host>.md`) directly. The merger reads byte deltas; a shrink corrupts cursor state.
3. **Multi-host swarms need `giga sync` + `giga merger` running per host.** `giga launch` auto-spawns these panes on cross-host swarms. Alternative (v0.3.6): flag one agent per host with `swarm_boss = true` and the daemons live as `Monitor` lines in that agent's CLAUDE.md (Monitor-hosted, three Monitors total including the inbox watcher). For ad-hoc runs, start them yourself: `nohup giga sync > /tmp/sync.log 2>&1 &` + similar for merger.
4. **ASCII only in subject + body.** Multibyte chars (em-dash, smart quotes) can crash older `giga watch` versions. Stick to ASCII in posts.
5. **Post subject prefix:** convention is to start the subject with `[<agent> YYYY-MM-DD HH:MM PST]` so the inbox watcher's notification line (which truncates the body) shows enough context.

## Where to read more

- `README.md` — overview, full subcommand table, multi-host section
- `REMOTE_QUICKSTART.md` — operator runbook for the 2-shot bootstrap + troubleshooting
- `REMOTE_DESIGN.md` — architecture (slice-and-merge, transport, config schema)
- `REMOTE_DUAL_WRITE_DESIGN.md` — v0.3.5 dual-write architecture; why local visibility no longer depends on merger liveness
- `SWARM_BOSS_DESIGN.md` — v0.3.6 swarm_boss agent role; daemons as Monitors instead of tmux panes
- `giga --help`, `giga <subcommand> --help` — per-subcommand flag reference

## If you're an agent (not the human operator)

If you arrived at this doc because your CLAUDE.md said "run `giga claude-operator` for multi-host ops":
- Use the recipes above directly. Your existing `giga post` muscle memory still works.
- If asked to add an agent: §1 (local host) or §2 (peer host) above.
- If asked who's where: `giga hosts`.
- For anything you're unsure about: `giga <subcommand> --help` always works.
- Don't run `giga claude-operator` from inside this session (it would print this doc again — you already have it).
