# `src/accounts/` — the credential manager

`giga switch` — the multi-account credential manager. Today it manages Claude
accounts only: it snapshots/swaps `~/.claude/.credentials.json` against named
snapshots under `~/.claude-accounts/<name>.json`.

## Why it's its own subsystem

`switch` shares **nothing** with the rest of the harness — no `Config`, no `Host`,
no `Runtime` dispatch, no channels, no coordination. It manages local OS
credential files for a single user. Keeping it in its own one-module subsystem
(rather than folding it into `runtime` or `mobility`) keeps that independence
explicit: the account model can grow (other runtimes, other credential shapes)
without entangling the swarm-topology code, and the swarm code never has to think
about credentials.

## switch (`switch.rs`)

`switch::run(Args { runtime, account, op })` dispatches on
`Op { Status, List, Setup, Add, Switch }`:

- `Status` — show the active account + list known ones.
- `List` — list known accounts.
- `Setup` — one-time: adopt the existing `~/.claude/.credentials.json` as a named
  snapshot.
- `Add` — provision an empty slot (populate it later by switching to it and
  running `/login`).
- `Switch` — make the named account active.

`ClaudePaths` decouples the on-disk locations from the live `$HOME` (so tests can
inject a `TempDir`): `cred_file()` (`~/.claude/.credentials.json`),
`accounts_dir()` (`~/.claude-accounts`), `active_marker()`
(`~/.claude-accounts/.active`), `account_file(name)`.

## Invariants

- **Real files, not symlinks.** Snapshots are copied files, because Claude's
  `/login` and silent OAuth refreshes do write-temp-then-rename — a symlink would
  be replaced rather than followed. Copies use atomic temp + fsync + rename at
  mode `0o600`.
- **Snapshot-the-active-creds-first.** On a switch, `op_switch` copies the
  *currently active* credentials back into the old account's snapshot **before**
  overwriting `~/.claude/.credentials.json` with the target — so any token
  refresh Claude did while running on the old account is preserved into its
  snapshot, not lost.
- **Unix-only.** `run` is `#[cfg(unix)]`-gated; the Windows variant bails. The
  snapshot model is POSIX-path + POSIX-mode based, and switching does **not**
  migrate already-running Claude processes (they hold auth in memory) — the
  closing message tells the user to restart their tabs.

## Cross-references

- [`../README.md`](../README.md) — the `src/` layered map.
- [`../foundation/README.md`](../foundation/README.md) — `atomic_io` /
  `dirs::home_dir` style primitives the credential copies mirror.
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §4 (the `switch` subcommand).
