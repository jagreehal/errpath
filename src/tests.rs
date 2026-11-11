use super::*;

fn analyse(src: &str) -> Report {
    let mut r = Report::new();
    r.add_source("test.rs", "", src);
    r.finish()
}

/// Lines and conversions are checked separately; shape tests compare the rest.
fn no_lines(nodes: &mut [Node]) {
    for n in nodes {
        match n {
            Node::Step {
                line, conversions, ..
            } => {
                *line = 0;
                conversions.clear();
            }
            Node::Fail { line, .. }
            | Node::Panic { line, .. }
            | Node::Swallow { line, .. }
            | Node::Recover { line, .. }
            | Node::Jump { line, .. } => *line = 0,
            Node::Convert {
                line, conversions, ..
            } => {
                *line = 0;
                conversions.clear();
            }
            Node::Branch { arms, .. } => arms.iter_mut().for_each(|a| no_lines(&mut a.body)),
            Node::Scope { body, .. } | Node::Block { body, .. } => no_lines(body),
            Node::Loop { head, body, .. } => {
                no_lines(head);
                no_lines(body);
            }
            Node::Succeed => {}
        }
    }
}

fn nodes(src: &str, id: &str) -> Vec<Node> {
    let mut n = analyse(src)
        .functions
        .into_iter()
        .find(|f| f.id == id)
        .unwrap()
        .nodes;
    no_lines(&mut n);
    n
}

#[test]
fn a_result_handled_without_anything_fallible_is_a_recovery() {
    let src = "fn f() { if let Err(e) = a() { log(e); } if let Ok(v) = b().await { use_it(v); } }
        fn g() -> Result<(), E> { if let Err(e) = a() { return Err(e); } Ok(()) }
        fn h() { match x() { Some(v) => v, None => 0 }; }";
    let recover = |name: &str, how: &str, arm: &str| Node::Recover {
        name: name.into(),
        how: how.into(),
        arms: vec![arm.into()],
        line: 0,
    };
    assert_eq!(
        nodes(src, "f"),
        vec![
            recover("a", "if let Err", "Err(e)"),
            recover("b", "if let Ok", "Ok(v)")
        ]
    );
    // A handler that fails is drawn as the fork it is.
    assert!(matches!(nodes(src, "g")[0], Node::Branch { .. }));
    // `Option` carries no error to handle.
    assert!(nodes(src, "h").is_empty());
}

#[test]
fn a_match_that_hands_the_error_back_is_a_step() {
    let src = "fn a() -> Result<u32, E> { Ok(1) }
        fn f() -> Result<u32, E> { match a() { Ok(v) => Ok(v), err => err } }
        fn g() -> Result<u32, E> { log(); match a() { Ok(v) => Ok(v), err => { log(&err); err } } }
        fn h() -> Result<u32, E> { let r = match a() { Ok(v) => Ok(v), err if retry() => err, _ => Ok(0) }; Ok(1) }";
    assert_eq!(nodes(src, "f"), vec![step("a", "E")]);
    assert_eq!(nodes(src, "g"), vec![step("a", "E")]);
    // Bindings in inner scopes leave the returned error alone.
    for arm in [
        "{ { let err = 0; log(err); } err }",
        "{ run(|err| log(err)); err }",
        "{ for err in list() { log(err); } err }",
        "{ if let Some(err) = other() { log(err); } err }",
        "{ match other() { err => log(err) } err }",
    ] {
        let f = format!(
            "{src} fn s() -> Result<u32, E> {{ match a() {{ Ok(v) => Ok(v), err => {arm} }} }}"
        );
        assert_eq!(nodes(&f, "s"), vec![step("a", "E")], "{arm}");
    }
    // An inner scope that assigns the outer binding still changes it.
    let f = format!(
        "{src} fn t() -> Result<u32, E> {{ match a() {{ Ok(v) => Ok(v), mut err => {{ {{ err = Ok(0); }} err }} }} }}"
    );
    assert!(nodes(&f, "t").is_empty());
    // A success handed back carries no error: the `Err` arm handles it.
    let j = nodes(
        &format!(
            "{src} fn j() -> Result<u32, E> {{ match a() {{ ok @ Ok(_) => ok, Err(_) => Ok(0) }} }}"
        ),
        "j",
    );
    assert!(matches!(j[..], [Node::Recover { .. }]), "{j:?}");
    // Bindings on both sides: the error one goes back.
    let k = nodes(
        &format!(
            "{src} fn k() -> Result<u32, E> {{ match a() {{ ok @ Ok(_) => ok, e @ Err(_) => e }} }}"
        ),
        "k",
    );
    assert_eq!(k, vec![step("a", "E")]);
    // A guard decides which errors go back: a fork with the step on that arm.
    let i = nodes(
        &format!(
            "{src} fn i(r: bool) -> Result<u32, E> {{ match a() {{ Ok(v) => Ok(v), err if r => err, _ => Ok(0) }} }}"
        ),
        "i",
    );
    let Node::Branch { arms, .. } = &i[0] else {
        panic!("{i:?}")
    };
    assert_eq!(arms[1].label, "err if r");
    assert_eq!(arms[1].body, vec![step("a", "E")]);
    assert!(arms[2].body.is_empty());
    // Bound to a local, the error stays here.
    assert!(matches!(nodes(src, "h")[0], Node::Recover { .. }));
}

fn step(name: &str, error: &str) -> Node {
    Node::Step {
        name: name.into(),
        error: Some(error.into()),
        source: ErrorSource::Callee,
        conversions: vec![],
        line: 0,
    }
}

fn via_enclosing(name: &str, error: &str) -> Node {
    Node::Step {
        name: name.into(),
        error: Some(error.into()),
        source: ErrorSource::Enclosing,
        conversions: vec![],
        line: 0,
    }
}

fn arm(label: &str, body: Vec<Node>) -> Arm {
    Arm {
        label: label.into(),
        body,
    }
}

fn fail(error: &str) -> Node {
    Node::Fail {
        error: error.into(),
        line: 0,
    }
}

#[test]
fn linear_railway() {
    let n = nodes(include_str!("../examples/transfer.rs"), "transfer");
    assert_eq!(
        n,
        [
            step("validate", "ValidationError"),
            step("fetch_rate", "RateUnavailableError"),
            step("convert", "InsufficientFundsError"),
            step("provider.execute", "TransferError"),
        ]
    );
    let out = render("transfer", &n);
    assert!(out.contains("flowchart LR") && out.contains("Done((Success))"));
    assert_eq!(out.matches("-->|err|").count(), 4);
}

#[test]
fn branches_and_early_returns() {
    let n = nodes(include_str!("../examples/checkout.rs"), "checkout");
    assert_eq!(
        n,
        [
            step("price", "PriceError"),
            Node::Branch {
                label: "if total > user.limit".into(),
                arms: vec![
                    arm("yes", vec![fail("AppError::OverLimit")]),
                    arm("no", vec![])
                ],
            },
            Node::Branch {
                label: "match user.method".into(),
                arms: vec![
                    arm("Method::Card", vec![step("charge_card", "CardDeclined")]),
                    arm(
                        "Method::Wallet",
                        vec![
                            step("load_wallet", "WalletError"),
                            step("debit", "WalletError")
                        ]
                    ),
                    arm("Method::Invoice", vec![Node::Succeed]),
                ],
            },
            step("send_email", "EmailError"),
        ]
    );
    let out = render("checkout", &n);
    // Card and Wallet rejoin at send_email; Invoice short-circuits to Done.
    let into_send_email = out.lines().filter(|l| l.ends_with("-->|ok| S12")).count();
    assert_eq!(into_send_email, 2, "{out}");
    assert!(out.contains("|\"Method::Invoice\"| Done"), "{out}");
    assert!(out.contains("|\"no\"| D"), "{out}"); // empty else arm flows to next fork
}

#[test]
fn wrapper_closures_aliases_tail_err_and_option() {
    let src = include_str!("../examples/wrappers.rs");
    let save = nodes(src, "save");
    assert_eq!(
        save,
        [Node::Scope {
            label: "traced".into(),
            body: vec![
                step("load", "anyhow_like::Error"),
                Node::Branch {
                    label: "match commit(v)".into(),
                    arms: vec![arm("Ok(())", vec![]), arm("Err(e)", vec![fail("e")])],
                },
            ],
        }]
    );
    assert_eq!(nodes(src, "first"), [via_enclosing("xs.first", "None")]);
    // impl method; external callee falls back to what `?` converts into
    assert_eq!(
        nodes(src, "Server::start"),
        [
            step("Listener::bind", "io::Error"),
            via_enclosing("env::var", "io::Error")
        ]
    );
    let out = render("save", &save);
    assert!(
        out.contains("subgraph G1[\"traced\"]") && out.contains("  end\n"),
        "{out}"
    );
    let out = render("start", &nodes(src, "Server::start"));
    assert!(out.contains("\"→ io::Error\""), "{out}");
}

const COLLIDING: &str = r#"
struct A; struct B; struct C;
impl A { fn run(&self) -> Result<(), AErr> { Ok(()) } fn go(&self) -> Result<(), Top> { self.run()?; Ok(()) } }
impl B { fn run() -> Result<(), BErr> { Ok(()) } }
fn by_type() -> Result<(), Top> { B::run()?; Ok(()) }
fn ambiguous(x: X) -> Result<(), Top> { x.run()?; Ok(()) }
fn foreign() -> Result<(), Top> { Foreign::run()?; Ok(()) }
"#;

#[test]
fn same_named_methods_do_not_collide() {
    assert_eq!(nodes(COLLIDING, "A::go"), [step("self.run", "AErr")]);
    assert_eq!(nodes(COLLIDING, "by_type"), [step("B::run", "BErr")]);
    assert_eq!(
        nodes(COLLIDING, "ambiguous"),
        [via_enclosing("x.run", "Top")]
    );
    assert_eq!(
        nodes(COLLIDING, "foreign"),
        [via_enclosing("Foreign::run", "Top")]
    );
}

#[test]
fn finds_every_fn_with_a_stable_id() {
    let r = analyse(
        r#"
        mod net { pub struct T; impl T { fn a(&self) {} } fn free() {} }
        trait Tr { fn with_body(&self) {} fn no_body(&self); }
        impl From<A> for X { fn from(a: A) -> Self { X } }
        impl From<B> for X { fn from(b: B) -> Self { X } }
        fn outer() { fn inner() {} }
        #[cfg(unix)] fn twin() {}
        #[cfg(windows)] fn twin() {}
        #[test] fn a_test() {}
        #[cfg(test)] mod tests { fn helper() {} }
        #[cfg(all(test, feature = "any"))] mod stubs { fn helper() {} }
        #[cfg(any(test, feature = "test-utils"))] mod support { fn helper() {} }
        #[cfg(feature = "latest")] fn name_contains_test() {}
        #[cfg(feature = "test")] fn named_test() {}
        #[cfg(any(test, test_like))] fn not_only_tests() {}
        #[cfg(any(test, unix))] fn also_unix() {}
        #[cfg(any(test, feature = "default"))] fn also_a_feature() {}
        #[cfg(not(test))] fn real() {}
        #[cfg(not(all(test, unix)))] fn real2() {}
        "#,
    );
    let ids: Vec<_> = r.functions.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "net::T::a",
            "net::free",
            "Tr::with_body",
            "Tr::no_body",
            "<X as From<A>>::from",
            "<X as From<B>>::from",
            "outer",
            "outer::inner",
            "twin",
            "twin#2",
            "support::helper",
            "name_contains_test",
            "named_test",
            "not_only_tests",
            "also_unix",
            "also_a_feature",
            "real",
            "real2",
        ]
    );
}

#[test]
fn module_paths() {
    assert_eq!(module_of(Path::new("lib.rs")), "");
    assert_eq!(module_of(Path::new("src/main.rs")), "");
    assert_eq!(module_of(Path::new("net/mod.rs")), "net");
    assert_eq!(module_of(Path::new("src/net/tcp.rs")), "net::tcp");
    assert_eq!(
        module_of(Path::new("billing-api/src/client/pool.rs")),
        "billing-api::client::pool"
    );
    assert_eq!(module_of(Path::new("billing/src/lib.rs")), "billing");
}

#[test]
fn nested_fn_is_not_part_of_the_outer_railway() {
    let src = "fn a() -> Result<(), E> { fn b() -> Result<(), F> { c()?; Ok(()) } d()?; Ok(()) }";
    assert_eq!(nodes(src, "a"), [via_enclosing("d", "E")]);
    assert_eq!(nodes(src, "a::b"), [via_enclosing("c", "F")]);
}

#[test]
fn let_else_keeps_the_rest_of_the_function() {
    let src = "fn f(x: Option<u32>) -> Result<(), E> { let Some(y) = x else { return Err(E); }; g()?; Ok(()) }";
    let n = nodes(src, "f");
    assert_eq!(
        n,
        [
            Node::Branch {
                label: "let Some(y) = x else".into(),
                arms: vec![arm("else", vec![fail("E")]), arm("matched", vec![])]
            },
            via_enclosing("g", "E"),
        ]
    );
    let out = render("f", &n);
    assert!(out.contains("|\"matched\"| S"), "{out}");
}

#[test]
fn async_block_does_not_exit_the_enclosing_fn() {
    let src = "fn f() -> Result<(), E> { spawn(async move { g()?; Ok(()) }); let h = async { i()?; Ok(()) }; Ok(()) }";
    let unknown = |name: &str| Node::Step {
        name: name.into(),
        error: None,
        source: ErrorSource::Unknown,
        conversions: vec![],
        line: 0,
    };
    assert_eq!(
        nodes(src, "f"),
        [
            Node::Scope {
                label: "spawn".into(),
                body: vec![unknown("g")]
            },
            Node::Scope {
                label: "async block".into(),
                body: vec![unknown("i")]
            },
        ]
    );
}

#[test]
fn macro_try_is_counted_not_guessed() {
    let r = analyse("fn f() -> Result<(), E> { a()?; ready!(b()?, c()?); Ok(()) }");
    assert_eq!(r.functions[0].macro_try, 2);
    assert_eq!((r.coverage.steps, r.coverage.macro_try), (1, 2));
}

fn findings_in<'a>(r: &'a Report, id: &str) -> Vec<&'a Finding> {
    r.findings
        .iter()
        .filter(|f| f.function.as_deref() == Some(id))
        .collect()
}

#[test]
fn detects_the_five_silent_failures() {
    let r = analyse(include_str!("../examples/silent.rs"));
    let got: Vec<_> = findings_in(&r, "silent")
        .iter()
        .map(|f| (f.rule.as_str(), f.line, f.confidence, f.message.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (
                NO_SWALLOW,
                10,
                Confidence::Certain,
                "`let _ =` drops the error from `commit`"
            ),
            (
                NO_SWALLOW,
                11,
                Confidence::Certain,
                "`.ok()` drops the error from `commit`"
            ),
            (
                NO_PANIC,
                12,
                Confidence::Certain,
                "`commit.unwrap` can panic"
            ),
            (
                NO_PANIC,
                13,
                Confidence::Certain,
                "`commit.expect` can panic"
            ),
            (
                NO_SWALLOW,
                14,
                Confidence::Certain,
                "`unwrap_or_default` drops the error from `commit`"
            ),
        ]
    );
}

#[test]
fn no_finding_when_no_error_is_lost() {
    let r = analyse(include_str!("../examples/silent.rs"));
    assert_eq!(findings_in(&r, "fine"), Vec::<&Finding>::new());
}

#[test]
fn unknown_callee_is_only_possible() {
    let r = analyse(include_str!("../examples/silent.rs"));
    let f = findings_in(&r, "unknown_callee");
    assert_eq!((f.len(), f[0].confidence), (1, Confidence::Possible));
}

#[test]
fn allow_comments_need_a_known_rule_and_a_reason() {
    let r = analyse(include_str!("../examples/silent.rs"));
    let reasons: Vec<_> = findings_in(&r, "allowed")
        .iter()
        .map(|f| f.allowed.as_deref())
        .collect();
    assert_eq!(
        reasons,
        [
            Some("receiver hung up during shutdown"),
            Some("checked at startup")
        ]
    );

    assert!(
        findings_in(&r, "bad_allows")
            .iter()
            .all(|f| f.allowed.is_none())
    );
    let bad: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == BAD_ALLOW)
        .map(|f| (f.line, f.message.as_str()))
        .collect();
    assert_eq!(
        bad,
        [
            (39, "allow(no-swallow) needs a reason, so it was ignored"),
            (41, "unknown rule `no-such-rule` in allow comment"),
        ]
    );
}

#[test]
fn allow_above_a_multi_line_chain_counts() {
    let src = "fn f() {\n    // errpath: allow(no-panic) fine here\n    let x = make()\n        .build()\n        .unwrap();\n}";
    let r = analyse(src);
    assert_eq!(r.findings[0].allowed.as_deref(), Some("fine here"));
    assert_eq!(r.findings[0].line, 5);
}

#[test]
fn panic_macro_ends_the_path_but_unwrap_does_not() {
    let n = nodes(include_str!("../examples/silent.rs"), "diverges");
    let out = render("diverges", &n);
    assert!(out.contains("{{\"panic!\"}}"), "{out}");
    assert!(!out.contains("P2 -->"), "{out}");

    let out = render(
        "silent",
        &nodes(include_str!("../examples/silent.rs"), "silent"),
    );
    assert_eq!(out.matches("[/\"panic\"/]").count(), 2, "{out}");
    assert_eq!(out.matches("dropped: commit").count(), 3, "{out}");
    assert!(
        out.lines()
            .any(|l| l.ends_with("-->|ok| Done") || l.ends_with("--> Done")),
        "{out}"
    );
}

#[test]
fn ir_json_has_a_versioned_shape() {
    let r = analyse("fn f() -> Result<(), E> { g()?; Ok(()) }");
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["schema_version"], SCHEMA_VERSION);
    let f = &v["functions"][0];
    assert_eq!(
        f["returns"],
        serde_json::json!({"kind": "result", "error": "E"})
    );
    assert_eq!(
        f["nodes"][0],
        serde_json::json!({"kind": "step", "name": "g", "error": "E", "source": "enclosing", "conversions": [], "line": 1})
    );
}

#[test]
fn a_file_that_will_not_parse_is_reported_not_fatal() {
    let mut r = Report::new();
    r.add_source("bad.rs", "", "fn (");
    r.add_source("good.rs", "", "fn ok() {}");
    let r = r.finish();
    assert_eq!((r.parse_failures.len(), r.functions.len()), (1, 1));
}

#[test]
fn out_of_line_test_modules_are_skipped() {
    let mut r = Report::new();
    r.add_source("src/lib.rs", "", "#[cfg(test)] mod tests; fn a() {}");
    r.add_source("src/net.rs", "net", "#[cfg(test)] mod checks; fn b() {}");
    r.add_source("src/tests.rs", "tests", "fn helper() { x().unwrap(); }");
    r.add_source("src/net/checks/mod.rs", "net::checks", "fn helper2() {}");
    let r = r.finish();
    let ids: Vec<_> = r.functions.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["a", "net::b"]);
    assert!(r.findings.is_empty());
}

#[test]
fn callee_errors_come_from_other_files_in_the_crate() {
    let mut r = Report::new();
    r.add_source(
        "src/db.rs",
        "db",
        "pub fn get_user() -> Result<User, NotFound> { todo!() }
         pub struct Pool;
         impl Pool { pub fn open() -> Result<Pool, PoolError> { todo!() } }
         impl Repo { fn load(&self) -> Result<u8, LoadError> { todo!() } }",
    );
    r.add_source(
        "src/tests.rs",
        "tests",
        "fn send() -> Result<(), FromTests> { todo!() }",
    );
    r.add_source(
        "src/lib.rs",
        "",
        "#[cfg(test)] mod tests;
         fn a() -> Result<(), Top> { get_user()?; db::get_user()?; Pool::open()?; x.load()?; send()?; Ok(()) }
         impl Repo { fn b(&self) -> Result<(), Top> { self.load()?; Ok(()) } }",
    );
    let r = r.finish();
    let errors = |id: &str| -> Vec<_> {
        let f = r.functions.iter().find(|f| f.id == id).unwrap();
        f.nodes
            .iter()
            .filter_map(|n| match n {
                Node::Step { error, source, .. } => Some((error.clone().unwrap(), *source)),
                _ => None,
            })
            .collect()
    };
    let callee = |e: &str| (e.to_string(), ErrorSource::Callee);
    let enclosing = ("Top".to_string(), ErrorSource::Enclosing);
    // A bare `x.load()` could be any type's, and test code isn't indexed.
    assert_eq!(
        errors("a"),
        [
            callee("NotFound"),
            callee("NotFound"),
            callee("PoolError"),
            enclosing.clone(),
            enclosing
        ]
    );
    assert_eq!(errors("Repo::b"), [callee("LoadError")]);
}

#[test]
fn callee_errors_stay_within_their_package() {
    let mut r = Report::new();
    r.add_source(
        "a/src/lib.rs",
        "a",
        "pub fn load() -> Result<(), AError> { todo!() }",
    );
    r.add_source(
        "b/src/util.rs",
        "b::util",
        "pub fn save() -> Result<(), BError> { todo!() }",
    );
    r.add_source(
        "b/src/lib.rs",
        "b",
        "fn f() -> Result<(), Top> { load()?; save()?; Ok(()) }",
    );
    let r = r.finish();
    let f = r.functions.iter().find(|f| f.id == "b::f").unwrap();
    let errors: Vec<_> = f
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Step { error, .. } => error.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(errors, ["Top", "BError"]);
}

#[test]
fn callee_errors_follow_the_module_tree_of_each_crate() {
    let mut r = Report::new();
    let files = [
        (
            "src/lib.rs",
            "mod shared; mod private; pub fn run() -> Result<(), LibError> { todo!() }
             fn in_lib() -> Result<(), Top> { parse()?; load()?; tool()?; Ok(()) }",
        ),
        (
            "src/shared.rs",
            "pub fn load() -> Result<(), SharedError> { todo!() }",
        ),
        (
            "src/private.rs",
            "pub fn hidden() -> Result<(), PrivateError> { todo!() }",
        ),
        (
            "src/main.rs",
            "mod cli; mod shared;
             fn in_bin() -> Result<(), Top> { run()?; parse()?; shared::load()?; hidden()?; Ok(()) }",
        ),
        (
            "src/cli.rs",
            "pub fn parse() -> Result<(), CliError> { todo!() }",
        ),
        (
            "src/bin/other.rs",
            "fn in_other() -> Result<(), Top> { parse()?; Ok(()) }",
        ),
        // `[lib] path = "lib.rs"`: a package without `src/`.
        (
            "tools/lib.rs",
            "mod y; pub fn tool() -> Result<(), ToolError> { todo!() }",
        ),
        (
            "tools/y.rs",
            "fn in_tools() -> Result<(), Top> { tool()?; Ok(()) }",
        ),
    ];
    for (file, src) in files {
        r.add_source(file, &module_of(Path::new(file)), src);
    }
    let r = r.finish();
    let errors = |name: &str| -> Vec<_> {
        let f = r.functions.iter().find(|f| f.name == name).unwrap();
        f.nodes
            .iter()
            .filter_map(|n| match n {
                Node::Step { error, .. } => error.clone(),
                _ => None,
            })
            .collect()
    };
    assert_eq!(errors("in_lib"), ["Top", "SharedError", "Top"]);
    // `shared` is compiled into both crates, `private` only into the lib.
    assert_eq!(errors("in_bin"), ["Top", "CliError", "SharedError", "Top"]);
    assert_eq!(errors("in_other"), ["Top"]);
    assert_eq!(errors("in_tools"), ["ToolError"]);
}

#[test]
fn a_file_in_two_crates_resolves_only_what_they_agree_on() {
    let mut r = Report::new();
    let files = [
        (
            "src/lib.rs",
            "mod shared; fn load() -> Result<(), LibError> { todo!() }
             fn same() -> Result<(), SameError> { todo!() }",
        ),
        (
            "src/main.rs",
            "mod shared; fn load() -> Result<(), BinError> { todo!() }
             fn same() -> Result<(), SameError> { todo!() }",
        ),
        (
            "src/shared.rs",
            "fn f() -> Result<(), Top> { super::load()?; super::same()?; Ok(()) }",
        ),
    ];
    for (file, src) in files {
        r.add_source(file, &module_of(Path::new(file)), src);
    }
    let r = r.finish();
    let f = r.functions.iter().find(|f| f.id == "shared::f").unwrap();
    let errors: Vec<_> = f
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Step { error, source, .. } => Some((error.clone().unwrap(), *source)),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors,
        [
            ("Top".to_string(), ErrorSource::Enclosing),
            ("SameError".to_string(), ErrorSource::Callee)
        ]
    );
}

#[test]
fn macros_wrapping_items_are_counted_once() {
    let r = analyse(
        "ffi_fn! { fn a() { b()?; c()?; } }
         macro_rules! m { ($($x:expr)?) => {} }
         fn d() -> Result<(), E> { ready!(e()?); Ok(()) }",
    );
    assert_eq!((r.coverage.item_macro_try, r.coverage.macro_try), (2, 1));
}

#[test]
fn unwrap_or_on_an_unknown_receiver_is_not_reported() {
    let r = analyse(
        "fn f(m: M) -> u32 { m.get(1).copied().unwrap_or(0) + m.len().unwrap_or_default() }",
    );
    assert!(r.findings.is_empty(), "{:?}", r.findings);
}

#[test]
fn allow_marker_inside_a_string_is_ignored() {
    let (allows, bad) = parse_allows("let s = \"// errpath: allow(nonsense\";");
    assert!(allows.is_empty() && bad.is_empty());
}

mod review {
    use super::*;
    use crate::canon;
    use crate::review::{ItemKind, Review, Risk, Status, compare, is_support_code, span};

    fn report(files: &[(&str, &str)]) -> Report {
        let mut r = Report::new();
        for (file, src) in files {
            r.add_source(file, &module_of(Path::new(file)), src);
        }
        r.finish()
    }

    fn diff(base: &str, head: &str) -> Review {
        compare(
            &report(&[("src/lib.rs", base)]),
            &report(&[("src/lib.rs", head)]),
            false,
        )
    }

    const BASE: &str = "fn a() -> Result<u32, E> { Ok(1) }
        fn b() -> Result<u32, E> { Ok(1) }
        fn f() -> Result<u32, Top> { let x = a()?; Ok(x) }";

    #[test]
    fn no_railway_change_means_no_comment() {
        let r = diff(BASE, BASE);
        assert!(!r.changed && r.markdown.is_empty() && r.functions.is_empty());
        assert_eq!(r.risk, Risk::Low);
    }

    #[test]
    fn edits_that_leave_the_railway_alone_are_silent() {
        let head = "// a new comment
            fn extra() -> u32 { 1 + 1 }

            fn a() -> Result<u32, E> { Ok(2) }
            fn b() -> Result<u32, E> { Ok(1) }
            fn f() -> Result<u32, Top> {
                let y = a()?;
                println!(\"{y}\");
                Ok(y + 1)
            }";
        assert!(!diff(BASE, head).changed);
    }

    #[test]
    fn renaming_a_local_is_not_a_change() {
        let base = "fn f(a: A) -> Result<u32, E> { let n = a.get().unwrap(); a.put()?; Ok(n) }";
        let head = "fn f(b: A) -> Result<u32, E> { let n = b.get().unwrap(); b.put()?; Ok(n) }";
        assert!(!diff(base, head).changed);
    }

    #[test]
    fn moved_code_is_not_new() {
        let code = "fn g() -> Result<u32, E> { let n = x().unwrap(); y()?; Ok(n) }";
        let base = report(&[("src/lib.rs", code), ("src/other.rs", "")]);
        let head = report(&[("src/lib.rs", ""), ("src/other.rs", code)]);
        let r = compare(&base, &head, false);
        assert!(!r.changed, "{r:?}");
        assert_eq!(r.moved, 1);
    }

    #[test]
    fn moved_and_edited_shows_only_the_edit() {
        let base = report(&[
            (
                "src/lib.rs",
                "fn g() -> Result<u32, E> { x().unwrap(); y()?; Ok(1) }",
            ),
            ("src/other.rs", ""),
        ]);
        let head = report(&[
            ("src/lib.rs", ""),
            (
                "src/other.rs",
                "fn g() -> Result<u32, E> { x().unwrap(); y()?; z()?; Ok(1) }",
            ),
        ]);
        let r = compare(&base, &head, false);
        assert_eq!(r.functions.len(), 1);
        let f = &r.functions[0];
        assert_eq!(
            (f.status, f.moved_from.as_deref(), f.added.len()),
            (Status::Changed, Some("g"), 1)
        );
        assert!(r.new_findings.is_empty());
    }

    #[test]
    fn a_new_step_is_low_risk_and_shown() {
        let head = BASE.replace("let x = a()?;", "let x = a()?; b()?;");
        let r = diff(BASE, &head);
        assert_eq!((r.changed, r.risk), (true, Risk::Low));
        assert_eq!(r.functions[0].added[0].kind, ItemKind::Step);
        assert!(
            r.markdown
                .starts_with("<!-- cargo-errpath-review -->\n## Error path review: low risk"),
            "{}",
            r.markdown
        );
        assert!(
            r.markdown.contains("- added step `b` → `E`"),
            "{}",
            r.markdown
        );
        assert!(
            r.markdown.contains("```mermaid\nflowchart LR"),
            "{}",
            r.markdown
        );
    }

    #[test]
    fn only_new_panics_are_reported() {
        let base = "fn f() -> u32 { x().unwrap() }";
        let head = "fn f() -> u32 { x().unwrap() + y().expect(\"y\") }";
        let r = diff(base, head);
        let got: Vec<_> = r
            .new_findings
            .iter()
            .map(|f| (f.severity.as_str(), f.finding.rule.as_str(), f.finding.line))
            .collect();
        assert_eq!(got, [("warning", NO_PANIC, 1)]);
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn a_dropped_error_is_high_risk() {
        let head = BASE.replace("let x = a()?;", "let x = a().unwrap_or_default();");
        let r = diff(BASE, &head);
        assert_eq!(r.risk, Risk::High);
        assert_eq!(r.new_findings[0].severity, "error");
        assert_eq!(r.new_findings[0].finding.rule, NO_SWALLOW);
        assert_eq!(r.functions[0].removed[0].kind, ItemKind::Step);
    }

    #[test]
    fn handling_an_error_locally_is_a_recovery() {
        let head = BASE.replace(
            "let x = a()?;",
            "let x = match a() { Ok(v) => v, Err(e) => { log(e); 0 } };",
        );
        let r = diff(BASE, &head);
        assert_eq!(r.risk, Risk::Moderate);
        assert_eq!(r.functions[0].added[0].kind, ItemKind::Recover);
        assert!(
            r.markdown.contains("handled error from `a`"),
            "{}",
            r.markdown
        );
        assert!(r.markdown.contains("handled: a (match)"), "{}", r.markdown);
    }

    #[test]
    fn handling_a_different_error_is_a_change() {
        let src = |pat: &str| {
            format!(
                "fn a() -> Result<u32, E> {{ Ok(1) }} fn f() {{ if let Err({pat}) = a() {{ log(); }} }}"
            )
        };
        let r = diff(&src("E::A"), &src("E::B"));
        assert_eq!(r.risk, Risk::Moderate);
        assert!(
            r.markdown.contains("arms `Err(E::A)` → `Err(E::B)`"),
            "{}",
            r.markdown
        );
    }

    #[test]
    fn a_guard_on_a_handed_back_error_is_a_change() {
        let src = |guard: &str| {
            format!("fn a() -> Result<u32, E> {{ Ok(1) }}
                fn f(retry: bool) -> Result<u32, E> {{ match a() {{ Ok(v) => Ok(v), err if {guard} => err, _ => Ok(0) }} }}")
        };
        let r = diff(&src("retry"), &src("!retry"));
        assert!(r.changed);
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn a_rebound_error_is_neither_handed_back_nor_handled() {
        let src = |arm: &str| {
            format!(
                "fn a() -> Result<u32, E> {{ Ok(1) }}
                fn f() -> Result<u32, E> {{ match a() {{ Ok(v) => Ok(v), {arm} }} }}"
            )
        };
        for arm in [
            "err => { let err = Ok(0); err }",
            "err => { let err = Err(E::B); err }",
            "mut err => { err = Ok(0); err }",
            "mut err => { reset(&mut err); err }",
        ] {
            let r = diff(&src("err => err"), &src(arm));
            assert!(r.changed, "{arm}");
            let f = &r.functions[0];
            assert!(f.added.is_empty(), "{arm}: {:?}", f.added);
            assert_eq!(f.removed[0].kind, ItemKind::Step, "{arm}");
        }
    }

    #[test]
    fn a_possible_drop_is_a_warning() {
        let r = diff("fn f(t: T) {}", "fn f(t: T) { let _ = t.send(1); }");
        assert_eq!(
            (r.new_findings[0].severity.as_str(), r.risk),
            ("warning", Risk::Moderate)
        );
    }

    #[test]
    fn allowing_a_new_finding_still_shows_it() {
        let r = diff(
            "fn f() {}",
            "fn f() {\n// errpath: allow(no-panic) proven safe\nx().unwrap();\n}",
        );
        assert!(r.new_findings.is_empty());
        assert_eq!(r.new_allows.len(), 1);
        assert!(r.markdown.contains("proven safe"), "{}", r.markdown);
    }

    #[test]
    fn a_new_bad_allow_is_reported_once() {
        let bad = "// errpath: allow(no-panic)\nfn f() {}";
        let r = diff("fn f() {}", bad);
        assert_eq!(r.new_findings[0].finding.rule, BAD_ALLOW);
        assert_eq!(r.new_findings[0].severity, "warning");
        assert!(!diff(bad, bad).changed);
    }

    #[test]
    fn a_changed_error_type_is_moderate_and_policy_is_left_to_the_reader() {
        let r = diff(
            "fn f() -> Result<(), MyError> { Ok(()) }",
            "fn f() -> Result<(), Box<dyn std::error::Error>> { Ok(()) }",
        );
        assert_eq!(r.risk, Risk::Moderate);
        assert!(
            r.markdown
                .contains("returns `Result<_, MyError>` → `Result<_, Box<dyn std::error::Error>>`"),
            "{}",
            r.markdown
        );
    }

    #[test]
    fn changing_a_callee_error_is_a_change() {
        let head = BASE.replace("fn a() -> Result<u32, E>", "fn a() -> Result<u32, F>");
        let r = diff(BASE, &head);
        let f = r.functions.iter().find(|f| f.id == "f").unwrap();
        assert_eq!(
            (f.added[0].error.as_deref(), f.removed[0].error.as_deref()),
            (Some("F"), Some("E"))
        );
    }

    #[test]
    fn reordering_exits_is_flagged() {
        let base = "fn f() -> Result<(), E> { a()?; b()?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { b()?; a()?; Ok(()) }";
        let r = diff(base, head);
        assert!(r.functions[0].reordered);
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn new_and_removed_fns_alone_stay_silent() {
        let r = diff(
            "fn old() -> Result<(), E> { a()?; Ok(()) } fn pure_old() {}",
            "fn fresh() -> Result<(), F> { b()?; c()?; Ok(()) } fn pure_new() {}",
        );
        assert!(!r.changed && r.markdown.is_empty(), "{r:?}");
        assert_eq!((r.new_functions, r.removed_functions), (1, 1));
    }

    #[test]
    fn a_new_fn_with_a_finding_is_listed() {
        let r = diff("", "fn fresh() { x().unwrap(); }");
        assert_eq!(r.functions[0].status, Status::Added);
        assert_eq!(r.new_findings.len(), 1);
    }

    #[test]
    fn a_method_name_match_is_never_certain() {
        // `Local::send` is unrelated to `tx`, so this is at most a guess.
        let local = "struct Local; impl Local { fn send(&self) -> Result<(), E> { Ok(()) } }";
        let base = format!("{local} fn f(tx: T) {{}}");
        let r = diff(
            &base,
            &format!("{local} fn f(tx: T) {{ let _ = tx.send(); }}"),
        );
        assert_eq!(
            (r.new_findings[0].finding.confidence, r.risk),
            (Confidence::Possible, Risk::Moderate)
        );
        // ...and one returning nothing proves nothing about `tx` either.
        let local = "struct Local; impl Local { fn send(&self) {} }";
        let r = diff(
            &format!("{local} fn f(tx: T) {{}}"),
            &format!("{local} fn f(tx: T) {{ let _ = tx.send(); }}"),
        );
        assert_eq!(r.new_findings.len(), 1);
    }

    #[test]
    fn unseen_changes_are_said_out_loud() {
        let r = diff("fn f() {}", "fn f() { ready!(a()?); }");
        assert_eq!(r.functions[0].added[0].kind, ItemKind::MacroTry);
        let r = diff("fn f() {}", "fn f() {} wrap! { fn g() { a()?; } }");
        assert!(
            r.changed && r.notes[0].contains("from 0 to 1"),
            "{:?}",
            r.notes
        );
        let r = diff("fn f() {}", "fn (");
        assert!(r.notes[0].contains("could not be read"), "{:?}", r.notes);
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn diagrams_collapse_on_request() {
        let head = BASE.replace("let x = a()?;", "let x = a()?; b()?;");
        let r = compare(
            &report(&[("src/lib.rs", BASE)]),
            &report(&[("src/lib.rs", &head)]),
            true,
        );
        assert!(
            r.markdown
                .contains("<details><summary>Error path diagram</summary>"),
            "{}",
            r.markdown
        );
    }

    #[test]
    fn moving_a_step_into_a_branch_is_a_change() {
        let base = "fn f(b: bool) -> Result<(), E> { a()?; if b { c()?; } Ok(()) }";
        let head = "fn f(b: bool) -> Result<(), E> { if b { a()?; c()?; } Ok(()) }";
        let r = diff(base, head);
        assert!(r.changed, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn moving_a_step_into_a_loop_is_a_change() {
        let base = "fn f(xs: X) -> Result<(), E> { a()?; for x in xs { c()?; } Ok(()) }";
        let head = "fn f(xs: X) -> Result<(), E> { for x in xs { a()?; c()?; } Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn changing_when_a_branch_runs_is_a_change() {
        let base = "fn f(n: u32) -> Result<(), E> { if n > 1 { a()?; } Ok(()) }";
        let head = "fn f(n: u32) -> Result<(), E> { if n > 2 { a()?; } Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn a_new_early_return_is_a_change() {
        let base = "fn f(n: u32) -> Result<(), E> { a()?; Ok(()) }";
        let head = "fn f(n: u32) -> Result<(), E> { if n > 1 { return Ok(()); } a()?; Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn changing_an_error_conversion_is_a_change() {
        let base = "fn f() -> Result<(), Top> { a().map_err(Top::A)?; Ok(()) }";
        let head = "fn f() -> Result<(), Top> { a().map_err(Top::B)?; Ok(()) }";
        let r = diff(base, head);
        assert!(r.changed, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
        assert!(
            r.markdown.contains("`map_err(Top::A)` → `map_err(Top::B)`"),
            "{}",
            r.markdown
        );
    }

    #[test]
    fn removing_an_allow_makes_the_finding_new() {
        let base = "fn f() {\n// errpath: allow(no-panic) proven safe\nx().unwrap();\n}";
        let head = "fn f() {\n\nx().unwrap();\n}";
        let r = diff(base, head);
        assert_eq!(r.new_findings.len(), 1, "{r:?}");
        assert_eq!(r.new_findings[0].finding.rule, NO_PANIC);
    }

    #[test]
    fn adding_an_allow_to_an_existing_finding_is_shown() {
        let base = "fn f() {\n\nx().unwrap();\n}";
        let head = "fn f() {\n// errpath: allow(no-panic) proven safe\nx().unwrap();\n}";
        let r = diff(base, head);
        assert_eq!(r.new_allows.len(), 1, "{r:?}");
        assert!(r.markdown.contains("proven safe"), "{}", r.markdown);
    }

    #[test]
    fn a_branch_move_says_where_from_and_to() {
        let base = "fn f(b: bool) -> Result<(), E> { a()?; if b { c()?; } Ok(()) }";
        let head = "fn f(b: bool) -> Result<(), E> { if b { a()?; c()?; } Ok(()) }";
        let r = diff(base, head);
        assert!(
            r.markdown
                .contains("step `a`: moved from `top level` to `if b → yes`"),
            "{}",
            r.markdown
        );
    }

    #[test]
    fn a_long_condition_differing_past_the_label_cap_is_a_change() {
        let cond = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let base = format!("fn f() -> Result<(), E> {{ if {cond}1 {{ a()?; }} Ok(()) }}");
        let head = format!("fn f() -> Result<(), E> {{ if {cond}2 {{ a()?; }} Ok(()) }}");
        assert!(diff(&base, &head).changed);
    }

    #[test]
    fn rewording_context_is_shown_but_stays_low_risk() {
        let base = "fn f() -> Result<(), E> { a().context(\"loading\")?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { a().context(\"reading\")?; Ok(()) }";
        let r = diff(base, head);
        assert_eq!((r.changed, r.risk), (true, Risk::Low));
    }

    #[test]
    fn conversions_are_kept_in_the_ir_in_order() {
        let r = analyse("fn f() -> Result<(), E> { a().map_err(X).context(\"c\")?; Ok(()) }");
        let Node::Step { conversions, .. } = &r.functions[0].nodes[0] else {
            panic!()
        };
        assert_eq!(conversions, &["map_err(X)", "context(\"c\")"]);
    }

    #[test]
    fn a_loop_can_be_skipped_in_the_diagram() {
        let src = "fn f(xs: X) -> Result<(), E> { for x in xs { return Err(E); } a()?; Ok(()) }";
        let out = render("f", &analyse(src).functions[0].nodes);
        assert!(out.contains("subgraph G1[\"for x in xs\"]"), "{out}");
        assert!(out.contains("-->|\"done\"| S"), "{out}"); // zero iterations reach `a`
    }

    #[test]
    fn every_result_says_what_it_could_not_see() {
        let r = diff(
            "fn f() { ready!(a()?); }",
            "fn f() { ready!(a()?); } wrap! { fn g() { b()?; } }",
        );
        assert_eq!((r.coverage.macro_try, r.coverage.item_macro_try), (1, 1));
        assert!(
            r.markdown.contains("2 `?` inside macros weren't analysed"),
            "{}",
            r.markdown
        );
        let quiet = diff("fn f() { ready!(a()?); }", "fn f() { ready!(a()?); }");
        assert!(!quiet.changed);
        assert!(
            quiet.summary.contains("not analysed: 1 `?` in macros"),
            "{}",
            quiet.summary
        );
    }

    #[test]
    fn a_step_in_a_match_guard_is_seen() {
        let base = "fn f(n: u32) -> Result<(), E> { match n { 0 if a()? => {} _ => {} } Ok(()) }";
        let head = "fn f(n: u32) -> Result<(), E> { match n { 0 if b()? => {} _ => {} } Ok(()) }";
        let r = diff(base, head);
        assert!(r.changed, "{r:?}");
        assert_eq!(r.functions[0].added[0].label, "b");
    }

    #[test]
    fn a_panic_in_a_match_guard_is_a_finding() {
        let r = diff(
            "fn f(n: u32) { match n { 0 => {} _ => {} } }",
            "fn f(n: u32) { match n { 0 if a().unwrap() => {} _ => {} } }",
        );
        assert_eq!(r.new_findings.len(), 1, "{r:?}");
    }

    #[test]
    fn changing_a_guard_condition_is_a_change() {
        let base =
            "fn f(n: u32) -> Result<(), E> { match n { x if x > 1 => g()?, _ => {} } Ok(()) }";
        let head =
            "fn f(n: u32) -> Result<(), E> { match n { x if x > 2 => g()?, _ => {} } Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn swapping_and_for_or_is_a_change() {
        let base = "fn f(b: bool) -> Result<(), E> { if b && a()? { g(); } Ok(()) }";
        let head = "fn f(b: bool) -> Result<(), E> { if b || a()? { g(); } Ok(()) }";
        let r = diff(base, head);
        assert!(r.changed, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn the_right_operand_of_a_short_circuit_is_conditional() {
        let r = diff(
            "fn f(b: bool) -> Result<(), E> { let x = a()?; let y = b && x; Ok(()) }",
            "fn f(b: bool) -> Result<(), E> { let y = b && a()?; Ok(()) }",
        );
        assert_eq!(r.risk, Risk::Moderate);
        assert!(r.markdown.contains("to `b && … → true`"), "{}", r.markdown);
    }

    #[test]
    fn a_reorder_survives_another_change() {
        let base = "fn f() -> Result<(), E> { a()?; b().context(\"old\")?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { b().context(\"new\")?; a()?; Ok(()) }";
        let r = diff(base, head);
        assert!(r.functions[0].reordered, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn a_reorder_survives_an_added_step() {
        let base = "fn f() -> Result<(), E> { a()?; b()?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { c()?; b()?; a()?; Ok(()) }";
        assert!(diff(base, head).functions[0].reordered);
    }

    #[test]
    fn a_new_step_alone_is_not_a_reorder() {
        let base = "fn f() -> Result<(), E> { a()?; b()?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { a()?; c()?; b()?; Ok(()) }";
        let r = diff(base, head);
        assert!(!r.functions[0].reordered);
        assert_eq!(r.risk, Risk::Low);
    }

    #[test]
    fn swapping_identical_calls_in_different_places_is_a_change() {
        let base = "fn f(x: bool) -> Result<(), E> { a()?; if x { a()?; } Ok(()) }";
        let head = "fn f(x: bool) -> Result<(), E> { if x { a()?; } a()?; Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn reviewed_code_cannot_inject_into_the_comment() {
        let head = "fn f() {\n// railway-free ![x](http://evil/a.png)\n// errpath: allow(no-panic) @org/team ![x](http://evil/a.png) `close\nx().unwrap();\n}";
        let r = diff(
            "fn f() {\n// errpath: allow(no-panic) ok then\nx().unwrap();\n}",
            head,
        );
        let line = r
            .markdown
            .lines()
            .find(|l| l.contains("allow comment"))
            .unwrap();
        assert!(
            line.contains("``@org/team ![x](http://evil/a.png) `close``"),
            "{line}"
        );
    }

    #[test]
    fn code_spans_hold_any_text() {
        assert_eq!(span("a"), "`a`");
        assert_eq!(span("a`b"), "``a`b``");
        assert_eq!(span("`a"), "`` `a ``");
        assert_eq!(span("a\nb\r# heading"), "`a b # heading`");
        assert_eq!(span(&"x".repeat(999)).len(), 202);
    }

    #[test]
    fn a_backtick_cannot_close_the_diagram_fence() {
        let src = "fn f() -> Result<(), E> { if s == \"```\" { a()?; } Ok(()) }";
        let out = render("f", &analyse(src).functions[0].nodes);
        assert!(!out.contains('`'), "{out}");
    }

    #[test]
    fn only_a_packages_own_support_dirs_are_skipped() {
        for path in [
            "tests/it.rs",
            "benches/b.rs",
            "examples/e.rs",
            "crates/a/tests/it.rs",
            "tests/src/x.rs",
        ] {
            assert!(is_support_code(path), "{path}");
        }
        for path in [
            "src/lib.rs",
            "src/tests/pay.rs",
            "crates/a/src/examples/x.rs",
            "src/benches.rs",
        ] {
            assert!(!is_support_code(path), "{path}");
        }
    }

    #[test]
    fn an_unwrap_behind_a_production_feature_is_a_finding() {
        let r = diff(
            "#[cfg(feature = \"latest\")] fn f() {}",
            "#[cfg(feature = \"latest\")] fn f() { x().unwrap(); }",
        );
        assert_eq!(r.new_findings.len(), 1, "{r:?}");
    }

    #[test]
    fn changing_a_let_else_pattern_is_a_change() {
        let base = "fn f(x: Option<u32>) -> Result<(), E> { let Some(0) = x else { return Err(E); }; Ok(()) }";
        let head = "fn f(x: Option<u32>) -> Result<(), E> { let Some(1) = x else { return Err(E); }; Ok(()) }";
        let r = diff(base, head);
        assert!(r.changed, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn changing_a_let_else_initializer_is_a_change() {
        let base =
            "fn f() -> Result<(), E> { let Some(v) = first() else { return Err(E); }; Ok(()) }";
        let head =
            "fn f() -> Result<(), E> { let Some(v) = last() else { return Err(E); }; Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn inverting_a_break_before_a_step_is_a_change() {
        let base = "fn f(b: bool) -> Result<(), E> { loop { if b { break; } a()?; } Ok(()) }";
        let head = "fn f(b: bool) -> Result<(), E> { loop { if !b { break; } a()?; } Ok(()) }";
        let r = diff(base, head);
        assert!(r.changed, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn a_continue_and_its_target_are_seen() {
        let base = "fn f(b: bool) -> Result<(), E> { 'o: for x in y { for z in w { if b { continue; } a()?; } } Ok(()) }";
        let head = "fn f(b: bool) -> Result<(), E> { 'o: for x in y { for z in w { if b { continue 'o; } a()?; } } Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn a_loop_with_nothing_fallible_stays_quiet() {
        let base = "fn f(xs: X) { for x in xs { if x > 1 { break; } println!(\"a\"); } }";
        let head = "fn f(xs: X) { for x in xs { if x > 2 { continue; } println!(\"b\"); } }";
        assert!(!diff(base, head).changed);
    }

    #[test]
    fn a_break_leaves_the_loop_in_the_diagram() {
        let src = "fn f(b: bool) -> Result<(), E> { loop { if b { break; } a()?; } c()?; Ok(()) }";
        let out = render("f", &analyse(src).functions[0].nodes);
        assert!(out.contains("([\"break\"])"), "{out}");
        let id = out
            .lines()
            .find(|l| l.contains("break"))
            .unwrap()
            .trim()
            .split('(')
            .next()
            .unwrap();
        assert!(
            out.lines()
                .any(|l| l.trim().starts_with(&format!("{id} --> S"))),
            "{out}"
        );
    }

    #[test]
    fn a_trait_contract_change_is_seen() {
        let r = diff(
            "trait T { fn f(&self) -> Result<(), A>; }",
            "trait T { fn f(&self) -> Result<(), B>; }",
        );
        assert_eq!((r.changed, r.risk), (true, Risk::Moderate));
    }

    #[test]
    fn a_conversion_away_from_an_exit_is_seen() {
        let base = "fn f() -> Result<(), E> { let r = a().map_err(X); g(r); Ok(()) }";
        let head = "fn f() -> Result<(), E> { let r = a().map_err(Y); g(r); Ok(()) }";
        let r = diff(base, head);
        assert_eq!((r.changed, r.risk), (true, Risk::Moderate));
    }

    #[test]
    fn a_conversion_at_a_step_is_recorded_once() {
        let r = analyse("fn f() -> Result<(), E> { a().map_err(X).map_err(Y)?; Ok(()) }");
        assert_eq!(r.functions[0].nodes.len(), 1, "{:?}", r.functions[0].nodes);
    }

    #[test]
    fn a_conversion_in_an_argument_is_its_own() {
        let n = nodes(
            "fn f() -> Result<(), E> { a(b().map_err(X))?; Ok(()) }",
            "f",
        );
        assert!(matches!(n[0], Node::Convert { .. }), "{n:?}");
    }

    #[test]
    fn swapping_which_call_panics_is_a_change() {
        let r = diff("fn f() { a().unwrap(); }", "fn f() { b().unwrap(); }");
        assert_eq!(r.new_findings.len(), 1, "{r:?}");
    }

    #[test]
    fn a_jump_out_of_an_inner_loop_keeps_that_loop() {
        let src = |c: &str| {
            format!(
                "fn f(xs: X) -> Result<(), E> {{ 'o: for x in xs {{ for y in x {{ if {c} {{ break 'o; }} }} a()?; }} Ok(()) }}"
            )
        };
        let r = diff(&src("y > 1"), &src("y < 1"));
        assert!(r.changed, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn jumps_that_stay_inside_nested_loops_stay_quiet() {
        let src = |c: &str| {
            format!(
                "fn f(xs: X) {{ 'o: for x in xs {{ for y in x {{ if {c} {{ break 'o; }} }} }} }}"
            )
        };
        assert!(!diff(&src("y > 1"), &src("y < 1")).changed);
    }

    #[test]
    fn continue_goes_back_to_the_head_of_its_loop() {
        let out = render(
            "f",
            &analyse("fn f() -> Result<(), E> { loop { a()?; continue; } b()?; Ok(()) }").functions
                [0]
            .nodes,
        );
        let head = out
            .lines()
            .find(|l| l.contains("((\"repeat\"))"))
            .unwrap()
            .trim()
            .split('(')
            .next()
            .unwrap();
        let jump = out
            .lines()
            .find(|l| l.contains("[\"continue\"]"))
            .unwrap()
            .trim()
            .split('(')
            .next()
            .unwrap();
        assert!(out.contains(&format!("{jump} --> {head}")), "{out}");
        // `loop` always runs and only `break` leaves it, so nothing reaches `b`.
        let b = out
            .lines()
            .find(|l| l.contains("[\"b\"]"))
            .unwrap()
            .trim()
            .split('[')
            .next()
            .unwrap();
        assert!(
            !out.lines()
                .any(|l| l.trim_end().ends_with(&format!(" {b}"))),
            "{out}"
        );
    }

    #[test]
    fn break_leaves_a_loop_and_a_for_loop_can_finish() {
        let out = render("f", &analyse("fn f(xs: X, c: bool) -> Result<(), E> { for x in xs { if c { break; } a()?; } b()?; Ok(()) }").functions[0].nodes);
        let head = out
            .lines()
            .find(|l| l.contains("((\"repeat\"))"))
            .unwrap()
            .trim()
            .split('(')
            .next()
            .unwrap();
        assert!(
            out.lines()
                .any(|l| l.trim().starts_with(&format!("{head} -->|\"done\"| S"))),
            "{out}"
        );
        assert!(
            out.lines().any(|l| l.trim().starts_with("S")
                && l.contains("-->|ok|")
                && l.trim_end().ends_with(head)),
            "{out}"
        );
    }

    #[test]
    fn a_labelled_jump_finds_its_own_loop() {
        let out = render("f", &analyse("fn f(xs: X, c: bool) -> Result<(), E> { 'o: for x in xs { for y in x { if c { continue 'o; } a()?; } b()?; } Ok(()) }").functions[0].nodes);
        let outer = out
            .lines()
            .find(|l| l.contains("((\"repeat\"))"))
            .unwrap()
            .trim()
            .split('(')
            .next()
            .unwrap();
        let jump = out
            .lines()
            .find(|l| l.contains("continue 'o"))
            .unwrap()
            .trim()
            .split('(')
            .next()
            .unwrap();
        assert!(out.contains(&format!("{jump} --> {outer}")), "{out}");
    }

    #[test]
    fn a_comment_inside_a_closure_is_not_a_change() {
        let src = |c: &str| {
            format!(
                "fn f() -> Result<(), E> {{\n a().ok_or_else(|| {{\n // {c}\n Err(E)\n }})?;\n Ok(())\n}}"
            )
        };
        assert!(!diff(&src("old note"), &src("new note")).changed);
    }

    #[test]
    fn a_rename_does_not_hide_a_changed_method() {
        let base = "fn f() -> Result<(), E> { let fetch = client(); fetch.fetch()?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { let load = client(); load.load()?; Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn a_rename_does_not_hide_a_changed_string() {
        let base =
            "fn f() -> Result<(), E> { let fetch = c(); a(fetch).context(\"fetch\")?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { let load = c(); a(load).context(\"load\")?; Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn a_rename_still_reaches_every_real_reference() {
        let src = |v: &str| {
            format!(
                "fn f() -> Result<(), E> {{ let {v} = c(); if {v} > 1 {{ {v}.run().map_err(|e| Wrap({v}, e))?; }} Ok(()) }}"
            )
        };
        assert!(!diff(&src("fetch"), &src("load")).changed);
    }

    #[test]
    fn swapping_which_loop_a_label_names_is_a_change() {
        let src = |o: &str, i: &str| {
            format!(
                "fn f(c: bool, xs: X) -> Result<(), E> {{ {o}: for x in xs {{ {i}: for y in x {{ if c {{ break 'outer; }} a()?; }} b()?; }} Ok(()) }}"
            )
        };
        let r = diff(&src("'outer", "'inner"), &src("'inner", "'outer"));
        assert!(r.changed, "{r:?}");
        assert_eq!(r.risk, Risk::Moderate);
    }

    #[test]
    fn renaming_a_label_everywhere_is_not_a_change() {
        let src = |l: &str| {
            format!(
                "fn f(c: bool, xs: X) -> Result<(), E> {{ {l}: for x in xs {{ for y in x {{ if c {{ break {l}; }} a()?; }} }} Ok(()) }}"
            )
        };
        assert!(!diff(&src("'outer"), &src("'rows")).changed);
    }

    #[test]
    fn a_while_condition_runs_on_every_iteration() {
        let out = render(
            "f",
            &analyse("fn f() -> Result<(), E> { while ready()? { work()?; } done()?; Ok(()) }")
                .functions[0]
                .nodes,
        );
        let id = |needle: &str| {
            out.lines()
                .find(|l| l.contains(needle))
                .unwrap()
                .trim()
                .split(['(', '['])
                .next()
                .unwrap()
                .to_string()
        };
        let (head, ready, work, done) = (
            id("((\"repeat\"))"),
            id("[\"ready\"]"),
            id("[\"work\"]"),
            id("[\"done\"]"),
        );
        assert!(out.contains(&format!("{head} --> {ready}")), "{out}");
        assert!(out.contains(&format!("{ready} -->|ok| {work}")), "{out}");
        assert!(out.contains(&format!("{work} -->|ok| {head}")), "{out}");
        assert!(
            out.contains(&format!("{ready} -->|\"done\"| {done}")),
            "{out}"
        );
    }

    #[test]
    fn moving_a_step_into_a_while_condition_is_a_change() {
        let base = "fn f() -> Result<(), E> { let r = ready()?; while r { work()?; } Ok(()) }";
        let head = "fn f() -> Result<(), E> { while ready()? { work()?; } Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn a_fallible_condition_keeps_an_otherwise_empty_loop() {
        let r = analyse("fn f() -> Result<(), E> { while ready()? { } Ok(()) }");
        assert_eq!(r.coverage.steps, 1);
    }

    #[test]
    fn literals_keep_their_exact_text() {
        let src = |s: &str| format!("fn f() -> Result<(), E> {{ a().context(\"{s}\")?; Ok(()) }}");
        assert!(diff(&src("a . b"), &src("a.b")).changed);
        assert!(diff(&src("a ( b"), &src("a(b")).changed);
        let cond = |s: &str| {
            format!("fn f(x: &str) -> Result<(), E> {{ if x == \"{s}\" {{ a()?; }} Ok(()) }}")
        };
        assert!(diff(&cond("a , b"), &cond("a, b")).changed);
    }

    #[test]
    fn a_trailing_literal_keeps_its_text() {
        let r = analyse("fn f() -> Result<(), E> { a().context(\"x ,\")?; Ok(()) }");
        let Node::Step { conversions, .. } = &r.functions[0].nodes[0] else {
            panic!()
        };
        assert_eq!(conversions, &["context(\"x ,\")"]);
    }

    #[test]
    fn retargeting_a_jump_between_labelled_blocks_is_a_change() {
        let src = |t: &str| {
            format!(
                "fn f() -> Result<(), E> {{ 'outer: {{ 'inner: {{ break {t}; }} a()?; }} Ok(()) }}"
            )
        };
        let r = diff(&src("'outer"), &src("'inner"));
        assert_eq!((r.changed, r.risk), (true, Risk::Moderate));
    }

    #[test]
    fn swapping_block_labels_is_a_change() {
        let src = |o: &str, i: &str| {
            format!(
                "fn f(c: bool) -> Result<(), E> {{ {o}: {{ {i}: {{ if c {{ break 'outer; }} a()?; }} b()?; }} Ok(()) }}"
            )
        };
        assert!(diff(&src("'outer", "'inner"), &src("'inner", "'outer")).changed);
    }

    #[test]
    fn a_plain_break_in_a_block_still_targets_the_loop() {
        let src = |c: &str| {
            format!(
                "fn f(xs: X) -> Result<(), E> {{ for x in xs {{ 'b: {{ if {c} {{ break; }} }} a()?; }} Ok(()) }}"
            )
        };
        assert!(diff(&src("x > 1"), &src("x < 1")).changed);
    }

    #[test]
    fn a_block_with_only_local_jumps_stays_quiet() {
        let src =
            |c: &str| format!("fn f() {{ 'b: {{ if {c} {{ break 'b; }} println!(\"x\"); }} }}");
        assert!(!diff(&src("x > 1"), &src("x < 1")).changed);
    }

    #[test]
    fn renaming_a_block_label_is_not_a_change() {
        let src = |l: &str| {
            format!(
                "fn f(c: bool) -> Result<(), E> {{ {l}: {{ if c {{ break {l}; }} a()?; }} b()?; Ok(()) }}"
            )
        };
        assert!(!diff(&src("'x"), &src("'y")).changed);
    }

    #[test]
    fn a_block_jump_lands_after_the_block_in_the_diagram() {
        let out = render(
            "f",
            &analyse(
                "fn f(c: bool) -> Result<(), E> { 'b: { if c { break 'b; } a()?; } z()?; Ok(()) }",
            )
            .functions[0]
                .nodes,
        );
        let id = |needle: &str| {
            out.lines()
                .find(|l| l.contains(needle))
                .unwrap()
                .trim()
                .split(['(', '['])
                .next()
                .unwrap()
                .to_string()
        };
        let (jump, a, z) = (id("break 'b"), id("[\"a\"]"), id("[\"z\"]"));
        assert!(out.contains(&format!("{jump} --> {z}")), "{out}");
        assert!(out.contains(&format!("{a} -->|ok| {z}")), "{out}");
    }

    #[test]
    fn a_rename_does_not_touch_any_literal() {
        let src = |v: &str, lit: &str| {
            format!(
                "fn f(c: X) -> Result<(), E> {{ let {v} = client(); if c == {lit} {{ {v}.run()?; }} Ok(()) }}"
            )
        };
        for (old, new) in [
            ("'a'", "'b'"),
            ("b'a'", "b'b'"),
            ("\"a\"", "\"b\""),
            ("b\"a\"", "b\"b\""),
            ("r\"a\"", "r\"b\""),
            ("r#\"a\"#", "r#\"b\"#"),
            ("br#\"a\"#", "br#\"b\"#"),
            ("c\"a\"", "c\"b\""),
        ] {
            assert!(
                diff(&src("a", old), &src("b", new)).changed,
                "{old} -> {new}"
            );
        }
    }

    #[test]
    fn a_rename_reaches_past_a_literal() {
        let src = |v: &str| {
            format!(
                "fn f(c: X) -> Result<(), E> {{ let {v} = client(); if c == '\\'' && {v} > r#\"x\"y\"# {{ {v}.run()?; }} Ok(()) }}"
            )
        };
        assert!(!diff(&src("a"), &src("b")).changed);
    }

    #[test]
    fn a_binding_named_like_a_literal_prefix_is_still_renamed() {
        let src = |v: &str| {
            format!("fn f() -> Result<(), E> {{ let {v} = c(); if {v} > 1 {{ a()?; }} Ok(()) }}")
        };
        assert!(!diff(&src("b"), &src("r")).changed);
    }

    #[test]
    fn an_endless_loop_before_a_step_is_a_change() {
        let base = "fn f() -> Result<(), E> { a()?; Ok(()) }";
        let head = "fn f() -> Result<(), E> { loop {} a()?; Ok(()) }";
        let r = diff(base, head);
        assert_eq!((r.changed, r.risk), (true, Risk::Moderate));
    }

    #[test]
    fn an_endless_loop_stops_the_diagram() {
        let out = render(
            "f",
            &analyse("fn f() -> Result<(), E> { loop {} a()?; Ok(()) }").functions[0].nodes,
        );
        let a = out
            .lines()
            .find(|l| l.contains("[\"a\"]"))
            .unwrap()
            .trim()
            .split('[')
            .next()
            .unwrap();
        assert!(
            !out.lines()
                .any(|l| l.trim_end().ends_with(&format!(" {a}"))),
            "{out}"
        );
    }

    #[test]
    fn removing_a_loops_only_break_is_a_change() {
        let base = "fn f(c: bool) -> Result<(), E> { loop { if c { break; } } a()?; Ok(()) }";
        let head = "fn f(c: bool) -> Result<(), E> { loop { if c { continue; } } a()?; Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn an_endless_loop_nested_in_a_pruned_loop_is_kept() {
        let base = "fn f(xs: X) -> Result<(), E> { for x in xs { } a()?; Ok(()) }";
        let head = "fn f(xs: X) -> Result<(), E> { for x in xs { loop {} } a()?; Ok(()) }";
        assert!(diff(base, head).changed);
    }

    #[test]
    fn a_loop_that_can_break_stays_quiet() {
        let src = |c: &str| format!("fn f() {{ loop {{ if {c} {{ break; }} }} }}");
        assert!(!diff(&src("x > 1"), &src("x < 1")).changed);
    }

    #[test]
    fn a_rename_handles_awkward_labels_without_panicking() {
        let src = |v: &str, lit: &str| {
            format!(
                "fn f(c: X) -> Result<(), E> {{ let {v} = k(); if c == {lit} && {v} > 1 {{ a()?; }} Ok(()) }}"
            )
        };
        for lit in [
            "'\\u{1F600}'",
            "'é'",
            "\"é → x\"",
            "'\\''",
            "b'\\n'",
            "\"\\\"\"",
        ] {
            assert!(!diff(&src("v", lit), &src("w", lit)).changed, "{lit}");
        }
    }

    #[test]
    fn an_identifier_cannot_pose_as_a_literal() {
        let src = |left: &str| {
            format!("fn f() -> Result<(), E> {{ if {left} == \"denied\" {{ a()?; }} Ok(()) }}")
        };
        assert!(diff(&src("__literal_0__"), &src("\"denied\"")).changed);
        assert!(diff(&src("__literal_1__"), &src("__literal_0__")).changed);
    }

    #[test]
    fn labels_are_written_from_tokens() {
        let cases = [
            ("x.y", "x.y"),
            ("f(a, b,)", "f(a, b)"),
            ("Foo { a, b: 1, }", "Foo { a, b: 1 }"),
            ("x == \"a , b ,\"", "x == \"a , b ,\""),
            ("v[0] >= 'a'", "v[0] >= 'a'"),
            ("a::b::<C>(d)", "a::b::< C >(d)"),
            ("x.map(|e| e + 1)", "x.map(|e| e + 1)"),
            ("x.map(move |a, b| a)", "x.map(move |a, b| a)"),
            ("a | b", "a | b"),
            ("f(a | b, |c| c)", "f(a | b, |c| c)"),
        ];
        for (expr, want) in cases {
            assert_eq!(
                canon(&syn::parse_str::<syn::Expr>(expr).unwrap()),
                want,
                "{expr}"
            );
        }
    }

    #[test]
    fn a_literal_ending_in_a_comma_keeps_it() {
        let r = analyse("fn f() -> Result<(), E> { a().context(\"x,\")?; Ok(()) }");
        let Node::Step { conversions, .. } = &r.functions[0].nodes[0] else {
            panic!()
        };
        assert_eq!(conversions, &["context(\"x,\")"]);
    }

    #[test]
    fn a_one_element_tuple_is_not_its_value() {
        let src = |e: &str| format!("fn f(x: X) -> Result<(), E> {{ if {e} {{ a()?; }} Ok(()) }}");
        for (tuple, value) in [
            ("check((x,))", "check((x))"),
            ("(x,) == y", "(x) == y"),
            ("y > (x,)", "y > (x)"),
            ("match (x,) { _ => true }", "match (x) { _ => true }"),
        ] {
            assert!(diff(&src(tuple), &src(value)).changed, "{tuple}");
        }
    }

    #[test]
    fn optional_commas_are_still_formatting() {
        let src = |e: &str| format!("fn f(x: X) -> Result<(), E> {{ if {e} {{ a()?; }} Ok(()) }}");
        for (with, without) in [
            ("check(x,)", "check(x)"),
            ("o.check(x,)", "o.check(x)"),
            ("(x, y,) == z", "(x, y) == z"),
            ("[x,] == z", "[x] == z"),
            ("g(x)(y,)", "g(x)(y)"),
            ("v[0](y,)", "v[0](y)"),
        ] {
            assert!(!diff(&src(with), &src(without)).changed, "{with}");
        }
    }

    #[test]
    fn a_one_element_tuple_pattern_keeps_its_comma() {
        let src = |p: &str| {
            format!(
                "fn f(x: X) -> Result<(), E> {{ let {p} = x else {{ return Err(E); }}; Ok(()) }}"
            )
        };
        assert!(diff(&src("(Some(y),)"), &src("(Some(y))")).changed);
    }

    #[test]
    fn renaming_a_closure_parameter_is_not_a_change() {
        let src = |p: &str| {
            format!(
                "fn f() -> Result<(), E> {{ let error = 1; a().map_err(|{p}| Wrap({p}, 1))?; b().map_err(|e| Other(e))?; Ok(()) }}"
            )
        };
        let r = diff(&src("e"), &src("error"));
        assert!(!r.changed, "{}", r.markdown);
    }

    #[test]
    fn changing_a_closure_body_is_still_a_change() {
        let src =
            |body: &str| format!("fn f() -> Result<(), E> {{ a().map_err(|e| {body})?; Ok(()) }}");
        assert!(diff(&src("A(e)"), &src("B(e)")).changed);
        assert!(diff(&src("A(e)"), &src("A(outer)")).changed);
    }

    #[test]
    fn closure_parameters_keep_their_order() {
        let src = |body: &str| {
            format!("fn f() -> Result<(), E> {{ a().map_err(|x, y| {body})?; Ok(()) }}")
        };
        assert!(diff(&src("A(x, y)"), &src("A(y, x)")).changed);
    }

    #[test]
    fn a_comment_shows_the_closure_as_written() {
        let r = diff(
            "fn f() -> Result<(), E> { a().map_err(|e| A(e))?; Ok(()) }",
            "fn f() -> Result<(), E> { a().map_err(|e| B(e))?; Ok(()) }",
        );
        assert!(
            r.markdown
                .contains("`map_err(|e| A(e))` → `map_err(|e| B(e))`"),
            "{}",
            r.markdown
        );
    }

    #[test]
    fn a_huge_change_still_fits_in_a_comment() {
        let body: String = (0..4000)
            .map(|i| format!("fn f{i}() {{ long_call_name_{i}().unwrap(); }}\n"))
            .collect();
        let base: String = (0..4000).map(|i| format!("fn f{i}() {{}}\n")).collect();
        let r = diff(&base, &body);
        assert!(r.markdown.len() <= 60_000, "{}", r.markdown.len());
        assert!(
            r.markdown.contains("too long"),
            "{}",
            &r.markdown[r.markdown.len() - 300..]
        );
        assert_eq!(r.new_findings.len(), 4000);
    }

    #[test]
    fn a_long_change_drops_diagrams_before_anything_else() {
        let many = |extra: &str| -> String {
            (0..5)
                .map(|i| format!("fn f{i}() -> Result<(), E> {{ {extra} Ok(()) }}\n"))
                .collect()
        };
        let steps: String = (0..300)
            .map(|i| format!("a_rather_long_step_name_number_{i}()?; "))
            .collect();
        let r = diff(&many(""), &many(&steps));
        assert!(r.markdown.len() <= 60_000);
        assert!(
            !r.markdown.contains("```mermaid"),
            "diagrams should go first"
        );
    }

    #[test]
    fn json_has_the_fields_the_action_reads() {
        let v = serde_json::to_value(diff("fn f() {}", "fn f() { x().unwrap(); }")).unwrap();
        assert_eq!(v["risk"], "moderate");
        assert!(v["markdown"].is_string());
        let f = &v["newFindings"][0];
        for field in ["severity", "file", "rule", "message"] {
            assert!(f[field].is_string(), "{field}: {f}");
        }
        assert_eq!(f["line"], 1);
    }
}
