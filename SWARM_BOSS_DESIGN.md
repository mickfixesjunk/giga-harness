# SWARM_BOSS_DESIGN.md

**Status:** draft, awaiting Mick GO before code
**Author:** giga
**Date:** 2026-06-01
**Context:** follow-on to REMOTE_DUAL_WRITE_DESIGN.md (v0.3.5 shipped). Mick's framing: "and just so I am clear — do we need one merger and one sync daemon on each host? … can that just be run as a 'monitor' on the coordinating agent maybe?"

---

## 1. Motivation

Today (post-v0.3.5), a multi-host swarm requires per-host tmux daemon panes:

| Per host | Process | Purpose |
|---|---|---|
| 1 | `giga sync` | Push this host's slices to peers |
| 1 | `giga merger` | Merge peer slices into local merged files |

`giga launch` spawns both panes alongside the agent panes. v0.3.4's F11 fix made this happen even on `--only` launches.

**The pain points:**
- Two extra tmux panes the operator has to either look at or ignore.
- Daemons crashing in a pane is hard to notice — the operator has to know to watch them.
- On a freshly-attached terminal (operator reconnects to a tmux server), nothing alerts you if the daemon panes exited.
- Adds noise to the multiplexer layout when most operators only care about agents.

**Mick's intuition:** since every agent already arms exactly one `Monitor` (the inbox watcher) at session start, the coordination daemons could live the same way. One designated agent — the "swarm boss" — arms two additional Monitors for `giga sync` and `giga merger`. No extra tmux panes; daemon errors surface as notifications in that agent's context where the operator-equivalent (the LLM) is paying attention.

---

## 2. Proposal in one sentence

Add `swarm_boss = true` to one agent per host in the TOML. That agent's generated CLAUDE.md auto-includes Monitor lines for `giga sync --quiet` and `giga merger --quiet`. `giga launch` skips the tmux daemon panes for any host where a swarm_boss agent exists.

---

## 3. Detailed design

### 3.1 Config schema addition

In `src/config.rs`, add to `Agent`:

```rust
/// True when this agent should host the per-host coordination daemons
/// (sync + merger) via its CLAUDE.md Monitor entries instead of as
/// separate tmux panes. At most one swarm_boss per host. Optional —
/// default is the v0.3.5 behavior where giga launch spawns the
/// daemon panes itself.
#[serde(default)]
pub swarm_boss: bool,
```

Backward compat: default `false` means today's behavior unchanged. Pure additive.

### 3.2 Validation (Config::validate)

- **At most one swarm_boss per host.** Mirror the existing `bench_scheduler` uniqueness check but scope it per host instead of per swarm.
- **swarm_boss agent must have a `host` matching this_host or no host (legacy local).** Validates the operator didn't accidentally put `swarm_boss` on a remote agent — that won't work because the Monitor lives in the agent's local Claude session.
- **swarm_boss agent's platform must be wsl.** sync/merger are POSIX-only today.

### 3.3 CLAUDE.md template generation (src/init.rs `render_agent_claudemd`)

When `agent.swarm_boss == true`, prepend a "Swarm coordination" section above the existing "Session Start" section:

```markdown
## Swarm coordination (this agent is the swarm_boss for {{HOST}})

In addition to your inbox watcher, arm two coordination daemons —
one Monitor for each. These keep cross-host channel comms flowing
for every agent on this host:

    Monitor(
      description: "giga sync — push slices to peers",
      persistent: true,
      command: "giga sync --quiet --config {{CONFIG_PATH}}"
    )

    Monitor(
      description: "giga merger — append peer slices into local merged files",
      persistent: true,
      command: "giga merger --quiet --config {{CONFIG_PATH}}"
    )

These are quiet-mode daemons — they emit lines only on errors or
state changes. Most notifications you receive from them will be
real signals worth surfacing or acting on.

If either Monitor stops firing for >5 minutes during active swarm
work, restart it (TaskStop then re-arm). Daemons dying mid-session
silently disrupts cross-host visibility for THIS host until restarted.
```

The `{{HOST}}` token is replaced with `this_host`; `{{CONFIG_PATH}}` is replaced with the absolute path of the canonical config (using `Config::source_path` from v0.3.4).

### 3.4 `giga launch` daemon-pane suppression (src/launch.rs)

Change `should_spawn_daemons(cfg, incremental)`:

```rust
fn should_spawn_daemons(cfg: &Config, _incremental: bool) -> bool {
    if cfg.hosts.is_empty() {
        return false;  // local-only, unchanged
    }
    // v0.3.6: if a swarm_boss agent exists on this_host, that agent's
    // CLAUDE.md will arm sync+merger Monitors at its session start.
    // Skip the tmux daemon panes to avoid duplicates.
    if let Some(this) = cfg.this_host.as_deref() {
        let has_boss = cfg.agents.iter().any(|a| {
            a.swarm_boss && cfg.agent_host(a).map(|h| h == this).unwrap_or(false)
        });
        if has_boss {
            return false;
        }
    }
    true
}
```

### 3.5 `--quiet` mode for sync and merger

Add `--quiet` flag to both subcommands' `Args` (`src/sync.rs`, `src/merger.rs`).

**sync `--quiet` suppression rules:**

| Today's output | Under `--quiet` |
|---|---|
| `sync: transport=…, this_host=…` (startup) | KEEP (one-shot signal to confirm boot) |
| `sync: tick complete — N attempted (X ok, Y failed)` (every 3s) | DROP |
| `sync: no cross-host slices…` (when plan empty) | DROP after first occurrence |
| `sync: <kind> push failed (e)` (rsync error) | KEEP |
| `[dry-run] …` | KEEP (--dry-run + --quiet still emits the plan) |

**merger `--quiet` suppression rules:**

| Today's output | Under `--quiet` |
|---|---|
| `merger: tracking N cross-host channels: …` (startup) | KEEP (one-shot signal) |
| `merger: config reload failed (e)` | KEEP |
| `merger: failed reading delta from …` | KEEP |
| `merger: failed appending to …` | KEEP |

Merger is already low-volume — most of its noise is errors today. `--quiet` mostly just gates startup chatter.

**Implementation note:** introduce a small `Quietable` log shim — `q_eprintln!(quiet, "…")` macro that skips when quiet. Avoids sprinkling `if !quiet` guards everywhere. Or accept the bare `if !quiet { eprintln!(…); }` pattern — fewer LOC overall, more explicit.

### 3.6 Edge cases

| Scenario | Behavior |
|---|---|
| swarm_boss agent's Claude session crashes | sync + merger Monitors die. Cross-host comms degrade on this host until session restart. The agent's CLAUDE.md mentions the >5-minute-silence test (§3.3). |
| swarm_boss agent restarted mid-session | Monitor re-arms on session start (existing pattern). sync re-reads config; merger picks up cursors from giga_home. Idempotent. |
| swarm_boss flag set on more than one agent per host | Validation error at Config::load. Operator picks one. |
| swarm_boss agent absent from a multi-host swarm | Falls back to today's behavior — tmux daemon panes via `giga launch`. Pure opt-in. |
| swarm_boss set but no `[[hosts]]` (local-only swarm) | Flag ignored (no daemons needed). Optional: warn at validate time, but it's harmless. |
| Operator runs `giga sync` manually in a terminal while swarm_boss Monitor is also running | Two daemons compete. Both are idempotent (rsync no-ops, merger cursor-tracked) so it's safe, just wasteful. Same as today's "two giga-sync panes" edge case. |

### 3.7 What this does NOT solve

- **Single point of failure shifts from a tmux pane to an agent session.** When the swarm_boss agent crashes, cross-host comms die for this host. This trade is explicit, not invisible. Mitigations:
  - swarm_boss should probably be a long-lived, low-churn agent — `design` or a dedicated `coordinator` slot, not an agent that's actively being restarted for code work.
  - Future: add a `giga doctor` (quality F15) that detects daemon silence and flags it.
- **Bootstrap order.** swarm_boss agent must come up before peers start posting OR the first few cross-host posts will land in slice + merged on the source host but not propagate to peers until sync wakes up. v0.3.5's dual-write keeps local visibility independent of this; only peer delivery is delayed. Acceptable for the first ~10s of swarm startup.

---

## 4. Decisions to confirm

1. **GO?** Or different shape (e.g., a separate `coordinator = true` agent role distinct from "agent that posts to channels"; or a daemon-only agent slot with no claude session)?
2. **Validation strictness — at most one vs exactly one per multi-host host?** "Exactly one" means a multi-host swarm without a swarm_boss is invalid; "at most one" means it falls back to today's tmux daemon panes. Recommend "at most one" (preserves the opt-in framing).
3. **`--quiet` ergonomics** — flag on the daemon binaries (this proposal) vs a TOML-level config (e.g., `[transport].quiet_daemons = true`)? Recommend flag on the binaries (simpler, no schema bloat).
4. **CLAUDE.md template injection** — auto-inject the Swarm coordination section (this proposal) vs require the agent's `claudemd_template` to opt in by referencing `{{SWARM_BOSS_MONITORS}}` placeholder? Recommend auto-inject (matches today's auto-watcher injection pattern; the operator gets it for free).

---

## 5. Test plan

| # | Test | Verifies |
|---|---|---|
| S1 | `validate_rejects_two_swarm_bosses_on_same_host` | Uniqueness validation. |
| S2 | `validate_allows_one_swarm_boss_per_host` | Multi-host swarm with one boss per host validates. |
| S3 | `render_agent_claudemd_injects_sync_merger_monitors_when_swarm_boss` | Template injection. |
| S4 | `render_agent_claudemd_omits_monitors_when_not_swarm_boss` | Default agents unaffected. |
| S5 | `should_spawn_daemons_returns_false_when_swarm_boss_present_on_this_host` | Launch suppression. |
| S6 | `should_spawn_daemons_returns_true_when_swarm_boss_only_on_peer_host` | Each host scoped independently. |
| S7 | `sync_quiet_mode_suppresses_per_tick_summary` | Behavior verification. |
| S8 | `sync_quiet_mode_still_emits_errors` | Critical signals preserved. |

---

## 6. Implementation surface

| File | Change | Estimated LOC |
|---|---|---|
| `src/config.rs` | `swarm_boss: bool` field + validation rule | ~15 |
| `src/init.rs` | `render_agent_claudemd` injects coordination section | ~25 |
| `src/launch.rs` | `should_spawn_daemons` consults swarm_boss presence | ~10 |
| `src/sync.rs` | `--quiet` flag + suppression guards | ~15 |
| `src/merger.rs` | `--quiet` flag + suppression guards | ~10 |
| `src/main.rs` (clap) | `--quiet` arg wiring on sync + merger subcommands | ~6 |
| Tests | S1-S8 | ~120 |
| Doc updates | `REMOTE_QUICKSTART.md` + `CLAUDE_OPERATOR.md` | ~25 |

Total: ~225 LOC. Estimate: ~2-3 hours including tests.

---

## 7. Migration / rollout

- Default: `swarm_boss = false` for every agent → today's behavior unchanged.
- Operator opts in by adding `swarm_boss = true` to one agent per host.
- `giga init` regenerates the agent's CLAUDE.md to include the coordination section.
- `giga launch` on the next launch skips the tmux daemon panes for that host.
- No data migration. No format changes. No version sniff between hosts (the swarm_boss agent on host A doesn't need to know whether host B uses the swarm_boss pattern; each host decides locally).
