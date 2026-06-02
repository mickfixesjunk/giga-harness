## Session Start (do this first, every session)

> **Runtime: Antigravity (`agy`).** Arm the inbox watcher as a
> persistent background task using AGY's `run_command` primitive.
> AGY's reactive-wakeup system streams the watcher's stdout (one
> notification per inbox event) directly into your conversation
> inbox — you can go fully idle, and AGY will resume your execution
> context the moment the watcher either prints a new event OR exits.

    run_command("giga watch --as {{AGENT}} --agy", background=true)

The `--agy` flag makes the watcher:
- Force-flush stdout immediately on every print (no line-buffering blocks).
- Exit cleanly with code 0 the moment a new message arrives that's
  actively `WAITING ON: {{AGENT}}`. AGY's task-completion wakeup then
  fires, resuming your session with the action-worthy event delivered.

So the model is: you stay idle until either an info-stream from
stdout reaches you, or the watcher exits because someone is waiting
on you. When the watcher exits, re-arm it for the next event:

    run_command("giga watch --as {{AGENT}} --agy", background=true)

## Belt-and-suspenders periodic sweep

The `--agy` mode only wakes you on direct asks (`WAITING ON: {{AGENT}}`).
Informational broadcasts that don't explicitly call you out will reach
your inbox via stdout streaming, but if you've been idle a long time
and the watcher hasn't fired, schedule a periodic sweep as backup:

    schedule({
      "CronExpression": "*/10 * * * *",
      "Prompt": "Run `giga sweep --as {{AGENT}}` and act on anything
                 you owe a response on."
    })

## Posting back

To respond, shell out to `giga post`:

    giga post <channel-file> \
      --as {{AGENT}} \
      --subject "<short>" \
      --body "<your response>" \
      [--waiting-on <recipient>]

End every substantive reply with `WAITING ON: <agent> (<what>)` (when
expecting a response) or `(Informational, no response required.)` so
`giga sweep` is meaningful for everyone in the swarm.
