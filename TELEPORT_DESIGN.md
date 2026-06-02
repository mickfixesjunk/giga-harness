# TELEPORT_DESIGN.md

**Status:** draft → implementing on `feature/teleport` branch (Mick GO 2026-06-02)
**Author:** giga
**Date:** 2026-06-02
**Problem:** moving an agent from host A to host B in the swarm is a multi-step manual procedure (edit TOML, rsync workdir, kill old pane, scaffold new, launch). Operators need one command.

---

## 1. What a teleport actually moves

| Concern | What teleport does | Notes |
|---|---|---|
| TOML `agent.host` field | Update from A to B in canonical config | Sync pushes to all hosts via existing cfg-reload mechanism (v0.4.2) |
| Agent's workdir on A | rsync to B (preserves HANDOVER.md, agent-private files) | See §3 for direction |
| Running tmux session on A | Kill: SIGTERM to claude → wait 5s → kill window | See §4 for grace handling |
| Tmux session on B | `giga init --only <agent>` then `giga launch --host B --only <agent>` | Reuses existing primitives |
| Channel slice files | **No action.** Slices are per-host append logs. Past posts stay in `<channel>.A.md` (visible swarm-wide via merge). Future posts go to `<channel>.B.md`. | Append-only invariant; no migration needed |
| Claude conversation history (`~/.claude/`) | **Does NOT transfer.** Per-machine. | Agent restarts fresh on B; reads HANDOVER.md for context |
| Giga cursors (`~/.giga/cursors/`) | Per-machine; reset on B | First watch tick re-replays history from byte 0 (existing auto-replay convention); agent gets a backlog dump, then proceeds |
| **HANDOVER.md** | Workdir rsync brings it across; teleport then prepends a "you have been moved" banner | See §2 |

---

## 2. HANDOVER banner — the load-bearing safety net

After the workdir rsync completes and BEFORE the agent starts on B, teleport prepends this block to `HANDOVER.md`:

```markdown
> **You have been teleported to `<target-host>`. You used to be on `<source-host>`.**
>
> Teleport timestamp: <UTC ISO-8601>. If anything looks off (missing context, broken paths, stale cursor state, vanished tooling), this move is the most likely explanation. The rest of this HANDOVER.md is what existed in your previous workdir at teleport time.

[... existing HANDOVER.md content below ...]
```

**Rules:**
- Banner is a blockquote (`>`) — stands out visually, renders cleanly in markdown.
- If `HANDOVER.md` doesn't exist on B (no prior session), teleport creates it with JUST the banner.
- If the agent has been teleported multiple times, new banner goes ABOVE old banners — preserves move history as a trail. No pruning in v1; if it gets noisy in practice we can cap to last N.
- Banner is prepended via SSH on the target host after rsync (not on the operator host) — single write, no second sync.

The agent's CLAUDE.md Session Start protocol already says "read HANDOVER.md first if present", so this lands in the agent's context as the first content of its new session. Self-explanatory: agent sees it, knows what happened, proceeds with awareness.

---

## 3. Workdir rsync direction

Operator runs `giga teleport` from machine X (typically the operator host, but could be anywhere). Source = A, target = B.

**Chosen approach (Mick GO): direct A→B over tailnet SSH.**

```
operator (X) ──ssh──> source (A) ──rsync──> target (B)
```

Operator SSHes to A, runs `rsync workdir <user>@<B-tailnet>:workdir`. Requires A to have SSH trust to B — which Tailscale SSH gives us automatically (both are on the tailnet; tailnet identity auth).

**Pros:**
- Single hop on the wire (no operator-host bouncing).
- Matches the tailnet-trust model giga already uses.
- Works regardless of where the operator is.

**Cons:**
- Requires A to be reachable from operator AND for B to be reachable from A. If A↔B has no tailnet path (rare), falls back to (a) two-hop via operator.

**Fallback (if direct A→B fails):** two-hop via operator. `rsync A:workdir → X:tmp` then `rsync X:tmp → B:workdir`. Slower, doubles wire traffic, but always works if the operator can reach both endpoints.

```
operator (X) <──rsync── source (A)
operator (X) ──rsync──> target (B)
```

---

## 4. Killing the old tmux session

Pre-teleport, the agent has a tmux pane on A running `claude` (or whatever launch command). After the new pane is up on B and the agent is alive there, we need to tear down the old pane gracefully.

**Sequence:**

1. SSH to A.
2. Find the tmux session by name (`giga-<swarm-name>`) and the window by title (the agent slug — set by `giga launch`).
3. Send SIGTERM to the foreground process in that pane: `tmux send-keys -t <session>:<window> "C-c"` or `kill -TERM <pane-pid>`. Gives claude a chance to flush state.
4. Sleep 5 seconds.
5. Kill the window: `tmux kill-window -t <session>:<window>`.

**With `--keep-running` flag**: skip the kill entirely. Operator wants to verify the B-side agent is healthy before tearing down A. They can manually `tmux kill-window` later.

**Edge case:** tmux session doesn't exist (operator never ran `giga launch` on A, or already killed it). No-op; print "no source pane found on A".

---

## 5. CLI surface

```sh
giga teleport <agent> --to <host> [--from <host>] [--keep-running] [--dry-run]
```

- `<agent>`: the agent slug (required, positional).
- `--to <host>`: destination host name (must exist in `[[hosts]]`).
- `--from <host>`: source host name (optional — defaults to current `agent.host` field from TOML).
- `--keep-running`: don't kill the source pane; operator handles teardown manually.
- `--dry-run`: print every step that would be taken; no side effects.

**Preflight validation (before any side effects):**
- Agent exists in `[[agents]]`.
- Source host exists in `[[hosts]]`; matches current `agent.host` (or `--from` if supplied).
- Target host exists in `[[hosts]]`.
- Source ≠ target.
- Operator's swarm transport supports remote exec (otherwise we can't SSH).

**Execution sequence (happy path):**

```
1. Preflight (errors abort, no side effects).
2. SSH A: ensure HANDOVER.md exists (touch if missing).
3. SSH A: rsync workdir to B (per §3).
4. SSH B: prepend teleport banner to HANDOVER.md (per §2).
5. Update TOML: set agent.host = <target>. Atomic write to canonical config.
6. Sync TOML to all peers (via `giga sync --once`).
7. SSH B: run `giga init --only <agent>` (scaffolds anything missing).
8. SSH B: run `giga launch --only <agent>` (new pane comes up).
9. (Unless --keep-running) SSH A: SIGTERM the old pane, sleep 5s, kill window.
10. Print summary: "agent <name> teleported from A to B; verify with giga hosts."
```

**Rollback semantics:** if step 5 fails (TOML write error), no remote state has been committed yet (steps 2-4 are idempotent — rsync overwrites are append-only; banner prepend is just file content). Step 6+ failures are best-effort — log warnings, continue. Step 9 with `--keep-running` is the safety valve.

---

## 6. Test plan

| # | Test | Covers |
|---|---|---|
| T1 | `prepend_teleport_banner_to_empty_handover_creates_file` | Banner-creation when no prior HANDOVER |
| T2 | `prepend_teleport_banner_preserves_existing_content` | Banner is prefix; existing content intact below |
| T3 | `prepend_teleport_banner_stacks_multiple_moves` | Re-teleport stacks new banner above old |
| T4 | `preflight_rejects_unknown_agent` | Validation |
| T5 | `preflight_rejects_same_source_and_target` | Validation |
| T6 | `preflight_rejects_unknown_target_host` | Validation |
| T7 | `preflight_auto_detects_source_from_toml_host_field` | --from default |
| T8 | `dry_run_prints_plan_without_side_effects` | --dry-run |
| T9 | `update_toml_changes_agent_host_field` | TOML edit primitive |

Live-test (manual, not in CI) on the morpheus-wsl ↔ trinity-wsl pair: teleport `research` morpheus → trinity, verify agent comes up on trinity with the banner visible at the top of HANDOVER.md.

---

## 7. Implementation surface

| File | Change | LOC est |
|---|---|---|
| `src/teleport.rs` | New module: Args, preflight, rsync helper, banner prepend, TOML update, kill, launch | ~250 |
| `src/main.rs` | Clap subcommand wiring | ~25 |
| Tests (in `src/teleport.rs`) | T1–T9 from §6 | ~150 |
| `README.md`, `CLAUDE_OPERATOR.md` | Document the command | ~30 |
| `TELEPORT_DESIGN.md` | This doc | ~250 |

Total: ~700 LOC. Estimate: ~3-4 hours including tests + docs.

---

## 8. Out of scope (deliberate)

- **Conversation-history transfer.** `~/.claude/` per-machine; HANDOVER.md is the canonical migration vehicle. If an operator needs full transcript continuity across hosts, that's a separate "session-portable claude credentials/session" project.
- **Per-channel slice file consolidation.** Past A-slice contents stay in A's slice forever. No merging-into-B. Slice files ARE history; preserving them where they were written matches the append-only contract.
- **Cross-tailnet teleport.** v1 assumes A and B are on the same tailnet (existing remote-channels assumption).
- **Concurrent teleport coordination.** If two operators run `giga teleport <same-agent>` simultaneously, the TOML-edit step is atomic but the rsync/launch steps could race. Document "one operator at a time"; coordination via the channel system if needed.

---

## 9. Estimate

- Branch + design doc: in progress.
- Implementation: ~2-3 hours.
- Tests: ~30 min.
- Docs: ~15 min.
- Live verify on morpheus↔trinity: depends on Mick's bandwidth to spin up an agent for the test.

Net: half-day of focused work.
