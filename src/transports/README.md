# `src/transports/` — concrete `Transport` plug implementations

This folder holds the three concrete implementations of the [`crate::transport::Transport`](../transport.rs) trait that ship in giga-harness: `local` (single-host no-op), `rsync+tailscale` (v0.2's default), and `git` (a shared git repo as swarm-state store).

> **Naming asymmetry — read this first.** The trait *and* the `for_config` factory live in `src/transport.rs` (**singular**). The plug *impls* live in `src/transports/` (**plural**). They are different files. Don't confuse the two.

## Role in the system

giga-harness coordinates parallel AI agents by having each host write its own single-writer slice file into a shared channel, and a local merger append peer slices into the watched merged channel (the "slice-and-merge" model). That slice-write/merge fast-path is **transport-agnostic** and lives in [`post.rs`](../post.rs) + [`merger.rs`](../merger.rs), *not* here. What a transport abstracts is narrower: only *how* per-host slice files and the canonical `giga-harness.toml` ship between hosts. A swarm picks **exactly one** transport for its entire lifetime via `[transport.kind]` in its TOML, and the factory [`crate::transport::for_config`](../transport.rs) (transport.rs:83) dispatches to one of the structs in this folder — inferring `local` vs `rsync+tailscale` from `[[hosts]]` emptiness when no `[transport]` section is present (transport.rs:88-94). Of the three plugs, only `git` carries substantial logic in this folder; `local` and `rsync+tailscale` are intentionally tiny.

## File index

| File | Lines (approx) | Purpose |
| --- | --- | --- |
| [`mod.rs`](./mod.rs) | 5 | Module aggregator — declares the three submodules `pub`. No logic. |
| [`local.rs`](./local.rs) | 91 | `local` plug: single-host swarm; every sync op is a no-op. |
| [`rsync_tailscale.rs`](./rsync_tailscale.rs) | 55 | `rsync+tailscale` plug: thin adapter forwarding to `crate::sync` / `crate::remote`. |
| [`git.rs`](./git.rs) | 631 | `git` plug: shared git repo as state store; pull → mirror → commit → push per tick. The only non-trivial impl here. |

## Files

### `mod.rs`

Trivial module aggregator. Declares the three concrete submodules as public:

- `pub mod git;`
- `pub mod local;`
- `pub mod rsync_tailscale;`

The module doc explicitly notes that the trait and factory live in `crate::transport`, not here. There is no code beyond the three `pub mod` lines; the factory reaches into these via fully-qualified paths such as `crate::transports::local::LocalTransport` (transport.rs:97-101).

### `local.rs`

**Purpose.** The `local` plug: a single-host swarm transport where every sync operation is a no-op. It is active when `[transport.kind] = "local"` is set explicitly, **or** (legacy v0.2 path) when the config has no `[[hosts]]` entries and the factory infers `local` (transport.rs:88-90). Its real jobs are to make `giga sync` exit cleanly and to make `bootstrap_peer` fail with a helpful, actionable error if someone tries to go multi-host without configuring a real transport.

**Key public items.**

- `pub struct LocalTransport` — unit (zero-field) struct; the plug (local.rs:18).
- `impl Transport for LocalTransport` (local.rs:20).
  - `fn name(&self) -> &'static str` — returns `"local"`.
  - `fn tick(&self, _cfg, _this_host, _dry_run) -> Result<()>` — unconditional `Ok(())`; ignores all arguments.
  - `fn bootstrap_peer(&self, _cfg, peer, _config_path) -> Result<()>` — **always** `Err`, pointing the operator at `rsync+tailscale` / `git` plus `giga add-host`.
  - `fn supports_remote_exec(&self) -> bool` — returns `false`.

**Internals / control flow.** All methods are pure and short. `tick` returns `Ok` immediately — the slice-and-merge fast-path for all-local channels is handled in `post.rs` + `merger.rs` with no transport involvement. `run_remote` is **not** overridden, so it inherits the trait default in transport.rs:65 which errors with `"...--host commands not supported by this transport..."`.

**Gotchas / invariants.**

- `tick` is a no-op even under `dry_run` (tested local.rs:60-64).
- The `bootstrap_peer` error string is **load-bearing** — tests assert it contains `"local transport"`, `"single-host"`, and `"giga add-host"` (local.rs:67-76). Don't reword it without updating the tests.
- Because `supports_remote_exec` is `false`, users of a local swarm who pass `--host` hit the inherited `run_remote` default error (asserted at local.rs:84-90).

### `rsync_tailscale.rs`

**Purpose.** The `rsync+tailscale` plug: v0.2's default transport, rsync over Tailscale SSH. It is deliberately a **thin adapter** — the actual rsync planning/execution lives in [`crate::sync`](../sync.rs) and the SSH passthrough for `giga remote --host` lives in [`crate::remote`](../remote.rs). This module is the Stage-1 v0.3.0 refactor that wraps those already-shipped, prod-tested modules in the new `Transport` trait without moving their bodies. The module doc notes later stages *may* inline that logic into this file.

**Key public items.**

- `pub struct RsyncTailscaleTransport` — unit struct; the plug (rsync_tailscale.rs:18).
- `impl Transport for RsyncTailscaleTransport` (rsync_tailscale.rs:20).
  - `fn name(&self) -> &'static str` — returns `"rsync+tailscale"`.
  - `fn tick(&self, cfg, this_host, dry_run)` — delegates to `crate::sync::tick_once(cfg, this_host, dry_run)`.
  - `fn bootstrap_peer(&self, cfg, peer, config_path)` — delegates to `crate::sync::bootstrap_peer(cfg, peer, config_path)`.
  - `fn supports_remote_exec(&self) -> bool` — returns `true`.
  - `fn run_remote(&self, cfg, peer, args) -> Result<i32>` — delegates to `crate::remote::run_passthrough(cfg, peer, args)`.

**Internals / control flow.** Every method is a one-line forward. This is the **only** plug that overrides `run_remote` and the **only** one that returns `true` from `supports_remote_exec`, so it is the only transport for which `giga remote` / `sweep` / `launch --host` work. `run_remote` returns `Result<i32>` — an exit code passed through from the remote process by `crate::remote::run_passthrough`.

**Gotchas / invariants.** Because the logic is delegated, the real behavior and invariants of sync ticking and SSH passthrough live in `crate::sync` (`sync::tick_once` is the per-tick worker) and `crate::remote` (`run_passthrough`). Tests in this file only assert the stable name and that `supports_remote_exec` is `true` (rsync_tailscale.rs:46-54).

### `git.rs`

**Purpose.** The `git` plug: uses a shared git repo as the swarm-state store (per [`TRANSPORT_DESIGN.md`](../../design/TRANSPORT_DESIGN.md) §4.3). The repo holds the canonical `giga-harness.toml`, `agents/<slug>.md` templates, and `slices/<channel-stem>.<host>.md` single-writer slice files. Each `tick` synchronizes the local inbox with the repo via `git pull`/`commit`/`push` plus append-only file mirroring. This is the only plug in the folder with non-trivial logic.

**Repo layout** (from the module doc, git.rs:4-8):

```
<local-clone-dir>/
├── giga-harness.toml            (canonical; everyone reads + operator writes)
├── agents/<slug>.md             (per-agent CLAUDE.md templates; operator writes)
└── slices/<channel>.<host>.md   (single-writer: only that host pushes)
```

**Key public items.**

- `pub struct GitTransport { pub state_repo: String, pub local_clone_dir: PathBuf }` — the plug, carrying the remote URL + local clone path (git.rs:35-38).
- `pub fn GitTransport::from_config(cfg) -> Result<Self>` (git.rs:44) — parses `[transport.git]`; errors if `[transport]` is missing (`"...without [transport]..."`) or `state_repo` is missing (`"...[transport.git]...state_repo..."`); defaults `local_clone_dir` via `default_clone_dir`.
- `fn ensure_clone(&self) -> Result<()>` (git.rs:63) — idempotent `git clone` only when `<dir>/.git` is absent.
- `fn run_git(&self, args) -> Result<()>` (git.rs:98) — runs git in the clone dir, errors on non-zero exit (stderr inherited).
- `fn run_git_quiet(&self, args) -> std::process::ExitStatus` (git.rs:120) — silent variant for the `diff --cached --quiet` change-detection test; returns `ExitStatus` rather than erroring.
- `fn default_clone_dir(project) -> PathBuf` (git.rs:134) — `~/.giga/swarm-state/<project>/` (`HOME`, else `USERPROFILE`, else `.`).
- `fn append_growth(src, dest) -> Result<u64>` (git.rs:151) — append-only mirror of the byte delta `[dest.len()..src.len()]`; creates `dest`; no-op if `src` absent or `src.len() <= dest.len()` (never truncates).
- `fn copy_if_different(src, dest) -> Result<bool>` (git.rs:181) — whole-file mirror with content-equality skip, used for the *non*-append-only canonical TOML.
- `struct SlicePlan { channel: String, owner: String }` (git.rs:201) — one cross-host slice (channel file + owning host).
- `fn slice_plan(cfg) -> Vec<SlicePlan>` (git.rs:212) — pure enumeration of every cross-host slice; skips `channel_is_local` channels; emits one entry per participating host (hosts resolved via `cfg.agent_host`, then sorted + deduped).
- `fn repo_slice_path(repo, p) -> PathBuf` (git.rs:242) — `slices/<stem>.<host>.md` inside the repo.
- `fn inbox_slice_path(inbox, p) -> PathBuf` (git.rs:248) — `<stem>.<host>.md` inside the local inbox (no `slices/` subdir).
- `fn canonical_toml_path(project) -> Option<PathBuf>` (git.rs:358) — best-effort registry lookup of the local config path.
- `impl Transport for GitTransport` (git.rs:257): `name()` = `"git"`, `tick()`, `bootstrap_peer()`, `supports_remote_exec()` = `false`.

**Control flow — `tick` (git.rs:262).** If `dry_run`, prints the plan to stderr and returns `Ok` with no side effects. Otherwise:

1. `ensure_clone` → `git pull --rebase --quiet` (errors are contextualized as auth/network/merge-conflict).
2. For each **peer-owned** slice in `slice_plan` (`owner != this_host`), `append_growth` from repo → local inbox.
3. `copy_if_different` from the local canonical TOML (looked up via `canonical_toml_path`) → the repo's `giga-harness.toml`.
4. For each **own-host** slice (`owner == this_host`), `append_growth` from inbox → repo.
5. `git add -A`, then `run_git_quiet(["diff", "--cached", "--quiet"])`; if that returns non-zero (staged changes present), `git commit --quiet -m "sync from <host>"` then `git push --quiet` (push errors contextualized as auth/network/non-fast-forward).

**Control flow — `bootstrap_peer` (git.rs:336).** Requires `cfg.this_host`. It simply calls `self.tick(...)` to ensure the latest state is committed/pushed — the peer is expected to bootstrap *itself* via `giga setup --remote-node --transport git --repo <url>`.

**Gotchas / invariants.**

1. **Single-writer slices** — `slices/<channel>.<host>.md` is written only by its owning host, so slices are conflict-free by construction.
2. **Append-only mirroring** — `append_growth` never truncates and is a no-op on shrink; anomalous truncation is left for a higher layer to repair (tested git.rs:586-597).
3. **Canonical TOML is last-writer-wins** — multi-writer in principle, but operator-on-one-box in practice (documented v0.3.0 limitation, [`TRANSPORT_DESIGN.md`](../../design/TRANSPORT_DESIGN.md) §7 Q4).
4. **Change-detection guard** — `git diff --cached --quiet` returns exit code 1 when staged changes exist; that's the no-op-when-nothing-changed commit guard. `run_git_quiet` calls `.expect("spawn git")`, so a missing git binary **panics** rather than erroring.
5. **`tick` requires `cfg.paths.wsl_inbox`** (errors otherwise, git.rs:275-279); `bootstrap_peer` requires `cfg.this_host` (git.rs:342-345).
6. **`supports_remote_exec` is `false`** → `--host` commands error via the trait default `run_remote`.
7. **Auth is delegated** to standard git mechanisms (SSH key / credential helper); giga manages none of it.
8. **`canonical_toml_path` returning `None`** (swarm not registered) is treated as "nothing to push" — harmless (step 3 is skipped).

## Data & control flow

Inside this folder the surface is uniform: each plug is a struct implementing the `Transport` trait, so callers never branch on transport kind directly — they go through the factory.

```
giga sync (daemon)            giga remote --host
        │                             │
        ▼                             ▼
crate::transport::for_config(cfg)  →  Box<dyn Transport>
        │                             │
   .tick(...)                    .supports_remote_exec()? → .run_remote(...)
        │
        ├─ LocalTransport::tick           → Ok(())  (no-op)
        ├─ RsyncTailscaleTransport::tick  → crate::sync::tick_once(...)
        └─ GitTransport::tick             → pull → mirror peers→inbox
                                            → mirror TOML → mirror own→repo
                                            → commit + push
```

- **Factory selection** (transport.rs:83): explicit `cfg.transport.kind` wins; otherwise no `[transport]` section infers `rsync+tailscale` when `[[hosts]]` is non-empty, else `local`; an unknown kind errors.
- **`giga sync`** (the daemon in `sync.rs`) calls `for_config(cfg).tick(...)` every tick; `tick` is required to be idempotent so a failed tick is simply retried next round.
- **`giga remote`** (in `remote.rs`) calls `for_config`, checks `supports_remote_exec`, and only then calls `run_remote` — so the `local`/`git` plugs surface a clean error instead of attempting remote exec.
- **Slice path discipline** lives entirely in `git.rs`: `slice_plan` decides *what* to mirror (pure, FS-free, tested), and `repo_slice_path` / `inbox_slice_path` decide *where* — the repo uses a `slices/` subdir, the inbox does not.
- **Cross-folder boundary:** the merger that turns mirrored peer slices into the watched merged channel is not in this folder — it's `merger.rs`, fed by the inbox files that `GitTransport::tick` step 2 (and the rsync plug, via `crate::sync`) keep up to date.

## Cross-references

- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — system-wide architecture hub; see its "Subsystems → Transports" and "Coordination model → slice-and-merge" sections.
- [`../README.md`](../README.md) — the `src/` module map this folder lives under.
- [`../transport.rs`](../transport.rs) — the `Transport` trait (`name` / `tick` / `bootstrap_peer` / `supports_remote_exec` / `run_remote`) and the `for_config` factory that constructs these structs.
- [`../sync.rs`](../sync.rs) — `crate::sync::tick_once` / `bootstrap_peer`, delegated to by the rsync+tailscale plug; also hosts the `giga sync` daemon that calls `for_config(...).tick(...)`.
- [`../remote.rs`](../remote.rs) — `crate::remote::run_passthrough`, delegated to by the rsync+tailscale plug; `giga remote` checks `supports_remote_exec` then calls `run_remote`.
- [`../config.rs`](../config.rs) — `Config`, `TransportConfig { kind, git }`, `GitTransportConfig { state_repo, local_clone_dir }`, `Channel`, `Agent`; the git plug uses `Config::channel_is_local`, `Config::agent_host`, and reads `cfg.paths.wsl_inbox` / `cfg.project.name` / `cfg.this_host`.
- [`../registry.rs`](../registry.rs) — `GitTransport::canonical_toml_path` calls `registry::load()` and reads its `entries`.
- [`../post.rs`](../post.rs) + [`../merger.rs`](../merger.rs) — the transport-agnostic slice-write/merge fast-path the `local` plug relies on.
- [`../../design/TRANSPORT_DESIGN.md`](../../design/TRANSPORT_DESIGN.md) — §4.3 git repo layout, §7 Q4 last-writer-wins limitation, per-kind TOML schema.
- [`../../design/REMOTE_DESIGN.md`](../../design/REMOTE_DESIGN.md) — §2.1 slice-and-merge append-only invariant (referenced by `git.rs`).
