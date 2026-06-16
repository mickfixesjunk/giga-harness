# `src/transport/` — the cross-host plumbing

Everything that moves bytes between hosts: the pluggable `Transport` abstraction,
the three concrete plugs, SSH passthrough for `--host` sugar, the hosts roster,
the per-peer installer, and the `sync` daemon that pushes this host's slices +
canonical TOML to peers.

**Transport only moves bytes.** The slice-and-merge *data model* (who writes
slices, who folds them in) is transport-agnostic and lives in
[`coordination`](../coordination/) (`post` writes slices, `merger` merges them).
This folder contains zero merge logic — it ships files and runs remote commands.

## The trait + factory (`mod.rs`)

A swarm picks **one** transport for its lifetime via `[transport.kind]` (or it's
inferred: `local` when `[[hosts]]` is empty, else `rsync+tailscale`).

```rust
pub trait Transport {
    fn name(&self) -> &'static str;
    fn self_check(&self) -> Result<()> { Ok(()) }
    fn tick(&self, ctx: &TickCtx) -> Result<()>;
    fn bootstrap_peer(&self, cfg: &Config, peer: &str, config_path: &Path) -> Result<()>;
    fn remote_exec(&self) -> Option<&dyn RemoteExec> { None }
}

pub trait RemoteExec {
    fn run_remote(&self, cfg: &Config, peer: &str, args: &[String]) -> Result<i32>;
}

pub struct TickCtx<'a> { pub cfg: &'a Config, pub this_host: &'a str,
                         pub dry_run: bool, pub quiet: bool }

pub fn for_config(cfg: &Config) -> Result<Box<dyn Transport>>;
```

`TickCtx` is the per-tick context handed to `tick`. Remote exec is a **separate,
optional capability**: a plug returns `Some(&dyn RemoteExec)` only if it can run
giga on a peer — so `giga remote` / `sweep --host` / `launch --host` error cleanly
under a plug that can't.

## The three plugs

| Plug | Struct | `tick` does | Remote exec |
|---|---|---|---|
| [`local`](./local.rs) | `LocalTransport` | nothing (no-op; single-host fast path) | no |
| [`rsync_tailscale`](./rsync_tailscale.rs) | `RsyncTailscaleTransport` | delegates to `sync::tick_once` | **yes** (`remote::run_passthrough`) |
| [`git`](./git.rs) | `GitTransport` | pull → mirror peer slices into the inbox → mirror canonical TOML → mirror own slice growth into the repo → commit/push | no |

`RsyncTailscaleTransport` is a thin adapter: `tick → sync::tick_once`,
`bootstrap_peer → sync::bootstrap_peer`, `run_remote → remote::run_passthrough`.
It's the only plug whose `remote_exec()` returns `Some`. See
[`../../design/TRANSPORT_DESIGN.md`](../../design/TRANSPORT_DESIGN.md).

## remote (`giga remote`)

`remote::run(Args { host, config, remote_args }) -> Result<i32>` gates on
`Transport::remote_exec()` then shells `ssh <user>@<tailnet_hostname>` running
`bash -lc 'cd <dir> && giga <args>'` with inherited stdio.
`remote::run_passthrough(cfg, peer, args)` is the shared executor
`RsyncTailscaleTransport::run_remote` calls. This is the primitive behind the
`--host` sugar on `launch`/`sweep`.

## hosts (`giga hosts`)

Read-only topology inspector: `hosts::run(config_path)` (one swarm),
`run_list_all()` (every registered swarm), `run_available(config_path)`
(registered hosts + unregistered tailnet members via
`foundation::tailscale`).

## setup_remote_node (`giga setup --remote-node`)

The on-the-peer installer. `setup_remote_node::run(Args { inbox_dir, dry_run,
transport, repo })` routes to a Tailscale path (install Tailscale + rsync, enable
Tailscale SSH, mkdir inbox, print the `giga add-host` command) or a git path
(install git + rsync, smoke-test the state-repo URL). Every step is idempotent;
WSL-only for v1.

## The sync daemon (`sync/`, `giga sync`)

The push daemon, decomposed into four files:

- [`sync/mod.rs`](./sync/mod.rs) — the loop. `run(Args { config, once, dry_run,
  quiet })` ticks every ~3s, reloads config every ~15s, applies exponential
  backoff on failure (`backoff_for`). Re-exports `tick_once` (from `rsync`),
  `compute_sync_plan` + `SyncCommand` (from `plan`), `bootstrap_peer` +
  `run_remote_giga_init` (from `bootstrap`).
- [`sync/plan.rs`](./sync/plan.rs) — the **pure planner**.
  `compute_sync_plan(cfg, this_host, canonical_config_path) -> Vec<SyncCommand>`
  emits one `toml` command per peer, one `template` per local `agents/<name>.md`,
  and one `slice` per cross-host channel/peer pair. `SyncCommand { peer_target,
  local_path, use_append_verify, kind }`.
- [`sync/rsync.rs`](./sync/rsync.rs) — the executor.
  `tick_once(cfg, this_host, dry_run, quiet)` runs the plan as rsync invocations
  (the `RsyncTailscaleTransport::tick` adapter); `build_rsync_target` assembles
  `user@host:path`.
- [`sync/bootstrap.rs`](./sync/bootstrap.rs) — one-shot peer bring-up.
  `bootstrap_peer(cfg, peer, config_path)` mkdirs + rsyncs the swarm dir;
  `run_remote_giga_init(cfg, peer, config_path)` SSHes to the peer and runs
  `giga init`.

**The wire invariant:** each host pushes only what it *owns* — its own
`<channel>.<this_host>.md` slices + the canonical TOML; reception is symmetric,
nobody pulls. All remote paths are forced to forward slashes (peers are
Linux/WSL). See [`../../design/REMOTE_DESIGN.md`](../../design/REMOTE_DESIGN.md).

## Cross-references

- [`../coordination/README.md`](../coordination/README.md) — `post`/`merger`,
  the slice-and-merge data model this layer ships for.
- [`../foundation/README.md`](../foundation/README.md) — `ssh`, `paths`,
  `slices`, `tailscale`, `proc` that the plugs build on.
- [`../mutate/README.md`](../mutate/README.md) — `add-host`/`add-agent --host`
  drive `bootstrap_peer` via the shared `peer_bootstrap` helper.
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §5 (Transports, Remote /
  multi-host).
- [`../../design/TRANSPORT_DESIGN.md`](../../design/TRANSPORT_DESIGN.md),
  [`../../design/REMOTE_DESIGN.md`](../../design/REMOTE_DESIGN.md).
