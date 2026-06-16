# Design Records

This directory holds **historical design documents** for giga-harness — the
proposals, threat analyses, and architecture decisions written before (or
alongside) the features they describe. They capture the reasoning and the
constraints at the time of writing; they are **not** living documentation and
may not reflect the current shipped behavior. For the current system overview,
see [../ARCHITECTURE.md](../ARCHITECTURE.md).

Each doc records its own status (draft / GO / shipped) and date in its header.

---

## Transport & remote

How agents on different physical hosts share the same channels, and how state
moves between them.

- **[REMOTE_DESIGN.md](REMOTE_DESIGN.md)** — The original cross-host design.
  Introduces the **slice-and-merge** model: each host writes only its own
  single-writer slice file, and a local merger appends peer slices into the
  watched merged channel, so remote writes look like local appends to the
  polling watcher. Preserves the all-local file model untouched.
- **[TRANSPORT_DESIGN.md](TRANSPORT_DESIGN.md)** — Makes the sync layer
  **pluggable** so slice-and-merge isn't coupled to one network stack. Lets a
  swarm pick its transport per-setup: local filesystem (no-op), a shared git
  state-repo, an object store, or the original rsync-over-Tailscale plug.
- **[REMOTE_DUAL_WRITE_DESIGN.md](REMOTE_DUAL_WRITE_DESIGN.md)** — Follow-on to
  REMOTE_DESIGN that fixes a coupling bug: adding one remote participant flipped
  a channel to slice-only writes, so same-host posts stopped appearing in the
  merged file unless the merger daemon was alive. Proposes dual-writing so local
  visibility no longer depends on daemon liveness.

## Coordination & protocol

How agents decide whose turn it is, keep the swarm healthy, and avoid blowing
rate limits.

- **[SWARM_BOSS_DESIGN.md](SWARM_BOSS_DESIGN.md)** — Removes the per-host sync
  and merger tmux daemon panes by having one designated agent — the **swarm
  boss** — arm them as additional `Monitor`s in its own session, so daemon
  errors surface as notifications instead of dying silently in an ignored pane.
- **[STALE_WAITS_NO_LLM_DESIGN.md](STALE_WAITS_NO_LLM_DESIGN.md)** — A
  **zero-LLM** variant of stale-wait detection. Unresolved `WAITING ON: <me>`
  tags are detected entirely in local subprocesses and on disk, trading operator
  vigilance for tokens so swarm-health monitoring costs no LLM turns until a
  human nudges.
- **[BROADCAST_FANOUT_DESIGN.md](BROADCAST_FANOUT_DESIGN.md)** — Tackles the
  rate-limit blowup when a broadcast wakes N agents within one watcher tick and
  they all spawn LLM turns at once. Proposes a **stagger limiter** on
  broadcast/fanout channels to spread the synchronous token load over time.

## Mobility

Moving a running agent between hosts.

- **[TELEPORT_DESIGN.md](TELEPORT_DESIGN.md)** — Collapses the multi-step manual
  procedure for relocating an agent from host A to host B into one **teleport**
  command: update the TOML host field, rsync the workdir, tear down the old
  tmux session, scaffold and launch on the target, and prepend a "you have been
  moved" banner to `HANDOVER.md` as the load-bearing context handoff.
