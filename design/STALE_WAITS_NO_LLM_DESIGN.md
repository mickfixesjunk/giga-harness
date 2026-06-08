# STALE_WAITS_NO_LLM_DESIGN.md

**Status:** draft, awaiting operator GO before code
**Author:** giga
**Date:** 2026-06-05
**Problem:** v0.6.17's stale-wait detection still costs ~1 LLM turn per stale wait surfaced. We want a zero-LLM variant: detection happens entirely in local subprocesses and disk; the operator (not any agent) decides when to act.

---

## 0. Context — what v0.6.17 already does

`giga watch` scans every tracked channel at arm time and on a 60s tick for unresolved `WAITING ON: <me>` tags older than `[watch].stale_wait_threshold_minutes`. Findings are written to **stderr**, which is exactly the stream Claude's `Monitor` tool reads — so each new finding fires a Monitor notification → agent turn → ~1k tokens minimum to absorb + decide.

That's already at the minimum for the "agent autonomously recovers" model. To go lower we have to take the agent out of the surfacing path entirely.

## 1. Design goal

| Metric | v0.6.17 | This design |
|---|---|---|
| LLM turns to detect a stale wait | 0 | 0 |
| LLM turns to surface it to a human | 1 (per supersede) | 0 |
| LLM turns to recover | 1 | 1 (when operator nudges, not before) |
| Detection latency | up to 60s (next re-scan) | up to N seconds (operator-chosen poll interval; could be sub-second on demand) |
| Operator-vigilance required | none | yes — operator must run / watch the surface command |

The trade is **vigilance for tokens**. Suits swarms whose operator is actively present (the giga-harness baseline) and whose token budget for "swarm health" is zero.

## 2. Architecture

### 2.1 What changes in the watcher

```
v0.6.17:
  watch loop → re-scan tick → eprintln!("STALE WAIT 47m ...")
                                  │
                                  └─► Monitor reads stderr → agent turn

v0.7.x:
  watch loop → re-scan tick → write_atomic(~/.giga/stale-waits/<slug>.tsv)
                                  │
                                  └─► (nothing — file just sits)
```

The arm-time stderr emission is **removed** (or gated behind a config flag, see §6). The periodic re-scan still runs but writes only to disk. Monitor sees nothing. No agent turn.

### 2.2 What gets written

Per-agent state file at `~/.giga/stale-waits/<slug>.tsv`. Atomic write (write to `.tmp`, rename over). Contents are the full current set of unresolved waits past threshold — overwritten each tick, not appended. This means the file's mtime is also the "freshness" signal.

Schema (one wait per line, tab-separated):

```
<tag_timestamp_iso>  <channel_file>  <sender_slug>  <age_minutes>  <subject>
```

Example:

```
2026-06-05T00:00:00Z	backend-design.md	backend	47	PR #43 ready for review
2026-06-04T23:30:00Z	infra-design.md	infra	77	worktree proposal — please review
```

Why TSV not JSON: greppable, awk-able, fits the giga "plain text on disk" aesthetic. JSON costs nothing here.

Why overwrite not append: append needs a resolution event to delete entries; overwrite means the file is always "current ground truth, derived this tick". Less state to maintain.

### 2.3 Where the operator sees it

New subcommand `giga doctor` (working name; could fold into `giga sweep --stale` if we prefer fewer top-level commands — see §7 Q1).

```
$ giga doctor
=== swarm health: 3 agents on this host ===
backend     ⏰ 1 stale wait
  backend-design.md  47m  [backend] PR #43 ready for review
infra       ⏰ 1 stale wait
  infra-design.md    77m  [infra] worktree proposal — please review
design      (none)

watcher liveness: all 3 daemons reporting within last 90s ✓
```

Reads every `~/.giga/stale-waits/*.tsv` on this host, formats, prints. Pure local I/O — no SSH, no LLM, no inter-agent traffic.

Add `--watch` mode: clears + re-prints every N seconds (like `htop`), so operator can leave it open in a tmux pane.

Add `--json` for tooling integration (PushNotification scripts, dashboards, slack bots).

### 2.4 Cross-host surfacing (optional, follow-up)

Single-host operators see everything via local `giga doctor`. For cross-host swarms, two options:

- **Operator-pull**: `giga doctor --host <name>` runs `giga doctor --json` over SSH. Per-host on demand.
- **Daemon-push**: existing `giga sync` daemon also rsyncs `~/.giga/stale-waits/<slug>.tsv` to peers under `~/.giga/stale-waits/<host>/<slug>.tsv`. `giga doctor` then aggregates.

Push variant has zero operator overhead but adds rsync chatter. Pull variant is on-demand only. Default: pull-only in v0.7.0; daemon-push as v0.7.1 if operator asks.

## 3. The watcher's job — pure-function rewrite

The arm-time and periodic re-scan logic in `src/watch.rs` collapses into:

```rust
fn write_stale_wait_state(slug: &str, waits: &[StaleWait]) -> Result<()> {
    let path = stale_wait_state_path(slug);
    write_atomic(&path, format_tsv(waits))
}
```

`scan_file` from `src/stale_wait.rs` is unchanged. The Monitor-shaped eprintln is deleted. The dedup HashSet (currently tracking "already surfaced this session") is also deleted — irrelevant when nothing fires.

Net effect on `watch.rs`: ~40 lines deleted, ~20 lines added (atomic write + path resolution).

## 4. The `giga doctor` command

Rough size:

- Module `src/doctor.rs` — ~150 lines (read files, format, optional watch mode, optional JSON).
- Clap arm in `src/main.rs` — ~30 lines.
- Tests — ~100 lines (TSV parser, formatter, deterministic output for fixtures).

Total: ~5 hours of work for the v1, including the `--watch` and `--json` modes.

## 5. Migration from v0.6.17

v0.6.17 ships stderr emission as the only surfacing. Switching to file-only is technically a behavior change for operators who rely on stderr — Monitor-armed agents would stop seeing notifications.

Two ways to handle:

- **Hard cutover at v0.7.0**: stderr emission deleted. Operators must adopt `giga doctor`. Documented in CHANGELOG with a one-liner.
- **Hybrid via flag** (recommended): `[watch].stale_wait_surface = "monitor" | "file" | "both"`, default `"file"` from v0.7.0. Existing operators flip to `"monitor"` or `"both"` if they want the old behavior. New operators get the zero-LLM default.

Hybrid keeps the door open without committing to a permanent dual code path — we can remove the `"monitor"` branch at v0.8 if everyone settles on file-only.

## 6. Why not just turn off v0.6.17's emission

Could be one line: `eprintln! → drop`. The reason it's a real design and not a one-liner: the surfacing PATH matters. If we just stop emitting, stale waits are detected but invisible to anyone. We need the operator-side command to make detection useful — and that command should be designed for the long-running, multi-agent, optionally-cross-host case from day one, not bolted on later.

## 7. Open questions

- **Q1.** New command name: `giga doctor` (own surface, expandable to other health checks) or `giga sweep --stale-waits` (folds into existing operator command)? Doctor is more discoverable; sweep keeps the command surface small. **Recommended:** `giga doctor` — sweep is per-channel, doctor is per-swarm-health; conceptually different.
- **Q2.** Should `giga doctor` also surface non-stale-wait health (daemon liveness, last-tick timestamps, channel byte counts) in v1, or keep it stale-wait-only and grow later? **Recommended:** v1 includes daemon liveness (cheap — already have last-tick output on stderr; can write a parallel file). Other checks deferred.
- **Q3.** The hybrid `[watch].stale_wait_surface` knob — required for migration, or can we go straight to file-only? **Recommended:** ship the knob; lets operators stay on v0.6.17 emission if a swarm depends on it.
- **Q4.** Watcher liveness file — should the watcher write a heartbeat to `~/.giga/watch-liveness/<slug>.ts` every poll tick so `giga doctor` can flag "agent N's watcher hasn't ticked in 5min"? That catches the case where the watcher process itself dies (which today is invisible until someone notices the agent isn't responding). **Recommended:** yes — same file pattern as stale-waits, near-zero cost, big win for "is this agent's pipe even alive".

## 8. Out of scope for v0.7.0

- Push notifications / Slack / desktop notifications. Operator can pipe `giga doctor --json --watch` into whatever they like; we don't build the integrations.
- Auto-recovery (any path that posts on behalf of an agent). The point of zero-LLM is to NOT inject agent turns; recovery stays manual or via `giga post --as <slug>` operator nudge.
- Cross-swarm doctor (aggregating across multiple `~/.giga/configs/*/`). Single-swarm v1; multi-swarm follow-up if asked.

---

**Token-cost summary (the original motivation):**

| Scenario | v0.6.17 | This design |
|---|---|---|
| Idle swarm, no stale waits | 0 turns | 0 turns |
| One agent has 3 unresolved stale waits, sits all day | 3 Monitor turns at first detection | 0 turns |
| Operator runs `giga doctor` 10× per day | n/a | 0 turns (subprocess only) |
| Operator decides to nudge an agent via `giga post` | 1 turn (the agent's reply) | 1 turn (same) |

The recovery cost is identical (~1 turn when the agent actually does the work). What we save is the always-on background cost of *telling* the agent something needs attention.
