# REMOTE_DESIGN.md — remote-enabled channels for giga-harness

**Author:** giga agent
**Date:** 2026-05-30
**Status:** Mick GO 2026-05-30/31. Transport: rsync over Tailscale SSH. Config: single canonical TOML rsync'd between hosts (Q6 a — operator-driven UX makes (b)'s write-isolation unnecessary). Driving direct (design relay paused). First proving target: a fresh small remote-test swarm, NOT an extension of production superdeduper.
**Scope:** Detailed plan, not exhaustive prose. Targets one architecture committed end-to-end.

---

## 1. Problem

`giga-harness` today coordinates ~17 agents on a single machine via append-only Markdown channel files in `paths.<side>_inbox`. A polling watcher (3s tick) tails each channel; a `giga post` call appends a header block via `O_APPEND`; auto-discovery rereads the config every ~15s so new channels appear without watcher restart. The model is robust and well-loved; it must not change for all-local setups.

We need a **parallel remote-enabled path** so agents on different physical machines can post and watch the same channels as if they were local. Concrete target: a 10-agent swarm split across 2 hosts (e.g., 8 on a WSL box + 2 on a Windows-native box, or 8 on one Linux + 2 on a cloud VM), with the design extending cleanly to 3-5 hosts and not painting into a corner at 10+.

**Hard constraints (Mick):**
- Don't break the local-file model. Existing all-local swarms keep working unchanged.
- Parallel path, not a replacement. The remote case COEXISTS with local.
- Seamless from the agent's perspective. `giga post` Just Works either way.

**Hard constraints (derived from code reading, ranked):**
1. **Append-only is sacrosanct.** The watcher uses `len() > last_size` delta scanning. Any mutation or shrink corrupts notification parsing.
2. **Watcher is polling, not push.** A local writer is transparently picked up within one 3s tick. No watcher changes needed if remote writes appear as local appends.
3. **`[<me>] ` prefix is the echo-filter.** Agent identity must be globally unique across the swarm. Confirmed by design (single trust domain).
4. **No `host` concept in the schema today.** Channel `side` is `wsl|windows`; agents have `platform` but no `host`. Adding remote requires a new axis.
5. **Busy-lock + cursors are per-agent local state.** Never touch a peer host's `~/.giga/<agent>/...`.

---

## 2. Recommended architecture: slice-and-merge

**Pattern: per-host write slices + a local merger that appends to the watched logical channel file.**

### 2.1 Disk layout per host

Under `paths.<side>_inbox/`:

```
<channel>.md                   ← local merged view. WATCHER TAILS THIS. Unchanged from today.
<channel>.<this-host>.md       ← local slice. Single-writer (this host's `giga post`).
<channel>.<peer-host>.md       ← peer slice(s). Single-writer-remote, append-only-via-sync, never written locally.
```

### 2.2 Three components

| Component | Status | Role |
|---|---|---|
| **post** | existing, lightly modified | When channel has remote participants, append to `<channel>.<this-host>.md`. When all participants are on this host, fall back to direct write to `<channel>.md` (zero overhead — slice IS the channel file). |
| **merger** | NEW | Polls all `<channel>.<*>.md` slice files. Appends new header blocks (and their body blocks) to `<channel>.md` in receive-order. Idempotent via per-slice cursors at `~/.giga/merge-cursors/<channel>/<host>.pos`. Skips its own slice in the simple model (or includes it; either works). |
| **watch** | existing, **UNCHANGED** | Tails `<channel>.md` exactly as today. Remote messages arrive as ordinary appends. |
| **sync** | NEW | Mirrors slice files between hosts via a transport (§ 4). The merger and the sync are independent; either can lag without breaking the other. |

### 2.3 Why slice-and-merge (and not "mirror the channel file directly")

Direct-mirroring `<channel>.md` is **unsafe under any concurrent write**:
- `rsync`: conflict → manual resolution.
- S3: last-writer-wins LOSES one host's posts silently.
- git: merge conflict → broken append-only.
- Even with deterministic concat: re-publishing changes file length non-monotonically, and the receiving host's watcher reads either a shrunk file (`last_size` resets) or a mid-block delta (`is_header_line` misparses).

Per-host slice files are **single-writer by construction.** No conflict is possible. The merger is the sole writer to the logical channel file, so the append-only invariant the watcher depends on is preserved exactly.

### 2.4 Ordering semantics (design-approved Q5 (b))

Per-host order + late merge. Each host appends its own posts to its own slice immediately; remote posts arrive when sync catches up and the merger flushes them. The two hosts' merged channel files end up with the same MESSAGES but possibly in different ORDERS. Agents tolerate this because:
- Every header carries a UTC timestamp; consumers order by timestamp mentally anyway.
- The swarm coordinates via `WAITING ON:` tags + subject lines, not via "the message immediately before mine."

### 2.5 Failure modes

| Failure | Behavior |
|---|---|
| Sync down for N minutes | Local watcher keeps firing for local-host posts. Peer events backlog in their slice files (still being written remotely) and flush at sync recovery. Append-only preserved. |
| Merger crash mid-tick | Cursor not yet advanced (we copy the watcher pattern: advance AFTER append). Next tick reprocesses the same range; same bytes get re-appended. Idempotence comes from cursor placement, not from content dedup. |
| Bad-actor host posts garbage to its slice | Contained to that slice. Auth lives in the transport (§ 5). Worst case: garbage bytes appended to local merged file; visible in `giga sweep`; never corrupts cursor/lock state. |
| Two hosts add the same agent name | Caught by validation at next config-merge (§ 3). Manual resolution; no silent data loss. |
| Network partition | Each side keeps working locally. At reconciliation, slice files diverge by deterministic per-host suffix → no merge conflict; just delayed delivery. |

### 2.6 Latency budget

CLAUDE.md target: ≤5s post-to-fire. Achievable with cloud-storage transport at 3s poll interval (matches the watcher tick). End-to-end:
- post → local slice (instant)
- sync push (≤3s)
- transport propagation (≤1s for S3-class storage)
- sync pull on peer (≤3s)
- merger tick (≤3s)
- watcher tick (≤3s)

Worst case ~10-12s with 3s ticks; typical 4-7s. Tightening to ≤5s typical means dropping to 1s poll, which is fine on local FS but doubles cloud-storage API costs. Recommend 3s ticks for v1; tune later if Mick wants tighter.

---

## 3. Config + operator UX

**Decision (Mick, 2026-05-30/31): single canonical `giga-harness.toml` rsync'd between hosts (Q6 a).** Originally recommended (b) per-host slices for write-isolation, but Mick's operator UX is "control plane on one machine, workers elsewhere" — the operator runs `giga` commands from his primary host A; B is a worker. Single authoritative writer eliminates the concurrent-edit hazard that motivated (b). Saves ~3 engineering days.

If an agent ever needs to write the TOML from a worker host (e.g., a B-resident agent calls `add-agent`), it shells back to A via `giga remote --host A add-agent ...` — same SSH primitive in reverse.

### 3.1 Schema (canonical TOML, rsync'd between hosts)

```toml
[project]
name = "remote-test"          # first target: a fresh test swarm, not superdeduper

[paths]
wsl_inbox = "/home/neomatrix/projects/inbox-remote-test"   # SAME relative path on every host

[[hosts]]                     # NEW: enumerate every host in the swarm
name = "wsl-neo"
tailnet_hostname = "wsl-neo.tail....ts.net"
ssh_user = "neomatrix"        # the OS user on this host (usually same everywhere)

[[hosts]]
name = "wsl-box-b"
tailnet_hostname = "wsl-box-b.tail....ts.net"
ssh_user = "neomatrix"

this_host = "wsl-neo"         # NEW: which [[hosts]] entry am I? Set on each host's local copy.

[[agents]]
name = "design"
host = "wsl-neo"              # NEW: which host this agent runs on (defaults to this_host)
workdir = "/home/neomatrix/.giga/configs/remote-test/workdirs/design"
role = "..."
platform = "wsl"

[[agents]]
name = "code-2"
host = "wsl-box-b"            # remote agent
workdir = "/home/neomatrix/.giga/configs/remote-test/workdirs/code-2"
role = "..."
platform = "wsl"

[[channels]]                  # unchanged shape; spans hosts based on participants
file = "design-code-2.md"
side = "wsl"
participants = ["design", "code-2"]
```

Each host has the same canonical TOML except for `this_host = "..."` which is local-only. v1 implementation: keep `this_host` in a separate one-line file `~/.giga/configs/<swarm>/this_host.toml` so rsync of the canonical doesn't trample it. (Alternative: env var `GIGA_THIS_HOST`. Either works; one-line file is more discoverable.)

### 3.2 Operator CLI — the 4 commands Mick asked for

```sh
# 1) Setup on each machine
#    Operator host: probably already set up.
#    New remote node (bare WSL):  giga setup --remote-node

# 2) From A: start a new agent on B
giga add-agent --host wsl-box-b --name code-2 \
               --role "code agent on box B" \
               --peer design

# 3) From A: connect existing local agent to existing remote agent
giga add-channel --participants design,code-2
# (also covered by add-agent --peer when adding the agent itself)

# 4) From A: list agents + channels on B
giga sweep --host wsl-box-b
giga ls    --host wsl-box-b                    # NEW: agents + channels in one view
```

Underneath, every `--host <host>` flag is sugar for one new primitive:

```sh
giga remote --host wsl-box-b <any-giga-subcommand-with-args>
# shells to `ssh <host>.tail....ts.net giga <args>` over tailscale SSH
# streams stdout/stderr back transparently
```

So `add-agent --host B ...` is really:
  (i) update the canonical TOML on A,
  (ii) `giga sync` pushes the new TOML to B (or it lands at next sync tick),
  (iii) `giga remote --host B launch --only code-2` brings up the tab on B.

`sweep --host B` is just `giga remote --host B sweep`. Etc.

### 3.3 Validation rules

- Every `[[agents]].host` must resolve to a `[[hosts]].name`.
- Every `[[channels]].participants` entry must resolve to some `[[agents]].name`.
- Every `[[hosts]].name` must be unique; exactly one `this_host` per host's local copy.
- A channel where ALL participants live on the same host gets fast-path local mode (no slicing).
- A channel where participants span 2+ hosts gets slice-and-merge mode.

### 3.4 What gets rsync'd between hosts

The canonical `giga-harness.toml` + the per-host channel slice files (`<channel>.<host>.md`). Both via the same rsync-over-Tailscale-SSH transport (§ 4). `this_host.toml` and `~/.giga/cursors/*` and `~/.giga/busy/*` are local-only — never sync'd.

---

## 4. Transport: rsync over Tailscale SSH (v1)

**Decision (Mick, 2026-05-30/31): rsync over Tailscale SSH.** Mick has a tailnet; enabling `tailscale set --ssh` on each host gives mutual SSH reachability with NO key exchange — auth is tailnet identity. Bootstrap is O(N) per-host commands instead of O(N²) per-pair `ssh-copy-id` runs.

### Concrete v1 implementation

`sync.rs` (new module): long-running daemon. Every poll tick (3s default):
- **Push the canonical TOML** if it changed since last tick: `rsync -avz <canonical-toml> <peer-tailnet>:<canonical-toml-path>` for each peer.
- **Push own slice files**: `rsync -avz --append-verify <inbox>/<channel>.<this_host>.md <peer-tailnet>:<inbox>/` for each peer that has agents on a channel this host participates in. `--append-verify` makes rsync trust that already-transferred bytes are unchanged (true for append-only slices) and only ship the tail.
- **Pull**: symmetric — peers push to us. We don't pull; we receive their pushes. Preserves single-writer-per-slice at the wire level.

Why push-only-own-slices: a host can only ever modify its own slice; no remote process is rewriting our local data. Safe under any failure.

### N-host scaling note

Push-to-all-peers is O(N²) connections per tick. Fine to ~5 hosts (Mick's stated lean). At 10+ hosts a hub-and-spoke (one host designated as fanout) is straightforward; deferred until needed.

### Future transport: cloud-storage (v1.1)

`sync.rs` accepts a transport URL: `rsync+tsssh://<tailnet-hostname>/<inbox-path>/` for v1. The interface is left open so `s3://...` can plug in next (Mick wants both supported). Cloud-storage covers the cases where tailnet isn't available — adding hosts outside the tailnet, NAT-traversal where Tailscale isn't installed, N>5 hub-and-spoke. v1.1 estimate: ~2-3 days after v1 ships.

---

## 5. Auth + identity

**Trust model (design Q2 answer): single trust domain.** All hosts are Mick-controlled. No third-party-collaborator hosts in v1.

### Concrete mechanism (v1, Tailscale SSH)

- Tailnet handles network identity + transport encryption (WireGuard). Reachability is implicit; no inbound port forwarding.
- **Tailscale SSH** (enabled on each host via `sudo tailscale set --ssh`) provides SSH access using tailnet identity. NO `authorized_keys`, NO key exchange, NO per-pair setup.
- Each host's SSH user owns its own inbox dir on every other host (write access scoped via filesystem perms — its own slice files only).
- Adding/removing peers = add/remove tailnet membership. Revocation is centralized in the tailnet admin console.
- No additional auth layer in v1. Tailnet membership IS authentication.

### Identity

- Agent names are globally unique across the swarm (design-confirmed).
- Host names are globally unique (enforced by config validation).
- A slice file's owner is encoded in its filename suffix; the host's posts to its own slice are implicitly authenticated (anyone with bucket creds can write any slice — that's the single-trust assumption).

### Future extension hook (not v1)

`sync.rs` transport interface accepts an `auth` parameter. v1 (rsync over Tailscale SSH) ignores it (tailnet handles everything). v1.1 (cloud-storage) uses a shared IAM key. v2 can introduce per-host signing keys (sign the slice tail with ed25519, verify on pull) for transports without inherent trust (e.g., public relay). v3 can add per-channel ACLs. Hooks present; bodies empty for v1.

---

## 6. Implementation steps

Ordered. Effort estimates are focused engineering days (one developer, no context switching). First proving target is a fresh `remote-test` swarm (NOT the production superdeduper) so we can't break anything load-bearing.

| # | Step | Effort | Notes |
|---|---|---|---|
| 0 | Scaffold a 2-agent `remote-test` swarm by hand (or via existing `giga setup`): one agent on host A, one on host B, `paths.wsl_inbox = ~/projects/inbox-remote-test` on both. Verifies the end-to-end target swarm structure before any code changes. | 0.5 day | Done with today's giga binary; only one agent per host so it's trivially "local" today. The remote bits get added below. |
| 1 | Add `[[hosts]]` + `this_host` + `[[agents]].host` schema to `config.rs`; extend validation. `this_host` lives in a separate `~/.giga/configs/<swarm>/this_host.toml` (single-line file) so rsync of the canonical doesn't trample it. | 1 day | Backward compat: missing `[[hosts]]` = local-only mode (today's behavior). |
| 2 | Implement `giga remote --host <host> <subcommand>`: SSH passthrough via tailnet hostname, stream stdout/stderr back, propagate exit code. | 0.5 day | Shells to `ssh <host>.tail....ts.net giga <args>`. ~50 LOC. |
| 3 | Modify `post.rs`: when channel spans hosts, write to `<channel>.<this_host>.md` slice. Otherwise unchanged. | 0.5 day | Two-line decision: are all `participants` on `this_host`? |
| 4 | Implement `merger.rs`: poll all `<channel>.*.md` slice files, append new blocks to `<channel>.md`, advance per-slice cursors at `~/.giga/merge-cursors/<channel>/<host>.pos`. Reuse `read_delta` + cursor utilities from `cursor.rs` / `watch.rs`. | 1.5 days | Closely follows `watch::run_multi` structure. ~150-200 LOC. |
| 5 | Implement `sync.rs` with rsync-over-Tailscale-SSH transport. Subcommand `giga sync` runs as a long daemon: every 3s, rsync the canonical TOML (if changed) + own slice files to each peer. | 1 day | ~150 LOC. Pluggable transport interface (cloud-storage / S3 plug deferred to v1.1). |
| 6 | Add `--host` flag to `add-agent`, `sweep`, `launch` (thin wrappers over `giga remote`). New subcommand `add-channel`. `add-agent --host B` is: update canonical TOML on A + `giga remote --host B launch --only <new>`. | 1 day | ~100 LOC total. |
| 7 | Update `launch.rs` to spawn the `Monitor` for `giga sync` + `giga merger` alongside `giga watch`. Update CLAUDE.md templates. | 0.5 day | Templates only; per-agent CLAUDE.md authors can opt out of either Monitor. |
| 8 | Integration tests: 2-host loopback against `localhost` over Tailscale SSH (or plain SSH for test env); post round-trip latency; sync down + recovery; concurrent post race; merger crash mid-tick replay. | 2 days | New test harness. ~300 LOC tests. |
| 9 | Docs: `REMOTE_QUICKSTART.md` operator runbook + `giga setup --remote-node` subcommand (installs Tailscale + runs `tailscale up` + enables Tailscale SSH + installs rsync + creates inbox dir). Supersedes the standalone setup-remote-peer.sh bash script committed earlier on this branch. | 1 day | Lower than the original estimate since less to set up (Tailscale SSH did most of the work). |
| 10 | Live 2-host smoke on the `remote-test` swarm: one agent posting on A, one on B, bilateral channel, watcher firing both directions for a full day. | 0.5 day | Real-world validation before extending to superdeduper. |

**Total: ~10 engineering days for v1.**

**Trimmed shortcut (if first live experience matters more than polish):**
- Skip step 9 (manual setup only): save 1 day
- Trim step 8 to "two-host happy path only, defer failure tests": save 1 day
- Skip step 10 (use as-needed): save 0.5 day
- **Trimmed total: ~6.5 engineering days for a working 2-host slice-and-merge + operator UX.**

After v1 ships and the `remote-test` swarm is healthy: extending production superdeduper to remote is just `giga add-agent --host <new-host> ...` from Mick's primary box. The full superdeduper TOML gains `[[hosts]]` entries, agents pick up `host = "..."` fields, and existing local channels keep their fast-path (all participants still on the same host).

**v1.1 (cloud-storage transport):** ~2-3 additional days for the S3 plug, once v1 is proven.

---

## 7. Decisions + remaining open questions

**Decided (Mick, 2026-05-30/31):**
- Q3: tailnet exists → transport is rsync over **Tailscale SSH** (auto-auth via tailnet identity; no key exchange).
- Q6: single canonical TOML rsync'd between hosts (option a). Originally went with (b); flipped after Mick's operator-UX spec made the single-writer model natural.
- Operator UX: control plane on the primary host; `--host <host>` sugar over a new `giga remote` SSH-passthrough primitive.
- First proving target: a fresh `remote-test` swarm (not extending production superdeduper until v1 is proven).
- Transports supported long-term: rsync over Tailscale SSH (v1) + cloud-storage (v1.1). Same `sync.rs` interface; just different plugs.
- Driving direct with Mick (design relay paused for this work).

**Still open, low-stakes — picking defaults unless Mick says otherwise:**
1. **Latency target.** Plan achieves typical 3-5s end-to-end with 3s ticks, worst case ~8-10s. If tighter, drop polls to 1s — negligible cost on tailnet.
2. **`this_host` storage.** Plan: one-line file at `~/.giga/configs/<swarm>/this_host.toml`. Alternative: env var `GIGA_THIS_HOST`. One-line file is more discoverable; defaulting to that.
3. **Subcommand naming.** Two new long-running daemons: `giga sync` (rsync transport) + `giga merger` (slice → merged file). Could collapse into one `giga sync` with both jobs; recommend keeping separate so each one's logs are cleanly attributable. Defaulting to two.

---

## Appendix: glossary

- **Slice file:** `<channel>.<host>.md`. Single-writer. The wire format for sync.
- **Merged file:** `<channel>.md`. What the watcher reads. Append-only, written only by the local merger.
- **Sync transport:** the mechanism that mirrors slice files between hosts. v1: rsync over SSH over tailnet. Pluggable for future cloud-storage / websocket / etc.
- **Slice cursor:** `~/.giga/merge-cursors/<channel>/<host>.pos`. Byte offset up to which the local merger has consumed a given slice. Per-channel-per-host, all local.
- **Local-only channel:** all participants on `this_host`. Fast-path: slice file IS the merged file. Zero remote overhead.

---

## Sign-off

Mick GO 2026-05-30/31. giga agent driving implementation direct against a fresh `remote-test` swarm. Doc updates done; step 0 of § 6 next.
