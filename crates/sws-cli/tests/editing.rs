//! Editing contract tests (issue #55): the read-only guarantee for legacy
//! inputs, deterministic explicit upgrade, lossless-save refusal and the pure
//! Rust half of the editing gate (the Wasm half is `npm run gate:editing`).
//!
//! Everything uses fictional fixtures only. The demo-based tests need the
//! hospitality pack (`--features pack-hospitality`) to lower `venue.json`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "solo-editing-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn solo(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_solo"))
        .args(args)
        .output()
        .expect("run solo")
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "solo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn p(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

/// (relative path -> bytes) for every file under `dir`.
fn file_map(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .expect("under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(&path).expect("read file"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Bytes, mtime and the sibling listing of `file`.
fn fingerprint(file: &Path) -> (Vec<u8>, SystemTime, Vec<String>) {
    let bytes = fs::read(file).expect("read fixture");
    let mtime = fs::metadata(file)
        .and_then(|m| m.modified())
        .expect("mtime");
    let mut listing: Vec<String> = fs::read_dir(file.parent().expect("parent"))
        .expect("list dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    listing.sort();
    (bytes, mtime, listing)
}

/// Starter IR plus unknown fields and a duplicate legacy HTML id: a valid
/// v0.1.2 document carrying exactly the shapes the audit lists as hazards.
fn legacy_fixtures(dir: &Path) -> Vec<PathBuf> {
    let init_dir = dir.join("init");
    ok(&solo(&[Path::new("init"), &init_dir]));
    let starter = init_dir.join("site.ir.json");
    assert!(starter.exists());

    let mut doc: Value =
        serde_json::from_str(&fs::read_to_string(&starter).expect("read starter")).expect("json");
    doc["x_ext"] = json!({ "vendor": "fictional" });
    doc["pages"][0]["future"] = json!(true);
    if let Some(first) = doc["pages"][0]["body"].get_mut(0) {
        first["unknown"] = json!("kept-out-of-typed-model");
    }
    let hazards = dir.join("hazards.ir.json");
    fs::write(&hazards, serde_json::to_string_pretty(&doc).expect("ser")).expect("write");
    vec![starter, hazards]
}

#[test]
fn legacy_build_check_export_never_write_the_input_or_emit_uid() {
    let dir = TempDir::new("readonly");
    let mut fixtures = legacy_fixtures(dir.path());
    if cfg!(feature = "pack-hospitality") {
        let venue = dir.path().join("venue.json");
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../packs/hospitality/demo/venue.json"
            ),
            &venue,
        )
        .expect("copy demo");
        fixtures.push(venue);
    }
    let outs = dir.path().join("outputs");
    fs::create_dir_all(&outs).expect("outputs dir");
    for (n, fixture) in fixtures.iter().enumerate() {
        let before = fingerprint(fixture);
        let out = outs.join(format!("out-{n}"));
        let exp = outs.join(format!("export-{n}"));
        let built = solo(&[Path::new("build"), fixture, &out]);
        assert!(
            built.status.success(),
            "{}: {}",
            fixture.display(),
            String::from_utf8_lossy(&built.stderr)
        );
        ok(&solo(&[
            Path::new("build"),
            fixture,
            &out,
            Path::new("--check"),
        ]));
        // `check` may report advisory findings; only the no-write contract matters.
        let _ = solo(&[Path::new("check"), fixture]);
        let json_report = solo(&[Path::new("check"), fixture, Path::new("--json")]);
        let report = String::from_utf8_lossy(&json_report.stdout).into_owned();
        assert!(!report.contains("\"uid\""), "check emitted uid");
        ok(&solo(&[Path::new("export"), fixture, &exp]));
        assert_eq!(
            fingerprint(fixture),
            before,
            "{} changed after build/check/export",
            fixture.display()
        );
        for (name, bytes) in file_map(&out) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains("\"uid\""),
                "{name} contains uid"
            );
        }
    }
}

#[test]
fn mcp_read_only_tools_never_write_the_input_or_emit_uid_for_legacy_ir() {
    let dir = TempDir::new("mcp");
    let fixtures = legacy_fixtures(dir.path());
    let target = &fixtures[0];
    let before = fingerprint(target);
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(target).expect("read")).expect("json");
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_site_ir",
            "arguments":{"path":"init/site.ir.json"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"compile_preview",
            "arguments":{"ir_json": doc.clone()}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"run_a11y_and_plugin_checks",
            "arguments":{"input_json": doc.clone()}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"apply_ir_patch",
            "arguments":{"ir_json": doc, "patch_json": {"op":"set_site_meta","name":"Patched"}}}}),
    ];
    let mut child = Command::new(env!("CARGO_BIN_EXE_solo"))
        .args(["mcp", "--root", p(dir.path())])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for request in &requests {
            writeln!(stdin, "{request}").expect("write request");
        }
    }
    let output = child.wait_with_output().expect("mcp output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), requests.len(), "{stdout}");
    assert!(
        !stdout.contains("uid"),
        "legacy MCP output must not contain uid"
    );
    assert_eq!(fingerprint(target), before);
}

#[test]
fn upgrade_is_explicit_deterministic_idempotent_and_does_not_change_output() {
    let dir = TempDir::new("upgrade");
    let fixtures = legacy_fixtures(dir.path());
    // `hazards.ir.json` carries unknown fields, so use the clean starter here.
    let legacy = &fixtures[0];
    let before = fingerprint(legacy);
    let (a, b, c) = (
        dir.path().join("a.json"),
        dir.path().join("b.json"),
        dir.path().join("c.json"),
    );
    for out in [&a, &b] {
        ok(&solo(&[
            Path::new("edit"),
            Path::new("upgrade"),
            legacy,
            Path::new("--out"),
            out,
        ]));
    }
    assert_eq!(
        fs::read(&a).unwrap(),
        fs::read(&b).unwrap(),
        "two processes differ"
    );
    assert_eq!(fingerprint(legacy), before, "upgrade wrote its input");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        &a,
        Path::new("--out"),
        &c,
    ]));
    assert_eq!(
        fs::read(&a).unwrap(),
        fs::read(&c).unwrap(),
        "re-upgrade is not a no-op"
    );

    let (legacy_out, upgraded_out) = (
        dir.path().join("legacy-out"),
        dir.path().join("upgraded-out"),
    );
    ok(&solo(&[Path::new("build"), legacy, &legacy_out]));
    ok(&solo(&[Path::new("build"), &a, &upgraded_out]));
    assert_eq!(
        file_map(&legacy_out),
        file_map(&upgraded_out),
        "uid changed compiled output"
    );
}

#[test]
fn lossy_sources_are_refused_and_the_target_is_left_unchanged() {
    let dir = TempDir::new("lossy");
    let fixtures = legacy_fixtures(dir.path());
    let hazards = &fixtures[1];
    let before = fingerprint(hazards);
    let out = dir.path().join("never.json");

    let refused = solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        hazards,
        Path::new("--out"),
        &out,
    ]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("lossy_document"), "{stderr}");
    assert!(stderr.contains("$.x_ext"), "{stderr}");
    assert!(!out.exists(), "refused save must not create the output");

    // Same refusal when the caller targets the input itself: bytes stay put.
    let in_place = solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        hazards,
        Path::new("--out"),
        hazards,
    ]);
    assert!(!in_place.status.success());
    assert_eq!(fingerprint(hazards), before);

    // The explicit, separate confirmation is the only way through.
    let allowed = solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        hazards,
        Path::new("--out"),
        &out,
        Path::new("--allow-lossy"),
    ]);
    let stdout = ok(&allowed);
    assert!(stdout.contains("dropped unsupported fields"), "{stdout}");
    assert!(out.exists());
}

#[test]
fn apply_refuses_legacy_documents_and_eject_refuses_site_ir() {
    let dir = TempDir::new("refuse");
    let fixtures = legacy_fixtures(dir.path());
    let batch = dir.path().join("batch.json");
    fs::write(
        &batch,
        r#"{"version":1,"commands":[{"op":"set_data_value","key":"k","value":1}]}"#,
    )
    .unwrap();
    let out = dir.path().join("out.json");
    let refused = solo(&[
        Path::new("edit"),
        Path::new("apply"),
        &fixtures[0],
        &batch,
        Path::new("--out"),
        &out,
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not_upgraded"));
    assert!(!out.exists());

    let eject = solo(&[
        Path::new("edit"),
        Path::new("eject"),
        &fixtures[0],
        Path::new("--out"),
        &out,
    ]);
    assert!(!eject.status.success());
    assert!(String::from_utf8_lossy(&eject.stderr).contains("already site IR"));
}

#[test]
fn apply_reports_typed_errors_and_writes_nothing_on_failure() {
    let dir = TempDir::new("atomic");
    let fixtures = legacy_fixtures(dir.path());
    let upgraded = dir.path().join("up.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        &fixtures[0],
        Path::new("--out"),
        &upgraded,
    ]));
    let batch = dir.path().join("bad.json");
    fs::write(
        &batch,
        r##"{"version":1,"commands":[
            {"op":"set_data_value","key":"k","value":1},
            {"op":"remove_node","uid":"no.such.node"}]}"##,
    )
    .unwrap();
    let out = dir.path().join("out.json");
    let before = fingerprint(&upgraded);
    // Even targeting the input in place, a failing batch must leave it untouched.
    for target in [&out, &upgraded] {
        let failed = solo(&[
            Path::new("edit"),
            Path::new("apply"),
            &upgraded,
            &batch,
            Path::new("--out"),
            target,
        ]);
        assert!(!failed.status.success());
        let stderr = String::from_utf8_lossy(&failed.stderr);
        assert!(
            stderr.contains("not_found") && stderr.contains("(command 1)"),
            "{stderr}"
        );
    }
    assert!(!out.exists());
    assert_eq!(fingerprint(&upgraded), before);
}

#[test]
fn a_failing_sidecar_write_leaves_no_new_output_behind() {
    let dir = TempDir::new("sidecar");
    let fixtures = legacy_fixtures(dir.path());
    let upgraded = dir.path().join("up.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        &fixtures[0],
        Path::new("--out"),
        &upgraded,
    ]));
    let batch = dir.path().join("batch.json");
    fs::write(
        &batch,
        r#"{"version":1,"commands":[{"op":"set_data_value","key":"k","value":1}]}"#,
    )
    .unwrap();
    let out = dir.path().join("out.json");
    let missing = dir.path().join("no-such-dir").join("tx.json");
    let failed = solo(&[
        Path::new("edit"),
        Path::new("apply"),
        &upgraded,
        &batch,
        Path::new("--out"),
        &out,
        Path::new("--transaction"),
        &missing,
    ]);
    assert!(!failed.status.success());
    assert!(
        !out.exists(),
        "--out must not exist without its transaction"
    );
    let leftovers: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("solo-tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );

    // Success writes both, atomically, with no temp files left.
    let tx = dir.path().join("tx.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("apply"),
        &upgraded,
        &batch,
        Path::new("--out"),
        &out,
        Path::new("--transaction"),
        &tx,
    ]));
    assert!(out.exists() && tx.exists());
    assert!(
        !fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("solo-tmp"))
    );
}

/// Upgraded legacy fixture plus a one-command batch, for writer tests.
fn writer_inputs(dir: &Path) -> (PathBuf, PathBuf) {
    let fixtures = legacy_fixtures(dir);
    let upgraded = dir.join("up.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        &fixtures[0],
        Path::new("--out"),
        &upgraded,
    ]));
    let batch = dir.join("batch.json");
    fs::write(
        &batch,
        r#"{"version":1,"commands":[{"op":"set_data_value","key":"k","value":1}]}"#,
    )
    .unwrap();
    (upgraded, batch)
}

fn apply_with_tx(upgraded: &Path, batch: &Path, out: &Path, tx: &Path) -> Output {
    solo(&[
        Path::new("edit"),
        Path::new("apply"),
        upgraded,
        batch,
        Path::new("--out"),
        out,
        Path::new("--transaction"),
        tx,
    ])
}

fn solo_tmp_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("solo-tmp"))
        .collect()
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn read_only_targets_are_refused_and_left_unchanged() {
    let dir = TempDir::new("readonly");
    let (upgraded, batch) = writer_inputs(dir.path());
    let out = dir.path().join("out.json");
    let tx = dir.path().join("tx.json");
    fs::write(&tx, "keep").unwrap();
    set_mode(&tx, 0o444);
    let refused = apply_with_tx(&upgraded, &batch, &out, &tx);
    set_mode(&tx, 0o644);
    if is_root() {
        return; // root ignores mode bits; nothing meaningful to assert
    }
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("read-only"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(fs::read_to_string(&tx).unwrap(), "keep");
    assert!(!out.exists(), "--out must not be written");
    assert!(solo_tmp_files(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn special_file_targets_are_written_directly() {
    let dir = TempDir::new("special");
    let (upgraded, batch) = writer_inputs(dir.path());
    let out = dir.path().join("out.json");
    ok(&apply_with_tx(
        &upgraded,
        &batch,
        &out,
        Path::new("/dev/null"),
    ));
    assert!(out.exists());
    assert!(Path::new("/dev/null").exists(), "device must survive");
    assert!(
        !fs::metadata("/dev/null").unwrap().is_file(),
        "/dev/null must not be replaced by a regular file"
    );
    assert!(solo_tmp_files(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn writable_file_in_a_read_only_directory_falls_back_to_a_direct_write() {
    let dir = TempDir::new("rodir");
    let (upgraded, batch) = writer_inputs(dir.path());
    let out = dir.path().join("out.json");
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    let tx = locked.join("tx.json");
    fs::write(&tx, "old").unwrap();
    set_mode(&locked, 0o555);
    let result = apply_with_tx(&upgraded, &batch, &out, &tx);
    set_mode(&locked, 0o755);
    if is_root() {
        return;
    }
    ok(&result);
    assert_ne!(fs::read_to_string(&tx).unwrap(), "old");
    assert!(solo_tmp_files(&locked).is_empty());
}

#[cfg(unix)]
#[test]
fn atomic_write_preserves_destination_permissions() {
    let dir = TempDir::new("perms");
    let (upgraded, batch) = writer_inputs(dir.path());
    let out = dir.path().join("out.json");
    let tx = dir.path().join("tx.json");
    fs::write(&tx, "old").unwrap();
    fs::write(&out, "old").unwrap();
    set_mode(&tx, 0o640);
    set_mode(&out, 0o600);
    ok(&apply_with_tx(&upgraded, &batch, &out, &tx));
    assert_eq!(mode_of(&tx), 0o640);
    assert_eq!(mode_of(&out), 0o600);
    assert_ne!(fs::read_to_string(&tx).unwrap(), "old");
    assert!(solo_tmp_files(dir.path()).is_empty());
}

#[cfg(feature = "pack-hospitality")]
#[test]
fn eject_never_writes_over_a_venue_manifest() {
    let dir = TempDir::new("eject-guard");
    let venue = dir.path().join("venue.json");
    fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packs/hospitality/demo/venue.json"
        ),
        &venue,
    )
    .expect("copy venue");
    let before = fs::read(&venue).unwrap();

    let link = dir.path().join("link.json");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&venue, &link).unwrap();
    let hard = dir.path().join("hard.json");
    fs::hard_link(&venue, &hard).unwrap();
    let dotted = dir.path().join(".").join("venue.json");
    let mut targets = vec![venue.clone(), dotted, hard];
    if cfg!(unix) {
        targets.push(link);
    }
    for target in &targets {
        let refused = solo(&[
            Path::new("edit"),
            Path::new("eject"),
            &venue,
            Path::new("--out"),
            target,
        ]);
        assert!(!refused.status.success(), "{}", target.display());
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.contains("refusing to write"), "{stderr}");
        assert_eq!(fs::read(&venue).unwrap(), before, "{}", target.display());
    }

    // Any other file that is a venue manifest is protected as well.
    let other = dir.path().join("other-venue.json");
    fs::write(&other, &before).unwrap();
    let refused = solo(&[
        Path::new("edit"),
        Path::new("eject"),
        &venue,
        Path::new("--out"),
        &other,
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("pack manifest"));
    assert_eq!(fs::read(&other).unwrap(), before);

    // A fresh path works, and the original is still untouched.
    let ir = dir.path().join("ir.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("eject"),
        &venue,
        Path::new("--out"),
        &ir,
    ]));
    assert!(ir.exists());
    assert_eq!(fs::read(&venue).unwrap(), before);
}

#[cfg(feature = "pack-hospitality")]
#[test]
fn editing_gate_pure_rust_edit_undo_redo_save_reopen_compile() {
    use sws_compiler::{CompileOptions, compile_site};
    use sws_ir::SiteDocument;
    use sws_ir::edit::{
        Command, CommandBatch, ContainerRef, EditOptions, History, canonical_json, upgrade_json,
    };

    let dir = TempDir::new("gate");
    let venue = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packs/hospitality/demo/venue.json"
    ));
    let venue_before = fs::read(venue).expect("read venue");

    // Load the legacy demo: one-way eject, then the explicit upgrade.
    let legacy_path = dir.path().join("legacy.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("eject"),
        venue,
        Path::new("--out"),
        &legacy_path,
    ]));
    let legacy = fs::read_to_string(&legacy_path).expect("legacy");
    let base_text = upgrade_json(&legacy, EditOptions::default())
        .expect("upgrade")
        .document;
    let base: SiteDocument = serde_json::from_str(&base_text).expect("parse upgraded");

    // Locate a section on the index page and its parent.
    let tree: Value = serde_json::from_str(&base_text).expect("json");
    fn find_section(nodes: &Value, parent: Option<&Value>) -> Option<(String, String, usize)> {
        for node in nodes.as_array()? {
            if let Some(parent) = parent
                && node["type"] == "element"
                && node["tag"] == "section"
            {
                return Some((
                    node["uid"].as_str()?.to_string(),
                    parent["uid"].as_str()?.to_string(),
                    parent["children"].as_array()?.len(),
                ));
            }
            if let Some(found) = find_section(&node["children"], Some(node)) {
                return Some(found);
            }
        }
        None
    }
    let index = tree["pages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pg| pg["id"] == "index")
        .expect("index page");
    let (section, parent, siblings) = find_section(&index["body"], None).expect("a section");

    let mut history = History::new(base.clone()).expect("editable");
    let token = CommandBatch::new(vec![Command::SetColorToken {
        key: "accent".to_string(),
        hex: "#701B27".to_string(),
    }]);
    let mv = CommandBatch::new(vec![Command::MoveNode {
        uid: section,
        container: ContainerRef::Node {
            uid: parent,
            slot: Some("children".to_string()),
        },
        index: Some(siblings - 1),
    }]);
    let s0 = canonical_json(history.document());
    history.apply(&token).expect("token");
    let s1 = canonical_json(history.document());
    history.apply(&mv).expect("move");
    let s2 = canonical_json(history.document());
    assert!(s0 != s1 && s1 != s2);

    history.undo().unwrap().expect("undo move");
    assert_eq!(canonical_json(history.document()), s1);
    history.undo().unwrap().expect("undo token");
    assert_eq!(canonical_json(history.document()), s0);
    history.redo().unwrap().expect("redo token");
    assert_eq!(canonical_json(history.document()), s1);
    history.redo().unwrap().expect("redo move");
    assert_eq!(canonical_json(history.document()), s2);

    // Save, reopen: byte-stable, still editable, nothing dropped.
    let saved_path = dir.path().join("saved.json");
    fs::write(&saved_path, &s2).expect("save");
    let reopened_text = fs::read_to_string(&saved_path).expect("reopen");
    let reopened = upgrade_json(&reopened_text, EditOptions::default()).expect("reopen upgrade");
    assert_eq!(reopened.document, s2);
    assert!(reopened.report.assigned.is_empty());
    let reopened_doc: SiteDocument = serde_json::from_str(&reopened_text).unwrap();
    History::new(reopened_doc.clone()).expect("reopened document is editable");

    // Same saved IR: native CLI build == library compile (the code path Wasm uses).
    let out = dir.path().join("out");
    ok(&solo(&[Path::new("build"), &saved_path, &out]));
    let native = file_map(&out);
    let options = CompileOptions {
        include_headers_file: true,
        ..CompileOptions::default()
    };
    let lib = compile_site(&reopened_doc, None, &options).expect("compile saved IR");
    let lib_files: BTreeMap<String, Vec<u8>> = lib
        .files
        .into_iter()
        .map(|(k, v)| (k, v.into_bytes()))
        .collect();
    assert_eq!(
        native, lib_files,
        "CLI build and library compile differ for the same saved IR"
    );

    // The edits are real and the source manifest was never touched.
    let base_out = dir.path().join("base-out");
    let base_path = dir.path().join("base.json");
    fs::write(&base_path, &base_text).unwrap();
    ok(&solo(&[Path::new("build"), &base_path, &base_out]));
    assert_ne!(file_map(&base_out)["index.html"], native["index.html"]);
    assert_ne!(file_map(&base_out)["styles.css"], native["styles.css"]);
    assert_eq!(fs::read(venue).unwrap(), venue_before);
}

#[test]
fn undo_restores_an_empty_font_stack_and_refuses_a_forged_transaction() {
    let dir = TempDir::new("undo");
    let fixtures = legacy_fixtures(dir.path());
    let upgraded = dir.path().join("up.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        &fixtures[0],
        Path::new("--out"),
        &upgraded,
    ]));
    // A document holding a legacy-shaped empty heading font stack.
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(&upgraded).unwrap()).unwrap();
    doc["tokens"]["fonts"]["heading"] = json!("");
    let loose = dir.path().join("loose.json");
    fs::write(&loose, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
    // Re-save canonically so byte comparison below is meaningful.
    let base = dir.path().join("base.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("upgrade"),
        &loose,
        Path::new("--out"),
        &base,
    ]));

    let batch = dir.path().join("batch.json");
    fs::write(
        &batch,
        r#"{"version":1,"commands":[{"op":"set_font_token","slot":"heading","stack":"Georgia"}]}"#,
    )
    .unwrap();
    let edited = dir.path().join("edited.json");
    let tx = dir.path().join("tx.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("apply"),
        &base,
        &batch,
        Path::new("--out"),
        &edited,
        Path::new("--transaction"),
        &tx,
    ]));

    let restored = dir.path().join("restored.json");
    let undo_tx = dir.path().join("undo-tx.json");
    ok(&solo(&[
        Path::new("edit"),
        Path::new("undo"),
        &edited,
        &tx,
        Path::new("--out"),
        &restored,
        Path::new("--undo-transaction"),
        &undo_tx,
    ]));
    assert_eq!(
        fs::read(&restored).unwrap(),
        fs::read(&base).unwrap(),
        "undo is not byte-exact"
    );
    assert!(undo_tx.exists());

    // A forged transaction (inverse writes a field the forward edit never did)
    // is refused and writes nothing.
    let mut forged: Value = serde_json::from_str(&fs::read_to_string(&tx).unwrap()).unwrap();
    forged["inverse"]
        .as_array_mut()
        .unwrap()
        .push(json!({"op":"set_font_token","slot":"mono","stack":" "}));
    let forged_path = dir.path().join("forged.json");
    fs::write(&forged_path, forged.to_string()).unwrap();
    let nowhere = dir.path().join("nowhere.json");
    let failed = solo(&[
        Path::new("edit"),
        Path::new("undo"),
        &edited,
        &forged_path,
        Path::new("--out"),
        &nowhere,
    ]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("invalid_value"));
    assert!(!nowhere.exists());
}
