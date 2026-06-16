# GitHub Actions — `giga-harness` CI/CD

This directory holds the two GitHub Actions workflows that build, test, and ship
`giga-harness` (the `giga` CLI, crate `giga-harness`).

| Workflow | File | Trigger | Purpose |
| --- | --- | --- | --- |
| **`ci`** | [`workflows/ci.yml`](workflows/ci.yml) | push / PR to `main` | Build + test on all three OSes; soft format check. |
| **`release`** | [`workflows/release.yml`](workflows/release.yml) | push of a `v*` tag | Cross-build release binaries and publish a GitHub Release. |

Both workflows set these defaults via the `ci` workflow's `env` block:

- `CARGO_TERM_COLOR: always`
- `RUST_BACKTRACE: short`

---

## `ci` — build + test

### Triggers

```yaml
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
```

Runs on every push to `main` and on every pull request targeting `main`.

### Job: `build-and-test`

- **Name:** `build + test (${{ matrix.os }})`
- **Runs on:** `${{ matrix.os }}` (one runner per matrix entry)
- **Matrix:**

  | `os` |
  | --- |
  | `ubuntu-latest` |
  | `macos-latest` |
  | `windows-latest` |

  `fail-fast: false` — a failure on one OS does not cancel the others, so you
  always see the full cross-platform picture.

### Steps

1. **`actions/checkout@v4`** — check out the repo.
2. **`dtolnay/rust-toolchain@stable`** — install the stable Rust toolchain.
3. **Cargo cache** (`actions/cache@v4`) — caches `~/.cargo/registry`,
   `~/.cargo/git`, and `target`. Cache key:
   `${{ runner.os }}-cargo-${{ hashFiles('Cargo.lock') }}`.
4. **Build (release profile)** — `cargo build --release`. The comment notes this
   is intentionally the **same build path the release workflow uses**, so CI
   exercises release-profile builds rather than debug-only ones.
5. **Test (release profile)** — `cargo test --release`.
6. **Format check** — `cargo fmt --check`, run under `shell: bash`.
   This is a **soft / warn-only** check: on a formatting diff it emits a
   `::warning::` annotation (`run \`cargo fmt\` to apply`) instead of failing the
   job. Style nits are deliberately **not** release-blockers.
   `shell: bash` is required so `||` short-circuits consistently on the Windows
   runner (whose default shell, `pwsh`, has different `||` semantics in older
   versions).

> **Note:** the CI build + test steps **are** hard gates (a compile error or a
> failing test fails the job); only the format check is soft.

---

## `release` — cross-build + publish

### Triggers

```yaml
on:
  push:
    tags:
      - 'v*'
```

Fires when a tag matching `v*` (e.g. `v0.6.55`) is pushed. There is no
branch/PR trigger — releases are tag-driven only.

### Permissions

```yaml
permissions:
  contents: write
```

The `contents: write` permission lets the final job create the GitHub Release
and upload assets to it.

### Job 1: `build` — produce per-target binaries

- **Name:** `build (${{ matrix.target }})`
- **Runs on:** `${{ matrix.os }}`
- **Matrix:** `fail-fast: false`, defined with an explicit `include` list of four
  build targets (OS runner + Rust target triple + archive name + binary name):

  | Runner (`os`) | Target triple (`target`) | Archive (`archive`) | Binary (`bin`) | Notes |
  | --- | --- | --- | --- | --- |
  | `ubuntu-latest` | `x86_64-unknown-linux-musl` | `giga-x86_64-unknown-linux-musl.tar.gz` | `giga` | **musl = statically linked**, no glibc dependency, runs on any Linux (old WSL distros included). Replaced an earlier `gnu`/`ubuntu-latest` build that broke on older systems with `GLIBC_2.34/2.39 not found`. |
  | `windows-latest` | `x86_64-pc-windows-msvc` | `giga-x86_64-pc-windows-msvc.zip` | `giga.exe` | Native Windows MSVC build. |
  | `macos-latest` | `aarch64-apple-darwin` | `giga-aarch64-apple-darwin.tar.gz` | `giga` | Apple Silicon (arm64), built natively on the arm64 runner. |
  | `macos-latest` | `x86_64-apple-darwin` | `giga-x86_64-apple-darwin.tar.gz` | `giga` | Intel Mac (x86_64), **cross-compiled** from the arm64 `macos-latest` runner (added in v0.6.53). The toolchain action installs the x86_64 target via `targets:`, producing a real x86_64 Mach-O. Rosetta-using Intel Macs run it natively; Apple Silicon prefers the aarch64 tarball above. |

  **Release target triples (4 total):**
  - `x86_64-unknown-linux-musl`
  - `x86_64-pc-windows-msvc`
  - `aarch64-apple-darwin`
  - `x86_64-apple-darwin`

#### Steps (per matrix entry)

1. **`actions/checkout@v4`** — check out the repo.
2. **`dtolnay/rust-toolchain@stable`** with `targets: ${{ matrix.target }}` —
   install stable Rust **and** the matrix target triple (this is what enables the
   Intel-Mac cross-compile and the musl target).
3. **Install musl tooling (Linux only)** — `if: matrix.target == 'x86_64-unknown-linux-musl'`;
   runs `sudo apt-get update -qq && sudo apt-get install -y musl-tools`.
4. **Cargo cache** (`actions/cache@v4`) — caches `~/.cargo/registry`,
   `~/.cargo/git`, `target`. Cache key:
   `${{ runner.os }}-cargo-${{ matrix.target }}-${{ hashFiles('Cargo.lock') }}`
   (per-target, unlike CI's per-OS key).
5. **Build** — `cargo build --release --target ${{ matrix.target }}`.
6. **Package (unix)** — `if: matrix.os != 'windows-latest'`; `cd` into
   `target/<target>/release` and `tar -czf` the binary into `<archive>` (`.tar.gz`).
7. **Package (windows)** — `if: matrix.os == 'windows-latest'`, `shell: pwsh`;
   `Compress-Archive` the `.exe` into the `.zip` archive.
8. **Upload artifact** (`actions/upload-artifact@v4`) — uploads `<archive>` under
   the artifact name `<archive>` with `if-no-files-found: error` (a missing
   archive fails the job).

### Job 2: `release` — publish the GitHub Release

- **`needs: build`** — waits for all four `build` matrix jobs to succeed.
- **Runs on:** `ubuntu-latest`.

#### Steps

1. **`actions/checkout@v4`** — needed for the repo-tracked `install.sh` /
   `install.ps1`.
2. **Download all artifacts** (`actions/download-artifact@v4`) — downloads every
   build artifact into `dist/` with `merge-multiple: true` (flattens all four
   archives into one directory).
3. **List artifacts** — `ls -lh dist/` (sanity log of what will be published).
4. **Create release** (`softprops/action-gh-release@v2`) — creates the GitHub
   Release for the pushed tag and attaches:
   - `dist/*.tar.gz` (Linux + both macOS archives)
   - `dist/*.zip` (Windows archive)
   - `install.sh` (Unix installer, from repo root)
   - `install.ps1` (Windows installer, from repo root)

   With `generate_release_notes: true` (auto-generated notes from commits/PRs)
   and `fail_on_unmatched_files: true` (the job fails if any of the listed file
   globs match nothing — e.g. a missing archive or installer).

---

## How a release is cut

1. **Bump the version** in `Cargo.toml` (currently `version = "0.6.55"`) and land
   it on `main` (CI must be green).
2. **Tag the commit** with a matching `v`-prefixed tag and push the tag:

   ```bash
   git tag v0.6.55
   git push origin v0.6.55
   ```

   Pushing the `v*` tag is the **only** trigger for the `release` workflow.
3. The `release` workflow runs:
   - the **`build`** matrix cross-compiles all four target triples and uploads one
     archive each;
   - the **`release`** job downloads every archive, then publishes a GitHub
     Release containing the four archives plus `install.sh` and `install.ps1`,
     with auto-generated release notes.
4. End users install from the published assets via `install.sh` (Unix) or
   `install.ps1` (Windows), which fetch the matching archive for their platform.

> **Tip:** keep the `Cargo.toml` version and the `v<version>` tag in sync — the
> tag is what names the Release, while the version is what the binary reports.
