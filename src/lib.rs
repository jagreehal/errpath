//! Railway analysis for plain Rust: every `expr?` is a step that can leave the
//! happy path, `if`/`match` containing one becomes a fork, `return Err(..)` and
//! a tail `Err(..)` are failure exits, closures and async blocks with fallible
//! bodies become labelled boxes, and `unwrap`-style panics and dropped errors
//! are recorded as findings. Syntax only (syn).
use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Block, Expr, ReturnType, Signature, Stmt, Token, Type};

pub mod review;

/// Bump when a field changes meaning or goes away.
pub const SCHEMA_VERSION: u32 = 1;

pub const NO_PANIC: &str = "no-panic";
pub const NO_SWALLOW: &str = "no-swallow";
pub const BAD_ALLOW: &str = "bad-allow";
const ALLOWABLE: &[&str] = &[NO_PANIC, NO_SWALLOW];

#[derive(Debug, PartialEq, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSource {
    /// The called function's own signature, found in the same crate.
    Callee,
    /// The enclosing function's error type: what `?` converts into.
    Enclosing,
    Unknown,
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// The callee is known to return `Result`.
    Certain,
    /// The callee's return type can't be seen from syntax.
    Possible,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    Step {
        name: String,
        error: Option<String>,
        source: ErrorSource,
        /// Adapters between the call and the `?`, innermost first:
        /// `map_err(Top::A)`. These decide the error the caller receives.
        conversions: Vec<String>,
        line: usize,
    },
    Branch {
        label: String,
        arms: Vec<Arm>,
    },
    /// A body that may run many times. `tag` is its `'label`, `head` is what
    /// a `while` condition does on every iteration, and `can_skip` says it can
    /// finish without a `break`: false only for `loop`.
    Loop {
        label: String,
        tag: Option<String>,
        can_skip: bool,
        head: Vec<Node>,
        body: Vec<Node>,
    },
    /// A closure or async body, e.g. `gen_ai(opts, |ctx| async move { .. })`.
    Scope {
        label: String,
        body: Vec<Node>,
    },
    Fail {
        error: String,
        line: usize,
    },
    Succeed,
    /// A `'label: { .. }` block, which `break 'label` leaves early.
    Block {
        tag: String,
        body: Vec<Node>,
    },
    /// `break`, `continue 'outer`: it decides what runs next in a loop.
    Jump {
        name: String,
        line: usize,
    },
    /// An error converted away from any exit: `let r = a().map_err(X);`.
    Convert {
        name: String,
        conversions: Vec<String>,
        line: usize,
    },
    /// `terminal` is true when the path always stops here (`panic!`), false
    /// when it panics only on failure (`unwrap`).
    Panic {
        name: String,
        terminal: bool,
        line: usize,
        allowed: Option<String>,
    },
    /// An error dropped or replaced without leaving the happy path.
    Swallow {
        name: String,
        how: String,
        confidence: Confidence,
        line: usize,
        allowed: Option<String>,
    },
    /// An error handled where it happens, with nothing fallible in the handler:
    /// `if let Err(e) = a() { log(e) }`. The path carries on either way.
    Recover {
        name: String,
        how: String,
        /// The patterns and guards, which decide what gets handled.
        arms: Vec<String>,
        line: usize,
    },
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Arm {
    pub label: String,
    pub body: Vec<Node>,
}

#[derive(Debug, PartialEq, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Returns {
    Result { error: String },
    Option,
    Other,
}

#[derive(Debug, Serialize)]
pub struct Function {
    /// Module path + impl + name. Stable across edits, not across moves.
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub returns: Returns,
    pub nodes: Vec<Node>,
    /// `?` tokens inside macro calls, which syntax alone can't place.
    pub macro_try: usize,
    /// Every local binding, so a review can see through a rename.
    #[serde(skip)]
    pub(crate) locals: Vec<String>,
}

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct Finding {
    pub rule: String,
    pub function: Option<String>,
    pub file: String,
    pub line: usize,
    pub message: String,
    pub confidence: Confidence,
    /// The reason from an allow comment; an allowed finding is not a problem.
    pub allowed: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Coverage {
    pub functions: usize,
    pub steps: usize,
    pub error_from_callee: usize,
    pub error_from_enclosing: usize,
    pub error_unknown: usize,
    /// `?` inside macro calls in fn bodies: not drawn.
    pub macro_try: usize,
    /// `?` inside macros that wrap items, so their fns aren't analysed at all.
    pub item_macro_try: usize,
}

#[derive(Debug, Serialize)]
pub struct ParseFailure {
    pub file: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub coverage: Coverage,
    pub functions: Vec<Function>,
    pub findings: Vec<Finding>,
    pub parse_failures: Vec<ParseFailure>,
    /// Files under a package's `tests/`, `benches/` or `examples/`.
    pub support_files_skipped: usize,
    #[serde(skip)]
    test_files: HashSet<String>,
    /// (file, module, source), walked in `finish` once every signature is known.
    #[serde(skip)]
    pending: Vec<(String, String, String)>,
}

fn line(s: &impl Spanned) -> usize {
    s.span().start().line
}

/// Does this cfg predicate compile only for tests? An author can put a cfg
/// on production code, so `any(test, unix)` must not count: every alternative
/// has to be test-only.
// `any(test, feature = "test-utils")` helpers are analysed too, because a feature
// can be on by default. A config allowlist of feature names would narrow this.
fn test_only(meta: &syn::Meta) -> bool {
    let nested =
        |l: &syn::MetaList| l.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated);
    match meta {
        syn::Meta::Path(p) => p.is_ident("test"),
        syn::Meta::List(l) if l.path.is_ident("all") => {
            nested(l).is_ok_and(|m| m.iter().any(test_only))
        }
        syn::Meta::List(l) if l.path.is_ident("any") => {
            nested(l).is_ok_and(|m| !m.is_empty() && m.iter().all(test_only))
        }
        _ => false,
    }
}

fn is_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let cfg_test =
            a.path().is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|m| test_only(&m));
        cfg_test || a.path().segments.last().is_some_and(|s| s.ident == "test")
    })
}

struct RawFn<'ast> {
    path: Vec<String>,
    owner: Option<String>,
    sig: &'ast Signature,
    /// `None` for a required trait method: a contract with no railway of its own.
    block: Option<&'ast Block>,
}

/// Finds every fn: free, impl, trait methods with or without a body, in inline
/// mods and nested in other fns. Test code is skipped.
#[derive(Default)]
struct Collector<'ast> {
    out: Vec<RawFn<'ast>>,
    path: Vec<String>,
    owner: Option<String>,
    /// `#[cfg(test)] mod x;`: the code lives in another file.
    test_mods: Vec<String>,
    /// Top-level `mod x;`: files that join this one's crate.
    mods: Vec<String>,
    /// `?` in macros outside any fn, e.g. `ffi_fn! { fn f() { a()?; } }`.
    item_macro_try: usize,
    fn_depth: usize,
}

impl<'ast> Collector<'ast> {
    fn add(&mut self, sig: &'ast Signature, block: Option<&'ast Block>) {
        self.out.push(RawFn {
            path: self.path.clone(),
            owner: self.owner.clone(),
            sig,
            block,
        });
        let Some(block) = block else { return };
        self.path.push(sig.ident.to_string());
        let owner = self.owner.take();
        self.fn_depth += 1;
        self.visit_block(block);
        self.fn_depth -= 1;
        self.owner = owner;
        self.path.pop();
    }

    fn within(&mut self, segment: String, owner: Option<String>, f: impl FnOnce(&mut Self)) {
        self.path.push(segment);
        let saved = std::mem::replace(&mut self.owner, owner);
        f(self);
        self.owner = saved;
        self.path.pop();
    }
}

impl<'ast> Visit<'ast> for Collector<'ast> {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if !is_test(&f.attrs) {
            self.add(&f.sig, Some(&f.block));
        }
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if !is_test(&f.attrs) {
            self.add(&f.sig, Some(&f.block));
        }
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.add(&f.sig, f.default.as_ref());
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !is_test(&m.attrs) {
            // `mod_files` maps top-level `mod x;` declarations to files.
            if m.content.is_none() && self.path.is_empty() {
                self.mods.push(m.ident.to_string());
            }
            self.within(m.ident.to_string(), None, |c| visit::visit_item_mod(c, m));
        } else if m.content.is_none() {
            self.test_mods.push(m.ident.to_string());
        }
    }

    fn visit_item_macro(&mut self, m: &'ast syn::ItemMacro) {
        // A `macro_rules!` body is a template, and its `$(..)?` isn't a `?` site.
        if m.ident.is_none() {
            self.visit_macro(&m.mac);
        }
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if self.fn_depth == 0 {
            self.item_macro_try += count_try(m.tokens.clone());
        }
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if is_test(&i.attrs) {
            return;
        }
        let ty = text(&*i.self_ty);
        // `impl From<A> for X` and `impl From<B> for X` both define `from`.
        let segment = match &i.trait_ {
            Some((path, _)) => format!("<{ty} as {}>", text(path)),
            None => ty,
        };
        self.within(segment, type_name(&i.self_ty), |c| {
            visit::visit_item_impl(c, i)
        });
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        self.within(t.ident.to_string(), Some(t.ident.to_string()), |c| {
            visit::visit_item_trait(c, t)
        });
    }
}

fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(p) = ty else { return None };
    Some(p.path.segments.last()?.ident.to_string())
}

fn returns(ret: &ReturnType) -> Returns {
    let ReturnType::Type(_, ty) = ret else {
        return Returns::Other;
    };
    let Type::Path(p) = &**ty else {
        return Returns::Other;
    };
    let Some(seg) = p.path.segments.last() else {
        return Returns::Other;
    };
    if seg.ident == "Option" {
        return Returns::Option;
    }
    if seg.ident != "Result" {
        return Returns::Other;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return Returns::Other;
    };
    match args.args.iter().nth(1) {
        Some(syn::GenericArgument::Type(e)) => Returns::Result { error: text(e) },
        Some(_) => Returns::Other,
        // `anyhow::Result<T>`, `io::Result<T>`: the alias's module names the error.
        None => {
            let segs: Vec<_> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let module = segs[..segs.len() - 1].last();
            Returns::Result {
                error: module.map_or("Error".into(), |m| format!("{m}::Error")),
            }
        }
    }
}

/// What a call site names, before looking it up.
struct Callee {
    /// `Type` in `Type::f()`, `module` in `module::f()`, `Self` for `self.f()`.
    owner: Option<String>,
    name: String,
    method: bool,
}

struct IndexEntry {
    owner: Option<String>,
    name: String,
    returns: Returns,
    /// Module path of the file plus any inline `mod`s, e.g. `app::db`.
    module: String,
}

/// Signatures from one file, backed by every crate the file is compiled into.
// Lookups match by name; `use` paths and type information would narrow them.
struct Index<'a> {
    file: Vec<IndexEntry>,
    /// Every crate the file is compiled into. Matches from all of them are
    /// pooled, so crates that disagree leave the callee unknown.
    krates: Vec<&'a [IndexEntry]>,
}

impl Index<'_> {
    /// The callee's return type, or `None` when it's unknown or ambiguous.
    fn resolve(&self, c: &Callee, current: Option<&str>) -> Option<&Returns> {
        let owner = c
            .owner
            .as_deref()
            .map(|o| if o == "Self" { current.unwrap_or(o) } else { o });
        let mut picked = self.in_file(c, owner);
        if picked.is_empty() {
            picked = self.in_crate(c, owner);
        }
        let first = &picked.first()?.returns;
        picked.iter().all(|e| e.returns == *first).then_some(first)
    }

    fn in_file(&self, c: &Callee, owner: Option<&str>) -> Vec<&IndexEntry> {
        let named = || self.file.iter().filter(|e| e.name == c.name);
        match owner {
            Some(o) if named().any(|e| e.owner.as_deref() == Some(o)) => {
                named().filter(|e| e.owner.as_deref() == Some(o)).collect()
            }
            // A type defined elsewhere: a local `new` says nothing about `Other::new`.
            Some(o) if !c.method && o.starts_with(char::is_uppercase) => vec![],
            _ if c.method => named().filter(|e| e.owner.is_some()).collect(),
            _ => named().filter(|e| e.owner.is_none()).collect(),
        }
    }

    /// Stricter than `in_file`: a bare `x.get()` could be any type's `get`.
    fn in_crate(&self, c: &Callee, owner: Option<&str>) -> Vec<&IndexEntry> {
        let named = self
            .krates
            .iter()
            .flat_map(|k| k.iter())
            .filter(|e| e.name == c.name);
        match owner {
            Some(o) if o.starts_with(char::is_uppercase) => {
                named.filter(|e| e.owner.as_deref() == Some(o)).collect()
            }
            _ if c.method => vec![],
            Some("crate" | "self" | "super") | None => {
                named.filter(|e| e.owner.is_none()).collect()
            }
            Some(m) => named
                .filter(|e| e.owner.is_none() && e.module.rsplit("::").next() == Some(m))
                .collect(),
        }
    }
}

/// Source text of a node with whitespace collapsed.
fn text(t: &(impl ToTokens + Spanned)) -> String {
    let s = t
        .span()
        .source_text()
        .unwrap_or_else(|| t.to_token_stream().to_string());
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keywords that a `(` after them starts an expression, not a call.
const NOT_CALLABLE: &[&str] = &[
    "if", "match", "while", "in", "return", "break", "let", "else", "mut", "ref", "move", "yield",
];

/// Does a `(` after this token hold call arguments, rather than a tuple or a
/// grouped expression?
// `f::<T>(x,)` counts as a tuple, because a `>` could equally be `a > (x,)`.
// Reporting it errs towards showing the reviewer. Parsing with syn would decide.
fn calls(before: Option<&TokenTree>) -> bool {
    match before {
        Some(TokenTree::Ident(i)) => !NOT_CALLABLE.iter().any(|k| i == k),
        Some(TokenTree::Group(g)) => g.delimiter() != Delimiter::Brace,
        Some(TokenTree::Punct(p)) => p.as_char() == '?',
        _ => false,
    }
}

/// Can a `|` after this token only start a closure, rather than be an `or`?
fn opens_closure(before: Option<&TokenTree>) -> bool {
    match before {
        None => true,
        Some(TokenTree::Ident(i)) => i == "move",
        Some(TokenTree::Punct(p)) => matches!(p.as_char(), ',' | '='),
        _ => false,
    }
}

fn is_comma(t: &TokenTree) -> bool {
    matches!(t, TokenTree::Punct(p) if p.as_char() == ',')
}

/// Writes tokens with readable spacing, deciding token by token, so a literal
/// is copied once and never touched by anything that edits text. A trailing
/// comma is formatting, except the one that makes `(x,)` a tuple.
fn write_tokens(tokens: TokenStream, tuple: bool, out: &mut String) {
    let mut tokens: Vec<_> = tokens.into_iter().collect();
    let lone_comma = tokens.iter().filter(|t| is_comma(t)).count() == 1;
    if tokens.last().is_some_and(is_comma) && !(tuple && lone_comma) {
        tokens.pop();
    }
    let mut glued = true;
    let mut before = None;
    let mut in_bars = false; // between the `|`s of a closure's parameters
    for t in tokens {
        // Whether this token hugs what's before it, and what's after it.
        let (hug_before, hug_after) = match &t {
            TokenTree::Punct(p) => {
                let joint = p.spacing() == Spacing::Joint;
                match p.as_char() {
                    '|' if in_bars => {
                        in_bars = false;
                        (true, false)
                    }
                    '|' if !joint && opens_closure(before.as_ref()) => {
                        in_bars = true;
                        (false, true)
                    }
                    '.' => (true, true),
                    ',' | ';' | '?' => (true, false),
                    ':' => (true, joint || out.ends_with(':')),
                    _ => (false, joint),
                }
            }
            TokenTree::Group(g) => (
                matches!(g.delimiter(), Delimiter::Parenthesis | Delimiter::Bracket),
                false,
            ),
            _ => (false, false),
        };
        if !glued && !hug_before {
            out.push(' ');
        }
        match &t {
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Brace => ("{ ", " }"),
                    Delimiter::None => ("", ""),
                };
                let tuple = g.delimiter() == Delimiter::Parenthesis && !calls(before.as_ref());
                out.push_str(open);
                write_tokens(g.stream(), tuple, out);
                out.push_str(close);
            }
            other => out.push_str(&other.to_string()),
        }
        glued = hug_after;
        before = Some(t);
    }
}

/// An expression or pattern as tokens: comments and formatting are gone, so
/// equal text means equal code.
fn canon(t: &impl ToTokens) -> String {
    let mut out = String::new();
    write_tokens(t.to_token_stream(), false, &mut out);
    out
}

fn cap(s: &str) -> String {
    if s.chars().count() > 40 {
        s.chars().take(37).collect::<String>() + "..."
    } else {
        s.to_string()
    }
}

/// `billing::invoice::Client::builder` -> `Client::builder`.
fn short_expr(e: &Expr) -> String {
    match e {
        Expr::Path(p) => {
            let segs: Vec<_> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            segs[segs.len().saturating_sub(2)..].join("::")
        }
        other => short(canon(other)),
    }
}

fn short(path: String) -> String {
    let parts: Vec<_> = path.split("::").collect();
    parts[parts.len().saturating_sub(2)..].join("::")
}

const ADAPTERS: &[&str] = &["map_err", "context", "with_context", "ok_or", "ok_or_else"];
const PANIC_MACROS: &[&str] = &["panic", "todo", "unimplemented", "unreachable"];

fn is_self(e: &Expr) -> bool {
    matches!(e, Expr::Path(p) if p.path.is_ident("self"))
}

/// What a `?` (or `.unwrap()` etc.) is applied to, and its display label.
fn step_label(mut e: &Expr) -> (Callee, String) {
    loop {
        match e {
            Expr::Await(a) => e = &a.base,
            Expr::Paren(p) => e = &p.expr,
            Expr::MethodCall(m) if ADAPTERS.contains(&m.method.to_string().as_str()) => {
                e = &m.receiver
            }
            _ => break,
        }
    }
    match e {
        Expr::Call(c) => {
            let label = short_expr(&c.func);
            let callee = match &*c.func {
                Expr::Path(p) => {
                    let segs: Vec<_> = p
                        .path
                        .segments
                        .iter()
                        .map(|s| s.ident.to_string())
                        .collect();
                    let owner = segs.len().checked_sub(2).map(|i| segs[i].clone());
                    Callee {
                        owner,
                        name: segs.last().cloned().unwrap_or_default(),
                        method: false,
                    }
                }
                _ => Callee {
                    owner: None,
                    name: label.clone(),
                    method: false,
                },
            };
            (callee, label)
        }
        Expr::MethodCall(m) => {
            let method = m.method.to_string();
            // Walk down a builder chain to its root: `X::new().a().b()` -> `X::new`.
            let (mut root, mut depth) = (&*m.receiver, 1);
            loop {
                match root {
                    Expr::MethodCall(x) => (root, depth) = (&*x.receiver, depth + 1),
                    Expr::Await(a) => root = &a.base,
                    _ => break,
                }
            }
            let label = match root {
                Expr::Try(_) => method.clone(), // earlier `?` in the chain is its own step
                Expr::Call(c) => format!("{}…{method}", short_expr(&c.func)),
                r if depth == 1 => format!("{}.{method}", short_expr(r)),
                r => format!("{}…{method}", short_expr(r)),
            };
            let owner = is_self(&m.receiver).then(|| "Self".to_string());
            (
                Callee {
                    owner,
                    name: method,
                    method: true,
                },
                label,
            )
        }
        other => (
            Callee {
                owner: None,
                name: canon(other),
                method: false,
            },
            canon(other),
        ),
    }
}

fn is_adapter(m: &syn::ExprMethodCall) -> bool {
    ADAPTERS.contains(&m.method.to_string().as_str())
}

fn adapter_text(m: &syn::ExprMethodCall) -> String {
    format!("{}({})", m.method, canon(&m.args))
}

/// The adapters `step_label` looks through, innermost first.
fn conversions(mut e: &Expr) -> Vec<String> {
    let mut out = vec![];
    loop {
        match e {
            Expr::Await(a) => e = &a.base,
            Expr::Paren(p) => e = &p.expr,
            Expr::MethodCall(m) if is_adapter(m) => {
                out.push(adapter_text(m));
                e = &m.receiver;
            }
            _ => break,
        }
    }
    out.reverse();
    out
}

/// `Ok(x)` / `Err(x)` -> ("Ok"/"Err", x).
fn ok_err(e: &Expr) -> Option<(String, Option<&Expr>)> {
    let Expr::Call(c) = e else { return None };
    let Expr::Path(p) = &*c.func else { return None };
    let tag = p.path.segments.last()?.ident.to_string();
    (tag == "Ok" || tag == "Err").then(|| (tag, c.args.first()))
}

fn count_try(tokens: TokenStream) -> usize {
    tokens
        .into_iter()
        .map(|t| match t {
            TokenTree::Punct(p) if p.as_char() == '?' => 1,
            TokenTree::Group(g) => count_try(g.stream()),
            _ => 0,
        })
        .sum()
}

fn strip_await(mut e: &Expr) -> &Expr {
    loop {
        match e {
            Expr::Await(a) => e = &a.base,
            Expr::Paren(p) => e = &p.expr,
            _ => return e,
        }
    }
}

fn ok_receiver(e: &Expr) -> Option<&Expr> {
    match e {
        Expr::MethodCall(m) if m.method == "ok" && m.args.is_empty() => Some(&m.receiver),
        _ => None,
    }
}

/// `Ok(..)` or `Err(..)`: a pattern that only a `Result` matches. Sees
/// through a binding, as in `e @ Err(_)`.
fn result_pat(p: &syn::Pat) -> Option<String> {
    let t = match unguarded(p) {
        syn::Pat::TupleStruct(t) => t,
        syn::Pat::Ident(syn::PatIdent {
            subpat: Some((_, sub)),
            ..
        }) => return result_pat(sub),
        _ => return None,
    };
    let tag = t.path.segments.last()?.ident.to_string();
    ["Ok", "Err"].contains(&tag.as_str()).then_some(tag)
}

/// What a returned match arm gives back.
#[derive(PartialEq)]
enum Handed {
    /// `e => e`: the value it matched, error included.
    Binding,
    /// A local that may hold anything by the time it's returned:
    /// `e => { let e = ..; e }`, `mut e => { e = ..; e }`.
    Local,
    Other,
}

fn handed(a: &syn::Arm) -> Handed {
    let bound = match unguarded(&a.pat) {
        syn::Pat::Ident(b) => Some(&b.ident),
        _ => None,
    };
    let mut changed = false;
    let mut e = &*a.body;
    loop {
        e = match e {
            Expr::Block(b) => {
                if let Some(name) = bound {
                    let mut r = Rebinds { name, found: false };
                    changed |= r.stmts(&b.block.stmts) || r.found;
                }
                match b.block.stmts.last() {
                    Some(Stmt::Expr(last, None)) => last,
                    _ => return Handed::Other,
                }
            }
            Expr::Paren(p) => &p.expr,
            Expr::Path(p) => {
                let Some(id) = p.path.get_ident() else {
                    return Handed::Other;
                };
                return if Some(id) == bound && !changed {
                    // `ok @ Ok(_) => ok` hands back only a success.
                    match result_pat(&a.pat).as_deref() {
                        Some("Ok") => Handed::Other,
                        _ => Handed::Binding,
                    }
                } else if id
                    .to_string()
                    .starts_with(|c: char| c.is_lowercase() || c == '_')
                {
                    Handed::Local
                } else {
                    Handed::Other // a constant or unit variant
                };
            }
            _ => return Handed::Other,
        };
    }
}

/// Does code change what `name` holds, by assigning to it or lending it
/// mutably? Inner scopes that bind `name` again are left out, since there
/// the name means something else.
struct Rebinds<'a> {
    name: &'a syn::Ident,
    found: bool,
}

impl Rebinds<'_> {
    fn names(&self, e: &Expr) -> bool {
        matches!(e, Expr::Path(p) if p.path.is_ident(self.name))
    }

    fn binds(&self, p: &syn::Pat) -> bool {
        let mut bound = Locals::default();
        bound.visit_pat(p);
        bound.0.iter().any(|n| self.name == n)
    }

    /// Visits statements up to a `let` that binds `name` again, and says
    /// whether one did.
    fn stmts(&mut self, stmts: &[Stmt]) -> bool {
        for s in stmts {
            self.visit_stmt(s);
            if matches!(s, Stmt::Local(l) if self.binds(&l.pat)) {
                return true;
            }
        }
        false
    }
}

impl<'ast> Visit<'ast> for Rebinds<'_> {
    fn visit_item(&mut self, _: &'ast syn::Item) {}

    fn visit_block(&mut self, b: &'ast Block) {
        self.stmts(&b.stmts);
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        if !c.inputs.iter().any(|p| self.binds(p)) {
            self.visit_expr(&c.body);
        }
    }

    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        if !self.binds(&a.pat) {
            visit::visit_arm(self, a);
        }
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        if !self.binds(&f.pat) {
            self.visit_block(&f.body);
        }
    }

    fn visit_expr_if(&mut self, i: &'ast syn::ExprIf) {
        match &*i.cond {
            Expr::Let(l) if self.binds(&l.pat) => self.visit_expr(&l.expr),
            cond => {
                self.visit_expr(cond);
                self.visit_block(&i.then_branch);
            }
        }
        if let Some((_, e)) = &i.else_branch {
            self.visit_expr(e);
        }
    }

    fn visit_expr_while(&mut self, w: &'ast syn::ExprWhile) {
        match &*w.cond {
            Expr::Let(l) if self.binds(&l.pat) => self.visit_expr(&l.expr),
            cond => {
                self.visit_expr(cond);
                self.visit_block(&w.body);
            }
        }
    }

    fn visit_expr_assign(&mut self, a: &'ast syn::ExprAssign) {
        self.found |= self.names(&a.left);
        visit::visit_expr_assign(self, a);
    }

    fn visit_expr_reference(&mut self, r: &'ast syn::ExprReference) {
        self.found |= r.mutability.is_some() && self.names(&r.expr);
        visit::visit_expr_reference(self, r);
    }
}

/// A pattern without its guard.
fn unguarded(p: &syn::Pat) -> &syn::Pat {
    match p {
        syn::Pat::Guard(g) => &g.pat,
        p => p,
    }
}

/// Is this a `loop` that nothing breaks out of? Nothing after it can run.
pub(crate) fn diverges(can_skip: bool, tag: Option<&str>, body: &[Node]) -> bool {
    !can_skip && !breaks_to(body, tag, true)
}

/// Is there a `break` here aimed at the loop `tag`? `direct` says a plain
/// `break` would reach it, which stops being true inside a nested loop.
fn breaks_to(nodes: &[Node], tag: Option<&str>, direct: bool) -> bool {
    nodes.iter().any(|n| match n {
        Node::Jump { name, .. } => match name.split_once(' ') {
            Some(("break", target)) => tag == Some(target),
            _ => name == "break" && direct,
        },
        Node::Branch { arms, .. } => arms.iter().any(|a| breaks_to(&a.body, tag, direct)),
        Node::Block { body, .. } => breaks_to(body, tag, direct),
        Node::Loop { head, body, .. } => breaks_to(head, tag, false) || breaks_to(body, tag, false),
        _ => false,
    })
}

/// Is this nothing but jumps that land inside what's being pruned? `tags` are
/// the labels in scope, and `in_loop` says a plain `break` has a loop here:
/// a labelled block doesn't catch one.
fn only_local_jumps(nodes: &[Node], tags: &[Option<&str>], in_loop: bool) -> bool {
    let with = |tag: Option<&'_ str>| -> Vec<Option<String>> {
        tags.iter()
            .map(|t| t.map(str::to_string))
            .chain([tag.map(str::to_string)])
            .collect()
    };
    nodes.iter().all(|n| match n {
        Node::Jump { name, .. } => match name.split_once(' ') {
            Some((_, target)) => tags.contains(&Some(target)),
            None => in_loop,
        },
        Node::Branch { arms, .. } => arms
            .iter()
            .all(|a| only_local_jumps(&a.body, tags, in_loop)),
        Node::Loop {
            tag,
            can_skip,
            head,
            body,
            ..
        } => {
            let inner = with(tag.as_deref());
            let inner: Vec<_> = inner.iter().map(Option::as_deref).collect();
            head.is_empty()
                && !diverges(*can_skip, tag.as_deref(), body)
                && only_local_jumps(body, &inner, true)
        }
        Node::Block { tag, body } => {
            let inner = with(Some(tag));
            let inner: Vec<_> = inner.iter().map(Option::as_deref).collect();
            only_local_jumps(body, &inner, in_loop)
        }
        _ => false,
    })
}

/// Local bindings and parameters, in order.
#[derive(Default)]
pub(crate) struct Locals(pub(crate) Vec<String>);

impl<'ast> Visit<'ast> for Locals {
    fn visit_item(&mut self, _: &'ast syn::Item) {}

    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        let name = p.ident.to_string();
        if name.starts_with(|c: char| c.is_lowercase() || c == '_') && !self.0.contains(&name) {
            self.0.push(name);
        }
        visit::visit_pat_ident(self, p);
    }
}

struct Allow {
    line: usize,
    rule: String,
    reason: String,
}

/// Reads allow comments, returning valid ones and (line, problem) for the rest.
fn parse_allows(src: &str) -> (Vec<Allow>, Vec<(usize, String)>) {
    let (mut allows, mut bad) = (vec![], vec![]);
    for (i, l) in src.lines().enumerate() {
        let n = i + 1;
        let doc = ["///", "//!"].iter().any(|d| l.trim_start().starts_with(d));
        let Some((before, rest)) = l.split_once("// errpath: allow(").filter(|_| !doc) else {
            continue;
        };
        if before.matches('"').count() % 2 == 1 {
            continue; // inside a string literal
        }
        let Some((rule, reason)) = rest.split_once(')') else {
            bad.push((n, "unclosed allow comment".to_string()));
            continue;
        };
        let reason = reason.trim();
        if !ALLOWABLE.contains(&rule) {
            bad.push((n, format!("unknown rule `{rule}` in allow comment")));
        } else if reason.is_empty() {
            bad.push((
                n,
                format!("allow({rule}) needs a reason, so it was ignored"),
            ));
        } else {
            allows.push(Allow {
                line: n,
                rule: rule.into(),
                reason: reason.into(),
            });
        }
    }
    (allows, bad)
}

struct Walker<'a> {
    index: &'a Index<'a>,
    allows: &'a [Allow],
    owner: Option<&'a str>,
    nodes: Vec<Node>,
    /// Is the expression being visited the function's (or closure's) return value?
    tail: bool,
    /// What `?` on an unknown callee converts into, if anything.
    default_err: Option<String>,
    macro_try: usize,
    /// The expression being visited is what a step applies to, so an adapter
    /// on it belongs to that step.
    in_step: bool,
}

impl Walker<'_> {
    /// Walk `f` into a fresh node list (one branch arm / scope body).
    fn arm(&mut self, f: impl FnOnce(&mut Self)) -> Vec<Node> {
        let outer = std::mem::take(&mut self.nodes);
        f(self);
        std::mem::replace(&mut self.nodes, outer)
    }

    fn with_tail(&mut self, tail: bool, f: impl FnOnce(&mut Self)) {
        let saved = std::mem::replace(&mut self.tail, tail);
        f(self);
        self.tail = saved;
    }

    fn push_branch(&mut self, label: String, arms: Vec<Arm>) {
        // A fork with nothing fallible in any arm is noise on a railway.
        if arms.iter().any(|a| !a.body.is_empty()) {
            self.nodes.push(Node::Branch { label, arms });
        }
    }

    /// A fork on a `Result` whose arms do nothing fallible handles the error
    /// in place, so it goes on the railway as a recovery.
    fn recover_or_branch(
        &mut self,
        label: String,
        arms: Vec<Arm>,
        scrutinee: &Expr,
        how: Option<(String, Vec<String>)>,
    ) {
        match how {
            Some((how, patterns)) if arms.iter().all(|a| a.body.is_empty()) => {
                self.nodes.push(Node::Recover {
                    name: step_label(strip_await(scrutinee)).1,
                    how,
                    arms: patterns,
                    line: line(scrutinee),
                })
            }
            _ => self.push_branch(label, arms),
        }
    }

    /// Visit an expression whose value may be returned: a tail `Err(..)` fails.
    fn tail_expr(&mut self, e: &Expr) {
        if self.tail
            && let Some((tag, arg)) = ok_err(e)
        {
            if let Some(a) = arg {
                self.with_tail(false, |w| w.visit_expr(a));
            }
            if tag == "Err" {
                self.nodes.push(Node::Fail {
                    error: arg.map(canon).unwrap_or_default(),
                    line: line(e),
                });
            }
            return;
        }
        if self.tail && matches!(e, Expr::Path(p) if p.path.is_ident("None")) {
            self.nodes.push(Node::Fail {
                error: "None".into(),
                line: line(e),
            });
            return;
        }
        let step = self.tail && self.propagates(e);
        self.in_step = step;
        self.visit_expr(e);
        self.in_step = false;
        if step {
            self.push_step(e, e.span().end().line);
        }
    }

    /// Is this returned call a `Result`/`Option` handed straight to the caller,
    /// which is a `?` in all but name?
    fn propagates(&self, e: &Expr) -> bool {
        let inner = strip_await(e);
        let args = match inner {
            Expr::Call(c) => &c.args,
            Expr::MethodCall(m) => &m.args,
            _ => return false,
        };
        if matches!(inner, Expr::Call(c) if matches!(&*c.func, Expr::Path(p) if p.path.is_ident("Some")))
        {
            return false; // a value, like `Ok(x)`
        }
        // A wrapper like `traced(|| ..)`: the closure's railway is the story.
        if args
            .iter()
            .any(|a| matches!(a, Expr::Closure(_) | Expr::Async(_)))
        {
            return false;
        }
        match self.index.resolve(&step_label(e).0, self.owner) {
            Some(Returns::Other) => false,
            Some(_) => true,
            None => self.default_err.is_some() || !conversions(e).is_empty(),
        }
    }

    /// One way off the happy path through `e`, which is what `?` applies to or
    /// a returned call.
    fn push_step(&mut self, e: &Expr, at: usize) {
        let (callee, name) = step_label(e);
        let (error, source) = match self.index.resolve(&callee, self.owner) {
            Some(Returns::Result { error }) => (Some(error.clone()), ErrorSource::Callee),
            Some(Returns::Option) => (Some("None".into()), ErrorSource::Callee),
            _ => match &self.default_err {
                Some(err) => (Some(err.clone()), ErrorSource::Enclosing),
                None => (None, ErrorSource::Unknown),
            },
        };
        self.nodes.push(Node::Step {
            name,
            error,
            source,
            conversions: conversions(e),
            line: at,
        });
    }

    /// A body that returns to its caller, not the enclosing fn, so `?` inside
    /// it doesn't convert into the enclosing error.
    // Assumes the body runs inline and its result is returned, as in a
    // `traced(|| ..)` wrapper. A stored closure is drawn the same, and a
    // `return Ok` inside a closure is drawn as reaching Done.
    fn scope(&mut self, label: String, walk: impl FnOnce(&mut Self)) {
        let saved = self.default_err.take();
        let body = self.arm(|w| w.with_tail(true, walk));
        self.default_err = saved;
        if !body.is_empty() {
            self.nodes.push(Node::Scope { label, body });
        }
    }

    fn loop_body(
        &mut self,
        label: String,
        tag: Option<&syn::Label>,
        can_skip: bool,
        head: Vec<Node>,
        b: &Block,
    ) {
        let body = self.arm(|w| w.with_tail(false, |w| w.visit_block(b)));
        let tag = tag.map(|t| text(&t.name));
        // Jumps only matter for what they skip: with nothing fallible around
        // them and none leaving, a loop is noise, like a fork with nothing fallible.
        if !head.is_empty()
            || diverges(can_skip, tag.as_deref(), &body)
            || !only_local_jumps(&body, &[tag.as_deref()], true)
        {
            self.nodes.push(Node::Loop {
                label,
                tag,
                can_skip,
                head,
                body,
            });
        }
    }

    fn jump(&mut self, word: &str, label: Option<&syn::Lifetime>, at: usize) {
        let name = label.map_or(word.to_string(), |l| format!("{word} {}", text(l)));
        self.nodes.push(Node::Jump { name, line: at });
    }

    fn closure(&mut self, label: String, c: &syn::ExprClosure) {
        match &*c.body {
            Expr::Async(a) => self.scope(label, |w| w.visit_block(&a.block)),
            body => self.scope(label, |w| w.tail_expr(body)),
        }
    }

    fn args<'e>(&mut self, label: &str, args: impl Iterator<Item = &'e Expr>) {
        for a in args {
            match a {
                Expr::Closure(c) => self.closure(label.to_string(), c),
                Expr::Async(b) => self.scope(label.to_string(), |w| w.visit_block(&b.block)),
                _ => self.with_tail(false, |w| w.visit_expr(a)),
            }
        }
    }

    /// `None` when the expression provably can't carry an error.
    fn fallibility(&self, e: &Expr) -> Option<Confidence> {
        let callee = step_label(e).0;
        // `x.send()` matched by method name alone says nothing about `x`.
        if callee.method && callee.owner.is_none() {
            return Some(Confidence::Possible);
        }
        match self.index.resolve(&callee, self.owner) {
            Some(Returns::Result { .. }) => Some(Confidence::Certain),
            Some(_) => None,
            None => Some(Confidence::Possible),
        }
    }

    /// Allow comments count on the line above an expression, or on any line of it.
    fn allowed(&self, rule: &str, first: usize, last: usize) -> Option<String> {
        let hit = self
            .allows
            .iter()
            .find(|a| a.rule == rule && a.line + 1 >= first && a.line <= last);
        hit.map(|a| a.reason.clone())
    }

    fn swallow(&mut self, whole: proc_macro2::Span, how: &str, target: &Expr) {
        if let Some(confidence) = self.fallibility(target) {
            let first = whole.start().line;
            let allowed = self.allowed(NO_SWALLOW, first, whole.end().line);
            let name = step_label(target).1;
            self.nodes.push(Node::Swallow {
                name,
                how: how.into(),
                confidence,
                line: first,
                allowed,
            });
        }
    }

    /// A value thrown away by `let _ =`, `_ =` or a discarded `.ok()`.
    fn discarded(&mut self, e: &Expr, how: &str) {
        let inner = strip_await(e);
        if matches!(inner, Expr::Call(_) | Expr::MethodCall(_)) {
            self.swallow(e.span(), how, ok_receiver(inner).unwrap_or(inner));
        }
    }

    fn check_discard(&mut self, s: &Stmt) {
        match s {
            Stmt::Local(l) if matches!(l.pat, syn::Pat::Wild(_)) => {
                if let Some(init) = &l.init {
                    self.discarded(&init.expr, "let _ =");
                }
            }
            Stmt::Expr(Expr::Assign(a), Some(_)) if matches!(*a.left, Expr::Infer(_)) => {
                self.discarded(&a.right, "_ =");
            }
            Stmt::Expr(e, Some(_)) if ok_receiver(e).is_some() => self.discarded(e, ".ok()"),
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for Walker<'_> {
    fn visit_item(&mut self, _: &'ast syn::Item) {} // a nested fn is its own railway

    fn visit_block(&mut self, b: &'ast Block) {
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            match s {
                Stmt::Expr(e, None) if i + 1 == n => self.tail_expr(e),
                _ => {
                    self.with_tail(false, |w| w.visit_stmt(s));
                    self.check_discard(s);
                }
            }
        }
    }

    fn visit_local(&mut self, l: &'ast syn::Local) {
        let Some(init) = &l.init else { return };
        self.with_tail(false, |w| w.visit_expr(&init.expr));
        if let Some((_, diverge)) = &init.diverge {
            let body = self.arm(|w| w.with_tail(false, |w| w.visit_expr(diverge)));
            let matched = Arm {
                label: "matched".into(),
                body: vec![],
            };
            let label = format!("let {} = {} else", canon(&l.pat), canon(&*init.expr));
            self.push_branch(
                label,
                vec![
                    Arm {
                        label: "else".into(),
                        body,
                    },
                    matched,
                ],
            );
        }
    }

    fn visit_expr_try(&mut self, t: &'ast syn::ExprTry) {
        self.in_step = true;
        self.with_tail(false, |w| visit::visit_expr_try(w, t)); // inner `?`s run first
        self.in_step = false;
        self.push_step(&t.expr, line(&t.question_token));
    }

    fn visit_expr_if(&mut self, i: &'ast syn::ExprIf) {
        self.with_tail(false, |w| w.visit_expr(&i.cond));
        let then = self.arm(|w| w.visit_block(&i.then_branch));
        let other = self.arm(|w| {
            if let Some((_, e)) = &i.else_branch {
                w.tail_expr(e) // `else if` nests as a branch inside this arm
            }
        });
        let arms = vec![
            Arm {
                label: "yes".into(),
                body: then,
            },
            Arm {
                label: "no".into(),
                body: other,
            },
        ];
        let label = format!("if {}", canon(&*i.cond));
        match &*i.cond {
            Expr::Let(l) => {
                let how = result_pat(&l.pat).map(|t| (format!("if let {t}"), vec![canon(&*l.pat)]));
                self.recover_or_branch(label, arms, &l.expr, how);
            }
            _ => self.push_branch(label, arms),
        }
    }

    fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
        self.with_tail(false, |w| w.visit_expr(&m.expr));
        // syn keeps a guard inside the pattern. It's drawn at the head of its
        // arm, though a false guard falls through to the next arm.
        let arm = |w: &mut Self, a: &'ast syn::Arm| {
            w.with_tail(false, |w| w.visit_pat(&a.pat));
            w.tail_expr(&a.body);
        };
        let on_result = m.arms.iter().any(|a| result_pat(&a.pat).is_some());
        // Returned, an arm that hands back its binding carries the error to
        // the caller: a step on that arm.
        let returns = on_result && self.tail;
        let handed: Vec<Handed> = m.arms.iter().map(handed).collect();
        let at = line(&*m.expr);
        let arms: Vec<Arm> = m
            .arms
            .iter()
            .zip(&handed)
            .map(|(a, h)| {
                let mut body = self.arm(|w| arm(w, a));
                if returns && *h == Handed::Binding {
                    body.extend(self.arm(|w| w.push_step(&m.expr, at)));
                }
                Arm {
                    label: canon(&a.pat),
                    body,
                }
            })
            .collect();
        // Every error handed back without a guard: the match is the call itself.
        let plain = m.arms.iter().zip(&arms).zip(&handed).all(|((a, arm), h)| {
            if *h == Handed::Binding {
                !matches!(a.pat, syn::Pat::Guard(_)) && arm.body.len() == 1
            } else {
                result_pat(&a.pat).as_deref() == Some("Ok") && arm.body.is_empty()
            }
        });
        if returns && plain {
            return self.push_step(&m.expr, at);
        }
        // A returned local may hold an error, so the match can't claim to handle one.
        let opaque = returns && self.default_err.is_some() && handed.contains(&Handed::Local);
        let how = (on_result && !opaque).then(|| {
            (
                "match".to_string(),
                m.arms.iter().map(|a| canon(&a.pat)).collect(),
            )
        });
        self.recover_or_branch(format!("match {}", canon(&*m.expr)), arms, &m.expr, how);
    }

    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        let (op, runs_when, skips_when) = match b.op {
            syn::BinOp::And(_) => ("&&", "true", "false"),
            syn::BinOp::Or(_) => ("||", "false", "true"),
            _ => return self.with_tail(false, |w| visit::visit_expr_binary(w, b)),
        };
        self.with_tail(false, |w| w.visit_expr(&b.left));
        let body = self.arm(|w| w.with_tail(false, |w| w.visit_expr(&b.right)));
        let arms = vec![
            Arm {
                label: runs_when.into(),
                body,
            },
            Arm {
                label: skips_when.into(),
                body: vec![],
            },
        ];
        self.push_branch(format!("{} {op} …", canon(&*b.left)), arms);
    }

    fn visit_expr_block(&mut self, b: &'ast syn::ExprBlock) {
        let Some(label) = &b.label else {
            return visit::visit_expr_block(self, b);
        };
        let tag = text(&label.name);
        let body = self.arm(|w| w.visit_block(&b.block));
        if !only_local_jumps(&body, &[Some(&tag)], false) {
            self.nodes.push(Node::Block { tag, body });
        }
    }

    fn visit_expr_break(&mut self, b: &'ast syn::ExprBreak) {
        self.with_tail(false, |w| visit::visit_expr_break(w, b));
        self.jump("break", b.label.as_ref(), line(b));
    }

    fn visit_expr_continue(&mut self, c: &'ast syn::ExprContinue) {
        self.jump("continue", c.label.as_ref(), line(c));
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.with_tail(false, |w| w.visit_expr(&f.expr));
        self.loop_body(
            format!("for {} in {}", canon(&*f.pat), canon(&*f.expr)),
            f.label.as_ref(),
            true,
            vec![],
            &f.body,
        );
    }

    fn visit_expr_while(&mut self, l: &'ast syn::ExprWhile) {
        let head = self.arm(|w| w.with_tail(false, |w| w.visit_expr(&l.cond)));
        self.loop_body(
            format!("while {}", canon(&*l.cond)),
            l.label.as_ref(),
            true,
            head,
            &l.body,
        );
    }

    fn visit_expr_loop(&mut self, l: &'ast syn::ExprLoop) {
        self.loop_body("loop".into(), l.label.as_ref(), false, vec![], &l.body);
    }

    fn visit_expr_return(&mut self, r: &'ast syn::ExprReturn) {
        match &r.expr {
            None => self.nodes.push(Node::Succeed),
            Some(e) => {
                self.with_tail(true, |w| w.tail_expr(e));
                if !matches!(self.nodes.last(), Some(Node::Fail { .. })) {
                    self.nodes.push(Node::Succeed);
                }
            }
        }
    }

    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        self.in_step = false;
        self.with_tail(false, |w| w.visit_expr(&c.func));
        self.args(&short_expr(&c.func), c.args.iter());
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let in_step = std::mem::take(&mut self.in_step);
        self.in_step = is_adapter(m); // the rest of the chain is reported here
        self.with_tail(false, |w| w.visit_expr(&m.receiver));
        self.in_step = false;
        self.args(&m.method.to_string(), m.args.iter());
        if is_adapter(m) && !in_step {
            let mut all = conversions(&m.receiver);
            all.push(adapter_text(m));
            self.nodes.push(Node::Convert {
                name: step_label(&m.receiver).1,
                conversions: all,
                line: line(&m.method),
            });
        }
        let method = m.method.to_string();
        match (method.as_str(), m.args.len()) {
            ("unwrap", 0) | ("expect", 1) => {
                let at = line(&m.method);
                let allowed = self.allowed(NO_PANIC, m.span().start().line, m.span().end().line);
                let name = format!("{}.{method}", step_label(&m.receiver).1);
                self.nodes.push(Node::Panic {
                    name,
                    terminal: false,
                    line: at,
                    allowed,
                });
            }
            // Most of these are on Option, so an unknown receiver is skipped.
            // Real type information would find the rest.
            ("unwrap_or_default", 0) | ("unwrap_or", 1)
                if self.fallibility(&m.receiver) == Some(Confidence::Certain) =>
            {
                self.swallow(m.span(), &method, &m.receiver);
            }
            _ => {}
        }
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.closure("closure".into(), c);
    }

    fn visit_expr_async(&mut self, a: &'ast syn::ExprAsync) {
        self.scope("async block".into(), |w| w.visit_block(&a.block));
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.macro_try += count_try(m.tokens.clone());
        let name = m
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        if PANIC_MACROS.contains(&name.as_str()) {
            let allowed = self.allowed(NO_PANIC, m.span().start().line, m.span().end().line);
            self.nodes.push(Node::Panic {
                name: format!("{name}!"),
                terminal: true,
                line: line(&m.path),
                allowed,
            });
        }
    }
}

pub(crate) fn add_findings(nodes: &[Node], f: &Function, out: &mut Vec<Finding>) {
    for n in nodes {
        let finding =
            |rule: &str, line: usize, message: String, confidence, allowed: &Option<String>| {
                Finding {
                    rule: rule.into(),
                    function: Some(f.id.clone()),
                    file: f.file.clone(),
                    line,
                    message,
                    confidence,
                    allowed: allowed.clone(),
                }
            };
        match n {
            Node::Panic {
                name,
                line,
                allowed,
                ..
            } => out.push(finding(
                NO_PANIC,
                *line,
                format!("`{name}` can panic"),
                Confidence::Certain,
                allowed,
            )),
            Node::Swallow {
                name,
                how,
                confidence,
                line,
                allowed,
            } => out.push(finding(
                NO_SWALLOW,
                *line,
                format!("`{how}` drops the error from `{name}`"),
                *confidence,
                allowed,
            )),
            Node::Branch { arms, .. } => arms.iter().for_each(|a| add_findings(&a.body, f, out)),
            Node::Scope { body, .. } | Node::Block { body, .. } => add_findings(body, f, out),
            Node::Loop { head, body, .. } => {
                add_findings(head, f, out);
                add_findings(body, f, out);
            }
            _ => {}
        }
    }
}

fn tally(nodes: &[Node], c: &mut Coverage) {
    for n in nodes {
        match n {
            Node::Step { source, .. } => {
                c.steps += 1;
                match source {
                    ErrorSource::Callee => c.error_from_callee += 1,
                    ErrorSource::Enclosing => c.error_from_enclosing += 1,
                    ErrorSource::Unknown => c.error_unknown += 1,
                }
            }
            Node::Branch { arms, .. } => arms.iter().for_each(|a| tally(&a.body, c)),
            Node::Scope { body, .. } | Node::Block { body, .. } => tally(body, c),
            Node::Loop { head, body, .. } => {
                tally(head, c);
                tally(body, c);
            }
            _ => {}
        }
    }
}

impl Report {
    pub(crate) fn new() -> Self {
        Report {
            schema_version: SCHEMA_VERSION,
            coverage: Coverage::default(),
            functions: vec![],
            findings: vec![],
            parse_failures: vec![],
            support_files_skipped: 0,
            test_files: HashSet::new(),
            pending: vec![],
        }
    }

    pub(crate) fn add_source(&mut self, file: &str, module: &str, src: &str) {
        self.pending.push((file.into(), module.into(), src.into()));
    }

    fn walk_file(
        &mut self,
        file: &str,
        module: &str,
        src: &str,
        collector: &Collector,
        krates: Vec<&[IndexEntry]>,
    ) {
        let index = Index {
            file: collector
                .out
                .iter()
                .map(|f| index_entry(module, f))
                .collect(),
            krates,
        };
        let (allows, bad) = parse_allows(src);
        for f in &collector.out {
            let ret = returns(&f.sig.output);
            let default_err = match &ret {
                Returns::Result { error } => Some(error.clone()),
                Returns::Option => Some("None".into()),
                Returns::Other => None,
            };
            let mut w = Walker {
                index: &index,
                allows: &allows,
                owner: f.owner.as_deref(),
                nodes: vec![],
                tail: true,
                default_err,
                macro_try: 0,
                in_step: false,
            };
            if let Some(block) = f.block {
                w.visit_block(block);
            }
            let name = f.sig.ident.to_string();
            let segments = std::iter::once(module)
                .chain(f.path.iter().map(String::as_str))
                .chain([name.as_str()]);
            let id = segments
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("::");
            let (nodes, macro_try) = (w.nodes, w.macro_try);
            let mut locals = Locals::default();
            locals.visit_signature(f.sig);
            if let Some(block) = f.block {
                locals.visit_block(block);
            }
            let locals = locals.0;
            self.functions.push(Function {
                id,
                name,
                file: file.into(),
                line: line(&f.sig.ident),
                returns: ret,
                nodes,
                macro_try,
                locals,
            });
        }
        self.findings
            .extend(bad.into_iter().map(|(line, message)| Finding {
                rule: BAD_ALLOW.into(),
                function: None,
                file: file.into(),
                line,
                message,
                confidence: Confidence::Certain,
                allowed: None,
            }));
    }

    /// `#[cfg(unix)]` / `#[cfg(windows)]` twins share a path: number the repeats.
    pub(crate) fn finish(mut self) -> Self {
        let pending = std::mem::take(&mut self.pending);
        let mut parsed = vec![];
        for (file, module, src) in &pending {
            match syn::parse_file(src) {
                Ok(p) => parsed.push((file, module, src, p)),
                Err(e) => self.parse_failures.push(ParseFailure {
                    file: file.clone(),
                    message: e.to_string(),
                }),
            }
        }
        let collectors: Vec<Collector> = parsed
            .iter()
            .map(|(.., p)| {
                let mut c = Collector::default();
                c.visit_file(p);
                c
            })
            .collect();
        for ((file, ..), c) in parsed.iter().zip(&collectors) {
            self.coverage.item_macro_try += c.item_macro_try;
            self.test_files
                .extend(c.test_mods.iter().flat_map(|m| mod_files(file, m)));
        }
        // Each crate gets its own index, matching rustc's name scope.
        let mods: HashMap<&str, &[String]> = parsed
            .iter()
            .zip(&collectors)
            .map(|((file, ..), c)| (file.as_str(), &c.mods[..]))
            .collect();
        let krate_of = crate_roots(&mods);
        let mut crates: HashMap<&str, Vec<IndexEntry>> = HashMap::new();
        for ((file, module, ..), c) in parsed.iter().zip(&collectors) {
            if !self.test_files.contains(*file) {
                let entries = c.out.iter().map(|f| index_entry(module, f));
                for krate in &krate_of[file.as_str()] {
                    crates.entry(krate).or_default().extend(entries.clone());
                }
            }
        }
        for ((file, module, src, _), c) in parsed.iter().zip(&collectors) {
            let krates = krate_of[file.as_str()]
                .iter()
                .filter_map(|k| crates.get(k.as_str()).map(Vec::as_slice))
                .collect();
            self.walk_file(file, module, src, c, krates);
        }
        let test_files = std::mem::take(&mut self.test_files);
        self.functions.retain(|f| !test_files.contains(&f.file));
        self.findings.retain(|f| !test_files.contains(&f.file));
        let mut seen: HashMap<String, usize> = HashMap::new();
        for f in &mut self.functions {
            let n = seen.entry(f.id.clone()).or_default();
            *n += 1;
            if *n > 1 {
                f.id = format!("{}#{n}", f.id);
            }
        }
        let mut findings = vec![];
        for f in &self.functions {
            add_findings(&f.nodes, f, &mut findings);
            tally(&f.nodes, &mut self.coverage);
            self.coverage.macro_try += f.macro_try;
        }
        self.coverage.functions = self.functions.len();
        self.findings.append(&mut findings);
        self
    }
}

fn index_entry(module: &str, f: &RawFn) -> IndexEntry {
    let module = std::iter::once(module)
        .chain(f.path.iter().map(String::as_str))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("::");
    IndexEntry {
        owner: f.owner.clone(),
        name: f.sig.ident.to_string(),
        returns: returns(&f.sig.output),
        module,
    }
}

/// Where `mod name;` declared in `file` can live.
fn mod_files(file: &str, name: &str) -> [String; 2] {
    let path = Path::new(file);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let parent = path.parent().unwrap_or(Path::new(""));
    let dir = if ["lib", "main", "mod"].contains(&&*stem) {
        parent.to_path_buf()
    } else {
        parent.join(&*stem)
    };
    [
        dir.join(format!("{name}.rs")),
        dir.join(name).join("mod.rs"),
    ]
    .map(|p| p.display().to_string())
}

/// Each file's crates, named by their root files. Crates grow from their roots
/// along `mod x;`, as rustc builds them: `src/lib.rs`, then `src/main.rs` and
/// `src/bin/*`, then any other `lib.rs`/`main.rs` nothing declares, which is
/// where a manifest's custom `path` usually points. A file no root reaches
/// (`#[path]`, a `mod` inside a macro) joins its package's lib, if it has one.
/// `mod shared;` in both lib.rs and main.rs puts shared.rs in both crates.
// Roots come from file names, so the analysis needs no Cargo.toml.
fn crate_roots<'f>(mods: &HashMap<&'f str, &[String]>) -> HashMap<&'f str, Vec<String>> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let rank = |file: &str| {
        let p = Path::new(file);
        let up = |n: usize| p.ancestors().nth(n).and_then(name);
        let (me, dir) = (up(0), up(1));
        let src = Some("src".to_string());
        match (me.as_deref(), dir.as_deref()) {
            (Some("lib.rs"), _) if up(1) == src => Some(0),
            (Some("main.rs"), _) if up(1) == src => Some(1),
            (_, Some("bin")) if up(2) == src => Some(1),
            (Some("main.rs"), _) if up(2).as_deref() == Some("bin") && up(3) == src => Some(1),
            (Some("lib.rs" | "main.rs"), _) => Some(2),
            _ => None,
        }
    };
    let mut roots: Vec<_> = mods.keys().filter_map(|f| Some((rank(f)?, *f))).collect();
    roots.sort();
    let mut out: HashMap<&str, Vec<String>> = HashMap::new();
    for (_, root) in roots {
        if out.contains_key(root) {
            continue; // a `lib.rs` some crate declares as a module
        }
        let mut todo = vec![root.to_string()];
        let mut seen = HashSet::new();
        while let Some(f) = todo.pop() {
            let Some((&file, children)) = mods.get_key_value(f.as_str()) else {
                continue;
            };
            if seen.insert(file) {
                out.entry(file).or_default().push(root.to_string());
                todo.extend(children.iter().flat_map(|m| mod_files(file, m)));
            }
        }
    }
    for &file in mods.keys() {
        if !out.contains_key(file) {
            let lib = crate_of(file).join("src/lib.rs").display().to_string();
            let krate = if mods.contains_key(lib.as_str()) {
                lib
            } else {
                crate_of(file).display().to_string()
            };
            out.insert(file, vec![krate]);
        }
    }
    out
}

/// `a/src/net/tcp.rs` -> `a`: the package a file belongs to, by its last `src`
/// directory. A file outside any `src` belongs to its own directory.
fn crate_of(file: &str) -> &Path {
    let path = Path::new(file);
    path.ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "src"))
        .and_then(Path::parent)
        .or(path.parent())
        .unwrap_or(Path::new(""))
}

/// `src/net/mod.rs` -> `net`, `src/net/tcp.rs` -> `net::tcp`, `src/lib.rs` -> ``.
pub(crate) fn module_of(rel: &Path) -> String {
    let mut parts: Vec<String> = rel
        .with_extension("")
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(i) = parts.iter().position(|p| p == "src") {
        parts.remove(i); // `billing/src/lib.rs` is the crate `billing`
    }
    if parts
        .last()
        .is_some_and(|p| ["lib", "main", "mod"].contains(&p.as_str()))
    {
        parts.pop();
    }
    parts.join("::")
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let (path, name) = (e.path(), e.file_name());
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                rust_files(&path, out)?;
            }
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Analyses one file (ids have no module prefix) or a directory tree.
pub fn analyse_path(root: &Path) -> std::io::Result<Report> {
    let mut report = Report::new();
    let mut files = vec![];
    if root.is_dir() {
        rust_files(root, &mut files)?;
    } else {
        files.push(root.to_path_buf());
    }
    for path in files {
        let file = path.display().to_string();
        let module = path
            .strip_prefix(root)
            .ok()
            .filter(|r| !r.as_os_str().is_empty())
            .map_or(String::new(), module_of);
        match std::fs::read_to_string(&path) {
            Ok(src) => report.add_source(&file, &module, &src),
            Err(e) if root.is_dir() => report.parse_failures.push(ParseFailure {
                file,
                message: e.to_string(),
            }),
            Err(e) => return Err(e),
        }
    }
    Ok(report.finish())
}

/// Mermaid-safe quoted text. Labels come from the code under review, so a
/// backtick mustn't be able to close the fence around a diagram.
fn q(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    format!("\"{}\"", s.replace('"', "#quot;").replace('`', "#96;"))
}

struct Renderer {
    out: String,
    /// Edges go after all node definitions, so an edge into a subgraph
    /// doesn't drag its source node into that box.
    edges: String,
    next: usize,
    loops: Vec<LoopFrame>,
}

/// An enclosing loop or labelled block: the edges `break` sends out of it, and
/// for a loop, where `continue` goes.
struct LoopFrame {
    tag: Option<String>,
    head: Option<String>,
    breaks: Vec<Exit>,
}

/// An open edge waiting for its target: (from node id, edge label).
type Exit = (String, Option<String>);

impl Renderer {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn connect(&mut self, from: &[Exit], to: &str) {
        for (f, label) in from {
            let label = match label.as_deref() {
                None => String::new(),
                Some("ok") => "|ok|".into(),
                Some(l) => format!("|{}|", q(l)),
            };
            self.edges += &format!("  {f} -->{label} {to}\n");
        }
    }

    /// Render a sequence; returns the edges still flowing out of its end.
    fn seq(&mut self, nodes: &[Node], mut open: Vec<Exit>) -> Vec<Exit> {
        for n in nodes {
            match n {
                Node::Step {
                    name,
                    error,
                    source,
                    ..
                } => {
                    let id = self.id("S");
                    let e = self.id("E");
                    let err = match (error.as_deref(), source) {
                        (None, _) => "unknown error".to_string(),
                        (Some(e), ErrorSource::Enclosing) if e != "None" => format!("→ {e}"),
                        (Some(e), _) => e.to_string(),
                    };
                    self.out += &format!("  {id}[{}]\n  {e}[/{}/]\n", q(&cap(name)), q(&cap(&err)));
                    self.connect(&open, &id);
                    self.edges += &format!("  {id} -->|err| {e}\n");
                    open = vec![(id, Some("ok".into()))];
                }
                Node::Branch { label, arms } => {
                    let id = self.id("D");
                    self.out += &format!("  {id}{{{}}}\n", q(&cap(label)));
                    self.connect(&open, &id);
                    open = arms
                        .iter()
                        .flat_map(|a| self.seq(&a.body, vec![(id.clone(), Some(cap(&a.label)))]))
                        .collect();
                }
                Node::Scope { label, body } => {
                    let id = self.id("G");
                    self.out += &format!("  subgraph {id}[{}]\n", q(label));
                    open = self.seq(body, open);
                    self.out += "  end\n";
                }
                Node::Loop {
                    label,
                    tag,
                    can_skip,
                    head: condition,
                    body,
                } => {
                    let (group, head) = (self.id("G"), self.id("H"));
                    self.out += &format!(
                        "  subgraph {group}[{}]\n  {head}((\"repeat\"))\n",
                        q(&cap(label))
                    );
                    self.connect(&open, &head);
                    self.loops.push(LoopFrame {
                        tag: tag.clone(),
                        head: Some(head.clone()),
                        breaks: vec![],
                    });
                    let checked = self.seq(condition, vec![(head.clone(), None)]);
                    let end_of_body = self.seq(body, checked.clone());
                    self.connect(&end_of_body, &head);
                    open = self.loops.pop().map_or(vec![], |f| f.breaks);
                    if *can_skip {
                        open.extend(
                            checked
                                .into_iter()
                                .map(|(from, _)| (from, Some("done".into()))),
                        );
                    }
                    self.out += "  end\n";
                }
                Node::Fail { error, .. } => {
                    let id = self.id("E");
                    self.out += &format!("  {id}[/{}/]\n", q(&cap(error)));
                    self.connect(&open, &id);
                    return vec![]; // this path never rejoins
                }
                Node::Succeed => {
                    self.connect(&open, "Done");
                    return vec![];
                }
                Node::Block { tag, body } => {
                    let group = self.id("G");
                    self.out += &format!("  subgraph {group}[{}]\n", q(&format!("{tag}: block")));
                    self.loops.push(LoopFrame {
                        tag: Some(tag.clone()),
                        head: None,
                        breaks: vec![],
                    });
                    open = self.seq(body, open);
                    open.extend(self.loops.pop().map_or(vec![], |f| f.breaks));
                    self.out += "  end\n";
                }
                Node::Convert { .. } => {} // not an exit, so not on the railway
                Node::Jump { name, .. } => {
                    let id = self.id("J");
                    self.out += &format!("  {id}([{}])\n", q(name));
                    self.connect(&open, &id);
                    let (word, target) = name
                        .split_once(' ')
                        .map_or((name.as_str(), None), |(w, t)| (w, Some(t)));
                    let frame = match target {
                        Some(t) => self.loops.iter().rposition(|f| f.tag.as_deref() == Some(t)),
                        None => self.loops.iter().rposition(|f| f.head.is_some()),
                    };
                    let Some(frame) = frame else {
                        open = vec![(id, None)]; // no target in sight: invalid Rust
                        continue;
                    };
                    match (word, self.loops[frame].head.clone()) {
                        ("continue", Some(head)) => self.connect(&[(id, None)], &head),
                        _ => self.loops[frame].breaks.push((id, None)),
                    }
                    return vec![];
                }
                Node::Panic { name, terminal, .. } => {
                    let id = self.id("P");
                    self.out += &format!("  {id}{{{{{}}}}}\n", q(&cap(name)));
                    self.connect(&open, &id);
                    if *terminal {
                        return vec![];
                    }
                    let e = self.id("E");
                    self.out += &format!("  {e}[/\"panic\"/]\n");
                    self.edges += &format!("  {id} -->|err| {e}\n");
                    open = vec![(id, Some("ok".into()))];
                }
                Node::Swallow { name, how, .. } => {
                    let id = self.id("W");
                    self.out += &format!(
                        "  {id}{{{{{}}}}}\n",
                        q(&cap(&format!("dropped: {name} ({how})")))
                    );
                    self.connect(&open, &id);
                    open = vec![(id, None)];
                }
                Node::Recover { name, how, .. } => {
                    let id = self.id("R");
                    self.out +=
                        &format!("  {id}[{}]\n", q(&cap(&format!("handled: {name} ({how})"))));
                    self.connect(&open, &id);
                    open = vec![(id, None)];
                }
            }
        }
        open
    }
}

pub fn render(fn_name: &str, nodes: &[Node]) -> String {
    let mut r = Renderer {
        out: format!(
            "flowchart LR\n  Start([{}])\n  Done((Success))\n",
            q(fn_name)
        ),
        edges: String::new(),
        next: 0,
        loops: vec![],
    };
    let open = r.seq(nodes, vec![("Start".into(), None)]);
    r.connect(&open, "Done");
    r.out + &r.edges
}

#[cfg(test)]
mod mutation_tests;
#[cfg(test)]
mod tests;
