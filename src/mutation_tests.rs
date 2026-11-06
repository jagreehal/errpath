//! Mutation test for the review: every edit to a failure path must be
//! reported, and every edit that leaves failure paths alone must stay silent.
use crate::review::{Review, compare};
use crate::{Locals, Report, is_test, module_of};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Block, Expr, Stmt};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Expect {
    Reported,
    Silent,
}
use Expect::{Reported, Silent};

struct Mutant {
    op: &'static str,
    expect: Expect,
    edits: Vec<(Range<usize>, String)>,
}

fn range(t: &impl Spanned) -> Range<usize> {
    t.span().byte_range()
}

/// Does this subtree have a way off the happy path?
#[derive(Default)]
struct Exits(bool, /* count `break`/`continue` too */ bool);

impl<'ast> Visit<'ast> for Exits {
    fn visit_item(&mut self, _: &'ast syn::Item) {}
    fn visit_expr_try(&mut self, _: &'ast syn::ExprTry) {
        self.0 = true;
    }
    fn visit_expr_return(&mut self, _: &'ast syn::ExprReturn) {
        self.0 = true;
    }
    fn visit_expr_break(&mut self, _: &'ast syn::ExprBreak) {
        self.0 |= self.1;
    }
    fn visit_expr_continue(&mut self, _: &'ast syn::ExprContinue) {
        self.0 |= self.1;
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        self.0 |= m.method == "unwrap" || m.method == "expect";
        visit::visit_expr_method_call(self, m);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.0 |= m.path.segments.last().is_some_and(|s| {
            ["panic", "todo", "unreachable", "unimplemented"]
                .contains(&s.ident.to_string().as_str())
        });
    }
}

fn exits_expr(e: &Expr) -> bool {
    let mut x = Exits::default();
    x.visit_expr(e);
    x.0
}

fn exits_block(b: &Block) -> bool {
    let mut x = Exits::default();
    x.visit_block(b);
    x.0
}

/// Does this subtree jump, and would that matter? A jump counts inside a loop
/// with a real exit, and a labelled one counts anywhere: it can leave for one.
#[derive(Default)]
struct Jumps {
    any: bool,
    labelled: bool,
}

impl<'ast> Visit<'ast> for Jumps {
    fn visit_item(&mut self, _: &'ast syn::Item) {}
    fn visit_expr_break(&mut self, b: &'ast syn::ExprBreak) {
        self.any = true;
        self.labelled |= b.label.is_some();
    }
    fn visit_expr_continue(&mut self, c: &'ast syn::ExprContinue) {
        self.any = true;
        self.labelled |= c.label.is_some();
    }
}

fn exits_stmt(s: &Stmt) -> bool {
    let mut x = Exits(false, true);
    x.visit_stmt(s);
    x.0
}

/// Jumps alone don't count: a loop of only jumps isn't a failure path.
fn real_exits(s: &Stmt) -> bool {
    let mut x = Exits::default();
    x.visit_stmt(s);
    x.0
}

#[derive(Default)]
struct Sites {
    out: Vec<Mutant>,
    /// Inside a condition that decides a failure path.
    cond: usize,
    /// The innermost loop has a real exit, so a jump changes what runs.
    fallible_loop: usize,
    /// Some enclosing loop has a real exit, so a labelled jump can matter.
    outer_fallible: bool,
    /// The label of each enclosing loop, innermost last.
    loop_tags: Vec<Option<String>>,
    /// The labels of enclosing blocks with a real exit, innermost last.
    block_tags: Vec<String>,
    /// Inside a conversion or a deciding condition, where a literal counts.
    literal_counts: usize,
}

impl Sites {
    fn add(&mut self, op: &'static str, expect: Expect, edits: Vec<(Range<usize>, String)>) {
        self.out.push(Mutant { op, expect, edits });
    }

    fn one(&mut self, op: &'static str, at: Range<usize>, to: impl Into<String>) {
        self.add(op, Reported, vec![(at, to.into())]);
    }

    fn decided_by(&mut self, c: &Expr, src: &str) {
        if !matches!(c, Expr::Let(_)) {
            let r = range(c);
            self.one(
                "negate-condition",
                r.clone(),
                format!("!({})", &src[r.clone()]),
            );
            self.add(
                "comment-in-condition",
                Silent,
                vec![(r.start..r.start, "/* why */ ".into())],
            );
        }
    }

    fn return_value(&mut self, e: &Expr) {
        match e {
            Expr::Call(c) => {
                let Expr::Path(p) = &*c.func else { return };
                let name = p
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default();
                let r = range(&c.func);
                match name.as_str() {
                    "Ok" => self.one("ok-to-err", r, "Err"),
                    "Err" => self.one("err-to-ok", r, "Ok"),
                    "Some" => self.one("some-to-none", range(e), "None"),
                    _ => {}
                }
            }
            Expr::Path(p) if p.path.is_ident("None") => {
                self.one("none-to-some", range(e), "Some(Default::default())")
            }
            Expr::If(i) => {
                if let Some(Stmt::Expr(t, None)) = i.then_branch.stmts.last() {
                    self.return_value(t);
                }
                if let Some((_, other)) = &i.else_branch {
                    self.return_value(other);
                }
            }
            Expr::Block(b) => {
                if let Some(Stmt::Expr(t, None)) = b.block.stmts.last() {
                    self.return_value(t);
                }
            }
            _ => {}
        }
    }

    fn walk(&mut self, file: &syn::File, src: &str) {
        Walk { sites: self, src }.visit_file(file);
    }
}

struct Walk<'s, 'a> {
    sites: &'s mut Sites,
    src: &'a str,
}

impl Walk<'_, '_> {
    fn body(&mut self, block: &Block) {
        if let Some(Stmt::Expr(t, None)) = block.stmts.last() {
            self.sites.return_value(t);
        }
    }

    fn in_loop(&mut self, tag: Option<&syn::Label>, body: &Block, f: impl FnOnce(&mut Self)) {
        self.sites
            .loop_tags
            .push(tag.map(|t| t.name.ident.to_string()));
        // A jump acts on its innermost loop, so only that loop counts.
        let saved = (self.sites.fallible_loop, self.sites.outer_fallible);
        self.sites.fallible_loop = usize::from(body.stmts.iter().any(real_exits));
        self.sites.outer_fallible |= self.sites.fallible_loop > 0;
        f(self);
        (self.sites.fallible_loop, self.sites.outer_fallible) = saved;
        self.sites.loop_tags.pop();
    }

    /// A labelled jump aimed past its innermost loop can lose the label.
    fn retarget(&mut self, label: Option<&syn::Lifetime>, at: Range<usize>, word: &str) {
        let inner = self.sites.loop_tags.last().cloned().flatten();
        if self.sites.outer_fallible && label.is_some_and(|l| Some(l.ident.to_string()) != inner) {
            self.sites.one("retarget-jump", at, word);
        }
    }
}

impl<'ast> Visit<'ast> for Walk<'_, '_> {
    fn visit_macro(&mut self, _: &'ast syn::Macro) {} // syntax-only: a stated limit

    // Evaluated at compile time, so no runtime failure path lives here.
    fn visit_item_static(&mut self, _: &'ast syn::ItemStatic) {}
    fn visit_item_const(&mut self, _: &'ast syn::ItemConst) {}

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !is_test(&m.attrs) {
            visit::visit_item_mod(self, m);
        }
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if !is_test(&i.attrs) {
            visit::visit_item_impl(self, i);
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        self.body(&f.block);
        self.quiet_edits(&f.block);
        visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        self.body(&f.block);
        self.quiet_edits(&f.block);
        visit::visit_impl_item_fn(self, f);
    }

    fn visit_signature(&mut self, s: &'ast syn::Signature) {
        if let syn::ReturnType::Type(_, ty) = &s.output
            && let syn::Type::Path(p) = &**ty
            && let Some(seg) = p.path.segments.last()
            && seg.ident == "Result"
            && let syn::PathArguments::AngleBracketed(a) = &seg.arguments
            && let Some(e) = a.args.iter().nth(1)
        {
            self.sites
                .one("change-declared-error", range(e), "MutatedError");
        }
    }

    fn visit_block(&mut self, b: &'ast Block) {
        for pair in b.stmts.windows(2) {
            let (x, y) = (range(&pair[0]), range(&pair[1]));
            if real_exits(&pair[0])
                && real_exits(&pair[1])
                && self.src[x.clone()] != self.src[y.clone()]
            {
                let (tx, ty) = (
                    self.src[x.clone()].to_string(),
                    self.src[y.clone()].to_string(),
                );
                self.sites
                    .add("swap-statements", Reported, vec![(x, ty), (y, tx)]);
            }
        }
        let last = b.stmts.len().saturating_sub(1);
        for (i, s) in b.stmts.iter().enumerate() {
            // A jump only matters in a loop with something fallible to skip.
            let matters = real_exits(s) || exits_stmt(s) && self.sites.fallible_loop > 0;
            if matters && i != last {
                self.sites.one("delete-statement", range(s), "");
            }
            if real_exits(s) {
                let at = range(s).start;
                self.sites.one("insert-endless-loop", at..at, "loop {} ");
            }
        }
        visit::visit_block(self, b);
    }

    fn visit_expr_if(&mut self, i: &'ast syn::ExprIf) {
        let else_exits = i.else_branch.as_ref().is_some_and(|(_, e)| exits_expr(e));
        let mut jumps = Jumps::default();
        jumps.visit_block(&i.then_branch);
        let jump_matters = jumps.labelled && self.sites.outer_fallible
            || jumps.any && self.sites.fallible_loop > 0;
        if exits_block(&i.then_branch) || else_exits || jump_matters {
            self.sites.decided_by(&i.cond, self.src);
            self.sites.cond += 1;
            self.sites.literal_counts += 1;
            self.visit_expr(&i.cond);
            self.sites.literal_counts -= 1;
            self.sites.cond -= 1;
        } else {
            self.visit_expr(&i.cond);
        }
        if let Some((token, e)) = &i.else_branch
            && else_exits
        {
            self.sites.one(
                "delete-else",
                token.span().byte_range().start..range(&**e).end,
                "",
            );
        }
        self.visit_block(&i.then_branch);
        if let Some((_, e)) = &i.else_branch {
            self.visit_expr(e);
        }
    }

    fn visit_lit_str(&mut self, l: &'ast syn::LitStr) {
        let (old, new) = (l.value(), l.value().replace(' ', ""));
        if self.sites.literal_counts > 0 && old != new && !old.contains(['\\', '"']) {
            self.sites
                .one("tighten-string", range(l), format!("{new:?}"));
        }
    }

    fn visit_expr_tuple(&mut self, t: &'ast syn::ExprTuple) {
        if self.sites.literal_counts > 0 && t.elems.len() == 1 {
            let at = range(t);
            let inner = self.src[range(&t.elems)].trim_end_matches(',').to_string();
            self.sites.one("tuple-to-value", at, format!("({inner})"));
        }
        visit::visit_expr_tuple(self, t);
    }

    fn visit_lit_char(&mut self, l: &'ast syn::LitChar) {
        if self.sites.literal_counts > 0 {
            let to = if l.value() == 'z' { 'y' } else { 'z' };
            self.sites.one("change-char", range(l), format!("{to:?}"));
        }
    }

    fn visit_expr_block(&mut self, b: &'ast syn::ExprBlock) {
        let tag = b
            .label
            .as_ref()
            .filter(|_| b.block.stmts.iter().any(real_exits));
        if let Some(t) = tag {
            self.sites.block_tags.push(t.name.ident.to_string());
        }
        visit::visit_expr_block(self, b);
        if tag.is_some() {
            self.sites.block_tags.pop();
        }
    }

    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        use syn::BinOp::{And, Eq, Ge, Gt, Le, Lt, Ne, Or};
        let swapped = match b.op {
            And(_) if exits_expr(&b.right) => Some("||"),
            Or(_) if exits_expr(&b.right) => Some("&&"),
            Gt(_) if self.sites.cond > 0 => Some(">="),
            Lt(_) if self.sites.cond > 0 => Some("<="),
            Ge(_) if self.sites.cond > 0 => Some(">"),
            Le(_) if self.sites.cond > 0 => Some("<"),
            Eq(_) if self.sites.cond > 0 => Some("!="),
            Ne(_) if self.sites.cond > 0 => Some("=="),
            _ => None,
        };
        if let Some(to) = swapped {
            self.sites.one("swap-operator", range(&b.op), to);
        }
        visit::visit_expr_binary(self, b);
    }

    fn visit_expr_try(&mut self, t: &'ast syn::ExprTry) {
        if let Expr::MethodCall(m) = &*t.expr
            && !matches!(
                m.method.to_string().as_str(),
                "map_err" | "context" | "with_context" | "ok_or" | "ok_or_else"
            )
        {
            self.sites
                .one("change-method", range(&m.method), "mutated_method");
        }
        let q = range(&t.question_token);
        self.sites.one("remove-try", q.clone(), "");
        self.sites.one("try-to-unwrap", q, ".unwrap()");
        visit::visit_expr_try(self, t);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let name = m.method.to_string();
        let call = range(&m.dot_token).start..range(m).end;
        match name.as_str() {
            "unwrap" | "expect" => self.sites.one("unwrap-to-try", call, "?"),
            "map_err" | "context" | "with_context" | "ok_or" | "ok_or_else" => {
                self.sites.one("drop-conversion", call, "");
                if !m.args.is_empty() {
                    let args = range(&m.args);
                    self.sites.one("change-conversion", args.clone(), "mutated");
                    self.sites.add(
                        "comment-in-conversion",
                        Silent,
                        vec![(args.start..args.start, "/* why */ ".into())],
                    );
                    self.sites.add(
                        "trailing-comma",
                        Silent,
                        vec![(args.end..args.end, ",".into())],
                    );
                    if let Some(Expr::Closure(c)) = m.args.first() {
                        let mut params = Locals::default();
                        c.inputs.iter().for_each(|p| params.visit_pat(p));
                        for name in params.0 {
                            let edits = rename_edits(self.src, range(c), &name);
                            self.sites.add("rename-closure-parameter", Silent, edits);
                        }
                    }
                }
            }
            "unwrap_or_default" | "unwrap_or" => self.sites.one("swallow-to-try", call, "?"),
            _ => {}
        }
        let adapter = matches!(
            name.as_str(),
            "map_err" | "context" | "with_context" | "ok_or" | "ok_or_else"
        );
        self.sites.literal_counts += usize::from(adapter);
        visit::visit_expr_method_call(self, m);
        self.sites.literal_counts -= usize::from(adapter);
    }

    fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
        if m.arms.iter().any(|a| {
            exits_expr(&a.body) || {
                let mut x = Exits::default();
                x.visit_pat(&a.pat);
                x.0
            }
        }) {
            for a in &m.arms {
                if !matches!(a.pat, syn::Pat::Wild(_)) {
                    self.sites
                        .one("change-pattern", range(&a.pat), "Mutated(_)");
                }
            }
            for pair in m.arms.windows(2) {
                let (x, y) = (range(&pair[0].pat), range(&pair[1].pat));
                let (tx, ty) = (
                    self.src[x.clone()].to_string(),
                    self.src[y.clone()].to_string(),
                );
                self.sites
                    .add("swap-arms", Reported, vec![(x, ty), (y, tx)]);
            }
        }
        visit::visit_expr_match(self, m);
    }

    fn visit_local(&mut self, l: &'ast syn::Local) {
        if let Some(init) = &l.init {
            if init.diverge.is_some() {
                self.sites
                    .one("change-let-else-pattern", range(&l.pat), "Mutated(_)");
                self.sites
                    .one("change-let-else-value", range(&*init.expr), "mutated()");
            }
            if matches!(l.pat, syn::Pat::Wild(_)) {
                let text = self.src[range(&*init.expr)].to_string();
                self.sites.one("drop-to-try", range(l), format!("{text}?;"));
            }
        }
        visit::visit_local(self, l);
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        if f.body.stmts.iter().any(real_exits) {
            self.sites
                .one("change-loop-source", range(&*f.expr), "mutated()");
        }
        self.visit_expr(&f.expr);
        self.in_loop(f.label.as_ref(), &f.body, |w| w.visit_block(&f.body));
    }

    fn visit_expr_while(&mut self, l: &'ast syn::ExprWhile) {
        if l.body.stmts.iter().any(real_exits) {
            self.sites.decided_by(&l.cond, self.src);
        }
        self.visit_expr(&l.cond);
        self.in_loop(l.label.as_ref(), &l.body, |w| w.visit_block(&l.body));
    }

    fn visit_expr_loop(&mut self, l: &'ast syn::ExprLoop) {
        self.in_loop(l.label.as_ref(), &l.body, |w| w.visit_block(&l.body));
    }

    fn visit_expr_break(&mut self, b: &'ast syn::ExprBreak) {
        if (self.sites.fallible_loop > 0 || b.label.is_some() && self.sites.outer_fallible)
            && b.expr.is_none()
        {
            self.sites.one("swap-jump", range(b), "continue");
        }
        if b.expr.is_none() {
            self.retarget(b.label.as_ref(), range(b), "break");
        }
        // Aim a jump at the other block.
        if let (Some(l), [.., outer, inner]) = (&b.label, &self.sites.block_tags[..]) {
            let other = if l.ident == inner.as_str() {
                outer
            } else {
                inner
            };
            let to = format!("break '{other}");
            self.sites.one("retarget-block", range(b), to);
        }
        visit::visit_expr_break(self, b);
    }

    fn visit_expr_continue(&mut self, c: &'ast syn::ExprContinue) {
        if self.sites.fallible_loop > 0 || c.label.is_some() && self.sites.outer_fallible {
            self.sites.one("swap-jump", range(c), "break");
        }
        self.retarget(c.label.as_ref(), range(c), "continue");
    }

    fn visit_expr_return(&mut self, r: &'ast syn::ExprReturn) {
        if let Some(e) = &r.expr {
            self.sites.return_value(e);
        }
        visit::visit_expr_return(self, r);
    }
}

impl Walk<'_, '_> {
    /// Edits a reviewer shouldn't hear about.
    fn quiet_edits(&mut self, block: &Block) {
        let body = range(block);
        let first = body.start + 1;
        self.sites.add(
            "add-comment",
            Silent,
            vec![(first..first, " // a note\n".into())],
        );
        self.sites.add(
            "add-plain-statement",
            Silent,
            vec![(first..first, " let unused_value = 1 + 1;\n".into())],
        );
        // What rustfmt does to a long line. String contents must stay as they are.
        let text = &self.src[body.clone()];
        if !text.contains('"') {
            let edits = text
                .match_indices(", ")
                .map(|(i, _)| {
                    (
                        body.start + i..body.start + i + 2,
                        ",\n        ".to_string(),
                    )
                })
                .collect();
            self.sites.add("reflow-lines", Silent, edits);
        }
        let mut locals = Locals::default();
        locals.visit_block(block);
        for name in locals.0 {
            let edits = rename_edits(self.src, body.clone(), &name);
            self.sites.add("rename-local", Silent, edits);
        }
    }
}

/// Edits renaming every reference to `name` inside `within`.
fn rename_edits(src: &str, within: Range<usize>, name: &str) -> Vec<(Range<usize>, String)> {
    let text = &src[within.clone()];
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let strings = string_spans(text);
    let mut edits = vec![];
    for (i, _) in text.match_indices(name) {
        if strings.iter().any(|s| s.contains(&i)) {
            continue;
        }
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        // `self.name`, `name::Type` and `Type::name` aren't the local.
        let other = text[..i].ends_with('.')
            || text[..i].ends_with("::")
            || text[i + name.len()..].starts_with("::");
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) && !other {
            edits.push((
                within.start + i..within.start + i + name.len(),
                format!("{name}_renamed"),
            ));
        }
    }
    edits
}

/// Where the string literals in `text` are.
fn string_spans(text: &str) -> Vec<Range<usize>> {
    let (mut out, mut start, mut escaped) = (vec![], None, false);
    for (i, c) in text.char_indices() {
        match (c, start) {
            ('"', None) => start = Some(i),
            ('"', Some(s)) if !escaped => {
                out.push(s..i);
                start = None;
            }
            _ => {}
        }
        escaped = c == '\\' && !escaped;
    }
    out
}

fn apply(src: &str, m: &Mutant) -> String {
    let mut edits = m.edits.clone();
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    let mut out = src.to_string();
    for (r, text) in edits {
        out.replace_range(r, &text);
    }
    out
}

fn analyse(file: &str, src: &str) -> Report {
    let mut r = Report::new();
    r.add_source(file, &module_of(Path::new(file)), src);
    r.finish()
}

fn review(base: &Report, file: &str, mutated: &str) -> Option<Review> {
    let head = analyse(file, mutated);
    head.parse_failures
        .is_empty()
        .then(|| compare(base, &head, false))
}

struct Survivor {
    op: &'static str,
    expect: Expect,
    line: usize,
    snippet: String,
    /// What the review said about an edit it should have stayed silent on.
    said: String,
}

#[derive(Default)]
struct Score {
    tried: BTreeMap<&'static str, usize>,
    survivors: Vec<Survivor>,
}

fn run(file: &str, src: &str, score: &mut Score, keep: impl Fn(usize) -> bool) {
    let parsed = syn::parse_file(src).unwrap();
    let mut sites = Sites::default();
    sites.walk(&parsed, src);
    let base = analyse(file, src);
    for (_, m) in sites.out.iter().enumerate().filter(|(i, _)| keep(*i)) {
        let mutated = apply(src, m);
        if mutated == src {
            continue;
        }
        let Some(r) = review(&base, file, &mutated) else {
            continue;
        }; // not valid Rust
        *score.tried.entry(m.op).or_default() += 1;
        let survived = match m.expect {
            Reported => !r.changed,
            Silent => r.changed,
        };
        if survived {
            let at = m.edits[0].0.start;
            let line = src[..at].matches('\n').count() + 1;
            let snippet = src.lines().nth(line - 1).unwrap_or("").trim().to_string();
            let said = r
                .markdown
                .lines()
                .find(|l| l.starts_with("- "))
                .unwrap_or("")
                .to_string();
            score.survivors.push(Survivor {
                op: m.op,
                expect: m.expect,
                line,
                snippet,
                said,
            });
        }
    }
}

fn describe(s: &Survivor) -> String {
    let what = if s.expect == Reported {
        "missed"
    } else {
        "noise "
    };
    format!(
        "{what} {:<24} line {:<4} {} {}",
        s.op, s.line, s.snippet, s.said
    )
}

/// Fails on any mutant that gets the wrong answer, unless it's a stated limit.
#[test]
fn every_failure_path_edit_is_reported_and_quiet_edits_are_not() {
    let mut score = Score::default();
    let corpus = [
        (
            "tests/corpus/control_flow.rs",
            include_str!("../tests/corpus/control_flow.rs"),
        ),
        (
            "examples/transfer.rs",
            include_str!("../examples/transfer.rs"),
        ),
        (
            "examples/checkout.rs",
            include_str!("../examples/checkout.rs"),
        ),
        (
            "examples/wrappers.rs",
            include_str!("../examples/wrappers.rs"),
        ),
        ("examples/silent.rs", include_str!("../examples/silent.rs")),
    ];
    for (file, src) in corpus {
        run(file, src, &mut score, |_| true);
    }
    let total: usize = score.tried.values().sum();
    let found: Vec<_> = score.survivors.iter().map(describe).collect();
    assert!(
        total > 300,
        "only {total} mutants: the generator has gone quiet"
    );
    assert!(
        found.is_empty(),
        "{} of {total} mutants got the wrong answer:\n{}",
        found.len(),
        found.join("\n")
    );
}

/// Scores real code: `MUTATE_PATH=<dir> cargo test real_code -- --ignored --nocapture`.
#[test]
#[ignore = "needs MUTATE_PATH"]
fn real_code() {
    let root = std::env::var("MUTATE_PATH").expect("MUTATE_PATH");
    let mut files = vec![];
    crate::rust_files(Path::new(&root), &mut files).unwrap();
    let every: usize = std::env::var("MUTATE_EVERY")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(25);
    let mut score = Score::default();
    for path in files {
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        if syn::parse_file(&src).is_ok() {
            run(&path.display().to_string(), &src, &mut score, |i| {
                i % every == 0
            });
        }
    }
    for (op, tried) in &score.tried {
        let wrong: Vec<_> = score.survivors.iter().filter(|s| s.op == *op).collect();
        println!("{op:<26} {:>5} tried {:>4} wrong", tried, wrong.len());
    }
    for s in score
        .survivors
        .iter()
        .step_by((score.survivors.len() / 40).max(1))
    {
        println!("{}", describe(s));
    }
}
