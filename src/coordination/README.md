# `src/coordination/` — the message substrate

The file-based message-passing substrate: how agents post frames, how watchers
surface them, how cross-host slices get merged, how stale waits are healed, and
how Codex agents are bridged. This is the data plane that runs while a swarm is
live.

Everything here parses frames through the **one** grammar in
[`foundation::frame`](../foundation/frame.rs) and serializes appends through
[`foundation::append`](../foundation/append.rs) — the `===`-frame convention
itself lives in `foundation`, not here.

## Modules (`mod.rs`)

`pub mod`: [`post`](./post.rs), [`merger`](./merger.rs), [`sweep`](./sweep.rs),
[`stale_wait`](./stale_wait.rs), [`cursor`](./cursor.rs),
[`codex_channel`](./codex_channel.rs), [`watch`](./watch/) (a sub-tree:
`mod`/`sink`/`broadcast`).

## post (`giga post`)

`post::run(Args)` appends one canonically-formatted frame. `Args` carries
`{ channel, me, subject, body, waiting_on, needs, config, to, fyi }`
(`body=None` reads stdin). The private `format_block(...)` is the pure
frame **formatter** (the one piece of frame *generation* that lives in
coordination, not foundation); the bytes go out via
`foundation::append::append_with_lock`.

Routing: on a local channel it writes the merged file directly (fast path); on a
cross-host channel it **dual-writes** — `foundation::slices::slice_path(this_host)`
first (the wire copy peers receive), then best-effort the merged file (so the
local watcher sees it immediately). Slice-first ordering is the invariant — a
failed merged write still reaches peers via sync.

## watch (`giga watch`)

The always-running per-agent inbox watcher, decomposed into three files:

- [`watch/mod.rs`](./watch/mod.rs) — the loop. `WatchMode { Default, Agy, Codex }`,
  `run_single(channel, me, mode)` (legacy one-channel), `run_multi(config_path,
  me, stagger_override, mode)` (config-aware multi-channel). `ChannelState` holds
  per-channel `{ name, path, last_size, participants, pending }`. It tails on a
  3s tick (`foundation::tail::POLL_INTERVAL`), refreshes the tracked set every
  `RELOAD_EVERY_N_TICKS`, gates emission on the per-agent busy lock, runs the
  stale-wait scan, and `self_rearm`s on `[giga-rearm]`.
- [`watch/sink.rs`](./watch/sink.rs) — the **`NotificationSink` trait**, the seam
  between buffering logic and the final emit:

  ```rust
  pub trait NotificationSink {
      fn deliver(&mut self, line: &str);
      fn flush(&mut self) {}
      fn exit_on_waiting_on_me(&self) -> bool { false }
      fn prime(&mut self, channel: &str, path: &Path, offset: u64) {}
  }
  ```

  Three impls — `StdoutSink` (Claude / default), `AgySink` (stdout + exit-0 on
  `WAITING ON: <me>`), `CodexSink` (writes JSON envelopes via
  `codex_channel::write_envelope`) — selected by `sink_for(mode, …)`.
- [`watch/broadcast.rs`](./watch/broadcast.rs) — broadcast classification.
  `classify(header_line)` is exactly
  `config::parse_broadcast_prefix(extract_subject(line))`; the resulting
  `[fyi]`/`[ack]`/`[all]`/`[giga-rearm]` behavior stays in `run_multi`.

## merger (`giga merger`)

`merger::run(config_path, once, quiet)` is the **sole writer** that folds *peer*
slice files into each watched merged channel file. `merge_tick` walks tracked
channels/slices, reads the new byte-delta, and appends via `append_bytes` (which
delegates to `foundation::append::append_with_lock`, sharing the lock with
`post`). Local channels and *this* host's own slice are excluded — `post`
already dual-writes the own slice into merged, so re-merging would double-append.

## cursor — the byte-cursor no-loss model

Two cursor namespaces under `~/.giga`:

- **Watch cursors** `~/.giga/cursors/<agent>/<channel>.pos` — a single ASCII
  decimal byte offset of how far an agent has been emitted to.
  `cursor_path`/`read`/`write`.
- **Merge cursors** `~/.giga/merge-cursors/<channel>/<host>.pos` — bytes of each
  *peer* slice already merged. `merge_cursor_path`/`read_merge`/`write_merge`.

The no-loss contract: the cursor advances in memory during a read but is
**persisted only after a successful emit/append**. A crash mid-delivery therefore
re-delivers the message on restart rather than dropping it — re-derivation is
purely from file content, no separate "delivered" database. Cursor writes
**never** crash the caller (every fs error is swallowed) because a failed cursor
write must not kill a watcher/merger.

## stale_wait — no-LLM wedge healing

`stale_wait::scan(content, me, now, threshold_minutes) -> Vec<StaleWait>` is a
pure re-derivation: it walks frames and reports unresolved `WAITING ON: <me>`
tags older than the threshold (a `WAITING ON: me` is resolved by me replying, by
the sender closing it, or by a newer wait superseding it). `scan_file` is the
best-effort wrapper (never crashes the watcher); `format_notification` renders
one. `watch::run_multi` calls this on arm and dedups on
`(channel, sender, tag_timestamp)`. See
[`../../design/STALE_WAITS_NO_LLM_DESIGN.md`](../../design/STALE_WAITS_NO_LLM_DESIGN.md).

## sweep (`giga sweep`)

`sweep::run(config_path, owed_by_filter)` tabulates each channel's last message +
open `WAITING ON` tag using `foundation::frame::last_header_block`. Pure display —
touches no cursors/slices.

## codex_channel (`giga codex-channel`)

Bridges giga inbox notifications into Codex's native filesystem-channel JSON
inbox. `run(Args { me, channel_dir, config, catch_up, direct_only })` plus the
`pub(crate) fn write_envelope(inbox_dir, swarm, me, channel, offset, text)`
reused by `watch`'s `CodexSink`. Envelopes are published atomically (write `.tmp`
+ `sync_all` + rename) so Codex never reads a partial file; `idempotency_key`
makes redelivery safe.

## Cross-references

- [`../foundation/README.md`](../foundation/README.md) — `frame`/`tail`/`append`
  the substrate sits on.
- [`../config/README.md`](../config/README.md) — `parse_broadcast_prefix`,
  `channel_is_local`, the broadcast config.
- [`../transport/README.md`](../transport/README.md) — sync ships the slices that
  `merger` folds in (slice-and-merge is transport-agnostic and lives here, not in
  transport).
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §2 (coordination model:
  cursors, slice-and-merge, broadcast fanout, stale-wait).
- [`../../design/REMOTE_DUAL_WRITE_DESIGN.md`](../../design/REMOTE_DUAL_WRITE_DESIGN.md),
  [`../../design/STALE_WAITS_NO_LLM_DESIGN.md`](../../design/STALE_WAITS_NO_LLM_DESIGN.md),
  [`../../design/BROADCAST_FANOUT_DESIGN.md`](../../design/BROADCAST_FANOUT_DESIGN.md).
