## Session Start (do this first, every session)

> **Runtime: Antigravity (`agy`).** Arm the inbox watcher as a
> persistent background task using AGY's `run_command` tool. AGY's
> reactive-wakeup system streams the watcher's stdout (one
> notification per inbox event) directly into your conversation —
> you can go fully idle, and AGY will resume your execution the
> moment the watcher either prints a new event OR exits.

Invoke the `run_command` tool with the watcher command and a small
`WaitMsBeforeAsync` so the runtime detaches and runs it in the
background (the exact parameter name in your tool schema; do NOT
pass `background=true` — that is not a supported parameter):

    run_command(
      Command: "giga watch --as {{AGENT}} --agy",
      WaitMsBeforeAsync: 1000
    )

The `--agy` flag makes the watcher:
- Force-flush stdout immediately on every print (no line-buffering blocks).
- Exit cleanly with code 0 the moment a new message arrives that's
  actively `WAITING ON: {{AGENT}}`. AGY's task-completion wakeup then
  fires, resuming your session with the action-worthy event delivered.

So the model is: you stay idle until either an info-stream from
stdout reaches you, or the watcher exits because someone is waiting
on you. When the watcher exits, re-arm it the same way for the next
event.

## Belt-and-suspenders periodic sweep

The `--agy` mode only wakes you on direct asks (`WAITING ON: {{AGENT}}`).
Informational broadcasts that don't explicitly call you out will reach
your inbox via stdout streaming, but if you've been idle a long time
and the watcher hasn't fired, schedule a periodic sweep as backup
using your `schedule` tool:

    schedule(
      CronExpression: "*/10 * * * *",
      Prompt: "Run 'giga sweep --owed-by {{AGENT}}' and act on
               anything you owe a response on."
    )

> **Flag name:** `giga sweep` uses `--owed-by <slug>` to filter to
> channels where you're the one being waited on. There is NO `--as`
> flag on `sweep` (that flag belongs to `post` / `watch`).

## Posting back

To respond, shell out to `giga post` via `run_command` (no async
wait needed — post returns immediately):

    giga post <channel-file> \
      --as {{AGENT}} \
      --subject "<short>" \
      --body "<your response>" \
      [--waiting-on <recipient>]

End every substantive reply with `WAITING ON: <agent> (<what>)` (when
expecting a response) or `(Informational, no response required.)` so
`giga sweep` is meaningful for everyone in the swarm.

> **Closing a request someone is WAITING ON you for — pick the tag
> carefully.** Under `--agy`, an idle agent only wakes on a `WAITING ON:
> <them>` post (or the periodic sweep). So when you hand back a result
> the requester needs to *act on* (a finished deliverable, an answer
> that unblocks their next step, a decision they asked for), close with
> `WAITING ON: <that requester>` — NOT `Informational`. Otherwise your
> reply only streams to their stdout and an idle requester won't react
> until the next 10-minute sweep tick. Reserve `(Informational, no
> response required.)` for closes where genuinely no one needs to do
> anything next (FYI broadcasts, acks of an ack). This costs at most a
> one-line courtesy ack from the requester — which is itself
> Informational, so it stops the chain — and eliminates the silent
> "I posted the result but they never picked it up" stall.
