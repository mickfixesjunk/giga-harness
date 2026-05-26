---
name: giga-harness
description: Use giga (the multi-agent coordination harness in this repo) to spawn, watch, and coordinate parallel Claude Code agents that talk via file-based inboxes. Trigger when the user mentions giga, asks how to add/launch/manage agents in a giga ecosystem, asks about a giga-harness-configs project, references file-based agent coordination (channels, watchers, inbox files), or asks how to use this repo to run a multi-agent workflow. Covers the subcommand surface, the conventions (channel headers, WAITING ON tags, bench-scheduler protocol), and the per-host setup flow.
---

# giga-harness

`giga` is a CLI for running N parallel Claude Code agents (macOS, Linux, Windows + WSL) that coordinate via append-only Markdown files. One terminal per agent; each opens in the agent's workdir with `claude -c` already running. Agents post to shared inbox files and arm a single `Monitor` running `giga watch --as <slug>`, which tails every channel that agent participates in (auto-discovered from the config) and rereads the config periodically so newly-added channels appear without a restart.

> **Fastest bootstrap:** `giga setup` from any project directory launches Claude Code with a baked-in prompt that scaffolds the swarm end-to-end (config in `~/.giga/configs/<name>/`, registry entry in `~/.giga/swarms.toml`). The cheat sheet below covers the manual surface.

## Subcommand cheat sheet

```
giga setup                          # interactive bootstrap (recommended for new swarms)
giga validate [config]              # parse + cross-check, no side effects
giga init     [config]              # create inbox files + per-agent CLAUDE.md; register swarm
giga launch   [config]              # spawn one terminal per agent
                                    #   --terminal mac-terminal | tmux | wt | auto | print
giga sweep    <config>              # tabulate open WAITING ON tags
giga post     <channel> --as <agent> --subject ... [--body ... | stdin] [--waiting-on <agent>]
giga watch    --as <agent> [<channel>]  # long-running watcher; --as filters own msgs
                                        # no <channel> → auto-track every channel agent participates in (preferred)
                                        # with <channel> → legacy single-file mode (back-compat)
```

All commands default to `--config giga-harness.toml` in the current directory. The per-agent workdir gets a `giga-harness.toml` symlink (WSL) or copy (Windows) so bare channel names resolve.

## Project shape (giga-harness-configs convention)

```
<project>/
  giga-harness.toml          # canonical config — DO edit this
  agents/<slug>.md           # canonical per-agent CLAUDE.md templates — DO edit
  setup-<host>.sh            # per-host bring-up (clones repos, installs giga, localizes)
  giga-harness.<host>.toml   # generated, gitignored — DO NOT edit
  agents.<host>/             # generated, gitignored — DO NOT edit
```

The canonical config and templates use the original author's machine-conventional paths (e.g. `/home/neo/...`, `C:\Users\Audio\...`). The per-host `setup-*.sh` substitutes these for the local user when generating the localized variants.

## Channel-file convention

Append-only Markdown files. Each message:

```
===
[<sender>] <subject> — <UTC ISO-8601 timestamp>
===

<body>

WAITING ON: <agent> (<what's needed>)   ← OR
(Informational, no response required.)
===
```

The `WAITING ON:` / `Informational` tag is load-bearing. Agents that close ambiguously stall the pipeline ("both sides think the ball is with the other" — happens within hours of dropping the convention).

## Broadcast channel pattern (optional convention)

Projects with more than ~3 agents often want an ecosystem-wide announcement bus rather than fan-out across N bilaterals. Convention:

- One channel file with the underscore prefix (e.g. `_broadcast.md`).
- `participants` lists every agent in the project.
- `side = "windows"` if the project has Windows-platform agents (so both sides can reach it via `/mnt/c/...`); otherwise `wsl`.
- Default closing tag: `(Informational, no response required.)`. If a broadcast needs per-agent confirmation, the requester pings each agent on their bilateral — broadcasts don't `WAITING ON` multiple agents at once.

For what to broadcast: migrations, standing-directive changes, agent lifecycle (additions, stand-downs), planned downtime. Not for bilateral coordination, not for bench-request/clear.

This is a config-only convention — no special giga support. The auto-discovery watcher (`giga watch --as <slug>`) picks it up because every agent is listed as a participant. New agents added via the `giga-add-agent` skill should be appended to the broadcast channel's participants list as part of scaffolding.

Order-of-operations caveat: broadcasts only reach agents already on the auto-discovery watcher. If you're migrating an ecosystem from per-channel watchers to single-Monitor for the first time, that one-time migration still needs bilateral fan-out — broadcasts work for everything after.

## Bench-scheduler protocol

One agent (set `bench_scheduler = true` on it) is the gatekeeper for CPU/IO-heavy work. Other agents `bench-request <slot>` on their bilateral channel with the scheduler, wait for `bench-clear <slot>`, do the work, then `bench-done <slot>`. Standing clearance for sub-60s housekeeping operations.

## Per-host setup flow

1. **Once per host:** `<project>/setup-<host>.sh` — clones source repos, installs giga (Linux + Windows), localizes the config + templates, drops workdir configs, sets bypassPermissions on both sides.
2. **Every session:** `giga launch <project>/giga-harness.<host>.toml` — re-renders CLAUDE.md files, opens N terminal tabs, drops each into `claude -c` so prior session state resumes.

## Common operations (which skill to load)

- **Add a new agent** → load `giga-add-agent` skill. It scaffolds the `[[agents]]` entry, the canonical template, the bilateral `[[channels]]`, and tells the user how to apply.
- **Spawn only newly-added agents without disturbing running tabs** → `giga launch --only <slug>[,<slug>] <config>`. New tabs join the existing wt window (named `giga-<project>`) or tmux session; existing agents keep running. `init` still runs to create any new inbox files; rendered CLAUDE.md files are refreshed but in-flight Claude sessions don't re-read them. **Add `--new-window`** if the user has torn the original launch window apart (e.g. dragged each agent's tab into its own window arranged on screen) — that forces wt to open a fresh window via `-w new` instead of guessing where the project's named window went. With config-aware watchers (the default since 0.1.9), existing agents auto-discover the new agent's bilateral channel — no manual Monitor arming needed.
- **Migrate an existing agent from N-Monitor to single-Monitor design** → only matters if their CLAUDE.md still has the legacy per-channel block. Have them kill all current channel watchers (drop the persistent Monitors), then arm one new `Monitor(persistent: true, command: "giga watch --as <slug>")`. The new watcher discovers all channels via the config and emits `inbox <channel>: ...` lines so they can still tell which channel fired. Existing in-flight messages aren't replayed — the new watcher starts at EOF.
- **Diagnose a stuck channel** → `giga sweep <config>`. Surfaces the last message + open WAITING ON tag per channel. If both sides think they're waiting, that's the bug to fix.
- **Pull an agent's runtime CLAUDE.md edits back to canonical** → diff `<workdir>/CLAUDE.md` against `agents.<host>/<slug>.md`, apply meaningful changes to `agents/<slug>.md`, reverse-substitute machine-specific paths back to the canonical author's placeholders. Verify by re-running the localizer and checking the round-trip diffs to zero.
- **Stand an agent down** → leave the `[[agents]]` entry, update `role` to `"Stood down. Watcher armed but channel inactive — only triggers if reactivated."`, rewrite the template to a minimal "you are stood down, arm watcher, standby" form. (Don't remove — keeping the watcher armed lets the agent be reactivated by a single channel message.)

## Don't

- **Don't edit localized files** (`giga-harness.*.toml`, `agents.*/`). They're regenerated on `setup-*.sh`. Edits are silently clobbered.
- **Don't hardcode the current user's paths** in canonical files. Use the canonical author's placeholders so localizers can substitute.
- **Don't skip `giga validate` after editing the TOML.** A typo in a channel participant or a missing inbox dir surfaces immediately; debugging it after a failed `giga launch` is much harder.
- **Don't run a full `giga launch` without first killing any prior wt.exe / tmux session** for the project. Multiple windows compete for the same agent tabs and you end up with stale per-agent sessions. Exception: `giga launch --only <slug>` is designed to add to an existing session and is safe to run against a live ecosystem.
