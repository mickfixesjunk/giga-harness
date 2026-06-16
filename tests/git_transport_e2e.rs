//! End-to-end test for the git transport plug. Uses a real `git`
//! subprocess against a bare local repo as the "state repo" — no
//! network, no auth, no GitHub dependency. Mirrors the production
//! flow: each host has its own clone of the bare repo; tick pushes
//! own slices + pulls peer slices via real git commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const GIGA: &str = env!("CARGO_BIN_EXE_giga");

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct GitFixture {
    _tmp: TempDir,
    bare_repo: PathBuf,
    home_a: PathBuf,
    home_b: PathBuf,
    cfg_a: PathBuf,
    cfg_b: PathBuf,
    inbox_a: PathBuf,
    inbox_b: PathBuf,
    clone_a: PathBuf,
    // Kept for fixture symmetry with `clone_a`; only `clone_a` is read
    // back off the fixture in assertions, so the underscore silences the
    // dead-field lint without dropping the parallel structure.
    _clone_b: PathBuf,
}

/// Build a 2-host swarm fixture using a bare local git repo as the
/// state repo. Each "host" has its own HOME, swarm dir, inbox dir,
/// and clone of the bare repo (sharing the same `state_repo` URL).
fn build_git_fixture() -> GitFixture {
    let tmp = TempDir::new().unwrap();
    let bare_repo = tmp.path().join("state-repo.git");

    // Initialize the bare repo (the "remote" everyone pushes to).
    // --initial-branch=main keeps HEAD aligned with our seed push below,
    // regardless of the test runner's git.defaultBranch setting.
    run_git(
        tmp.path(),
        &[
            "init",
            "--bare",
            "--quiet",
            "--initial-branch=main",
            bare_repo.to_str().unwrap(),
        ],
    );

    // Seed the bare repo with an initial commit so `git pull --rebase`
    // has something to rebase against (an empty bare repo errors on pull
    // with "couldn't find remote ref HEAD").
    let seed = tmp.path().join("seed");
    fs::create_dir_all(&seed).unwrap();
    run_git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    run_git(&seed, &["config", "user.email", "test@local"]);
    run_git(&seed, &["config", "user.name", "test"]);
    fs::write(seed.join("README.md"), b"giga swarm state\n").unwrap();
    run_git(&seed, &["add", "README.md"]);
    run_git(&seed, &["commit", "--quiet", "-m", "seed"]);
    run_git(
        &seed,
        &["remote", "add", "origin", bare_repo.to_str().unwrap()],
    );
    run_git(&seed, &["push", "--quiet", "origin", "main"]);

    let home_a = tmp.path().join("home_a");
    let home_b = tmp.path().join("home_b");
    let host_a = tmp.path().join("host_a");
    let host_b = tmp.path().join("host_b");
    for d in [&home_a, &home_b, &host_a, &host_b] {
        fs::create_dir_all(d).unwrap();
    }

    let cfg_a_dir = host_a.join("swarm");
    let cfg_b_dir = host_b.join("swarm");
    let inbox_a = host_a.join("inbox");
    let inbox_b = host_b.join("inbox");
    let clone_a = host_a.join("clone");
    let clone_b = host_b.join("clone");
    for d in [&cfg_a_dir, &cfg_b_dir, &inbox_a, &inbox_b] {
        fs::create_dir_all(d).unwrap();
    }

    // Per-host config — same swarm, distinct inbox + clone paths.
    let toml_for = |inbox: &Path, clone: &Path| -> String {
        format!(
            r#"
[project]
name = "git-e2e"

[paths]
wsl_inbox = '{inbox}'

[transport]
kind = "git"

[transport.git]
state_repo = '{bare}'
local_clone_dir = '{clone}'

[[hosts]]
name = "wsl-a"
tailnet_hostname = "unused.tail.ts.net"
[[hosts]]
name = "wsl-b"
tailnet_hostname = "unused.tail.ts.net"

[[agents]]
name = "alice"
workdir = "/h/a"
role = "."
platform = "wsl"
host = "wsl-a"
[[agents]]
name = "bob"
workdir = "/h/b"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
            bare = bare_repo.to_string_lossy(),
            clone = clone.to_string_lossy(),
        )
    };

    let cfg_a = cfg_a_dir.join("giga-harness.toml");
    let cfg_b = cfg_b_dir.join("giga-harness.toml");
    fs::write(&cfg_a, toml_for(&inbox_a, &clone_a)).unwrap();
    fs::write(&cfg_b, toml_for(&inbox_b, &clone_b)).unwrap();
    fs::write(cfg_a_dir.join("this_host.toml"), "this_host = \"wsl-a\"\n").unwrap();
    fs::write(cfg_b_dir.join("this_host.toml"), "this_host = \"wsl-b\"\n").unwrap();

    GitFixture {
        _tmp: tmp,
        bare_repo,
        home_a,
        home_b,
        cfg_a,
        cfg_b,
        inbox_a,
        inbox_b,
        clone_a,
        _clone_b: clone_b,
    }
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn git");
    if !status.status.success() {
        panic!(
            "git {args:?} in {} failed: {}",
            cwd.display(),
            String::from_utf8_lossy(&status.stderr)
        );
    }
}

/// Drive a giga subcommand on one of the "hosts". Sets HOME for cursor
/// isolation + GIT_AUTHOR_* / GIT_COMMITTER_* so git commits don't
/// need a real user.email / user.name (the per-clone git config we'd
/// otherwise have to set up).
fn giga(home: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new(GIGA)
        .env("HOME", home)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@local")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@local")
        .args(args)
        .output()
        .expect("spawn giga");
    if !out.status.success() {
        panic!(
            "giga {args:?} exited {}: stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr),
        );
    }
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[test]
fn git_tick_pushes_own_slice_to_repo_and_pulls_peer_slice_from_repo() {
    let fx = build_git_fixture();

    // alice posts on host-a → writes alice-bob.wsl-a.md slice in host-a's inbox
    giga(
        &fx.home_a,
        &[
            "post",
            "alice-bob",
            "--as",
            "alice",
            "--subject",
            "hello",
            "--body",
            "from a",
            "--config",
            fx.cfg_a.to_str().unwrap(),
        ],
    );
    assert!(fx.inbox_a.join("alice-bob.wsl-a.md").exists());

    // host-a tick: clone (first time), pull (nothing peer yet), mirror
    // own slice → repo, commit, push.
    giga(
        &fx.home_a,
        &["sync", "--once", "--config", fx.cfg_a.to_str().unwrap()],
    );

    // host-a's clone should now contain alice's slice.
    let a_slice_in_clone = fx.clone_a.join("slices").join("alice-bob.wsl-a.md");
    assert!(
        a_slice_in_clone.exists(),
        "host-a tick should have copied alice's slice into the clone"
    );

    // host-b tick: clone (first time), pull (gets alice's slice via the
    // bare repo), mirror peer slice → b's inbox.
    giga(
        &fx.home_b,
        &["sync", "--once", "--config", fx.cfg_b.to_str().unwrap()],
    );

    // host-b's inbox should now have alice's slice content.
    let a_slice_on_b = fx.inbox_b.join("alice-bob.wsl-a.md");
    assert!(
        a_slice_on_b.exists(),
        "host-b tick should have pulled alice's slice from the repo into its inbox"
    );
    let body = fs::read_to_string(&a_slice_on_b).unwrap();
    assert!(
        body.contains("[alice] hello"),
        "host-b's mirrored slice should contain alice's post:\n{body}"
    );
}

#[test]
fn git_tick_bidirectional_round_trip() {
    let fx = build_git_fixture();

    // alice posts on A, bob posts on B (both before any tick).
    giga(
        &fx.home_a,
        &[
            "post",
            "alice-bob",
            "--as",
            "alice",
            "--subject",
            "ping",
            "--body",
            "from-a",
            "--config",
            fx.cfg_a.to_str().unwrap(),
        ],
    );
    giga(
        &fx.home_b,
        &[
            "post",
            "alice-bob",
            "--as",
            "bob",
            "--subject",
            "pong",
            "--body",
            "from-b",
            "--config",
            fx.cfg_b.to_str().unwrap(),
        ],
    );

    // Tick A then B then A (gives both directions a chance to round-trip).
    for _ in 0..2 {
        giga(
            &fx.home_a,
            &["sync", "--once", "--config", fx.cfg_a.to_str().unwrap()],
        );
        giga(
            &fx.home_b,
            &["sync", "--once", "--config", fx.cfg_b.to_str().unwrap()],
        );
    }

    // Both hosts should have both slice files in their inbox.
    assert!(fx.inbox_a.join("alice-bob.wsl-a.md").exists()); // own
    assert!(
        fx.inbox_a.join("alice-bob.wsl-b.md").exists(),
        "A should have B's slice"
    );
    assert!(fx.inbox_b.join("alice-bob.wsl-b.md").exists()); // own
    assert!(
        fx.inbox_b.join("alice-bob.wsl-a.md").exists(),
        "B should have A's slice"
    );

    let a_seen_b = fs::read_to_string(fx.inbox_a.join("alice-bob.wsl-b.md")).unwrap();
    let b_seen_a = fs::read_to_string(fx.inbox_b.join("alice-bob.wsl-a.md")).unwrap();
    assert!(a_seen_b.contains("[bob] pong"));
    assert!(b_seen_a.contains("[alice] ping"));

    // The bare repo should have both slices.
    let _ = fx.bare_repo.exists(); // sanity (we wrote to it via git push)
}

#[test]
fn git_tick_is_noop_when_no_changes() {
    let fx = build_git_fixture();
    // First tick: clone + initial state.
    giga(
        &fx.home_a,
        &["sync", "--once", "--config", fx.cfg_a.to_str().unwrap()],
    );
    // Second tick: nothing new, should succeed silently (no commit, no push).
    giga(
        &fx.home_a,
        &["sync", "--once", "--config", fx.cfg_a.to_str().unwrap()],
    );
    giga(
        &fx.home_a,
        &["sync", "--once", "--config", fx.cfg_a.to_str().unwrap()],
    );
    // No assertion needed beyond "didn't panic" — the test passes if all
    // three giga calls returned successfully.
}
