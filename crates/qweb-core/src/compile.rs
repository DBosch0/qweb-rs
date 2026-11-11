//! Template compiler: XML tree → [`Program`].
//!
//! This is a structural port of `IrQweb._compile_*`: same traversal, same
//! directive order, same text-chunk buffer (`_text_concat`) and the same tree
//! mutations, so whitespace handling and output are byte-identical. Instead
//! of Python source, every method returns IR statements.
//!
//! Most hosts never use the compiler directly: [`Engine`](crate::Engine)
//! compiles templates as they are loaded. Custom directives
//! ([`Host::compile_directive`]) see it through [`CompileApi`].

use std::fmt::Write as _;
use std::sync::Arc;

use crate::error::{Error, ErrorKind, Result};
use crate::expr::{self, Expr, ParseError};
use crate::format::{FormatPart as FP, split_format};
use crate::host::{CompileHooks, CompileOptions, Host, TemplateRef};
use crate::ir::*;
use crate::text;
use crate::xml::{self, Document, NodeId, NodeKind, NsMap, XStr};

/// HTML void elements: rendered as `<tag/>`, without a closing tag.
pub const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "keygen", "link", "menuitem",
    "meta", "param", "source", "track", "wbr",
];

/// Attributes ignored by the compiler (`SPECIAL_DIRECTIVES`).
const SPECIAL_DIRECTIVES: &[&str] = &[
    "t-translation",
    "t-ignore",
    "t-title",
    "id",
    "name",
    "data-oe-model",
    "data-oe-id",
    "data-oe-field",
    "data-oe-xpath",
    "data-oe-source-id",
];

/// Whether `tag` is one of [`VOID_ELEMENTS`].
#[inline]
pub fn is_void(tag: &str) -> bool {
    VOID_ELEMENTS.contains(&tag)
}

/// A directive, i.e. a `_compile_directive_<name>` handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    /// `t-elif`
    Elif,
    /// `t-else`
    Else,
    /// `t-debug`
    Debug,
    /// `t-log`
    Log,
    /// `t-groups` / `groups`
    Groups,
    /// `t-as`
    As,
    /// `t-foreach`
    Foreach,
    /// `t-if`
    If,
    /// `t-call-assets`
    CallAssets,
    /// `t-lang`
    Lang,
    /// `t-options` / `t-options-*`
    Options,
    /// `t-call`
    Call,
    /// Attributes: static ones, `t-att`, `t-att-*` and `t-attf-*`.
    Att,
    /// `t-field`
    Field,
    /// `t-out`
    Out,
    /// Start tag (internal `t-tag-open`).
    TagOpen,
    /// `t-set`
    Set,
    /// Element content (internal `t-inner-content`).
    InnerContent,
    /// End tag (internal `t-tag-close`).
    TagClose,
    /// Host-defined directive (index in the compiler's custom names).
    Custom(u16),
}

impl Directive {
    /// Directive name (as in `_directives_eval_order`), for built-in directives.
    pub fn name(self) -> &'static str {
        &self.attr()[2..]
    }

    fn from_name(name: &str) -> Option<Directive> {
        Directive::from_handler_attr(&format!("t-{name}"))
    }

    fn attr(self) -> &'static str {
        match self {
            Directive::Elif => "t-elif",
            Directive::Else => "t-else",
            Directive::Debug => "t-debug",
            Directive::Log => "t-log",
            Directive::Groups => "t-groups",
            Directive::As => "t-as",
            Directive::Foreach => "t-foreach",
            Directive::If => "t-if",
            Directive::CallAssets => "t-call-assets",
            Directive::Lang => "t-lang",
            Directive::Options => "t-options",
            Directive::Call => "t-call",
            Directive::Att => "t-att",
            Directive::Field => "t-field",
            Directive::Out => "t-out",
            Directive::TagOpen => "t-tag-open",
            Directive::Set => "t-set",
            Directive::InnerContent => "t-inner-content",
            Directive::TagClose => "t-tag-close",
            Directive::Custom(_) => "t-",
        }
    }

    /// Directive with a `_compile_directive_<name>` handler for `t-<name>`.
    fn from_handler_attr(attr: &str) -> Option<Directive> {
        Some(match attr {
            "t-elif" => Directive::Elif,
            "t-else" => Directive::Else,
            "t-debug" => Directive::Debug,
            "t-log" => Directive::Log,
            "t-groups" => Directive::Groups,
            "t-as" => Directive::As,
            "t-foreach" => Directive::Foreach,
            "t-if" => Directive::If,
            "t-call-assets" => Directive::CallAssets,
            "t-lang" => Directive::Lang,
            "t-options" => Directive::Options,
            "t-call" => Directive::Call,
            "t-att" => Directive::Att,
            "t-field" => Directive::Field,
            "t-out" => Directive::Out,
            "t-tag-open" => Directive::TagOpen,
            "t-set" => Directive::Set,
            "t-inner-content" => Directive::InnerContent,
            "t-tag-close" => Directive::TagClose,
            _ => return None,
        })
    }
}

/// `_directives_eval_order`.
pub const DIRECTIVES_ORDER: &[Directive] = &[
    Directive::Elif,
    Directive::Else,
    Directive::Debug,
    Directive::Log,
    Directive::Groups,
    Directive::As,
    Directive::Foreach,
    Directive::If,
    Directive::CallAssets,
    Directive::Lang,
    Directive::Options,
    Directive::Call,
    Directive::Att,
    Directive::Field,
    Directive::Out,
    Directive::TagOpen,
    Directive::Set,
    Directive::InnerContent,
    Directive::TagClose,
];

/// Attributes that have a handler but are not part of the ordered list
/// (`_compile_directive_value`, `_valuef`, `_consumed_options`).
const EXTRA_HANDLER_ATTRS: &[&str] = &["t-value", "t-valuef", "t-consumed-options"];

/// Position in the directive order of one element
/// (`compile_context['iter_directives']`). Handlers that compile the rest of
/// the element (`t-if`, `t-foreach`, `t-groups`, ...) continue the same
/// iteration, so the cursor is passed down by `&mut`.
#[derive(Default)]
struct DirIter {
    pos: usize,
}

impl DirIter {
    fn next(&mut self, order: &[Directive]) -> Option<Directive> {
        let d = order.get(self.pos).copied();
        if d.is_some() {
            self.pos += 1;
        }
        d
    }
}

/// Per-`compile_context` state that Python copies with `dict(compile_context, ...)`.
#[derive(Clone)]
struct Cx {
    attrs_created: bool,
    nsmap: NsMap,
}

type Code<H> = Vec<Stmt<H>>;

/// Pending static text (`_text_concat`). The chunks are stored back to
/// back: `_rstrip_text` only touches the last chunk, so only its start is
/// kept.
#[derive(Default)]
struct TextBuf {
    buf: String,
    /// Start of the last chunk, `None` when there is no chunk.
    last: Option<usize>,
}

impl TextBuf {
    /// Add a chunk.
    fn push(&mut self, s: &str) {
        self.last = Some(self.buf.len());
        self.buf.push_str(s);
    }

    /// Add a formatted chunk.
    fn push_fmt(&mut self, args: std::fmt::Arguments<'_>) {
        self.last = Some(self.buf.len());
        let _ = self.buf.write_fmt(args);
    }

    /// Start a chunk and return the buffer to write it into.
    fn chunk(&mut self) -> &mut String {
        self.last = Some(self.buf.len());
        &mut self.buf
    }

    fn is_empty(&self) -> bool {
        self.last.is_none()
    }

    /// `_rstrip_text`: strip the last chunk, return what was removed.
    fn rstrip(&mut self) -> String {
        let Some(start) = self.last else {
            return String::new();
        };
        let (len, strip) = text::rstrip_newline(&self.buf[start..]);
        let strip = strip.to_owned();
        self.buf.truncate(start + len);
        strip
    }

    /// Remove every chunk and return their concatenation, if not empty.
    fn take(&mut self) -> Option<Box<str>> {
        self.last = None;
        if self.buf.is_empty() {
            return None;
        }
        let text = Box::from(self.buf.as_str());
        self.buf.clear();
        Some(text)
    }
}

/// Template compiler. A compiler compiles one template:
/// see [`Compiler::compile`].
pub struct Compiler<'a, H: Host> {
    host: &'a H,
    ctx: &'a H::Context,
    opts: Arc<CompileOptions>,
    hooks: CompileHooks,
    /// Directive evaluation order.
    order: Box<[Directive]>,
    /// Names of the custom directives (`Directive::Custom` indexes).
    custom: Vec<String>,
    /// The template, mutated while compiling like the Python lxml tree.
    doc: Document,
    /// Pending static text chunks (`_text_concat`).
    text: TextBuf,
    /// Reused buffers for the child lists of the elements being compiled.
    child_lists: Vec<Vec<NodeId>>,
    /// Compiled function bodies (lazy `t-set`/`t-call` bodies, then main).
    blocks: Vec<Code<H>>,
    name: Arc<str>,
    reference: Arc<str>,
    /// Location of each compiled element, indexed by [`NodeId`], taken
    /// before its directives are processed
    /// (`compile_context['_qweb_error_path_xml']`).
    locs: Vec<Option<Loc>>,
    /// The template source, to build location texts on demand.
    loc_src: LocSource,
    /// Elements whose attributes may differ from the parsed source: their
    /// location text is built when recorded.
    changed: Vec<bool>,
    /// A custom directive had mutable access to the tree: every location
    /// recorded from then on is built as text.
    edited: bool,
}

/// Compiler interface given to custom directive handlers
/// ([`Host::compile_directive`]) and directive checks ([`Host::check_directive`]).
pub trait CompileApi<H: Host> {
    /// Element being compiled.
    fn node(&self) -> NodeId;
    /// The template tree. Compiled attributes have been removed from their
    /// elements (as `attrib.pop` does in Python).
    fn doc(&self) -> &Document;
    /// The template tree, for handlers that rewrite it.
    fn doc_mut(&mut self) -> &mut Document;
    /// `_append_text`: add static text to the output.
    fn append_text(&mut self, text: &str);
    /// `_compile_node`: compile an element; its code is added to the code of
    /// the directive.
    fn compile_node(&mut self, id: NodeId) -> Result<()>;
    /// Add a statement run by the host at render time ([`Host::run_hook`]).
    /// Like the Python handlers, pending static text is not flushed first.
    fn emit(&mut self, hook: H::Hook);
    /// Name of the template being compiled.
    fn template_name(&self) -> &str;
}

struct DirApi<'c, 'a, H: Host> {
    c: &'c mut Compiler<'a, H>,
    cx: &'c mut Cx,
    id: NodeId,
    code: Code<H>,
}

impl<H: Host> CompileApi<H> for DirApi<'_, '_, H> {
    fn node(&self) -> NodeId {
        self.id
    }
    fn doc(&self) -> &Document {
        &self.c.doc
    }
    fn doc_mut(&mut self) -> &mut Document {
        self.c.edited = true;
        &mut self.c.doc
    }
    fn append_text(&mut self, text: &str) {
        self.c.append(text);
    }
    fn compile_node(&mut self, id: NodeId) -> Result<()> {
        let code = self.c.compile_node(id, self.cx)?;
        self.code.extend(code);
        Ok(())
    }
    fn emit(&mut self, hook: H::Hook) {
        let loc = self.c.loc(self.id);
        self.code.push(Stmt::Hook { hook, loc });
    }
    fn template_name(&self) -> &str {
        &self.c.name
    }
}

// `is_varname`, `py_isspace` and `py_int` check template attributes that are
// not expressions (`t-set`/`t-as` names, `t-if` emptiness, `t-call` names), so
// they follow the Python checks rather than the expression grammar: e.g.
// `t-as="for"` is a valid varname, but not a valid expression.

/// `VARNAME_REGEXP = r'^[A-Za-z_][A-Za-z0-9_]*$'`
fn is_varname(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// A whitespace character for Python's `str.isspace()` / `str.strip()`.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python `str.isspace()`.
pub fn py_isspace(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_py_space)
}

/// Python `int(s)` for a str that fits in an `i64` (surrounding whitespace,
/// a sign and `_` separators are accepted; other digits than ASCII are not).
///
/// ```
/// use qweb_core::compile::py_int;
/// assert_eq!(py_int(" -1_000\n"), Some(-1000));
/// assert_eq!(py_int("1__0"), None);
/// ```
pub fn py_int(s: &str) -> Option<i64> {
    let t = s.trim_matches(is_py_space);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    if !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_') {
        return None;
    }
    let clean: String = digits.chars().filter(|&c| c != '_').collect();
    // Parse with the sign so that `i64::MIN` is accepted.
    if neg {
        format!("-{clean}").parse().ok()
    } else {
        clean.parse().ok()
    }
}

fn compile_bool(attr: Option<XStr>, default: bool) -> bool {
    if let Some(a) = attr
        && !a.is_empty()
    {
        let l = a.to_lowercase();
        if l == "false" || l == "0" {
            return false;
        } else if l == "true" || l == "1" {
            return true;
        }
    }
    default
}

impl<'a, H: Host> Compiler<'a, H> {
    /// Compile the template whose root element is the document root.
    ///
    /// `source` is the XML `doc` was parsed from (with [`Document::parse`]):
    /// error locations are then built from it only when an error is
    /// reported. Without it, they are built while compiling.
    ///
    /// `name` is used in error messages, `xmlid` and `viewid` become the
    /// `xmlid` / `viewid` template values (`xmlid` defaults to the root
    /// `t-name`).
    pub fn compile(
        host: &'a H,
        ctx: &'a H::Context,
        doc: Document,
        source: Option<Arc<str>>,
        name: Arc<str>,
        xmlid: Option<String>,
        viewid: Option<H::Value>,
    ) -> Result<Program<H>> {
        let opts = host.compile_options(ctx);
        let hooks = host.compile_hooks(ctx)?;
        let mut custom = Vec::new();
        let order: Box<[Directive]> = match &hooks.order {
            None => DIRECTIVES_ORDER.into(),
            Some(names) => names
                .iter()
                .map(|n| {
                    Directive::from_name(n).unwrap_or_else(|| {
                        custom.push(n.clone());
                        Directive::Custom((custom.len() - 1) as u16)
                    })
                })
                .collect(),
        };
        let nsmap = opts.nsmap.clone();
        let reference: Arc<str> = match &viewid {
            Some(v) => {
                let mut s = String::new();
                host.write_str(v, &mut s)?;
                Arc::from(s)
            }
            None => name.clone(),
        };
        let locs = vec![None; doc.nodes.len()];
        let changed = vec![false; doc.nodes.len()];
        let mut c = Compiler {
            host,
            ctx,
            opts,
            hooks,
            order,
            custom,
            doc,
            text: TextBuf::default(),
            child_lists: Vec::new(),
            blocks: Vec::new(),
            name: name.clone(),
            reference: reference.clone(),
            locs,
            loc_src: LocSource::new(source),
            changed,
            edited: false,
        };
        let root = c.doc.root;
        let ref_name = xmlid.or_else(|| c.doc.node(root).get("t-name").map(str::to_owned));
        let mut cx = Cx {
            attrs_created: false,
            nsmap,
        };
        // `self._append_text("", compile_context)` before compiling the root.
        c.append("");
        let mut main = c.compile_root(root, &mut cx)?;
        main.extend(c.flush(true));
        c.blocks.push(main);
        let main = c.blocks.len() - 1;
        Ok(Program {
            blocks: c.blocks,
            main,
            xmlid: match ref_name {
                Some(n) => host.str(&n),
                None => host.none(),
            },
            viewid: viewid.unwrap_or_else(|| host.none()),
            xmlid_key: host.key("xmlid"),
            viewid_key: host.key("viewid"),
            name,
            reference,
            size_hint: std::sync::atomic::AtomicUsize::new(0),
            locs: c.loc_src,
        })
    }

    // ----- text buffer ------------------------------------------------------

    fn append(&mut self, s: &str) {
        self.text.push(s);
    }

    /// `_rstrip_text`.
    fn rstrip(&mut self) -> String {
        self.text.rstrip()
    }

    /// `_flush_text`.
    fn flush(&mut self, rstrip: bool) -> Code<H> {
        if self.text.is_empty() {
            return Vec::new();
        }
        if rstrip {
            self.text.rstrip();
        }
        match self.text.take() {
            Some(text) => vec![Stmt::Text(text)],
            None => Vec::new(),
        }
    }

    // ----- helpers -----------------------------------------------------------

    /// Location of an element for error messages: the one recorded when
    /// its compilation started, or its current state.
    fn loc(&self, id: NodeId) -> Loc {
        match self.locs.get(id).and_then(Option::as_ref) {
            Some(l) => l.clone(),
            None => self.current_loc(id),
        }
    }

    fn current_loc(&self, id: NodeId) -> Loc {
        Loc::current(&self.doc, id)
    }

    /// Record the location of an element whose compilation starts. It is a
    /// node id when the element is as parsed, else its current text.
    fn record_loc(&mut self, id: NodeId) {
        if self.locs.len() <= id {
            // nodes created by custom directives
            self.locs.resize(id + 1, None);
        }
        let as_parsed = !self.edited
            && self.loc_src.has_source()
            && !self.changed.get(id).copied().unwrap_or(true);
        self.locs[id] = Some(if as_parsed {
            Loc::node(id)
        } else {
            self.current_loc(id)
        });
        // compiling pops its attributes
        self.mark_changed(id);
    }

    /// Note that the attributes of an element were modified.
    fn mark_changed(&mut self, id: NodeId) {
        if let Some(c) = self.changed.get_mut(id) {
            *c = true;
        }
    }

    fn located(&self, err: Error, id: NodeId) -> Error {
        let (path, element) = self.loc_src.resolve(&self.loc(id));
        // compile errors are reported with the template reference (`_get_error_info`)
        err.with_location(crate::Frame {
            template: self.reference.clone(),
            reference: self.reference.clone(),
            path,
            element,
        })
    }

    /// Pop the next attribute of `id` whose name matches `f`, starting at
    /// index `*i`: called until `None`, it pops the matching attributes in
    /// source order (`for key in list(el.attrib): ... el.attrib.pop(key)`),
    /// without copying the names first.
    fn take_attr(
        &mut self,
        id: NodeId,
        i: &mut usize,
        f: impl Fn(&str) -> bool,
    ) -> Option<(XStr, XStr)> {
        let attrs = &mut self.doc.node_mut(id).attrs;
        while *i < attrs.len() {
            if f(&attrs[*i].0) {
                // Not `swap_remove`: attribute order is output order.
                return Some(attrs.remove(*i));
            }
            *i += 1;
        }
        None
    }

    fn is_t(&self, id: NodeId) -> bool {
        self.doc.node(id).tag.eq_ignore_ascii_case("t")
    }

    fn el_tag(&self, id: NodeId) -> (XStr, XStr) {
        // (unqualified tag, tag with prefix); short tags are stored inline
        let n = self.doc.node(id);
        if n.nsmap.is_empty() {
            (n.tag.clone(), n.tag.clone())
        } else {
            let local = XStr::from(n.local_name());
            let tag = match &n.prefix {
                Some(p) => format!("{p}:{local}").into(),
                None => local.clone(),
            };
            (local, tag)
        }
    }

    /// Attribute of a directive (`t-<name>`).
    fn dattr(&self, d: Directive) -> std::borrow::Cow<'static, str> {
        match d {
            Directive::Custom(i) => format!("t-{}", self.custom[i as usize]).into(),
            d => d.attr().into(),
        }
    }

    fn dname(&self, d: Directive) -> &str {
        match d {
            Directive::Custom(i) => &self.custom[i as usize],
            d => d.name(),
        }
    }

    /// `getattr(self, f"_compile_directive_{name}")` for a `t-*` attribute.
    fn has_handler(&self, attr: &str) -> bool {
        if Directive::from_handler_attr(attr).is_some() || EXTRA_HANDLER_ATTRS.contains(&attr) {
            return true;
        }
        let name = &attr[2..];
        self.custom.iter().any(|c| c == name) || self.host.has_directive(self.ctx, name)
    }

    /// `_compile_directive` hook: run the host check before a directive.
    fn check(&mut self, id: NodeId, cx: &mut Cx, d: Directive) -> Result<()> {
        if !self.hooks.check_directive {
            return Ok(());
        }
        let name = self.dname(d).to_owned();
        let (host, ctx) = (self.host, self.ctx);
        let mut api = DirApi {
            c: self,
            cx,
            id,
            code: Vec::new(),
        };
        host.check_directive(ctx, &name, &mut api)
    }

    fn directive_custom(&mut self, id: NodeId, cx: &mut Cx, i: u16) -> Result<Code<H>> {
        let name = self.custom[i as usize].clone();
        let (host, ctx) = (self.host, self.ctx);
        let mut api = DirApi {
            c: self,
            cx,
            id,
            code: Vec::new(),
        };
        host.compile_directive(ctx, &name, &mut api)?;
        Ok(api.code)
    }

    /// Compile a Python expression; `raise_on_missing` makes every name
    /// lookup strict. Expressions the native parser does not support are
    /// compiled by the host ([`Host::compile_opaque`]).
    pub fn compile_expr(&self, src: &str, raise_on_missing: bool) -> Result<Ex<H>> {
        if self.hooks.check_expr {
            self.host.check_expr(self.ctx, src)?;
        }
        match expr::parse(src, raise_on_missing) {
            Ok(e) => Ok(self.lower(e)),
            Err(ParseError::Dunder(n)) => Err(Error::syntax(format!(
                "Using variable names with '__' is not allowed: {}",
                py_repr_str(&n)
            ))),
            Err(err) => match self.host.compile_opaque(src, raise_on_missing)? {
                Some(op) => Ok(Ex::Opaque(Box::new(op))),
                None => Err(Error::new(ErrorKind::Syntax(format!(
                    "Can not compile expression: {src} ({err})"
                )))),
            },
        }
    }

    fn lower(&self, e: Expr) -> Ex<H> {
        let h = self.host;
        let b = |e: Expr| Box::new(self.lower(e));
        match e {
            Expr::None => Ex::Const(h.none()),
            Expr::Bool(v) => Ex::Const(h.bool(v)),
            Expr::Int(v) => Ex::Const(h.int(v)),
            Expr::BigInt(s) => match h.big_int(&s) {
                Ok(v) => Ex::Const(v),
                Err(_) => Ex::Unknown(Arc::from(s.as_str())),
            },
            Expr::Float(v) => Ex::Const(h.float(v)),
            Expr::Str(s) => Ex::Const(h.str(&s)),
            Expr::Bytes(v) => Ex::Const(h.bytes(&v)),
            Expr::Name { name, strict } => Ex::Var {
                key: h.key(&name),
                name: Arc::from(name.as_str()),
                strict,
            },
            Expr::Builtin(name) => match h.builtin(&name) {
                Some(v) => Ex::Const(v),
                None => Ex::Unknown(Arc::from(name.as_str())),
            },
            Expr::Attr(o, n) => Ex::Attr(b(*o), h.key(&n)),
            Expr::Item(o, k) => Ex::Item(b(*o), b(*k)),
            Expr::Slice(l, u, s) => Ex::Slice(l.map(|e| b(*e)), u.map(|e| b(*e)), s.map(|e| b(*e))),
            Expr::Call { func, args, kwargs } => Ex::Call {
                func: b(*func),
                args: args.into_iter().map(|a| self.lower(a)).collect(),
                kwargs: kwargs
                    .into_iter()
                    .map(|(k, v)| (h.key(&k), self.lower(v)))
                    .collect(),
            },
            Expr::Unary(op, v) => {
                if let (expr::UnOp::Neg, Expr::Int(i)) = (op, &*v) {
                    return Ex::Const(h.int(-*i));
                }
                if let (expr::UnOp::Neg, Expr::Float(f)) = (op, &*v) {
                    return Ex::Const(h.float(-*f));
                }
                Ex::Unary(op, b(*v))
            }
            Expr::Not(v) => Ex::Not(b(*v)),
            Expr::Bin(op, l, r) => Ex::Bin(op, b(*l), b(*r)),
            Expr::Cmp(first, rest) => Ex::Cmp(
                b(*first),
                rest.into_iter()
                    .map(|(op, e)| (op, self.lower(e)))
                    .collect(),
            ),
            Expr::And(items) => Ex::And(items.into_iter().map(|e| self.lower(e)).collect()),
            Expr::Or(items) => Ex::Or(items.into_iter().map(|e| self.lower(e)).collect()),
            Expr::IfElse { cond, then, orelse } => Ex::IfElse(b(*cond), b(*then), b(*orelse)),
            Expr::Tuple(items) => {
                let lowered: Vec<Ex<H>> = items.into_iter().map(|e| self.lower(e)).collect();
                if lowered.iter().all(|e| matches!(e, Ex::Const(_))) {
                    let vals = lowered
                        .into_iter()
                        .map(|e| {
                            if let Ex::Const(v) = e {
                                v
                            } else {
                                unreachable!()
                            }
                        })
                        .collect();
                    return Ex::Const(h.tuple(vals));
                }
                Ex::Tuple(lowered)
            }
            Expr::List(items) => Ex::List(items.into_iter().map(|e| self.lower(e)).collect()),
            Expr::Set(items) => Ex::Set(items.into_iter().map(|e| self.lower(e)).collect()),
            Expr::Dict(items) => Ex::Dict(
                items
                    .into_iter()
                    .map(|(k, v)| (self.lower(k), self.lower(v)))
                    .collect(),
            ),
        }
    }

    /// `_compile_format`.
    fn compile_format(&self, src: &str) -> Result<Format<H>> {
        let parts = split_format(src);
        if !parts.iter().any(|p| matches!(p, FP::Expr(_))) {
            return Ok(Format::Const(self.host.str(src)));
        }
        let mut out = Vec::with_capacity(parts.len());
        for p in parts {
            match p {
                FP::Lit(l) => out.push(FormatPart::Lit(l.into())),
                FP::Expr(e) => out.push(FormatPart::Expr(self.compile_expr(e, false)?)),
            }
        }
        Ok(Format::Parts(out))
    }

    // ----- nodes -------------------------------------------------------------

    fn compile_root(&mut self, root: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        self.mark_changed(root);
        let n = self.doc.node_mut(root);
        n.pop("t-name");
        if let Some(t) = &n.text {
            let off = text::first_rstrip_offset(t);
            if off > 0 {
                n.text = Some(t[off..].into());
            }
        }
        self.compile_node(root, cx)
    }

    fn is_static_node(&self, id: NodeId) -> bool {
        let n = self.doc.node(id);
        n.tag != "t"
            && !n.has("groups")
            && !n.attrs.iter().any(|(k, _)| {
                k.starts_with("t-")
                    && !matches!(k.as_str(), "t-tag-open" | "t-tag-close" | "t-inner-content")
            })
    }

    fn compile_node(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        if self.doc.node(id).kind != NodeKind::Element {
            return Ok(Vec::new());
        }
        if self.doc.node(id).has("t-qweb-skip") {
            return Ok(Vec::new());
        }
        if self.is_static_node(id) {
            return self.compile_static_node(id, cx);
        }
        let (unqualified, el_tag) = self.el_tag(id);
        self.record_loc(id);
        let mut iter = DirIter::default();
        {
            let n = self.doc.node_mut(id);
            if unqualified != "t" {
                n.set("t-tag-open", el_tag.clone());
                if !is_void(&el_tag) {
                    n.set("t-tag-close", el_tag);
                }
            }
            if !n.has("t-out") && !n.has("t-field") {
                n.set("t-inner-content", "True");
            }
        }
        self.compile_directives(id, cx, &mut iter)
    }

    fn pre_processing_att(&mut self, id: NodeId) -> Result<()> {
        if self.hooks.pre_processing {
            let mut attrs = std::mem::take(&mut self.doc.node_mut(id).attrs);
            let res = self.host.pre_processing_att(self.ctx, &mut attrs);
            self.doc.node_mut(id).attrs = attrs;
            return res;
        }
        // `_remove_translate_suffix`
        let n = self.doc.node(id);
        if !n.attrs.iter().any(|(k, _)| k.ends_with(".translate")) {
            return Ok(());
        }
        // `set` may append a name that ends with `.translate` itself: walk
        // a copy of the names, like `list(el.attrib)`.
        let keys: Vec<XStr> = self
            .doc
            .node(id)
            .attrs
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        let n = self.doc.node_mut(id);
        for key in keys {
            if let Some(base) = key.strip_suffix(".translate")
                && let Some(v) = n.pop(&key)
            {
                n.set(base, v);
            }
        }
        Ok(())
    }

    /// New namespaces of an element compared to the inherited ones (`set(el.nsmap.items()) - set(nsmap.items())`).
    fn new_namespaces(&self, id: NodeId, inherited: &NsMap) -> Vec<(XStr, XStr)> {
        let n = self.doc.node(id);
        n.nsmap
            .iter()
            .filter(|item| !inherited.contains(item))
            .map(|(p, u)| match p {
                None => ("xmlns".into(), u.into()),
                Some(p) => (format!("xmlns:{p}").into(), u.into()),
            })
            .collect()
    }

    /// Restore `prefix:local` for a namespaced attribute name.
    fn attr_qname(&self, id: NodeId, inherited: &NsMap, key: &str) -> String {
        match xml::namespace(key) {
            Some(uri) => {
                let local = xml::local_name(key);
                let n = self.doc.node(id);
                let mut prefix: Option<&Option<String>> = None;
                for (p, u) in inherited.iter().chain(n.nsmap.iter()) {
                    if u == uri {
                        prefix = Some(p);
                    }
                }
                match prefix {
                    Some(Some(p)) => format!("{p}:{local}"),
                    Some(None) => format!("None:{local}"),
                    None if uri == xml::XML_NS => format!("xml:{local}"),
                    None => key.to_owned(),
                }
            }
            None => key.to_owned(),
        }
    }

    fn compile_static_node(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        self.pre_processing_att(id)?;
        let has_ns = !self.doc.node(id).nsmap.is_empty();
        let (unqualified, el_tag) = self.el_tag(id);
        // The attributes are consumed (Python clears them after output).
        let attrs = std::mem::take(&mut self.doc.node_mut(id).attrs);
        let attrib: Vec<(XStr, XStr)> = if !has_ns {
            attrs
        } else {
            let mut a = self.new_namespaces(id, &cx.nsmap);
            for (k, v) in attrs {
                let name = self.attr_qname(id, &cx.nsmap, &k);
                if let Some(slot) = a.iter_mut().find(|(n, _)| *n == name) {
                    slot.1 = v;
                } else {
                    a.push((name.into(), v));
                }
            }
            a
        };
        let attrib = if self.hooks.post_processing {
            let tag = self.doc.node(id).tag.clone();
            self.host.post_processing_static(self.ctx, &tag, attrib)?
        } else {
            attrib
        };
        if unqualified != "t" {
            let s = self.text.chunk();
            s.push('<');
            s.push_str(&el_tag);
            for (name, value) in &attrib {
                s.push(' ');
                s.push_str(name);
                s.push_str("=\"");
                crate::escape::escape_into(s, value);
                s.push('"');
            }
            self.append(if is_void(&el_tag) { "/>" } else { ">" });
        }
        let body = if has_ns {
            let original = cx.nsmap.clone();
            let mut merged: Vec<(Option<String>, String)> = (*cx.nsmap).clone();
            for (p, u) in self.doc.node(id).nsmap.iter() {
                if let Some(slot) = merged.iter_mut().find(|(q, _)| q == p) {
                    slot.1 = u.clone();
                } else {
                    merged.push((p.clone(), u.clone()));
                }
            }
            cx.nsmap = Arc::new(merged);
            let body = self
                .check(id, cx, Directive::InnerContent)
                .and_then(|_| self.directive_inner_content(id, cx));
            cx.nsmap = original;
            body?
        } else {
            self.check(id, cx, Directive::InnerContent)?;
            self.directive_inner_content(id, cx)?
        };
        if unqualified != "t" && !is_void(&el_tag) {
            self.text.push_fmt(format_args!("</{el_tag}>"));
        }
        Ok(body)
    }

    fn compile_directives(
        &mut self,
        id: NodeId,
        cx: &mut Cx,
        iter: &mut DirIter,
    ) -> Result<Code<H>> {
        if self.is_static_node(id) {
            let n = self.doc.node_mut(id);
            n.pop("t-tag-open");
            n.pop("t-inner-content");
            n.pop("t-tag-close");
            return self.compile_static_node(id, cx);
        }
        let mut code = Vec::new();
        let mut last: Option<Directive> = None;
        while let Some(d) = iter.next(&self.order) {
            last = Some(d);
            let n = self.doc.node(id);
            let run = if n.has(&self.dattr(d)) {
                true
            } else {
                match d {
                    Directive::Groups => n.has("groups"),
                    Directive::Att => true,
                    Directive::Options => n.attrs.iter().any(|(k, _)| k.starts_with("t-options-")),
                    _ => false,
                }
            };
            if run {
                let c = self
                    .compile_directive(id, cx, d, iter)
                    .map_err(|e| self.located(e, id))?;
                code.extend(c);
            }
        }
        // Unordered directives still present: Python re-dispatches the last
        // directive of the loop (a long-standing quirk, reproduced).
        let leftovers = self
            .doc
            .node(id)
            .attrs
            .iter()
            .filter(|(k, _)| {
                !SPECIAL_DIRECTIVES.contains(&k.as_str())
                    && k.starts_with("t-")
                    && self.has_handler(k)
            })
            .count();
        for _ in 0..leftovers {
            match last {
                Some(d) => {
                    let c = self
                        .compile_directive(id, cx, d, iter)
                        .map_err(|e| self.located(e, id))?;
                    code.extend(c);
                }
                None => {
                    return Err(self.located(
                        Error::other("cannot access local variable 'directive' where it is not associated with a value"),
                        id,
                    ));
                }
            }
        }
        let remaining: Vec<&str> = self
            .doc
            .node(id)
            .attrs
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !SPECIAL_DIRECTIVES.contains(k))
            .collect();
        if !remaining.is_empty() {
            self.host.warn(&format!(
                "Unknown directives or unused attributes: {:?} from {}\nPath: {}\nTemplate: {}",
                remaining,
                self.doc.start_tag_repr(id),
                self.doc.path(id),
                self.name
            ));
        }
        Ok(code)
    }

    fn compile_directive(
        &mut self,
        id: NodeId,
        cx: &mut Cx,
        d: Directive,
        iter: &mut DirIter,
    ) -> Result<Code<H>> {
        self.check(id, cx, d)?;
        match d {
            Directive::Custom(i) => self.directive_custom(id, cx, i),
            Directive::Elif => self.directive_elif(id, cx, iter),
            Directive::Else => self.directive_else(id),
            Directive::Debug => self.directive_debug(id),
            Directive::Log => Ok(Vec::new()),
            Directive::Groups => self.directive_groups(id, cx, iter),
            Directive::As => self.directive_as(id),
            Directive::Foreach => self.directive_foreach(id, cx, iter),
            Directive::If => self.directive_if(id, cx, iter),
            Directive::CallAssets => self.directive_call_assets(id),
            Directive::Lang => self.directive_lang(id, cx),
            Directive::Options => self.directive_options(id),
            Directive::Call => self.directive_call(id, cx),
            Directive::Att => self.directive_att(id, cx),
            Directive::Field => self.directive_field(id, cx),
            Directive::Out => self.directive_out(id, cx),
            Directive::TagOpen => self.directive_tag_open(id, cx),
            Directive::Set => self.directive_set(id, cx),
            Directive::InnerContent => self.directive_inner_content(id, cx),
            Directive::TagClose => self.directive_tag_close(id),
        }
    }

    // ----- directives --------------------------------------------------------

    fn directive_debug(&mut self, id: NodeId) -> Result<Code<H>> {
        self.doc.node_mut(id).pop("t-debug");
        if !self.opts.dev_mode {
            self.host
                .warn("@t-debug in template is only available in qweb dev mode");
        }
        Ok(Vec::new())
    }

    fn directive_options(&mut self, id: NodeId) -> Result<Code<H>> {
        let mut items = Vec::new();
        let mut i = 0;
        while let Some((key, value)) = self.take_attr(id, &mut i, |k| k.starts_with("t-options-")) {
            let name = &key["t-options-".len()..];
            items.push((self.host.str(name), self.compile_expr(&value, false)?));
        }
        let base = match self.doc.node_mut(id).pop("t-options") {
            Some(v) if !v.is_empty() => Some(self.compile_expr(&v, false)?),
            _ => None,
        };
        let loc = self.loc(id);
        self.doc.node_mut(id).set("t-consumed-options", "True");
        Ok(vec![Stmt::Options { base, items, loc }])
    }

    fn directive_att(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        let n = self.doc.node(id);
        let is_t = if n.nsmap.is_empty() {
            n.tag == "t"
        } else {
            n.local_name() == "t"
        };
        if is_t {
            let produces = ["t-out", "t-esc", "t-raw"]
                .iter()
                .any(|a| n.get(a).is_some_and(|v| !v.is_empty()));
            if !produces && n.parent.is_some() {
                return Ok(Vec::new());
            }
            if n.get("t-consumed-options") != Some("True") {
                return Ok(Vec::new());
            }
        }
        let mut code = Vec::new();
        let loc = self.loc(id);
        if !self.doc.node(id).nsmap.is_empty() {
            for (k, v) in self.new_namespaces(id, &cx.nsmap) {
                code.push(Stmt::AttrSet {
                    name: Arc::from(k.as_str()),
                    value: AttrSrc::Static(Arc::from(v.as_str())),
                    loc: loc.clone(),
                });
            }
        }
        self.pre_processing_att(id)?;
        // static attributes
        let mut i = 0;
        while let Some((key, value)) = self.take_attr(id, &mut i, |k| !k.starts_with("t-")) {
            let name = self.attr_qname(id, &cx.nsmap, &key);
            code.push(Stmt::AttrSet {
                name: Arc::from(name),
                value: AttrSrc::Static(Arc::from(value.as_str())),
                loc: loc.clone(),
            });
        }
        // dynamic attributes, in source order
        let mut i = 0;
        let dynamic = |k: &str| k.starts_with("t-att-") || k.starts_with("t-attf-") || k == "t-att";
        while let Some((key, value)) = self.take_attr(id, &mut i, dynamic) {
            if let Some(name) = key.strip_prefix("t-attf-") {
                code.push(Stmt::AttrSet {
                    name: Arc::from(name),
                    value: AttrSrc::Format(self.compile_format(&value)?),
                    loc: loc.clone(),
                });
            } else if let Some(name) = key.strip_prefix("t-att-") {
                code.push(Stmt::AttrSet {
                    name: Arc::from(name),
                    value: AttrSrc::Expr(self.compile_expr(&value, false)?),
                    loc: loc.clone(),
                });
            } else {
                code.push(Stmt::AttrUpdate {
                    value: self.compile_expr(&value, false)?,
                    loc: loc.clone(),
                });
            }
        }
        if !code.is_empty() {
            code.insert(0, Stmt::AttrsInit);
            cx.attrs_created = true;
        }
        Ok(code)
    }

    fn directive_tag_open(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        let Some(el_tag) = self.doc.node_mut(id).pop("t-tag-open") else {
            return Ok(Vec::new());
        };
        if el_tag.is_empty() {
            return Ok(Vec::new());
        }
        self.text.push_fmt(format_args!("<{el_tag}"));
        let code = if cx.attrs_created {
            let mut code = self.flush(false);
            code.push(Stmt::AttrsFlush {
                tag: Arc::from(self.doc.node(id).tag.as_str()),
            });
            code
        } else {
            Vec::new()
        };
        let closes = self.doc.node(id).has("t-tag-close");
        self.append(if closes { ">" } else { "/>" });
        Ok(code)
    }

    fn directive_tag_close(&mut self, id: NodeId) -> Result<Code<H>> {
        if let Some(el_tag) = self.doc.node_mut(id).pop("t-tag-close")
            && !el_tag.is_empty()
        {
            self.text.push_fmt(format_args!("</{el_tag}>"));
        }

        Ok(Vec::new())
    }

    fn directive_set(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        let is_t = self.is_t(id);
        let mut code = self.flush(is_t);
        let Some(varname) = self.doc.node_mut(id).pop("t-set") else {
            return Ok(code);
        };
        if varname.is_empty() {
            return Err(Error::key("'t-set'"));
        }
        let first = varname.chars().next().unwrap();
        if varname != "0" && first != '{' && !is_varname(&varname) {
            return Err(Error::syntax(
                "The varname can only contain alphanumeric characters and underscores.",
            ));
        }
        if varname.contains("__") {
            return Err(Error::syntax(format!(
                "Using variable names with '__' is not allowed: {}",
                py_repr_str(&varname)
            )));
        }
        let n = self.doc.node(id);
        if n.has("t-value") || n.has("t-valuef") || n.has("t-valuef.translate") || first == '{' {
            if self.doc.node_mut(id).pop("t-inner-content").is_none() {
                return Err(Error::key("'t-inner-content'"));
            }
            if varname == "0" {
                return Err(Error::syntax(
                    "t-set=\"0\" should not be set from t-value or t-valuef",
                ));
            }
        }
        let key = self.host.key(&varname);
        let loc = self.loc(id);
        if let Some(expr) = self.doc.node_mut(id).pop("t-value") {
            let expr = if expr.is_empty() {
                XStr::from("None")
            } else {
                expr
            };
            code.push(Stmt::Set {
                key,
                value: self.compile_expr(&expr, false)?,
                loc,
            });
        } else if let Some(exprf) = self.doc.node_mut(id).pop("t-valuef") {
            code.push(Stmt::SetFormat {
                key,
                fmt: self.compile_format(&exprf)?,
                markup: false,
                loc,
            });
        } else if let Some(exprf) = self.doc.node_mut(id).pop("t-valuef.translate") {
            let markup = self.opts.edit_translations;
            code.push(Stmt::SetFormat {
                key,
                fmt: self.compile_format(&exprf)?,
                markup,
                loc,
            });
        } else if first == '{' {
            code.push(Stmt::SetUpdate {
                value: self.compile_expr(&varname, false)?,
                loc,
            });
        } else {
            let mut sub = Cx {
                attrs_created: false,
                nsmap: cx.nsmap.clone(),
            };
            self.check(id, &mut sub, Directive::InnerContent)?;
            let mut content = self.directive_inner_content(id, &mut sub)?;
            content.extend(self.flush(false));
            if !content.is_empty() {
                self.blocks.push(content);
                code.push(Stmt::SetLazy {
                    key,
                    block: self.blocks.len() - 1,
                    loc: self.loc(id),
                });
            } else {
                code.push(Stmt::SetConst {
                    key,
                    value: self.host.str(""),
                });
            }
        }
        Ok(code)
    }

    fn directive_inner_content(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        self.doc.node_mut(id).pop("t-inner-content");
        let mut local;
        let cx: &mut Cx = if !self.doc.node(id).nsmap.is_empty() {
            local = Cx {
                attrs_created: cx.attrs_created,
                nsmap: self.doc.node(id).nsmap.clone(),
            };
            &mut local
        } else {
            cx
        };
        if let Some(t) = &self.doc.node(id).text {
            self.text.push(t);
        }
        let mut body = Vec::new();
        // `for item in list(el)`: a snapshot, in a reused buffer.
        let mut children = self.child_lists.pop().unwrap_or_default();
        children.extend_from_slice(&self.doc.node(id).children);
        let res = (|| -> Result<()> {
            for &item in &children {
                let n = self.doc.node(item);
                match n.kind {
                    NodeKind::Comment => {
                        if self.opts.preserve_comments {
                            let t = n.text.as_deref().unwrap_or("");
                            self.text.push_fmt(format_args!("<!--{t}-->"));
                        }
                    }
                    NodeKind::Pi => {
                        if self.opts.preserve_comments {
                            let t = n.text.as_deref().unwrap_or("");
                            self.text.push_fmt(format_args!("<?{} {t}?>", n.tag));
                        }
                    }
                    NodeKind::Element => {
                        body.extend(self.compile_node(item, cx)?);
                    }
                }
                if let Some(tail) = &self.doc.node(item).tail {
                    self.text.push(tail);
                }
            }
            Ok(())
        })();
        children.clear();
        self.child_lists.push(children);
        res?;
        Ok(body)
    }

    fn directive_if(&mut self, id: NodeId, cx: &mut Cx, iter: &mut DirIter) -> Result<Code<H>> {
        // `el.attrib.pop('t-if', el.attrib.pop('t-elif', None))`: t-elif is always popped.
        let elif = self.doc.node_mut(id).pop("t-elif");
        let expr = self.doc.node_mut(id).pop("t-if").or(elif);
        let Some(expr) = expr else {
            return Err(Error::other("'NoneType' object has no attribute 'isspace'"));
        };
        if py_isspace(&expr) {
            return Err(Error::assertion(
                "t-if or t-elif expression should not be empty.",
            ));
        }
        let mut strip = self.rstrip();
        if self.is_t(id)
            && let Some(t) = &self.doc.node(id).text
            && text::lstrip_matches(t)
        {
            strip.clear();
        }
        let mut code = self.flush(false);
        let loc = self.loc(id);
        let cond = self.compile_expr(&expr, false)?;
        if !strip.is_empty() {
            self.append(&strip);
        }
        let mut then = self.compile_directives(id, cx, iter)?;
        then.extend(self.flush(true));

        // look for t-else / t-elif
        let mut next = self.doc.next_sibling(id);
        let mut comments = Vec::new();
        while let Some(nid) = next {
            if self.doc.node(nid).kind == NodeKind::Comment {
                comments.push(nid);
                next = self.doc.next_sibling(nid);
            } else {
                break;
            }
        }
        let mut orelse = Vec::new();
        if let Some(nid) = next {
            let nn = self.doc.node(nid);
            if nn.kind == NodeKind::Element && (nn.has("t-else") || nn.has("t-elif")) {
                self.mark_changed(nid);
                self.doc.node_mut(nid).set("t-else-valid", "True");
                if let Some(parent) = self.doc.node(id).parent {
                    for c in comments {
                        self.doc.remove_child(parent, c);
                    }
                }
                if let Some(tail) = &self.doc.node(id).tail
                    && !py_isspace(tail)
                {
                    return Err(Error::syntax(
                        "Unexpected non-whitespace characters between t-if and t-else directives",
                    ));
                }
                self.doc.node_mut(id).tail = None;
                if !strip.is_empty() {
                    self.append(&strip);
                }
                orelse = self.compile_node(nid, cx)?;
                orelse.extend(self.flush(true));
                self.doc.node_mut(nid).set("t-qweb-skip", "True");
            }
        }
        code.push(Stmt::If {
            cond,
            then,
            orelse,
            loc,
        });
        Ok(code)
    }

    fn directive_elif(&mut self, id: NodeId, cx: &mut Cx, iter: &mut DirIter) -> Result<Code<H>> {
        if self.doc.node_mut(id).pop("t-else-valid").is_none() {
            return Err(Error::syntax(
                "t-elif directive must be preceded by t-if or t-elif directive",
            ));
        }
        self.directive_if(id, cx, iter)
    }

    fn directive_else(&mut self, id: NodeId) -> Result<Code<H>> {
        if self.doc.node_mut(id).pop("t-else-valid").is_none() {
            return Err(Error::syntax(
                "t-elif directive must be preceded by t-if or t-elif directive",
            ));
        }
        self.doc.node_mut(id).pop("t-else");
        Ok(Vec::new())
    }

    fn directive_groups(&mut self, id: NodeId, cx: &mut Cx, iter: &mut DirIter) -> Result<Code<H>> {
        let g2 = self.doc.node_mut(id).pop("groups");
        let groups = self
            .doc
            .node_mut(id)
            .pop("t-groups")
            .or(g2)
            .unwrap_or_else(|| "None".into());
        let strip = self.rstrip();
        let mut code = self.flush(false);
        if !strip.is_empty() && !self.is_t(id) {
            self.append(&strip);
        }
        let mut body = self.compile_directives(id, cx, iter)?;
        body.extend(self.flush(true));
        code.push(Stmt::Groups {
            groups: groups.into_boxed_str(),
            body,
        });
        Ok(code)
    }

    fn directive_as(&mut self, id: NodeId) -> Result<Code<H>> {
        if !self.doc.node(id).has("t-foreach") {
            return Err(Error::syntax("t-as must be on the same node of t-foreach"));
        }
        Ok(Vec::new())
    }

    fn directive_foreach(
        &mut self,
        id: NodeId,
        cx: &mut Cx,
        iter: &mut DirIter,
    ) -> Result<Code<H>> {
        let Some(expr_foreach) = self.doc.node_mut(id).pop("t-foreach") else {
            return Err(Error::key("'t-foreach'"));
        };
        let Some(expr_as) = self.doc.node_mut(id).pop("t-as") else {
            return Err(Error::key("'t-as'"));
        };
        if expr_as.is_empty() {
            return Err(Error::key("'t-as'"));
        }
        if !is_varname(&expr_as) {
            return Err(Error::value(format!(
                "The varname {} can only contain alphanumeric characters and underscores.",
                py_repr_str(&expr_as)
            )));
        }
        if self.is_t(id) {
            self.rstrip();
        }
        let mut code = self.flush(false);
        let loc = self.loc(id);
        let mut body = self.compile_directives(id, cx, iter)?;
        body.extend(self.flush(true));
        let source = if !expr_foreach.is_empty() && expr_foreach.bytes().all(|b| b.is_ascii_digit())
        {
            match expr_foreach.parse::<i64>() {
                Ok(n) => Err(n),
                Err(_) => Ok(self.compile_expr(&expr_foreach, false)?),
            }
        } else {
            Ok(self.compile_expr(&expr_foreach, false)?)
        };
        let h = self.host;
        let k = |suffix: &str| h.key(&format!("{expr_as}{suffix}"));
        code.push(Stmt::Foreach(Box::new(Foreach {
            source,
            var: h.key(&expr_as),
            index: k("_index"),
            value: k("_value"),
            first: k("_first"),
            last: k("_last"),
            odd: k("_odd"),
            even: k("_even"),
            parity: k("_parity"),
            size: k("_size"),
            parity_odd: h.str("odd"),
            parity_even: h.str("even"),
            body,
            loc,
        })));
        Ok(code)
    }

    fn directive_out(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        let (expr, is_field) = match self.doc.node_mut(id).pop("t-out") {
            Some(e) => (e, false),
            None => (
                self.doc.node_mut(id).pop("t-field").unwrap_or_default(),
                true,
            ),
        };
        let flush = self.flush(false);
        let loc = self.loc(id);
        let code_options = self.doc.node_mut(id).pop("t-consumed-options");
        let consumed = code_options.as_deref() == Some("True");
        let is_slot = expr == "0" && !consumed;
        let tag = self.doc.node(id).tag.clone();

        let mut content = None;
        let mut convert = None;
        let mut force_display_dependent = false;
        if !is_slot {
            if is_field {
                let Some((record, field_name)) = expr.rsplit_once('.') else {
                    return Err(Error::value(
                        "not enough values to unpack (expected 2, got 1)",
                    ));
                };
                let merge = cx.attrs_created;
                cx.attrs_created = true;
                content = Some(OutContent::Field {
                    record: self.compile_expr(record, true)?,
                    field: field_name.into(),
                });
                convert = Some(Convert {
                    field: true,
                    expression: expr.clone().into(),
                    tag: tag.clone().into(),
                    merge,
                });
                force_display_dependent = true;
            } else {
                content = Some(if expr == "0" {
                    OutContent::Slot
                } else {
                    OutContent::Expr(self.compile_expr(&expr, false)?)
                });
                if consumed {
                    let merge = cx.attrs_created;
                    cx.attrs_created = true;
                    convert = Some(Convert {
                        field: false,
                        expression: expr.clone().into(),
                        tag: tag.clone().into(),
                        merge,
                    });
                    force_display_dependent = true;
                }
            }
        }

        let mut iter = DirIter::default();
        let mut tag_open = self.compile_directive(id, cx, Directive::TagOpen, &mut iter)?;
        tag_open.extend(self.flush(false));
        let mut tag_close = self.compile_directive(id, cx, Directive::TagClose, &mut iter)?;
        tag_close.extend(self.flush(false));
        let mut default_body =
            self.compile_directive(id, cx, Directive::InnerContent, &mut iter)?;
        default_body.extend(self.flush(false));

        let mut code = flush;
        if is_slot {
            code.extend(tag_open);
            code.push(Stmt::Slot { loc: loc.clone() });
            code.extend(tag_close);
            return Ok(code);
        }
        self.doc.node_mut(id).pop("t-tag");
        let otherwise = if !default_body.is_empty() || !self.text.is_empty() {
            // (the text buffer is always empty here, flushed above)
            Otherwise::Default(default_body)
        } else if force_display_dependent {
            Otherwise::ForceDisplay {
                has_tag: !(tag_open.is_empty() && tag_close.is_empty()),
            }
        } else {
            Otherwise::Nothing
        };
        code.push(Stmt::Out(Box::new(Out {
            content: content.unwrap(),
            convert,
            tag_open,
            tag_close,
            otherwise,
            loc,
        })));
        Ok(code)
    }

    fn directive_field(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        let n = self.doc.node(id);
        let tag = n.tag.as_str();
        if [
            "table", "tbody", "thead", "tfoot", "tr", "td", "li", "ul", "ol", "dl", "dt", "dd",
        ]
        .contains(&tag)
        {
            return Err(Error::assertion(format!(
                "QWeb widgets do not work correctly on {} elements",
                py_repr_str(tag)
            )));
        }
        if tag == "t" {
            return Err(Error::assertion(
                "t-field can not be used on a t element, provide an actual HTML node",
            ));
        }
        if !n.get("t-field").unwrap_or("").contains('.') {
            return Err(Error::assertion(
                "t-field must have at least a dot like 'record.field_name'",
            ));
        }
        self.directive_out(id, cx)
    }

    fn directive_call(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        let expr = self.doc.node_mut(id).pop("t-call").unwrap_or_default();
        let n = self.doc.node(id);
        let el_tag = if n.nsmap.is_empty() {
            n.tag.as_str()
        } else {
            n.local_name()
        };
        if el_tag != "t" {
            return Err(Error::syntax(format!(
                "t-call must be on a <t> element (actually on <{el_tag}>)."
            )));
        }
        let is_t = self.is_t(id);
        let mut code = self.flush(is_t);
        let loc = self.loc(id);
        self.doc.node_mut(id).pop("t-consumed-options");
        let nsmap = if cx.nsmap.is_empty() {
            None
        } else {
            Some(cx.nsmap.clone())
        };

        let n = self.doc.node(id);
        let has_body = !n.children.is_empty() || n.text.as_deref().is_some_and(|t| !t.is_empty());
        let body = if has_body {
            let mut sub = Cx {
                attrs_created: false,
                nsmap: cx.nsmap.clone(),
            };
            self.check(id, &mut sub, Directive::InnerContent)?;
            let mut content = self.directive_inner_content(id, &mut sub)?;
            self.append("");
            content.extend(self.flush(true));
            self.blocks.push(content);
            Some(self.blocks.len() - 1)
        } else {
            None
        };

        let mut args = Vec::new();
        let mut i = 0;
        let is_arg = |k: &str| {
            k.ends_with(".f") || k.ends_with(".translate") || !k.starts_with("t-") || k == "t-args"
        };
        while let Some((key, value)) = self.take_attr(id, &mut i, is_arg) {
            if let Some(name) = key.strip_suffix(".f") {
                args.push(CallArg::Format(
                    self.host.key(name),
                    self.compile_format(&value)?,
                ));
            } else if key.ends_with(".translate") {
                let name = key.trim_end_matches(".translate");
                let name = name.strip_suffix(".f").unwrap_or(name);
                let fmt = self.compile_format(&value)?;
                if self.opts.edit_translations {
                    args.push(CallArg::MarkupFormat(self.host.key(name), fmt));
                } else {
                    args.push(CallArg::Format(self.host.key(name), fmt));
                }
            } else if !key.starts_with("t-") {
                args.push(CallArg::Expr(
                    self.host.key(&key),
                    self.compile_expr(&value, false)?,
                ));
            } else {
                args.push(CallArg::Args(self.compile_expr(&value, false)?));
            }
        }

        let target = match self.compile_format(&expr)? {
            Format::Const(_) => CallTarget::Static(TemplateRef::parse(&expr)),
            fmt => CallTarget::Dynamic(fmt),
        };
        code.push(Stmt::Call(Box::new(Call {
            body,
            args,
            target,
            nsmap,
            loc,
        })));
        Ok(code)
    }

    fn directive_lang(&mut self, id: NodeId, cx: &mut Cx) -> Result<Code<H>> {
        if !self.doc.node(id).has("t-call") {
            return Err(Error::syntax(
                "t-lang is an alias of t-options-lang but only available on the same node of t-call",
            ));
        }
        let v = self.doc.node_mut(id).pop("t-lang").unwrap_or_default();
        self.doc.node_mut(id).set("t-options-lang", v);
        self.compile_node(id, cx)
    }

    fn directive_call_assets(&mut self, id: NodeId) -> Result<Code<H>> {
        if !self.doc.node(id).children.is_empty() {
            return Err(Error::syntax("t-call-assets cannot contain children nodes"));
        }
        let mut code = self.flush(false);
        let n = self.doc.node_mut(id);
        let bundle = n.pop("t-call-assets").unwrap_or_default();
        let css = compile_bool(n.pop("t-css"), true);
        let js = compile_bool(n.pop("t-js"), true);
        let binary = compile_bool(n.pop("t-binary"), false);
        let defer_load = compile_bool(n.pop("defer_load"), false);
        let lazy_load = compile_bool(n.pop("lazy_load"), false);
        let media = n.pop("media").map(XStr::into_boxed_str);
        let autoprefix = compile_bool(n.pop("t-autoprefix"), false);
        code.push(Stmt::Assets(Box::new(Assets {
            bundle: bundle.into(),
            css,
            js,
            binary,
            autoprefix,
            defer_load,
            lazy_load,
            media,
        })));
        Ok(code)
    }
}

/// Python `repr()` of a str (used in error messages).
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
