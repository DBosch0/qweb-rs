//! Interpreter for compiled templates, lazy content and the template cache.

use derive_where::derive_where;

use std::collections::HashMap;
use std::sync::{
    Arc, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, atomic::Ordering,
};
use std::{fmt, mem};

use crate::compile::{Compiler, is_void};
use crate::error::{Error, ErrorKind, Result};
use crate::escape::escape_into;
use crate::expr::CmpOp;
use crate::host::{CompileOptions, Host, Loaded, TemplateRef};
use crate::ir::{
    Assets, Attr, AttrSrc, Call, CallArg, CallTarget, Ex, Foreach, Format, FormatPart, Loc,
    LoopVars, Otherwise, Out, OutContent, Program, Stmt,
};
use crate::xml::Document;

/// Maximum number of nested template frames.
///
/// Python raises `RecursionError` when `len(stack) > 50`, and its stack has
/// one more frame than our depth (the root frame iterating the call
/// parameters). Allowing 50 here would render templates that fail in
/// Python.
const MAX_DEPTH: usize = 49;

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Hash)]
struct SourceKey {
    source: Arc<str>,
    opts: Arc<CompileOptions>,
    xmlid: Option<String>,
    viewid: String,
}

/// A compiled-template cache.
type Cache<K, H> = RwLock<HashMap<K, Arc<Program<H>>>>;

// A cache stays valid when a thread panicked while holding its lock (it only
// maps keys to finished programs), so a poisoned lock is used as is.
fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Template engine: owns the host and the compiled-template caches.
///
/// Both caches are bounded: when one holds `max_entries` programs it is
/// cleared before the next insertion.
pub struct Engine<H: Host> {
    host: H,
    /// Cache by template reference (used when `trust_names` is set).
    by_name: Cache<(TemplateRef, Arc<CompileOptions>), H>,
    /// Cache by template source.
    by_source: Cache<SourceKey, H>,
    /// Reuse compiled templates by name without calling the loader again.
    /// The cache must then be invalidated when templates change.
    trust_names: bool,
    max_entries: usize,
}

// The caches hold whole programs: show their sizes only.
impl<H: Host + fmt::Debug> fmt::Debug for Engine<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("host", &self.host)
            .field("by_name", &read(&self.by_name).len())
            .field("by_source", &read(&self.by_source).len())
            .field("trust_names", &self.trust_names)
            .field("max_entries", &self.max_entries)
            .finish()
    }
}

impl<H: Host> Engine<H> {
    /// An engine that calls [`Host::load`] for every template lookup.
    pub fn new(host: H) -> Arc<Self> {
        Self::with_options(host, false)
    }

    /// An engine; with `trust_names`, compiled templates are reused by name
    /// without calling [`Host::load`] again, and [`Engine::invalidate`] must
    /// be called when templates change.
    pub fn with_options(host: H, trust_names: bool) -> Arc<Self> {
        Arc::new(Engine {
            host,
            by_name: RwLock::new(HashMap::new()),
            by_source: RwLock::new(HashMap::new()),
            trust_names,
            max_entries: 4096,
        })
    }

    /// The host.
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Drop all compiled templates.
    pub fn invalidate(&self) {
        write(&self.by_name).clear();
        write(&self.by_source).clear();
    }

    /// Render a template by reference with the given values and context.
    ///
    /// # Errors
    ///
    /// Any compilation error (see [`Compiler::compile`]), an error raised
    /// while rendering (a failed name lookup, an expression or host error,
    /// t-call nesting deeper than 50: [`ErrorKind::Recursion`]), or an error
    /// from [`Host::load`] for the template or a template it calls. The error
    /// carries the location of the element and the t-call frames.
    pub fn render(
        self: &Arc<Self>,
        template: &TemplateRef,
        scope: H::Scope,
        ctx: H::Context,
    ) -> Result<String> {
        let program = self.program(template, &ctx)?;
        self.render_program(&program, scope, ctx)
    }

    /// Render a template given its XML source (like rendering an etree).
    ///
    /// # Errors
    ///
    /// An XML syntax error ([`ErrorKind::Xml`]), any compilation error (see [`Compiler::compile`]), an error raised
    /// while rendering (a failed name lookup, an expression or host error,
    /// t-call nesting deeper than 50: [`ErrorKind::Recursion`]), or an error
    /// from [`Host::load`] for the template or a template it calls. The error
    /// carries the location of the element and the t-call frames.
    pub fn render_source(
        self: &Arc<Self>,
        source: &str,
        scope: H::Scope,
        ctx: H::Context,
    ) -> Result<String> {
        let program = self.compile_cached(Arc::from(source), &ctx, None, None, "etree._Element")?;
        self.render_program(&program, scope, ctx)
    }

    /// Render a compiled program.
    ///
    /// # Errors
    ///
    /// An error raised while rendering, as for [`Engine::render`].
    pub fn render_program(
        self: &Arc<Self>,
        program: &Arc<Program<H>>,
        scope: H::Scope,
        ctx: H::Context,
    ) -> Result<String> {
        let hint = program.size_hint.load(Ordering::Relaxed);
        let mut out = String::with_capacity(hint + hint / 8);
        let mut r = Renderer {
            engine: self,
            h: &self.host,
            depth: 1,
            memo: Vec::new(),
            attr_pool: Default::default(),
            scratch: String::new(),
            attr_tmp: Vec::new(),
        };
        r.run_program(program, scope, ctx, &mut out)?;
        program.size_hint.store(out.len(), Ordering::Relaxed);
        Ok(out)
    }

    /// Compile a template source in the given context (no caching): hosts
    /// with their own template cache return the result from [`Host::load`].
    ///
    /// # Errors
    ///
    /// An XML syntax error ([`ErrorKind::Xml`]) or a compilation error (see
    /// [`Compiler::compile`]).
    pub fn compile_program(
        &self,
        source: &str,
        ctx: &H::Context,
        xmlid: Option<String>,
        viewid: Option<H::Value>,
        name: &str,
    ) -> Result<Program<H>> {
        self.compile_source(Arc::from(source), ctx, xmlid, viewid, name)
    }

    fn compile_source(
        &self,
        source: Arc<str>,
        ctx: &H::Context,
        xmlid: Option<String>,
        viewid: Option<H::Value>,
        name: &str,
    ) -> Result<Program<H>> {
        let doc =
            Document::parse(&source).map_err(|e| Error::new(ErrorKind::Xml(e.to_string())))?;
        Compiler::compile(
            &self.host,
            ctx,
            doc,
            Some(source),
            Arc::from(name),
            xmlid,
            viewid,
        )
    }

    fn compile_cached(
        &self,
        source: Arc<str>,
        ctx: &H::Context,
        xmlid: Option<String>,
        viewid: Option<H::Value>,
        name: &str,
    ) -> Result<Arc<Program<H>>> {
        let opts = self.host.compile_options(ctx);
        let viewid_repr = match &viewid {
            Some(v) => {
                let mut s = String::from("v:");
                self.host.write_str(v, &mut s)?;
                s
            }
            None => String::new(),
        };
        let key = SourceKey {
            source,
            opts,
            xmlid,
            viewid: viewid_repr,
        };
        if let Some(p) = read(&self.by_source).get(&key) {
            return Ok(p.clone());
        }
        let program = Arc::new(self.compile_source(
            key.source.clone(),
            ctx,
            key.xmlid.clone(),
            viewid,
            name,
        )?);
        let mut cache = write(&self.by_source);
        if cache.len() >= self.max_entries {
            cache.clear();
        }
        cache.insert(key, program.clone());
        Ok(program)
    }

    /// Get (load and compile if needed) the program for a template reference.
    ///
    /// # Errors
    ///
    /// An error from [`Host::load`], an XML syntax error
    /// ([`ErrorKind::Xml`]) or a compilation error (see [`Compiler::compile`]).
    pub fn program(&self, template: &TemplateRef, ctx: &H::Context) -> Result<Arc<Program<H>>> {
        let opts = self.host.compile_options(ctx);
        if self.trust_names
            && let Some(p) = read(&self.by_name).get(&(template.clone(), opts.clone()))
        {
            return Ok(p.clone());
        }
        let program = match self.host.load(template, ctx)? {
            Loaded::Program(p) => p,
            Loaded::Source {
                source,
                viewid,
                xmlid,
            } => self.compile_cached(source, ctx, xmlid, viewid, &template.to_string())?,
        };
        if self.trust_names {
            let mut cache = write(&self.by_name);
            if cache.len() >= self.max_entries {
                cache.clear();
            }
            cache.insert((template.clone(), opts), program.clone());
        }
        Ok(program)
    }
}

// ---------------------------------------------------------------------------
// Lazy content (QwebContent)
// ---------------------------------------------------------------------------

/// A template fragment rendered on demand (`t-set` body, `t-call` body): the
/// engine side of Python's `QwebContent`.
///
/// It keeps a snapshot of the scope taken when it was created. The first
/// rendering is cached, later outputs reuse it.
pub struct Lazy<H: Host> {
    engine: Arc<Engine<H>>,
    program: Arc<Program<H>>,
    block: usize,
    /// Element defining the content (`t-set` or `t-call`).
    loc: Loc,
    scope: H::Scope,
    ctx: H::Context,
    html: OnceLock<String>,
}

// The scope and context are host types without `Debug`.
impl<H: Host> fmt::Debug for Lazy<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lazy")
            .field("block", &self.block)
            .field("loc", &self.loc)
            .field("html", &self.html.get())
            .finish_non_exhaustive()
    }
}

impl<H: Host> Lazy<H> {
    /// Render (once) and return the markup (`str(QwebContent)`).
    ///
    /// # Errors
    ///
    /// An error raised while rendering the content, as for
    /// [`Engine::render`]. A failed rendering is not cached.
    pub fn render(&self) -> Result<&str> {
        if let Some(h) = self.html.get() {
            return Ok(h);
        }
        let mut out = String::new();
        let mut r = Renderer {
            engine: &self.engine,
            h: &self.engine.host,
            depth: 1,
            memo: Vec::new(),
            attr_pool: Default::default(),
            scratch: String::new(),
            attr_tmp: Vec::new(),
        };
        r.render_lazy_body(self, &mut out)
            .map_err(|e| e.with_frame(err_frame(&self.program, &self.loc)))?;
        Ok(self.html.get_or_init(|| out))
    }

    /// The host.
    pub fn host(&self) -> &H {
        &self.engine.host
    }

    /// Whether the content was rendered already.
    pub fn is_rendered(&self) -> bool {
        self.html.get().is_some()
    }
}

// ---------------------------------------------------------------------------
// Attributes
// ---------------------------------------------------------------------------

/// Value of a dynamic attribute, as given to [`Host::post_processing_att`].
#[derive_where(Debug; H::Value, H::Key, H::Opaque, H::Hook)]
pub enum AttrVal<H: Host> {
    /// Static attribute value.
    Static(Arc<str>),
    /// Rendered format string (`t-attf-*`).
    Text(String),
    /// Expression result (`t-att-*`, `t-att`, field/widget attributes).
    Value(H::Value),
}

impl<H: Host> Clone for AttrVal<H> {
    fn clone(&self) -> Self {
        match self {
            AttrVal::Static(s) => AttrVal::Static(s.clone()),
            AttrVal::Text(s) => AttrVal::Text(s.clone()),
            AttrVal::Value(v) => AttrVal::Value(v.clone()),
        }
    }
}

/// Ordered attribute dict (Python dict semantics: a later value for an
/// existing name replaces it in place).
pub(crate) struct AttrMap<H: Host> {
    items: AttrItems<H>,
}

type AttrItems<H> = Vec<(Arc<str>, AttrVal<H>)>;

impl<H: Host> AttrMap<H> {
    fn set(&mut self, name: Arc<str>, value: AttrVal<H>) {
        if let Some(slot) = self.items.iter_mut().find(|(n, _)| **n == *name) {
            slot.1 = value;
        } else {
            self.items.push((name, value));
        }
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

struct Frame<H: Host> {
    scope: H::Scope,
    attrs: Option<AttrMap<H>>,
    /// The `foreach_attrs` local of the Python function: `attrs` as the
    /// last `t-foreach` started, restored at each iteration. Nested loops
    /// share it, so an inner loop changes what the outer one restores.
    foreach_attrs: Option<AttrMap<H>>,
    /// `values['__qweb_options__']`.
    options: Option<H::Value>,
    ctx: H::Context,
}

/// State of one rendering (one call of [`Engine::render`] or [`Lazy::render`]).
struct Renderer<'e, H: Host> {
    engine: &'e Arc<Engine<H>>,
    h: &'e H,
    depth: usize,
    /// Programs used during this render (like `process['qweb_loaded_functions']`).
    memo: Vec<(TemplateRef, Arc<CompileOptions>, Arc<Program<H>>)>,
    /// Emptied attribute lists, reused by the next elements (a few are
    /// enough: attributes are rendered when their tag opens).
    attr_pool: [Option<AttrItems<H>>; 4],
    /// Scratch buffer for attribute names and values.
    scratch: String,
    /// Evaluated dynamic attributes of the [`Stmt::Attrs`] being run.
    attr_tmp: Vec<AttrTmp<H>>,
}

/// An evaluated dynamic attribute of a [`Stmt::Attrs`].
enum AttrTmp<H: Host> {
    /// Expression result.
    Value(H::Value),
    /// Rendered format string: where it ends in the scratch buffer (it
    /// starts where the previous one ends).
    Text(usize),
}

fn err_frame<H: Host>(prog: &Program<H>, loc: &Loc) -> crate::Frame {
    let (path, element) = prog.location(loc);
    crate::Frame {
        template: prog.name.clone(),
        reference: prog.reference.clone(),
        path,
        element,
    }
}

/// `_render_attributes` (with the base `_post_processing_att`); `buf` is
/// scratch space.
fn write_attributes<H: Host>(
    h: &H,
    ctx: &H::Context,
    tag: &str,
    attrs: &mut AttrMap<H>,
    buf: &mut String,
    out: &mut String,
) -> Result<()> {
    if h.custom_post_processing(ctx) {
        let items = mem::take(&mut attrs.items);
        for (name, value) in h.post_processing_att(ctx, tag, items)? {
            if !(h.is_str(&value) || h.truthy(&value)?) {
                continue;
            }
            buf.clear();
            h.write_str(&name, buf)?;
            out.push(' ');
            escape_into(out, buf);
            out.push_str("=\"");
            buf.clear();
            h.write_str(&value, buf)?;
            escape_into(out, buf);
            out.push('"');
        }
        return Ok(());
    }
    // `_post_processing_att`: neutralise javascript: URLs in href.
    if let Some(idx) = attrs.items.iter().position(|(n, _)| &**n == "href") {
        let malicious = match &attrs.items[idx].1 {
            AttrVal::Static(s) => crate::url::is_malicious_href(s),
            AttrVal::Text(s) => crate::url::is_malicious_href(s),
            AttrVal::Value(v) => {
                buf.clear();
                if h.truthy(v)? {
                    h.write_str(v, buf)?;
                }
                crate::url::is_malicious_href(buf)
            }
        };
        if malicious {
            attrs.items[idx].1 = AttrVal::Static(Arc::from(""));
        }
    }
    for (name, value) in &attrs.items {
        let text: &str = match value {
            AttrVal::Static(s) => s,
            AttrVal::Text(s) => s,
            AttrVal::Value(v) => {
                if !(h.is_str(v) || h.truthy(v)?) {
                    continue;
                }
                out.push(' ');
                escape_into(out, name);
                out.push_str("=\"");
                h.write_str_escaped(v, out)?;
                out.push('"');
                continue;
            }
        };
        out.push(' ');
        escape_into(out, name);
        out.push_str("=\"");
        escape_into(out, text);
        out.push('"');
    }
    Ok(())
}

/// Set the error location (innermost element) if not set yet.
fn locate<H: Host>(err: Error, prog: &Program<H>, loc: &Loc) -> Error {
    if err.has_location() {
        err
    } else {
        err.with_location(err_frame(prog, loc))
    }
}

impl<H: Host> Renderer<'_, H> {
    /// An empty attribute map, reusing a recycled list when there is one.
    fn new_attrs(&mut self) -> AttrMap<H> {
        let items = self.attr_pool.iter_mut().find_map(Option::take);
        AttrMap {
            items: items.unwrap_or_default(),
        }
    }

    /// A copy of an attribute map.
    fn copy_attrs(&mut self, attrs: &AttrMap<H>) -> AttrMap<H> {
        let mut copy = self.new_attrs();
        copy.items.extend(attrs.items.iter().cloned());
        copy
    }

    /// Give back the list of an attribute map that is no longer used.
    fn recycle_attrs(&mut self, attrs: Option<AttrMap<H>>) {
        if let Some(mut a) = attrs
            && a.items.capacity() > 0
            && let Some(slot) = self.attr_pool.iter_mut().find(|s| s.is_none())
        {
            a.items.clear();
            *slot = Some(a.items);
        }
    }

    /// `str(k)` as an attribute name (one allocation, through the scratch buffer).
    fn attr_name(&mut self, k: &H::Value) -> Result<Arc<str>> {
        self.scratch.clear();
        self.h.write_str(k, &mut self.scratch)?;
        Ok(Arc::from(self.scratch.as_str()))
    }

    fn push_depth(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(Error::new(ErrorKind::Recursion(
                "Qweb template infinite recursion".into(),
            )));
        }
        Ok(())
    }

    fn render_template(
        &mut self,
        template: &TemplateRef,
        scope: H::Scope,
        ctx: H::Context,
        out: &mut String,
    ) -> Result<()> {
        self.push_depth()?;
        let res = match self.program(template, &ctx) {
            Ok(program) => self.run_program(&program, scope, ctx, out),
            Err(e) => Err(e),
        };
        self.depth -= 1;
        res
    }

    fn program(&mut self, template: &TemplateRef, ctx: &H::Context) -> Result<Arc<Program<H>>> {
        let opts = self.h.compile_options(ctx);
        if let Some((_, _, p)) = self
            .memo
            .iter()
            .find(|(t, o, _)| t == template && *o == opts)
        {
            return Ok(p.clone());
        }
        let p = self.engine.program(template, ctx)?;
        if self.memo.len() < 64 {
            self.memo.push((template.clone(), opts, p.clone()));
        }
        Ok(p)
    }

    fn run_program(
        &mut self,
        program: &Arc<Program<H>>,
        mut scope: H::Scope,
        ctx: H::Context,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        if !h.scope_contains(&scope, &program.xmlid_key) {
            h.scope_set(&mut scope, &program.xmlid_key, program.xmlid.clone());
            h.scope_set(&mut scope, &program.viewid_key, program.viewid.clone());
        }
        let mut frame = Frame {
            scope,
            attrs: None,
            foreach_attrs: None,
            options: None,
            ctx,
        };
        self.run(program, &program.blocks[program.main], &mut frame, out)
    }

    fn render_lazy_body(&mut self, lazy: &Lazy<H>, out: &mut String) -> Result<()> {
        let mut frame = Frame {
            scope: self.h.scope_copy(&lazy.scope),
            attrs: None,
            foreach_attrs: None,
            options: None,
            ctx: lazy.ctx.clone(),
        };
        self.run(
            &lazy.program,
            &lazy.program.blocks[lazy.block],
            &mut frame,
            out,
        )
    }

    fn render_lazy(&mut self, lazy: &Lazy<H>, out: &mut String) -> Result<()> {
        if let Some(h) = lazy.html.get() {
            out.push_str(h);
            return Ok(());
        }
        self.push_depth()?;
        let start = out.len();
        let res = self.render_lazy_body(lazy, out);
        self.depth -= 1;
        res.map_err(|e| e.with_frame(err_frame(&lazy.program, &lazy.loc)))?;
        let _ = lazy.html.set(out[start..].to_owned());
        Ok(())
    }

    fn make_lazy(
        &self,
        program: &Arc<Program<H>>,
        block: usize,
        loc: &Loc,
        scope: &H::Scope,
        ctx: &H::Context,
    ) -> H::Value {
        let lazy = Lazy {
            engine: self.engine.clone(),
            program: program.clone(),
            block,
            loc: loc.clone(),
            scope: self.h.scope_copy(scope),
            ctx: ctx.clone(),
            html: OnceLock::new(),
        };
        self.h.wrap_lazy(Arc::new(lazy))
    }

    fn run(
        &mut self,
        prog: &Arc<Program<H>>,
        stmts: &[Stmt<H>],
        frame: &mut Frame<H>,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        for stmt in stmts {
            match stmt {
                Stmt::Text(s) => out.push_str(s),
                Stmt::If {
                    cond,
                    then,
                    orelse,
                    loc,
                } => {
                    let v = self
                        .eval(cond, &frame.scope)
                        .map_err(|e| locate(e, prog, loc))?;
                    let t = h.truthy(&v).map_err(|e| locate(e, prog, loc))?;
                    self.run(prog, if t { then } else { orelse }, frame, out)?;
                }
                Stmt::Groups { groups, body } => {
                    if h.has_groups(&frame.ctx, groups)? {
                        self.run(prog, body, frame, out)?;
                    }
                }
                Stmt::Foreach(f) => self.run_foreach(prog, f, frame, out)?,
                Stmt::Set { key, value, loc } => {
                    let v = self
                        .eval(value, &frame.scope)
                        .map_err(|e| locate(e, prog, loc))?;
                    h.scope_set(&mut frame.scope, key, v);
                }
                Stmt::SetFormat {
                    key,
                    fmt,
                    markup,
                    loc,
                } => {
                    let v = if let (Format::Const(v), false) = (fmt, markup) {
                        v.clone()
                    } else {
                        let s = self
                            .format_string(fmt, &frame.scope)
                            .map_err(|e| locate(e, prog, loc))?;
                        if *markup { h.markup(&s) } else { h.str(&s) }
                    };
                    h.scope_set(&mut frame.scope, key, v);
                }
                Stmt::SetUpdate { value, loc } => {
                    let v = self
                        .eval(value, &frame.scope)
                        .map_err(|e| locate(e, prog, loc))?;
                    h.scope_update_value(&mut frame.scope, &v)
                        .map_err(|e| locate(e, prog, loc))?;
                }
                Stmt::SetLazy { key, block, loc } => {
                    let v = self.make_lazy(prog, *block, loc, &frame.scope, &frame.ctx);
                    h.scope_set(&mut frame.scope, key, v);
                }
                Stmt::SetConst { key, value } => h.scope_set(&mut frame.scope, key, value.clone()),
                Stmt::Options { base, items, loc } => {
                    let v = (|| -> Result<H::Value> {
                        let base_v = match base {
                            Some(b) => Some(self.eval(b, &frame.scope)?),
                            None => None,
                        };
                        if items.is_empty() {
                            return Ok(base_v.unwrap_or_else(|| h.empty_options()));
                        }
                        let mut vals = Vec::with_capacity(items.len());
                        for (k, e) in items {
                            vals.push((k.clone(), self.eval(e, &frame.scope)?));
                        }
                        h.merge_dict(base_v.as_ref(), vals)
                    })()
                    .map_err(|e| locate(e, prog, loc))?;
                    frame.options = Some(v);
                }
                Stmt::AttrsInit => {
                    let attrs = self.new_attrs();
                    let old = frame.attrs.replace(attrs);
                    self.recycle_attrs(old);
                }
                Stmt::AttrSet { name, value, loc } => {
                    let v = match value {
                        AttrSrc::Static(s) => AttrVal::Static(s.clone()),
                        AttrSrc::Expr(e) => AttrVal::Value(
                            self.eval(e, &frame.scope)
                                .map_err(|e| locate(e, prog, loc))?,
                        ),
                        AttrSrc::Format(Format::Const(v)) => AttrVal::Value(v.clone()),
                        AttrSrc::Format(f) => AttrVal::Text(
                            self.format_string(f, &frame.scope)
                                .map_err(|e| locate(e, prog, loc))?,
                        ),
                    };
                    frame
                        .attrs
                        .get_or_insert_with(|| self.new_attrs())
                        .set(name.clone(), v);
                }
                Stmt::AttrUpdate { value, loc } => {
                    let v = self
                        .eval(value, &frame.scope)
                        .map_err(|e| locate(e, prog, loc))?;
                    if let Some(pairs) = h.pairs(&v).map_err(|e| locate(e, prog, loc))? {
                        if frame.attrs.is_none() {
                            frame.attrs = Some(self.new_attrs());
                        }
                        for (k, val) in pairs {
                            let name = self.attr_name(&k)?;
                            if let Some(attrs) = &mut frame.attrs {
                                attrs.set(name, AttrVal::Value(val));
                            }
                        }
                    }
                }
                Stmt::AttrsFlush { tag } => {
                    if let Some(mut attrs) = frame.attrs.take() {
                        if attrs.items.is_empty() {
                            frame.attrs = Some(attrs);
                        } else {
                            self.render_attributes(&frame.ctx, tag, &mut attrs, out)?;
                            self.recycle_attrs(Some(attrs));
                        }
                    }
                }
                Stmt::Attrs { attrs, tag, loc } => self
                    .run_attrs(attrs, tag, frame, out)
                    .map_err(|e| locate(e, prog, loc))?,
                Stmt::Out(o) => self
                    .run_out(prog, o, frame, out)
                    .map_err(|e| locate(e, prog, &o.loc))?,
                Stmt::Slot { loc } => {
                    let v = h.scope_get(&frame.scope, &h.slot_key());
                    if let Some(v) = v {
                        if let Some(lazy) = h.as_lazy(&v) {
                            self.render_lazy(&lazy, out)
                                .map_err(|e| e.with_frame(err_frame(prog, loc)))?;
                        } else {
                            h.write_text(&v, out)?;
                        }
                    }
                }
                Stmt::Call(c) => self.run_call(prog, c, frame, out)?,
                Stmt::Assets(a) => self.run_assets(a, frame, out)?,
                Stmt::Hook { hook, loc } => h
                    .run_hook(&frame.ctx, hook, &frame.scope, out)
                    .map_err(|e| locate(e, prog, loc))?,
            }
        }
        Ok(())
    }

    fn run_foreach(
        &mut self,
        prog: &Arc<Program<H>>,
        f: &Foreach<H>,
        frame: &mut Frame<H>,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        let loc_err = |e: Error| locate(e, prog, &f.loc);
        let src = match &f.source {
            Err(n) => h.iterate(h.int(*n)).map_err(loc_err)?,
            Ok(e) => {
                let v = self.eval(e, &frame.scope).map_err(loc_err)?;
                if !h.truthy(&v).map_err(loc_err)? {
                    // A falsy value iterates `[]`: only the size is set.
                    h.scope_set(&mut frame.scope, &f.size, h.int(0));
                    return Ok(());
                }
                h.iterate(v).map_err(loc_err)?
            }
        };
        if let Some(n) = src.size {
            h.scope_set(&mut frame.scope, &f.size, h.int(n));
        }
        // `foreach_attrs = attrs`
        let saved = frame.attrs.as_ref().map(|a| self.copy_attrs(a));
        let old = mem::replace(&mut frame.foreach_attrs, saved);
        self.recycle_attrs(old);
        // Derived variables the body cannot observe are assigned at the
        // first iteration (which creates the keys, in order), then only once
        // the loop ends. `stale` is the last iteration they were skipped
        // for, with its `x_value`.
        let each = f.each;
        let mut stale: Option<(usize, H::Value)> = None;
        let mut res = Ok(());
        for (index, item) in src.iter.enumerate() {
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    res = Err(loc_err(e));
                    break;
                }
            };
            // `attrs = foreach_attrs`
            let attrs = frame.foreach_attrs.as_ref().map(|a| self.copy_attrs(a));
            let old = mem::replace(&mut frame.attrs, attrs);
            self.recycle_attrs(old);
            let scope = &mut frame.scope;
            let all = index == 0;
            let sets = |var: LoopVars| all || each.contains(var);
            if sets(LoopVars::INDEX) {
                h.scope_set(scope, &f.index, h.int(index as i64));
            }
            let value = if src.mapping {
                match h.unpack_pair(item) {
                    Ok((k, v)) => {
                        h.scope_set(scope, &f.var, k);
                        v
                    }
                    Err(e) => {
                        // Python has assigned `x_index` only.
                        if let Some((i, v)) = stale.take() {
                            Self::set_loop_vars(h, f, scope, i, v, src.size);
                        }
                        h.scope_set(scope, &f.index, h.int(index as i64));
                        res = Err(loc_err(e));
                        break;
                    }
                }
            } else {
                h.scope_set(scope, &f.var, item.clone());
                item
            };
            if all || each == LoopVars::ALL {
                Self::set_loop_vars(h, f, scope, index, value, src.size);
            } else {
                if each != LoopVars::NONE {
                    if each.contains(LoopVars::VALUE) {
                        h.scope_set(scope, &f.value, value.clone());
                    }
                    if each.contains(LoopVars::FIRST) {
                        h.scope_set(scope, &f.first, h.bool(false));
                    }
                    if each.contains(LoopVars::LAST)
                        && let Some(n) = src.size
                    {
                        h.scope_set(scope, &f.last, h.bool(index as i64 + 1 == n));
                    }
                    let odd = index % 2 == 1;
                    if each.contains(LoopVars::ODD) {
                        h.scope_set(scope, &f.odd, h.int(i64::from(odd)));
                    }
                    if each.contains(LoopVars::EVEN) {
                        h.scope_set(scope, &f.even, h.bool(!odd));
                    }
                    if each.contains(LoopVars::PARITY) {
                        let parity = if odd { &f.parity_odd } else { &f.parity_even };
                        h.scope_set(scope, &f.parity, parity.clone());
                    }
                }
                stale = Some((index, value));
            }
            res = self.run(prog, &f.body, frame, out);
            if res.is_err() {
                break;
            }
            // `attrs = None`
            let old = frame.attrs.take();
            self.recycle_attrs(old);
        }
        if let Some((index, value)) = stale {
            let scope = &mut frame.scope;
            h.scope_set(scope, &f.index, h.int(index as i64));
            Self::set_loop_vars(h, f, scope, index, value, src.size);
        }
        res
    }

    /// Assign the derived variables of iteration `index` that follow `x`
    /// (`x_value` to `x_parity`), in the order of the Python engine.
    fn set_loop_vars(
        h: &H,
        f: &Foreach<H>,
        scope: &mut H::Scope,
        index: usize,
        value: H::Value,
        size: Option<i64>,
    ) {
        h.scope_set(scope, &f.value, value);
        h.scope_set(scope, &f.first, h.bool(index == 0));
        if let Some(n) = size {
            h.scope_set(scope, &f.last, h.bool(index as i64 + 1 == n));
        }
        let odd = index % 2 == 1;
        h.scope_set(scope, &f.odd, h.int(i64::from(odd)));
        h.scope_set(scope, &f.even, h.bool(!odd));
        let parity = if odd { &f.parity_odd } else { &f.parity_even };
        h.scope_set(scope, &f.parity, parity.clone());
    }

    fn run_out(
        &mut self,
        prog: &Arc<Program<H>>,
        o: &Out<H>,
        frame: &mut Frame<H>,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        let mut content = match &o.content {
            OutContent::Expr(e) => self.eval(e, &frame.scope)?,
            OutContent::Slot => h
                .scope_get(&frame.scope, &h.slot_key())
                .unwrap_or_else(|| h.str("")),
            OutContent::Field { record, .. } => self.eval(record, &frame.scope)?,
        };
        let mut force_display = false;
        if let Some(conv) = &o.convert {
            let options = frame.options.take().unwrap_or_else(|| h.empty_options());
            let r = match &o.content {
                OutContent::Field { field, .. } => h.get_field(
                    &frame.ctx,
                    &content,
                    field,
                    &conv.expression,
                    &conv.tag,
                    options,
                    &frame.scope,
                )?,
                _ => h.get_widget(
                    &frame.ctx,
                    &content,
                    &conv.expression,
                    &conv.tag,
                    options,
                    &frame.scope,
                )?,
            };
            let mut new_attrs = self.new_attrs();
            for (k, v) in r.attrs {
                let name = self.attr_name(&k)?;
                new_attrs.set(name, AttrVal::Value(v));
            }
            if let (Some(existing), true) = (&mut frame.attrs, conv.merge) {
                for (k, v) in new_attrs.items.drain(..) {
                    existing.set(k, v);
                }
                self.recycle_attrs(Some(new_attrs));
            } else {
                let old = frame.attrs.replace(new_attrs);
                self.recycle_attrs(old);
            }
            content = r.content;
            force_display = r.force_display;
            if !conv.field || !h.is_none_or_false(&content) {
                content = h.text_value(&content)?;
            }
        }
        if h.is_none_or_false(&content) {
            match &o.otherwise {
                Otherwise::Default(body) => {
                    self.run(prog, &o.tag_open, frame, out)?;
                    self.run(prog, body, frame, out)?;
                    self.run(prog, &o.tag_close, frame, out)?;
                }
                Otherwise::ForceDisplay { has_tag } => {
                    if *has_tag && force_display {
                        self.run(prog, &o.tag_open, frame, out)?;
                        self.run(prog, &o.tag_close, frame, out)?;
                    } else {
                        let old = frame.attrs.take();
                        self.recycle_attrs(old);
                    }
                }
                Otherwise::Nothing => {}
            }
        } else {
            self.run(prog, &o.tag_open, frame, out)?;
            if let Some(lazy) = h.as_lazy(&content) {
                self.render_lazy(&lazy, out)
                    .map_err(|e| e.with_frame(err_frame(prog, &o.loc)))?;
            } else {
                h.write_escaped(&content, out)?;
            }
            self.run(prog, &o.tag_close, frame, out)?;
        }
        Ok(())
    }

    fn run_call(
        &mut self,
        prog: &Arc<Program<H>>,
        c: &Call<H>,
        frame: &mut Frame<H>,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        let loc_err = |e: Error| locate(e, prog, &c.loc);
        let options = frame.options.take();
        let ctx = if options.is_some() || c.nsmap.is_some() {
            h.call_context(&frame.ctx, options.as_ref(), c.nsmap.as_ref())
                .map_err(loc_err)?
        } else {
            frame.ctx.clone()
        };
        let slot = match c.body {
            Some(b) => self.make_lazy(prog, b, &c.loc, &frame.scope, &frame.ctx),
            None => h.str(""),
        };
        // `t_call_values = {0: body, name: value, ...}`, evaluated in the
        // caller's scope, then `values = values.copy()` updated with them.
        // Without `t-args` (which updates the dict from a value), the call
        // values are kept in a list and assigned to the copy directly: the
        // resulting dict is the same, keys in the same order.
        let has_args = c.args.iter().any(|a| matches!(a, CallArg::Args(_)));
        let mut values = has_args.then(|| h.scope_new());
        let mut list = Vec::with_capacity(if has_args { 0 } else { c.args.len() + 1 });
        let slot_key = h.slot_key();
        match &mut values {
            Some(values) => h.scope_set(values, &slot_key, slot),
            None => list.push((&slot_key, slot)),
        }
        for arg in &c.args {
            let (k, v) = match arg {
                CallArg::Expr(k, e) => (k, self.eval(e, &frame.scope).map_err(loc_err)?),
                CallArg::Format(k, f) => (k, self.format_value(f, &frame.scope).map_err(loc_err)?),
                CallArg::MarkupFormat(k, f) => {
                    let s = self.format_string(f, &frame.scope).map_err(loc_err)?;
                    (k, h.markup(&s))
                }
                CallArg::Args(e) => {
                    let v = self.eval(e, &frame.scope).map_err(loc_err)?;
                    if let Some(values) = &mut values {
                        h.scope_update_pairs(values, &v).map_err(loc_err)?;
                    }
                    continue;
                }
            };
            match &mut values {
                Some(values) => h.scope_set(values, k, v),
                None => list.push((k, v)),
            }
        }
        let target = match &c.target {
            CallTarget::Static(t) => t.clone(),
            CallTarget::Dynamic(fmt) => {
                let s = self.format_string(fmt, &frame.scope).map_err(loc_err)?;
                TemplateRef::parse(&s)
            }
        };
        let mut scope = h.scope_copy(&frame.scope);
        if let Some(values) = &values {
            h.scope_update(&mut scope, values);
        }
        for (k, v) in list {
            h.scope_set(&mut scope, k, v);
        }
        self.render_template(&target, scope, ctx, out)
            .map_err(|e| e.with_frame(err_frame(prog, &c.loc)))
    }

    fn run_assets(&mut self, a: &Assets, frame: &mut Frame<H>, out: &mut String) -> Result<()> {
        let h = self.h;
        let debug = h.scope_get(&frame.scope, &h.key("debug"));
        let nodes = h.get_asset_nodes(
            &frame.ctx,
            &a.bundle,
            a.css,
            a.js,
            a.binary,
            a.autoprefix,
            debug.as_ref(),
            a.defer_load,
            a.lazy_load,
            a.media.as_deref(),
        )?;
        for (index, (tag, attrs)) in nodes.into_iter().enumerate() {
            if index > 0 {
                out.push_str("\n        ");
            }
            out.push('<');
            out.push_str(&tag);
            let mut map = self.new_attrs();
            for (k, v) in attrs {
                let name = self.attr_name(&k)?;
                map.set(name, AttrVal::Value(v));
            }
            self.render_attributes(&frame.ctx, &tag, &mut map, out)?;
            self.recycle_attrs(Some(map));
            if is_void(&tag) {
                out.push_str("/>");
            } else {
                out.push_str("></");
                out.push_str(&tag);
                out.push('>');
            }
        }
        Ok(())
    }

    /// The value of an attribute, as stored in the attribute dict.
    fn attr_value(&self, src: &AttrSrc<H>, scope: &H::Scope) -> Result<AttrVal<H>> {
        Ok(match src {
            AttrSrc::Static(s) => AttrVal::Static(s.clone()),
            AttrSrc::Expr(e) => AttrVal::Value(self.eval(e, scope)?),
            AttrSrc::Format(Format::Const(v)) => AttrVal::Value(v.clone()),
            AttrSrc::Format(f) => AttrVal::Text(self.format_string(f, scope)?),
        })
    }

    /// [`Stmt::Attrs`]: `attrs = {...}`, `_render_attributes(tag, attrs)`,
    /// `attrs = None`, without the dict.
    fn run_attrs(
        &mut self,
        attrs: &[Attr<H>],
        tag: &str,
        frame: &mut Frame<H>,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        let old = frame.attrs.take();
        self.recycle_attrs(old);
        if h.custom_post_processing(&frame.ctx) {
            // The host wants the dict.
            let mut map = self.new_attrs();
            let mut res = Ok(());
            for a in attrs {
                match self.attr_value(&a.value, &frame.scope) {
                    Ok(v) => map.items.push((a.name.clone(), v)),
                    Err(e) => {
                        res = Err(e);
                        break;
                    }
                }
            }
            if res.is_ok() {
                res = self.render_attributes(&frame.ctx, tag, &mut map, out);
            }
            self.recycle_attrs(Some(map));
            return res;
        }
        let mut buf = mem::take(&mut self.scratch);
        let mut tmp = mem::take(&mut self.attr_tmp);
        buf.clear();
        let res = self.write_attrs(attrs, &frame.scope, &mut buf, &mut tmp, out);
        tmp.clear();
        self.scratch = buf;
        self.attr_tmp = tmp;
        res
    }

    /// Evaluate every dynamic attribute (like the dict assignments), then
    /// write them all (like `_render_attributes`). `buf` holds the rendered
    /// format strings, `tmp` the evaluated attributes.
    fn write_attrs(
        &self,
        attrs: &[Attr<H>],
        scope: &H::Scope,
        buf: &mut String,
        tmp: &mut Vec<AttrTmp<H>>,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        let is_const = |a: &Attr<H>| {
            matches!(
                a.value,
                AttrSrc::Static(_) | AttrSrc::Format(Format::Const(_))
            )
        };
        for a in attrs {
            match &a.value {
                AttrSrc::Expr(e) => tmp.push(AttrTmp::Value(self.eval(e, scope)?)),
                AttrSrc::Format(Format::Parts(parts)) => {
                    self.format_into(parts, scope, buf)?;
                    tmp.push(AttrTmp::Text(buf.len()));
                }
                AttrSrc::Static(_) | AttrSrc::Format(Format::Const(_)) => {}
            }
        }
        let texts = buf.len();
        // `_post_processing_att`: neutralise javascript: URLs in href (the
        // check of a constant href was made when compiling).
        let mut malicious = None;
        if let Some(i) = attrs.iter().position(|a| &*a.name == "href")
            && !is_const(&attrs[i])
        {
            let t = attrs[..i].iter().filter(|a| !is_const(a)).count();
            let bad = match &tmp[t] {
                AttrTmp::Value(v) => {
                    if h.truthy(v)? {
                        h.write_str(v, buf)?;
                    }
                    let bad = crate::url::is_malicious_href(&buf[texts..]);
                    buf.truncate(texts);
                    bad
                }
                AttrTmp::Text(end) => {
                    crate::url::is_malicious_href(&buf[Self::text_start(tmp, t)..*end])
                }
            };
            if bad {
                malicious = Some(i);
            }
        }
        let mut dynamic = tmp.iter();
        let mut start = 0;
        for (i, a) in attrs.iter().enumerate() {
            if is_const(a) {
                out.push_str(&a.rendered);
                continue;
            }
            let Some(value) = dynamic.next() else {
                break;
            };
            match value {
                _ if malicious == Some(i) => {
                    if let AttrTmp::Text(end) = value {
                        start = *end;
                    }
                    out.push_str(&a.rendered);
                }
                AttrTmp::Value(v) => {
                    if !(h.is_str(v) || h.truthy(v)?) {
                        continue;
                    }
                    out.push_str(&a.rendered);
                    h.write_str_escaped(v, out)?;
                }
                AttrTmp::Text(end) => {
                    out.push_str(&a.rendered);
                    escape_into(out, &buf[start..*end]);
                    start = *end;
                }
            }
            out.push('"');
        }
        Ok(())
    }

    /// Where the text of `tmp[t]` starts in the scratch buffer.
    fn text_start(tmp: &[AttrTmp<H>], t: usize) -> usize {
        tmp[..t]
            .iter()
            .rev()
            .find_map(|v| match v {
                AttrTmp::Text(end) => Some(*end),
                AttrTmp::Value(_) => None,
            })
            .unwrap_or(0)
    }

    /// `_render_attributes` (with the base `_post_processing_att`).
    fn render_attributes(
        &mut self,
        ctx: &H::Context,
        tag: &str,
        attrs: &mut AttrMap<H>,
        out: &mut String,
    ) -> Result<()> {
        let mut buf = mem::take(&mut self.scratch);
        let res = write_attributes(self.h, ctx, tag, attrs, &mut buf, out);
        self.scratch = buf;
        res
    }

    // ----- expressions -------------------------------------------------------

    fn format_string(&self, f: &Format<H>, scope: &H::Scope) -> Result<String> {
        match f {
            Format::Const(v) => {
                let mut s = String::new();
                self.h.write_text(v, &mut s)?;
                Ok(s)
            }
            Format::Parts(parts) => {
                // Room for the literals plus a short value per expression,
                // so that most strings need a single allocation.
                let cap = parts
                    .iter()
                    .map(|p| match p {
                        FormatPart::Lit(l) => l.len(),
                        FormatPart::Expr(_) => 16,
                    })
                    .sum();
                let mut s = String::with_capacity(cap);
                self.format_into(parts, scope, &mut s)?;
                Ok(s)
            }
        }
    }

    /// Append a rendered format string to `out`.
    fn format_into(
        &self,
        parts: &[FormatPart<H>],
        scope: &H::Scope,
        out: &mut String,
    ) -> Result<()> {
        for p in parts {
            match p {
                FormatPart::Lit(l) => out.push_str(l),
                FormatPart::Expr(e) => {
                    let v = self.eval(e, scope)?;
                    self.h.write_text(&v, out)?;
                }
            }
        }
        Ok(())
    }

    fn format_value(&self, f: &Format<H>, scope: &H::Scope) -> Result<H::Value> {
        match f {
            Format::Const(v) => Ok(v.clone()),
            Format::Parts(_) => Ok(self.h.str(&self.format_string(f, scope)?)),
        }
    }

    fn eval(&self, e: &Ex<H>, scope: &H::Scope) -> Result<H::Value> {
        let h = self.h;
        Ok(match e {
            Ex::Const(v) => v.clone(),
            Ex::Var { key, name, strict } => match h.scope_get(scope, key) {
                Some(v) => v,
                None if *strict => return Err(h.missing_name(name)),
                None => h.none(),
            },
            Ex::Unknown(name) => {
                return Err(Error::other(format!("name '{name}' is not defined")));
            }
            Ex::Attr(o, name) => {
                let o = self.eval(o, scope)?;
                h.get_attr(&o, name)?
            }
            Ex::Item(o, k) => {
                let o = self.eval(o, scope)?;
                let k = self.eval(k, scope)?;
                h.get_item(&o, &k)?
            }
            Ex::Slice(l, u, s) => {
                let l = match l {
                    Some(e) => Some(self.eval(e, scope)?),
                    None => None,
                };
                let u = match u {
                    Some(e) => Some(self.eval(e, scope)?),
                    None => None,
                };
                let s = match s {
                    Some(e) => Some(self.eval(e, scope)?),
                    None => None,
                };
                h.slice(l, u, s)
            }
            Ex::Call { func, args, kwargs } => {
                let f = self.eval(func, scope)?;
                let mut a = Vec::with_capacity(args.len());
                for arg in args {
                    a.push(self.eval(arg, scope)?);
                }
                let mut kw = Vec::with_capacity(kwargs.len());
                for (k, v) in kwargs {
                    kw.push((k.clone(), self.eval(v, scope)?));
                }
                h.call(&f, a, kw)?
            }
            Ex::Unary(op, v) => {
                let v = self.eval(v, scope)?;
                h.unary(*op, &v)?
            }
            Ex::Not(v) => {
                let v = self.eval(v, scope)?;
                h.bool(!h.truthy(&v)?)
            }
            Ex::Bin(op, l, r) => {
                let l = self.eval(l, scope)?;
                let r = self.eval(r, scope)?;
                h.binary(*op, &l, &r)?
            }
            Ex::Cmp(first, rest) => {
                let mut left = self.eval(first, scope)?;
                let n = rest.len();
                for (i, (op, re)) in rest.iter().enumerate() {
                    let right = self.eval(re, scope)?;
                    let res = match op {
                        CmpOp::In => h.bool(h.contains(&right, &left)?),
                        CmpOp::NotIn => h.bool(!h.contains(&right, &left)?),
                        CmpOp::Is => h.bool(h.is(&left, &right)),
                        CmpOp::IsNot => h.bool(!h.is(&left, &right)),
                        _ => h.compare(*op, &left, &right)?,
                    };
                    if i + 1 == n || !h.truthy(&res)? {
                        return Ok(res);
                    }
                    left = right;
                }
                unreachable!()
            }
            Ex::And(items) => {
                let mut v = h.none();
                for (i, item) in items.iter().enumerate() {
                    v = self.eval(item, scope)?;
                    if i + 1 < items.len() && !h.truthy(&v)? {
                        return Ok(v);
                    }
                }
                v
            }
            Ex::Or(items) => {
                let mut v = h.none();
                for (i, item) in items.iter().enumerate() {
                    v = self.eval(item, scope)?;
                    if i + 1 < items.len() && h.truthy(&v)? {
                        return Ok(v);
                    }
                }
                v
            }
            Ex::IfElse(c, t, o) => {
                let c = self.eval(c, scope)?;
                if h.truthy(&c)? {
                    self.eval(t, scope)?
                } else {
                    self.eval(o, scope)?
                }
            }
            Ex::Tuple(items) => h.tuple(self.eval_all(items, scope)?),
            Ex::List(items) => h.list(self.eval_all(items, scope)?),
            Ex::Set(items) => h.set(self.eval_all(items, scope)?)?,
            Ex::Dict(items) => {
                let mut pairs = Vec::with_capacity(items.len());
                for (k, v) in items {
                    let k = self.eval(k, scope)?;
                    let v = self.eval(v, scope)?;
                    pairs.push((k, v));
                }
                h.dict(pairs)?
            }
            Ex::Opaque(code) => h.eval_opaque(code, scope)?,
        })
    }

    fn eval_all(&self, items: &[Ex<H>], scope: &H::Scope) -> Result<Vec<H::Value>> {
        let mut v = Vec::with_capacity(items.len());
        for i in items {
            v.push(self.eval(i, scope)?);
        }
        Ok(v)
    }
}
