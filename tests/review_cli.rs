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

    let out = review(&dir, &["--base", "base", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["newFindings"].as_array().unwrap().len(), 1, "{v}"); // bad\xff.rs was read
    assert_eq!(v["unparsed_files"], 1, "{v}"); // binary.rs, and it's said out loud
    assert_eq!(v["risk"], "moderate");
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
    // The work tree is ignored: only the refs count.
    std::fs::write(dir.join("src/lib.rs"), "not rust (").unwrap();

    let report = dir.join("r.json");
    let out = review(
        &dir.join("src"),
        &[
            "--base",
            "base",
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
