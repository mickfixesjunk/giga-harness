# `src/` — giga-harness core CLI

The Rust source for the `giga` binary (crate `giga-harness`). `giga` turns one
canonical `giga-harness.toml` into a runnable swarm of parallel AI coding agents
(Claude Code, Codex, or Antigravity) that coordinate by appending
convention-formatted frames to shared Markdown "channel" files and tailing them.
All coordination is plain text in shared files — there is no database, no message
bus, and no LLM in the loop.

This folder is the whole CLI surface. For the system-wide picture (coordination
model, on-disk layout, command lifecycle, glossary) start at
[`../ARCHITECTURE.md`](../ARCHITECTURE.md) — that is the hub; this README is the
src-tree map.

## Layered architecture

`src/` is organized into subsystem folders with a **clean, one-directional
dependency graph**:

```
                       main.rs  →  cli.rs  →  dispatch.rs        (entry shim)
                                                  │
        ┌──────────────┬──────────────┬──────────┼──────────┬──────────────┐
        ▼              ▼              ▼           ▼           ▼              ▼
   coordination/   transport/     scaffold/    mutate/    mobility/    accounts/  ui/
        │              │              │           │           │           │       │
        └──────────────┴──────────────┴── config/ · runtime/ ─┴───────────┴───────┘
                                          │
                                          ▼
                                     foundation/   ← dependency-free LEAF layer
```

- **`foundation/`** is the leaf: pure primitives (frame grammar, byte-tail,
  locked/atomic file writes, dirs/paths, subprocess/ssh/tailscale, self-invoke,
  slice naming, timestamp format). It knows nothing domain-y.
- **`config/` and `runtime/`** are the shared domain types every other subsystem
  loads (`Config`, the `Runtime` enum).
- The **domain subsystems** (`coordination`, `transport`, `scaffold`, `mutate`,
  `mobility`, `accounts`, `ui`) build on those, never on each other in a cycle.
- **`main.rs`** is a 3-line shim (`cli::Cli::parse().command.run()`); the clap
  schema is in [`cli.rs`](./cli.rs) and the dispatch match in
  [`dispatch.rs`](./dispatch.rs).

**The dependency rule:** nothing depends back into `foundation` — i.e. nothing in
`foundation/` imports from the subsystem layers, and the subsystems lean *down*
on `foundation`/`config`/`runtime`, not *sideways* into each other's internals.
When code needs to "call another command", it re-invokes the `giga` binary as a
subprocess (via [`foundation::self_invoke`](./foundation/)) rather than reaching
across folders in-process. This is most visible in `mobility/`.

## Subsystem tour

| Folder | Role | README |
|---|---|---|
| [`foundation/`](./foundation/) | Dependency-free leaf layer: the `===`-frame grammar (`frame`), byte-cursor tail (`tail`), locked append (`append`), atomic writes (`atomic_io`), `dirs`/`paths`, subprocess/`ssh`/`tailscale` substrate, `self_invoke` binary resolver, `slices` naming, `timefmt`. | [foundation/README.md](./foundation/README.md) |
| [`config/`](./config/) | TOML schema (`schema`), load/canonicalize (`load`), per-invariant `validate`, read-side `resolve` accessors, `broadcast` semantics, and the `edit_then_validate_with_rollback` mutation lifecycle (`edit`). `mod.rs` is the re-export façade. | [config/README.md](./config/README.md) |
| [`runtime/`](./runtime/) | The `Copy` `Runtime` enum (`Claude`/`Codex`/`Agy`) + per-runtime `claude`/`codex`/`agy` modules holding each runtime's session snippet, intro, and session-log locator. | [runtime/README.md](./runtime/README.md) |
| [`coordination/`](./coordination/) | The message substrate: `post`, `merger`, `sweep`, `stale_wait`, `cursor`, `codex_channel`, and the `watch` daemon (`NotificationSink` + broadcast classification). | [coordination/README.md](./coordination/README.md) |
| [`transport/`](./transport/) | The `Transport`/`RemoteExec` traits + `TickCtx` + `for_config` factory, the three plugs (`local`/`rsync_tailscale`/`git`), `remote` SSH passthrough, the `hosts` roster, `setup_remote_node`, and the `sync` daemon (`mod`/`plan`/`rsync`/`bootstrap`). | [transport/README.md](./transport/README.md) |
| [`scaffold/`](./scaffold/) | `init` (filesystem effects) vs `render` (AGENTS.md / channel-header text), `launch` (pane assembly), `templates`, and the `terminal/` backends behind `TerminalBackend`. | [scaffold/README.md](./scaffold/README.md) |
| [`mutate/`](./mutate/) | The four TOML-mutating commands (`add_agent`, `add_channel`, `add_host`, `set_swarm_boss`), all atomic via `config::edit`, plus the shared `peer_bootstrap` helper. | [mutate/README.md](./mutate/README.md) |
| [`mobility/`](./mobility/) | Agent/harness mobility: `teleport` (host→host), `takeover` (runtime flip), `upgrade` (binary reinstall, split into `installer`/`windows_rearm`). | [mobility/README.md](./mobility/README.md) |
| [`accounts/`](./accounts/) | `switch` — the multi-account Claude credential manager. | [accounts/README.md](./accounts/README.md) |
| [`ui/`](./ui/) | The `giga ui` dashboard: an axum HTTP + WebSocket server over every registered swarm (the only async/tokio part of the CLI). REST handlers live under `api/{read,mutate,dto}`. | [ui/README.md](./ui/README.md) |

Top-level files that don't belong to a subsystem:

| File | Purpose |
|---|---|
| [`main.rs`](./main.rs) | Binary entrypoint — the `cli::Cli::parse().command.run()` shim + the `mod` declarations. |
| [`cli.rs`](./cli.rs) | The clap `Cli` struct + `Command` enum — **the `--help` surface** (a compatibility contract; don't reword the doc-comments). |
| [`dispatch.rs`](./dispatch.rs) | `impl Command { fn run() }` — maps each variant to its subsystem (`registry::resolve_config` first, then the call). |
| [`registry.rs`](./registry.rs) | `~/.giga/swarms.toml` resolver — `resolve_config`/`resolve_config_or`, `upsert`, `find_by_cwd`. |
| [`trust.rs`](./trust.rs) | `pre_trust` — pre-seeds Claude per-folder trust (`~/.claude.json`) so launched agents don't hit a folder-trust prompt. |
| [`fs_paths.rs`](./fs_paths.rs) | Cross-platform path translation (`to_host_fs`: Windows drive ↔ WSL `/mnt`). |
| [`validate.rs`](./validate.rs) | `giga validate` presentation (the schema check itself lives in `config::Config::load`/`validate`). |
| [`setup.rs`](./setup.rs) | `giga setup` — zero-state bootstrap via a baked-in Claude prompt (writes nothing in Rust; shells out to `claude`). |
| [`claude_operator.rs`](./claude_operator.rs) | `giga claude-operator` — TTY-aware operator command-surface doc/launcher. |

## Command → subsystem map

`dispatch.rs` is the index of which subsystem owns each subcommand. Grouped by
subsystem:

- **coordination:** `post` → `coordination::post`, `watch` → `coordination::watch`,
  `merger` → `coordination::merger`, `sweep` → `coordination::sweep`,
  `codex-channel` → `coordination::codex_channel`.
- **transport:** `sync` → `transport::sync`, `remote` → `transport::remote`,
  `hosts` → `transport::hosts`, `setup --remote-node` → `transport::setup_remote_node`.
  `launch --host` / `sweep --host` route through `transport::remote::run`.
- **scaffold:** `init` → `scaffold::init`, `launch` → `scaffold::launch`.
- **mutate:** `add-agent` → `mutate::add_agent`, `add-channel` → `mutate::add_channel`,
  `add-host` → `mutate::add_host`, `set-swarm-boss` → `mutate::set_swarm_boss`.
- **mobility:** `teleport` → `mobility::teleport`, `takeover` → `mobility::takeover`,
  `upgrade` → `mobility::upgrade` (`run` or `run_bare`).
- **accounts:** `switch` → `accounts::switch`.
- **ui:** `ui` → `ui::run`.
- **top-level:** `setup` → `setup::run`, `validate` → `validate::run`,
  `claude-operator` → `claude_operator::run`.

Most arms call `registry::resolve_config(config)` first so a bare `giga <cmd>`
works from any cwd under a swarm; `setup` (non-`--remote-node`), `ui`, and
`claude-operator` skip resolution.

## Cross-references

- [`../ARCHITECTURE.md`](../ARCHITECTURE.md) — the system-wide hub (coordination
  model §2, module map §3, command lifecycle §4, on-disk layout §6, glossary §8).
- [`../README.md`](../README.md) — operator-facing overview.
- [`../docs/`](../docs/) — operator walkthroughs (QUICKSTART, MANUAL_SETUP,
  COMMAND_REFERENCE, REMOTE_QUICKSTART).
- [`../design/`](../design/) — design rationale per subsystem (REMOTE,
  REMOTE_DUAL_WRITE, TRANSPORT, BROADCAST_FANOUT, SWARM_BOSS, TELEPORT,
  STALE_WAITS_NO_LLM).
- [`../tests/`](../tests/) — integration tests driving the real `giga` binary.
