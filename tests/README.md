# `tests/` — Integration & chaos test suite

End-to-end and concurrency-chaos tests for giga's file-based coordination pipeline. These four Cargo integration tests are the ground-truth regression guard for the no-database, plain-text coordination model: append atomicity, merger idempotency / no double-delivery, per-author ordering, watcher self-filtering, and cursor persistence across restart.

## Role in the system

Everything here drives the **real `giga` binary as a subprocess** (via `env!("CARGO_BIN_EXE_giga")`) rather than calling library functions, so the tests validate the exact code paths real agents hit: `giga post`, `giga merger --once`, `giga sync --once [--dry-run]`, `giga watch`, and `giga sweep`. They are split into three concerns: **local single-host concurrency invariants** (`swarm_chaos.rs`), the **cross-host slice-and-merge pipeline** with the rsync transport faked by `fs::copy` (`cross_host_e2e.rs` sequential, `cross_host_chaos.rs` concurrent), and the **git transport** driven against a real bare local git repo (`git_transport_e2e.rs`). Each file is compiled as its own test binary; every test is fully isolated via a `tempfile::TempDir` plus a pinned `HOME` so `~/.giga` cursor/lock state never cross-contaminates (critical, since the cross-host tests simulate two physical hosts inside one process and each "host" needs its own `$HOME`).

These tests sit downstream of the unit tests embedded in `src/` (e.g. `src/sync.rs`'s pure-planner tests, `src/merger.rs`'s `merge_tick` tests) and upstream of the live multi-host smoke described in `design/REMOTE_DESIGN.md` §6 step 10. They cover what unit tests cannot stress (real concurrent processes hitting one file) and what the live smoke is too expensive to run on every CI pass.

## File index

| File | Lines (approx) | Purpose |
| --- | --- | --- |
| [`swarm_chaos.rs`](./swarm_chaos.rs) | ~616 | Local-mode-only (no `[[hosts]]`, no sync, no merger) concurrency chaos: concurrent `giga post` + a live `giga watch` child. Asserts no-clobber, watcher fire-once + self-filter, cursor persistence, write atomicity around `PIPE_BUF`, and `giga sweep` safety under live traffic. |
| [`cross_host_e2e.rs`](./cross_host_e2e.rs) | ~547 | Sequential end-to-end tests of the cross-host slice-and-merge pipeline with rsync faked by `fs::copy`. Covers the v0.3.5 dual-write invariant, bilateral round-trip, merger idempotency, incremental growth, the `sync --dry-run` plan, and legacy local-only fallback. |
| [`cross_host_chaos.rs`](./cross_host_chaos.rs) | ~546 | Concurrent cross-host chaos (plan items R1–R3). Stresses the post → slice → sync-by-copy → merger → merged pipeline under racing load: all posts arrive on both hosts (R1), a slice is always a growing prefix mid-append (R2), the merger never double-delivers under racing growth (R3). |
| [`git_transport_e2e.rs`](./git_transport_e2e.rs) | ~344 | End-to-end test of the git transport plug (`src/transports/git.rs`) against a real bare local git repo as the shared state repo. Covers one-way push+pull of a slice, bidirectional round-trip, and no-op idempotency when nothing changed. |

## Files

### `swarm_chaos.rs`

**Purpose.** Local-mode-only chaos suite. Existing unit coverage runs single-threaded; the watcher's `len > last_size` invariant and `append_with_lock`'s exclusive-lock-plus-seek write atomicity (with the `O_APPEND` fallback in `append_plain`) only get stressed under real concurrency. These five tests fill that gap by spawning N concurrent `giga post` subprocesses plus a real `giga watch` child and asserting post-level invariants at the end.

**Harness / fixtures.**
- `struct LocalFixture { _tmp, home, config_path, inbox }` — per-test `TempDir` + isolated `HOME`; method `channel_path(file)` joins the inbox dir.
- `fn simple_local_swarm() -> LocalFixture` — writes a 2-agent (alice + bob) **local-only** `giga-harness.toml` with one bilateral channel `alice-bob.md` and **no `[[hosts]]` block**.
- `fn giga_post(home, config, sender, subject, body) -> Result<(), String>` — runs `giga post alice-bob.md --as <sender> --subject … --body … --config …`; returns `Err` (does not panic) so callers can tell a deliberate race-loser from an infrastructure break.
- `struct Header { sender, subject }` + `fn parse_headers(text) -> Vec<Header>` — file-order header extraction; `fn is_post_header(line)` replicates the `src/watch.rs::is_header_line` predicate; `fn count_delimiter_triples` counts `===` triples (`#[allow(dead_code)]`).
- `fn spawn_watcher` / `fn stop_watcher` — start/kill a `giga watch --as alice` child whose stdout is captured to a log file.

**Tests & invariants.**
- `local_no_clobbering_under_concurrent_post` — 100 posts/agent × 2 racing. Asserts exactly `2*100` headers **and** per-author subject monotonicity (`alice-msg-000..099` in file order, `bob-msg-…` likewise; cross-author interleave is allowed). This pins down append no-clobber under contention.
- `local_watcher_fires_for_every_concurrent_post` — N=10 bob posts + 2 alice posts concurrently against a live watcher running `--as alice`. Asserts the watcher log contains exactly 10 `[bob]` notifications and 0 `[alice] self-` (own posts are self-filtered).
- `local_watcher_cursor_persists_across_restart` — post 3, watch+stop, post 2, restart the watcher. Asserts 0 re-deliveries of the first 3 and exactly 2 new ones (cursor advanced **and** persisted to `~/.giga`).
- `local_post_atomicity_around_pipe_buf_boundary` — 8 KB all-`A` vs all-`B` bodies racing 5× each (block size well above the 4 KB `PIPE_BUF` atomicity limit). Relocates each header in the text, samples the first 200 body chars, and asserts no body region contains the other author's char (no mid-block interleave).
- `local_sweep_under_active_traffic` — hammers `giga sweep <config>` ≥10 times in 3 s while 30 posts/agent flow. Asserts every sweep exits 0 and the final file has exactly `2*30` valid headers (sweep did not corrupt the channel).

**Control flow.** Each test builds a `LocalFixture`, spawns `std::thread` workers (each running a `giga` subprocess with `HOME` pinned to the fixture), joins them, then reads the merged inbox file and parses headers. Tests 2 and 3 additionally spawn a long-lived `giga watch` child writing to a log file and sleep across the watcher's ~3 s poll tick (8 s and 7 s waits = 2+ ticks) before killing it and grepping the log.

**Gotchas / invariants.**
- All timing is wall-clock and tick-dependent — comments flag flakiness on slow CI (watcher 3 s tick → 7–8 s waits; a 500 ms / 400 ms startup grace so posts after the watcher comes up aren't mistaken for history replay).
- `is_post_header` expects an exact 20-char ISO-8601 tail ending in `Z` with `-`, `-`, `T`, `:`, `:` at fixed byte offsets and rejects template placeholders beginning `[<`. `parse_headers` uses `rsplit_once(" — ")` on the 3-byte em-dash to stay on char boundaries.
- Test 4's `body_start` math hardcodes the timestamp tail length (`" — 2026-01-01T00:00:00Z"`) and the `"\n===\n\n"` envelope, so any header-format change would break the byte offset.

### `cross_host_e2e.rs`

**Purpose.** Sequential end-to-end tests for the cross-host slice-and-merge pipeline (`design/REMOTE_DESIGN.md` §6 step 8). Two hosts are simulated as two inbox dirs on the local filesystem; the rsync-over-Tailscale transport is faked by `fs::copy` of slice files at the points where sync would push. Validates the `post → slice → (fake sync) → merger → merged` path, including the v0.3.5 dual-write inversion.

**Harness / fixtures.**
- `struct Fixture { _tmp, home_a, home_b, host_a_swarm_dir, host_a_inbox, host_b_swarm_dir, host_b_inbox }` with methods `host_a_config()` / `host_b_config()` returning each host's `giga-harness.toml` path.
- `fn build_fixture() -> Fixture` — two hosts `wsl-a` / `wsl-b`, agents `alice@wsl-a` + `bob@wsl-b`, one bilateral channel `alice-bob.md`. The two TOMLs differ only in the `wsl_inbox` path; each host also gets a `this_host.toml`. **Each host gets its own `HOME`** so per-host merge cursors at `~/.giga/merge-cursors/<channel>/<host>.pos` don't clash (in production each physical host has its own `$HOME`).
- `fn giga(home, args) -> Output` — subprocess wrapper that panics on non-zero exit and pins `HOME`.
- `fn fake_sync_slice(src_inbox, dst_inbox, slice_filename)` — copies one slice file between inboxes (stand-in for an rsync push).

**Tests & invariants.**
- `post_dual_writes_to_slice_and_merged_no_merger_needed` — **v0.3.5**: a cross-host post writes BOTH the slice (`alice-bob.wsl-a.md`) and the merged file (`alice-bob.md`) byte-identically (`assert_eq!(slice_content, merged_content)`); a subsequent `merger --once` must NOT change the merged length (own slice is excluded from tracked slices). This is what makes local watcher visibility independent of merger-daemon liveness.
- `round_trip_bilateral_via_simulated_sync` — alice posts on A, fake-sync slice to B, B's merger merges, bob replies on B, reverse sync, A's merger merges; asserts both merged files contain both posts.
- `merger_idempotent_on_repeated_runs` — 3 consecutive merger runs yield exactly one `[alice] once` in the merged file.
- `incremental_slice_growth_appears_in_merged_on_next_tick` — post/merge then post/merge; both `first` and `second` are present.
- `sync_dry_run_prints_expected_plan` — `sync --once --dry-run` **stderr** must mention a `toml` push to `wsl-b.tail0.ts.net` AND a `slice` push of `alice-bob.wsl-a.md`, and must NEVER push to own host `wsl-a.tail0.ts.net`.
- `local_only_swarm_falls_back_to_direct_write` — a config with NO `[[hosts]]`: a post writes directly to the merged file and creates NO slice file (the inbox contains only `alice-bob.md`).

**Control flow.** Tests invoke the real `giga` binary with `--config` and per-host `HOME`, manually call `fake_sync_slice` to move slices between the two inbox dirs (decoupling from SSH / tailnet), then run `merger --once` on the receiving host and assert merged-file contents. `sync_dry_run_prints_expected_plan` asserts on captured **stderr** (dry-run lines go to stderr). The dual-write test captures merged length before/after a merger run to prove the merger leaves own-slice content untouched.

**Gotchas / invariants.**
- Encodes the v0.3.5 inversion (`design/REMOTE_DUAL_WRITE_DESIGN.md`): **post** owns the merged write for OWN posts, and the **merger** only merges PEER slices. Pre-v0.3.5 the merger was the sole merged writer.
- Two simulated hosts live in ONE process, so each needs its own `HOME`; sharing it would clash cursor files.
- `fake_sync_slice` does NOT model rsync atomicity or partial transfer — those are deferred to `src/sync.rs` unit tests (pure planner) and the live step-10 2-host smoke.
- Channel arg is passed **without** the `.md` extension here (`"alice-bob"`), unlike `swarm_chaos.rs` and `cross_host_chaos.rs` which pass `"alice-bob.md"`.

### `cross_host_chaos.rs`

**Purpose.** Concurrent cross-host chaos suite (plan items R1–R3); companion to `swarm_chaos.rs` (local) and `cross_host_e2e.rs` (sequential). Simulates two hosts as two inbox dirs, fakes rsync with a **push-own-slices-only** `fs::copy`, and stresses the `post → slice → sync-by-copy → merger → merged` pipeline under concurrent load.

**Harness / fixtures.**
- `struct CrossHostFixture { _tmp, home_a, home_b, cfg_a, cfg_b, inbox_a, inbox_b }` + `fn build_fixture()` — the same 2-host (`wsl-a` / `wsl-b`) alice + bob fixture as `cross_host_e2e.rs`, with per-host `HOME` for cursor isolation.
- `fn giga_post(home, config, sender, subject, body) -> Result` and `fn giga_merger_once(home, config) -> Result` — subprocess wrappers (channel passed **with** `.md`: `"alice-bob.md"`).
- `fn fake_sync_tick(inbox_a, inbox_b)` + `fn push_own_slices(src, dst, own_host)` — the **push-own-only** sync model: each host only copies its own `.<host>.md` slices to the peer. This mirrors `src/sync.rs::compute_sync_plan`'s single-writer invariant and is *load-bearing*: a naive bidirectional copy would round-trip a stale snapshot of the peer's slice back over the peer's actively-growing slice.
- `struct Header` + `fn parse_headers` + `fn is_post_header` — the same header parser as `swarm_chaos.rs`.

**Tests & invariants.**
- `r1_concurrent_cross_host_posts_all_arrive` — 30 posts/agent × 2 racing while a pump thread runs `fake_sync_tick` + a merger on both hosts every 100 ms; after stopping the pump it drains 5 explicit tick rounds. Asserts each host's merged file has exactly 30 alice + 30 bob posts AND per-author subject monotonicity (`alice-00..29`, `bob-00..29`).
- `r2_slice_file_is_always_a_complete_prefix_while_appended` — a writer posts 50 over ~3 s; a reader snapshots the slice every 15 ms into a `Vec<Vec<u8>>`. The snapshots are sorted by length and each shorter must be a byte-prefix of each longer (no mid-write split or truncate). The final slice parses to exactly 50 headers. This is a POSIX-read-invariant proxy: it would catch any future regression where `giga post` splits one block into multiple `write_all` calls.
- `r3_merger_no_double_delivery_under_racing_slice_growth` — a writer posts 80 over ~4 s while a merger pump runs `merger --once` every 60 ms; after draining one final merger run, asserts the merged file has exactly 80 unique `r3-NNN` subjects, no duplicates, all present.

**Control flow.** R1 launches two poster threads (`alice@A`, `bob@B`) plus a pump thread looping `fake_sync_tick → merger A → merger B`; the pump is stopped via an `AtomicBool` **before** the final drain so two mergers don't race on the same-host cursor files (a comment notes the under-delivery risk if one merger reads a stale cursor while another writes). R2 races a writer against a snapshot-collecting reader, then verifies the growing-prefix property. R3 races a writer against a merger pump, drains one final merger after the writer finishes, then dedups subjects.

**Gotchas / invariants.**
- Explicitly does NOT test real rsync `--append-verify` atomicity, Tailscale SSH failures, or SSH auth — those need a live peer (covered by the step-10 smoke and the pure `src/sync.rs` planner unit tests).
- R3 documents the merger's read-delta contract: the buffer size is fixed from `fs::metadata().len()` at tick start (`src/merger.rs:115`), `read_delta` (`src/merger.rs:276`) reads exactly that many bytes, and any growth past that point defers to the next tick — so concurrent growth never double-delivers and never loses bytes.
- Flakiness guards: `assert!(snapshots.len() >= 5)` (R2) and `assert!(merger_iterations >= 10)` (R3); the sleeps are tuned with margin for slow CI.

### `git_transport_e2e.rs`

**Purpose.** End-to-end test of the git transport plug (`src/transports/git.rs`) against a **real local bare git repo** as the shared state repo — no network, auth, or GitHub. Mirrors production: each host has its own clone of the bare repo, and `giga sync --once` runs the real git tick (`pull --rebase`, mirror peer slices repo→inbox, mirror own slice inbox→repo/slices, commit, push).

**Harness / fixtures.**
- `struct GitFixture { _tmp, bare_repo, home_a, home_b, cfg_a, cfg_b, inbox_a, inbox_b, clone_a, clone_b }`.
- `fn build_git_fixture() -> GitFixture` — `git init --bare --initial-branch=main` for the state repo, then a **seed clone** commits `README.md` and pushes `main` (so `git pull --rebase` has a base; an empty bare repo errors on pull with "couldn't find remote ref HEAD"). Config uses `[transport] kind = "git"` with `[transport.git] state_repo = <bare>` + `local_clone_dir = <per-host clone>`. Both configs point at the same `state_repo` URL but distinct clone + inbox dirs.
- `fn run_git(cwd, args)` — git subprocess helper, panics on failure.
- `fn giga(home, args) -> Output` — runs `giga` with `HOME` pinned AND `GIT_AUTHOR_*` / `GIT_COMMITTER_*` env set, so commits don't need per-clone `user.email` / `user.name` config.

**Tests & invariants.**
- `git_tick_pushes_own_slice_to_repo_and_pulls_peer_slice_from_repo` — alice posts on A; A's tick mirrors the slice into `clone_a/slices/alice-bob.wsl-a.md` and pushes; B's tick pulls and mirrors alice's slice into `inbox_b`, which then contains `[alice] hello`.
- `git_tick_bidirectional_round_trip` — alice on A + bob on B both post, then tick A,B twice (so both directions propagate through the single shared remote); asserts both inboxes end up with both `wsl-a` and `wsl-b` slices, A sees `[bob] pong`, B sees `[alice] ping`.
- `git_tick_is_noop_when_no_changes` — the first tick clones+seeds, two more ticks succeed silently (no commit, no push); passes if all `giga` calls return 0. Relies on the commit-skip-when-clean behaviour in the git transport's `tick`.

**Control flow.** Each `giga sync --once` invocation drives `GitTransport::tick` (`src/transports/git.rs:262`): `ensure_clone` on the first call, `git pull --rebase`, mirror peer slices repo→inbox via `append_growth`, mirror own slice inbox→repo via `append_growth` (`copy_if_different` is used only for the canonical `giga-harness.toml`), then `git add -A` / `commit --quiet` / `push --quiet` (skipped when nothing changed). Tests assert on slice presence/content in the clones, the inboxes, and (sanity) the bare repo. The bidirectional test ticks A,B twice to let both directions round-trip through the single shared remote.

**Gotchas / invariants.**
- Real `git` is a hard test dependency (spawns the `git` binary); `--initial-branch=main` pins the branch regardless of the runner's `git.defaultBranch`.
- The empty-bare-repo pull failure is the reason for the seed commit.
- `GIT_AUTHOR_*` / `GIT_COMMITTER_*` env vars substitute for per-clone identity config.
- Slices live under a `slices/` subdir in the repo (`clone_<x>/slices/<channel>.<host>.md`), single-writer per host. Channel arg is passed **without** `.md` (`"alice-bob"`).

## Data & control flow

All four files share the same skeleton: write a `giga-harness.toml` (and, for cross-host, a `this_host.toml`) into a `TempDir`, then drive the real `giga` binary as a subprocess with `HOME` pinned for cursor isolation, and assert on the resulting Markdown channel files.

The cross-host model under test is **slice-and-merge**:

1. `giga post` appends a message block to the author's single-writer **slice** file `<channel>.<host>.md`. On a cross-host channel it *also* dual-writes the same bytes to the local **merged** file `<channel>.md` (v0.3.5).
2. A transport (`fs::copy` fake in the chaos/e2e tests, real `git` in `git_transport_e2e.rs`) pushes each host's own slices to peers and pulls peer slices in.
3. `giga merger --once` reads the *delta* of each tracked **peer** slice (buffer sized from `metadata().len()` at tick start) and appends it verbatim to the merged file, advancing a per-slice cursor under `~/.giga/merge-cursors/`.
4. `giga watch` tails the merged file, self-filters the watching agent's own posts, and persists a cursor so restarts don't re-deliver.

`swarm_chaos.rs` exercises the degenerate local-only case (steps 2–3 are no-ops; post writes the merged file directly and the watcher tails it). `cross_host_e2e.rs` and `cross_host_chaos.rs` exercise the full slice→sync→merge path with the transport faked. `git_transport_e2e.rs` swaps the fake transport for the real git tick. The header format produced by `giga post` is the contract that ties them together: the tests' `is_post_header` / `parse_headers` must stay in lock-step with `src/watch.rs::is_header_line` and the post-rendering code.

## Cross-references

Source under test (SUT):

- [`../src/post.rs`](../src/post.rs) — `giga post`; `append_with_lock` (exclusive file lock + explicit seek-to-end, with a plain `OpenOptions::append(true)` fallback in `append_plain`) is the SUT for all post-atomicity / no-clobber assertions — the lock is what holds atomicity above the 4 KB `PIPE_BUF` limit, since `O_APPEND` alone only serializes ≤ `PIPE_BUF` writes.
- [`../src/merger.rs`](../src/merger.rs) — `giga merger`; `merge_tick` (line 112), `read_delta` (line 276), `ChannelMergeState`, and merge-cursor persistence at `~/.giga/merge-cursors/<channel>/<host>.pos` — SUT for idempotency / no-double-delivery.
- [`../src/sync.rs`](../src/sync.rs) — `giga sync`; `compute_sync_plan` pure planner (line 343) — SUT for `sync --dry-run` plan output; its single-writer / push-own invariant is what `fake_sync_tick` mirrors.
- [`../src/transports/git.rs`](../src/transports/git.rs) — `GitTransport::from_config` / `ensure_clone` / `tick` / `slice_plan`, `append_growth`, `copy_if_different`, `default_clone_dir` — SUT for `git_transport_e2e.rs`.
- [`../src/transports/`](../src/transports/) — `mod.rs`, `local.rs`, `rsync_tailscale.rs`: the transport-plug abstraction selected by `[transport] kind`.
- [`../src/watch.rs`](../src/watch.rs) — `giga watch`; `is_header_line` (line 646), which the tests' `is_post_header` replicates; cursor / self-filter logic.
- [`../src/sweep.rs`](../src/sweep.rs) — `giga sweep`; SUT for `local_sweep_under_active_traffic`.
- [`../src/cursor.rs`](../src/cursor.rs) — `giga_home`, merge/watcher cursor read+write under `~/.giga` (the reason each simulated host needs its own `HOME`).
- [`../src/config.rs`](../src/config.rs) — the `Config` / TOML schema (`[project]`, `[paths].wsl_inbox`, `[[hosts]]`, `[[agents]]`, `[[channels]]`, `[transport]` / `[transport.git]`) that every fixture writes.
- [`../src/main.rs`](../src/main.rs) — CLI dispatch for the `post` / `merger` / `sync` / `watch` / `sweep` subcommands invoked as subprocesses.

Design docs:

- [`../design/REMOTE_DESIGN.md`](../design/REMOTE_DESIGN.md) — §6 step 8 e2e tests, §6 step 10 live 2-host smoke, and the remote-mode chaos follow-ups these files implement.
- [`../design/REMOTE_DUAL_WRITE_DESIGN.md`](../design/REMOTE_DUAL_WRITE_DESIGN.md) — the v0.3.5 dual-write inversion (post owns the merged write for own posts; merger merges peer slices only) asserted by `post_dual_writes_to_slice_and_merged_no_merger_needed`.
- [`../design/TRANSPORT_DESIGN.md`](../design/TRANSPORT_DESIGN.md) — the transport-plug abstraction exercised by `git_transport_e2e.rs`.

Other docs:

- [`../README.md`](../README.md) — top-level project overview.
- [`../docs/COMMAND_REFERENCE.md`](../docs/COMMAND_REFERENCE.md) — the `post` / `merger` / `sync` / `watch` / `sweep` subcommand surface these tests drive.
