//! `check`, `hook` and `init`: one gate that runs rustfmt, Clippy, errpath and
//! whichever dependency checkers are installed, for people, CI and coding agents.
use cargo_errpath::Confidence;
use cargo_errpath::review::{NewFinding, Options, Review, path_from_bytes, path_line, run};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub enum Outcome {
    Pass,
    Fail(String),
    Skip(String),
}

pub struct Step {
    pub name: String,
    pub outcome: Outcome,
    /// For the person, whatever the outcome: new allow comments, and code
    /// errpath couldn't analyse.
    pub notes: String,
}

fn step(name: impl Into<String>, outcome: Outcome) -> Step {
    Step {
        name: name.into(),
        outcome,
        notes: String::new(),
    }
}

fn output(cmd: &mut Command) -> Option<(bool, String)> {
    let out = cmd.output().ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text.trim().to_string()))
}

/// Cargo picks its workspace from the directory it runs in.
fn cargo(dir: &Path, args: &[&str]) -> Outcome {
    match output(Command::new("cargo").current_dir(dir).args(args)) {
        Some((true, _)) => Outcome::Pass,
        Some((false, text)) => Outcome::Fail(text),
        None => Outcome::Fail("could not run cargo".into()),
    }
}

/// A cargo subcommand people install separately, run only when it's there.
fn optional(dir: &Path, tool: &str, args: &[&str]) -> Outcome {
    match output(Command::new("cargo").args([tool, "--version"])) {
        Some((true, _)) => cargo(dir, args),
        _ => Outcome::Skip(format!("not installed (cargo install cargo-{tool})")),
    }
}

/// A command's stdout, when it succeeds.
fn stdout(cmd: &mut Command) -> Option<Vec<u8>> {
    let out = cmd.output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn git(args: &[&str]) -> Option<String> {
    match output(Command::new("git").args(args)) {
        Some((true, text)) => Some(text),
        _ => None,
    }
}

pub fn top() -> Option<PathBuf> {
    stdout(Command::new("git").args(["rev-parse", "--show-toplevel"])).map(|o| path_line(&o))
}

/// Where the branch started from: the remote's default branch, else `main`
/// or `master`, else `HEAD`, which reviews only uncommitted edits.
pub fn default_base() -> &'static str {
    ["origin/HEAD", "main", "master"]
        .into_iter()
        .find(|r| {
            git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{r}^{{commit}}"),
            ])
            .is_some()
        })
        .unwrap_or("HEAD")
}

fn finding_lines(findings: &[&NewFinding]) -> String {
    let mut s = String::new();
    for f in findings {
        let f = &f.finding;
        let _ = writeln!(s, "  {}:{} {}: {}", f.file, f.line, f.rule, f.message);
    }
    s
}

fn notes(review: &Review, base: &str) -> String {
    let mut s = String::new();
    if !review.new_allows.is_empty() {
        let _ = writeln!(s, "new allow comments since {base}, check the reasons:");
    }
    for f in &review.new_allows {
        let reason = f.allowed.as_deref().unwrap_or("");
        let _ = writeln!(s, "  {}:{} {}: {reason}", f.file, f.line, f.rule);
    }
    // Not a failure: rustc and Clippy still saw this code, only errpath didn't.
    for n in &review.notes {
        let _ = writeln!(s, "not analysed: {n}");
    }
    s
}

/// Fails on new findings only. Risk is for reviewers: a moved step or a new
/// `match` is worth reading, but not worth blocking. `only` keeps one file's;
/// the whole tree is still read, since a parent's `mod tests;` decides what's test code.
fn errpath(base: &str, only: Option<&str>) -> Step {
    let options = Options {
        base,
        head: None,
        paths: &[],
        collapse_diagrams: true,
    };
    let review = run(&options).map(|mut r| {
        if let Some(file) = only {
            r.new_findings.retain(|f| f.finding.file == file);
            r.new_allows.retain(|f| f.file == file);
            r.notes.retain(|n| n.contains(file));
        }
        r
    });
    // errpath: allow(no-trace) the error is the failed step, which every caller prints
    let r = match review {
        Err(e) => return step("errpath", Outcome::Fail(e)),
        Ok(r) => r,
    };
    // Only what errpath is sure of blocks: an agent shouldn't rewrite code
    // over a guess. The person still sees the rest.
    let (sure, unsure): (Vec<_>, Vec<_>) = r
        .new_findings
        .iter()
        .partition(|f| f.finding.confidence == Confidence::Certain);
    let mut notes = notes(&r, base);
    if !unsure.is_empty() {
        let _ = write!(
            notes,
            "possible findings since {base}, not blocking (syntax can't tell for sure):\n{}",
            finding_lines(&unsure)
        );
    }
    Step {
        name: "errpath".into(),
        outcome: if sure.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Fail(format!(
                "new findings since {base}:\n{}\n{}",
                finding_lines(&sure),
                fixes(&sure)
            ))
        },
        notes,
    }
}

/// How to fix each rule that turned up, once each.
fn fixes(findings: &[&NewFinding]) -> String {
    let mut rules: Vec<_> = findings.iter().map(|f| f.finding.rule.as_str()).collect();
    rules.sort_unstable();
    rules.dedup();
    let mut s = String::new();
    for rule in rules {
        let fix = match rule {
            "no-panic" => "return the error instead, with `?`",
            "no-swallow" => {
                "propagate the error with `?`, or handle it with `if let Err(e) = ..` when the caller shouldn't fail"
            }
            "no-trace" => {
                "record the error where it's handled, so it reaches your traces: `tracing::warn!(error = %e, \"..\")`, or `ctx.record_error(&e)` with autotel"
            }
            _ => "fix the allow comment: a known rule and a reason",
        };
        let _ = writeln!(s, "{rule}: {fix}.");
    }
    s.push_str(
        "If it's deliberate, add an `errpath: allow(rule) reason` comment on the line above.",
    );
    s
}

/// fmt, Clippy, deny and shear in each Cargo workspace, then errpath once
/// over the whole repository.
pub fn check(base: &str, workspaces: &[PathBuf]) -> Vec<Step> {
    let top = top();
    let mut steps = vec![];
    for dir in workspaces {
        // Named by workspace only when it isn't the repository's own.
        let name = |tool: &str| match top.as_deref().and_then(|t| dir.strip_prefix(t).ok()) {
            Some(rel) if rel.as_os_str().is_empty() => tool.to_string(),
            Some(rel) => format!("{tool} ({})", rel.display()),
            None if workspaces.len() == 1 => tool.to_string(),
            None => format!("{tool} ({})", dir.display()),
        };
        steps.push(step(name("fmt"), cargo(dir, &["fmt", "--all", "--check"])));
        steps.push(step(
            name("clippy"),
            cargo(
                dir,
                &[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--quiet",
                    "--",
                    "-D",
                    "warnings",
                ],
            ),
        ));
        let has_config = [Some(dir.as_path()), top.as_deref()]
            .into_iter()
            .flatten()
            .any(|d| d.join("deny.toml").is_file());
        let deny = if has_config {
            optional(dir, "deny", &["deny", "--log-level", "error", "check"])
        } else {
            // Without a config, cargo-deny rejects every licence.
            Outcome::Skip("no deny.toml (cargo deny init)".into())
        };
        steps.push(step(name("deny"), deny));
        steps.push(step(name("shear"), optional(dir, "shear", &["shear"])));
    }
    steps.insert(steps.len().min(2), errpath(base, None));
    steps
}

/// Every Cargo workspace a set of changed files can affect. A file affects
/// the workspace of its nearest `Cargo.toml`. A shared config file, like
/// `.cargo/config.toml` or `rustfmt.toml`, also affects every workspace below it.
fn workspaces(top: &Path, changed: &[PathBuf]) -> Vec<PathBuf> {
    let mut manifests = std::collections::BTreeSet::new();
    let mut all: Option<Vec<PathBuf>> = None;
    for file in changed {
        if let Some(m) = top
            .join(file)
            .ancestors()
            .skip(1)
            .take_while(|d| d.starts_with(top))
            .map(|d| d.join("Cargo.toml"))
            .find(|m| m.is_file())
        {
            manifests.insert(m);
        }
        if let Some(scope) = config_scope(file) {
            let all = all.get_or_insert_with(|| {
                let list = [
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "--",
                    "*Cargo.toml",
                ];
                git_paths(top, &list).unwrap_or_default()
            });
            let below = all
                .iter()
                .filter(|m| m.file_name() == Some("Cargo.toml".as_ref()) && m.starts_with(scope));
            manifests.extend(below.map(|m| top.join(m)).filter(|m| m.is_file()));
        }
    }
    let mut roots: Vec<_> = manifests.into_iter().filter_map(workspace_root).collect();
    roots.sort();
    roots.dedup();
    roots
}

/// The directory a config file applies below, or `None` for anything else.
fn config_scope(file: &Path) -> Option<&Path> {
    let dir = file.parent()?;
    match file.file_name()?.to_str()? {
        "config" | "config.toml" if dir.file_name() == Some(".cargo".as_ref()) => dir.parent(),
        "rustfmt.toml"
        | ".rustfmt.toml"
        | "clippy.toml"
        | ".clippy.toml"
        | "deny.toml"
        | "rust-toolchain"
        | "rust-toolchain.toml" => Some(dir),
        _ => None,
    }
}

/// A package's workspace root. A manifest cargo can't read is its own root,
/// so fmt and Clippy report it.
fn workspace_root(manifest: PathBuf) -> Option<PathBuf> {
    let located = stdout(
        Command::new("cargo")
            .args([
                "locate-project",
                "--workspace",
                "--message-format",
                "plain",
                "--manifest-path",
            ])
            .arg(&manifest),
    );
    let root = located.map_or(manifest, |o| path_line(&o));
    root.parent().map(Path::to_path_buf)
}

/// Repo-relative paths from a `-z` git listing, as the filesystem's bytes:
/// git's line output quotes non-ASCII names like `é/Cargo.toml`.
fn git_paths(top: &Path, args: &[&str]) -> Option<Vec<PathBuf>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(top)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let paths = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(path_from_bytes);
    Some(paths.collect())
}

pub fn print(steps: &[Step]) -> bool {
    let mut ok = true;
    for s in steps {
        match &s.outcome {
            Outcome::Pass => eprintln!("{:8} ok", s.name),
            Outcome::Skip(why) => eprintln!("{:8} skipped: {why}", s.name),
            Outcome::Fail(text) => {
                ok = false;
                eprintln!("{:8} failed\n{text}\n", s.name);
            }
        }
        if !s.notes.is_empty() {
            eprintln!("{}", s.notes);
        }
    }
    ok
}

/// Keeps a hook's message short enough for an agent to act on.
fn head(text: &str, lines: usize) -> String {
    let all: Vec<_> = text.lines().collect();
    let mut s = all[..all.len().min(lines)].join("\n");
    if all.len() > lines {
        let _ = write!(s, "\n  ... {} more lines", all.len() - lines);
    }
    s
}

/// Claude Code hooks. Exit 2 puts stderr in front of the agent, which then
/// fixes it; `systemMessage` on stdout goes to the person instead.
pub fn hook(kind: &str) -> i32 {
    let input: Value = serde_json::from_reader(std::io::stdin()).unwrap_or(Value::Null);
    let session = session_file(&input);
    match kind {
        "start" => session.as_deref().map_or(0, hook_start),
        "edit" => hook_edit(&input, &session_base(session.as_deref())),
        "stop" => hook_stop(&input, &session_base(session.as_deref())),
        _ => 2,
    }
}

/// Records where the session began, so work it commits still counts as its
/// own. It fires again on resume and compaction, which mustn't move the base,
/// and nothing fires when a session is gone for good: old records are pruned.
fn hook_start(file: &Path) -> i32 {
    let saved = if file.exists() {
        // Resumed: keep the base, and keep the record from being pruned.
        std::fs::File::options()
            .append(true)
            .open(file)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()))
    } else {
        // Before the first commit, everything the session writes is its own.
        let base = git(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .or_else(|| git(&["hash-object", "-t", "tree", "--stdin"]))
            .unwrap_or_default();
        prune(file.parent());
        file.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(file, base))
    };
    // Exit 1 shows the person, without blocking: falling back to HEAD would
    // let work the agent commits skip the gate.
    // errpath: allow(no-trace) a CLI with no telemetry: the person reads it on stderr
    if let Err(e) = saved {
        eprintln!(
            "errpath: couldn't record the session start, so committed work won't be checked: {e}"
        );
        return 1;
    }
    0
}

/// Records last 30 days from a session's latest start or resume.
fn prune(dir: Option<&Path>) {
    let month = std::time::Duration::from_secs(30 * 24 * 60 * 60);
    let Some(entries) = dir.and_then(|d| std::fs::read_dir(d).ok()) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > month));
        if old {
            // errpath: allow(no-swallow) a record that can't be pruned costs a few bytes
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `<git dir>/errpath/session-<id>`: per work tree, and never committed.
fn session_file(input: &Value) -> Option<PathBuf> {
    let id = input["session_id"].as_str()?;
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let dir = stdout(Command::new("git").args(["rev-parse", "--absolute-git-dir"]))?;
    Some(
        path_line(&dir)
            .join("errpath")
            .join(format!("session-{id}")),
    )
}

/// The commit (or empty tree) the session started from. Without a start
/// hook, `HEAD`, or the empty tree before the first commit.
fn session_base(file: Option<&Path>) -> String {
    file.and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| git(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]))
        .or_else(|| git(&["hash-object", "-t", "tree", "--stdin"]))
        .unwrap_or_else(|| "HEAD".into())
}

/// After each edit: errpath on that one file. Syntax only, so it needs no
/// build; formatting and Clippy wait for `stop`.
fn hook_edit(input: &Value, base: &str) -> i32 {
    let Some(file) = input["tool_input"]["file_path"].as_str() else {
        return 0;
    };
    if !file.ends_with(".rs") {
        return 0;
    }
    let Some(top) = top() else { return 0 };
    let rel = repo_relative(&top, Path::new(file));
    // Findings name files as the review does, lossily for non-UTF-8 paths.
    let rel = rel.to_string_lossy();
    let rel = rel.as_ref();
    match errpath(base, Some(rel)).outcome {
        Outcome::Fail(text) => {
            eprintln!("errpath, {rel}:\n{text}");
            2
        }
        _ => 0,
    }
}

fn repo_relative(top: &Path, file: &Path) -> PathBuf {
    // macOS reports /private/var for /var: compare real paths.
    let top = top.canonicalize().unwrap_or_else(|_| top.into());
    let file = file.canonicalize().unwrap_or_else(|_| file.into());
    file.strip_prefix(&top)
        .map(Path::to_path_buf)
        .unwrap_or(file)
}

/// Files that change what `check` would say.
const RELEVANT: &[&str] = &[
    "*.rs",
    "*Cargo.toml",
    "*Cargo.lock",
    "*rustfmt.toml",
    "*clippy.toml",
    "*deny.toml",
    "*rust-toolchain",
    "*rust-toolchain.toml",
    "*.cargo/config",
    "*.cargo/config.toml",
];

/// Relevant files changed since `base`, committed or not, anywhere in the
/// repository (pathspecs are relative to the current directory otherwise).
/// `None` when git can't say.
fn changed_since(top: &Path, base: &str) -> Option<Vec<PathBuf>> {
    // Without --no-renames a move lists only its destination, and the
    // workspace it left goes unchecked.
    let mut diff = vec!["diff", "-z", "--name-only", "--no-renames", base, "--"];
    diff.extend(RELEVANT);
    let mut untracked = vec!["ls-files", "-z", "--others", "--exclude-standard", "--"];
    untracked.extend(RELEVANT);
    let mut files = git_paths(top, &diff)?;
    files.extend(git_paths(top, &untracked)?);
    Some(files)
}

/// When the agent is about to finish: the whole gate, against where the
/// session started, so it answers for its own work, committed or not.
fn hook_stop(input: &Value, base: &str) -> i32 {
    // Already continuing because of this hook: report, don't trap it in a loop.
    let retry = input["stop_hook_active"].as_bool().unwrap_or(false);
    let Some(top) = top() else { return 0 };
    // If git can't say what changed, check the repository's own workspace.
    let changed = changed_since(&top, base).unwrap_or_else(|| vec!["Cargo.toml".into()]);
    if changed.is_empty() {
        return 0;
    }
    // Every workspace the agent touched, wherever it's working from.
    let workspaces = workspaces(&top, &changed);
    let (mut failed, mut notes) = (String::new(), String::new());
    for s in &check(base, &workspaces) {
        if let Outcome::Fail(text) = &s.outcome {
            let _ = writeln!(failed, "{}:\n{}\n", s.name, head(text, 40));
        }
        notes.push_str(&s.notes);
    }
    if !failed.is_empty() && retry {
        let _ = write!(notes, "Still failing after a fix attempt:\n{failed}");
    }
    // JSON rather than exit 2, so one answer can both block the agent and
    // tell the person.
    let mut out = serde_json::Map::new();
    if !failed.is_empty() && !retry {
        out.insert("decision".into(), "block".into());
        out.insert(
            "reason".into(),
            format!("Before finishing, fix:\n{failed}").into(),
        );
    }
    if !notes.is_empty() {
        out.insert(
            "systemMessage".into(),
            format!("errpath: {}", notes.trim_end()).into(),
        );
    }
    if !out.is_empty() {
        println!("{}", Value::Object(out));
    }
    0
}

const SETTINGS: &str = r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "cargo errpath hook start", "timeout": 10 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit",
        "hooks": [
          {
            "type": "command",
            "command": "cargo errpath hook edit",
            "timeout": 30,
            "statusMessage": "errpath: checking the edited file"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "cargo errpath hook stop",
            "timeout": 300,
            "statusMessage": "errpath: rustfmt, Clippy and error paths"
          }
        ]
      }
    ]
  }
}
"#;

fn workflow() -> String {
    format!(
        r#"name: errpath
on: pull_request
permissions:
  contents: read
  pull-requests: write
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      # Comments on the PR, and puts cargo-errpath on the PATH.
      - uses: jagreehal/errpath@v{version}
      - run: cargo errpath check --base "$BASE"
        env:
          BASE: ${{{{ github.event.pull_request.base.sha }}}}
      - run: cargo test --workspace
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

/// Writes the CI workflow and agent hooks. Never overwrites a file: one that
/// exists is checked for each piece errpath needs, and what's missing is printed.
pub fn init() -> i32 {
    let Some(top) = top() else {
        eprintln!("not in a git repository");
        return 1;
    };
    let files = [
        (
            ".github/workflows/errpath.yml",
            workflow(),
            &["jagreehal/errpath@", "cargo errpath check"][..],
        ),
        (
            ".claude/settings.json",
            SETTINGS.to_string(),
            &[
                "cargo errpath hook start",
                "cargo errpath hook edit",
                "cargo errpath hook stop",
            ][..],
        ),
    ];
    for (rel, content, needs) in files {
        let path = top.join(rel);
        if path.exists() {
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let missing: Vec<_> = needs.iter().filter(|n| !existing.contains(*n)).collect();
            if missing.is_empty() {
                eprintln!("{rel}: already set up");
            } else {
                let missing: Vec<_> = missing.iter().map(|m| format!("`{m}`")).collect();
                eprintln!(
                    "{rel}: exists, so not changed. It lacks {}. Merge in what's missing from:\n{content}",
                    missing.join(", ")
                );
            }
            continue;
        }
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, content));
        // errpath: allow(no-trace) a CLI with no telemetry: the person reads it on stderr
        match written {
            Ok(()) => eprintln!("{rel}: written"),
            Err(e) => {
                eprintln!("{rel}: {e}");
                return 1;
            }
        }
    }
    if !exports_telemetry(&top) {
        eprintln!("{AUTOTEL}");
    }
    eprintln!("Run `cargo errpath check` to see where the code stands now.");
    0
}

const AUTOTEL: &str = "No OpenTelemetry setup found. errpath's `no-trace` rule makes handled errors reach `tracing`. To export them over OTLP, add autotel:
  cargo add autotel
and make this the first line of `main`:
  let _telemetry = autotel::init(autotel::config!().with_tracing_subscriber())?;";

/// Does any manifest in the repository bring in autotel or the OpenTelemetry SDK?
fn exports_telemetry(top: &Path) -> bool {
    let list = [
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        "*Cargo.toml",
    ];
    git_paths(top, &list).unwrap_or_default().iter().any(|m| {
        std::fs::read_to_string(top.join(m))
            .is_ok_and(|s| s.contains("autotel") || s.contains("opentelemetry_sdk"))
    })
}
