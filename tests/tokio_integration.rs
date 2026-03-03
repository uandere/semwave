//! Integration test: run semwave against the tokio workspace.
//!
//! This test clones a pinned commit of tokio and verifies that
//! `semwave --direct pin-project-lite` produces the expected output.
//! The tests pass `RUSTFLAGS` and `RUSTDOCFLAGS` with `--cfg tokio_unstable`
//! so that tokio's full API (including tracing instrumentation) compiles.
//!
//! Skipped by default (`#[ignore]`). Run with:
//!   cargo test -- --ignored
//!
//! Requires: nightly toolchain installed (`rustup toolchain install nightly`).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

const TOKIO_REPO: &str = "https://github.com/tokio-rs/tokio.git";
const TOKIO_COMMIT: &str = "8c980ea75a0f8dd2799403777db700c2e8f4cda4";

fn tokio_dir() -> PathBuf {
    std::env::temp_dir().join("semwave-test-tokio")
}

fn ensure_tokio_cloned() {
    static SETUP: OnceLock<()> = OnceLock::new();
    SETUP.get_or_init(|| {
        let dir = tokio_dir();

        if dir.join(".git").exists() {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&dir)
                .output()
                .expect("git rev-parse failed");
            let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if head == TOKIO_COMMIT {
                return;
            }
            std::fs::remove_dir_all(&dir).expect("failed to remove stale clone");
        }

        std::fs::create_dir_all(&dir).expect("failed to create temp dir");

        let out = Command::new("git")
            .args(["init"])
            .current_dir(&dir)
            .output()
            .expect("git init failed");
        assert!(out.status.success(), "git init failed");

        let out = Command::new("git")
            .args(["fetch", "--depth", "1", TOKIO_REPO, TOKIO_COMMIT])
            .current_dir(&dir)
            .output()
            .expect("git fetch failed");
        assert!(
            out.status.success(),
            "git fetch failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let out = Command::new("git")
            .args(["checkout", "FETCH_HEAD"])
            .current_dir(&dir)
            .output()
            .expect("git checkout failed");
        assert!(
            out.status.success(),
            "git checkout failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    });
}

/// Serialize all semwave invocations so concurrent `cargo rustdoc` calls
/// don't corrupt each other's JSON output in the shared target directory.
static RUN_LOCK: Mutex<()> = Mutex::new(());

fn run_semwave(extra_args: &[&str]) -> (String, String, bool) {
    let _guard = RUN_LOCK.lock().unwrap();
    ensure_tokio_cloned();

    let mut args = vec!["--direct", "pin-project-lite", "--no-color"];
    args.extend_from_slice(extra_args);

    let output = Command::new(env!("CARGO_BIN_EXE_semwave"))
        .args(&args)
        .env("RUSTFLAGS", "--cfg tokio_unstable")
        .env("RUSTDOCFLAGS", "--cfg tokio_unstable")
        .current_dir(tokio_dir())
        .output()
        .expect("failed to run semwave");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

/// At commit 8c980ea7 of tokio (with `--cfg tokio_unstable`), `--direct pin-project-lite` produces:
///
///   MAJOR: {"tokio"}              (tokio >= 1.0.0, leaks pin-project-lite → Major)
///   MINOR: {"tests-build", "tokio-stream", "tokio-test", "tokio-util"}
///   PATCH: {}                     (binary-only crates are skipped by default)
///
/// Exit code is 0 (direct mode, no local_bumps to compare against).
#[test]
#[ignore]
fn tokio_direct_pin_project_lite() {
    let (stdout, stderr, success) = run_semwave(&[]);

    assert!(
        success,
        "semwave exited with non-zero status.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    assert!(
        stdout.contains("Direct mode:"),
        "should show direct mode header.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("pin-project-lite"),
        "should mention the seed crate.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("=== Analysis Complete ==="),
        "should reach analysis completion.\nstdout:\n{stdout}"
    );

    let major_line = stdout
        .lines()
        .find(|l| l.contains("MAJOR-bump list"))
        .expect("MAJOR-bump line not found");
    let minor_line = stdout
        .lines()
        .find(|l| l.contains("MINOR-bump list"))
        .expect("MINOR-bump line not found");
    let patch_line = stdout
        .lines()
        .find(|l| l.contains("PATCH-bump list"))
        .expect("PATCH-bump line not found");

    // tokio >= 1.0.0 leaks pin-project-lite → needs MAJOR bump.
    assert!(
        major_line.contains("tokio"),
        "tokio should require a MAJOR bump.\nMAJOR line: {major_line}\nfull stdout:\n{stdout}"
    );

    // tokio-stream and tokio-util are 0.y.z and leak pin-project-lite → MINOR.
    assert!(
        minor_line.contains("tokio-stream"),
        "tokio-stream should require MINOR.\nMINOR line: {minor_line}\nfull stdout:\n{stdout}"
    );
    assert!(
        minor_line.contains("tokio-util"),
        "tokio-util should require MINOR.\nMINOR line: {minor_line}\nfull stdout:\n{stdout}"
    );

    // Binary-only crates are skipped by default.
    assert!(
        !patch_line.contains("benches") && !patch_line.contains("stress-test"),
        "binary-only crates should NOT appear when --include-binaries is off.\nPATCH line: {patch_line}\nfull stdout:\n{stdout}"
    );
}

#[test]
#[ignore]
fn tokio_direct_pin_project_lite_with_tree() {
    let (stdout, stderr, success) = run_semwave(&["--tree"]);

    assert!(
        success,
        "semwave exited with non-zero status.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    assert!(
        stdout.contains("Influence Tree"),
        "should print influence tree.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("pin-project-lite (seed)"),
        "tree should show pin-project-lite as seed.\nstdout:\n{stdout}"
    );
}

#[test]
#[ignore]
fn tokio_direct_pin_project_lite_verbose() {
    let (stdout, stderr, success) = run_semwave(&["--verbose"]);

    assert!(
        success,
        "semwave exited with non-zero status.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Verbose mode should show at least some "uses <type>" detail lines.
    assert!(
        stdout.contains(" — uses "),
        "verbose output should contain ' — uses ' leak detail lines.\nstdout:\n{stdout}"
    );
}
