//! Compares the railways of two git refs: how a change moved error
//! propagation, recovery and panic paths.
//!
//! Each exit has an identity (what it is) and a state (where it sits, how its
//! error is converted, whether it's allowed). Comparing them separately keeps
//! "same call, now inside an `if`" from cancelling out as no change.
use crate::{
    Confidence, Coverage, ErrorSource, Finding, Function, Locals, NO_PANIC, Node, Report, Returns,
    add_findings, module_of, render,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;
use std::process::Command;
use syn::Expr;
use syn::visit::{self, Visit};

/// Starts every review comment, so the Action can find and update it.
pub const MARKER: &str = "<!-- cargo-errpath-review -->";
const MAX_FUNCTIONS: usize = 20;
const MAX_DIAGRAMS: usize = 5;
const MAX_SPAN: usize = 200;
const MAX_COMMENT: usize = 60_000;

#[derive(Debug, PartialEq, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Moderate,
    High,
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Changed,
    Added,
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Step,
    Fail,
    Panic,
    Swallow,
    /// An explicit `return` that succeeds, skipping whatever follows.
    Return,
    /// `break` or `continue`, deciding what runs next.
    Jump,
    /// An error converted away from any exit.
    Convert,
    /// A `loop` nothing breaks out of: what follows it can't run.
    Diverge,
    /// A `match`, tracked for its arms even when some have nothing fallible.
    Fork,
    /// A `?` inside a macro call: we know it's there, not where it goes.
    MacroTry,
}

/// What can change about an exit while it stays the same exit.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct State {
    /// The branches, loops and closures around it, outermost first.
    pub context: String,
    pub conversions: Vec<String>,
    /// `conversions` for comparing, with closure parameters by position.
    #[serde(skip)]
    conversion_ids: Vec<String>,
    pub allowed: Option<String>,
    /// For a `match`: its arms in order, which decide what each one catches.
    pub arms: Vec<String>,
    /// For a jump: how many loops and labelled blocks out it lands, 0 being the
    /// innermost. This, not the label's spelling, is what makes it the same jump.
    pub target: Option<usize>,
}

/// One way out of the happy path, flattened from the IR.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub kind: ItemKind,
    pub label: String,
    /// The callee's error, when its own signature named one.
    pub error: Option<String>,
    pub line: usize,
    #[serde(flatten)]
    pub state: State,
    /// What makes two items the same across refs.
    #[serde(skip)]
    key: String,
    #[serde(skip)]
    finding: Option<Finding>,
}

/// The same exit in a different state.
#[derive(Debug, Serialize)]
pub struct ItemChange {
    #[serde(flatten)]
    pub item: Item,
    pub from: State,
}

#[derive(Debug, Serialize)]
pub struct ReturnsChange {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize)]
pub struct FnChange {
    pub id: String,
    pub status: Status,
    pub file: String,
    pub line: usize,
    /// The id at the base ref, when the function moved.
    pub moved_from: Option<String>,
    pub returns: Option<ReturnsChange>,
    pub added: Vec<Item>,
    pub removed: Vec<Item>,
    pub changed: Vec<ItemChange>,
    /// The exits on both sides run in a different order.
    pub reordered: bool,
    #[serde(skip)]
    diagram: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct NewFinding {
    pub severity: String,
    #[serde(flatten)]
    pub finding: Finding,
}

/// `markdown`, `risk` and `newFindings` are what the GitHub Action reads.
#[derive(Debug, Serialize)]
pub struct Review {
    pub schema_version: u32,
    /// False means no change was seen, and `markdown` is empty. Read that
    /// together with `coverage`: this is not proof that nothing changed.
    pub changed: bool,
    pub risk: Risk,
    pub markdown: String,
    #[serde(rename = "newFindings")]
    pub new_findings: Vec<NewFinding>,
    /// Findings this change introduced and silenced with an allow comment.
    pub new_allows: Vec<Finding>,
    pub functions: Vec<FnChange>,
    /// Functions that moved or were renamed with their railway intact.
    pub moved: usize,
    /// New and deleted fallible functions with nothing to flag. They aren't
    /// a change to an existing failure path, so they alone stay silent.
    pub new_functions: usize,
    pub removed_functions: usize,
    /// Things that limit this review, such as files that wouldn't parse.
    pub notes: Vec<String>,
    /// How much of the head ref could be analysed.
    pub coverage: Coverage,
    pub unparsed_files: usize,
    /// Files under `tests/`, `benches/` or `examples/`, which aren't compared.
    pub support_files_skipped: usize,
    /// One line on what was compared and what wasn't.
    pub summary: String,
}

fn nested(context: &str, part: &str) -> String {
    if context.is_empty() {
        part.to_string()
    } else {
        format!("{context} / {part}")
    }
}

/// A loop or labelled block a jump can land on.
#[derive(Clone)]
struct Frame {
    tag: Option<String>,
    is_loop: bool,
}

/// How many of `frames` (innermost last) a jump passes to reach its target.
/// A plain `break` passes labelled blocks on its way to a loop.
fn jump_target(name: &str, frames: &[Frame]) -> Option<usize> {
    match name.split_once(' ') {
        Some((_, label)) => frames
            .iter()
            .rev()
            .position(|f| f.tag.as_deref() == Some(label)),
        None => frames.iter().rev().position(|f| f.is_loop),
    }
}

fn flatten(nodes: &[Node], f: &Function, context: &str, tags: &[Frame], out: &mut Vec<Item>) {
    for n in nodes {
        let mut found = vec![];
        add_findings(std::slice::from_ref(n), f, &mut found);
        let finding = if matches!(n, Node::Panic { .. } | Node::Swallow { .. }) {
            found.pop()
        } else {
            None
        };
        let item = |kind,
                    label: &str,
                    error: Option<String>,
                    line: usize,
                    key: String,
                    conversions: &[String],
                    allowed: &Option<String>| Item {
            kind,
            label: label.to_string(),
            error,
            line,
            state: State {
                context: context.to_string(),
                conversions: conversions.to_vec(),
                conversion_ids: conversions.iter().map(|c| positional(c)).collect(),
                allowed: allowed.clone(),
                arms: vec![],
                target: None,
            },
            key,
            finding: finding.clone(),
        };
        match n {
            Node::Step {
                name,
                error,
                source,
                conversions,
                line,
            } => {
                let error = error.clone().filter(|_| *source == ErrorSource::Callee);
                let key = format!("step {name} {error:?}");
                out.push(item(
                    ItemKind::Step,
                    name,
                    error,
                    *line,
                    key,
                    conversions,
                    &None,
                ));
            }
            Node::Fail { error, line } => out.push(item(
                ItemKind::Fail,
                error,
                None,
                *line,
                format!("fail {error}"),
                &[],
                &None,
            )),
            Node::Panic {
                name,
                line,
                allowed,
                ..
            } => {
                out.push(item(
                    ItemKind::Panic,
                    name,
                    None,
                    *line,
                    format!("panic {name}"),
                    &[],
                    allowed,
                ));
            }
            Node::Swallow {
                name,
                how,
                line,
                allowed,
                ..
            } => {
                out.push(item(
                    ItemKind::Swallow,
                    name,
                    None,
                    *line,
                    format!("swallow {how} {name}"),
                    &[],
                    allowed,
                ));
            }
            Node::Succeed => out.push(item(
                ItemKind::Return,
                "return",
                None,
                f.line,
                "return".into(),
                &[],
                &None,
            )),
            Node::Jump { name, line } => {
                let word = name.split(' ').next().unwrap_or(name);
                let mut jump = item(
                    ItemKind::Jump,
                    name,
                    None,
                    *line,
                    format!("jump {word}"),
                    &[],
                    &None,
                );
                jump.state.target = jump_target(name, tags);
                out.push(jump);
            }
            Node::Convert {
                name,
                conversions,
                line,
            } => {
                out.push(item(
                    ItemKind::Convert,
                    name,
                    None,
                    *line,
                    format!("convert {name}"),
                    conversions,
                    &None,
                ));
            }
            Node::Branch { label, arms } => {
                if label.starts_with("match ") {
                    let mut fork = item(
                        ItemKind::Fork,
                        label,
                        None,
                        f.line,
                        format!("fork {label}"),
                        &[],
                        &None,
                    );
                    fork.state.arms = arms.iter().map(|a| a.label.clone()).collect();
                    out.push(fork);
                }
                for a in arms {
                    flatten(
                        &a.body,
                        f,
                        &nested(context, &format!("{label} → {}", a.label)),
                        tags,
                        out,
                    );
                }
            }
            Node::Scope { label, body } => flatten(body, f, &nested(context, label), tags, out),
            Node::Block { tag, body } => {
                let tags = [
                    tags,
                    &[Frame {
                        tag: Some(tag.clone()),
                        is_loop: false,
                    }],
                ]
                .concat();
                flatten(body, f, &nested(context, "block"), &tags, out);
            }
            Node::Loop {
                label,
                tag,
                can_skip,
                head,
                body,
            } => {
                let inside = nested(context, label);
                let frames = [
                    tags,
                    &[Frame {
                        tag: tag.clone(),
                        is_loop: true,
                    }],
                ]
                .concat();
                flatten(head, f, &nested(&inside, "condition"), &frames, out);
                flatten(body, f, &inside, &frames, out);
                if crate::diverges(*can_skip, tag.as_deref(), body) {
                    out.push(item(
                        ItemKind::Diverge,
                        label,
                        None,
                        f.line,
                        "diverge".into(),
                        &[],
                        &None,
                    ));
                }
            }
        }
    }
}

fn items(f: &Function) -> Vec<Item> {
    let mut out = vec![];
    flatten(&f.nodes, f, "", &[], &mut out);
    let macro_try = |_| Item {
        kind: ItemKind::MacroTry,
        label: "`?` inside a macro".into(),
        error: None,
        line: f.line,
        state: State::default(),
        key: "macro".into(),
        finding: None,
    };
    out.extend((0..f.macro_try).map(macro_try));
    out
}

/// Replaces references to local bindings in a label. `fetch.run` is a
/// reference, but `x.fetch`, `fetch::f`, `Error`, `'a` and any literal are not.
fn rename(text: &str, map: &HashMap<&str, &str>) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let (mut out, mut rest) = (String::new(), text);
    while let Some(first) = rest.chars().next() {
        if let Some(end) = literal_end(rest) {
            let (literal, tail) = rest.split_at(end);
            out += literal;
            rest = tail;
            continue;
        }
        let end = if word(first) {
            rest.find(|c| !word(c)).unwrap_or(rest.len())
        } else {
            rest.char_indices()
                .skip(1)
                .find(|(_, c)| word(*c) || matches!(c, '"' | '\''))
                .map_or(rest.len(), |(i, _)| i)
        };
        let (token, tail) = rest.split_at(end);
        let member = (out.ends_with('.') && !out.ends_with(".."))
            || out.ends_with("::")
            || tail.starts_with("::");
        out += if member {
            token
        } else {
            map.get(token).copied().unwrap_or(token)
        };
        rest = tail;
    }
    out
}

/// If `s` starts with a literal or lifetime, where it ends. Comes before
/// identifiers, because `b` in `b"x"` is not a binding called `b`.
fn literal_end(s: &str) -> Option<usize> {
    let opens = |p: &str, after: &str| {
        after.starts_with('"')
            || (p == "b" || p.is_empty()) && after.starts_with('\'')
            || p.ends_with('r') && after.trim_start_matches('#').starts_with('"')
    };
    let prefix = ["br", "cr", "b", "c", "r", ""]
        .into_iter()
        .find(|p| s.strip_prefix(*p).is_some_and(|after| opens(p, after)))?;
    let body = &s[prefix.len()..];
    if prefix.ends_with('r') {
        let hashes = body.len() - body.trim_start_matches('#').len();
        let close = format!("\"{}", "#".repeat(hashes));
        let end = body[hashes + 1..]
            .find(&close)
            .map_or(body.len(), |i| hashes + 1 + i + close.len());
        return Some(prefix.len() + end);
    }
    let quote = body.chars().next()?;
    let (mut escaped, mut has_escape) = (false, false);
    for (i, c) in body.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' => (escaped, has_escape) = (true, true),
            c if c == quote => return Some(prefix.len() + i + 1),
            // `'ab`: more than one character and no closing quote is a lifetime.
            _ if quote == '\'' && i > 1 && !has_escape => break,
            _ => {}
        }
    }
    if quote == '"' {
        return Some(s.len());
    }
    let name = body[1..]
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .map_or(body.len(), |i| i + 1);
    Some(prefix.len() + name)
}

/// Bindings that vanished, paired in order with ones that appeared. A rename
/// shouldn't reach a reviewer, but `if a` -> `if b` between two bindings that
/// exist on both sides still should.
// Removing one binding and adding an unrelated one in the same function reads
// as a rename. Matching bindings by where they're used would separate them.
fn renames<'a>(base: &'a [String], head: &'a [String]) -> HashMap<&'a str, &'a str> {
    let gone = base.iter().filter(|b| !head.contains(b));
    let new = head.iter().filter(|h| !base.contains(h));
    new.map(String::as_str)
        .zip(gone.map(String::as_str))
        .collect()
}

/// Rewrites head items in base's names, for comparing only.
fn in_base_names(items: &mut [Item], map: &HashMap<&str, &str>) {
    for i in items {
        i.key = rename(&i.key, map);
        i.state.context = rename(&i.state.context, map);
        let list = |v: &mut Vec<String>| v.iter_mut().for_each(|s| *s = rename(s, map));
        list(&mut i.state.conversions);
        list(&mut i.state.conversion_ids);
        list(&mut i.state.arms);
    }
}

/// Identity and state together: only an exact match means nothing changed.
fn exact(i: &Item) -> String {
    let s = &i.state;
    format!(
        "{}\0{}\0{:?}\0{:?}\0{:?}\0{:?}",
        i.key, s.context, s.conversion_ids, s.allowed, s.arms, s.target
    )
}

/// Closure parameters are scoped to their closure, so their names mean nothing
/// to a review: `map_err(|e| A(e))` is `map_err(|err| A(err))`. `$` can't start
/// an identifier, so these can't collide with real code.
fn positional(conversion: &str) -> String {
    #[derive(Default)]
    struct Params(Locals);
    impl<'ast> Visit<'ast> for Params {
        fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
            c.inputs.iter().for_each(|p| self.0.visit_pat(p));
            visit::visit_expr_closure(self, c);
        }
    }
    let Ok(call) = syn::parse_str::<Expr>(&format!("x.{conversion}")) else {
        return conversion.to_string();
    };
    let mut params = Params::default();
    params.visit_expr(&call);
    let slots: Vec<_> = (0..params.0.0.len()).map(|i| format!("${i}")).collect();
    rename(
        conversion,
        &params
            .0
            .0
            .iter()
            .map(String::as_str)
            .zip(slots.iter().map(String::as_str))
            .collect(),
    )
}

/// Whole-railway identity, for spotting a function that moved intact.
fn fingerprint(f: &Function, items: &[Item]) -> Option<String> {
    let keys: Vec<_> = items.iter().map(exact).collect();
    (!keys.is_empty()).then(|| format!("{:?} {}", f.returns, keys.join("\n")))
}

/// Items in `a` with no exact counterpart in `b`, counting repeats.
fn without(a: &[Item], b: &[Item]) -> Vec<Item> {
    let mut left: HashMap<String, usize> = HashMap::new();
    for i in b {
        *left.entry(exact(i)).or_default() += 1;
    }
    let mut out = vec![];
    for i in a {
        match left.get_mut(&exact(i)) {
            Some(n) if *n > 0 => *n -= 1,
            _ => out.push(i.clone()),
        }
    }
    out
}

/// Splits the difference into added, removed, and the same exit in a new state.
fn diff_items(base: &[Item], head: &[Item]) -> (Vec<Item>, Vec<Item>, Vec<ItemChange>) {
    let mut minus = without(base, head);
    let (mut plus, mut changed) = (vec![], vec![]);
    for item in without(head, base) {
        match minus.iter().position(|b| b.key == item.key) {
            Some(i) => changed.push(ItemChange {
                from: minus.remove(i).state,
                item,
            }),
            None => plus.push(item),
        }
    }
    // A fork only adds to what its arms' exits already say by appearing.
    plus.retain(|i| i.kind != ItemKind::Fork);
    minus.retain(|i| i.kind != ItemKind::Fork);
    (plus, minus, changed)
}

/// Does this change what can happen, rather than how it's annotated?
fn alters_behaviour(c: &ItemChange) -> bool {
    let typed = |s: &State| {
        s.conversion_ids
            .iter()
            .filter(|v| !v.starts_with("context(") && !v.starts_with("with_context("))
            .cloned()
            .collect::<Vec<_>>()
    };
    c.from.context != c.item.state.context
        || c.from.target != c.item.state.target
        || typed(&c.from) != typed(&c.item.state)
}

fn describe(r: &Returns) -> String {
    match r {
        Returns::Result { error } => format!("Result<_, {error}>"),
        Returns::Option => "Option<_>".into(),
        Returns::Other => "no Result".into(),
    }
}

/// Moves functions with the same key out of `added`/`removed` into `pairs`.
/// With `unique`, a key shared by several functions is too weak to trust.
fn match_up<'a>(
    pairs: &mut Vec<(&'a Function, &'a Function)>,
    added: &mut Vec<&'a Function>,
    removed: &mut Vec<&'a Function>,
    key: impl Fn(&Function) -> Option<String>,
    unique: bool,
) {
    let mut candidates: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, b) in removed.iter().enumerate().rev() {
        if let Some(k) = key(b) {
            candidates.entry(k).or_default().push(i);
        }
    }
    let mut claims: HashMap<String, usize> = HashMap::new();
    for k in added.iter().filter_map(|h| key(h)) {
        *claims.entry(k).or_default() += 1;
    }
    let mut taken = HashSet::new();
    added.retain(|h| {
        let Some(k) = key(h) else { return true };
        let Some(list) = candidates.get_mut(&k) else {
            return true;
        };
        if unique && (list.len() != 1 || claims[&k] != 1) {
            return true;
        }
        let Some(i) = list.pop() else { return true };
        pairs.push((removed[i], *h));
        taken.insert(i);
        false
    });
    let mut i = 0;
    removed.retain(|_| {
        i += 1;
        !taken.contains(&(i - 1))
    });
}

/// Pairs each head function with its base self: same id, then an identical
/// railway under a new id, then a unique shared name.
type Paired<'a> = (
    Vec<(&'a Function, &'a Function)>,
    Vec<&'a Function>,
    Vec<&'a Function>,
);

fn pair<'a>(base: &'a [Function], head: &'a [Function]) -> Paired<'a> {
    let by_id: HashMap<_, _> = base.iter().map(|f| (f.id.as_str(), f)).collect();
    let head_ids: HashSet<_> = head.iter().map(|f| f.id.as_str()).collect();
    let (mut pairs, mut added) = (vec![], vec![]);
    for h in head {
        match by_id.get(h.id.as_str()) {
            Some(b) => pairs.push((*b, h)),
            None => added.push(h),
        }
    }
    let mut removed: Vec<_> = base
        .iter()
        .filter(|f| !head_ids.contains(f.id.as_str()))
        .collect();
    match_up(
        &mut pairs,
        &mut added,
        &mut removed,
        |f| fingerprint(f, &items(f)),
        false,
    );
    match_up(
        &mut pairs,
        &mut added,
        &mut removed,
        |f| Some(f.name.clone()),
        true,
    );
    (pairs, added, removed)
}

fn severity(f: &Finding) -> &'static str {
    if f.rule != NO_PANIC && f.confidence == Confidence::Certain {
        "error"
    } else {
        "warning"
    }
}

/// Bad allow comments that weren't already there.
fn new_bad_allows(base: &Report, head: &Report) -> Vec<Finding> {
    let is_bad = |f: &&Finding| f.function.is_none();
    let mut left: Vec<_> = base
        .findings
        .iter()
        .filter(is_bad)
        .map(|f| (&f.file, &f.message))
        .collect();
    let mut out = vec![];
    for f in head.findings.iter().filter(is_bad) {
        match left.iter().position(|k| *k == (&f.file, &f.message)) {
            Some(i) => {
                left.swap_remove(i);
            }
            None => out.push(f.clone()),
        }
    }
    out
}

/// Compares two analysed refs. Pure, so everything but git is testable here.
pub fn compare(base: &Report, head: &Report, collapse_diagrams: bool) -> Review {
    let (pairs, added, removed) = pair(&base.functions, &head.functions);
    let mut functions = vec![];
    let mut moved = 0;
    for (b, h) in pairs {
        let (bi, mut hi) = (items(b), items(h));
        in_base_names(&mut hi, &renames(&b.locals, &h.locals));
        let (plus, minus, changed) = diff_items(&bi, &hi);
        let unchanged = plus.is_empty() && minus.is_empty() && changed.is_empty();
        // Order of what's on both sides, whatever else happened to it.
        let order = |list: &[Item]| {
            list.iter()
                .filter(|i| i.kind != ItemKind::Fork)
                .map(|i| i.key.clone())
                .collect::<Vec<_>>()
        };
        let exact_order = |list: &[Item]| list.iter().map(exact).collect::<Vec<_>>();
        let reordered = order(&without(&bi, &minus)) != order(&without(&hi, &plus))
            || (unchanged && exact_order(&bi) != exact_order(&hi));
        let returns = (b.returns != h.returns).then(|| ReturnsChange {
            from: describe(&b.returns),
            to: describe(&h.returns),
        });
        if unchanged && !reordered && returns.is_none() {
            moved += usize::from(b.id != h.id);
            continue;
        }
        functions.push(FnChange {
            id: h.id.clone(),
            status: Status::Changed,
            file: h.file.clone(),
            line: h.line,
            moved_from: (b.id != h.id).then(|| b.id.clone()),
            returns,
            added: plus,
            removed: minus,
            changed,
            reordered,
            diagram: Some(render(&h.name, &h.nodes)),
        });
    }
    let removed_functions = removed.iter().filter(|f| !items(f).is_empty()).count();
    let mut new_functions = 0;
    for f in added {
        let plus = items(f);
        if plus.iter().any(|i| i.finding.is_some()) {
            functions.push(FnChange {
                id: f.id.clone(),
                status: Status::Added,
                file: f.file.clone(),
                line: f.line,
                moved_from: None,
                returns: None,
                added: plus,
                removed: vec![],
                changed: vec![],
                reordered: false,
                diagram: Some(render(&f.name, &f.nodes)),
            });
        } else {
            new_functions += usize::from(!plus.is_empty());
        }
    }
    functions.sort_by(|a, b| (&a.file, a.line, &a.id).cmp(&(&b.file, b.line, &b.id)));

    let (mut new_findings, mut new_allows) = (vec![], vec![]);
    for finding in functions
        .iter()
        .flat_map(|f| &f.added)
        .filter_map(|i| i.finding.clone())
    {
        if finding.allowed.is_some() {
            new_allows.push(finding);
        } else {
            new_findings.push(NewFinding {
                severity: severity(&finding).into(),
                finding,
            });
        }
    }
    for c in functions.iter().flat_map(|f| &f.changed) {
        let Some(finding) = c.item.finding.clone() else {
            continue;
        };
        match (&c.from.allowed, &c.item.state.allowed) {
            (Some(_), None) => new_findings.push(NewFinding {
                severity: severity(&finding).into(),
                finding,
            }),
            (None, Some(_)) => new_allows.push(finding),
            _ => {}
        }
    }
    let bad = new_bad_allows(base, head);
    new_findings.extend(bad.into_iter().map(|finding| NewFinding {
        severity: "warning".into(),
        finding,
    }));

    let mut notes = vec![];
    for p in &head.parse_failures {
        if !base.parse_failures.iter().any(|b| b.file == p.file) {
            notes.push(format!(
                "{} could not be read, so its functions were not compared: {}",
                span(&p.file),
                span(&p.message)
            ));
        }
    }
    let (was, now) = (base.coverage.item_macro_try, head.coverage.item_macro_try);
    if was != now {
        notes.push(format!("`?` sites inside macros that wrap whole functions went from {was} to {now}. Those functions can't be analysed."));
    }

    // These coming or going decide what else runs, unlike a step doing so.
    let jumps = |f: &FnChange| {
        f.added
            .iter()
            .chain(&f.removed)
            .any(|i| matches!(i.kind, ItemKind::Jump | ItemKind::Diverge))
    };
    let altered = |f: &FnChange| {
        f.returns.is_some() || f.reordered || jumps(f) || f.changed.iter().any(alters_behaviour)
    };
    let risk = if new_findings.iter().any(|f| f.severity == "error") {
        Risk::High
    } else if !new_findings.is_empty() || !notes.is_empty() || functions.iter().any(altered) {
        Risk::Moderate
    } else {
        Risk::Low
    };
    let changed = !functions.is_empty() || !new_findings.is_empty() || !notes.is_empty();
    let mut review = Review {
        schema_version: crate::SCHEMA_VERSION,
        changed,
        risk,
        markdown: String::new(),
        new_findings,
        new_allows,
        functions,
        moved,
        new_functions,
        removed_functions,
        notes,
        coverage: head.coverage.clone(),
        unparsed_files: head.parse_failures.len(),
        support_files_skipped: head.support_files_skipped,
        summary: String::new(),
    };
    let c = &review.coverage;
    let seen = if changed {
        "changes seen"
    } else {
        "no failure-path change seen"
    };
    review.summary = format!(
        "{seen} in {} functions; not analysed: {} `?` in macros, {} `?` in macro-wrapped functions, {} unparsed files, {} test/bench/example files",
        c.functions,
        c.macro_try,
        c.item_macro_try,
        review.unparsed_files,
        review.support_files_skipped
    );
    if changed {
        review.markdown = fitted_markdown(&review, collapse_diagrams);
    }
    review
}

/// GitHub refuses a comment over 65,536 characters, which would lose the review
/// altogether. Diagrams go first, then the tail of the list.
fn fitted_markdown(r: &Review, collapse: bool) -> String {
    let full = markdown(r, collapse, true);
    if full.len() <= MAX_COMMENT {
        return full;
    }
    let plain = markdown(r, collapse, false);
    if plain.len() <= MAX_COMMENT {
        return plain;
    }
    let note = "\n…too long for a comment. The full list is in the JSON output.\n";
    let room = plain.floor_char_boundary(MAX_COMMENT - note.len());
    let end = plain[..room].rfind('\n').unwrap_or(room);
    format!("{}{note}", &plain[..end])
}

/// An inline code span that holds any text. Everything here comes from the
/// code under review, and a PR author controls that: unescaped, a label or
/// allow reason could add mentions, links or images to the comment.
pub(crate) fn span(s: &str) -> String {
    let clean: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_SPAN)
        .collect();
    let mut longest = (0, 0);
    for c in clean.chars() {
        longest.0 = if c == '`' { longest.0 + 1 } else { 0 };
        longest.1 = longest.1.max(longest.0);
    }
    let fence = "`".repeat(longest.1 + 1);
    let pad = if clean.starts_with('`') || clean.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{clean}{pad}{fence}")
}

/// A finding message as one span: its own backticks are only decoration.
fn message_span(m: &str) -> String {
    span(&m.replace('`', ""))
}

fn item_line(i: &Item) -> String {
    let what = match i.kind {
        ItemKind::Step => format!("step {}", span(&i.label)),
        ItemKind::Fail => format!("failure exit {}", span(&i.label)),
        ItemKind::Panic => format!("panic path {}", span(&i.label)),
        ItemKind::Swallow => format!("dropped error from {}", span(&i.label)),
        ItemKind::Return => "early return".to_string(),
        ItemKind::Jump => span(&i.label),
        ItemKind::Fork => span(&i.label),
        ItemKind::Diverge => format!("loop that never finishes {}", span(&i.label)),
        ItemKind::Convert => format!("error conversion on {}", span(&i.label)),
        ItemKind::MacroTry => i.label.clone(),
    };
    match &i.error {
        Some(e) => format!("{what} → {}", span(e)),
        None => what,
    }
}

fn change_lines(c: &ItemChange) -> Vec<String> {
    let (from, to) = (&c.from, &c.item.state);
    let at = |s: &str| {
        if s.is_empty() {
            "top level".to_string()
        } else {
            s.to_string()
        }
    };
    let list = |v: &[String]| {
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(".")
        }
    };
    let mut out = vec![];
    if from.context != to.context {
        out.push(format!(
            "moved from {} to {}",
            span(&at(&from.context)),
            span(&at(&to.context))
        ));
    }
    if from.conversion_ids != to.conversion_ids {
        out.push(format!(
            "error conversion {} → {}",
            span(&list(&from.conversions)),
            span(&list(&to.conversions))
        ));
    }
    if from.target != to.target {
        out.push("now lands somewhere else".to_string());
    }
    if from.arms != to.arms {
        out.push(format!(
            "arms {} → {}",
            span(&from.arms.join(" | ")),
            span(&to.arms.join(" | "))
        ));
    }
    if from.allowed != to.allowed {
        out.push(match &to.allowed {
            Some(reason) => format!("allow comment: {}", span(reason)),
            None => "allow comment removed".to_string(),
        });
    }
    let what = item_line(&c.item);
    out.into_iter()
        .map(|change| format!("{what}: {change}"))
        .collect()
}

fn markdown(r: &Review, collapse: bool, diagrams: bool) -> String {
    let risk = match r.risk {
        Risk::Low => "low",
        Risk::Moderate => "moderate",
        Risk::High => "high",
    };
    let mut s = format!("{MARKER}\n## Error path review: {risk} risk\n\n");
    let n = r.functions.len();
    let _ = writeln!(
        s,
        "{n} function{} changed, {} new finding{}.\n",
        if n == 1 { "" } else { "s" },
        r.new_findings.len(),
        if r.new_findings.len() == 1 { "" } else { "s" }
    );
    for note in &r.notes {
        let _ = writeln!(s, "> {note}\n");
    }
    if !r.new_findings.is_empty() {
        s += "### New findings\n\n";
        for f in &r.new_findings {
            let f = &f.finding;
            let possibly = if f.confidence == Confidence::Possible {
                " (possible)"
            } else {
                ""
            };
            let _ = writeln!(
                s,
                "- `{}` {}{possibly} at {}",
                f.rule,
                message_span(&f.message),
                span(&format!("{}:{}", f.file, f.line))
            );
        }
        s += "\n";
    }
    if !r.new_allows.is_empty() {
        s += "### New allow comments\n\n";
        for f in &r.new_allows {
            let reason = span(f.allowed.as_deref().unwrap_or(""));
            let _ = writeln!(
                s,
                "- `{}` {} at {}: {reason}",
                f.rule,
                message_span(&f.message),
                span(&format!("{}:{}", f.file, f.line))
            );
        }
        s += "\n";
    }
    if !r.functions.is_empty() {
        s += "### Changed error paths\n\n";
    }
    for (i, f) in r.functions.iter().take(MAX_FUNCTIONS).enumerate() {
        let status = match f.status {
            Status::Changed => "",
            Status::Added => " (new)",
        };
        let _ = writeln!(
            s,
            "#### {}{status}\n\n{}\n",
            span(&f.id),
            span(&format!("{}:{}", f.file, f.line))
        );
        if let Some(from) = &f.moved_from {
            let _ = writeln!(s, "- moved from {}", span(from));
        }
        if let Some(c) = &f.returns {
            let _ = writeln!(s, "- returns {} → {}", span(&c.from), span(&c.to));
        }
        for a in &f.added {
            let _ = writeln!(s, "- added {}", item_line(a));
        }
        for a in &f.removed {
            let _ = writeln!(s, "- removed {}", item_line(a));
        }
        for line in f.changed.iter().flat_map(change_lines) {
            let _ = writeln!(s, "- {line}");
        }
        if f.reordered {
            s += "- exits in a different order\n";
        }
        s += "\n";
        if let Some(d) = f.diagram.as_ref().filter(|_| diagrams && i < MAX_DIAGRAMS) {
            let body = format!("```mermaid\n{d}```\n");
            if collapse {
                let _ = writeln!(
                    s,
                    "<details><summary>Error path diagram</summary>\n\n{body}\n</details>\n"
                );
            } else {
                let _ = writeln!(s, "{body}");
            }
        }
    }
    if n > MAX_FUNCTIONS {
        let _ = writeln!(
            s,
            "…and {} more. The full list is in the JSON output.\n",
            n - MAX_FUNCTIONS
        );
    }
    let c = &r.coverage;
    let _ = writeln!(
        s,
        "---\n<sub>Syntax-only analysis. In this ref, {} of {} `?` steps name their error from the callee, and {} `?` inside macros weren't analysed. {} files under `tests/`, `benches/` or `examples/` were skipped. Implicit `From` conversions and macro expansions aren't seen.</sub>",
        c.error_from_callee,
        c.steps,
        c.macro_try + c.item_macro_try,
        r.support_files_skipped
    );
    s
}

fn git(top: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(top)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Under a package's `tests/`, `benches/` or `examples/`, which never ship.
/// Below `src` those names are ordinary modules, so `src/tests/pay.rs` counts.
/// `#[cfg(test)]` code is skipped later, by attribute.
pub(crate) fn is_support_code(path: &str) -> bool {
    path.split('/')
        .take_while(|p| *p != "src")
        .any(|p| ["tests", "benches", "examples"].contains(&p))
}

/// Analyses every Rust file at a commit, reading from git, not the work tree.
// Unchanged files are analysed too, since a `mod tests;` in an unchanged parent
// decides what counts as test code. `git cat-file --batch` would cut the process
// count on big repos.
fn report_at(top: &Path, commit: &str, paths: &[&str]) -> Result<Report, String> {
    let mut args = vec!["ls-tree", "-r", "-z", commit, "--"];
    args.extend(paths);
    let listing = git(top, &args)?;
    let mut report = Report::new();
    for entry in listing.split(|b| *b == 0) {
        // `<mode> <type> <oid>\t<path>`. Paths needn't be UTF-8 and git output
        // reaches us lossily, so blobs are read by oid, never by name.
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let [mode, kind, oid] = meta.split(' ').collect::<Vec<_>>()[..] else {
            continue;
        };
        if kind != "blob" || mode == "120000" || !path.ends_with(".rs") {
            continue; // submodules and symlinks hold no source here
        }
        if is_support_code(path) {
            report.support_files_skipped += 1;
            continue;
        }
        let src = git(top, &["cat-file", "blob", oid])
            .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()));
        match src {
            Ok(src) => report.add_source(path, &module_of(Path::new(path)), &src),
            Err(message) => report.parse_failures.push(crate::ParseFailure {
                file: path.into(),
                message,
            }),
        }
    }
    Ok(report.finish())
}

fn resolve(top: &Path, rev: &str) -> Result<String, String> {
    let spec = format!("{rev}^{{commit}}");
    let out = git(top, &["rev-parse", "--verify", "--end-of-options", &spec])?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

pub struct Options<'a> {
    pub base: &'a str,
    pub head: &'a str,
    /// Repo-relative paths to limit the review to; empty means everything.
    pub paths: &'a [&'a str],
    pub collapse_diagrams: bool,
}

pub fn run(o: &Options) -> Result<Review, String> {
    let top = git(Path::new("."), &["rev-parse", "--show-toplevel"])?;
    let top = String::from_utf8_lossy(&top).trim().to_string();
    let top = Path::new(&top);
    let (base, head) = (resolve(top, o.base)?, resolve(top, o.head)?);
    // From where the branch left, or a base that moved on would be read as the
    // branch undoing everything merged since.
    let start = match git(top, &["merge-base", &base, &head]) {
        Ok(out) => String::from_utf8_lossy(&out).trim().to_string(),
        Err(_) => {
            eprintln!(
                "no merge base with {}, so comparing against it directly (a shallow clone?)",
                o.base
            );
            base
        }
    };
    Ok(compare(
        &report_at(top, &start, o.paths)?,
        &report_at(top, &head, o.paths)?,
        o.collapse_diagrams,
    ))
}
