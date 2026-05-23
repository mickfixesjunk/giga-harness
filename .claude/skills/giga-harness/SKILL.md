---
name: giga-harness
description: Use giga (the multi-agent coordination harness in this repo) to spawn, watch, and coordinate parallel Claude Code agents that talk via file-based inboxes. Trigger when the user mentions giga, asks how to add/launch/manage agents in a giga ecosystem, asks about a giga-harness-configs project, references file-based agent coordination (channels, watchers, inbox files), or asks how to use this repo to run a multi-agent workflow. Covers the subcommand surface, the conventions (channel headers, WAITING ON tags, bench-scheduler protocol), and the per-host setup flow.
---

# giga-harness

`giga` is a CLI for running N parallel Claude Code agents (Windows + WSL mix) that coordinate via append-only Markdown files. One terminal tab per agent; each tab opens in the agent's workdir with `claude -c` already running. Agents post to shared inbox files and arm `Monitor` tools that tail those files for new messages.

## Subcommand cheat sheet

```
giga validate <config>              # parse + cross-check, no side effects
giga init     <config>              # create inbox files + per-agent CLAUDE.md
giga launch   <config>              # spawn one terminal per agent
giga sweep    <config>              # tabulate open WAITING ON tags
giga post     <channel> --as <agent> --subject ... [--body ... | stdin] [--waiting-on <agent>]
giga watch    <channel> --as <agent>    # long-running watcher; --as filters own msgs
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

The canonical config and templates use Mick's machine-conventional paths (`/home/neo/...`, `C:\Users\Audio\...`). The per-host `setup-*.sh` substitutes these for the local user (e.g. `/home/neomatrix/`, `C:\Users\NeoMatrix\`) when generating the localized variants.

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

## Bench-scheduler protocol

One agent (set `bench_scheduler = true` on it) is the gatekeeper for CPU/IO-heavy work. Other agents `bench-request <slot>` on their bilateral channel with the scheduler, wait for `bench-clear <slot>`, do the work, then `bench-done <slot>`. Standing clearance for sub-60s housekeeping operations.

## Per-host setup flow

1. **Once per host:** `<project>/setup-<host>.sh` — clones source repos, installs giga (Linux + Windows), localizes the config + templates, drops workdir configs, sets bypassPermissions on both sides.
2. **Every session:** `giga launch <project>/giga-harness.<host>.toml` — re-renders CLAUDE.md files, opens N terminal tabs, drops each into `claude -c` so prior session state resumes.

## Common operations (which skill to load)

- **Add a new agent** → load `giga-add-agent` skill. It scaffolds the `[[agents]]` entry, the canonical template, the bilateral `[[channels]]`, and tells the user how to apply.
- **Diagnose a stuck channel** → `giga sweep <config>`. Surfaces the last message + open WAITING ON tag per channel. If both sides think they're waiting, that's the bug to fix.
- **Pull an agent's runtime CLAUDE.md edits back to canonical** → diff `<workdir>/CLAUDE.md` against `agents.<host>/<slug>.md`, apply meaningful changes to `agents/<slug>.md`, reverse-substitute machine-specific paths (NeoMatrix → Audio, /home/neomatrix → /home/neo). Verify by re-running the localizer and checking the round-trip diffs to zero.
- **Stand an agent down** → leave the `[[agents]]` entry, update `role` to `"Stood down. Watcher armed but channel inactive — only triggers if reactivated."`, rewrite the template to a minimal "you are stood down, arm watcher, standby" form. (Don't remove — keeping the watcher armed lets the agent be reactivated by a single channel message.)

## Don't

- **Don't edit localized files** (`giga-harness.*.toml`, `agents.*/`). They're regenerated on `setup-*.sh`. Edits are silently clobbered.
- **Don't hardcode the current user's paths** in canonical files. Use the canonical author's placeholders so localizers can substitute.
- **Don't skip `giga validate` after editing the TOML.** A typo in a channel participant or a missing inbox dir surfaces immediately; debugging it after a failed `giga launch` is much harder.
- **Don't run `giga launch` without first killing any prior wt.exe / tmux session** for the project. Multiple windows compete for the same agent tabs and you end up with stale per-agent sessions.
