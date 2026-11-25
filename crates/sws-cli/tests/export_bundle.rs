//! Integration test for GitHub issue #4: the `solo export` bundle must not
//! just exist, its emitted worker crate must compile and its embedded
//! `#[cfg(test)] mod tests` must pass under `cargo test`.
//!
//! Network-dependent (resolves serde_json from crates.io on first run), so it
//! is `#[ignore]`-by-default and runs explicitly via CI's dedicated
//! `export-bundle` job or:
//!
//! ```sh
//! cargo test -p sws-cli --features pack-hospitality --locked -- --ignored export
//! ```

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique, lock-scoped tempdir: process id + nanos so parallel CI runs and
/// nested cargo invocations never collide. Removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("sws-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn assert_command_ok(command: &Command, output: &Output) {
    assert!(
        output.status.success(),
        "`{command:?}` failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Runs `solo init` + `solo export` via the built CLI binary and returns the
/// emitted bundle directory.
fn export_bundle(dir: &TempDir) -> PathBuf {
    let solo = env!("CARGO_BIN_EXE_solo");
    let src_dir = dir.path().join("src-site");
    let out_dir = dir.path().join("dist");

    let mut cmd = Command::new(solo);
    cmd.arg("init").arg(&src_dir);
    let output = cmd.output().expect("spawn solo init");
    assert_command_ok(&cmd, &output);

    let ir_path = src_dir.join("site.ir.json");
    assert!(ir_path.exists(), "solo init must emit site.ir.json");

    let mut cmd = Command::new(solo);
    cmd.arg("export").arg(&ir_path).arg(&out_dir);
    let output = cmd.output().expect("spawn solo export");
    assert_command_ok(&cmd, &output);

    assert!(
        out_dir.join("Cargo.toml").exists(),
        "export must emit Cargo.toml"
    );
    assert!(
        out_dir.join("src/lib.rs").exists(),
        "export must emit src/lib.rs"
    );
    out_dir
}

#[test]
#[ignore = "network-dependent: the emitted crate resolves serde_json from crates.io"]
fn export_bundle_emitted_crate_tests_pass() {
    let dir = TempDir::new("export-bundle");
    let out_dir = export_bundle(&dir);

    // The exported crate does not ship a Cargo.lock (the wrangler/worker-build
    // flow generates it on first build), so resolve the lock first, then run
    // the embedded test module against it with --locked.
    let mut cmd = Command::new("cargo");
    cmd.arg("generate-lockfile").current_dir(&out_dir);
    let output = cmd
        .output()
        .expect("spawn cargo generate-lockfile in the exported crate");
    assert_command_ok(&cmd, &output);

    // Execute the exported crate's own embedded test module.
    let mut cmd = Command::new("cargo");
    cmd.arg("test").arg("--locked").current_dir(&out_dir);
    let output = cmd.output().expect("spawn cargo test in the exported crate");
    assert_command_ok(&cmd, &output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("test result: ok"),
        "exported crate test output must report ok:\n{stdout}"
    );
    assert!(
        stdout.contains("routes_and_validation_work"),
        "the exported crate's embedded routes_and_validation_work test must run:\n{stdout}"
    );
}
