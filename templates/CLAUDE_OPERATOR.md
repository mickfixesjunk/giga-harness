# giga-harness operator reference (for Claude)

You are operating a `giga` swarm (binary `giga`, v0.6.54) — N parallel AI agents that coordinate via append-only Markdown channel files. This doc teaches you the command surface so you can do swarm-ops without me dictating every flag. It is baked into the binary and printed by `giga claude-operator`, so treat `giga <subcommand> --help` as the always-available authority if anything here looks out of date.

## Mental model (60 seconds)

- A **swarm** is one TOML file (`giga-harness.toml`) describing **agents** + **channels**. By convention it lives under `~/.giga/configs/<name>/` (where `giga setup` scaffolds it), but giga resolves any registered config via the registry (`~/.giga/swarms.toml`) or a walk up the current directory's ancestors — a swarm TOML can live anywhere.
- An **agent** is a Claude (or Codex / Antigravity) session with its own workdir + an `AGENTS.md` giga generates. Each agent runs a `giga watch --as <slug>` watcher that tails every channel they participate in. `AGENTS.md` is **re-rendered on every `init`/`launch`** — workdir edits are overwritten; persistent changes go in the source `claudemd_template` (`agents/<slug>.md`).
- A **channel** is an append-only `.md` file in the inbox dir. Messages are header-blocks: `===\n[<sender>] <subject> — <UTC ts>\n===\n\n<body>\n\n<footer>\n===`. The header separator is intentionally an em-dash (` — `, U+2014). The timestamp is UTC ISO-8601 ending in `Z` (e.g. `2026-05-28T14:30:00Z`). The footer is either `WAITING ON: <agent> (<what>)` or `(Informational, no response required.)`.
- A swarm can be **single-host** (default — all agents on one machine, polling watcher on local files) or **multi-host** (agents on multiple machines on a tailnet — per-host slice files `<channel>.<host>.md` rsync'd via Tailscale SSH; a local merger appends incoming peer bytes to the watched merged file). The watcher doesn't know or care which case is which.
- In multi-host mode, `giga post` on a cross-host channel **dual-writes** the frame to the slice (for sync to ship to peers) AND to the merged file (so local watchers see it immediately, independent of merger liveness). Adding one remote agent doesn't disrupt local-to-local comms.
- **Strict validation in multi-host swarms:** every `[[agents]]` block must declare `host = "<name>"` explicitly. The pre-strict-validation fallback to `this_host` silently misrouted channels.
- **`this_host.local.toml`:** per-host identity file (single key `this_host = "<host-name>"`). The `*.local.toml` suffix is the convention for "host-private, never rsync between hosts" files. The legacy `this_host.toml` name is also accepted at load.

## Core commands (the ones you'll use 90% of the time)

| Command | What it does |
|---|---|
| `giga validate [config]` | TOML cross-check. No side effects. Run before/after edits. |
| `giga init [config]` | Scaffold inbox files + per-agent `AGENTS.md` (idempotent). Host-aware in multi-host swarms. Uses the config path literally — run from the swarm dir or pass an explicit path. |
| `giga launch [config] [--host H] [--only A,B]` | Open terminals (tmux/wt/Terminal.app). `--host` SSHes to a peer + launches there. `--only` brings up specific agents incrementally. Auto-spawns `giga sync` + `giga merger` panes on multi-host swarms (unless a `swarm_boss` hosts them). `--ui` also spawns a `giga ui` dashboard pane. |
| `giga post <channel> --as <agent> --subject "…" --body "…"` | Append a properly-formatted message. Add `--waiting-on <peer>` (optionally `--needs "<hint>"`) for non-informational. |
| `giga sweep [config] [--host H] [--owed-by A]` | Last-message-per-channel + open `WAITING ON` tags. `--host` runs on a peer. |
| `giga watch --as <agent>` | Long-running notification stream. **Run under Claude Code's `Monitor` tool with `persistent: true`. Not via Bash** — Monitor delivers events into the agent's conversation; a Bash-backgrounded watch never surfaces stdout. |
| `giga hosts` | List the swarm's hosts + which agents live where + which one is `this_host`. Pure read. |
| `giga ui [--bind 127.0.0.1] [--port 7878]` | Browser dashboard for every registered swarm (browse/launch/kill/validate/post/add-agent/add-channel/upgrade, live channel + pane tailing). Localhost-only by default with **no auth** — never `--bind 0.0.0.0` on an untrusted network. |

### Scaffolding

| Command | What it does |
|---|---|
| `giga add-agent --name X --workdir Y --role "…" --peer Z [--host H]` | Scaffold a new agent — appends `[[agents]]` + per-peer `[[channels]]` + the slug to broadcast channels + writes a stub template. `--host` makes it cross-host: auto-bootstraps the peer + scaffolds the workdir remotely. Uses `--config` literally. |
| `giga add-channel --participants A,B` | Append a new bilateral channel between two existing agents (exactly two). On a multi-host swarm, run `giga sync --once` so peers pick it up. |
| `giga add-host --name N --tailnet-hostname FQDN [--ssh-user U] [--remote-config-dir P] [--this-host-name M]` | Register a new tailnet peer + auto-bootstrap. **First-host migration:** when this is the FIRST host being added (local-only → multi-host), atomically also registers the LOCAL host (defaults to `$HOSTNAME`; override with `--this-host-name`), sets `host = "<local>"` on every existing agent, and writes `this_host.local.toml`. |

### Multi-host daemons & remote

| Command | What it does |
|---|---|
| `giga sync [--once] [--dry-run] [--quiet]` | Push-only sync daemon: rsyncs your own slices + the canonical TOML to each peer (~3s loop). No-op on local-only swarms. `--once` is the diagnostic/verification form (single tick then exit); `--dry-run` previews the rsync commands (combine with `--once`). |
| `giga merger [--once] [--quiet]` | Merger daemon: appends new peer-slice bytes into the local merged `<channel>.md` the watcher tails. Sole writer of peer content. No-op on local-only swarms. `--once` runs a single merge sweep — use it to verify cross-host delivery. |
| `giga remote --host H <subcommand> [flags]` | Run any giga subcommand on a peer over SSH. Backbone of all `--host` sugar. |

### Lifecycle

| Command | What it does |
|---|---|
| `giga upgrade [--as A] [--skip-peers] [--skip-broadcast] [--dry-run] [--bare]` | Install the latest binary on this host (+ every peer), then broadcast a re-arm so watchers pick up the new binary. See Recipe 6. |
| `giga teleport <agent> --to <host> [--from <host>] [--keep-running] [--dry-run]` | Move an agent (workdir + `agent.host`) to another tailnet host. See Recipe 7. |
| `giga takeover [--as <slug>] [--to <runtime>] [--dry-run]` | Flip an agent's runtime in place (claude/codex/agy). See Recipe 9. |
| `giga set-swarm-boss <slug> [--unset] [--no-init]` | Promote (or `--unset` to demote) the swarm_boss. See "swarm_boss role". |
| `giga switch --runtime claude [--setup\|--add\|--list] [ACCOUNT]` | Rotate which Claude account credentials are active (Unix-only). See Recipe 10. |

## The `--host <H>` pattern

Every multi-host operation is sugar over `giga remote --host H <subcommand>`. The `--host` flag on `add-agent`, `sweep`, `launch` exists so you don't have to type `giga remote` manually. The long form helps when you need a subcommand without a dedicated `--host` flag, e.g. `giga remote --host wsl-b init`.

The trailing args after the host are captured verbatim — including hyphenated flags — so the bare form works: `giga remote --host wsl-b sweep --owed-by alice`. You only need an explicit `--` separator (or to rely on that trailing capture) to disambiguate the literal flag `--config`, which `giga remote` also defines for itself; in that one case put the remote subcommand's `--config` after a `--`.

## Broadcast channels & fanout addressing

Only `_*.md` channels get special fanout handling. A broadcast post fans out to recipients **staggered** by `[broadcast].stagger_seconds` (default **30s** per recipient slot; `0` = instant) to smooth the per-account Anthropic TPM hit. The **subject prefix** controls who actually wakes:

- **no prefix** / `[all]` — every participant is notified (staggered). This is the default.
- `[ack: a,b,c]` — only the named agents fire a notification; others see the message in the channel file but stay silent. Synthesized by `giga post --to a,b,c`.
- `[fyi]` — **no notification fires for anyone** (zero LLM cost); each receiver archives it to `~/.giga/fyi-archive.<agent>.log`. Synthesized by `giga post --fyi`. Mutually exclusive with `--to`.
- `[giga-rearm]` — **reserved** for `giga upgrade`; triggers the watcher's silent self-rearm. Don't post this by hand.

Override the stagger per-watcher with `giga watch --stagger-seconds N` or `--no-stagger` (the `giga watch` help text says "15s default" — that's stale; the real default is 30).

## swarm_boss role (optional, multi-host)

One agent per host can be the `swarm_boss`. Instead of separate tmux daemon panes, it runs the `giga sync` + `giga merger` daemons as **`Monitor` entries in its `AGENTS.md`** (and supervises worker compaction when smart-compaction is on). The boss section only materializes when the swarm has `[[hosts]]`. Constraints: **at most one per host**, and it **must be `platform = "wsl"`** (sync + merger are POSIX-only). Promote/demote with:

```sh
giga set-swarm-boss <slug>            # promote (requires platform=wsl)
giga set-swarm-boss <slug> --unset    # demote (removes the flag)
```

It re-runs `giga init` afterward (so the AGENTS.md supervision section is regenerated) unless you pass `--no-init`. Daemons hosted this way die with the agent's session — pick a long-lived agent.

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
                         [--waiting-on <recipient>] [--needs "<what you need>"]
```

The channel may be positional or via `--channel <name>` (exactly one form). If `--waiting-on` is omitted, the footer is `(Informational, no response required.)`; with it, `--needs "<hint>"` appends as `WAITING ON: <recipient> (<hint>)` so the wait is self-describing. Omit `--body` to read the body from stdin until EOF. Convention: end every substantive message with one footer or the other so `giga sweep` is meaningful.

### 5. See what's in flight + who's waiting

```sh
giga sweep                        # this swarm
giga sweep --owed-by <agent>      # filter to channels where <agent> owes a reply
giga sweep --host <peer>          # run sweep on a peer (SSHs there)
giga hosts                        # who lives where
```

### 6. Upgrade the binary across the whole swarm

```sh
giga upgrade --as design          # installs latest on this host + all peers,
                                  # posts a [giga-rearm] broadcast on every _*.md
giga upgrade --dry-run --as design           # preview without running anything
giga upgrade --skip-peers --skip-broadcast   # local-only silent update
giga upgrade --bare                          # update only the local binary, no swarm machinery
```

`giga upgrade` posts a `[giga-rearm]` broadcast. **POSIX watchers self-rearm in place** — same PID, **zero API calls**, no agent action required (the watcher `execve`s itself). Windows agents get an automatic disarm/rearm dance to release the `.exe` lock (briefly TaskStop + re-arm). Auto-detects `--as` (swarm_boss first, then any local broadcast participant); omit it and giga prints the manual `giga post` command if it can't pick one.

### 7. Teleport an agent between hosts

```sh
giga teleport research --to trinity-wsl                  # full one-shot
giga teleport research --to trinity-wsl --dry-run        # preview steps
giga teleport research --to trinity-wsl --keep-running   # don't kill source pane
giga teleport research --to trinity-wsl --from morpheus-wsl  # explicit source override
```

Moves the agent's workdir over tailnet SSH (direct A→B; two-hop via operator fallback), edits `agent.host` in the TOML, prepends a banner to HANDOVER.md so the agent knows it was moved when it boots on the new host, then re-inits + launches on the target and kills the source tmux pane gracefully (SIGTERM + 5s + kill). Past channel slice contributions stay where they were (append-only history, still visible swarm-wide via merge); future posts go to the new host's slice. Conversation history and cursors don't transfer — HANDOVER.md is the migration vehicle, and the first watch tick replays channel history from byte 0.

### 8. Address a broadcast to a subset

```sh
# Only A and B get a notification; other participants see the message in the
# channel file but their watchers stay silent ([ack: alice, bob] prefix).
giga post _broadcast --as design --to alice,bob \
  --subject "scope question" --body "..."

# FYI — no Monitor notification fires for ANYONE. Archived to
# ~/.giga/fyi-archive.<agent>.log per receiver ([fyi] prefix). Zero LLM cost.
giga post _broadcast --as design --fyi \
  --subject "morpheus came online" --body "..."
```

### 9. Convert an agent to a different runtime (takeover)

`giga takeover` flips an agent's runtime (claude / codex / agy; `antigravity` aliases agy) **in place** — no host/slug/role/channel change. It re-renders `AGENTS.md` for the new runtime, prepends a takeover block to HANDOVER.md, and prints a one-shot prompt.

Operator workflow: cd into the agent's workdir, start a fresh CLI of the target runtime, and tell it "use giga to take over from this agent". It runs `giga takeover` **with no flags** — the slug is auto-detected from cwd and `--to` defaults to `claude`. To drive it directly:

```sh
giga takeover --as research --to codex     # flip the 'research' agent to codex
giga takeover --to agy --dry-run           # preview (slug auto-detected from cwd)
```

### 10. Rotate Claude accounts (dodge per-account TPM limits)

`giga switch` flips which Claude account's credentials are active (Unix-only; only `--runtime claude` today). It saves the outgoing account's snapshot back before switching, so silent OAuth refreshes aren't lost.

```sh
giga switch --runtime claude                  # show current + list
giga switch --runtime claude --setup primary  # one-time: adopt current creds as 'primary'
giga switch --runtime claude --add overflow   # provision an empty slot
giga switch --runtime claude overflow         # switch to 'overflow'
```

After a switch, already-running claude processes keep the old auth until restarted: `pkill -f '^claude$'` then `giga launch` so tabs re-spawn on the new account.

## Sharp edges (what to know)

1. **`giga remote` trailing args.** The bare form `giga remote --host H <sub> [flags]` works — hyphenated flags after the host are captured for the remote subcommand. Use a `--` separator only to be explicit or to disambiguate `--config` (which `giga remote` also defines for itself).
2. **Channel files are append-only by convention.** Never edit or delete a slice file (`<channel>.<host>.md`) directly. The merger reads byte deltas; a shrink resets cursor state.
3. **Multi-host swarms need `giga sync` + `giga merger` running per host.** `giga launch` auto-spawns these panes on cross-host swarms. Alternatively, flag one agent per host as `swarm_boss` and the daemons live as `Monitor` lines in that agent's `AGENTS.md`. To debug delivery, run a single tick: `giga sync --once` then `giga merger --once`.
4. **Header separator is an em-dash.** The message header format requires ` — ` (U+2014); bodies may contain any UTF-8. (Only very old pre-fix watchers had a char-boundary panic on multibyte chars — current watchers are char-boundary-safe.)
5. **Subject convention.** Keep subjects short and informative — the watcher's notification line truncates the body, so a clear subject is what an agent sees first. If you stamp a timestamp into a subject by convention, use UTC ending in `Z` to match every other giga timestamp. On `_*.md` channels, remember the leading `[...]` prefix is semantically parsed (see "Broadcast channels & fanout addressing") — don't accidentally collide with `[fyi]`/`[ack: …]`/`[all]`/`[giga-rearm]`.

## Where to read more

- `../README.md` — overview + full subcommand table.
- `QUICKSTART.md`, `COMMAND_REFERENCE.md`, `MANUAL_SETUP.md`, `REMOTE_QUICKSTART.md` — task-specific guides.
- `giga --help`, `giga <subcommand> --help` — per-subcommand flag reference. Always available and version-locked to your binary; trust it over anything here if they disagree.

## If you're an agent (not the human operator)

If you arrived at this doc because your `AGENTS.md` said "run `giga claude-operator` for multi-host ops":
- Use the recipes above directly. Your existing `giga post` muscle memory still works.
- If asked to add an agent: Recipe 1 (local host) or Recipe 2 (peer host).
- If asked who's where: `giga hosts`.
- For anything you're unsure about: `giga <subcommand> --help` always works.
- Don't run `giga claude-operator` from inside this session (it would print this doc again — you already have it).
