# `src/mutate/` — the TOML-mutating commands

The four commands that edit the canonical `giga-harness.toml` in place, plus the
shared peer-bootstrap helper they lean on after a cross-host change. These are the
control plane for topology: adding agents, channels, and hosts, and assigning the
swarm-boss role.

## Modules (`mod.rs`)

`pub mod`: [`add_agent`](./add_agent.rs), [`add_channel`](./add_channel.rs),
[`add_host`](./add_host.rs), [`set_swarm_boss`](./set_swarm_boss.rs),
[`peer_bootstrap`](./peer_bootstrap.rs).

## All edits are atomic

Every one of the four commands mutates the TOML through
[`config::edit::edit_then_validate_with_rollback`](../config/edit.rs) — never by
hand-writing the file. That function reads into a `toml_edit::DocumentMut`
(comments survive), applies the command's `mutate` closure, writes back, then
reloads + re-validates and **restores the original bytes on any validation
failure**. So a malformed edit can never leave a broken config on disk.

| Command | `run` Args (key fields) | Mutation |
|---|---|---|
| `add_agent` | `{ name, workdir, role, platform, peers, bench_scheduler, swarm_boss, no_broadcast, template, code_root, host, dry_run, config }` | append `[[agents]]` + a `[[channels]]` per peer + broadcast enrollment |
| `add_channel` | `{ participants, file, dry_run, config }` | append one bilateral `[[channels]]` |
| `add_host` | `{ name, tailnet_hostname, ssh_user, remote_config_dir, remote_inbox_dir, no_bootstrap, dry_run, this_host_name, config }` | append `[[hosts]]` (+ first-host migration) |
| `set_swarm_boss` | `{ slug, unset, no_init, config }` | set/clear `swarm_boss` on an agent |

## Channel derivation (`derive_bilateral`)

Both add commands compute the bilateral channel's `{ file, side, participants,
purpose }` via `config::resolve`, but through two entry points:

- `add_channel` calls the method `cfg.derive_bilateral(a, b)` (looks the two
  agents up in the loaded config).
- `add_agent` calls the free fn `config::derive_bilateral_with_platforms(name,
  platform, peer, peer_platform)` per peer (it already knows the new agent's
  platform before it's in the config).

Both produce the alphabetical `<a>-<b>.md` filename and force `side=windows` if
either participant is windows.

## peer_bootstrap

`peer_bootstrap::bootstrap_peer_best_effort(cfg, peer, config_path,
run_remote_init)` is the shared post-mutation hook for cross-host changes. It
wraps the transport-layer primitives `transport::sync::bootstrap_peer` (mkdir +
rsync the swarm dir) and, when `run_remote_init` is true,
`transport::sync::run_remote_giga_init` (SSH to the peer and run `giga init`). It
is **best-effort** — a locally-correct edit is never rolled back just because the
peer is offline; it warns instead of failing. `add_agent --host` calls it with
`run_remote_init = true`; `add_host` calls it with `false`.

## Notable behaviors

- **add_agent asymmetry:** it writes the `agents/<slug>.md` template *before* the
  TOML edit so that, on a rollback, both the TOML and the orphaned template are
  cleaned up together.
- **add_host first-host migration:** when `cfg.hosts` is empty it also registers
  the local host, assigns `host=` to every previously host-less agent, and writes
  `this_host.local.toml` — restoring all of it on rollback.
- **set_swarm_boss demote** removes the `swarm_boss` key entirely (so it reads as
  default), and (unless `--no-init`) re-runs `giga init` afterward to regenerate
  the boss section in AGENTS.md.
- **`--dry-run`** prints the planned change and returns before any disk write.

## Cross-references

- [`../config/README.md`](../config/README.md) — `edit_then_validate_with_rollback`,
  `derive_bilateral*`, `DerivedChannel`.
- [`../transport/README.md`](../transport/README.md) — `sync::bootstrap_peer` /
  `run_remote_giga_init` that `peer_bootstrap` wraps.
- [`../scaffold/README.md`](../scaffold/README.md) — `init` is re-run by
  `set_swarm_boss`.
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §4 (subcommand reference), §5
  (swarm boss / bench scheduler roles).
- [`../../design/SWARM_BOSS_DESIGN.md`](../../design/SWARM_BOSS_DESIGN.md).
