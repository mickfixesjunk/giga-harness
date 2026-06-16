# giga-harness Claude Code skills

This directory holds the [Claude Code skills](https://docs.anthropic.com/en/docs/claude-code) bundled with the **giga-harness** repo. A skill is a self-contained instruction set (`SKILL.md` plus any supporting files) that Claude Code loads on demand when your request matches the skill's trigger. These three skills exist to help you operate **giga** — the manual multi-agent coordination harness in this repo — without having to remember the subcommand surface or the config conventions yourself.

You don't invoke these by hand. Just describe what you want ("set up giga for my project", "add a deploy agent", "how do I launch the swarm"); Claude Code picks the matching skill automatically. Each subfolder under `.claude/skills/` is one skill, named by its folder.

## Skill index

| Skill | When it fires | What it does |
|---|---|---|
| [`giga-harness`](./giga-harness/SKILL.md) | You mention giga, ask how to add/launch/manage agents, reference a `giga-harness-configs` project, or talk about file-based agent coordination (channels, watchers, inbox files). The general "how do I use this thing" skill. | Operating reference for the `giga` CLI: the subcommand cheat sheet (`setup`, `validate`, `init`, `launch`, `sweep`, `post`, `watch`), the coordination conventions (channel headers, `WAITING ON:` tags, the bench-scheduler protocol), the `giga-harness-configs` project shape, and the per-host setup flow. |
| [`giga-bootstrap-project`](./giga-bootstrap-project/SKILL.md) | You ask how to *start* using giga from scratch — "set up giga for my project", "where should my config live", "scaffold a new ecosystem", or anything about initial config storage/structure/layout. | Recommends where to lay out a new giga config so it scales from one box to multiple hosts/developers/ecosystems. Points most single-host users at `giga setup`, then documents three layouts: (A) a dedicated configs repo (recommended), (B) a subdirectory in the main project, and (C) local files with no git. |
| [`giga-add-agent`](./giga-add-agent/SKILL.md) | You want to add an agent to an existing ecosystem — "add an agent to giga", "scaffold a new [role] agent", "integrate this agent into the swarm". Requires being in a giga project dir (one containing `giga-harness.toml` + `agents/`). | Scaffolds a new agent consistently: generates the `[[agents]]` TOML entry, a canonical `agents/<slug>.md` CLAUDE.md template, and the bilateral `[[channels]]` for each peer (plus broadcast wiring), then tells you how to apply it. Edits only the canonical files (`giga-harness.toml`, `agents/*.md`) — never the generated per-host artifacts. |

## Notes

- **Canonical vs. generated files.** The add-agent and bootstrap skills are careful to edit only the canonical sources — `giga-harness.toml` and `agents/<slug>.md`. The localized per-host variants (`giga-harness.<host>.toml`, `agents.<host>/`) are produced by `setup-*.sh` and get clobbered on the next setup, so they are never hand-edited.
- **Fastest path.** For most single-host projects, `giga setup` launches a Claude Code session with a baked-in prompt that scaffolds the whole swarm end-to-end. The skills above cover the manual surface and the multi-host / multi-developer layouts that `giga setup` doesn't yet handle.
