# giga-harness examples

Runnable, copy-pasteable `giga-harness.toml` files that show how to wire up a
swarm. Start here, then read [`../docs/MANUAL_SETUP.md`](../docs/MANUAL_SETUP.md)
(the full field-by-field schema reference) and [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
(how the moving parts fit together) for the complete picture.

| Example | Agents | Channels | Hosts | What it shows |
| --- | --- | --- | --- | --- |
| [`minimal/`](./minimal/giga-harness.toml) | 2 | 1 | 1 | The smallest useful setup: two agents talking over a single shared inbox file, plus a one-line bench-protocol stub. |

---

## `minimal/` — two agents, one channel

[`minimal/giga-harness.toml`](./minimal/giga-harness.toml) is the smallest
config that still does something useful. Two agents — a **researcher** (`alpha`)
and an **engineer** (`beta`) — coordinate by appending Markdown messages to a
single shared inbox file. There is no database, no message bus, and no LLM in
the coordination loop: just a plain-text file that both agents watch and write.

### What it demonstrates

- A complete **local-only swarm** using only the three hard-required tables:
  `[project]`, `[[agents]]`, and `[[channels]]`. Everything else in the file is
  optional sugar.
- A **bilateral channel** (`alpha-beta.md`) shared by exactly two participants.
- The **bench-scheduler** convention: one agent (`beta`) is marked as the
  scheduler and a `[bench_protocol]` table names it, so heavy/exclusive work can
  be gated behind `bench-request` / `bench-clear` messages.

### Field-by-field walkthrough

```toml
[project]
name = "minimal-example"
description = "Two agents, one channel — the smallest useful setup."
```

**`[project]`** — swarm-wide metadata.
- `name` (**required**) — the project slug. Referenced internally and used when
  resolving default paths.
- `description` (optional) — free text; documentation only.

```toml
[paths]
wsl_inbox = "/tmp/giga-example-inbox"
```

**`[paths]`** — where channel files live on disk. This whole table is **optional**
(since v0.6.24); when omitted, `wsl_inbox` defaults to `<config_dir>/inbox`. The
example sets it explicitly to `/tmp/giga-example-inbox` so the example is
self-contained and easy to clean up.
- `wsl_inbox` — directory holding every channel whose `side = "wsl"`. Each
  channel is a single text file inside this directory (here, just
  `alpha-beta.md`).

```toml
[[agents]]
name = "alpha"
workdir = "/tmp/giga-example/alpha"
role = "Researcher — answers questions from beta."
platform = "wsl"

[[agents]]
name = "beta"
workdir = "/tmp/giga-example/beta"
role = "Engineer — asks alpha for context, implements changes."
platform = "wsl"
bench_scheduler = true
```

**`[[agents]]`** — one block per agent (each runs in its own terminal tab and
its own working directory).
- `name` (**required**) — the agent slug. This is the identity referenced by
  channel `participants` and by `bench_protocol.scheduler`. Here: `alpha` and
  `beta`.
- `workdir` (**required**) — the isolated directory the agent runs in. `init`
  renders an `AGENTS.md` / `CLAUDE.md` here so the agent knows who it is, which
  channels it watches, and the coordination convention.
- `role` (**required**) — a one-line description of the agent's job, baked into
  its generated template. `alpha` researches and answers; `beta` asks for
  context and implements.
- `platform` (optional, defaults to `"wsl"`) — `wsl` or `windows`; controls how
  the terminal tab is spawned and where per-folder trust state is written. Both
  agents here are `wsl`.
- `bench_scheduler` (optional, defaults to `false`) — marks the bench scheduler.
  **At most one per swarm.** `beta` carries this flag, which pairs with the
  `[bench_protocol]` table below.

```toml
[[channels]]
file = "alpha-beta.md"
side = "wsl"
participants = ["alpha", "beta"]
purpose = "Research questions + answers, code review comments."
```

**`[[channels]]`** — one block per shared inbox file.
- `file` (**required**) — the filename only; the directory comes from
  `paths.<side>_inbox`. So `alpha-beta.md` resolves to
  `/tmp/giga-example-inbox/alpha-beta.md`. The bilateral naming convention is
  sorted `<a>-<b>.md`; a leading underscore (`_*.md`) would instead mark a
  broadcast/fanout channel.
- `side` (**required**) — `wsl` or `windows`; selects which inbox directory the
  `file` lives in. `wsl` here, so the matching `[paths].wsl_inbox` is the one
  that must be set.
- `participants` (**required**) — the agent slugs that read and write this
  channel. Both `alpha` and `beta` watch this file; each posts replies the other
  picks up. Every slug listed must match an `[[agents]].name`.
- `purpose` (optional) — free text describing what the channel is for;
  documentation only.

```toml
[bench_protocol]
scheduler = "beta"
slot_pool = "this-host"
```

**`[bench_protocol]`** — opt-in table that wires up the bench-scheduler
convention. Bench coordination is a protocol layered on top of channels: agents
post `bench-request <slot>` and wait for `bench-clear <slot>` from the designated
scheduler before doing heavy or exclusive work (for example, a long benchmark
that needs sole access to a shared resource).
- `scheduler` (**required when this table is present**) — the agent that hands
  out bench slots. Must be `"beta"` here to match the agent carrying
  `bench_scheduler = true`.
- `slot_pool` (optional, defaults to `"this-host"`) — `this-host` (one shared
  pool of slots across the host) or `per-host`.

> The `[bench_protocol]` table is purely illustrative in this two-agent example —
> with one researcher and one engineer there is little to schedule. It is here so
> you can see how the scheduler agent (`bench_scheduler = true`) and the protocol
> table reference each other.

---

## How to run it

All three commands default to a config named `giga-harness.toml`, so run them
from inside `examples/minimal/`, or pass the path explicitly from elsewhere.

### 1. Validate the config (no side effects)

```bash
giga validate examples/minimal/giga-harness.toml
```

`validate` parses the TOML, checks the schema, and cross-references it — every
channel `participant` must name a real agent, the `bench_protocol.scheduler`
must match the agent flagged `bench_scheduler = true`, and so on. It touches
nothing on disk. It will also flag orphan channel files in the inbox dir that
aren't enrolled in `[[channels]]`.

### 2. Scaffold inboxes + templates

```bash
giga init examples/minimal/giga-harness.toml
```

`init` creates the inbox directory and the `alpha-beta.md` channel file, and
renders a per-agent `AGENTS.md` / `CLAUDE.md` into each `workdir`. By default it
also pre-seeds each agent `workdir` as trusted so the agent CLI doesn't prompt on
first launch; pass `--no-trust` to skip that.

### 3. Launch the swarm

```bash
giga launch examples/minimal/giga-harness.toml
```

`launch` spawns one terminal tab per agent (Windows Terminal or tmux,
auto-detected) and starts each agent's CLI in its `workdir`. It runs `init`
first unless you pass `--skip-init`. Useful flags: `--dry-run` to print the
launch plan without spawning anything, `--only alpha` to spawn a subset, and
`--terminal tmux|wt|mac-terminal|print` to force a launcher.

Once both tabs are up, `alpha` and `beta` watch `alpha-beta.md` and coordinate by
appending messages to it.

> Heads up: this example writes to `/tmp` (`/tmp/giga-example-inbox` and
> `/tmp/giga-example/{alpha,beta}`). That keeps it disposable, but `/tmp` is
> cleared on reboot. Copy the file and change `[paths].wsl_inbox` + each
> `workdir` to a persistent location before using it for real work.

---

## See also

- [`../docs/MANUAL_SETUP.md`](../docs/MANUAL_SETUP.md) — the full config schema
  reference: every table, every field, defaults, and required keys.
- [`../ARCHITECTURE.md`](../ARCHITECTURE.md) — how channels, watchers, the
  merger, transports, and the coordination convention fit together.
