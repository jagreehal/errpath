//! Runs the real binary against a real git repository.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn review(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-errpath"))
        .arg("errpath")
        .arg("review")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn commit(dir: &Path, file: &str, src: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, src).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "c"]);
}

#[test]
fn only_the_branch_is_reviewed_when_the_base_has_moved_on() {
    let dir = std::env::temp_dir().join(format!("errpath-moved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    commit(&dir, "src/lib.rs", "fn f() {}\n");
    git(&dir, &["checkout", "-qb", "feature"]);
    commit(&dir, "src/feature.rs", "fn g() { y().unwrap(); }\n");
    git(&dir, &["checkout", "-q", "main"]);
    // Only on main, and nothing the branch did.
    commit(&dir, "src/lib.rs", "fn f() { x().unwrap(); }\n");
    git(&dir, &["checkout", "-q", "feature"]);

    let out = review(&dir, &["--base", "main", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // Against main's tip, `f` would read as the branch removing main's `unwrap`.
    let functions: Vec<_> = v["functions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(functions, ["feature::g"], "{v}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hostile_tree_entries_do_not_abort_the_review() {
    let dir = std::env::temp_dir().join(format!("errpath-hostile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(&dir, &["init", "-q"]);
    commit(&dir, "src/lib.rs", "fn f() {}\n");
    git(&dir, &["tag", "base"]);
    std::fs::write(dir.join("src/binary.rs"), [0xff, 0xfe, 0x00]).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.join("src/link.rs")).unwrap();
    // A submodule entry whose name ends in .rs.
    git(&dir, &["add", "."]);
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    git(
        &dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},src/module.rs", head.trim()),
        ],
    );
    // Valid to git but not to a String, and refused by some filesystems, so
    // it goes straight into the index.
    std::fs::write(dir.join("blob"), "fn g() { x().unwrap(); }\n").unwrap();
    let oid = String::from_utf8(
        Command::new("git")
            .args(["hash-object", "-w", "blob"])
            .current_dir(&dir)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    std::fs::remove_file(dir.join("blob")).unwrap();
    let mut entry = format!("100644 blob {}\tsrc/bad", oid.trim()).into_bytes();
    entry.extend(b"\xff.rs\0");
    let mut child = Command::new("git")
        .args(["update-index", "-z", "--index-info"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&entry).unwrap();
    assert!(child.wait().unwrap().success());
    git(&dir, &["commit", "-qm", "c"]);

    let out = review(
        &dir,
        &["--base", "base", "--head", "HEAD", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["newFindings"].as_array().unwrap().len(), 1, "{v}"); // bad\xff.rs was read
    assert_eq!(v["unparsed_files"], 1, "{v}"); // binary.rs, and it's said out loud
    assert_eq!(v["risk"], "moderate");

    // The work tree: bad\xff.rs only ever lived in the index, so it's gone from disk.
    let out = review(&dir, &["--base", "base", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["unparsed_files"], 1, "{v}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reviews_two_refs_from_a_subdirectory() {
    let dir = std::env::temp_dir().join(format!("errpath-review-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    commit(
        &dir,
        "src/lib.rs",
        "#[cfg(test)] mod tests;\nfn a() -> Result<u32, E> { Ok(1) }\nfn f() -> Result<u32, E> { a() }\n",
    );
    commit(&dir, "src/tests.rs", "fn t() { x().unwrap(); }\n");
    git(&dir, &["tag", "base"]);
    commit(&dir, "tests/it.rs", "fn t() { y().unwrap(); }\n");
    commit(
        &dir,
        "src/tests.rs",
        "fn t() { x().unwrap(); z().unwrap(); }\n",
    );

    // Only test code changed.
    let out = review(&dir, &["--base", "base"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    commit(
        &dir,
        "src/lib.rs",
        "#[cfg(test)] mod tests;\nfn a() -> Result<u32, E> { Ok(1) }\nfn f() -> Result<u32, E> {\n    let _ = a();\n    a()\n}\n",
    );
    // With --head, the work tree is ignored: only the refs count.
    std::fs::write(dir.join("src/lib.rs"), "not rust (").unwrap();

    let report = dir.join("r.json");
    let out = review(
        &dir.join("src"),
        &[
            "--base",
            "base",
            "--head",
            "HEAD",
            "--format",
            "json",
            "-o",
            report.to_str().unwrap(),
            "src",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(v["risk"], "high");
    assert_eq!(v["newFindings"][0]["file"], "src/lib.rs");
    assert_eq!(v["newFindings"][0]["line"], 4);

    let out = review(&dir, &["--base", "no-such-ref"]);
    assert_eq!(out.status.code(), Some(1));
    let out = review(&dir, &[]);
    assert_eq!(out.status.code(), Some(2));
    std::fs::remove_dir_all(&dir).unwrap();
}

fn errpath(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cargo-errpath"))
        .arg("errpath")
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn repo(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("errpath-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    dir
}

#[test]
fn reviews_uncommitted_and_untracked_edits_without_head() {
    let dir = repo("worktree");
    commit(&dir, "src/lib.rs", "mod gone;\nfn f() {}\n");
    commit(&dir, "src/gone.rs", "fn g() {}\n");
    std::fs::write(
        dir.join("src/lib.rs"),
        "mod new;\nfn f() { x().unwrap(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/new.rs"),
        "fn n() { let _ = std::fs::remove_file(\"a\"); }\n",
    )
    .unwrap();
    std::fs::remove_file(dir.join("src/gone.rs")).unwrap();

    let out = review(&dir, &["--base", "HEAD", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut files: Vec<_> = v["newFindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    files.sort_unstable();
    assert_eq!(files, ["src/lib.rs", "src/new.rs"], "{v}");

    // A ref as head still reads only git.
    let out = review(&dir, &["--base", "HEAD", "--head", "HEAD"]);
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn edit_hook_blocks_only_on_the_edited_files_new_findings() {
    let dir = repo("hook-edit");
    commit(&dir, "src/lib.rs", "#[cfg(test)] mod tests;\nfn f() {}\n");
    commit(&dir, "src/tests.rs", "fn t() {}\n");
    std::fs::write(
        dir.join("src/lib.rs"),
        "#[cfg(test)] mod tests;\nfn f() { x().unwrap(); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/tests.rs"), "fn t() { x().unwrap(); }\n").unwrap();
    let hook = |file: &Path| {
        let input = serde_json::json!({ "tool_input": { "file_path": file } }).to_string();
        errpath(&dir, &["hook", "edit"], &input)
    };

    let out = hook(&dir.join("src/lib.rs"));
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("src/lib.rs:2 no-panic"), "{err}");

    // Test code, a file without findings, and a non-Rust file pass.
    assert_eq!(hook(&dir.join("src/tests.rs")).status.code(), Some(0));
    std::fs::write(dir.join("README.md"), "x").unwrap();
    assert_eq!(hook(&dir.join("README.md")).status.code(), Some(0));

    std::fs::write(
        dir.join("src/lib.rs"),
        "#[cfg(test)] mod tests;\n// errpath: allow(no-panic) x is checked at startup\nfn f() { x().unwrap(); }\n",
    )
    .unwrap();
    assert_eq!(hook(&dir.join("src/lib.rs")).status.code(), Some(0));
    std::fs::remove_dir_all(&dir).unwrap();
}

fn crate_repo(name: &str) -> std::path::PathBuf {
    let dir = repo(name);
    commit(&dir, ".gitignore", "target\n");
    commit(
        &dir,
        "Cargo.toml",
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    commit(&dir, "src/lib.rs", "pub fn f() {}\n");
    dir
}

fn stop_json(out: &Output) -> serde_json::Value {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stdout)))
}

fn blocked(out: &Output) -> String {
    let v = stop_json(out);
    assert_eq!(v["decision"], "block", "{v}");
    v["reason"].as_str().unwrap().to_string()
}

#[test]
fn stop_hook_blocks_once_then_tells_the_person() {
    let dir = crate_repo("hook-stop");

    // Clean tree: nothing to check.
    let out = errpath(&dir, &["hook", "stop"], "{}");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());

    // One panic allowed, one not, and a file errpath can't read.
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn f() -> u8 {\n    // errpath: allow(no-panic) a literal always parses\n    \"1\".parse().unwrap()\n}\n\npub fn g() -> u8 {\n    \"2\".parse().unwrap()\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/orphan.rs"), [0xff, 0xfe]).unwrap();
    // Run from a subdirectory: the change is still found at the root.
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    let out = errpath(
        &dir.join("nested"),
        &["hook", "stop"],
        r#"{"stop_hook_active": false}"#,
    );
    let reason = blocked(&out);
    assert!(reason.contains("src/lib.rs:7 no-panic"), "{reason}");
    assert!(
        !reason.contains("fmt:") && !reason.contains("clippy:"),
        "{reason}"
    );
    // The first block tells the person too.
    let msg = stop_json(&out)["systemMessage"].to_string();
    assert!(msg.contains("a literal always parses"), "{msg}");
    assert!(
        msg.contains("not analysed") && msg.contains("src/orphan.rs"),
        "{msg}"
    );

    // Already retrying: no second block. The person hears about the failure,
    // and still sees the allow reason.
    let v = stop_json(&errpath(
        &dir,
        &["hook", "stop"],
        r#"{"stop_hook_active": true}"#,
    ));
    assert!(v.get("decision").is_none(), "{v}");
    let msg = v["systemMessage"].as_str().unwrap();
    assert!(msg.contains("Still failing"), "{msg}");
    assert!(msg.contains("a literal always parses"), "{msg}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stop_hook_checks_a_manifest_only_change() {
    let dir = crate_repo("hook-manifest");
    std::fs::write(dir.join("Cargo.toml"), "[package\n").unwrap();
    let reason = blocked(&errpath(&dir, &["hook", "stop"], "{}"));
    assert!(
        reason.contains("fmt:") || reason.contains("clippy:"),
        "{reason}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn work_the_session_commits_still_counts_after_a_resume() {
    let dir = crate_repo("hook-session");
    let session = r#"{"session_id": "abc-1"}"#;
    assert_eq!(
        errpath(&dir, &["hook", "start"], session).status.code(),
        Some(0)
    );
    commit(
        &dir,
        "src/lib.rs",
        "pub fn f() -> u8 {\n    \"1\".parse().unwrap()\n}\n",
    );
    let reason = blocked(&errpath(&dir, &["hook", "stop"], session));
    assert!(reason.contains("src/lib.rs:2 no-panic"), "{reason}");

    // Exit and resume: the base stays where the session began.
    errpath(&dir, &["hook", "start"], session);
    let reason = blocked(&errpath(&dir, &["hook", "stop"], session));
    assert!(reason.contains("src/lib.rs:2 no-panic"), "{reason}");

    // Without the session, only uncommitted work counts.
    let out = errpath(&dir, &["hook", "stop"], "{}");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_session_started_before_the_first_commit_owns_everything() {
    let dir = repo("hook-unborn");
    let session = r#"{"session_id": "abc-2"}"#;
    assert_eq!(
        errpath(&dir, &["hook", "start"], session).status.code(),
        Some(0)
    );
    commit(&dir, ".gitignore", "target\n");
    commit(
        &dir,
        "Cargo.toml",
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    commit(
        &dir,
        "src/lib.rs",
        "pub fn f() -> u8 {\n    \"1\".parse().unwrap()\n}\n",
    );
    let reason = blocked(&errpath(&dir, &["hook", "stop"], session));
    assert!(reason.contains("src/lib.rs:2 no-panic"), "{reason}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn edit_hook_works_before_the_first_commit() {
    let dir = repo("edit-unborn");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "fn f() { x().unwrap(); }\n").unwrap();
    let input = serde_json::json!({ "tool_input": { "file_path": dir.join("src/lib.rs") } });
    let out = errpath(&dir, &["hook", "edit"], &input.to_string());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("src/lib.rs:1 no-panic"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn init_names_the_hooks_an_existing_config_lacks() {
    let dir = repo("init-partial");
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    let partial = r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "cargo errpath hook stop"}]}]}}"#;
    std::fs::write(dir.join(".claude/settings.json"), partial).unwrap();
    let out = errpath(&dir, &["init"], "");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(
        err.contains("lacks `cargo errpath hook start`, `cargo errpath hook edit`"),
        "{err}"
    );
    assert!(
        err.contains(".github/workflows/errpath.yml: written"),
        "{err}"
    );
    assert!(err.contains("cargo add autotel"), "{err}");
    // Never overwritten.
    assert_eq!(
        std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        partial
    );
    let out = errpath(&dir, &["init"], "");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(".github/workflows/errpath.yml: already set up"),
        "{err}"
    );
    // Once a manifest brings in autotel, init stops suggesting it.
    commit(
        &dir,
        "Cargo.toml",
        &format!("{PACKAGE}\n[dependencies]\nautotel = \"0.2\"\n"),
    );
    let out = errpath(&dir, &["init"], "");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("cargo add autotel"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stop_hook_checks_every_workspace_the_change_touched() {
    let dir = crate_repo("hook-workspaces");
    // An independent crate inside the repository, with its own workspace.
    commit(
        &dir,
        "nested/Cargo.toml",
        "[package]\nname = \"n\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    commit(&dir, "nested/src/lib.rs", "pub fn n() {}\n");
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn f() -> u8 {\n    \"not a number\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("nested/src/lib.rs"),
        "pub fn n() -> u8 {\n    1\n}\n",
    )
    .unwrap();

    // From the nested crate, the root crate's type error still blocks.
    let reason = blocked(&errpath(&dir.join("nested"), &["hook", "stop"], "{}"));
    assert!(reason.contains("E0308"), "{reason}");
    assert!(reason.contains("clippy:"), "{reason}");
    assert!(!reason.contains("clippy (nested)"), "{reason}");

    // And from the root, a broken nested crate blocks, named by its directory.
    std::fs::write(dir.join("src/lib.rs"), "pub fn f() -> u8 {\n    1\n}\n").unwrap();
    std::fs::write(
        dir.join("nested/src/lib.rs"),
        "pub fn n() -> u8 {\n    \"not a number\"\n}\n",
    )
    .unwrap();
    let reason = blocked(&errpath(&dir, &["hook", "stop"], "{}"));
    assert!(reason.contains("clippy (nested):"), "{reason}");
    assert!(reason.contains("E0308"), "{reason}");
    std::fs::remove_dir_all(&dir).unwrap();
}

const PACKAGE: &str = "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
const TYPE_ERROR: &str = "pub fn f() -> u8 {\n    \"not a number\"\n}\n";

#[test]
fn stop_hook_finds_a_crate_with_a_non_ascii_path() {
    let dir = repo("hook-unicode");
    commit(&dir, ".gitignore", "target\n");
    commit(&dir, "é/Cargo.toml", PACKAGE);
    commit(&dir, "é/src/lib.rs", "pub fn f() {}\n");
    // Git's line output would quote this as "\303\251/src/lib.rs".
    std::fs::write(dir.join("é/src/lib.rs"), TYPE_ERROR).unwrap();
    let reason = blocked(&errpath(&dir, &["hook", "stop"], "{}"));
    assert!(reason.contains("clippy (é):"), "{reason}");
    assert!(reason.contains("E0308"), "{reason}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stop_hook_checks_workspaces_below_a_changed_shared_config() {
    let dir = repo("hook-config");
    commit(&dir, ".gitignore", "target\n");
    commit(&dir, "app/Cargo.toml", PACKAGE);
    commit(&dir, "app/src/lib.rs", "pub fn f() {}\n");
    // No manifest above it: only the crate below can be affected.
    std::fs::create_dir_all(dir.join(".cargo")).unwrap();
    std::fs::write(
        dir.join(".cargo/config.toml"),
        "[build]\nrustflags = [\"--not-a-real-flag\"]\n",
    )
    .unwrap();
    let reason = blocked(&errpath(&dir, &["hook", "stop"], "{}"));
    assert!(reason.contains("clippy (app):"), "{reason}");
    assert!(reason.contains("not-a-real-flag"), "{reason}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Only Linux lets a directory name be bytes that aren't UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn a_repository_under_a_non_utf8_path_is_still_checked() {
    use std::os::unix::ffi::OsStrExt;
    let name = format!("errpath-bytes-{}-", std::process::id());
    let mut raw = name.into_bytes();
    raw.push(0xff);
    let dir = std::env::temp_dir().join(std::ffi::OsStr::from_bytes(&raw));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    commit(&dir, ".gitignore", "target\n");
    commit(&dir, "Cargo.toml", PACKAGE);
    commit(&dir, "src/lib.rs", "pub fn f() {}\n");

    std::fs::write(dir.join("src/lib.rs"), "fn g() { x().unwrap(); }\n").unwrap();
    let out = review(&dir, &["--base", "HEAD", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["newFindings"][0]["rule"], "no-panic", "{v}");

    let input = serde_json::json!({ "tool_input": { "file_path": "src/lib.rs" } });
    let out = errpath(&dir, &["hook", "edit"], &input.to_string());
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Cargo rejects a non-UTF-8 path, so the stop hook blocks with cargo's error.
    let session = r#"{"session_id": "bytes"}"#;
    let reason = blocked(&errpath(&dir, &["hook", "stop"], session));
    assert!(reason.contains("fmt:"), "{reason}");
    assert!(reason.contains("invalid UTF-8"), "{reason}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stop_hook_checks_the_workspace_a_file_moved_out_of() {
    let dir = crate_repo("hook-move");
    commit(&dir, "src/lib.rs", "mod helper;\npub fn f() {}\n");
    commit(&dir, "src/helper.rs", "pub fn h() {}\n");
    commit(&dir, "nested/Cargo.toml", PACKAGE);
    commit(&dir, "nested/src/lib.rs", "mod helper;\n");
    // Moved into the nested crate, and staged so git can see it as a rename.
    git(&dir, &["mv", "src/helper.rs", "nested/src/helper.rs"]);
    let reason = blocked(&errpath(&dir, &["hook", "stop"], "{}"));
    assert!(reason.contains("E0583"), "{reason}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_possible_finding_is_shown_but_does_not_block() {
    let dir = crate_repo("hook-possible");
    // errpath can't see what `send` returns: maybe a dropped error, maybe not.
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn f(tx: &std::sync::mpsc::Sender<u8>) {\n    let _ = tx.send(1);\n}\n",
    )
    .unwrap();
    let input = serde_json::json!({ "tool_input": { "file_path": dir.join("src/lib.rs") } });
    let out = errpath(&dir, &["hook", "edit"], &input.to_string());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = stop_json(&errpath(&dir, &["hook", "stop"], "{}"));
    assert!(v.get("decision").is_none(), "{v}");
    let msg = v["systemMessage"].as_str().unwrap();
    assert!(msg.contains("not blocking"), "{msg}");
    assert!(msg.contains("src/lib.rs:2 no-swallow"), "{msg}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn check_before_the_first_commit_skips_build_output() {
    let dir = repo("check-unborn");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn f() -> u8 {\n    \"1\".parse().unwrap()\n}\n",
    )
    .unwrap();
    // No .gitignore: Cargo's tagged target dir shows up as untracked files.
    let out_dir = dir.join("target/debug/build/x-1/out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(
        dir.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    std::fs::write(out_dir.join("version.rs"), "fn v() { x().unwrap(); }\n").unwrap();

    let out = errpath(&dir, &["check"], "");
    let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert!(!text.contains("Needed a single revision"), "{text}");
    assert!(text.contains("src/lib.rs:2 no-panic"), "{text}");
    assert!(!text.contains("version.rs"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}
