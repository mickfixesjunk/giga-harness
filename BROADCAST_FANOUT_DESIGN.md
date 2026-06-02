# BROADCAST_FANOUT_DESIGN.md

**Status:** draft, awaiting Mick GO before code
**Author:** giga
**Date:** 2026-06-01
**Problem:** broadcasts on `_*.md` channels cause N agents to wake within seconds, each spawn an LLM turn near-simultaneously, and Anthropic per-account TPM rate limits blow up.

---

## 1. The threat

### What actually happens today

1. Operator (or an agent) posts to `_broadcast.md`.
2. Every agent's inbox watcher (`giga watch --as <name>`) polls every 3 seconds.
3. On the next tick, every watcher detects the new bytes and surfaces a `Monitor` notification into that agent's Claude session.
4. The notification triggers the agent's next turn. Each next-turn = one LLM API call carrying the full conversation context (50K-200K input tokens for active agents).
5. With ~17 agents on the superdeduper swarm, the worst-case window is ~3 seconds wall-clock and ~17 × 100K = **~1.7M input tokens / minute** of synchronous load.

### Why this hurts

Anthropic API rate limits are per-organization, applied as token-bucket windows:

| Tier | Approx Opus-class input TPM |
|---|---|
| Tier 1 | 40K |
| Tier 2 | 80K |
| Tier 3 | 160K |
| Tier 4 | 400K |
| Tier 5 | 2M+ |

For a 17-agent swarm at ~100K tokens/agent, the synchronous broadcast load *alone* exceeds Tier 4 by 4×. Even at Tier 5, you're chewing through most of the per-minute budget on a single broadcast, leaving no headroom for any other agent activity in that window.

The result Mick observed/feared: 429s / temporary rate-limits, agents hanging mid-turn, retries amplifying load, swarm-wide degradation that compounds.

### Important nuance: not every broadcast needs N readers

Looking at design's two recent cleanup-sprint posts on `_broadcast.md`:

- "MICK CLEANUP SPRINT: branches + GH issues + TODOs … every agent owns their own cleanup lane" — actionable for every agent
- "CLEANUP SPRINT NUDGE: engine + dev-health engaged; testdesign + sdd-testwin + web haven't acked" — actionable for 3 named agents; informational to others (including me)

The second case ALSO wakes every agent including the 14 who don't need to do anything. Pure overhead.

---

## 2. Design space

Five non-orthogonal ideas. The proposal combines the load-bearing ones.

### Idea A: Sender-side recipient targeting

A `--to <agent>[,<agent>]` flag on `giga post` for broadcast channels. Watchers only fire for named recipients. Body still lands in the broadcast file (for posterity), but the fanout is bounded.

```sh
giga post _broadcast --as design --subject "..." --to testdesign,web,sdd-testwin --body "..."
```

- **Bounds fanout to the explicitly addressed set.**
- Doesn't help for "this is for everyone" broadcasts (which still exist).
- Requires sender discipline; easy to forget.
- Wire format change: how is `--to` encoded in the on-disk message? Likely a header field after the subject.

### Idea B: Receiver-side staggered fanout

When a watcher detects a broadcast channel notification (channel name starts with `_`), it doesn't fire the Monitor notification immediately. Instead it delays by `slot * stagger_seconds` where `slot` is a stable hash of the agent's name into a `[0, N)` bucket.

- 17 agents × 15s stagger = 4-minute fanout window for the worst-case "everyone notified" broadcast.
- Smooths the TPM curve across 4 minutes instead of 3 seconds.
- Bounded total: the same N agents wake, just spread out.
- Naturally adaptive: more agents = wider window automatically.
- Per-agent delay is stable across watcher restarts (it's a hash, not a random).
- No coordinator, no shared state, no new failure modes.

### Idea C: Subject-prefix filtering

Convention: structured subject prefixes communicate intent + scope.

- `[fyi]` — informational. Watcher logs to a per-agent file, does NOT fire a Monitor notification. Zero LLM cost.
- `[ack: A,B,C]` — only agents A, B, C see the notification. Equivalent to Idea A but encoded in subject for backward compat with `giga post`.
- `[all]` or no prefix — everyone notified (with staggered fanout, per Idea B).

This is `--to` (Idea A) implemented in the SUBJECT instead of as a wire-protocol addition. Less invasive; pure convention; watchers do the filtering.

### Idea D: Per-account token-bucket gate at the watcher

Watcher consults a shared per-account file (`~/.giga/state/api_budget`) before firing a notification. The bucket has TPM-shaped semantics — agents that would exceed the budget hold off until the bucket refills.

- Globally bounded API load — the load-bearing guarantee.
- Adds shared state + lock contention; per-account assumes a single Anthropic account (true today on Mick's setup).
- Requires every watcher to estimate the input-token cost of waking each agent — hard without modeling the conversation length.
- Worth deferring: stagger (Idea B) already smooths the burst; per-account budget is the v2 layer.

### Idea E: Bench-coordination piggyback

Each agent that wants to RESPOND to a broadcast posts a `bench-request` first; the bench-scheduler issues `bench-clear` one at a time. This already exists for CPU/IO-heavy work; could extend to "respond to a broadcast" as a virtual bench class.

- Reuses existing mechanism.
- Doesn't gate the initial READ + reasoning that fires when the notification surfaces — only the post-action.
- The expensive thing is the LLM turn, not the post. So bench gating arrives too late.

### What gets combined

**B + C** is the proposal. They're complementary:
- **C** removes unnecessary wakes (informational broadcasts; addressed broadcasts).
- **B** spreads the unavoidable wakes (the "this is for everyone" case).
- Together: fanout is bounded by intent (C) AND smoothed in time (B).

**A** falls out of **C** for free — operators who prefer the explicit flag get `--to`, watchers translate it to `[ack:...]` in the subject.

**D** stays parked as v2: a token-bucket gate kicks in only when stagger + filtering aren't enough. If Mick observes 429s after this lands, we add it.

**E** stays out of scope.

---

## 3. Proposed design

### 3.1 Subject convention (Idea C)

Three prefixes that watchers interpret. All are case-insensitive. All co-exist with the existing `[<agent> YYYY-MM-DD HH:MM PST]` convention by appearing AFTER it.

```
[design 2026-06-01 12:00 PST] [fyi] cleanup sprint nudge — engine + dev-health engaged…
[design 2026-06-01 12:00 PST] [ack: testdesign, sdd-testwin, web] cleanup sprint nudge…
[design 2026-06-01 12:00 PST] [all] MICK CLEANUP SPRINT: every agent owns their own lane…
[design 2026-06-01 12:00 PST] cleanup sprint update…   ← no prefix = treated as [all]
```

| Prefix | Watcher behavior on a `_*.md` channel |
|---|---|
| `[fyi]` | log to `~/.giga/state/fyi-archive.<agent>.log`; DO NOT fire Monitor notification |
| `[ack: <list>]` | fire only when agent's slug is in `<list>` (CSV; tolerant of whitespace) |
| `[all]` or none | fire for everyone, with the staggered-fanout from §3.2 |

On non-broadcast channels (`<a>-<b>.md` and similar bilateral channels), the prefixes are ignored — those channels have explicit participants, no fanout problem.

### 3.2 Staggered fanout (Idea B)

When a watcher would fire a Monitor notification for a broadcast channel (and the message isn't `[fyi]` or filtered out by `[ack:...]`), it computes:

```rust
fn fanout_delay(this_agent: &str, all_recipients: &[&str], stagger_secs: u64) -> Duration {
    // Stable slot: agent's position in the sorted recipients list (alphabetical).
    // Same agent always gets the same slot → idempotent across watcher restarts.
    let mut sorted = all_recipients.to_vec();
    sorted.sort();
    let slot = sorted.iter().position(|a| a == &this_agent).unwrap_or(0) as u64;
    Duration::from_secs(slot * stagger_secs)
}
```

For an `[all]` broadcast, `all_recipients` = every agent participating in the channel. For an `[ack:...]` broadcast, `all_recipients` = the ack list. The delay is per-agent stable so a sender can predict the worst-case window: `N * stagger_secs`.

The watcher sleeps the delay, then surfaces the notification as today. If a SECOND broadcast arrives during the wait, the watcher queues it FIFO (no overlapping delays per agent — that'd starve them under sustained broadcast traffic).

### 3.3 TOML config

```toml
[broadcast]
# Per-slot delay in seconds for staggered fanout on _*.md channels.
# 0 = disabled (today's behavior; instant fan-out to everyone).
# Default: 15s — gives a 17-agent swarm a 4-minute fanout window.
stagger_seconds = 15

# Treat broadcasts without a [fyi]/[ack:.../all] prefix as [all].
# Set to "named-only" to enforce explicit addressing (no prefix = error).
default_recipients = "all"  # "all" | "named-only"
```

### 3.4 Sender-side sugar (optional, Idea A)

`giga post` learns `--to <CSV>` and `--fyi` flags that auto-inject the subject prefix:

```sh
giga post _broadcast --as design --to testdesign,web --subject "nudge" --body "..."
# → subject becomes: "[ack: testdesign, web] nudge"

giga post _broadcast --as design --fyi --subject "FYI: morpheus came online" --body "..."
# → subject becomes: "[fyi] FYI: morpheus came online"
```

Backward-compat: callers who type the prefix manually keep working; the flags are just ergonomic shortcuts.

---

## 4. Worked example: today's "cleanup sprint nudge"

The post that landed at 20:52 UTC: `CLEANUP SPRINT NUDGE: engine + dev-health engaged; testdesign + sdd-testwin + web haven't acked yet`.

**Under today's behavior:** all 17 agents (including 14 not addressed) wake within 3s. ~1.7M input tokens consumed in a single TPM window.

**Under the proposed design (sender-cooperating):**

```sh
giga post _broadcast --as design \
  --to testdesign,sdd-testwin,web \
  --subject "CLEANUP SPRINT NUDGE: 3 unacked agents" \
  --body "engine + dev-health engaged; testdesign + sdd-testwin + web haven't acked yet…"
```

- Subject becomes `[ack: testdesign, sdd-testwin, web] CLEANUP SPRINT NUDGE…`
- 14 agents see the message in their merged file but their watchers don't fire — zero LLM cost.
- 3 agents wake, staggered 0s / 15s / 30s.
- TPM impact: 3 × 100K = 300K tokens spread over 30s. Well within budget at any tier ≥ 2.

**Under the proposed design (sender forgot the `--to` flag):**

- Subject has no prefix → treated as `[all]`.
- All 17 wake, but staggered 0s / 15s / 30s / … / 240s.
- Per-15s-window: 1 agent = 100K tokens. Well within any tier.
- Worst-case wall-clock latency: 4 minutes from post to last-agent-notification. Acceptable for cleanup-sprint scale work.

---

## 5. Failure modes + edge cases

| Scenario | Behavior |
|---|---|
| Watcher restarts mid-stagger | New watcher reads its cursor from giga_home, recomputes the slot deterministically, finds the broadcast un-delivered, delays by the slot's offset. Lost delays don't compound. |
| Agent name in `[ack:...]` doesn't match any participant | Watcher silently doesn't fire for that name (it's no-op; treat as a no-show). The other named recipients still receive normally. |
| Sender uses `[all]` AND `[ack:...]` in the same subject | Watcher parses left-to-right; first prefix wins. Document this in the convention; surface a one-time warning at post time. |
| Multibyte chars in subject (existing watcher limitation) | Same as today — fail-fast at post time before write. No change. |
| FYI volume exceeds operator capacity to skim the archive | Out of scope — operator periodically inspects `~/.giga/state/fyi-archive.<agent>.log` or `tail -f` if curious. |
| Cross-host broadcast (the message originates on a peer host) | Slice + merger architecture means each host's watchers see the merged file independently. Stagger computed per-host, against per-host agent count. Peer-host agents wake on their own schedule. Net global fanout is bounded by per-host stagger × per-host agent count, summed across hosts. Worst case: 17 agents on one host = 240s; 8 + 9 agents on two hosts = 120s + 135s ≈ overlapping 135s window. Both better than synchronous. |
| Bench scheduler also responds to broadcasts | No special-casing — they're an agent like any other and get a slot. |

---

## 6. Implementation surface

| File | Change | LOC |
|---|---|---|
| `src/config.rs` | New `[broadcast]` section: `BroadcastConfig { stagger_seconds: u64, default_recipients: String }`. Backward compat: missing section = defaults (stagger 15, all). | ~30 |
| `src/watch.rs` | When firing a notification on a `_*.md` channel: parse subject for `[fyi]` / `[ack: ...]` / `[all]` prefix → filter or delay. New `fanout_delay()` pure function. | ~70 |
| `src/post.rs` | New `--to <CSV>` and `--fyi` flags that synthesize the subject prefix. Backward compat: callers using manual prefixes keep working. | ~25 |
| `src/main.rs` | clap wiring for the two new post flags. | ~10 |
| `templates/CONVENTION.md` (the auto-generated CLAUDE.md convention section) | Document the three prefixes + when to use each. | ~15 |
| Tests | unit: `fanout_delay` slot stability; subject-parse for each prefix; `[ack:...]` filtering for matching + non-matching agents; `[fyi]` skips notification; stagger durations for synthetic 17-agent fixture. integration: a synthetic broadcast test that asserts notification arrival times stagger correctly. | ~150 |
| Docs | README.md broadcast section; CLAUDE_OPERATOR.md command reference; new BROADCAST_FANOUT_DESIGN.md committed to repo root. | ~40 |

Total: ~340 LOC. Estimate: ~3 hours including tests + docs.

---

## 7. Migration / rollout

- `[broadcast]` section default values match v0.3.8 behavior in spirit:
  - `stagger_seconds = 15` is the new default — every operator gets fanout smoothing for free on upgrade.
  - `default_recipients = "all"` preserves existing semantics — broadcasts without a prefix continue to reach everyone.
- Senders don't need to change anything for the smoothing benefit (Idea B) — pure receiver-side change.
- Senders OPT IN to recipient filtering (Idea C) by adding subject prefixes or using the new `--to` / `--fyi` flags.
- Document in the convention: explicit prefix is preferred for broadcasts that don't need everyone — it's the courtesy default for the agent population that will otherwise eat their slot delay for an FYI they can't act on.

To set `stagger_seconds = 0` for testing or single-account high-tier setups: explicit opt-out, no surprises.

---

## 8. Decisions to confirm

1. **GO?** Or different shape — e.g., per-account token-bucket gate (Idea D) as the primary mechanism instead of stagger?
2. **Default stagger value.** 15s is conservative (4 min fanout for 17 agents). Faster default (5s = 85s fanout)? Slower (30s = 8.5 min)? I recommend 15s — it's noticeable on Mick's reading cadence but well below the TPM-budget threshold even at Tier 2.
3. **`default_recipients = "named-only"` enforcement.** If you want to force every broadcast to be explicitly addressed, set this to `"named-only"` and `giga post` errors on un-prefixed broadcasts. Useful if you observe people forgetting the `--to` flag. Recommend leaving at `"all"` for v1; tighten if needed.
4. **Sender-side sugar (`--to`, `--fyi`).** Ship both with v1, or ship the watcher-side first (Idea B alone) and add the post flags as a v2?
5. **`[fyi]` archive location.** `~/.giga/state/fyi-archive.<agent>.log` keeps it under giga_home (consistent with cursor state). Alternative: per-channel file, per-swarm dir. Recommend the simple flat file.

---

## 9. What this design does NOT do

- **Doesn't limit non-broadcast fanout.** A bilateral `a-b.md` channel between two agents always wakes both — that's the contract. The cap is on `_*.md` channels only.
- **Doesn't model actual API token cost.** Stagger is wall-clock based, not token-bucket based. If a swarm has ONE agent with a 1M-token context, that single agent can still blow the TPM window on its own. The fix for that is per-account budget gating (Idea D), parked for v2.
- **Doesn't prevent the SAME agent from receiving consecutive broadcasts that each trigger a full reasoning cycle.** If design posts 5 broadcasts in 5 minutes, that agent does 5 reasoning passes. The cap is one-broadcast-per-N-agents, not N-broadcasts-per-agent. Mitigation: post fewer broadcasts (operator discipline); or rate-limit POST itself on `_*.md` channels (out of scope here).
- **Doesn't help with non-Anthropic API rate limits.** Token budget gating would be needed for that; out of scope.

---

## 10. Estimate

- Design iteration on this doc: short (Mick driving direct).
- Implementation: ~3 hours including tests + doc updates.
- Live-test on the superdeduper swarm: real, since it's a behavior change Mick will notice immediately (broadcasts arrive slower for some agents).
