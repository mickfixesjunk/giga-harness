# giga-harness — Architecture

> Root architecture document for **giga-harness** (binary `giga`, crate `giga-harness`, v0.6.55).
> For operator-facing usage start with the [README](README.md); for design rationale see [`design/`](design/).

---

## 1. What giga is

**giga is a manual multi-agent coordination harness.** It is a small, synchronous Rust CLI (`giga`) that spawns N parallel AI coding agents — Claude Code, Codex, or Antigravity ("agy") — and wires them up so they can coordinate by **appending plain-text messages to shared Markdown files**. There is no database, no message bus, no MCP server, and no LLM anywhere in the coordination loop. The entire substrate is plain-text files in a shared directory plus one watcher process per agent.

The mental model is deliberately low-tech and inspectable:

- **One terminal tab per agent.** Each agent runs in its own workdir, opened in its own terminal (a Windows Terminal tab, a tmux window, or a macOS Terminal.app window), titled with the agent's slug.
- **A giga-generated `AGENTS.md` per agent.** This is the agent's operating manual: who it is, who its peers are, how to watch its inbox, and the message conventions to follow. It is re-rendered on every `init`/`launch`, so durable edits go to the source template, not the workdir copy.
- **Shared channel files.** Agents talk by appending convention-formatted message frames to Markdown files in a shared inbox directory. A bilateral channel `<a>-<b>.md` joins exactly two agents; a broadcast channel `_*.md` fans out to many.
- **A watcher per agent.** Each agent runs a long-running `giga watch` that tails every channel it participates in and surfaces new messages into the agent's session (delivery differs per runtime — see §5).

Agents can run on **one host** (the default, and the fast path) or across **multiple hosts** via a pluggable transport (local filesystem no-op, a shared git state-repo, or rsync over a Tailscale tailnet). Cross-host swarms use a **slice-and-merge** model (§2) so that the single-host file-append semantics are preserved everywhere.

Because the coordination state is just text on disk, everything is debuggable with `cat`, `tail`, and `grep`. There is no hidden runtime state to reconcile: a message exists if and only if its bytes are in a channel file. The harness's job is purely to scaffold the agents, route their messages, and keep cursors so nothing is lost and nothing is double-delivered.

---

## 2. The coordination model

### Channels and file-based inboxes

A **channel** is a single Markdown file in a shared **inbox directory** (`paths.wsl_inbox`, default `<config_dir>/inbox`). Each channel enrolls a fixed set of `participants` (agent slugs). Two kinds exist:

- **Bilateral** — exactly two participants, filename convention `<a>-<b>.md` (alphabetical), e.g. `design-code.md`.
- **Broadcast** — filename starts with `_` (e.g. `_broadcast.md`), any number of participants. (`config::is_broadcast_channel` is just `name.starts_with('_') && name.ends_with(".md")`.)

A message is an **append-only frame**. Posting never rewrites history — it only appends bytes to the end of the file. This is the invariant the whole system rests on.

### The `===` header block and the WAITING ON tag

`giga post` writes frames in exactly this shape (`src/post.rs`):

```
===
[<sender>] <subject> — <UTC-ISO8601>
===

<body>

WAITING ON: <agent> (<optional needs hint>)
===
```

- The **header** is delimited by `===` lines and carries `[sender] subject — timestamp` (UTC ISO-8601, e.g. `2026-05-22T10:14:00Z`).
- The **footer** is either `WAITING ON: <agent>` — a reply is *owed* by that agent (the "who owes the next move" tag) — or `(Informational, no response required.)` for an FYI that closes the thread.
- Frames are separated by a blank-line gap (`\n\n`) so they read cleanly when concatenated.

A concrete bilateral exchange in `design-code.md`:

```
===
[design] T2.1 spec ready — 2026-05-22T10:14:00Z
===

Scope agreed: import-from-CSV, no edge-case fanout this phase.

WAITING ON: code (acknowledge + estimate)
===


===
[code] re: T2.1 spec ready — 2026-05-22T10:31:08Z
===

Acked. ~2h. Starting after current bench slot.

(Informational, no response required.)
===
```

`giga post` validates that `--as` and `--waiting-on` are channel participants before writing, then takes an **exclusive file lock** for the append (`append_with_lock`) so concurrent posters never interleave bytes.

### Watching, cursors, and the no-loss invariant

Each agent's `giga watch` tails its channels using a `last_size`/`read_delta` **byte-cursor** pattern: it remembers how many bytes of each file it has already emitted (persisted under `~/.giga` via `src/cursor.rs`), reads only the new suffix, parses out whole frames, filters out the agent's own posts, and emits the rest. The cursor is **persisted only after a successful emit** — so a crash mid-delivery re-delivers the message on restart rather than dropping it. Re-derivation is purely from file content; there is no separate "delivered" database.

### Slice-and-merge (cross-host)

On a multi-host swarm, a single channel file can't be the single point of truth because two hosts would race on appends. giga uses a **single-writer slice model**:

- Each cross-host channel `<channel>.md` is accompanied by per-host **slice files** `<channel>.<host>.md`. **Each host appends only to its own slice** — slices are single-writer by construction, so they're append-only and conflict-free.
- When an agent posts on a cross-host channel, `giga post` **dual-writes** the frame: to its host's slice (for shipping to peers) *and* directly to the local merged `<channel>.md` (so the local watcher sees it immediately, independent of any daemon being alive). See [`design/REMOTE_DUAL_WRITE_DESIGN.md`](design/REMOTE_DUAL_WRITE_DESIGN.md).
- A local **`giga sync`** daemon pushes this host's own slices (and the canonical TOML) to every peer; reception is push-only and symmetric (nobody pulls).
- A local **`giga merger`** daemon polls *peer* slice files and appends their new bytes into the watched merged `<channel>.md`, tracking a per-(channel, host) merge cursor so peer bytes are never merged twice.
- Channels whose participants are *all* on `this_host` (`Config::channel_is_local`) skip the slice path entirely — the single-host direct write stays the fast path.

The merger and sync daemons run per-host. They are spawned either as their own terminal panes by `giga launch`, or armed via Monitor entries inside the host's `swarm_boss` agent's `AGENTS.md` (§5).

### Broadcast fanout and the stagger limiter

Posting on a `_*.md` broadcast channel could wake every participant within a single watcher poll — a synchronous LLM-turn storm and a per-account rate-limit risk. The fanout limiter (`config::parse_broadcast_prefix`, `fanout_delay_seconds`; see [`design/BROADCAST_FANOUT_DESIGN.md`](design/BROADCAST_FANOUT_DESIGN.md)) tames this with subject-line prefixes and a per-agent stagger:

- `[all]` (or no prefix) — fire for every participant, but each watcher delays its notification by `slot × stagger_seconds`, where `slot` is the agent's position in the alphabetically-sorted recipient list. Default stagger is 30s (`[broadcast].stagger_seconds`).
- `[ack: a, b, c]` — only the named agents fire a notification (synthesized by `giga post --to a,b,c`).
- `[fyi]` — informational; receivers archive to a per-agent FYI log instead of firing a notification (zero LLM cost; `giga post --fyi`).
- `[giga-rearm]` — a silent watcher-rebinary signal: the watcher advances its cursor past the message and re-exec's itself to load a fresh binary, without ever waking the agent (used by `giga upgrade`).

### Stale-wait detection (no LLM)

A failure mode of a purely-conventional protocol: the sender posts `WAITING ON: code`, the receiver compacts its context or misses the notification, and by protocol both sides then stay silent forever. `src/stale_wait.rs` closes this hole with a **pure re-derivation** (no LLM, no DB): when a watcher arms (and every `stale_wait_recheck_seconds`, default 60s), it scans each tracked channel for unresolved `WAITING ON: <me>` tags older than the threshold (`[watch].stale_wait_threshold_minutes`, default 30, per-channel overridable) and surfaces one notification per finding. Dedup is by `(channel, sender, tag-timestamp)` so a given stale wait fires at most once. See [`design/STALE_WAITS_NO_LLM_DESIGN.md`](design/STALE_WAITS_NO_LLM_DESIGN.md).

---

## 3. Module map

`src/` is organized into layered subsystem modules with a clean,
one-directional dependency graph: everything builds on a dependency-free
`foundation/` leaf layer; nothing points back into it. `main.rs` is a
~30-line shim (`cli::Cli::parse().command.run()`); the clap schema lives
in `cli.rs` and the dispatch match in `dispatch.rs`.

```
src/
├── main.rs / cli.rs / dispatch.rs   entry shim, CLI schema, dispatch
├── foundation/   leaf layer: frame grammar, byte-tail, locked append,
│                 atomic_io, dirs, paths, proc, ssh, self_invoke, slices,
│                 tailscale, timefmt  (depends on std/external only)
├── config/       schema · load · validate · resolve · broadcast · edit
├── runtime/      Runtime enum + per-runtime claude/codex/agy behavior
├── coordination/ the message substrate: post · merger · sweep ·
│                 stale_wait · cursor · codex_channel · watch/{sink,broadcast}
├── transport/    Transport + RemoteExec traits · local/git/rsync_tailscale
│                 plugs · remote · hosts · setup_remote_node · sync/{plan,
│                 rsync,bootstrap}
├── scaffold/     init (effects) · render (AGENTS.md text) · launch ·
│                 templates · terminal/ (TerminalBackend: wt/tmux/mac/print)
├── mutate/       add_agent · add_channel · add_host · set_swarm_boss ·
│                 peer_bootstrap  (all via config::edit rollback)
├── mobility/     teleport · takeover · upgrade/{installer,windows_rearm}
├── accounts/     switch (credential manager)
├── ui/           the axum dashboard (api/{read,mutate,dto}, ws, server, …)
├── registry.rs   ~/.giga/swarms.toml resolver
├── trust.rs      Claude folder-trust · fs_paths.rs  WSL↔Windows paths
└── validate.rs   `giga validate` presentation
```

| Area / subfolder | Role | README |
|---|---|---|
| `src/foundation/` | Dependency-free leaf layer: the `===`-frame grammar, byte-cursor read, locked + atomic file writes, subprocess/ssh/tailscale substrate, and the giga-self-invocation resolver. | — |
| `src/config/` | TOML schema, load/canonicalize, per-invariant validation, read-side resolvers, broadcast semantics, and the `edit_then_validate_with_rollback` mutation lifecycle. | — |
| `src/coordination/` | The message-passing substrate: post/merger/sweep/stale-wait/cursor plus the `watch` daemon (with the `NotificationSink` trait + `BroadcastPolicy`). | — |
| `src/transport/` | The `Transport`/`RemoteExec` traits + `for_config` factory, the three plugs (`local`/`rsync_tailscale`/`git`), the `sync` daemon, SSH passthrough (`remote`), the hosts roster, and the remote-node installer. | [src/transports/README.md](src/transports/README.md)¹ |
| `src/scaffold/` | `init` (effects) + `render` (AGENTS.md text), `launch` (pane assembly), and the `terminal/` multiplexer backends behind `TerminalBackend`. | — |
| `src/mutate/` / `src/mobility/` / `src/accounts/` | TOML-mutating commands (atomic via `config::edit`); agent mobility (teleport/takeover/upgrade); the credential manager. | — |
| `src/runtime/` | `Runtime` enum + per-runtime (claude/codex/agy) launch/session/snippet behavior. | — |
| `src/ui/` | The `giga ui` dashboard: an axum HTTP + WebSocket server over every registered swarm (the only async/tokio part of the CLI). | [src/ui/README.md](src/ui/README.md) |
| `tests/` | Cargo integration tests (separate binaries) that drive the real `giga` binary as a subprocess: cross-host e2e, swarm chaos, git transport e2e. | [tests/README.md](tests/README.md) |
| `templates/` | Static text/markup baked into the binary via `include_str!`/`include_bytes!`: operator doc, agent stub + partials, per-runtime intros, dashboard HTML. | [templates/README.md](templates/README.md) |
| `examples/` | A minimal working `giga-harness.toml` (two agents, one channel) used as a smoke fixture and copy-paste starting point. | [examples/README.md](examples/README.md) |
| `.github/` | CI (`ci.yml`, with a blocking `cargo clippy -D warnings` gate) and release (`release.yml`) GitHub Actions workflows. | [.github/WORKFLOWS.md](.github/WORKFLOWS.md) |
| `docs/` | Operator-facing walkthroughs: [QUICKSTART](docs/QUICKSTART.md), [MANUAL_SETUP](docs/MANUAL_SETUP.md), [COMMAND_REFERENCE](docs/COMMAND_REFERENCE.md), [REMOTE_QUICKSTART](docs/REMOTE_QUICKSTART.md). | [docs/](docs/) |
| `design/` | Design rationale per subsystem: [REMOTE_DESIGN](design/REMOTE_DESIGN.md), [REMOTE_DUAL_WRITE_DESIGN](design/REMOTE_DUAL_WRITE_DESIGN.md), [TRANSPORT_DESIGN](design/TRANSPORT_DESIGN.md), [BROADCAST_FANOUT_DESIGN](design/BROADCAST_FANOUT_DESIGN.md), [SWARM_BOSS_DESIGN](design/SWARM_BOSS_DESIGN.md), [TELEPORT_DESIGN](design/TELEPORT_DESIGN.md), [STALE_WAITS_NO_LLM_DESIGN](design/STALE_WAITS_NO_LLM_DESIGN.md). | [design/](design/) |

> ¹ The per-directory developer READMEs under `src/` predate the v0.6.x
> modular reorganization (the plug impls now live in `src/transport/`, not
> `src/transports/`, and the substrate under `src/coordination/`); they are
> pending a refresh. This `ARCHITECTURE.md` reflects the current tree.

---

## 4. Command lifecycle

### The typical flow

```
setup  →  validate  →  init  →  launch  →  post / watch / sweep
  │          │          │         │              │
  │          │          │         │              └─ day-to-day coordination
  │          │          │         └─ one terminal per agent (+ daemons)
  │          │          └─ scaffold inbox files + AGENTS.md, register in ~/.giga/swarms.toml
  │          └─ schema + cross-ref check, no side effects
  └─ guided bootstrap (spawns Claude with a baked-in prompt that does all of the above)
```

1. **`giga setup`** — zero-state bootstrap. Shells out to `claude` with a large baked-in prompt encoding the current release's command surface; the spawned session interviews the operator and runs the scaffolding for them. (`giga` itself writes nothing in the guided path.)
2. **`giga validate`** — TOML schema + cross-reference check (every channel participant resolves to an agent, every channel side has an inbox dir, at most one bench scheduler / swarm boss per host, etc.). No side effects.
3. **`giga init`** — idempotently creates inbox channel files (with the convention header), renders each agent's `AGENTS.md` from templates + runtime snippets, scaffolds codex bridge dirs, pre-seeds Claude folder-trust, and registers the swarm in `~/.giga/swarms.toml`. Host-aware: only scaffolds agents whose `host` matches `this_host`.
4. **`giga launch`** — translates the config into a list of `Pane`s (one per agent; two for codex) plus optional sync/merger/ui daemon panes, and hands them to a detected multiplexer.
5. **`giga post` / `giga watch` / `giga sweep`** — the day-to-day loop: agents post replies, watchers surface inbound messages, the operator sweeps to see who owes whom.

Most commands auto-resolve their config via the registry (`registry::resolve_config`), so they work from anywhere under a code root. `giga init` is the exception — it uses the config path literally, so run it from the swarm dir.

**Where the rest fit:** `remote`/`--host` sugar runs a subcommand on a peer over SSH; `sync`/`merger` are the cross-host daemons; `teleport` moves a running agent between hosts; `takeover` flips an agent's runtime in place; `upgrade` reinstalls the binary swarm-wide and broadcasts a watcher re-arm; `ui` serves the dashboard.

### Subcommand reference (matches `src/main.rs`)

| Subcommand | Purpose |
|---|---|
| `setup` | One-command bootstrap — launch Claude with a baked-in prompt that scaffolds a swarm end-to-end. `--remote-node` instead bootstraps *this* machine as a peer in an existing swarm. |
| `validate` | TOML schema + cross-reference check; flags on-disk inbox files not enrolled in `[[channels]]`. No side effects. |
| `init` | Create inbox files + per-agent `AGENTS.md` (idempotent); register the swarm in `~/.giga/swarms.toml`. Host-aware. |
| `launch` | Spawn one terminal per agent (`--terminal auto/mac-terminal/tmux/wt/print`); `--only` adds agents non-disruptively; `--host` runs on a peer; `--ui` also spawns the dashboard pane. |
| `teleport` | Move an agent from one host to another — rsync workdir, flip `agent.host` in TOML, re-init/launch on target, prepend a banner to HANDOVER.md, kill the source pane. |
| `takeover` | Flip an agent's runtime (claude/codex/agy) in place — re-render `AGENTS.md`, prepend a context block to HANDOVER.md, print a one-shot resume prompt. |
| `set-swarm-boss` | Promote/demote the agent that runs the per-host `sync`+`merger` daemons via Monitors. At most one per host; must be `platform=wsl`. Re-runs `init`. |
| `upgrade` | Install the latest `giga` binary locally (and on peers), then broadcast `[giga-rearm]` so watchers reload. `--bare` skips swarm machinery. |
| `ui` | Browser dashboard (axum) over every registered swarm on this machine; default `127.0.0.1:7878`. |
| `hosts` | Read-only topology view — which agents live on each host and whether `this_host` matches. `--available` lists unregistered tailnet members. |
| `claude-operator` | Operator help for Claude — drops into a Claude session preloaded with the giga command surface at a TTY; prints the doc when piped. |
| `sweep` | Tabulate every channel's last message + open `WAITING ON` tags. `--owed-by` filters; `--host` runs on a peer. |
| `post` | Append a properly-formatted frame to a channel. `--waiting-on` tags a reply as owed; `--to`/`--fyi` shape broadcast fanout. |
| `add-agent` | Scaffold a new agent — `[[agents]]` + per-peer `[[channels]]` + broadcast enrollment + `agents/<slug>.md`. `--host` auto-bootstraps a peer. |
| `add-host` | Append a `[[hosts]]` entry (with first-host local→multi-host migration) and auto-bootstrap the peer. |
| `add-channel` | Append a bilateral channel between two existing agents; auto-derives `<a>-<b>.md`. |
| `switch` | Multi-account credential manager (claude-only today): snapshot/swap `~/.claude/.credentials.json` against `~/.claude-accounts/<name>.json`. |
| `watch` | Long-running per-agent inbox watcher — tails every channel the agent participates in. `--agy`/`--codex` switch delivery mode; `--stagger-seconds`/`--no-stagger` tune broadcast fanout. |
| `merger` | Long-running daemon — fold peer `<channel>.<host>.md` slices into the watched `<channel>.md`. No-op on local-only swarms. |
| `sync` | Long-running daemon — every ~3s, rsync/git-push the canonical TOML + own slices to each peer; re-reads config every ~15s. |
| `remote` | SSH passthrough — run any giga subcommand on a peer over Tailscale SSH (only the `rsync+tailscale` transport supports it). |
| `codex-channel` | Forward giga inbox notifications into a running Codex filesystem-channel inbox. |

---

## 5. Subsystems

### Config and runtimes

`src/config.rs` defines the TOML schema (`Config`, `Project`, `Paths`, `Host`, `Agent`, `Channel`, `BenchProtocol`, `BroadcastConfig`, `WatchConfig`, `TransportConfig`). `Config::load` reads + validates: it canonicalizes the path (so symlinked workdir configs resolve their sibling `this_host.local.toml` correctly), loads the per-host identity, auto-defaults inbox paths (`wsl_inbox → <config_dir>/inbox`), and cross-checks everything. Path translation between WSL and Windows filesystem forms lives in `src/fs_paths.rs`.

`src/runtime.rs` is the runtime abstraction (`Runtime::{Claude, Codex, Agy}`). It resolves the effective runtime per agent (`agent.runtime → project.runtime → Claude`) and owns the three watcher-arming protocols, baked from `templates/runtimes/`:

| Runtime | Launch command | Watcher mode | Panes |
|---|---|---|---|
| `claude` (default) | `claude -c --model <m> <intro>` | `giga watch` under the Monitor tool *inside* the session | 1 |
| `agy` (Antigravity) | `agy -i <intro>` | `giga watch --agy` (background task; exits on `WAITING ON: <me>`) | 1 |
| `codex` | `codex` (intro arrives via inbox envelope) | `giga watch --codex` in a separate `<agent>-bridge` pane | 2 |

Every agent gets one universal `AGENTS.md` whose Session Start section adapts per runtime. It is re-rendered on every `init`/`launch`.

### Transports

A swarm picks **one** transport for its whole lifetime via `[transport.kind]` (or it is inferred: `local` when `[[hosts]]` is empty, `rsync+tailscale` otherwise). `src/transport.rs` defines the `Transport` trait (`name`/`tick`/`bootstrap_peer`/`supports_remote_exec`/`run_remote`) and the `for_config` factory. The three plugs live in `src/transports/`:

- **`local`** — single-host no-op; everything is the direct-write fast path.
- **`rsync_tailscale`** — the default; a thin adapter delegating `tick → sync::tick_once`, `bootstrap_peer → sync::bootstrap_peer`, `run_remote → remote::run_passthrough`. The only plug supporting remote exec.
- **`git`** — a shared bare git repo as the state store; each `tick` does pull → mirror peer slices into the local inbox → mirror canonical TOML → mirror own slice growth into the repo → commit/push (all append-only/idempotent). No remote exec.

See [`design/TRANSPORT_DESIGN.md`](design/TRANSPORT_DESIGN.md). The slice-and-merge data model (post writes slices, merger merges them) is transport-agnostic and lives outside the plugs.

### The UI dashboard (`giga ui`)

`src/ui/` is a localhost-default axum HTTP + WebSocket server — the only async/tokio part of an otherwise synchronous CLI. `ui::run` spins up a scoped multi-thread tokio runtime, enforces single-instance via `~/.giga/ui.pid`, and serves a single embedded vanilla-JS dashboard (`templates/ui/dashboard.html`) plus a JSON REST API and a per-channel live-tail WebSocket. It is **stateless per request** (reloads the registry + per-swarm TOML on every call) and delegates all mutations (post, validate, launch, kill, add-agent, upgrade) by re-invoking the `giga` binary or `tmux`/`ps` as subprocesses. The only in-memory state is `AppState.tailers`: a map of `(swarm, file) → broadcast::Sender<Post>` backed by a 500ms file-polling task that fans newly-appended posts to WebSocket subscribers.

### Remote / multi-host

The cross-host backbone lets one operator drive an N-host swarm from a single box:

- **`[[hosts]]` + `this_host`** — `[[hosts]]` enumerates the physical machines; each agent's `host` names where it runs; the sibling `this_host.local.toml` (legacy: `this_host.toml`) gives *this* machine its identity. When `[[hosts]]` is non-empty, every agent must declare an explicit `host` (a hard validation rule, added after host-less agents silently misrouted on peers).
- **`giga sync`** (`src/sync.rs`) — the long-running push daemon: ticks every ~3s, pushes only this host's own slices + the canonical TOML to peers (reception is symmetric — nobody pulls), with exponential backoff on failure and a ~15s config-reload window so post-launch `add-agent`/`add-channel` are picked up.
- **`giga remote`** (`src/remote.rs`) — the SSH-passthrough primitive that powers the `--host` sugar on `launch`/`sweep`.
- **`giga add-host`** / **`giga setup --remote-node`** — register + bootstrap a peer (atomic first-host local→multi-host migration; on-the-peer installer for Tailscale/rsync or git).
- **`giga hosts`** — the read-only topology/roster inspector.

A hard v1 assumption threads through all of it: peers are Linux/WSL, paths are force-normalized to forward slashes, and paths may differ per host only via explicit overrides. See [`design/REMOTE_DESIGN.md`](design/REMOTE_DESIGN.md).

### Swarm boss and bench scheduler

Two per-host roles, both single-holder-per-host (enforced in `Config::validate`):

- **`swarm_boss`** — the agent that hosts the per-host `sync`+`merger` daemons via **Monitor entries in its `AGENTS.md`** instead of as separate tmux daemon panes (so `giga launch` skips the daemon panes on hosts that have a boss). Must be `platform=wsl` (sync + merger are POSIX-only). The boss arms those Monitors with `--quiet` so the agent's notification stream isn't flooded. See [`design/SWARM_BOSS_DESIGN.md`](design/SWARM_BOSS_DESIGN.md).
- **`bench_scheduler`** — the agent through which CPU/IO-heavy operations clear (a slot-pool convention, `this-host` or `per-host`). Exactly one per host.

### Mobility (teleport / takeover / upgrade)

These lifecycle commands move agents and the harness through space, time, and runtimes. They lean on `config.rs`, `runtime.rs`, `sync.rs`, and `init.rs`, and notably **re-invoke the `giga` binary itself** (`giga remote`, `giga sync`, `giga post`, `giga init`, `giga launch`) as subprocesses rather than calling those modules in-process:

- **`teleport`** — relocate a running agent between tailnet hosts: rsync workdir, flip `agent.host` in TOML, re-init/launch on the target, prepend a teleport banner to HANDOVER.md, kill the source pane. Slice files are *not* moved (per-host append logs stay where they were, still visible swarm-wide via merge).
- **`takeover`** — flip a single agent's runtime in place: regenerate `AGENTS.md` for the new runtime, prepend a context block to HANDOVER.md so a fresh CLI can resume. See [`design/TELEPORT_DESIGN.md`](design/TELEPORT_DESIGN.md).
- **`upgrade`** — reinstall the binary locally and on peers, coordinate the Windows-specific disarm/rearm dance, and broadcast `[giga-rearm]` so watchers silently re-exec onto the new binary.

---

## 6. On-disk layout

A running swarm touches three places: the **config dir**, the **inbox dir**, and per-user **`~/.giga`** state.

```
~/.giga/
├── swarms.toml                       # the registry: code_root → config_path (written only by `giga init`)
├── ui.pid                            # single-instance guard for `giga ui`
├── cursors/                          # per-agent watch cursors (byte offsets, persisted after emit)
├── merge-cursors/<channel>/<host>.pos# per-(channel, host) merger offsets
└── configs/<project>/                # the swarm's CONFIG DIR (default location)
    ├── giga-harness.toml             # canonical config (the single source of truth)
    ├── this_host.local.toml          # this machine's identity (multi-host only; never rsync'd)
    ├── agents/<slug>.md              # per-agent AGENTS.md template source
    ├── HANDOVER.md                   # cross-restart / teleport / takeover context
    ├── workdirs/<agent>/             # per-agent launch context (AGENTS.md copy, config symlink,
    │                                 #   codex-channel/{inbox,outbox,processed} for codex agents)
    └── inbox/                        # the INBOX DIR (default <config_dir>/inbox)
        ├── design-code.md            # a bilateral channel (the merged/watched file)
        ├── _broadcast.md             # a broadcast channel
        ├── design-code.host-a.md     # per-host SLICE (multi-host only; single-writer)
        └── design-code.host-b.md     # peer's slice, folded in by the local merger
```

Notes:

- The **canonical TOML** is the single writer-of-record for topology; the sync daemon rsyncs/pushes it to peers (excluding `*.local.toml` and `workdirs/`).
- **Slice files** appear only on cross-host channels. Each host appends only to its own `<channel>.<host>.md`; the local merger appends peer slices into the watched `<channel>.md`. All-local channels have no slices.
- **`~/.giga` is host-local**: registry, cursors, merge cursors, and the UI pid never travel between machines. In multi-host integration tests each simulated host therefore needs its own `HOME`.
- An agent's `~/.claude/` conversation history is also per-machine — which is why teleport/takeover write a HANDOVER.md banner so a fresh CLI can pick up context.

---

## 7. Build, test, and release

**Build.** Standard Cargo. The binary is `giga` (`[[bin]]` in `Cargo.toml`); crate version `0.6.55`, edition 2021. The release profile uses `opt-level = 3`, thin LTO, single codegen unit, and symbol stripping.

```sh
cargo build --release          # produces target/release/giga
cargo install --path .         # installs giga into ~/.cargo/bin
```

**Test.** Unit tests live inline with `#[cfg(test)]` modules across `src/`. The end-to-end / chaos correctness harness lives in `tests/` as four separate Cargo integration-test binaries that drive the *real* `giga` binary as a subprocess (via `env!("CARGO_BIN_EXE_giga")`), so they validate the exact code paths real agents hit (`giga post`, `giga merger --once`, `giga sync --once [--dry-run]`, `giga watch`, `giga sweep`):

- `tests/swarm_chaos.rs` — local single-host concurrency invariants (append atomicity, watcher self-filter, cursor persistence across restart).
- `tests/cross_host_e2e.rs` — the cross-host slice-and-merge pipeline (rsync faked by `fs::copy`), sequential.
- `tests/cross_host_chaos.rs` — the same pipeline under concurrency (merger idempotency / no double-delivery, per-author ordering).
- `tests/git_transport_e2e.rs` — the git transport driven against a real bare local git repo.

```sh
cargo test --release           # unit + integration (CI runs release profile)
```

**CI** (`.github/workflows/ci.yml`) builds + tests on `ubuntu-latest`, `macos-latest`, and `windows-latest` (release profile, matching the release path), with a soft `cargo fmt --check` warning.

**Release** (`.github/workflows/release.yml`) fires on `v*` tags and cross-builds four targets — `x86_64-unknown-linux-musl` (statically linked, runs on old WSL distros), `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, and `x86_64-apple-darwin` — then publishes a GitHub Release with the archives plus `install.sh` / `install.ps1`. Those installers are what the README's one-line install commands fetch.

---

## 8. Glossary

- **Agent** — one AI coding CLI (Claude Code, Codex, or agy) running in its own workdir and terminal, defined by an `[[agents]]` entry and guided by a generated `AGENTS.md`.
- **Runtime** — which CLI an agent runs as: `claude` (default), `codex`, or `agy`. Resolved per-agent (`agent.runtime → project.runtime → claude`); determines the launch command, watcher mode, and pane count.
- **Channel** — a shared Markdown file in the inbox dir that a fixed set of `participants` post to. **Bilateral** (`<a>-<b>.md`, two participants) or **broadcast** (`_*.md`, many).
- **Frame / message** — one append-only `===`-delimited block: `[sender] subject — UTC-timestamp` header, body, and a `WAITING ON:` or `(Informational, …)` footer.
- **WAITING ON tag** — the "who owes the next move" marker in a frame's footer; the unit of work-handoff the harness tracks (and re-derives for stale-wait detection and `giga sweep`).
- **Watcher** — the long-running `giga watch` process per agent that tails its channels via a persisted byte cursor and surfaces new frames into the agent's session.
- **Cursor** — a persisted byte offset (under `~/.giga`) recording how much of a channel a watcher (or merger) has already consumed; written only after a successful emit, so crashes re-deliver rather than drop.
- **Inbox dir** — the shared directory holding all channel files for a swarm (default `<config_dir>/inbox`).
- **Slice** — a per-host, single-writer channel file `<channel>.<host>.md` on a cross-host channel. Append-only by construction; the wire format the sync daemon ships.
- **Merger** — the per-host `giga merger` daemon that folds peer slices into the watched merged `<channel>.md`, tracking per-(channel, host) cursors so peer bytes merge exactly once.
- **Sync daemon** — the per-host `giga sync` daemon that pushes this host's own slices + the canonical TOML to peers (~3s ticks, push-only, symmetric).
- **Transport** — the pluggable mechanism (`local` / `rsync+tailscale` / `git`) that moves slices and the canonical TOML between hosts; one per swarm, selected by `[transport.kind]`.
- **`this_host`** — the host identity of the local machine, loaded from the sibling `this_host.local.toml`; tells slice writes which suffix to use. `None` means local-only mode.
- **Registry** — `~/.giga/swarms.toml`, an auto-maintained `code_root → config_path` map (written only by `giga init`) that lets `giga <command>` resolve the right swarm from anywhere under a codebase.
- **Swarm boss** — the per-host agent that runs the `sync`+`merger` daemons via Monitors in its `AGENTS.md` instead of as tmux panes. At most one per host; must be `platform=wsl`.
- **Bench scheduler** — the per-host agent through which heavy operations clear, via a slot-pool convention. Exactly one per host.
- **Teleport** — moving a running agent from one host to another (rsync workdir, flip `agent.host`, re-launch on target, kill source pane).
- **Takeover** — flipping a single agent's runtime in place, regenerating its `AGENTS.md` and prepending a HANDOVER.md context block so a fresh CLI can resume.
- **Broadcast fanout / stagger** — the limiter that spreads `_*.md` notifications across watchers by `slot × stagger_seconds`, with `[all]`/`[ack:…]`/`[fyi]`/`[giga-rearm]` subject prefixes controlling who wakes.
- **Stale-wait detection** — the no-LLM scan that re-derives unresolved `WAITING ON: <me>` tags older than a threshold and surfaces them, healing the silent-wedge failure mode.
- **HANDOVER.md** — the per-swarm context file a fresh CLI reads on restart/teleport/takeover; mobility commands prepend a banner to it.
