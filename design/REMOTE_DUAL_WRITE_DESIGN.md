# REMOTE_DUAL_WRITE_DESIGN.md

**Status:** draft, awaiting Mick GO before code
**Author:** giga
**Date:** 2026-06-01
**Context:** follow-on to REMOTE_DESIGN.md (slice-and-merge architecture, shipped v0.3.0–v0.3.4)
**Problem:** local visibility on cross-host channels currently depends on merger daemon liveness; adding one remote agent silently disrupts in-host posting on every channel the new agent participates in.

---

## 1. The disruption that triggered this doc

Last night (2026-05-31 → 2026-06-01) Mick added `performance@morpheus-wsl` to the otherwise-local `superdeduper` swarm. Symptoms reported on `_broadcast.md` at 13:19 UTC: "merger/sync daemons weren't running; reverted to single-host". On the surface: yet another bootstrap-fragility issue (quality F11, fixed in v0.3.4). On closer reading: a **deeper architectural coupling** that v0.3.4 only papers over.

### What actually broke (src/post.rs:69-81)

```rust
let write_path = match (cfg_opt.as_ref(), channel_entry) {
    (Some(cfg), Some(ch)) if !cfg.channel_is_local(ch) => {
        // ANY participant on a remote host → slice-only write
        slice_path(&merged_path, this_host)
    }
    _ => merged_path.clone(),  // local-only fast path
};
```

When `performance@morpheus-wsl` was added as a participant on `_broadcast.md`, that channel flipped from local-only to cross-host. Every post to `_broadcast.md` — including neo↔neo posts between agents that share the same physical box — started writing **exclusively** to `_broadcast.<this_host>.md` (the slice).

Local watchers tail `_broadcast.md` (the merged file). They didn't see the slice writes. They depend on the merger daemon to read the slice and append to the merged file.

When the merger daemon was missing (a separate bug, F11), local↔local posts went dark. Not just communication with the remote agent — *everything on that channel*.

### Channel-by-channel breakdown of the disruption

| Channel | Participants (post-add) | Pre-add | Post-add | Visible without merger? |
|---|---|---|---|---|
| `_broadcast.md` | everyone incl. performance@morpheus | local | **cross-host** | NO |
| `design-performance.md` | design@neo + performance@morpheus | n/a (new) | cross-host | NO |
| `design-giga.md` | design@neo + giga@neo | local | local | YES |
| `design-superdeduper.md` | design@neo + superdeduper@neo | local | local | YES |
| `giga-quality.md` | giga@neo + quality@neo | local | local | YES |

So Mick's phrasing "local comms disrupted" was emotionally correct (because `_broadcast.md` is the most-watched channel in the swarm — when it goes dark, the swarm *feels* broken) but the literal fault surface was narrower: any channel where the remote agent appears.

### Why the v0.3.4 fix (F11) is not sufficient

v0.3.4 ensures `launch --only` spawns the sync + merger daemons even on incremental launches. That closes the most common "merger was never started" failure mode. But it doesn't address:

- **Merger crash.** A merger panic, OOM, or stuck rsync subprocess (sync side) silently re-disrupts every cross-host channel for everyone on this host until the daemon is restarted. No user-visible alert until someone notices messages aren't flowing.
- **Merger lag.** Merger polls every 3s; a long rsync from a peer can push merge-tick latency past 10s. During that window, local↔local posts on cross-host channels are invisible to local watchers. Mick observed this empirically on 2026-05-31; quality noted it in F10.
- **First post in a freshly-bootstrapped peer's session.** The merger needs ~3s after spawn to discover all slice files and read their initial sizes. Posts during that window can be either missed or double-appended depending on race ordering.
- **Network partition.** If sync can't reach the peer, the local merger keeps running but the peer's slice never updates on local disk. Local watcher sees own posts via merger (because merger reads own slice too). But sync errors are silent unless the user tails the daemon pane — they could go undetected for hours.

The common thread: **the merger is on the critical path for local visibility, and any merger fragility is felt as a swarm-wide outage on the affected channels.**

This is the wrong topology. A bilateral between two neo-side agents should not be disruptible by a sync/merger problem on a third-party remote agent's channel involvement.

---

## 2. Current architecture (post-v0.3.4)

```
┌─────────────────────────────────────────────────────────────────┐
│ HOST: neo-wsl                                                   │
│                                                                 │
│   design ─┐                                                     │
│           │  giga post _broadcast --as design …                 │
│           │                                                     │
│           ▼                                                     │
│   ╔════════════════════╗      ╔════════════════════╗            │
│   ║ _broadcast.        ║      ║ _broadcast.md      ║            │
│   ║ neo-wsl.md (slice) ║ ───► ║ (merged, watched)  ║            │
│   ╚════════════════════╝      ╚════════════════════╝            │
│      ↑                ↑           ↑                             │
│      │ append-only    │           │ merger sole writer          │
│      │ writer = post  │           │                             │
│      │                │           │                             │
│      │     ┌──────────┴──────┐    │                             │
│      │     │ merger          │────┘                             │
│      │     │ - reads own +   │                                  │
│      │     │   peer slices   │                                  │
│      │     │ - appends both  │                                  │
│      │     │   to merged     │                                  │
│      │     └─────────────────┘                                  │
│      │                                                          │
│      │           ┌────────────────────────────────────┐         │
│      │           │ watcher (every agent's giga watch) │         │
│      │           │ - tails _broadcast.md ONLY         │         │
│      │           │ - NEVER reads slice directly       │         │
│      │           └────────────────────────────────────┘         │
│      │                                                          │
│      │      ┌─────────────────────────────────┐                 │
│      │      │ sync                            │                 │
│      └──────│ - reads own slice               │                 │
│             │ - rsync to peer's slice path    │                 │
│             └─────────────────────────────────┘                 │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ rsync over Tailscale SSH
                              ▼
                  (similar topology on morpheus-wsl)
```

### Invariants honored today

| # | Invariant | Where enforced |
|---|---|---|
| I1 | Slice files are append-only | post.rs (O_APPEND), merger ignores backward truncations |
| I2 | Merged file has a single writer | merger.rs append_bytes is the sole writer; post.rs falls through to merged only for local-only channels |
| I3 | Watcher only reads merged file | watch.rs auto-discovers channels via cfg.channel_path |
| I4 | Each slice has one writer (its owning host) | sync.rs never pushes a peer's slice; bootstrap_peer rsyncs only own slice |
| I5 | Cursor state per (channel, slice) is durable | cursor::write_merge in giga_home |

Invariant I2 is the load-bearing one for this design. It's what makes the merged file globally consistent — at most one writer at a time means no torn appends.

### The implicit coupling we want to remove

> **Local visibility of a local-originated post on a cross-host channel depends on the local merger daemon completing one full tick.**

That dependency is invisible to the agent posting (they get exit 0 from `giga post` regardless), invisible to the agent watching (they see silence and assume nobody posted), and invisible to the operator (no error surfaces). It's exactly the kind of coupling that produces "the swarm felt broken last night" reports without a clear root cause.

---

## 3. Proposed: dual-write at post time

### Core idea

For cross-host channels, `giga post` writes the same body to **two** files:

1. The slice file (`<channel>.<this_host>.md`) — so sync ships it to peers.
2. The merged file (`<channel>.md`) — so local watchers see it without waiting for merger.

The merger continues to merge **peer** slices into the merged file as today, but **skips its own slice** (because post already wrote it).

### What changes

| File | Change | Risk |
|---|---|---|
| `src/post.rs` | Cross-host branch writes to BOTH slice + merged. Today it writes slice only. | Low — both writes use O_APPEND, both atomic per-write. Failure handling needs spec (§ 6). |
| `src/merger.rs` | `compute_active_channels` no longer returns this_host as a slice to track. Or `merge_tick` keeps the entry but skips appending. | Low — pure code change; existing peer-slice handling unchanged. |
| `src/cursor.rs` (probably none) | Existing cursor format unchanged; we just stop writing the own-slice cursor in merger. | Low |
| `src/sync.rs` | No change. Slice content already comes from post; sync still ships it. | None |
| `src/watch.rs` | No change. Still tails merged file. | None |
| Tests: post.rs, merger.rs | Update existing cross-host tests; add new "merger-down still delivers local→local" test. | Bounded |
| Documentation: REMOTE_DESIGN.md | Add §dual-write subsection explaining the post→merged direct write. | None |

### Diagram (proposed)

```
   design ─┐
           │  giga post _broadcast --as design …
           ▼
   ╔════════════════════╗                                    ╔════════════════════╗
   ║ _broadcast.        ║ ◄── post: append (existing) ─────► ║ _broadcast.md      ║
   ║ neo-wsl.md (slice) ║                                    ║ (merged, watched)  ║
   ║                    ║                                    ║                    ║
   ║                    ║                                    ║ ◄─ post: append    ║
   ║                    ║                                    ║   NEW: dual-write  ║
   ╚════════════════════╝                                    ╚════════════════════╝
            ↑                                                     ↑       ↑
            │ sync ships to peer                                  │       │
            │                                                     │       │
            │                          ┌─ merger appends ─────────┘       │
            │                          │  PEER SLICES ONLY                │
            │                          │  (own slice skipped — post       │
            │                          │   already wrote merged)          │
            │                          │                                  │
            │                          │                                  │
            │      peer slice ─────────┘                                  │
            │      (received via sync)                                    │
            │                                                             │
            │      watcher tails merged ─────────────────────────────────►│
```

### Invariants under the proposal

| # | Invariant | How preserved |
|---|---|---|
| I1 | Slice files are append-only | Same as today — post still O_APPENDs to slice. |
| I2 | Merged file has a single writer | **Changes shape:** today's "merger is sole writer"  becomes "(post for own posts) + (merger for peer slices) are the writers". Both use O_APPEND, both are constrained to monotonic-suffix writes. No reader needs to seek. Still safe; see §5. |
| I3 | Watcher only reads merged file | Unchanged. |
| I4 | Each slice has one writer | Unchanged. |
| I5 | Cursor state per (channel, slice) is durable | Unchanged — but the own-slice cursor entry is no longer maintained (merger doesn't track own slice). |

I2 is the change that needs the most care. See §5 below.

---

## 4. Why dual-write over alternatives

### Alt A: status quo + better merger reliability (rejected)
Make the merger restart on crash, faster polling, better failure surfacing. Reduces merger fragility but keeps the coupling. Any future merger bug re-introduces the disruption. Treats the symptom.

### Alt B: post writes to merged only; sync ships merged file (rejected)
Breaks I4 (each slice / merged has one writer) — two hosts writing to the same logical file is the original problem that motivated slice-and-merge. Not viable.

### Alt C: post writes to slice only; trigger one-shot merge in-process (rejected)
Post reads its own slice and appends to merged inside the post call. Adds latency, races with daemon merger reading the same slice, opens up double-append on race. Complex.

### Alt D: dual-write (THIS PROPOSAL)
Local-visibility is decoupled from merger liveness. Merger's job shrinks (peer slices only). Post path is one extra append (~10s of microseconds for typical message size). Failure modes are bounded and surfaceable. Migration path is clean (see §7).

### Alt E: stale-merger fallback in post (deferred)
Post checks "has merger ticked recently?" via a heartbeat file; falls back to in-process merge only when stale. Combines best of A+C but adds heartbeat tracking, race conditions, and per-post latency depending on heartbeat freshness. Could be built later on top of dual-write if dual-write turns out to have an unhandled edge case.

---

## 5. Concurrency analysis

### The new race: post and merger both writing to merged

**Today:** merger is the sole writer to the merged file. POSIX O_APPEND guarantees that each `write(2)` call is atomic up to PIPE_BUF (4KB on Linux) and serialized with other O_APPEND writers. The merger writes per-tick deltas as one `write_all` — for typical message sizes (single-digit KB) this is one syscall, atomic.

**Under dual-write:** post writes one fully-formatted message frame in a single `write_all` (existing behavior, just to a second file). Merger writes per-tick deltas. Both use O_APPEND on the same merged file. POSIX guarantees:

- Each individual `write_all` is atomic and serializes with other O_APPEND writers (kernel-side append-then-write under the inode lock).
- Resulting file is the byte-concatenation of all `write_all` results in some serialization order.

So no torn writes. The order of messages in the merged file may interleave post-originated bytes with merger-originated bytes from different ticks, but each frame is intact.

**Frame structure** (existing convention from REMOTE_DESIGN.md and watch.rs parser):

```
===
[sender] subject — UTC ISO-8601
===

body

WAITING ON: agent (...)
===
```

Each frame is a contiguous byte range with a stable header. The watcher parser is already tolerant of interleaved frames from different senders — that's the whole point of merging in the first place.

**Edge case:** post writes 10KB message in a single `write_all`. If 10KB > PIPE_BUF the kernel may split the append into multiple write operations under the same inode lock; an interleaved merger write could land between them. **Result on disk:** partial frame from post, then merger bytes, then rest of post's frame. The frame parser sees torn content.

**Mitigation:** wrap the post and merger writes with a file lock (`flock` advisory or our existing `cursor` file lock pattern) keyed by the merged file path. Cheap (one lock acquire per write), eliminates the torn-write window entirely. Tests in `swarm_chaos` already exercise concurrent posts up to PIPE_BUF boundary; extend to cover post-vs-merger contention.

**Estimation of message sizes:** today's typical agent post is 500B–3KB. Worst case (full quality finding report) was ~10KB. Common case fits in a single O_APPEND write. Edge case (10KB+) is rare but exists, hence the lock.

### Crash safety

Failure modes during dual-write (slice then merged, in that order):

| Stage | Local user sees | Peer eventually sees | Mitigation |
|---|---|---|---|
| Slice succeeds, merged fails | nothing (until merger catches up reading own slice as fallback — see §7 migration) | yes | Surface error to caller; merger can backfill own slice on first tick after restart |
| Merged succeeds, slice fails | yes | no (silent divergence) | UNACCEPTABLE — write slice first to avoid this |
| Process crash between writes | depends on order | depends on order | Same as above — slice-first ordering means peer is source of truth |
| Both succeed | yes | yes | Happy path |

**Decision: slice-first.** The slice is the canonical record (sync ships it, merger can backfill from it). If merged-write fails, the next merger tick reading own slice as a fallback recovers visibility for local. Worst case: a brief window where the operator's own post isn't visible to themselves; they re-post and the second copy lands (and may duplicate to peer — explicit user action, easy to undo).

To support this fallback gracefully, the proposal includes a small concession: **merger continues to track own slice but with a cursor that starts at the current slice EOF** (not 0), so it only catches up on bytes written after the last merger restart. This handles the rare "merged write failed mid-session" case without re-appending the entire slice on every merger restart.

### Backward compat (rolling deployment across hosts)

If host A runs new-version giga and host B runs old-version:
- Host A dual-writes: own slice + own merged. Sync ships A's slice to B.
- Host B's old merger reads A's slice and appends to B's merged. ✓
- Host A's new merger skips own slice but reads B's slice (B doesn't dual-write yet). ✓
- Host B's old post writes only to slice. A's merger reads B's slice. ✓
- Host A's old merger (if running on the new bin pre-rollout) would still merge own slice — would duplicate the dual-write. Avoid this by gating the "skip own slice" merger change behind a config or a version sniff.

**Recommendation:** ship the post-side dual-write first as v0.3.5 (no merger change yet). Merger continues to dedupe-by-cursor from own slice, so dual-write yields a brief "duplicate own messages" window until the merger's own-slice cursor catches up to the slice EOF that post already populated. Mitigated by initializing the own-slice cursor to current EOF on merger startup. Then ship the "merger skips own slice" change as v0.3.6 once all hosts are on >= v0.3.5. Two-stage roll-out is overkill for Mick's current 2-host swarm but matters for any future 5+ host configuration.

---

## 6. Implementation sketch

### 6.1 `src/post.rs`

```rust
// Today (line 69-81):
let write_path = match (cfg_opt.as_ref(), channel_entry) {
    (Some(cfg), Some(ch)) if !cfg.channel_is_local(ch) => {
        let this_host = cfg.this_host.as_deref().ok_or_else(|| anyhow!(...))?;
        slice_path(&merged_path, this_host)
    }
    _ => merged_path.clone(),
};
append_frame(&write_path, &frame)?;

// Proposed:
let (primary, secondary) = match (cfg_opt.as_ref(), channel_entry) {
    (Some(cfg), Some(ch)) if !cfg.channel_is_local(ch) => {
        let this_host = cfg.this_host.as_deref().ok_or_else(|| anyhow!(...))?;
        // slice first for crash-safe ordering (§5)
        (slice_path(&merged_path, this_host), Some(merged_path.clone()))
    }
    _ => (merged_path.clone(), None),
};
append_frame(&primary, &frame)?;
if let Some(secondary) = secondary {
    if let Err(e) = append_frame(&secondary, &frame) {
        // Surface but don't fail the call — slice succeeded, sync will ship,
        // merger own-slice fallback will catch up local visibility eventually.
        eprintln!("post: warning — slice write OK but merged write failed: {e}");
    }
}
```

`append_frame` needs to acquire the per-merged-file lock when writing to `secondary` (the merged file) to prevent torn writes with merger ticks. The slice write doesn't need the lock — it's still single-writer.

### 6.2 `src/merger.rs`

Two staged changes:

**v0.3.5 (no behavior change for merger):**
- Initialize own-slice cursor to current slice EOF on first refresh_tracked tick after startup, not to 0. Prevents double-append of bytes post just wrote to merged.

**v0.3.6 (merger drops own slice):**
- `compute_active_channels` no longer includes `this_host` in slice_hosts when post is known to dual-write. Gated by a config flag `[transport].dual_write` defaulting to true (we control both sides).
- Saves merger work; eliminates the v0.3.5 cursor-initialization trick.

### 6.3 `src/sync.rs`

No change. Slice is still the wire format between hosts.

### 6.4 `src/watch.rs`

No change. Still tails merged file.

---

## 7. Test plan

| # | Test | Verifies |
|---|---|---|
| T1 | `post_dual_writes_to_slice_and_merged_for_cross_host_channel` | Cross-host channel → post call leaves both files with the frame. Existing fixtures + new assertion. |
| T2 | `post_writes_slice_first_then_merged` | Crash-safe ordering. Mock the second write to fail and assert slice has the frame. |
| T3 | `post_returns_ok_even_when_merged_write_fails` | Slice success is sufficient for caller success. Stderr warning emitted. |
| T4 | `merger_skips_own_slice_when_dual_write_enabled` | (v0.3.6) Merger doesn't append own slice bytes; merged file size matches sum of post-direct writes + peer-merge writes. |
| T5 | `cross_host_channel_local_post_visible_without_merger` | The headline use case. Start a swarm with no merger; post on cross-host channel from local agent; local watcher sees the post within 100ms. |
| T6 | `concurrent_post_and_merger_no_torn_frames` | Lock-correctness. Run post and merger concurrently on the same merged file; verify frame-by-frame parse round-trip. Extend swarm_chaos suite. |
| T7 | `dual_write_roundtrip_old_merger_with_new_post` (v0.3.5 backcompat) | Old-merger-on-peer + new-post-on-local: peer eventually sees all messages exactly once after one merger tick post-rollout. |

T5 is the test that demonstrates Mick's "adding one remote agent must not disrupt local comms" requirement is met.

---

## 8. Out of scope / future work

- **Stale-merger detector + in-process fallback (Alt E)** — only build if dual-write turns out to leave a real-world gap.
- **Cross-host channel garbage collection** — when an agent is removed, dual-write doesn't need to know; slice files just stop receiving new frames.
- **Slice rotation / compaction** — slice files grow without bound today; dual-write doesn't change this. Tracked separately if it becomes operational pain.
- **Transactional dual-write (both or neither)** — using a journal or fsync(2) barrier. Significant implementation cost; current "slice first, surface partial-failure" semantics are acceptable for a swarm-coordination tool where re-posting on error is the normal user workflow.

---

## 9. Decision points for Mick

1. **GO/NO-GO on dual-write as proposed?** Or prefer Alt A (just harden merger) / Alt E (defer + add stale detector)?
2. **Two-stage roll-out (v0.3.5 = post-side change; v0.3.6 = merger-side change) vs one-shot v0.3.5 with both changes?** Two-stage is safer for any future 3+ host swarm; one-shot is simpler given the current 2-host topology.
3. **Lock implementation for the post-vs-merger race on merged file:** advisory `flock` (POSIX-portable, works over rsync no-op semantics) vs a lockfile in giga_home (matches existing cursor file pattern). Recommend `flock` — narrower scope, OS-level.
4. **Anything missing from the test plan?** T5 is the critical one but easy to construct; T6 needs swarm_chaos extension.

---

## 10. Estimate

- Design review + iteration on this doc: short (Mick driving direct)
- v0.3.5 implementation (post-side dual-write, merger cursor init): ~2-3 hours including new tests
- v0.3.6 merger drop-own-slice: ~1-2 hours including T4
- Rollout / observation on the morpheus-wsl swarm: depends on Mick's next multi-host attempt

Net: a half-day of focused engineering closes the architectural coupling that produced last night's revert.
