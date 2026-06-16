# `src/mobility/` — moving agents through space, runtimes, and versions

The lifecycle commands that relocate a running agent (`teleport`), flip its
runtime in place (`takeover`), or reinstall the binary swarm-wide (`upgrade`).

## Modules (`mod.rs`)

`pub mod`: [`teleport`](./teleport.rs), [`takeover`](./takeover.rs),
[`upgrade`](./upgrade/) (a sub-tree: `mod`/`installer`/`windows_rearm`).

## They re-invoke the giga binary

The defining trait of this subsystem: rather than calling other subsystems
in-process, these commands **re-invoke the `giga` binary itself** as a subprocess
(`giga remote`, `giga sync --once`, `giga init`, `giga launch`, `giga post`). The
binary path comes from [`foundation::self_invoke`](../foundation/self_invoke.rs) —
`giga_binary()` for the normal case, and `fresh_giga_binary()` during `upgrade`
(because a self-overwriting install leaves `current_exe()` pointing at a
`(deleted)` inode, so the path must be re-resolved via `which`). This keeps
mobility honest about the layering rule: it composes commands, it doesn't reach
sideways.

## teleport (`giga teleport <agent> --to <host>`)

`teleport::run(Args { agent, to, from, keep_running, dry_run, config })` moves a
running agent between tailnet hosts: rsync the workdir, prepend a teleport banner
to HANDOVER.md (`render_teleport_banner`), flip `agent.host` in the TOML, then
re-invoke `giga sync --once` → `giga remote ... init` → `giga remote ... launch
--only <agent>` and kill the source pane (unless `--keep-running`). Slice files
are **not** moved — past posts stay in `<channel>.<source>.md`, new posts go to
`<channel>.<target>.md`, preserving the append-only invariant. Everything after
preflight is best-effort with printed remediation.

## takeover (`giga takeover [--as <slug>] [--to <runtime>]`)

`takeover::run(Args { config, as_agent, to_runtime, dry_run })` flips a single
agent's runtime in place (no host move). It flips the TOML, reloads the config,
re-renders AGENTS.md for the new runtime via
**`scaffold::render::render_agent_claudemd`**, locates the prior runtime's session
log (`old_runtime.session_log(workdir)`), prepends a TAKEOVER block to HANDOVER.md
(`render_takeover_block`, written via `foundation::atomic_io::atomic_prepend`),
and prints a self-contained turn-1 prompt (`takeover_prompt`). Runtime/slug/role/
channel memberships are otherwise unchanged. See
[`../../design/TELEPORT_DESIGN.md`](../../design/TELEPORT_DESIGN.md).

## upgrade (`giga upgrade`)

Split across three files:

- [`upgrade/mod.rs`](./upgrade/mod.rs) — the orchestration. `run(Args { config,
  as_agent, skip_peers, skip_broadcast, skip_windows, dry_run })` is the
  swarm-aware path; `run_bare(dry_run)` is the swarm-less install (reached via
  `--bare` or when cwd isn't under a swarm). It captures the running binary
  (`self_invoke::giga_binary`), installs, **re-binds via
  `self_invoke::fresh_giga_binary`** for all subsequent spawns, propagates to
  peers, and posts the `[giga-rearm]` broadcast so watchers silently re-exec.
  Consts `WINDOWS_OPERATOR_WAIT_SECS = 15`, `WINDOWS_AGENT_REARM_DELAY_SECS = 60`
  (the rearm delay must exceed the operator wait + install + buffer).
- [`upgrade/installer.rs`](./upgrade/installer.rs) — the install mechanics:
  `install_local` (dispatches `install.sh` / `install.ps1` by platform),
  `install_local_windows_via_wsl_interop`, `install_remote` (via `giga remote
  --host`).
- [`upgrade/windows_rearm.rs`](./upgrade/windows_rearm.rs) — the Windows
  disarm/rearm dance. An in-place overwrite of a running `giga.exe` hits a sharing
  violation, so `windows_pre_install_disarm` posts a targeted `[ack: <slugs>]`
  broadcast (only Windows agents act, via the watcher's fanout filter) telling
  them to drop their watcher, the operator waits, installs, then
  `windows_post_install_rearm` posts the matching rearm.

## Cross-references

- [`../foundation/README.md`](../foundation/README.md) — `self_invoke`
  (`giga_binary`/`fresh_giga_binary`), `atomic_io::atomic_prepend`.
- [`../scaffold/README.md`](../scaffold/README.md) — `render::render_agent_claudemd`
  reused by `takeover`.
- [`../runtime/README.md`](../runtime/README.md) — `Runtime::session_log` /
  `parse` that `takeover` drives.
- [`../config/README.md`](../config/README.md) — the TOML flips
  (`agent.host` / `agent.runtime`).
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §5 (Mobility).
- [`../../design/TELEPORT_DESIGN.md`](../../design/TELEPORT_DESIGN.md),
  [`../../design/BROADCAST_FANOUT_DESIGN.md`](../../design/BROADCAST_FANOUT_DESIGN.md).
