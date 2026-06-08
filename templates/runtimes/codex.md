## Session Start (do this first, every session)

> **Runtime: Codex CLI.** Your watcher runs in a SEPARATE tmux pane
> named `{{AGENT}}-bridge` (spawned alongside your CLI pane by
> `giga launch`). The bridge process runs `giga watch --as {{AGENT}}
> --codex` and writes JSON envelopes into `$CODEX_CHANNEL_DIR/inbox/`
> — Codex CLI consumes them and surfaces them to you as inbound
> messages with `kind: "brief"`.

You don't need to arm anything yourself. The bridge is already running.

When you receive an envelope, the body tells you which channel + path
+ message. To respond, shell out to `giga post`:

    giga post <channel-file> \
      --as {{AGENT}} \
      --subject "<short>" \
      --body "<your response>" \
      [--waiting-on <recipient>]

Conventions:
- End every substantive reply with either `WAITING ON: <agent> (<what>)`
  (when expecting a response) or `(Informational, no response required.)`.
- Subject prefix `[<your-slug> YYYY-MM-DD HH:MM TZ]` so the inbox
  watcher's notification line shows enough context.

## Pane-only output from built-in commands MUST be posted to channel

Codex CLI's built-in slash commands (e.g. `/review`, `/diff`,
`/explain`) produce output in your pane only — they do NOT
automatically trigger a `giga post`. This is the most common comms
break for codex agents: the verdict sits in the pane unposted while
peer agents wait silently for a response.

**Rule:** every built-in slash command's FINAL step is a `giga post`
of the result to the relevant channel. Treat any pane-producing
command as TWO halves: (1) run the command, (2) `giga post` the
result.

**Example (`/review` verdict):**

- Subject: `[<your-slug> YYYY-MM-DD HH:MM TZ] PR #N review verdict — LGTM | changes requested | concerns`
- Body: formal verdict + findings with `file:line` references + recommended fix
- Channel: the PR author's bilateral with you (e.g. `codex-review-<peer>.md`)
- End with the standard `WAITING ON:` / `(Informational, no response required.)` closing tag

If you find yourself thinking "the work is done, the user can see
the output," you have skipped the post step. Re-read this section.

## Bridge-pane health

If you stop receiving envelopes, the bridge process may have died. The
operator can verify with `tmux list-windows -t giga-<swarm>` and restart
the `{{AGENT}}-bridge` pane manually if needed.

> **Limitation:** Codex CLI has no `Monitor`-equivalent push-into-context
> primitive, so notification delivery uses the envelope-file mechanism.
> Codex's "busy with another turn" error is the natural backpressure —
> envelopes are queued and retried by the bridge.
