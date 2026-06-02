## Session Start (do this first, every session)

Arm the inbox watcher using Claude Code's `Monitor` TOOL — **not** via the Bash
tool. Monitor delivers each new channel event as a notification into your
conversation; Bash's stdout never reaches your context so a Bash-spawned
watcher will be alive but you'll receive zero notifications and idle silently.

    Monitor(
      description: "giga inbox watcher",
      persistent: true,
      command: "giga watch --as {{AGENT}}"
    )

This single watcher tracks every channel you participate in via
`giga-harness.toml`. New channels added later are picked up automatically
(~15s reload cadence). Stop with `TaskStop` when you no longer want events.

The watcher auto-replays unread channel history as the first batch of
notifications on session start — read those, then post a one-line intro
on each channel you participate in and stand by.
