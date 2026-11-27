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
pub const NO_TRACE: &str = "no-trace";
pub const BAD_ALLOW: &str = "bad-allow";
const ALLOWABLE: &[&str] = &[NO_PANIC, NO_SWALLOW, NO_TRACE];

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
    /// The handler of an error stops it without recording it anywhere
    /// OpenTelemetry can see: no `tracing` event, nothing on a span. It goes
    /// right after the recovery or fork it belongs to, and draws as nothing.
    Unrecorded {
        name: String,
        how: String,
        line: usize,
        /// `Possible` when the handler logs with a macro whose crate syntax
        /// can't resolve, like a bare `warn!` with no `use`.
        confidence: Confidence,
        allowed: Option<String>,
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
    /// Located in `Report::unanalysed_try`.
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
    /// `file:line` of each `?` counted in `coverage.item_macro_try`.
    pub unanalysed_try: Vec<String>,
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

struct RawConst<'ast> {
    path: Vec<String>,
    owner: Option<String>,
    ident: &'ast syn::Ident,
    ty: &'ast Type,
    expr: &'ast Expr,
}

/// A struct field that holds a fn pointer or closure, and what that returns.
struct RawField {
    path: Vec<String>,
    owner: String,
    name: String,
    returns: Returns,
    /// The struct's type parameters, which `returns` may name.
    params: Vec<String>,
}

/// The outermost closures in an expression; ones inside them are theirs.
#[derive(Default)]
struct Closures<'ast>(Vec<&'ast syn::ExprClosure>);

impl<'ast> Visit<'ast> for Closures<'ast> {
    fn visit_item(&mut self, _: &'ast syn::Item) {}

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.0.push(c);
    }
}

/// A closure stored in a `const` or `static`.
struct ConstClosure<'ast> {
    closure: &'ast syn::ExprClosure,
    /// The struct field it fills, if any.
    label: Option<String>,
    returns: Option<Returns>,
}

/// The closures in a `const` initialiser, with what each returns when a type
/// says: its own `-> T`, else the field it fills, else the item's type.
/// `label` names the field `e` fills, empty for none.
fn const_closures<'ast>(
    index: &Index,
    site: &Site,
    e: &'ast Expr,
    label: &str,
    expect: Option<Returns>,
    out: &mut Vec<ConstClosure<'ast>>,
) {
    let start = out.len();
    match e {
        Expr::Closure(c) => out.push(ConstClosure {
            closure: c,
            label: None,
            returns: closure_returns(c).or(expect),
        }),
        Expr::Array(a) => a
            .elems
            .iter()
            .for_each(|x| const_closures(index, site, x, "", expect.clone(), out)),
        Expr::Reference(r) => const_closures(index, site, &r.expr, "", expect, out),
        Expr::Paren(p) => const_closures(index, site, &p.expr, "", expect, out),
        Expr::Struct(s) => struct_closures(index, site, s, out),
        // `{ ..; || .. }`: the block's value is what the type describes.
        Expr::Block(b) => match b.block.stmts.split_last() {
            Some((Stmt::Expr(tail, None), rest)) => {
                rest.iter().for_each(|s| untyped(|f| f.visit_stmt(s), out));
                const_closures(index, site, tail, "", expect, out);
            }
            _ => untyped(|f| f.visit_block(&b.block), out),
        },
        // `Box::new(|| ..)` for a `Box<dyn Fn() -> R>` field: the type reads through.
        Expr::Call(c) if is_wrapper_new(c) => c
            .args
            .iter()
            .for_each(|x| const_closures(index, site, x, "", expect.clone(), out)),
        other => untyped(|f| f.visit_expr(other), out),
    }
    // The innermost field names a closure: `a: S { b: || .. }` is `b`.
    if !label.is_empty() {
        for c in &mut out[start..] {
            c.label.get_or_insert_with(|| label.to_string());
        }
    }
}

/// Closures no type describes, typed only by their own `-> T`.
fn untyped<'ast>(find: impl FnOnce(&mut Closures<'ast>), out: &mut Vec<ConstClosure<'ast>>) {
    let mut found = Closures::default();
    find(&mut found);
    out.extend(found.0.into_iter().map(|c| ConstClosure {
        closure: c,
        label: None,
        returns: closure_returns(c),
    }));
}

/// Each field of a struct literal, expecting what the field's type returns.
fn struct_closures<'ast>(
    index: &Index,
    site: &Site,
    s: &'ast syn::ExprStruct,
    out: &mut Vec<ConstClosure<'ast>>,
) {
    let path: Vec<_> = s
        .path
        .segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect();
    // `Job::<String> { .. }`, else the item's type when it names this struct.
    let mut args = s.path.segments.last().map(type_args).unwrap_or_default();
    if args.is_empty()
        && let Some(owner) = path.last()
    {
        args = struct_type_args(site.item_ty, owner);
    }
    for f in &s.fields {
        let name = match &f.member {
            syn::Member::Named(n) => n.to_string(),
            syn::Member::Unnamed(i) => i.index.to_string(),
        };
        let expect = index.field(&path, &name, site, &args);
        const_closures(index, site, &f.expr, &name, expect, out);
    }
}

/// `Box::new(x)`, `Arc::new(x)`, `Rc::new(x)`: the wrapped value is the closure.
fn is_wrapper_new(c: &syn::ExprCall) -> bool {
    let Expr::Path(p) = &*c.func else {
        return false;
    };
    let names: Vec<_> = p
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    c.args.len() == 1
        && matches!(&names[..], [.., w, new] if new == "new" && ["Box", "Arc", "Rc"].contains(&w.as_str()))
}

fn closure_returns(c: &syn::ExprClosure) -> Option<Returns> {
    match &c.output {
        ReturnType::Default => None,
        out => Some(returns(out)),
    }
}

/// What `?` converts into in a body returning this.
fn default_err(r: &Returns) -> Option<String> {
    match r {
        Returns::Result { error } => Some(error.clone()),
        Returns::Option => Some("None".into()),
        Returns::Other => None,
    }
}

/// What a fn pointer or `Fn` trait type returns, through arrays, references
/// and wrappers such as `Box<dyn Fn() -> R>`.
// A wrapper is read through its first type argument.
fn callable_returns(ty: &Type) -> Option<Returns> {
    match ty {
        Type::FnPtr(f) => Some(returns(&f.output)),
        Type::Array(a) => callable_returns(&a.elem),
        Type::Slice(s) => callable_returns(&s.elem),
        Type::Reference(r) => callable_returns(&r.elem),
        Type::Paren(p) => callable_returns(&p.elem),
        Type::Group(g) => callable_returns(&g.elem),
        Type::TraitObject(t) => fn_bound(&t.bounds),
        Type::ImplTrait(t) => fn_bound(&t.bounds),
        Type::Path(p) => first_type_arg(p).and_then(callable_returns),
        _ => None,
    }
}

/// `Fn(..) -> R`, `FnMut` or `FnOnce` among a type's bounds.
fn fn_bound(bounds: &Punctuated<syn::TypeParamBound, Token![+]>) -> Option<Returns> {
    bounds.iter().find_map(|b| {
        let syn::TypeParamBound::Trait(t) = b else {
            return None;
        };
        let seg = t.path.segments.last()?;
        let syn::PathArguments::Parenthesized(args) = &seg.arguments else {
            return None;
        };
        ["Fn", "FnMut", "FnOnce"]
            .iter()
            .any(|f| seg.ident == f)
            .then(|| returns(&args.output))
    })
}

/// The type arguments of a path segment, as text: `String` in `Job<String>`.
fn type_args(seg: &syn::PathSegment) -> Vec<String> {
    let syn::PathArguments::AngleBracketed(a) = &seg.arguments else {
        return vec![];
    };
    (a.args.iter())
        .filter_map(|g| match g {
            syn::GenericArgument::Type(t) => Some(text(t)),
            _ => None,
        })
        .collect()
}

/// The type arguments `ty` gives struct `owner`, through arrays and references.
fn struct_type_args(ty: &Type, owner: &str) -> Vec<String> {
    match ty {
        Type::Path(p) => match p.path.segments.last() {
            Some(seg) if seg.ident == owner => type_args(seg),
            _ => vec![],
        },
        Type::Array(a) => struct_type_args(&a.elem, owner),
        Type::Slice(s) => struct_type_args(&s.elem, owner),
        Type::Reference(r) => struct_type_args(&r.elem, owner),
        Type::Paren(p) => struct_type_args(&p.elem, owner),
        _ => vec![],
    }
}

/// `returns` with each type parameter in its error replaced by its argument:
/// `E` and `E::Assoc` are the parameter, `errors::E` isn't.
fn substitute(returns: Returns, params: &[String], args: &[String]) -> Returns {
    let Returns::Result { error } = returns else {
        return returns;
    };
    let Ok(ty) = syn::parse_str::<Type>(&error) else {
        return Returns::Result { error };
    };
    let mut uses = ParamUses { params, at: vec![] };
    uses.visit_type(&ty);
    let mut chars: Vec<char> = error.chars().collect();
    // Back to front, so earlier columns stay put.
    uses.at.sort_unstable_by(|a, b| b.cmp(a));
    for (column, i) in uses.at {
        if let Some(arg) = args.get(i) {
            chars.splice(column..column + params[i].chars().count(), arg.chars());
        }
    }
    Returns::Result {
        error: chars.into_iter().collect(),
    }
}

/// Where a type names one of `params`: (column, which).
struct ParamUses<'p> {
    params: &'p [String],
    at: Vec<(usize, usize)>,
}

impl<'ast> Visit<'ast> for ParamUses<'_> {
    fn visit_type_path(&mut self, t: &'ast syn::TypePath) {
        if t.qself.is_none()
            && t.path.leading_colon.is_none()
            && let Some(first) = t.path.segments.first()
            && let Some(i) = self.params.iter().position(|p| first.ident == p)
        {
            self.at.push((first.ident.span().start().column, i));
        }
        visit::visit_type_path(self, t);
    }
}

fn first_type_arg(p: &syn::TypePath) -> Option<&Type> {
    let syn::PathArguments::AngleBracketed(a) = &p.path.segments.last()?.arguments else {
        return None;
    };
    a.args.iter().find_map(|g| match g {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    })
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
    /// Lines of `?` in macros outside any fn, e.g. `ffi_fn! { fn f() { a()?; } }`.
    item_macro_try: Vec<usize>,
    /// `const` and `static` items that hold closures.
    consts: Vec<RawConst<'ast>>,
    /// Struct fields that hold closures, so a closure stored in one knows its return type.
    fields: Vec<RawField>,
    fn_depth: usize,
    /// `use` items by the scope they sit in, to tell `tracing::warn!` from `log::warn!`.
    imports: Scopes,
    /// Blocks inside the current fn body: their `use` items are read where
    /// they apply, by `block_uses`.
    blocks: usize,
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
        let blocks = std::mem::take(&mut self.blocks);
        block.stmts.iter().for_each(|s| self.visit_stmt(s));
        self.blocks = blocks;
        self.fn_depth -= 1;
        self.owner = owner;
        self.path.pop();
    }

    /// A `const` or `static` holding a closure, walked like a fn whose body is
    /// its initialiser. Without one it's evaluated at compile time, and can't
    /// hold a `?`. Still descended into for nested items and macros.
    fn add_const(&mut self, ident: &'ast syn::Ident, ty: &'ast Type, expr: &'ast Expr) {
        let mut found = Closures::default();
        found.visit_expr(expr);
        if !found.0.is_empty() {
            self.consts.push(RawConst {
                path: self.path.clone(),
                owner: self.owner.clone(),
                ident,
                ty,
                expr,
            });
        }
        self.visit_expr(expr);
    }

    fn within(&mut self, segment: String, owner: Option<String>, f: impl FnOnce(&mut Self)) {
        self.path.push(segment);
        let saved = std::mem::replace(&mut self.owner, owner);
        let blocks = std::mem::take(&mut self.blocks);
        f(self);
        self.blocks = blocks;
        self.owner = saved;
        self.path.pop();
    }
}

impl<'ast> Visit<'ast> for Collector<'ast> {
    fn visit_block(&mut self, b: &'ast Block) {
        self.blocks += 1;
        visit::visit_block(self, b);
        self.blocks -= 1;
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        if self.blocks > 0 {
            return;
        }
        let scope = self.imports.names.entry(self.path.clone()).or_default();
        scope.add(&u.tree, &mut vec![]);
    }

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

    fn visit_trait_item_const(&mut self, c: &'ast syn::TraitItemConst) {
        if let Some((_, expr)) = &c.default
            && !is_test(&c.attrs)
        {
            self.add_const(&c.ident, &c.ty, expr);
        }
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !is_test(&m.attrs) {
            // `mod_files` maps top-level `mod x;` declarations to files.
            if m.content.is_none() && self.path.is_empty() {
                self.mods.push(m.ident.to_string());
            }
            let mut module = self.path.clone();
            module.push(m.ident.to_string());
            self.imports.modules.insert(module);
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

    /// A closure body is walked (in a fn, or as a const's own function), so its
    /// macros aren't unanalysed `?` sites.
    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.fn_depth += 1;
        visit::visit_expr_closure(self, c);
        self.fn_depth -= 1;
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if self.fn_depth == 0 {
            self.item_macro_try.extend(try_lines(m.tokens.clone()));
        }
    }

    fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
        if !is_test(&c.attrs) {
            self.add_const(&c.ident, &c.ty, &c.expr);
        }
    }

    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        if is_test(&s.attrs) {
            return;
        }
        for f in &s.fields {
            if let (Some(name), Some(returns)) = (&f.ident, callable_returns(&f.ty)) {
                self.fields.push(RawField {
                    path: self.path.clone(),
                    owner: s.ident.to_string(),
                    name: name.to_string(),
                    returns,
                    params: s
                        .generics
                        .type_params()
                        .map(|p| p.ident.to_string())
                        .collect(),
                });
            }
        }
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        if !is_test(&s.attrs) {
            self.add_const(&s.ident, &s.ty, &s.expr);
        }
    }

    fn visit_impl_item_const(&mut self, c: &'ast syn::ImplItemConst) {
        if !is_test(&c.attrs) {
            self.add_const(&c.ident, &c.ty, &c.expr);
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

#[derive(Clone)]
struct IndexEntry {
    owner: Option<String>,
    name: String,
    returns: Returns,
    /// Module path of the file plus any inline `mod`s, e.g. `app::db`.
    module: String,
    /// A struct field holding a fn pointer or closure, not a fn: `(s.build)(x)`
    /// isn't a method call, so only `Index::field` finds it.
    field: bool,
    /// A field's struct's type parameters, which `returns` may name.
    params: Vec<String>,
}

/// A module a struct path can name.
enum Within {
    Exact(String),
    /// This module or any ending in it: `a::Job` through a `use`.
    EndsIn(String),
    Any,
}

impl Within {
    fn matches(&self, m: &str) -> bool {
        match self {
            Within::Exact(x) => m == x,
            Within::EndsIn(x) => m == x || m.ends_with(&format!("::{x}")),
            Within::Any => true,
        }
    }
}

fn join_module(base: &str, rest: &[String]) -> String {
    let base = base
        .split("::")
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    base.chain(rest.iter().cloned())
        .collect::<Vec<_>>()
        .join("::")
}

/// Where a struct literal is written: its module, and the `use` items it sees.
struct Site<'a> {
    here: &'a str,
    imports: &'a [&'a Imports],
    /// The const or static's declared type.
    item_ty: &'a Type,
}

impl Site<'_> {
    /// `path` with its first segment replaced by the path a `use` brought it in by.
    fn expand(&self, path: &[String]) -> Vec<String> {
        let Some((first, rest)) = path.split_first() else {
            return vec![];
        };
        match self.imports.iter().find_map(|i| i.paths.get(first)) {
            Some(full) => full.iter().chain(rest).cloned().collect(),
            None => path.to_vec(),
        }
    }
}

/// The modules `qual::Owner` written at `site` can name, most specific first:
/// exact ones, then the modules of its glob imports, then looser matches.
/// `roots` are the crate roots' modules.
// When no exact module, re-export or glob defines it (a `#[path]` file, a
// macro-made module), a bare name falls back to one match anywhere, and a
// relative path to modules ending in it.
fn struct_modules(qual: &[String], site: &Site, roots: &[String]) -> Vec<Within> {
    let mut found = exact_modules(qual, site.here, roots);
    match qual.first().map(String::as_str) {
        None => {
            let globs = site.imports.iter().flat_map(|i| &i.glob_paths);
            found.extend(globs.flat_map(|g| exact_modules(g, site.here, roots)));
            found.push(Within::Any);
        }
        Some("crate" | "self" | "super") => {}
        Some(_) => found.push(Within::EndsIn(qual.join("::"))),
    }
    found
}

/// The modules `qual` names exactly, written in module `here`.
fn exact_modules(qual: &[String], here: &str, roots: &[String]) -> Vec<Within> {
    match qual.first().map(String::as_str) {
        None => vec![Within::Exact(here.into())],
        Some("crate") => roots
            .iter()
            .map(|r| Within::Exact(join_module(r, &qual[1..])))
            .collect(),
        Some("self" | "super") => {
            let rest = if qual[0] == "self" { &qual[1..] } else { qual };
            let ups = rest.iter().take_while(|s| *s == "super").count();
            let mut base: Vec<_> = here.split("::").filter(|s| !s.is_empty()).collect();
            base.truncate(base.len().saturating_sub(ups));
            vec![Within::Exact(join_module(&base.join("::"), &rest[ups..]))]
        }
        Some(_) => std::iter::once(Within::Exact(join_module(here, qual)))
            .chain(roots.iter().map(|r| Within::Exact(join_module(r, qual))))
            .collect(),
    }
}

/// The one return type all the matches agree on, if they do.
fn agreed<'e>(picked: &[&'e IndexEntry]) -> Option<&'e Returns> {
    let first = &picked.first()?.returns;
    picked.iter().all(|e| e.returns == *first).then_some(first)
}

/// Signatures from one file, backed by every crate the file is compiled into.
// Lookups match by name; `use` paths and type information would narrow them.
struct Index<'a> {
    file: Vec<IndexEntry>,
    /// Every crate the file is compiled into. Matches from all of them are
    /// pooled, so crates that disagree leave the callee unknown.
    krates: Vec<&'a [IndexEntry]>,
    /// Modules of the roots of those crates, for `crate::` paths.
    roots: Vec<String>,
    /// The module-level `use` items of those crates, to follow re-exports.
    uses: Vec<&'a [UseEntry]>,
}

/// A module-level `use` item: `name` in `module` is `path`. A glob's name is `*`.
#[derive(Clone)]
struct UseEntry {
    module: String,
    name: String,
    path: Vec<String>,
}

fn use_entries(module: &str, c: &Collector) -> Vec<UseEntry> {
    let mut out = vec![];
    for (at, imports) in &c.imports.names {
        let module = path_id(module, at, "");
        let named = imports.paths.iter().map(|(n, p)| (n.clone(), p));
        let globs = imports.glob_paths.iter().map(|p| ("*".to_string(), p));
        out.extend(named.chain(globs).map(|(name, path)| UseEntry {
            module: module.clone(),
            name,
            path: path.clone(),
        }));
    }
    out
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
        agreed(&picked)
    }

    /// What the closure in field `name` of struct `path` (`Job`, `a::Job`)
    /// returns, for a struct literal written at `site`.
    fn field(&self, path: &[String], name: &str, site: &Site, args: &[String]) -> Option<Returns> {
        let path = site.expand(path);
        let (owner, qual) = path.split_last()?;
        let found = struct_modules(qual, site, &self.roots)
            .into_iter()
            .find_map(|w| Some(self.field_in(&w, owner, name, 0)).filter(|p| !p.is_empty()))?;
        let params = &found.first()?.params;
        Some(substitute(agreed(&found)?.clone(), params, args))
    }

    /// Field `name` of struct `owner` in the modules `w` matches, or brought
    /// into one of them by a `use`: `pub use jobs::Job` makes `crate::Job` it.
    fn field_in(&self, w: &Within, owner: &str, name: &str, depth: usize) -> Vec<&IndexEntry> {
        let direct: Vec<_> = (self.file.iter())
            .chain(self.krates.iter().flat_map(|k| k.iter()))
            .filter(|e| e.field && e.name == name && e.owner.as_deref() == Some(owner))
            .filter(|e| w.matches(&e.module))
            .collect();
        match w {
            // Glob imports can form cycles; real re-export chains are short.
            Within::Exact(m) if direct.is_empty() && depth < 4 => {
                self.reexported(m, owner, name, depth)
            }
            _ => direct,
        }
    }

    fn reexported(&self, module: &str, owner: &str, name: &str, depth: usize) -> Vec<&IndexEntry> {
        let uses = (self.uses.iter().flat_map(|u| u.iter()))
            .filter(|u| u.module == module && (u.name == owner || u.name == "*"));
        let mut found = vec![];
        for u in uses {
            let (target, qual) = match u.path.split_last() {
                Some((target, qual)) if u.name != "*" => (target.as_str(), qual),
                _ => (owner, &u.path[..]),
            };
            for w in exact_modules(qual, module, &self.roots) {
                found.extend(self.field_in(&w, target, name, depth + 1));
            }
        }
        found
    }

    fn in_file(&self, c: &Callee, owner: Option<&str>) -> Vec<&IndexEntry> {
        let named = || self.file.iter().filter(|e| e.name == c.name && !e.field);
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
            .filter(|e| e.name == c.name && !e.field);
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
    try_lines(tokens).len()
}

/// The line of each `?` token.
fn try_lines(tokens: TokenStream) -> Vec<usize> {
    tokens
        .into_iter()
        .flat_map(|t| match t {
            TokenTree::Punct(p) if p.as_char() == '?' => vec![p.span().start().line],
            TokenTree::Group(g) => try_lines(g.stream()),
            _ => vec![],
        })
        .collect()
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

/// Where a name is written: its line and column.
type Pos = (usize, usize);

fn ident_at(i: &syn::Ident) -> Pos {
    let at = i.span().start();
    (at.line, at.column)
}

/// Use sites of the locals a fn hands on, each resolved to its own binding,
/// so a namesake in an inner block or closure doesn't count.
fn handed_on_fn(sig: &syn::Signature, b: &Block) -> HashSet<Pos> {
    let mut names = Bindings::default();
    names.plain(|names| {
        for arg in &sig.inputs {
            if let syn::FnArg::Typed(t) = arg {
                names.bind(&t.pat);
            }
        }
        names.visit_block(b);
    });
    let mut h = HandedOn::default();
    // A fn returning `()` hands nothing on through its value.
    if !matches!(&sig.output, ReturnType::Default) && !is_unit(&sig.output) {
        h.tail(b);
    }
    h.visit_block(b);
    h.settle(&names);
    names.uses_of(&h.uses)
}

fn is_unit(out: &ReturnType) -> bool {
    matches!(out, ReturnType::Type(_, t) if matches!(&**t, Type::Tuple(t) if t.elems.is_empty()))
}

fn handed_on_closure(c: &syn::ExprClosure) -> HashSet<Pos> {
    let mut names = Bindings::default();
    names.visit_expr_closure(c);
    let mut h = HandedOn::default();
    h.run(c);
    h.settle(&names);
    names.uses_of(&h.uses)
}

/// Each use of a local, by position, mapped to the binding it names.
// A macro's names are seen when its tokens parse as expressions, as in `vec![r?]`.
#[derive(Default)]
struct Bindings {
    scopes: Vec<Scope>,
    uses: HashMap<Pos, Pos>,
}

#[derive(Default)]
struct Scope {
    names: Vec<(String, Pos)>,
    /// Runs whenever the scope around it does: a plain block, not a branch,
    /// loop or closure body.
    plain: bool,
}

impl Bindings {
    /// A scope that may not run.
    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.within(false, f);
    }

    fn plain(&mut self, f: impl FnOnce(&mut Self)) {
        self.within(true, f);
    }

    fn within(&mut self, plain: bool, f: impl FnOnce(&mut Self)) {
        self.scopes.push(Scope {
            names: vec![],
            plain,
        });
        f(self);
        self.scopes.pop();
    }

    fn bind(&mut self, p: &syn::Pat) {
        let mut at = PatAt::default();
        at.visit_pat(p);
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.extend(at.0);
        }
    }

    /// Every use of a binding that one of `handed` names.
    fn uses_of(&self, handed: &[Pos]) -> HashSet<Pos> {
        let decls: HashSet<_> = handed.iter().filter_map(|u| self.uses.get(u)).collect();
        self.uses
            .iter()
            .filter(|(_, d)| decls.contains(d))
            .map(|(u, _)| *u)
            .collect()
    }
}

/// Every identifier a pattern binds, with where it's written.
#[derive(Default)]
struct PatAt(Vec<(String, Pos)>);

impl<'ast> Visit<'ast> for PatAt {
    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        let name = p.ident.to_string();
        // `None` or `Ordering::Less` written bare is a constant, not a binding.
        if name.starts_with(|c: char| c.is_lowercase() || c == '_') {
            self.0.push((name, ident_at(&p.ident)));
        }
        visit::visit_pat_ident(self, p);
    }
}

impl<'ast> Visit<'ast> for Bindings {
    fn visit_item(&mut self, _: &'ast syn::Item) {}

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        for e in macro_exprs(m) {
            <Self as Visit<'_>>::visit_expr(self, &e);
        }
    }

    /// `r = x` that runs whenever `r`'s scope does replaces `r`: later uses
    /// name the new value. Inside a branch, loop or closure it may not run, so
    /// the old one stays.
    fn visit_expr_assign(&mut self, a: &'ast syn::ExprAssign) {
        self.visit_expr(&a.right);
        let Expr::Path(p) = &*a.left else {
            return self.visit_expr(&a.left);
        };
        let Some(i) = p.path.get_ident() else { return };
        let declared = (self.scopes.iter()).rposition(|s| s.names.iter().any(|(n, _)| i == n));
        match declared {
            Some(at) if self.scopes[at + 1..].iter().all(|s| s.plain) => {
                self.scopes[at].names.push((i.to_string(), ident_at(i)));
            }
            _ => self.visit_expr_path(p),
        }
    }

    fn visit_block(&mut self, b: &'ast Block) {
        self.plain(|s| visit::visit_block(s, b));
    }

    /// A labelled block can be left early with `break 'label`.
    fn visit_expr_block(&mut self, b: &'ast syn::ExprBlock) {
        match b.label {
            Some(_) => self.scoped(|s| s.visit_block(&b.block)),
            None => self.visit_block(&b.block),
        }
    }

    fn visit_expr_loop(&mut self, l: &'ast syn::ExprLoop) {
        self.scoped(|s| s.visit_block(&l.body));
    }

    fn visit_expr_async(&mut self, a: &'ast syn::ExprAsync) {
        self.scoped(|s| s.visit_block(&a.block));
    }

    /// The right of `&&` and `||` may not run.
    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        self.visit_expr(&b.left);
        match b.op {
            syn::BinOp::And(_) | syn::BinOp::Or(_) => self.scoped(|s| s.visit_expr(&b.right)),
            _ => self.visit_expr(&b.right),
        }
    }

    fn visit_local(&mut self, l: &'ast syn::Local) {
        if let Some(init) = &l.init {
            self.visit_local_init(init);
        }
        self.bind(&l.pat);
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.scoped(|s| {
            c.inputs.iter().for_each(|p| s.bind(p));
            s.visit_expr(&c.body);
        });
    }

    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        self.scoped(|s| {
            s.bind(unguarded(&a.pat));
            if let syn::Pat::Guard(g) = &a.pat {
                s.visit_expr(&g.guard);
            }
            s.visit_expr(&a.body);
        });
    }

    /// `if let` bindings reach the then branch only.
    fn visit_expr_if(&mut self, i: &'ast syn::ExprIf) {
        self.scoped(|s| {
            s.visit_expr(&i.cond);
            s.visit_block(&i.then_branch);
        });
        if let Some((_, e)) = &i.else_branch {
            self.scoped(|s| s.visit_expr(e));
        }
    }

    fn visit_expr_while(&mut self, w: &'ast syn::ExprWhile) {
        self.scoped(|s| {
            s.visit_expr(&w.cond);
            s.visit_block(&w.body);
        });
    }

    fn visit_expr_let(&mut self, l: &'ast syn::ExprLet) {
        self.visit_expr(&l.expr);
        self.bind(&l.pat);
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        self.scoped(|s| {
            s.bind(&f.pat);
            s.visit_block(&f.body);
        });
    }

    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        let Some(i) = p.path.get_ident() else { return };
        let decl = self.scopes.iter().rev().flat_map(|s| s.names.iter().rev());
        if let Some((_, at)) = decl.into_iter().find(|(n, _)| i == n) {
            self.uses.insert(ident_at(i), *at);
        }
    }
}

/// Uses handed on to the caller: `x?`, `return x`, or `x` as the value of
/// the fn or of a closure that runs where its value goes on.
#[derive(Default)]
struct HandedOn<'a> {
    uses: Vec<Pos>,
    /// `let x = e` and `x = e`, by where `x` is written: if `x` goes on,
    /// so does `e`'s value.
    aliases: Vec<(Pos, &'a Expr)>,
}

impl<'a> HandedOn<'a> {
    /// A closure that runs: its value, `?` and `return` go to whoever called it.
    fn run(&mut self, c: &'a syn::ExprClosure) {
        self.value(&c.body);
        self.visit_expr(&c.body);
    }

    /// Follows values through locals: `let out = traced(|| r); out` hands on
    /// `r`, and `let f = || r; f()` runs `f`. Each alias is followed once.
    fn settle(&mut self, names: &Bindings) {
        let decl = |p: &Pos| names.uses.get(p).copied().unwrap_or(*p);
        let mut pending = std::mem::take(&mut self.aliases);
        loop {
            let handed: HashSet<Pos> = self.uses.iter().map(decl).collect();
            let Some(i) = pending
                .iter()
                .position(|(at, _)| handed.contains(&decl(at)))
            else {
                return;
            };
            let (_, e) = pending.swap_remove(i);
            self.value(e);
            pending.append(&mut self.aliases);
        }
    }

    fn name(&mut self, e: &Expr) {
        if let Expr::Path(p) = e
            && let Some(i) = p.path.get_ident()
        {
            self.uses.push(ident_at(i));
        }
    }

    /// The locals an expression's value can be: through blocks and branches,
    /// and what it builds (`Ok(Job { run: || r })`, `(a, b)`).
    fn value(&mut self, e: &'a Expr) {
        match e {
            Expr::Block(b) => self.tail(&b.block),
            Expr::Paren(p) => self.value(&p.expr),
            Expr::If(i) => {
                self.tail(&i.then_branch);
                if let Some((_, e)) = &i.else_branch {
                    self.value(e);
                }
            }
            Expr::Match(m) => m.arms.iter().for_each(|a| self.value(&a.body)),
            Expr::Await(a) => self.value(&a.base),
            Expr::Call(c) => self.call(c),
            Expr::MethodCall(c) => self.closure_args(&c.args),
            // A closure or future handed on runs wherever it goes.
            Expr::Closure(c) => self.run(c),
            Expr::Async(a) => {
                self.tail(&a.block);
                self.visit_block(&a.block);
            }
            Expr::Struct(s) => s.fields.iter().for_each(|f| self.value(&f.expr)),
            Expr::Tuple(t) => t.elems.iter().for_each(|e| self.value(e)),
            Expr::Array(a) => a.elems.iter().for_each(|e| self.value(e)),
            e => self.name(e),
        }
    }

    fn tail(&mut self, b: &'a Block) {
        if let Some(Stmt::Expr(e, None)) = b.stmts.last() {
            self.value(e);
        }
    }

    /// A call whose value goes on: `traced(|| r)` runs its closure, and `f()`
    /// hands on `f`, so a stored closure `f` runs. A closure whose value goes
    /// nowhere the caller sees (a call made as a statement, `drop`) hands nothing on.
    fn call(&mut self, c: &'a syn::ExprCall) {
        let discards = matches!(&*c.func, Expr::Path(p)
            if p.path.segments.last().is_some_and(|s| s.ident == "drop" || s.ident == "forget"));
        if !discards {
            self.closure_args(&c.args);
            self.name(&c.func);
        }
    }

    fn closure_args(&mut self, args: &'a Punctuated<Expr, Token![,]>) {
        for a in args {
            if let Expr::Closure(c) = a {
                self.run(c);
            }
        }
    }
}

impl<'a> Visit<'a> for HandedOn<'a> {
    fn visit_item(&mut self, _: &'a syn::Item) {}

    /// A closure's body hands on only if the closure runs; see `run`.
    fn visit_expr_closure(&mut self, _: &'a syn::ExprClosure) {}

    /// Likewise a future only if it's polled: awaited or handed on; see `value`.
    fn visit_expr_async(&mut self, _: &'a syn::ExprAsync) {}

    fn visit_local(&mut self, l: &'a syn::Local) {
        let pat = match &l.pat {
            syn::Pat::Type(t) => &*t.pat,
            p => p,
        };
        if let (syn::Pat::Ident(p), Some(init)) = (pat, &l.init) {
            self.aliases.push((ident_at(&p.ident), &init.expr));
        }
        visit::visit_local(self, l);
    }

    fn visit_expr_assign(&mut self, a: &'a syn::ExprAssign) {
        if let Expr::Path(p) = &*a.left
            && let Some(i) = p.path.get_ident()
        {
            self.aliases.push((ident_at(i), &a.right));
        }
        visit::visit_expr_assign(self, a);
    }

    fn visit_expr_try(&mut self, t: &'a syn::ExprTry) {
        self.value(&t.expr);
        visit::visit_expr_try(self, t);
    }

    fn visit_expr_return(&mut self, r: &'a syn::ExprReturn) {
        if let Some(e) = &r.expr {
            self.value(e);
        }
        visit::visit_expr_return(self, r);
    }

    /// A macro's arguments, when they parse, as in `vec![r?]`.
    fn visit_macro(&mut self, m: &'a syn::Macro) {
        for e in macro_exprs(m) {
            let mut inner = HandedOn::default();
            inner.visit_expr(&e);
            self.uses.extend(inner.uses);
        }
    }
}

/// A macro's tokens as comma-separated expressions, when they parse.
fn macro_exprs(m: &syn::Macro) -> Vec<Expr> {
    m.parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated)
        .map(|args| args.into_iter().collect())
        .unwrap_or_default()
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

/// How sure we are that an error handler records what it stops.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Recorded {
    No,
    /// Only through a logging macro whose crate can't be resolved.
    Maybe,
    Yes,
}

/// One scope's `use` items: a module, or a fn body.
#[derive(Default)]
struct Imports {
    /// Name in scope → (root crate, original name).
    names: HashMap<String, (String, String)>,
    /// Roots of `use x::*`.
    globs: Vec<String>,
    /// Name in scope → the whole path it was brought in by.
    paths: HashMap<String, Vec<String>>,
    /// Whole paths of `use x::*`.
    glob_paths: Vec<Vec<String>>,
}

impl Imports {
    /// `use a::b::{self as c}` brings in `a::b` as `c`.
    fn add_path(&mut self, prefix: &[String], ident: &syn::Ident, name: &syn::Ident) {
        let mut path = prefix.to_vec();
        if ident != "self" {
            path.push(ident.to_string());
        }
        let name = match (name == "self", path.last()) {
            (true, Some(last)) => last.clone(),
            _ => name.to_string(),
        };
        self.paths.insert(name, path);
    }

    fn add(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        let root = |prefix: &[String], own: &syn::Ident| {
            prefix.first().cloned().unwrap_or_else(|| own.to_string())
        };
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.add(&p.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(n) => {
                let entry = (root(prefix, &n.ident), n.ident.to_string());
                self.names.insert(n.ident.to_string(), entry);
                self.add_path(prefix, &n.ident, &n.ident);
            }
            syn::UseTree::Rename(r) => {
                let entry = (root(prefix, &r.ident), r.ident.to_string());
                self.names.insert(r.rename.to_string(), entry);
                self.add_path(prefix, &r.ident, &r.rename);
            }
            syn::UseTree::Glob(_) => {
                self.globs.extend(prefix.first().cloned());
                self.glob_paths.push(prefix.clone());
            }
            syn::UseTree::Group(g) => g.items.iter().for_each(|t| self.add(t, prefix)),
        }
    }
}

/// A file's `use` items by scope. Keys are collector paths: modules, impl
/// types and fns. Only modules are marked, since only they stop a lookup.
#[derive(Default)]
struct Scopes {
    names: HashMap<Vec<String>, Imports>,
    modules: HashSet<Vec<String>>,
}

impl Scopes {
    /// What a fn at `path` named `name` can see, innermost first: its own
    /// body, any fns around it, then its module. As in rustc, a module's
    /// `use` items stop at the module: a child module doesn't inherit them.
    fn chain(&self, path: &[String], name: &str) -> Vec<&Imports> {
        let mut own = path.to_vec();
        own.push(name.to_string());
        let mut chain: Vec<&Imports> = self.names.get(&own).into_iter().collect();
        let mut at = path.to_vec();
        loop {
            chain.extend(self.names.get(&at));
            if at.is_empty() || self.modules.contains(&at) {
                return chain;
            }
            at.pop();
        }
    }
}

/// A nested block's own `use` items, in scope only inside it.
fn block_uses(b: &Block) -> Imports {
    let mut imports = Imports::default();
    for s in &b.stmts {
        if let Stmt::Item(syn::Item::Use(u)) = s {
            imports.add(&u.tree, &mut vec![]);
        }
    }
    imports
}

/// The crate a macro comes from, and its original name, through the `use`
/// items in scope: `warn!` after `use tracing::warn`, or `t::warn!` after
/// `use tracing as t`.
fn resolve(chain: &[&Imports], path: &syn::Path) -> (Option<String>, String) {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let lookup = |name: &str| chain.iter().find_map(|i| i.names.get(name));
    match &segments[..] {
        [name] => match lookup(name) {
            Some((root, original)) => (Some(root.clone()), original.clone()),
            None => {
                // Only an unambiguous glob decides.
                let globs: HashSet<&str> = chain
                    .iter()
                    .flat_map(|i| &i.globs)
                    .map(String::as_str)
                    .filter(|g| ["tracing", "log"].contains(g))
                    .collect();
                let root = (globs.len() == 1)
                    .then(|| globs.iter().next().map(|g| g.to_string()))
                    .flatten();
                (root, name.clone())
            }
        },
        [first, .., name] => {
            let root = lookup(first).map_or_else(|| first.clone(), |(root, _)| root.clone());
            (Some(root), name.clone())
        }
        [] => (None, String::new()),
    }
}

/// Standard macros that evaluate each argument as an expression.
const RUNS_ARGUMENTS: [&str; 13] = [
    "print",
    "println",
    "eprint",
    "eprintln",
    "format",
    "format_args",
    "write",
    "writeln",
    "assert",
    "assert_eq",
    "assert_ne",
    "dbg",
    "vec",
];

/// Span methods that record onto the current trace.
const RECORDING_METHODS: [&str; 5] = [
    "record_error",
    "record_exception",
    "add_event",
    "set_status",
    "record",
];

/// How well a logging macro records: `tracing`'s do, `log`'s don't, and an
/// unresolved bare name may be either. Anything else records nothing.
fn macro_records(imports: &[&Imports], path: &syn::Path) -> Recorded {
    let (root, name) = resolve(imports, path);
    if !["error", "warn", "info", "debug", "trace", "event"].contains(&name.as_str()) {
        return Recorded::No;
    }
    match root.as_deref() {
        Some("tracing") => Recorded::Yes,
        Some("log") => Recorded::No,
        _ => Recorded::Maybe,
    }
}

/// `Err`, `Err::<T, E>`, `Result::Err`, `std::result::Result::Err`, or
/// through an alias of `Result` (`io::Result::Err`, `type R = ..; R::Err`)
/// or a rename (`use Result::Err as Fail; Fail(e)`).
fn is_err_ctor(imports: &[&Imports], path: &syn::Path) -> bool {
    match path.segments.iter().collect::<Vec<_>>()[..] {
        [one] => {
            let name = one.ident.to_string();
            match imports.iter().find_map(|i| i.names.get(&name)) {
                Some((_, original)) => original == "Err",
                None => name == "Err",
            }
        }
        [.., last] => last.ident == "Err",
        [] => false,
    }
}

/// `_`, a lowercase binding, or a tuple of them: a pattern that can't fail.
fn always_matches(p: &syn::Pat) -> bool {
    match p {
        syn::Pat::Wild(_) => true,
        syn::Pat::Ident(i) => {
            i.subpat.is_none()
                && i.ident
                    .to_string()
                    .starts_with(|c: char| c.is_lowercase() || c == '_')
        }
        syn::Pat::Paren(p) => always_matches(&p.pat),
        syn::Pat::Tuple(t) => t.elems.iter().all(always_matches),
        _ => false,
    }
}

/// `Err(e)`, `Err(_)`: every error.
fn any_err(p: &syn::Pat) -> bool {
    matches!(p, syn::Pat::TupleStruct(t)
        if t.path.segments.last().is_some_and(|s| s.ident == "Err")
            && t.elems.len() == 1
            && t.elems.iter().all(always_matches))
}

/// `Err(..)` itself, not wrapped.
fn is_err_call(imports: &[&Imports], e: &Expr) -> bool {
    match e {
        Expr::Paren(p) => is_err_call(imports, &p.expr),
        Expr::Group(g) => is_err_call(imports, &g.expr),
        Expr::Call(c) => matches!(&*c.func, Expr::Path(p) if is_err_ctor(imports, &p.path)),
        _ => false,
    }
}

/// `Err(..)`, alone or wrapped in a constructor, tuple, array or struct
/// (`Poll::Ready(Err(e))`, `(Err(e), buf)`): the error carried on as a value.
/// A plain function call (`discard(Err(e))`) may consume it, so it doesn't count.
fn is_err_value(imports: &[&Imports], e: &Expr) -> bool {
    let err = |e: &Expr| is_err_value(imports, e);
    match e {
        Expr::Paren(p) => err(&p.expr),
        Expr::Group(g) => err(&g.expr),
        Expr::Call(c) => match &*c.func {
            Expr::Path(p) if is_err_ctor(imports, &p.path) => true,
            // An enum variant or tuple struct: by convention, capitalised.
            Expr::Path(p) => {
                p.path.segments.last().is_some_and(|s| {
                    s.ident
                        .to_string()
                        .starts_with(|ch: char| ch.is_ascii_uppercase())
                }) && c.args.iter().any(err)
            }
            _ => false,
        },
        Expr::Tuple(t) => t.elems.iter().any(err),
        Expr::Array(a) => a.elems.iter().any(err),
        Expr::Struct(s) => s.fields.iter().any(|f| err(&f.expr)),
        _ => false,
    }
}

/// Where the paths through some code go, and how well each records the error.
#[derive(Clone, Copy, Default)]
struct Paths {
    /// The worst-recorded path that leaves the handler early: `return`, `?`,
    /// or a `break` or `continue` out of it.
    left: Option<Recorded>,
    /// The worst-recorded `break` out of the innermost loop in the handler.
    broke: Option<Recorded>,
    /// The worst-recorded path that reaches the end, or `None` if none does.
    falls: Option<Recorded>,
}

impl Paths {
    fn falls(at: Recorded) -> Self {
        Paths {
            falls: Some(at),
            ..Self::default()
        }
    }

    fn leaves(at: Recorded) -> Self {
        Paths {
            left: Some(at),
            ..Self::default()
        }
    }

    /// Adds another piece of code's early exits to these.
    fn exits(&mut self, other: Paths) {
        self.left = worst(self.left, other.left);
        self.broke = worst(self.broke, other.broke);
    }
}

fn worst(a: Option<Recorded>, b: Option<Recorded>) -> Option<Recorded> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) | (None, a) => a,
    }
}

/// Follows every path through an error handler, so one exit that hands the
/// error on doesn't excuse another that drops it.
struct Flow<'a> {
    imports: &'a [&'a Imports],
    /// Inside a loop that's inside the handler, so `break` stays in the handler.
    looping: bool,
}

impl Flow<'_> {
    fn handler(&self, e: &Expr) -> Recorded {
        let p = self.expr(e, Recorded::No, true);
        worst(p.left, p.falls).unwrap_or(Recorded::Yes) // never ends: nothing dropped
    }

    fn handler_block(&self, b: &Block) -> Recorded {
        let p = self.block(b, Recorded::No, true);
        worst(p.left, p.falls).unwrap_or(Recorded::Yes)
    }

    /// `value`: whether the code's value leaves the handler, so an `Err(..)`
    /// there hands the error on rather than holding it in a local.
    fn block(&self, b: &Block, level: Recorded, value: bool) -> Paths {
        let local = block_uses(b);
        if local.names.is_empty() && local.globs.is_empty() {
            return self.stmts(b, level, value);
        }
        let chain: Vec<&Imports> = std::iter::once(&local)
            .chain(self.imports.iter().copied())
            .collect();
        Flow {
            imports: &chain,
            looping: self.looping,
        }
        .stmts(b, level, value)
    }

    fn stmts(&self, b: &Block, mut level: Recorded, value: bool) -> Paths {
        let mut out = Paths::default();
        let n = b.stmts.len();
        for (i, stmt) in b.stmts.iter().enumerate() {
            let p = match stmt {
                Stmt::Expr(e, None) if i + 1 == n => self.expr(e, level, value),
                Stmt::Expr(e, _) => self.expr(e, level, false),
                Stmt::Macro(m) => self.mac(&m.mac, level),
                Stmt::Local(l) => {
                    let Some(init) = &l.init else { continue };
                    let mut p = self.expr(&init.expr, level, false);
                    if let (Some((_, other)), Some(at)) = (&init.diverge, p.falls) {
                        let e = self.expr(other, at, false);
                        p.exits(e);
                        p.left = worst(p.left, e.falls);
                    }
                    p
                }
                Stmt::Item(_) => continue,
            };
            out.exits(p);
            match p.falls {
                Some(at) => level = at,
                None => return out,
            }
        }
        out.falls = Some(level);
        out
    }

    /// The arguments of a macro known to run them (`println!`, `assert!`,
    /// `format!` ..) run first, when they parse as expressions. Any other
    /// macro's tokens may not run at all (`stringify!`), so they're skipped.
    /// A logging or `debug_` macro's may not run, so they can't record, only leave.
    fn mac(&self, m: &syn::Macro, level: Recorded) -> Paths {
        // The original name, through any rename: `use tracing::warn as tw`.
        let name = resolve(self.imports, &m.path).1;
        let panics = PANIC_MACROS.contains(&name.as_str());
        let logs = ["error", "warn", "info", "debug", "trace", "event"].contains(&name.as_str());
        let maybe = logs || name.starts_with("debug_assert");
        let runs = maybe || RUNS_ARGUMENTS.contains(&name.as_str()) || panics;
        let parsed = runs
            .then(|| m.parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated))
            .and_then(Result::ok);
        // An assertion's message runs only when it fails.
        let always = match name.as_str() {
            "assert" => 1,
            "assert_eq" | "assert_ne" => 2,
            _ => usize::MAX,
        };
        let mut p = match parsed {
            Some(args) => {
                let mut p = self.seq(args.iter().take(always), level);
                if let Some(at) = p.falls {
                    let message = self.seq(args.iter().skip(always), at);
                    p.exits(message);
                    p.falls = worst(Some(at), message.falls);
                }
                p
            }
            None => Paths::falls(level),
        };
        if maybe {
            p.falls = worst(Some(level), p.falls);
        }
        if panics {
            p.left = worst(p.left, p.falls.map(|_| Recorded::Yes));
            p.falls = None;
            return p;
        }
        p.falls = p
            .falls
            .map(|at| at.max(macro_records(self.imports, &m.path)));
        p
    }

    /// Expressions in the order they run, each starting where the last ended.
    fn seq<'e>(&self, es: impl IntoIterator<Item = &'e Expr>, mut level: Recorded) -> Paths {
        let mut out = Paths::default();
        for e in es {
            let p = self.expr(e, level, false);
            out.exits(p);
            match p.falls {
                Some(at) => level = at,
                None => return out,
            }
        }
        out.falls = Some(level);
        out
    }

    /// A loop body. A path that reaches its end runs it again from a level
    /// no lower, so the first pass's exits are the worst there are.
    fn body(&self, b: &Block, level: Recorded) -> Paths {
        Flow {
            imports: self.imports,
            looping: true,
        }
        .block(b, level, false)
    }

    fn expr(&self, e: &Expr, level: Recorded, value: bool) -> Paths {
        match e {
            // Its parts run first, and may leave without it.
            _ if value && is_err_value(self.imports, e) => {
                let mut p = self.expr(e, level, false);
                p.falls = p.falls.map(|_| Recorded::Yes);
                p
            }
            Expr::Return(r) => match &r.expr {
                Some(v) => {
                    let p = self.expr(v, level, true);
                    Paths {
                        left: worst(p.left, p.falls),
                        broke: p.broke,
                        falls: None,
                    }
                }
                None => Paths::leaves(level),
            },
            // A labelled one may leave the handler: count it as leaving.
            Expr::Break(b) if self.looping && b.label.is_none() => {
                let mut p = match &b.expr {
                    Some(v) => self.expr(v, level, false),
                    None => Paths::falls(level),
                };
                p.broke = worst(p.broke, p.falls);
                p.falls = None;
                p
            }
            // Back to the top of the loop, at a level no lower.
            Expr::Continue(c) if self.looping && c.label.is_none() => Paths::default(),
            Expr::Break(_) | Expr::Continue(_) => Paths::leaves(level),
            Expr::Macro(m) => self.mac(&m.mac, level),
            // `Err(e)?` hands `e` on; `Ok(Err(e))?` unwraps the `Ok` and keeps it.
            Expr::Try(t) if is_err_call(self.imports, &t.expr) => {
                let p = self.expr(&t.expr, level, false);
                Paths {
                    left: worst(p.left, p.falls.map(|_| Recorded::Yes)),
                    broke: p.broke,
                    falls: None,
                }
            }
            // `fallback()?` leaves with another error: this one is dropped.
            Expr::Try(t) => {
                let mut p = self.expr(&t.expr, level, false);
                p.left = worst(p.left, p.falls);
                p
            }
            Expr::If(i) => {
                let mut out = self.expr(&i.cond, level, false);
                let Some(at) = out.falls else { return out };
                let then = self.block(&i.then_branch, at, value);
                let other = match &i.else_branch {
                    Some((_, e)) => self.expr(e, at, value),
                    None => Paths::falls(at),
                };
                out.exits(then);
                out.exits(other);
                out.falls = worst(then.falls, other.falls);
                out
            }
            Expr::Match(m) => {
                let mut out = self.expr(&m.expr, level, false);
                let Some(at) = out.falls.take() else {
                    return out;
                };
                let arms = self.match_arms(m.arms.iter().map(|a| (a, false)), at, value, false);
                out.exits(arms);
                out.falls = arms.falls;
                out
            }
            Expr::Block(b) => self.block(&b.block, level, value),
            Expr::Unsafe(u) => self.block(&u.block, level, value),
            Expr::Paren(p) => self.expr(&p.expr, level, value),
            Expr::Group(g) => self.expr(&g.expr, level, value),
            // `a && b`, `a || b`: `b` may not run.
            Expr::Binary(b) if matches!(b.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) => {
                let mut out = self.expr(&b.left, level, false);
                let Some(at) = out.falls else { return out };
                let r = self.expr(&b.right, at, false);
                out.exits(r);
                out.falls = worst(Some(at), r.falls);
                out
            }
            Expr::Binary(b) => self.seq([&*b.left, &*b.right], level),
            Expr::Assign(a) => self.seq([&*a.right, &*a.left], level),
            Expr::Call(c) => self.seq(std::iter::once(&*c.func).chain(&c.args), level),
            Expr::MethodCall(m) => {
                let mut p = self.seq(std::iter::once(&*m.receiver).chain(&m.args), level);
                if RECORDING_METHODS.contains(&m.method.to_string().as_str()) {
                    p.falls = p.falls.map(|_| Recorded::Yes);
                }
                p
            }
            Expr::Unary(u) => self.expr(&u.expr, level, false),
            Expr::Reference(r) => self.expr(&r.expr, level, false),
            Expr::Field(f) => self.expr(&f.base, level, false),
            Expr::Cast(c) => self.expr(&c.expr, level, false),
            Expr::Await(a) => self.expr(&a.base, level, false),
            Expr::Let(l) => self.expr(&l.expr, level, false),
            Expr::Index(i) => self.seq([&*i.expr, &*i.index], level),
            Expr::Tuple(t) => self.seq(&t.elems, level),
            Expr::Array(a) => self.seq(&a.elems, level),
            Expr::Struct(s) => self.seq(
                s.fields.iter().map(|f| &f.expr).chain(s.rest.as_deref()),
                level,
            ),
            // `loop` runs its body at least once and ends only by `break`.
            Expr::Loop(l) => {
                let b = self.body(&l.body, level);
                Paths {
                    left: b.left,
                    broke: None,
                    falls: b.broke,
                }
            }
            // `while` and `for` may run their body no times.
            Expr::While(w) => self.maybe_looped(&w.cond, &w.body, level),
            Expr::ForLoop(f) => self.maybe_looped(&f.expr, &f.body, level),
            Expr::Range(r) => self.seq(r.start.iter().chain(&r.end).map(|e| &**e), level),
            // The length is a constant, worked out at compile time.
            Expr::Repeat(r) => self.expr(&r.expr, level, false),
            Expr::RawAddr(r) => self.expr(&r.expr, level, false),
            Expr::Yield(y) => self.seq(y.expr.as_deref(), level),
            // A `?` inside `try { .. }` counts as leaving the handler.
            Expr::TryBlock(t) => self.block(&t.block, level, false),
            // Any call through `autotel` records.
            Expr::Path(p) if p.path.segments.iter().any(|s| s.ident == "autotel") => {
                Paths::falls(Recorded::Yes)
            }
            // Closures and async blocks may never run; a `const` block runs at
            // compile time; the rest (paths, literals ..) run nothing.
            _ => Paths::falls(level),
        }
    }

    /// A match's arms, in order, from `at`. An arm marked as handing the
    /// error back records whatever its body does. `on_err`: only the error
    /// path is followed, so `Err(e)` always matches. The level the next arm is
    /// tried at: a guard that ran and failed passes its level on, at worst the
    /// level before it, unless its pattern always matches and so it always ran.
    fn match_arms<'m>(
        &self,
        arms: impl IntoIterator<Item = (&'m syn::Arm, bool)>,
        at: Recorded,
        value: bool,
        on_err: bool,
    ) -> Paths {
        let mut out = Paths::default();
        let mut reached = Some(at);
        for (arm, handed_back) in arms {
            let Some(tried) = reached else { break };
            let start = match &arm.pat {
                syn::Pat::Guard(g) => {
                    let p = self.expr(&g.guard, tried, false);
                    out.exits(p);
                    reached = if always_matches(&g.pat) || (on_err && any_err(&g.pat)) {
                        p.falls
                    } else {
                        worst(Some(tried), p.falls)
                    };
                    let Some(start) = p.falls else { continue };
                    start
                }
                _ => tried,
            };
            let p = if handed_back {
                Paths::falls(Recorded::Yes)
            } else {
                self.expr(&arm.body, start, value)
            };
            out.exits(p);
            out.falls = worst(out.falls, p.falls);
        }
        out
    }

    /// A direct handler's error arms.
    fn arms<'m>(&self, arms: impl IntoIterator<Item = (&'m syn::Arm, bool)>) -> Recorded {
        let p = self.match_arms(arms, Recorded::No, true, true);
        worst(p.left, p.falls).unwrap_or(Recorded::Yes)
    }

    /// The condition or iterator runs first, outside the loop's own `break`.
    fn maybe_looped(&self, head: &Expr, body: &Block, level: Recorded) -> Paths {
        let mut out = self.expr(head, level, false);
        let Some(at) = out.falls else { return out };
        let b = self.body(body, at);
        out.left = worst(out.left, b.left);
        out.falls = worst(Some(at), b.broke);
        out
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
    /// The `use` items this fn can see, innermost scope first.
    imports: &'a [&'a Imports],
    /// `use` items of the blocks being walked, outermost first.
    block_imports: Vec<Imports>,
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
    /// Uses of locals the fn hands on to its caller; see `handed_on_fn`.
    handed_on: HashSet<Pos>,
}

impl<'a> Walker<'a> {
    fn new(
        index: &'a Index<'a>,
        imports: &'a [&'a Imports],
        allows: &'a [Allow],
        owner: Option<&'a str>,
        default_err: Option<String>,
        handed_on: HashSet<Pos>,
    ) -> Self {
        Walker {
            index,
            imports,
            block_imports: vec![],
            allows,
            owner,
            nodes: vec![],
            tail: true,
            default_err,
            macro_try: 0,
            in_step: false,
            handed_on,
        }
    }

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

    /// `&x` where `x` goes on to the caller: reading the error isn't handling it.
    fn only_reads(&self, e: &Expr) -> bool {
        matches!(e, Expr::Reference(r) if r.mutability.is_none()
            && matches!(&*r.expr, Expr::Path(p)
                if p.path.get_ident().is_some_and(|i| self.handed_on.contains(&ident_at(i)))))
    }

    fn push_branch(&mut self, label: String, arms: Vec<Arm>) {
        // A fork with nothing fallible in any arm is noise on a railway.
        if arms.iter().any(|a| !a.body.is_empty()) {
            self.nodes.push(Node::Branch { label, arms });
        }
    }

    /// A fork on a `Result` whose arms do nothing fallible handles the error
    /// in place, so it goes on the railway as a recovery. Either way, an
    /// error handler that doesn't record what it stops is a finding.
    fn recover_or_branch(
        &mut self,
        label: String,
        arms: Vec<Arm>,
        scrutinee: &Expr,
        how: Option<(String, Vec<String>)>,
        (whole, recorded): (proc_macro2::Span, Recorded),
    ) {
        let handler = how.as_ref().map(|(how, _)| how.clone());
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
        let Some(how) = handler else { return };
        let confidence = match recorded {
            Recorded::Yes => return,
            Recorded::Maybe => Confidence::Possible,
            Recorded::No => Confidence::Certain,
        };
        self.nodes.push(Node::Unrecorded {
            name: step_label(strip_await(scrutinee)).1,
            how,
            line: line(scrutinee),
            confidence,
            allowed: self.allowed(NO_TRACE, whole.start().line, whole.end().line),
        });
    }

    /// How well one error arm records the error it stops, on every path.
    /// Handing it back to the caller counts as recording.
    /// The `use` items in scope here, innermost first.
    fn in_scope(&self) -> Vec<&Imports> {
        self.block_imports
            .iter()
            .rev()
            .chain(self.imports.iter().copied())
            .collect()
    }

    fn arm_records(&self, handed_back: bool, visit: impl FnOnce(&Flow) -> Recorded) -> Recorded {
        if handed_back {
            return Recorded::Yes;
        }
        let chain = self.in_scope();
        visit(&Flow {
            imports: &chain,
            looping: false,
        })
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
    /// `default_err` is what `?` converts into there, when a type says.
    fn scope(&mut self, label: String, default_err: Option<String>, walk: impl FnOnce(&mut Self)) {
        let saved = std::mem::replace(&mut self.default_err, default_err);
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
        let err = closure_returns(c).as_ref().and_then(default_err);
        self.scope(label, err, |w| w.closure_body(c));
    }

    fn closure_body(&mut self, c: &syn::ExprClosure) {
        match &*c.body {
            Expr::Async(a) => self.visit_block(&a.block),
            body => self.tail_expr(body),
        }
    }

    fn args<'e>(&mut self, label: &str, args: impl Iterator<Item = &'e Expr>) {
        for a in args {
            match a {
                Expr::Closure(c) => self.closure(label.to_string(), c),
                Expr::Async(b) => self.scope(label.to_string(), None, |w| w.visit_block(&b.block)),
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
        self.block_imports.push(block_uses(b));
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
        self.block_imports.pop();
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
                let tag = result_pat(&l.pat);
                let how = tag
                    .as_ref()
                    .filter(|_| !self.only_reads(&l.expr))
                    .map(|t| (format!("if let {t}"), vec![canon(&*l.pat)]));
                // The error arm: `then` for `if let Err`, `else` for `if let Ok`.
                let recorded = match (tag.as_deref(), &i.else_branch) {
                    (Some("Err"), _) => {
                        self.arm_records(false, |f| f.handler_block(&i.then_branch))
                    }
                    (_, Some((_, other))) => self.arm_records(false, |f| f.handler(other)),
                    (_, None) => Recorded::No, // `if let Ok(v) = a() { .. }` ignores it
                };
                self.recover_or_branch(label, arms, &l.expr, how, (i.span(), recorded));
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
        let how = (on_result && !opaque && !self.only_reads(&m.expr)).then(|| {
            (
                "match".to_string(),
                m.arms.iter().map(|a| canon(&a.pat)).collect(),
            )
        });
        // Every arm that isn't `Ok(..)` can see the error, its guard included.
        let error_arms = m
            .arms
            .iter()
            .zip(&handed)
            .filter(|(a, _)| result_pat(&a.pat).as_deref() != Some("Ok"))
            .map(|(a, h)| (a, returns && *h == Handed::Binding));
        let recorded = self.arm_records(false, |f| f.arms(error_arms));
        let label = format!("match {}", canon(&*m.expr));
        self.recover_or_branch(label, arms, &m.expr, how, (m.span(), recorded));
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
        self.scope("async block".into(), None, |w| w.visit_block(&a.block));
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.macro_try += count_try(m.tokens.clone());
        // Through any rename: `use std::panic as boom; boom!()`.
        let name = resolve(&self.in_scope(), &m.path).1;
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
            Node::Unrecorded {
                name,
                how,
                line,
                confidence,
                allowed,
            } => out.push(finding(
                NO_TRACE,
                *line,
                format!(
                    "`{how}` {} the error from `{name}` without recording it{}",
                    // `if let Ok(v) = a() { .. }` never looks at the error.
                    if how == "if let Ok" { "ignores" } else { "handles" },
                    match confidence {
                        Confidence::Certain => "",
                        Confidence::Possible => {
                            " (it logs with a macro that may be the `log` crate's: import it from `tracing`)"
                        }
                    }
                ),
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
            unanalysed_try: vec![],
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
        index: &Index,
    ) {
        let (allows, bad) = parse_allows(src);
        for f in &collector.out {
            let ret = returns(&f.sig.output);
            let default_err = default_err(&ret);
            let chain = collector.imports.chain(&f.path, &f.sig.ident.to_string());
            let handed_on = f.block.map(|b| handed_on_fn(f.sig, b)).unwrap_or_default();
            let mut w = Walker::new(
                index,
                &chain,
                &allows,
                f.owner.as_deref(),
                default_err,
                handed_on,
            );
            if let Some(block) = f.block {
                w.visit_block(block);
            }
            let name = f.sig.ident.to_string();
            let id = path_id(module, &f.path, &name);
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
        for k in &collector.consts {
            self.walk_const(file, module, k, collector, index, &allows);
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

    /// Each closure in a `const` or `static` as a function of its own, named
    /// by the struct field it fills (`SCENARIOS::build`), else by the item.
    fn walk_const(
        &mut self,
        file: &str,
        module: &str,
        k: &RawConst,
        collector: &Collector,
        index: &Index,
        allows: &[Allow],
    ) {
        let item = k.ident.to_string();
        // The module the item is in: its path less any impl or fn around it.
        let mut within = k.path.clone();
        while !within.is_empty() && !collector.imports.modules.contains(&within) {
            within.pop();
        }
        let here = path_id(module, &within, "");
        let chain = collector.imports.chain(&k.path, &item);
        let site = Site {
            here: &here,
            imports: &chain,
            item_ty: k.ty,
        };
        let mut closures = vec![];
        let expect = callable_returns(k.ty);
        const_closures(index, &site, k.expr, "", expect, &mut closures);
        let mut inside = k.path.clone();
        inside.push(item.clone());
        for c in closures {
            let id = match c.label.as_deref() {
                Some(field) => path_id(module, &inside, field),
                None => path_id(module, &k.path, &item),
            };
            let ret = c.returns.unwrap_or(Returns::Other);
            let handed_on = handed_on_closure(c.closure);
            let mut w = Walker::new(
                index,
                &chain,
                allows,
                k.owner.as_deref(),
                default_err(&ret),
                handed_on,
            );
            w.closure_body(c.closure);
            let mut locals = Locals::default();
            locals.visit_expr_closure(c.closure);
            self.functions.push(Function {
                id,
                name: c.label.unwrap_or_else(|| item.clone()),
                file: file.into(),
                line: line(c.closure),
                returns: ret,
                nodes: w.nodes,
                macro_try: w.macro_try,
                locals: locals.0,
            });
        }
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
            self.coverage.item_macro_try += c.item_macro_try.len();
            self.unanalysed_try
                .extend(c.item_macro_try.iter().map(|l| format!("{file}:{l}")));
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
        let mut crate_uses: HashMap<&str, Vec<UseEntry>> = HashMap::new();
        for ((file, module, ..), c) in parsed.iter().zip(&collectors) {
            if !self.test_files.contains(*file) {
                let (entries, uses) = (index_entries(module, c), use_entries(module, c));
                for krate in &krate_of[file.as_str()] {
                    crates
                        .entry(krate)
                        .or_default()
                        .extend(entries.iter().cloned());
                    crate_uses
                        .entry(krate)
                        .or_default()
                        .extend(uses.iter().cloned());
                }
            }
        }
        let module_by_file: HashMap<&str, &str> = parsed
            .iter()
            .map(|(file, module, ..)| (file.as_str(), module.as_str()))
            .collect();
        for ((file, module, src, _), c) in parsed.iter().zip(&collectors) {
            let mine = &krate_of[file.as_str()];
            let index = Index {
                file: index_entries(module, c),
                krates: mine
                    .iter()
                    .filter_map(|k| crates.get(k.as_str()).map(Vec::as_slice))
                    .collect(),
                roots: mine
                    .iter()
                    .map(|k| {
                        module_by_file
                            .get(k.as_str())
                            .map_or_else(|| module_of(Path::new(k)), |m| m.to_string())
                    })
                    .collect(),
                uses: mine
                    .iter()
                    .filter_map(|k| crate_uses.get(k.as_str()).map(Vec::as_slice))
                    .collect(),
            };
            self.walk_file(file, module, src, c, &index);
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

/// `module::path::name`, skipping empty parts.
fn path_id(module: &str, path: &[String], name: &str) -> String {
    std::iter::once(module)
        .chain(path.iter().map(String::as_str))
        .chain([name])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("::")
}

fn index_entry(module: &str, f: &RawFn) -> IndexEntry {
    IndexEntry {
        owner: f.owner.clone(),
        name: f.sig.ident.to_string(),
        returns: returns(&f.sig.output),
        module: path_id(module, &f.path, ""),
        field: false,
        params: vec![],
    }
}

/// A file's fns and closure-holding struct fields, for lookups.
fn index_entries(module: &str, c: &Collector) -> Vec<IndexEntry> {
    let fields = c.fields.iter().map(|f| IndexEntry {
        owner: Some(f.owner.clone()),
        name: f.name.clone(),
        returns: f.returns.clone(),
        module: path_id(module, &f.path, ""),
        field: true,
        params: f.params.clone(),
    });
    c.out
        .iter()
        .map(|f| index_entry(module, f))
        .chain(fields)
        .collect()
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
            // Cargo tags its target dir, whatever CARGO_TARGET_DIR named it.
            if !name.starts_with('.') && name != "target" && !path.join("CACHEDIR.TAG").is_file() {
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
                Node::Unrecorded { .. } => {} // a finding, not a step on the path
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
