# giga-harness docs index

`giga` (binary `giga`, crate `giga-harness`, v0.6.55) is a manual multi-agent
coordination harness: it spawns N parallel AI coding agents that coordinate by
appending messages to shared Markdown "channel" files — no database, no message
bus, no LLM in the coordination loop. This page indexes the user-facing docs and
points you at the right starting place.

## Start here

- **Single-host swarm?** Begin with [QUICKSTART.md](QUICKSTART.md).
- **Multi-host swarm (across a tailnet)?** Begin with [REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md) — the multi-host swarm is a strict superset of single-host.
- **Need the full flag surface?** Reach for [COMMAND_REFERENCE.md](COMMAND_REFERENCE.md).
- **Need the `giga-harness.toml` field reference?** See [MANUAL_SETUP.md](MANUAL_SETUP.md).

## The docs

- **[QUICKSTART.md](QUICKSTART.md)** — Fast single-host getting-started guide. Walks you from a freshly installed binary to a working 2-agent swarm that's actually talking, with a copy-pasteable worked example.
- **[REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md)** — Operator runbook for the remote-channels feature: spreading one swarm across two or more machines on a Tailscale tailnet, so agents on different hosts coordinate over shared channels as if co-located. Covers adding a bare remote node and the operator/remote host roles.
- **[COMMAND_REFERENCE.md](COMMAND_REFERENCE.md)** — The authoritative, exhaustive command reference: every one of the 22 subcommands, every flag, every default, matching the binary's `--help` verbatim, grouped logically with the "when would I reach for this" context and the per-flag caveats that bite people.
- **[MANUAL_SETUP.md](MANUAL_SETUP.md)** — The hand-driven walkthrough (every step you'd type, every file you'd write) plus the field-by-field reference for `giga-harness.toml`. The reference for what `giga setup` does under the hood; best when you're debugging an unusual setup or authoring the config yourself.

## See also

- [../README.md](../README.md) — project overview and top-level entry point.
- [../ARCHITECTURE.md](../ARCHITECTURE.md) — developer-facing architecture overview: how the harness is built (channels, watchers, transports, slice-and-merge), with a map of every source subfolder.
- [../design/](../design/) — per-subsystem design notes (transport, remote/dual-write, broadcast-fanout, swarm-boss, teleport, stale-waits).
