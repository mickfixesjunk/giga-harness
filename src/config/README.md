# `src/config/` — the TOML schema, validation, and edit lifecycle

The single source of truth for a swarm: the `giga-harness.toml` schema, the
load/canonicalize pipeline, per-invariant validation, the read-side resolvers
every command uses, broadcast message semantics, and the atomic mutation
lifecycle shared by all the `add-*` commands.

The module is split into siblings; [`mod.rs`](./mod.rs) is a **re-export façade**
so the rest of the crate keeps importing `crate::config::{Config, …}` even after
the split.

## Modules

| Module | Visibility | Role |
|---|---|---|
| [`schema`](./schema.rs) | private | Pure data types — `#[derive(Deserialize)]` structs + defaults. |
| [`load`](./load.rs) | private | Read TOML, resolve the sibling `this_host` identity, fill path defaults, then validate. |
| [`validate`](./validate.rs) | private | Semantic cross-checks, decomposed one fn per invariant. |
| [`resolve`](./resolve.rs) | `pub` | Read-side accessors (agent → host/runtime, channel → path) + bilateral-channel derivation. |
| [`broadcast`](./broadcast.rs) | private | `_*.md` channel convention + subject-prefix parsing + stagger-slot math. |
| [`edit`](./edit.rs) | `pub` | The `edit_then_validate_with_rollback` mutation lifecycle + `toml_edit` helpers. |

## The façade (`mod.rs`)

`mod.rs` re-exports the public surface so callers never reach into the private
submodules:

```rust
pub use schema::{Agent, Channel, Config, Host};
pub use schema::{BenchProtocol, BroadcastConfig, GitTransportConfig, Paths,
                 Project, TransportConfig, WatchConfig};
pub use schema::{THIS_HOST_FILE, THIS_HOST_FILE_LEGACY};
pub use broadcast::{BroadcastPrefix, fanout_delay_seconds,
                    is_broadcast_channel, parse_broadcast_prefix};
pub use resolve::{derive_bilateral_with_platforms, DerivedChannel};
pub use edit::edit_then_validate_with_rollback;
```

## Schema (`schema.rs`)

The TOML structs: `Config`, `Project`, `Paths`, `Host`, `Agent`, `Channel`,
`BenchProtocol`, `TransportConfig`, `GitTransportConfig`, `BroadcastConfig`,
`WatchConfig`. Consts `THIS_HOST_FILE` (`this_host.local.toml`) and
`THIS_HOST_FILE_LEGACY` (`this_host.toml`) name the per-host identity file.

`Config` carries the parsed topology plus two `#[serde(skip)]` fields filled at
load time: `this_host: Option<String>` (this machine's identity) and
`source_path: Option<PathBuf>` (the canonical config path, so daemons push the
right file rather than a cwd-relative bare name).

## Load (`load.rs`)

`Config::load(path)` is the entry point. It canonicalizes the path (so a
symlinked workdir config resolves its *sibling* `this_host.local.toml`), reads
the per-host identity (`load_this_host`, preferring `THIS_HOST_FILE` over the
legacy name), fills inbox defaults (`apply_path_defaults`,
`resolve_windows_userprofile`), then calls `validate`.

## Validate (`validate.rs`)

`Config::validate(&self)` runs the semantic cross-checks, decomposed per
invariant: `validate_hosts`, `validate_channels`, `validate_schedulers`,
`validate_agents`, `validate_swarm_bosses` — e.g. every channel participant
resolves to an agent, every channel side has an inbox dir, at most one bench
scheduler / swarm boss per host, the boss is `platform=wsl`, and (in a multi-host
swarm) every agent declares an explicit `host`.

## Resolve (`resolve.rs`)

The read-side accessors used everywhere else:

- `agent_host(&self, agent)` — the agent's host, if any.
- `agent_runtime(&self, agent) -> crate::runtime::Runtime` — priority
  agent-level → project-level → `Claude` default.
- `channel_is_local(&self, ch)` — true when *all* participants are on
  `this_host` (the single-host direct-write fast path; cross-host channels take
  the slice path instead).
- `channel_path(&self, ch)` — resolve the merged channel file's host-fs path.
- `inbox_for_host_side(&self, host, side)` — the inbox dir for a `(host, side)`.
- `agent_by_name(&self, name)`.
- `derive_bilateral(&self, a, b) -> DerivedChannel` and the free fn
  `derive_bilateral_with_platforms(a, a_platform, b, b_platform)` — compute a
  bilateral channel's `{ file, side, participants, purpose }` (alphabetical
  `<a>-<b>.md`; `side=windows` if either participant is windows). Used by
  `mutate::add_channel`/`add_agent`.

## Broadcast (`broadcast.rs`)

The `_*.md` fanout semantics:

- `is_broadcast_channel(filename)` — `name.starts_with('_') && name.ends_with(".md")`.
- `BroadcastPrefix { Fyi, Ack(Vec<String>), All, GigaRearm }` +
  `parse_broadcast_prefix(subject)` — classify a subject line's `[fyi]` /
  `[ack: a, b]` / `[all]` / `[giga-rearm]` prefix.
- `fanout_delay_seconds(this_agent, recipients, stagger_seconds)` — the
  per-agent stagger (`slot × stagger_seconds`) that spreads a broadcast wake-up
  across watchers. See [`../../design/BROADCAST_FANOUT_DESIGN.md`](../../design/BROADCAST_FANOUT_DESIGN.md).

The watcher re-uses this from `coordination::watch::broadcast::classify`, which
is exactly `config::parse_broadcast_prefix(extract_subject(line))`.

## Edit lifecycle (`edit.rs`)

Every TOML-mutating command funnels through one function so edits are **atomic
and self-validating**:

```rust
pub fn edit_then_validate_with_rollback(
    path: &Path,
    mutate: impl FnOnce(&mut DocumentMut) -> Result<()>,
) -> Result<Config>
```

It reads the file into a `toml_edit::DocumentMut` (comments survive), runs the
caller's `mutate` closure, writes it back, then reloads via `Config::load`
(which re-validates). **On any validation failure it restores the original
bytes** and returns the error — so a bad mutation never leaves a broken config on
disk. It returns the freshly-loaded `Config` on success. The `pub(crate)` helpers
`ensure_array_of_tables` and `append_channel` are the building blocks the
`mutate` closures compose.

## Cross-references

- [`../README.md`](../README.md) — the `src/` layered map.
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §5 (Config and runtimes), §2
  (broadcast fanout).
- [`../mutate/README.md`](../mutate/README.md) — the four commands built on
  `edit_then_validate_with_rollback`.
- [`../runtime/README.md`](../runtime/README.md) — the `Runtime` enum
  `agent_runtime` returns.
- [`../../design/BROADCAST_FANOUT_DESIGN.md`](../../design/BROADCAST_FANOUT_DESIGN.md).
