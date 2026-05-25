# Quickstart

Three flows: bootstrap a new project, add an agent to a running one, stand an agent down.

If you drive Claude Code, the `giga-bootstrap-project` and `giga-add-agent` skills walk these flows interactively. The commands below are the manual equivalents — useful for understanding what's going on and for non-Claude users.

## 1. Bootstrap a new project

A "project" is one TOML file + a folder of agent templates. From zero to N agents talking:

```sh
# Install giga (Linux/WSL):
curl -sSfL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash

# Scaffold the project directory:
mkdir -p ~/giga-configs/myproject/agents
cd ~/giga-configs/myproject
```

Write `giga-harness.toml`. Minimal 2-agent example:

```toml
[project]
name = "myproject"

[paths]
wsl_inbox = "/home/me/projects/inbox"   # required if any channel side = "wsl"

[[agents]]
name = "alice"
workdir = "/home/me/projects/alice-work"
role = "Implementation."
platform = "wsl"
claudemd_template = "agents/alice.md"

[[agents]]
name = "bob"
workdir = "/home/me/projects/bob-work"
role = "Review."
platform = "wsl"
claudemd_template = "agents/bob.md"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
purpose = "Implementation ↔ review handoffs."
```

Write `agents/alice.md` and `agents/bob.md`. Each needs:

```markdown
# alice agent

You are the **implementation** agent for myproject.

## Session Start

1. Post intro on each of your channels via `giga post <channel> --as alice --subject "online" --body "..."`.
2. Arm the Monitor below.
3. Standby.

## Channels you watch

\```
Monitor(persistent: true, command: "giga watch --as alice")
\```

One watcher auto-discovers every channel where you participate (per `giga-harness.toml`).

## Convention

Every channel message closes with `WAITING ON: <agent> (<what>)` or `(Informational, no response required.)`.
```

Validate, scaffold inboxes + CLAUDE.md, launch:

```sh
giga validate
giga init       # creates inbox files + renders each agent's CLAUDE.md in their workdir
giga launch     # one terminal tab per agent (wt on Windows/WSL, tmux on Linux)
```

That's it. Each tab opens in the agent's workdir with `claude` already running. Each agent reads its CLAUDE.md, arms its watcher, posts its intro, waits for the other side to talk.

**Scaling up.** For more than ~3 agents on more than one host, the `giga-bootstrap-project` skill scaffolds the harder layout — canonical templates in `agents/`, per-host localizer in `setup-<host>.sh`, generated `agents.<host>/` and `giga-harness.<host>.toml` so the same canonical config works for multiple developers/hosts. Loading that skill via Claude Code (`/giga-bootstrap-project`) is faster than recreating the pattern by hand.

## 2. Add an agent to a running ecosystem

Three ways, in increasing autonomy:

**a. Single command (fastest, runnable from anywhere — including from inside a swarm agent's session):**

```sh
giga add-agent \
  --name <slug> \
  --workdir <abs-path> \
  --role "<one-liner>" \
  --platform wsl \
  --peer <existing-agent> [--peer <another>] \
  --config <path-to-giga-harness.toml>
```

This appends `[[agents]]` + per-peer `[[channels]]` blocks to the canonical TOML (`toml_edit` preserves comments + formatting), adds the slug to any `_broadcast.md` channel's participants, and scaffolds `agents/<slug>.md` with a minimal stub. Re-validates after writing. Use `--dry-run` to preview. Use `--template <path>` to supply a custom CLAUDE.md instead of the auto-generated stub.

**b. Via the Claude Code skill (interactive, walks you through choices):**

```
/giga-add-agent
```

Asks slug / workdir / peers / etc., then does the same edits.

**c. Manual TOML editing** if you prefer to write each block by hand.

Once scaffolded:

```sh
./setup-<host>.sh    # if your project has a per-host localizer
giga validate

# Spawn JUST the new agent into the live ecosystem:
giga launch --only <slug> --new-window <config>
```

What this does:
- `init` (run automatically by `launch` unless `--skip-init`) creates the new agent's bilateral inbox files and renders their CLAUDE.md. Existing inbox files are kept; existing in-flight Claude sessions don't re-read their CLAUDE.md, so they're not disturbed.
- `--only <slug>` spawns just the named agent's tab.
- `--new-window` (wt only) forces a fresh window — useful if you've torn the original launch window apart and have one window per agent arranged on screen. Drop it if you want the new tab to dock into the existing window named `giga-<project>`.

**Bootstrap visibility.** The new agent's single-Monitor watcher auto-discovers its bilateral channels from the config on startup, so it sees its peers immediately. Peers on the auto-discovery design (`giga watch --as <slug>` with no channel arg) pick up the new bilateral on their next config reread (~15s), zero manual re-arming.

Peers still on the legacy per-channel design need one message asking them to either arm an additional `Monitor(persistent: true, command: "giga watch <new-channel>.md --as <peer>")` or migrate to the new single-Monitor design.

## 3. Stand an agent down

Pause an agent without losing the ability to reactivate cleanly. **Don't reach for "remove" first** — stand-down keeps the structure intact, makes reactivation a 30-second edit, and preserves the inbox history.

### a. Announce

On `_broadcast.md` (or fan out to each of their bilateral channels if you don't have a broadcast channel yet):

```sh
giga post _broadcast.md --as <announcer> --subject "stand-down: <slug>" \
  --body "Standing <slug> down as of $(date -u +%Y-%m-%d). <Brief reason.>
Their watcher will be armed in standby mode — they won't respond on
their bilaterals. To reactivate later: post on any of their channels and
ping the user to restore the full template + role.
(Informational, no response required.)"
```

This tells peers to stop expecting responses from `<slug>` and stop routing work to them.

### b. Update the canonical config

In `giga-harness.toml`:

- **Keep** the `[[agents]]` entry. Update its `role` field to:
  ```toml
  role = "Stood down. Watcher armed but channel inactive — only triggers if reactivated."
  ```
- **Keep** all `[[channels]]` blocks listing them as a participant. Removing them now would require recreating the bilaterals on reactivation, which forfeits the audit trail.
- **Keep** them in `_broadcast.md` participants. They should still receive broadcasts if they're ever woken up.

### c. Rewrite the agent template to a minimal standby form

Replace `agents/<slug>.md` with:

```markdown
# <slug> agent (stood down)

You are currently **stood down**. You exist to keep the channel structure intact for possible reactivation; you do not initiate work.

## Session Start

1. Read `./HANDOVER.md` if it exists.
2. Arm `Monitor(persistent: true, command: "giga watch --as <slug>")`.
3. Standby. If a message arrives, read it. If it asks you to do work, reply on the originating channel: "I'm currently stood down. Confirm with the user before I resume." Don't act without confirmation.

## Convention

Same as before — close every reply with the explicit `WAITING ON: ...` or `(Informational, ...)` tag.
```

### d. Re-localize + close the agent's tab

```sh
./setup-<host>.sh    # regenerates agents.<host>/<slug>.md from the new template
```

Then close the agent's terminal tab. Their watcher won't be armed once the session ends; peers' watchers no longer have anyone on the other side of the bilateral. The channel files stay on disk so the history isn't lost.

### Reactivation (later, if needed)

1. Restore the canonical template (`agents/<slug>.md`) and role line in `giga-harness.toml`.
2. `./setup-<host>.sh && giga launch --only <slug> --new-window <config>`.
3. Announce on `_broadcast.md`: "`<slug>` reactivated as of <date>. Routing to them via `<their-channels>` is open again."

Reactivation is intentionally cheap because the structure was never broken — only the agent's behavior changed.

## 4. Remove an agent permanently (rare)

Stand-down is almost always better. Reach for full removal only when the role itself is dissolved — e.g., the project no longer has the underlying responsibility (deleted the relevant repo, killed the product surface, merged the role into another agent).

```sh
# 1. Announce on _broadcast.md (same shape as stand-down's announcement,
#    but say "removed" not "stood down" so peers don't expect reactivation).

# 2. Edit giga-harness.toml:
#    - delete the [[agents]] block for <slug>
#    - delete every [[channels]] block that lists <slug> as participant
#    - remove <slug> from _broadcast.md participants

# 3. Delete the canonical template:
rm agents/<slug>.md
rm agents.<host>/<slug>.md    # if you have a per-host localized version

# 4. Validate:
giga validate    # should pass; if it complains about dangling participants, you missed one

# 5. Re-localize + close the agent's tab:
./setup-<host>.sh
```

The inbox files for the deleted channels stay on disk as historical records. `giga init` won't recreate them now that the config doesn't list them, but it also won't delete them. Archive or `rm` them manually if you want a clean inbox dir.

## Reference: which command for which goal

| Goal | Command |
|------|---------|
| Cold start a fresh project | `giga validate && giga init && giga launch` |
| Validate a config edit | `giga validate <config>` |
| See open WAITING ON tags | `giga sweep <config>` |
| Scaffold a new agent | `giga add-agent --name <slug> --workdir <path> --role "..." --peer <existing>` |
| Add an agent's tab to the live ecosystem | `giga launch --only <slug> --new-window <config>` |
| Post a properly-formatted message | `giga post <channel> --as <agent> --subject ... --body ...` |
| Long-running watcher | `giga watch --as <agent>` (config-aware) |
| Legacy single-channel watcher | `giga watch <channel> --as <agent>` |

Full subcommand details: see [README.md](README.md). Convention details (channel headers, `WAITING ON` tags, bench-scheduler protocol): see the `giga-harness` skill at `.claude/skills/giga-harness/SKILL.md`.
