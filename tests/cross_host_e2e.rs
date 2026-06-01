//! End-to-end integration tests for the remote-channels slice-and-merge
//! pipeline. Per REMOTE_DESIGN.md §6 step 8.
//!
//! Each test sets up a 2-host swarm fixture (two inbox directories on the
//! local filesystem — `host_a/` and `host_b/`) and exercises the
//! pipeline by invoking the real `giga` binary as a subprocess. The sync
//! transport (rsync over SSH) is simulated by manually copying slice
//! files between the two host inbox dirs at the points sync would have
//! pushed — that decouples the e2e from SSH/tailnet availability while
//! still exercising the post -> slice -> merger -> merged path that's
//! the heart of the correctness story.
//!
//! The actual rsync execution is covered separately by:
//!   - sync.rs unit tests (planning is pure)
//!   - the step 10 live 2-host smoke (real tailnet)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Path to the built giga binary. Cargo provides this for integration
/// tests; rebuilds on demand if the source changed.
const GIGA: &str = env!("CARGO_BIN_EXE_giga");

/// Build a 2-host swarm fixture mirroring the design's canonical
/// example: alice on host-a, bob on host-b, one bilateral. Each host
/// gets its own inbox dir + its own canonical TOML copy + its own
/// `this_host.toml`. Returns (TempDir, paths).
struct Fixture {
    _tmp: TempDir,
    /// Per-test isolated HOME so `~/.giga/merge-cursors/<channel>/<host>.pos`
    /// doesn't bleed between tests. We simulate TWO hosts in ONE process,
    /// so each "host" needs its OWN HOME (otherwise their cursor files
    /// would clash — in production each physical host has its own $HOME).
    home_a: PathBuf,
    home_b: PathBuf,
    host_a_swarm_dir: PathBuf,
    host_a_inbox: PathBuf,
    host_b_swarm_dir: PathBuf,
    host_b_inbox: PathBuf,
}

impl Fixture {
    fn host_a_config(&self) -> PathBuf {
        self.host_a_swarm_dir.join("giga-harness.toml")
    }
    fn host_b_config(&self) -> PathBuf {
        self.host_b_swarm_dir.join("giga-harness.toml")
    }
}

fn build_fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let home_a = tmp.path().join("home_a");
    let home_b = tmp.path().join("home_b");
    let host_a_dir = tmp.path().join("host_a");
    let host_b_dir = tmp.path().join("host_b");
    fs::create_dir_all(&home_a).unwrap();
    fs::create_dir_all(&home_b).unwrap();
    fs::create_dir_all(&host_a_dir).unwrap();
    fs::create_dir_all(&host_b_dir).unwrap();

    let host_a_swarm_dir = host_a_dir.join("swarm");
    let host_b_swarm_dir = host_b_dir.join("swarm");
    let host_a_inbox = host_a_dir.join("inbox");
    let host_b_inbox = host_b_dir.join("inbox");
    for d in [
        &host_a_swarm_dir,
        &host_b_swarm_dir,
        &host_a_inbox,
        &host_b_inbox,
    ] {
        fs::create_dir_all(d).unwrap();
    }

    // Both hosts hold the same canonical TOML, but each points at its
    // own local inbox dir (simulating per-host filesystem layouts).
    let toml_for = |inbox: &Path| -> String {
        format!(
            r#"
[project]
name = "remote-test"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
        )
    };

    fs::write(host_a_swarm_dir.join("giga-harness.toml"), toml_for(&host_a_inbox)).unwrap();
    fs::write(host_b_swarm_dir.join("giga-harness.toml"), toml_for(&host_b_inbox)).unwrap();
    fs::write(host_a_swarm_dir.join("this_host.toml"), "this_host = \"wsl-a\"\n").unwrap();
    fs::write(host_b_swarm_dir.join("this_host.toml"), "this_host = \"wsl-b\"\n").unwrap();

    Fixture {
        _tmp: tmp,
        home_a,
        home_b,
        host_a_swarm_dir,
        host_a_inbox,
        host_b_swarm_dir,
        host_b_inbox,
    }
}

/// Wrapper around `Command::new(GIGA)` that bails the test on non-zero
/// exit + captures stdout for assertions. Always sets HOME to the
/// fixture's isolated dir so per-test cursors don't bleed.
fn giga(home: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new(GIGA)
        .env("HOME", home)
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

/// Simulate `giga sync` for a single slice file: copy the slice from
/// source to dest inbox, preserving filename. Real sync uses rsync over
/// Tailscale SSH (covered by unit tests for the planner + by step 10).
fn fake_sync_slice(src_inbox: &Path, dst_inbox: &Path, slice_filename: &str) {
    let src = src_inbox.join(slice_filename);
    let dst = dst_inbox.join(slice_filename);
    fs::copy(&src, &dst).unwrap_or_else(|e| {
        panic!(
            "fake_sync_slice: copy {} -> {}: {e}",
            src.display(),
            dst.display(),
        )
    });
}

// ===========================================================================
// Tests
// ===========================================================================

#[test]
fn post_dual_writes_to_slice_and_merged_no_merger_needed() {
    // v0.3.5 (REMOTE_DUAL_WRITE_DESIGN.md): on a cross-host channel
    // post dual-writes (slice + merged). Local watcher visibility
    // does NOT depend on the merger daemon liveness any more.
    // Pre-v0.3.5 this test asserted "post NOT touching merged"
    // because merger was the sole writer to merged; v0.3.5 inverts
    // that invariant: post owns the merged write for OWN posts;
    // merger only merges PEER slices.
    let fx = build_fixture();

    giga(&fx.home_a, &[
        "post",
        "alice-bob",
        "--as",
        "alice",
        "--subject",
        "hello-from-alice",
        "--body",
        "first message",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);

    let slice = fx.host_a_inbox.join("alice-bob.wsl-a.md");
    let merged_a = fx.host_a_inbox.join("alice-bob.md");
    assert!(slice.exists(), "post should create the slice file");
    assert!(
        merged_a.exists(),
        "v0.3.5: post should ALSO write the merged file (dual-write)"
    );

    let slice_content = fs::read_to_string(&slice).unwrap();
    let merged_content = fs::read_to_string(&merged_a).unwrap();
    assert!(slice_content.contains("[alice] hello-from-alice"));
    assert!(merged_content.contains("[alice] hello-from-alice"));
    assert!(merged_content.contains("first message"));
    assert_eq!(
        slice_content, merged_content,
        "dual-write must produce byte-identical content"
    );

    // Run merger once. It should NOT modify the merged file — own slice
    // is excluded from tracked slices (post's responsibility), and no
    // peer slice exists yet.
    let merged_len_before = fs::metadata(&merged_a).unwrap().len();
    giga(&fx.home_a, &[
        "merger",
        "--once",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);
    let merged_len_after = fs::metadata(&merged_a).unwrap().len();
    assert_eq!(
        merged_len_before, merged_len_after,
        "merger must not touch own-slice content already in merged via dual-write"
    );
}

#[test]
fn round_trip_bilateral_via_simulated_sync() {
    let fx = build_fixture();

    // 1) alice posts on host-a.
    giga(&fx.home_a, &[
        "post",
        "alice-bob",
        "--as",
        "alice",
        "--subject",
        "ping",
        "--body",
        "ping from alice",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);

    // 2) Simulate sync: alice's slice on host-a -> host-b's inbox.
    fake_sync_slice(&fx.host_a_inbox, &fx.host_b_inbox, "alice-bob.wsl-a.md");

    // 3) host-b's merger picks up the incoming slice + appends to its
    //    local merged file. Note home_b — each "host" has its own cursors.
    giga(&fx.home_b, &[
        "merger",
        "--once",
        "--config",
        fx.host_b_config().to_str().unwrap(),
    ]);

    let merged_b = fx.host_b_inbox.join("alice-bob.md");
    let body = fs::read_to_string(&merged_b).unwrap();
    assert!(
        body.contains("[alice] ping"),
        "bob's merged view should contain alice's post: {body}"
    );

    // 4) bob replies on host-b. Writes to bob's slice (wsl-b).
    giga(&fx.home_b, &[
        "post",
        "alice-bob",
        "--as",
        "bob",
        "--subject",
        "pong",
        "--body",
        "pong from bob",
        "--config",
        fx.host_b_config().to_str().unwrap(),
    ]);

    let bob_slice = fx.host_b_inbox.join("alice-bob.wsl-b.md");
    assert!(bob_slice.exists());

    // 5) Reverse sync: bob's slice -> alice's inbox.
    fake_sync_slice(&fx.host_b_inbox, &fx.host_a_inbox, "alice-bob.wsl-b.md");

    // 6) host-a's merger picks up bob's incoming slice + appends.
    giga(&fx.home_a, &[
        "merger",
        "--once",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);

    let merged_a = fs::read_to_string(fx.host_a_inbox.join("alice-bob.md")).unwrap();
    assert!(merged_a.contains("[alice] ping"));
    assert!(merged_a.contains("[bob] pong"));
}

#[test]
fn merger_idempotent_on_repeated_runs() {
    let fx = build_fixture();

    giga(&fx.home_a, &[
        "post",
        "alice-bob",
        "--as",
        "alice",
        "--subject",
        "once",
        "--body",
        "x",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);

    // Three consecutive merger runs — the message should appear exactly
    // once in the merged file, not three times.
    for _ in 0..3 {
        giga(&fx.home_a, &[
            "merger",
            "--once",
            "--config",
            fx.host_a_config().to_str().unwrap(),
        ]);
    }

    let merged = fs::read_to_string(fx.host_a_inbox.join("alice-bob.md")).unwrap();
    assert_eq!(
        merged.matches("[alice] once").count(),
        1,
        "no re-delivery on repeated merger runs"
    );
}

#[test]
fn incremental_slice_growth_appears_in_merged_on_next_tick() {
    let fx = build_fixture();

    giga(&fx.home_a, &[
        "post",
        "alice-bob",
        "--as",
        "alice",
        "--subject",
        "first",
        "--body",
        "1",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);
    giga(&fx.home_a, &[
        "merger",
        "--once",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);

    // Second post (slice file grows).
    giga(&fx.home_a, &[
        "post",
        "alice-bob",
        "--as",
        "alice",
        "--subject",
        "second",
        "--body",
        "2",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);
    giga(&fx.home_a, &[
        "merger",
        "--once",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);

    let merged = fs::read_to_string(fx.host_a_inbox.join("alice-bob.md")).unwrap();
    assert!(merged.contains("[alice] first"));
    assert!(merged.contains("[alice] second"));
}

#[test]
fn sync_dry_run_prints_expected_plan() {
    let fx = build_fixture();
    // Make sure both slice files exist so sync has something to plan.
    fs::write(fx.host_a_inbox.join("alice-bob.wsl-a.md"), b"").unwrap();

    let out = giga(&fx.home_a, &[
        "sync",
        "--once",
        "--dry-run",
        "--config",
        fx.host_a_config().to_str().unwrap(),
    ]);
    // sync logs to stderr (dry-run lines).
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Expect: one TOML push (to wsl-b) + one slice push (alice-bob.wsl-a.md to wsl-b).
    assert!(
        stderr.contains("toml") && stderr.contains("wsl-b.tail0.ts.net"),
        "expected TOML push to wsl-b in: {stderr}"
    );
    assert!(
        stderr.contains("slice") && stderr.contains("alice-bob.wsl-a.md"),
        "expected slice push of alice-bob.wsl-a.md in: {stderr}"
    );
    // Should NOT push our own host as a target.
    assert!(
        !stderr.contains("wsl-a.tail0.ts.net"),
        "should never push to own host: {stderr}"
    );
}

#[test]
fn local_only_swarm_falls_back_to_direct_write() {
    // A legacy swarm (no [[hosts]]) — post writes directly to the merged
    // file, no slice files, merger is a no-op.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let swarm_dir = tmp.path().join("swarm");
    let inbox = tmp.path().join("inbox");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&swarm_dir).unwrap();
    fs::create_dir_all(&inbox).unwrap();
    let config_path = swarm_dir.join("giga-harness.toml");
    let toml = format!(
        r#"
[project]
name = "legacy"

[paths]
wsl_inbox = '{inbox}'

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
        inbox = inbox.to_string_lossy(),
    );
    fs::write(&config_path, toml).unwrap();

    giga(&home, &[
        "post",
        "alice-bob",
        "--as",
        "alice",
        "--subject",
        "hi",
        "--body",
        "x",
        "--config",
        config_path.to_str().unwrap(),
    ]);

    let merged = inbox.join("alice-bob.md");
    assert!(merged.exists(), "legacy local-only swarm writes directly to merged");
    let body = fs::read_to_string(&merged).unwrap();
    assert!(body.contains("[alice] hi"));

    // Verify no slice file was created.
    for entry in fs::read_dir(&inbox).unwrap() {
        let name = entry.unwrap().file_name();
        let name_str = name.to_string_lossy();
        assert!(
            name_str == "alice-bob.md",
            "unexpected file in legacy inbox: {name_str}"
        );
    }
}
