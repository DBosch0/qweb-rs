//! Interpreter for compiled templates, lazy content and the template cache.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::compile::{Compiler, is_void};
use crate::error::{Error, ErrorKind, Result};
use crate::escape::escape_into;
use crate::expr::CmpOp;
use crate::host::{CompileOptions, Host, Loaded, TemplateRef};
use crate::ir::*;
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
    opts: CompileOptions,
    xmlid: Option<String>,
    viewid: String,
}

/// A compiled-template cache.
type Cache<K, H> = RwLock<HashMap<K, Arc<Program<H>>>>;

/// Template engine: owns the host and the compiled-template caches.
///
/// Both caches are bounded: when one holds `max_entries` programs it is
/// cleared before the next insertion.
pub struct Engine<H: Host> {
    host: H,
    /// Cache by template reference (used when `trust_names` is set).
    by_name: Cache<(TemplateRef, CompileOptions), H>,
    /// Cache by template source.
    by_source: Cache<SourceKey, H>,
    /// Reuse compiled templates by name without calling the loader again.
    /// The cache must then be invalidated when templates change.
    trust_names: bool,
    max_entries: usize,
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
        self.by_name.write().unwrap().clear();
        self.by_source.write().unwrap().clear();
    }

    /// Render a template by reference with the given values and context.
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
    pub fn render_program(
        self: &Arc<Self>,
        program: &Arc<Program<H>>,
        scope: H::Scope,
        ctx: H::Context,
    ) -> Result<String> {
        let hint = program.size_hint.load(std::sync::atomic::Ordering::Relaxed);
        let mut out = String::with_capacity(hint + hint / 8);
        let mut r = Renderer {
            engine: self,
            h: &self.host,
            depth: 1,
            memo: Vec::new(),
        };
        r.run_program(program, scope, ctx, &mut out)?;
        program
            .size_hint
            .store(out.len(), std::sync::atomic::Ordering::Relaxed);
        Ok(out)
    }

    /// Compile a template source in the given context (no caching): hosts
    /// with their own template cache return the result from [`Host::load`].
    pub fn compile_program(
        &self,
        source: &str,
        ctx: &H::Context,
        xmlid: Option<String>,
        viewid: Option<H::Value>,
        name: &str,
    ) -> Result<Program<H>> {
        let doc = Document::parse(source).map_err(|e| Error::new(ErrorKind::Xml(e.to_string())))?;
        Compiler::compile(&self.host, ctx, doc, Arc::from(name), xmlid, viewid)
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
        if let Some(p) = self.by_source.read().unwrap().get(&key) {
            return Ok(p.clone());
        }
        let program =
            Arc::new(self.compile_program(&key.source, ctx, key.xmlid.clone(), viewid, name)?);
        let mut cache = self.by_source.write().unwrap();
        if cache.len() >= self.max_entries {
            cache.clear();
        }
        cache.insert(key, program.clone());
        Ok(program)
    }

    /// Get (load and compile if needed) the program for a template reference.
    pub fn program(&self, template: &TemplateRef, ctx: &H::Context) -> Result<Arc<Program<H>>> {
        let opts = self.host.compile_options(ctx);
        if self.trust_names
            && let Some(p) = self
                .by_name
                .read()
                .unwrap()
                .get(&(template.clone(), opts.clone()))
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
            let mut cache = self.by_name.write().unwrap();
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

impl<H: Host> Lazy<H> {
    /// Render (once) and return the markup (`str(QwebContent)`).
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
    items: Vec<(Arc<str>, AttrVal<H>)>,
}

impl<H: Host> Clone for AttrMap<H> {
    fn clone(&self) -> Self {
        AttrMap {
            items: self.items.clone(),
        }
    }
}

impl<H: Host> AttrMap<H> {
    fn new() -> Self {
        AttrMap { items: Vec::new() }
    }
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
    memo: Vec<(TemplateRef, CompileOptions, Arc<Program<H>>)>,
}

fn err_frame<H: Host>(prog: &Program<H>, loc: &Loc) -> crate::Frame {
    crate::Frame {
        template: prog.name.clone(),
        reference: prog.reference.clone(),
        path: loc.path.clone(),
        element: loc.element.clone(),
    }
}

/// Set the error location (innermost element) if not set yet.
fn locate<H: Host>(err: Error, prog: &Program<H>, loc: &Loc) -> Error {
    if err.has_location() {
        err
    } else {
        err.with_location(err_frame(prog, loc))
    }
}

impl<'e, H: Host> Renderer<'e, H> {
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
            options: None,
            ctx,
        };
        self.run(program, &program.blocks[program.main], &mut frame, out)
    }

    fn render_lazy_body(&mut self, lazy: &Lazy<H>, out: &mut String) -> Result<()> {
        let mut frame = Frame {
            scope: self.h.scope_copy(&lazy.scope),
            attrs: None,
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
                    let v = match (fmt, markup) {
                        (Format::Const(v), false) => v.clone(),
                        _ => {
                            let s = self
                                .format_string(fmt, &frame.scope)
                                .map_err(|e| locate(e, prog, loc))?;
                            if *markup { h.markup(&s) } else { h.str(&s) }
                        }
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
                Stmt::AttrsInit => frame.attrs = Some(AttrMap::new()),
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
                        .get_or_insert_with(AttrMap::new)
                        .set(name.clone(), v);
                }
                Stmt::AttrUpdate { value, loc } => {
                    let v = self
                        .eval(value, &frame.scope)
                        .map_err(|e| locate(e, prog, loc))?;
                    if let Some(pairs) = h.pairs(&v).map_err(|e| locate(e, prog, loc))? {
                        let attrs = frame.attrs.get_or_insert_with(AttrMap::new);
                        for (k, val) in pairs {
                            let mut name = String::new();
                            h.write_str(&k, &mut name)?;
                            attrs.set(Arc::from(name), AttrVal::Value(val));
                        }
                    }
                }
                Stmt::AttrsFlush { tag } => {
                    if let Some(mut attrs) = frame.attrs.take() {
                        if attrs.items.is_empty() {
                            frame.attrs = Some(attrs);
                        } else {
                            self.render_attributes(&frame.ctx, tag, &mut attrs, out)?;
                        }
                    }
                }
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
                let v = if h.truthy(&v).map_err(loc_err)? {
                    v
                } else {
                    h.list(Vec::new())
                };
                h.iterate(v).map_err(loc_err)?
            }
        };
        if let Some(n) = src.size {
            h.scope_set(&mut frame.scope, &f.size, h.int(n as i64));
        }
        let saved = frame.attrs.clone();
        for (index, item) in src.iter.enumerate() {
            let item = item.map_err(loc_err)?;
            frame.attrs = saved.clone();
            let scope = &mut frame.scope;
            h.scope_set(scope, &f.index, h.int(index as i64));
            if src.mapping {
                let (k, v) = h.unpack_pair(item).map_err(loc_err)?;
                h.scope_set(scope, &f.var, k);
                h.scope_set(scope, &f.value, v);
            } else {
                h.scope_set(scope, &f.var, item.clone());
                h.scope_set(scope, &f.value, item);
            }
            h.scope_set(scope, &f.first, h.bool(index == 0));
            if let Some(n) = src.size {
                h.scope_set(scope, &f.last, h.bool(index + 1 == n));
            }
            let odd = index % 2 == 1;
            h.scope_set(scope, &f.odd, h.int((index % 2) as i64));
            h.scope_set(scope, &f.even, h.bool(!odd));
            h.scope_set(
                scope,
                &f.parity,
                if odd {
                    f.parity_odd.clone()
                } else {
                    f.parity_even.clone()
                },
            );
            self.run(prog, &f.body, frame, out)?;
            frame.attrs = None;
        }
        Ok(())
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
            let mut new_attrs = AttrMap::new();
            for (k, v) in r.attrs {
                let mut name = String::new();
                h.write_str(&k, &mut name)?;
                new_attrs.set(Arc::from(name), AttrVal::Value(v));
            }
            match (&mut frame.attrs, conv.merge) {
                (Some(existing), true) => {
                    for (k, v) in new_attrs.items {
                        existing.set(k, v);
                    }
                }
                _ => frame.attrs = Some(new_attrs),
            }
            content = r.content;
            force_display = r.force_display;
            if !conv.field || !h.is_none_or_false(&content) {
                content = h.text_value(&content)?;
            }
        }
        if !h.is_none_or_false(&content) {
            self.run(prog, &o.tag_open, frame, out)?;
            if let Some(lazy) = h.as_lazy(&content) {
                self.render_lazy(&lazy, out)
                    .map_err(|e| e.with_frame(err_frame(prog, &o.loc)))?;
            } else {
                h.write_escaped(&content, out)?;
            }
            self.run(prog, &o.tag_close, frame, out)?;
        } else {
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
                        frame.attrs = None;
                    }
                }
                Otherwise::Nothing => {}
            }
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
        let mut values = h.scope_new();
        let slot = match c.body {
            Some(b) => self.make_lazy(prog, b, &c.loc, &frame.scope, &frame.ctx),
            None => h.str(""),
        };
        h.scope_set(&mut values, &h.slot_key(), slot);
        for arg in &c.args {
            match arg {
                CallArg::Expr(k, e) => {
                    let v = self.eval(e, &frame.scope).map_err(loc_err)?;
                    h.scope_set(&mut values, k, v);
                }
                CallArg::Format(k, f) => {
                    let v = self.format_value(f, &frame.scope).map_err(loc_err)?;
                    h.scope_set(&mut values, k, v);
                }
                CallArg::MarkupFormat(k, f) => {
                    let s = self.format_string(f, &frame.scope).map_err(loc_err)?;
                    h.scope_set(&mut values, k, h.markup(&s));
                }
                CallArg::Args(e) => {
                    let v = self.eval(e, &frame.scope).map_err(loc_err)?;
                    h.scope_update_pairs(&mut values, &v).map_err(loc_err)?;
                }
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
        h.scope_update(&mut scope, &values);
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
            let mut map = AttrMap::new();
            for (k, v) in attrs {
                let mut name = String::new();
                h.write_str(&k, &mut name)?;
                map.set(Arc::from(name), AttrVal::Value(v));
            }
            self.render_attributes(&frame.ctx, &tag, &mut map, out)?;
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

    /// `_render_attributes` (with the base `_post_processing_att`).
    fn render_attributes(
        &mut self,
        ctx: &H::Context,
        tag: &str,
        attrs: &mut AttrMap<H>,
        out: &mut String,
    ) -> Result<()> {
        let h = self.h;
        if h.custom_post_processing(ctx) {
            let items = std::mem::take(&mut attrs.items);
            let mut buf = String::new();
            for (name, value) in h.post_processing_att(ctx, tag, items)? {
                if !(h.is_str(&value) || h.truthy(&value)?) {
                    continue;
                }
                buf.clear();
                h.write_str(&name, &mut buf)?;
                out.push(' ');
                escape_into(out, &buf);
                out.push_str("=\"");
                buf.clear();
                h.write_str(&value, &mut buf)?;
                escape_into(out, &buf);
                out.push('"');
            }
            return Ok(());
        }
        // `_post_processing_att`: neutralise javascript: URLs in href.
        if let Some(idx) = attrs.items.iter().position(|(n, _)| &**n == "href") {
            let href = match &attrs.items[idx].1 {
                AttrVal::Static(s) => s.to_string(),
                AttrVal::Text(s) => s.clone(),
                AttrVal::Value(v) => {
                    let mut s = String::new();
                    if h.truthy(v)? {
                        h.write_str(v, &mut s)?;
                    }
                    s
                }
            };
            if crate::url::is_malicious_href(&href) {
                attrs.items[idx].1 = AttrVal::Static(Arc::from(""));
            }
        }
        let mut buf = String::new();
        for (name, value) in &attrs.items {
            let text: &str = match value {
                AttrVal::Static(s) => s,
                AttrVal::Text(s) => s,
                AttrVal::Value(v) => {
                    if !(h.is_str(v) || h.truthy(v)?) {
                        continue;
                    }
                    buf.clear();
                    h.write_str(v, &mut buf)?;
                    &buf
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

    // ----- expressions -------------------------------------------------------

    fn format_string(&self, f: &Format<H>, scope: &H::Scope) -> Result<String> {
        match f {
            Format::Const(v) => {
                let mut s = String::new();
                self.h.write_text(v, &mut s)?;
                Ok(s)
            }
            Format::Parts(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        FormatPart::Lit(l) => s.push_str(l),
                        FormatPart::Expr(e) => {
                            let v = self.eval(e, scope)?;
                            self.h.write_text(&v, &mut s)?;
                        }
                    }
                }
                Ok(s)
            }
        }
    }

    fn format_value(&self, f: &Format<H>, scope: &H::Scope) -> Result<H::Value> {
        match f {
            Format::Const(v) => Ok(v.clone()),
            _ => Ok(self.h.str(&self.format_string(f, scope)?)),
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
