//! The [`Host`] trait: everything value-related is delegated to the host.
//!
//! The core never inspects values itself. The Python binding implements the
//! trait on top of the CPython object protocol (giving exact Python
//! semantics); `qweb-value` provides a pure-Rust implementation.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::expr::{BinOp, CmpOp, UnOp};
use crate::compile::CompileApi;
use crate::ir::Program;
use crate::runtime::{AttrVal, Lazy};
use crate::xml::NsMap;

/// Template reference, like `_id_or_xmlid`: numeric names are ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TemplateRef {
    Id(i64),
    Name(Arc<str>),
}

impl TemplateRef {
    /// `_id_or_xmlid`: all-digit names become ids.
    pub fn parse(s: &str) -> TemplateRef {
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(id) = s.parse() {
                return TemplateRef::Id(id);
            }
        }
        TemplateRef::Name(Arc::from(s))
    }
}

impl std::fmt::Display for TemplateRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateRef::Id(i) => write!(f, "{i}"),
            TemplateRef::Name(n) => write!(f, "{n}"),
        }
    }
}

/// A loaded template: its XML source, or a program the host compiled
/// (and cached) itself with [`crate::Engine::compile_program`].
pub enum Loaded<H: Host> {
    Source {
        /// XML source of the template element (its root is the template).
        source: Arc<str>,
        /// Value of `viewid` (template id), `None` when unknown.
        viewid: Option<H::Value>,
        /// Value of `xmlid`; when `None` the root `t-name` attribute is used.
        xmlid: Option<String>,
    },
    Program(Arc<Program<H>>),
}

impl<H: Host> Loaded<H> {
    /// A template source without ids.
    pub fn source(source: impl Into<Arc<str>>) -> Self {
        Loaded::Source { source: source.into(), viewid: None, xmlid: None }
    }
}

/// Compile hooks enabled for one compilation (see [`Host::compile_hooks`]).
#[derive(Debug, Clone, Default)]
pub struct CompileHooks {
    /// Directive evaluation order (`_directives_eval_order`, names without
    /// `t-`). Names that are not built-in are custom directives handled by
    /// [`Host::compile_directive`]. `None` keeps the built-in order.
    pub order: Option<Vec<String>>,
    /// Call [`Host::check_directive`] before every directive.
    pub check_directive: bool,
    /// Call [`Host::check_expr`] for every compiled expression.
    pub check_expr: bool,
    /// Use [`Host::pre_processing_att`] instead of the built-in
    /// `_pre_processing_att` (removal of the `.translate` suffixes).
    pub pre_processing: bool,
    /// Use [`Host::post_processing_static`] for the attributes of static nodes.
    pub post_processing: bool,
}

/// Options that change the compiled code; part of the template cache key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CompileOptions {
    pub preserve_comments: bool,
    pub edit_translations: bool,
    pub dev_mode: bool,
    /// Namespaces inherited from the caller (`t-call` inside namespaced XML).
    pub nsmap: Vec<(Option<String>, String)>,
    /// Extra host-defined cache key parts (e.g. `lang`).
    pub extra: Vec<String>,
}

/// How `t-foreach` iterates a value (see `_compile_directive_foreach`).
pub struct LoopSource<I> {
    pub iter: I,
    /// Size when the value is `Sized` (or an int).
    pub size: Option<usize>,
    /// The value is a `Mapping`: items are `(key, value)` pairs.
    pub mapping: bool,
}

/// Result of a field/widget conversion: `(attributes, content, force_display)`.
pub struct Converted<V> {
    pub attrs: Vec<(V, V)>,
    pub content: V,
    pub force_display: bool,
}

pub trait Host: Sized + Send + Sync + 'static {
    type Value: Clone + Send + Sync;
    /// Interned scope key / attribute name.
    type Key: Clone + Send + Sync;
    /// Template values (the `values` dict).
    type Scope: Send + Sync;
    /// Host-compiled expression for constructs the native evaluator lacks.
    type Opaque: Send + Sync;
    /// Rendering context (the `env.context` / render options).
    type Context: Clone + Send + Sync;
    type Iter: Iterator<Item = Result<Self::Value>>;
    /// Payload of a statement emitted by a custom directive ([`CompileApi::emit`]).
    type Hook: Send + Sync;

    // ----- constants -------------------------------------------------------
    fn key(&self, name: &str) -> Self::Key;
    /// Key of the `t-call` body (`values[0]`, an int key in Python).
    fn slot_key(&self) -> Self::Key;
    fn none(&self) -> Self::Value;
    fn bool(&self, b: bool) -> Self::Value;
    fn int(&self, i: i64) -> Self::Value;
    fn big_int(&self, digits: &str) -> Result<Self::Value>;
    fn float(&self, f: f64) -> Self::Value;
    fn str(&self, s: &str) -> Self::Value;
    /// `Markup(s)`.
    fn markup(&self, s: &str) -> Self::Value;
    fn bytes(&self, b: &[u8]) -> Self::Value;
    fn tuple(&self, items: Vec<Self::Value>) -> Self::Value;
    fn list(&self, items: Vec<Self::Value>) -> Self::Value;
    fn set(&self, items: Vec<Self::Value>) -> Result<Self::Value>;
    fn dict(&self, items: Vec<(Self::Value, Self::Value)>) -> Result<Self::Value>;
    fn slice(&self, lower: Option<Self::Value>, upper: Option<Self::Value>, step: Option<Self::Value>) -> Self::Value;
    /// Builtin function by name (see `expr::BUILTINS`).
    fn builtin(&self, name: &str) -> Option<Self::Value>;
    /// Compile an expression the native parser does not support.
    /// Return `Ok(None)` if the host cannot evaluate source expressions.
    fn compile_opaque(&self, _src: &str, _raise_on_missing: bool) -> Result<Option<Self::Opaque>> {
        Ok(None)
    }
    fn eval_opaque(&self, _code: &Self::Opaque, _scope: &Self::Scope) -> Result<Self::Value> {
        Err(Error::not_implemented("opaque expressions are not supported by this host"))
    }

    // ----- scope -----------------------------------------------------------
    fn scope_new(&self) -> Self::Scope;
    fn scope_copy(&self, scope: &Self::Scope) -> Self::Scope;
    fn scope_get(&self, scope: &Self::Scope, key: &Self::Key) -> Option<Self::Value>;
    fn scope_contains(&self, scope: &Self::Scope, key: &Self::Key) -> bool {
        self.scope_get(scope, key).is_some()
    }
    fn scope_set(&self, scope: &mut Self::Scope, key: &Self::Key, value: Self::Value);
    fn scope_remove(&self, scope: &mut Self::Scope, key: &Self::Key);
    /// `scope.update(other)`.
    fn scope_update(&self, scope: &mut Self::Scope, other: &Self::Scope);
    /// `scope.update(value)` (`t-set="{...}"`).
    fn scope_update_value(&self, scope: &mut Self::Scope, value: &Self::Value) -> Result<()>;
    /// `t-args` semantics: dict → update, a single `(k, v)` pair, or a list of pairs.
    fn scope_update_pairs(&self, scope: &mut Self::Scope, value: &Self::Value) -> Result<()>;
    /// Error raised by a strict lookup of a missing name (`KeyError`).
    fn missing_name(&self, name: &str) -> Error {
        Error::key(format!("{name:?}"))
    }

    // ----- object protocol -------------------------------------------------
    fn get_attr(&self, obj: &Self::Value, name: &Self::Key) -> Result<Self::Value>;
    fn get_item(&self, obj: &Self::Value, key: &Self::Value) -> Result<Self::Value>;
    fn call(&self, func: &Self::Value, args: Vec<Self::Value>, kwargs: Vec<(Self::Key, Self::Value)>) -> Result<Self::Value>;
    fn unary(&self, op: UnOp, v: &Self::Value) -> Result<Self::Value>;
    fn binary(&self, op: BinOp, a: &Self::Value, b: &Self::Value) -> Result<Self::Value>;
    /// Rich comparison (`<`, `<=`, `==`, `!=`, `>`, `>=` only).
    fn compare(&self, op: CmpOp, a: &Self::Value, b: &Self::Value) -> Result<Self::Value>;
    /// `item in container`.
    fn contains(&self, container: &Self::Value, item: &Self::Value) -> Result<bool>;
    /// `a is b`.
    fn is(&self, a: &Self::Value, b: &Self::Value) -> bool;
    fn truthy(&self, v: &Self::Value) -> Result<bool>;
    /// `v is None or v is False`.
    fn is_none_or_false(&self, v: &Self::Value) -> bool;
    /// `isinstance(v, str)`.
    fn is_str(&self, v: &Self::Value) -> bool;

    /// `{**base, k: v, ...}` (`t-options` with `t-options-*`).
    fn merge_dict(&self, base: Option<&Self::Value>, items: Vec<(Self::Value, Self::Value)>) -> Result<Self::Value>;

    // ----- text conversion -------------------------------------------------
    /// `IrQweb._compile_to_str` returning a value: str instances (including
    /// Markup) unchanged, `None`/`False` → `''`, others converted.
    fn text_value(&self, v: &Self::Value) -> Result<Self::Value>;
    /// `IrQweb._compile_to_str`: `None`/`False` → `''`, str as is, buffers
    /// decoded, anything else through `str()`.
    fn write_text(&self, v: &Self::Value, out: &mut String) -> Result<()>;
    /// `str(v)` (used for attribute names and values).
    fn write_str(&self, v: &Self::Value, out: &mut String) -> Result<()>;
    /// `str(escape(v))`: markup (`__html__`) unescaped, everything else
    /// converted with `str()` and escaped.
    fn write_escaped(&self, v: &Self::Value, out: &mut String) -> Result<()>;

    // ----- t-foreach / t-att -----------------------------------------------
    fn iterate(&self, v: Self::Value) -> Result<LoopSource<Self::Iter>>;
    /// Unpack a 2-item sequence (`k, v = item`).
    fn unpack_pair(&self, item: Self::Value) -> Result<(Self::Value, Self::Value)>;
    /// `t-att` / `t-args` value: dict items, a single `(name, value)` pair or a
    /// list of pairs. `Ok(None)` when the value is none of these (ignored).
    fn pairs(&self, v: &Self::Value) -> Result<Option<Vec<(Self::Value, Self::Value)>>>;

    // ----- lazy content (`QwebContent`) ------------------------------------
    fn wrap_lazy(&self, lazy: Arc<Lazy<Self>>) -> Self::Value;
    fn as_lazy(&self, v: &Self::Value) -> Option<Arc<Lazy<Self>>>;

    // ----- context and loading ---------------------------------------------
    fn compile_options(&self, ctx: &Self::Context) -> CompileOptions;
    /// Context for a `t-call`: `ctx` updated with the `t-options` dict and
    /// the namespaces of the caller.
    fn call_context(&self, ctx: &Self::Context, options: Option<&Self::Value>, nsmap: Option<&NsMap>) -> Result<Self::Context>;
    fn load(&self, template: &TemplateRef, ctx: &Self::Context) -> Result<Loaded<Self>>;
    /// Default `t-options` value when none was set (`{}`).
    fn empty_options(&self) -> Self::Value;

    // ----- ORM-dependent directives (unsupported by default, like `render()`) -
    fn has_groups(&self, _ctx: &Self::Context, _groups: &str) -> Result<bool> {
        Err(Error::not_implemented("Groups are not allowed in this rendering mode."))
    }
    #[allow(clippy::too_many_arguments)]
    fn get_field(
        &self,
        _ctx: &Self::Context,
        _record: &Self::Value,
        _field: &str,
        _expression: &str,
        _tag: &str,
        _options: Self::Value,
        _scope: &Self::Scope,
    ) -> Result<Converted<Self::Value>> {
        Err(Error::not_implemented(
            "Fields are not allowed in this rendering mode. Please use \"env['ir.qweb']._render\" method",
        ))
    }
    #[allow(clippy::too_many_arguments)]
    fn get_widget(
        &self,
        _ctx: &Self::Context,
        _value: &Self::Value,
        _expression: &str,
        _tag: &str,
        _options: Self::Value,
        _scope: &Self::Scope,
    ) -> Result<Converted<Self::Value>> {
        Err(Error::not_implemented(
            "Widgets are not allowed in this rendering mode. Please use \"env['ir.qweb']._render\" method",
        ))
    }
    /// `_get_asset_nodes`: list of `(tag, attributes)`.
    #[allow(clippy::too_many_arguments)]
    fn get_asset_nodes(
        &self,
        _ctx: &Self::Context,
        _bundle: &str,
        _css: bool,
        _js: bool,
        _binary: bool,
        _autoprefix: bool,
        _debug: Option<&Self::Value>,
        _defer_load: bool,
        _lazy_load: bool,
        _media: Option<&str>,
    ) -> Result<Vec<(String, Vec<(Self::Value, Self::Value)>)>> {
        Err(Error::not_implemented(
            "Assets are not allowed in this rendering mode. Please use \"env['ir.qweb']._render\" method",
        ))
    }

    /// Compile-time warnings (unknown directives, ...).
    fn warn(&self, _msg: &str) {}

    // ----- extension hooks (all optional) ------------------------------------
    /// Hooks used to compile a template in this context (called once per compilation).
    fn compile_hooks(&self, _ctx: &Self::Context) -> Result<CompileHooks> {
        Ok(CompileHooks::default())
    }
    /// Whether a handler exists for the custom directive `t-<name>`.
    fn has_directive(&self, _ctx: &Self::Context, _name: &str) -> bool {
        false
    }
    /// Compile the custom directive `t-<name>` (a name of [`CompileHooks::order`]
    /// that is not built-in) on `api.node()`.
    fn compile_directive(&self, _ctx: &Self::Context, name: &str, _api: &mut dyn CompileApi<Self>) -> Result<()> {
        Err(Error::type_error(format!("no handler for the directive 't-{name}'")))
    }
    /// Called before compiling each directive (when enabled), e.g. to restrict
    /// the allowed directives.
    fn check_directive(&self, _ctx: &Self::Context, _name: &str, _api: &mut dyn CompileApi<Self>) -> Result<()> {
        Ok(())
    }
    /// Called for each compiled expression (when enabled).
    fn check_expr(&self, _ctx: &Self::Context, _expr: &str) -> Result<()> {
        Ok(())
    }
    /// `_pre_processing_att` (when enabled): edit the attributes of an element
    /// before they are compiled. Must also remove the `.translate` suffixes.
    fn pre_processing_att(&self, _ctx: &Self::Context, _attrs: &mut Vec<(String, String)>) -> Result<()> {
        Ok(())
    }
    /// `_post_processing_att` for a static node (when enabled): returns the
    /// attributes to output, as text, already filtered (`value or isinstance(value, str)`).
    fn post_processing_static(
        &self,
        _ctx: &Self::Context,
        _tag: &str,
        attrs: Vec<(String, String)>,
    ) -> Result<Vec<(String, String)>> {
        Ok(attrs)
    }
    /// Whether dynamic attributes go through [`Host::post_processing_att`]
    /// instead of the built-in `_post_processing_att` (the `href` check).
    fn custom_post_processing(&self, _ctx: &Self::Context) -> bool {
        false
    }
    /// `_post_processing_att` for dynamic attributes: returns the attributes
    /// to output (`(name, value)`, rendered when `value or isinstance(value, str)`).
    fn post_processing_att(
        &self,
        _ctx: &Self::Context,
        _tag: &str,
        _attrs: Vec<(Arc<str>, AttrVal<Self>)>,
    ) -> Result<Vec<(Self::Value, Self::Value)>> {
        Err(Error::not_implemented("post_processing_att"))
    }
    /// Run a statement emitted by a custom directive, writing its output.
    fn run_hook(&self, _ctx: &Self::Context, _hook: &Self::Hook, _scope: &Self::Scope, _out: &mut String) -> Result<()> {
        Err(Error::not_implemented("hooks are not supported by this host"))
    }
}
