# `src/foundation/` — the dependency-free leaf layer

The primitives every other subsystem builds on: the `===`-frame grammar,
byte-cursor file tailing, locked + atomic file writes, path/string normalization,
the subprocess/SSH/Tailscale substrate, the giga-self-invocation resolver, slice
naming, and the canonical timestamp format.

## The contract

`foundation/` is the **leaf** of the dependency graph. The rule it must never
break:

> **It knows nothing domain-y, and it never depends sideways.**

Concretely: no module here imports `crate::config`, `crate::coordination`,
`crate::transport`, or any other subsystem — only `std`, external crates
(`chrono`, `anyhow`, `serde`, `which`, `libc`), and other `foundation` modules.
Everything above leans *down* on `foundation`; nothing points back into it. If a
helper needs to know what a `Config` is, it does not belong here.

## Modules (one line each)

| Module | What it provides |
|---|---|
| [`frame`](./frame.rs) | The canonical `===`-delimited frame grammar — the **one** header/footer parser. `is_header_line`, `parse_header → Header`, `parse_footer → Footer`, `last_header_block → LastFrame`, `parse_posts → Vec<Post>`. |
| [`tail`](./tail.rs) | Byte-cursor tailing for append-only channel files. `read_delta`/`read_delta_lossy` read bytes `[from, to)`; consts `POLL_INTERVAL` (3s) and `RELOAD_EVERY_N_TICKS` (5). |
| [`append`](./append.rs) | The locked append — `append_with_lock` takes an exclusive file lock so cross-host appenders (post / merger / FYI archive) never tear a frame; `append_plain` is the fallback. |
| [`atomic_io`](./atomic_io.rs) | Crash-safe writes: `atomic_write` / `atomic_write_mode` (write-temp + fsync + rename) and `atomic_prepend`. |
| [`dirs`](./dirs.rs) | Home resolution with `$HOME` → `%USERPROFILE%` fallback: `home_dir`, `giga_home` (`~/.giga`). |
| [`paths`](./paths.rs) | Path-string normalization for SSH boundaries: `to_unix` (force forward slashes), `unix_join`. |
| [`proc`](./proc.rs) | Subprocess substrate: `run_checked`, `sh_escape` (POSIX shell-escape), `cmd_exe_echo` (read a Windows env var, incl. from WSL via cmd.exe interop). |
| [`ssh`](./ssh.rs) | SSH over the tailnet with fast-fail timeouts: `SSH_TIMEOUT_OPTS`, `rsync_ssh_e_arg`, `ssh_exec`. |
| [`self_invoke`](./self_invoke.rs) | Resolving the running `giga` binary for re-invocation: `giga_binary`, and `fresh_giga_binary` (re-resolves after a self-overwriting install where `current_exe()` becomes a `(deleted)` path). |
| [`slices`](./slices.rs) | Cross-host slice naming: `slice_path(merged, host)` inserts `<host>` before `.md` (`design-code.md` → `design-code.host-a.md`). |
| [`tailscale`](./tailscale.rs) | Tailnet roster + identity from `tailscale status --json`: pure `parse_status → TailnetStatus`, plus live `status`/`roster`/`is_logged_in`. `TailnetNode { dns_name, host_name, os }`. |
| [`timefmt`](./timefmt.rs) | The one canonical timestamp format: `GIGA_TS_FMT` (`%Y-%m-%dT%H:%M:%SZ`), `now_iso8601`, `parse_ts`. |

## Key invariants

- **One frame grammar.** `frame` replaces what used to be five divergent
  header/footer parsers across `post`/`watch`/`sweep`/`stale_wait`/`ui`. Its
  documented contract: `parse_header(l).is_some()` implies `is_header_line(l)`
  (`is_header_line` is the cheap structural gate; `parse_header` is the precise
  extractor that also validates the timestamp).
- **The 20-byte timestamp is load-bearing.** `timefmt::GIGA_TS_FMT` always renders
  to exactly 20 bytes (`...Z`), and `frame::is_header_line` keys header detection
  off that fixed-width tail. The two modules are a matched pair.
- **Locked append never tears frames.** `append::append_with_lock` is the single
  serialization point for concurrent writers (POSIX `flock` / Windows
  `LockFileEx`); it falls back to plain `O_APPEND` only if the lock can't be
  acquired.
- **Atomic writes don't leave half-files.** `atomic_io` always goes
  write-temp → fsync → rename, so a reader never sees a partial file (used for
  credentials at mode `0o600`, the registry, HANDOVER prepends).

## How it's used

Everything upstream pulls from here. A few representative consumers:

- `coordination::post` formats a frame and appends it via
  `foundation::append::append_with_lock`; `sweep`/`watch`/`stale_wait`/`codex_channel`
  *parse* frames via `foundation::frame`.
- `coordination::cursor` resolves `~/.giga` via `foundation::dirs::giga_home`.
- `transport::sync` uses `foundation::ssh` (`SSH_TIMEOUT_OPTS`,
  `rsync_ssh_e_arg`), `foundation::paths` (forward-slash normalization), and
  `foundation::slices::slice_path`.
- `transport::hosts` parses the tailnet roster via `foundation::tailscale`.
- `mobility::teleport`/`upgrade` re-invoke the binary via
  `foundation::self_invoke::{giga_binary, fresh_giga_binary}`.

## Cross-references

- [`../README.md`](../README.md) — the `src/` layered-architecture map.
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §2 (coordination model), §3
  (module map).
- [`../coordination/README.md`](../coordination/README.md) — the substrate built
  directly on `frame`/`tail`/`append`/`cursor`.
