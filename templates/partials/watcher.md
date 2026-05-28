**Arm the inbox watcher using the Monitor TOOL.** Use exactly this invocation — copy it verbatim:

```
Monitor(
  description: "giga inbox watcher",
  persistent: true,
  command: "giga watch --as {{AGENT}}"
)
```

**CRITICAL — read this twice.** The Monitor tool is the ONLY correct way to arm the watcher. It is NOT the Bash tool. Do NOT do any of these — they look plausible but all of them silently break the harness:

* `Bash(command: "giga watch --as {{AGENT}}")` — runs in foreground, blocks the conversation, you'll idle forever.
* `Bash(command: "giga watch --as {{AGENT}}", run_in_background: true)` — runs detached, but its stdout never reaches your conversation. The watcher process is alive but you receive ZERO notifications. This is the most common failure mode; the agent thinks it's listening but is actually deaf.
* `Bash(command: "giga watch --as {{AGENT}} &")` — same problem.
* `Monitor(persistent: false, ...)` — the tool stops after the first message, you'll miss everything after that.

Only the Monitor TOOL with `persistent: true` delivers each new message into your context as a notification you can react to. If you find yourself reaching for Bash to start the watcher: stop, and use the Monitor tool instead.

On first arm in a session, the watcher delivers any unread messages from prior sessions as the initial batch of notifications, then transitions to live tailing. Read those notifications before doing anything else.
