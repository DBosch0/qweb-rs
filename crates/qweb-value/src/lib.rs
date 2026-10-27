//! # qweb-value
//!
//! Pure-Rust [`Host`] for `qweb-core` with Python-like values. Useful to
//! render QWeb templates from Rust (or WASM) with JSON-like data, and as the
//! reference host for tests and benchmarks.
//!
//! ```
//! use qweb_value::{ValueHost, Value};
//! let host = ValueHost::new();
//! host.add_template("hello", r#"<t t-name="hello"><p t-out="name"/></t>"#);
//! let out = host.render("hello", [("name", Value::str("<world>"))]).unwrap();
//! assert_eq!(out, "<p>&lt;world&gt;</p>");
//! ```

pub mod ops;
pub mod value;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use indexmap::IndexMap;
use qweb_core::expr::{BinOp, CmpOp, UnOp};
use qweb_core::host::{CompileOptions, Loaded, LoopSource, TemplateRef};
use qweb_core::xml::NsMap;
use qweb_core::{Engine, Error, Host, Lazy, Result};

pub use value::{Builtin, HKey, Value};

/// Scope: the template `values` dict.
pub type Scope = IndexMap<Arc<str>, Value>;

/// Render options (`env.context`).
#[derive(Clone, Default)]
pub struct Context {
    pub options: Arc<IndexMap<String, Value>>,
    pub nsmap: Option<NsMap>,
}

impl Context {
    pub fn new<I: IntoIterator<Item = (String, Value)>>(options: I) -> Self {
        Context { options: Arc::new(options.into_iter().collect()), nsmap: None }
    }
}

type LoaderFn = dyn Fn(&TemplateRef) -> Result<String> + Send + Sync;

/// Pure-Rust host. Templates are registered by name (or provided by a loader).
#[derive(Clone)]
pub struct ValueHost {
    inner: Arc<HostInner>,
}

struct HostInner {
    templates: RwLock<HashMap<String, Arc<str>>>,
    loader: Option<Box<LoaderFn>>,
    warnings: Mutex<Vec<String>>,
}

/// Key of `values[0]` (the t-call body); not reachable from expressions.
const SLOT_KEY: &str = "\u{0}0";

impl Default for ValueHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ValueHost {
    pub fn new() -> Self {
        ValueHost {
            inner: Arc::new(HostInner { templates: RwLock::new(HashMap::new()), loader: None, warnings: Mutex::new(Vec::new()) }),
        }
    }

    pub fn with_loader(loader: impl Fn(&TemplateRef) -> Result<String> + Send + Sync + 'static) -> Self {
        ValueHost {
            inner: Arc::new(HostInner {
                templates: RwLock::new(HashMap::new()),
                loader: Some(Box::new(loader)),
                warnings: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn add_template(&self, name: &str, source: &str) {
        self.inner.templates.write().unwrap().insert(name.to_owned(), Arc::from(source));
    }

    pub fn warnings(&self) -> Vec<String> {
        self.inner.warnings.lock().unwrap().clone()
    }

    /// Convenience: build an engine and render `template` (`render()` semantics:
    /// `true`/`false` are added to the values).
    pub fn render<'a, I: IntoIterator<Item = (&'a str, Value)>>(&self, template: &str, values: I) -> Result<String> {
        let engine = Engine::new(self.clone());
        let scope = self.make_scope(values);
        engine.render(&qweb_core::compile::template_ref(template), scope, Context::default())
    }

    /// Build the initial scope like `ir_qweb.render` (`true`, `false` added).
    pub fn make_scope<'a, I: IntoIterator<Item = (&'a str, Value)>>(&self, values: I) -> Scope {
        let mut scope: Scope = values.into_iter().map(|(k, v)| (Arc::from(k), v)).collect();
        scope.shift_remove("0");
        scope.insert(Arc::from("true"), Value::Bool(true));
        scope.insert(Arc::from("false"), Value::Bool(false));
        scope
    }
}

fn pairs_of(v: &Value) -> Result<Option<Vec<(Value, Value)>>> {
    Ok(match v {
        Value::Dict(d) => Some(d.values().cloned().collect()),
        Value::List(items) | Value::Tuple(items) => {
            let first = items.first().ok_or_else(|| Error::other("IndexError: list index out of range"))?;
            if !matches!(first, Value::List(_) | Value::Tuple(_)) {
                if items.len() != 2 {
                    return Err(Error::value(format!(
                        "dictionary update sequence element #0 has length {}; 2 is required",
                        items.len()
                    )));
                }
                Some(vec![(items[0].clone(), items[1].clone())])
            } else {
                let mut out = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let kv = item.iter_items()?;
                    if kv.len() != 2 {
                        return Err(Error::value(format!(
                            "dictionary update sequence element #{i} has length {}; 2 is required",
                            kv.len()
                        )));
                    }
                    out.push((kv[0].clone(), kv[1].clone()));
                }
                Some(out)
            }
        }
        _ => None,
    })
}

fn scope_key(k: &Value) -> Result<Arc<str>> {
    Ok(match k {
        Value::Str(s) | Value::Markup(s) => s.clone(),
        Value::Int(0) => Arc::from(SLOT_KEY),
        other => Arc::from(other.to_str()?),
    })
}

impl Host for ValueHost {
    type Value = Value;
    type Key = Arc<str>;
    type Scope = Scope;
    type Opaque = ();
    type Context = Context;
    type Iter = std::vec::IntoIter<Result<Value>>;
    type Hook = ();

    fn key(&self, name: &str) -> Arc<str> {
        Arc::from(name)
    }
    fn slot_key(&self) -> Arc<str> {
        Arc::from(SLOT_KEY)
    }
    fn none(&self) -> Value {
        Value::None
    }
    fn bool(&self, b: bool) -> Value {
        Value::Bool(b)
    }
    fn int(&self, i: i64) -> Value {
        Value::Int(i)
    }
    fn big_int(&self, digits: &str) -> Result<Value> {
        digits
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| Error::other(format!("OverflowError: integer {digits} too large")))
    }
    fn float(&self, f: f64) -> Value {
        Value::Float(f)
    }
    fn str(&self, s: &str) -> Value {
        Value::str(s)
    }
    fn markup(&self, s: &str) -> Value {
        Value::markup(s)
    }
    fn bytes(&self, b: &[u8]) -> Value {
        Value::Bytes(Arc::from(b))
    }
    fn tuple(&self, items: Vec<Value>) -> Value {
        Value::Tuple(Arc::new(items))
    }
    fn list(&self, items: Vec<Value>) -> Value {
        Value::list(items)
    }
    fn set(&self, items: Vec<Value>) -> Result<Value> {
        let mut out: Vec<Value> = Vec::new();
        for x in items {
            x.hkey()?;
            if !out.iter().any(|y| y.py_eq(&x)) {
                out.push(x);
            }
        }
        Ok(Value::Set(Arc::new(out)))
    }
    fn dict(&self, items: Vec<(Value, Value)>) -> Result<Value> {
        Value::dict_from(items)
    }
    fn slice(&self, lower: Option<Value>, upper: Option<Value>, step: Option<Value>) -> Value {
        Value::Slice(Arc::new((lower.unwrap_or(Value::None), upper.unwrap_or(Value::None), step.unwrap_or(Value::None))))
    }
    fn builtin(&self, name: &str) -> Option<Value> {
        Builtin::from_name(name).map(Value::Builtin)
    }

    fn scope_new(&self) -> Scope {
        IndexMap::new()
    }
    fn scope_copy(&self, scope: &Scope) -> Scope {
        scope.clone()
    }
    fn scope_get(&self, scope: &Scope, key: &Arc<str>) -> Option<Value> {
        scope.get(key).cloned()
    }
    fn scope_contains(&self, scope: &Scope, key: &Arc<str>) -> bool {
        scope.contains_key(key)
    }
    fn scope_set(&self, scope: &mut Scope, key: &Arc<str>, value: Value) {
        scope.insert(key.clone(), value);
    }
    fn scope_remove(&self, scope: &mut Scope, key: &Arc<str>) {
        scope.shift_remove(key);
    }
    fn scope_update(&self, scope: &mut Scope, other: &Scope) {
        for (k, v) in other {
            scope.insert(k.clone(), v.clone());
        }
    }
    fn scope_update_value(&self, scope: &mut Scope, value: &Value) -> Result<()> {
        let pairs: Vec<(Value, Value)> = match value {
            Value::Dict(d) => d.values().cloned().collect(),
            other => {
                let mut out = Vec::new();
                for item in other.iter_items()? {
                    let kv = item.iter_items()?;
                    if kv.len() != 2 {
                        return Err(Error::value("dictionary update sequence element has wrong length"));
                    }
                    out.push((kv[0].clone(), kv[1].clone()));
                }
                out
            }
        };
        for (k, v) in pairs {
            scope.insert(scope_key(&k)?, v);
        }
        Ok(())
    }
    fn scope_update_pairs(&self, scope: &mut Scope, value: &Value) -> Result<()> {
        if let Some(pairs) = pairs_of(value)? {
            for (k, v) in pairs {
                scope.insert(scope_key(&k)?, v);
            }
        }
        Ok(())
    }
    fn missing_name(&self, name: &str) -> Error {
        Error::key(value::str_repr(name))
    }

    fn get_attr(&self, obj: &Value, name: &Arc<str>) -> Result<Value> {
        ops::get_attr(obj, name)
    }
    fn get_item(&self, obj: &Value, key: &Value) -> Result<Value> {
        ops::get_item(obj, key)
    }
    fn call(&self, func: &Value, args: Vec<Value>, kwargs: Vec<(Arc<str>, Value)>) -> Result<Value> {
        if !kwargs.is_empty() {
            return Err(Error::type_error("keyword arguments are not supported by qweb-value builtins"));
        }
        match func {
            Value::Builtin(b) => ops::call_builtin(*b, args),
            Value::Method(m) => ops::call_method(&m.0, &m.1, &args),
            other => Err(Error::type_error(format!("'{}' object is not callable", other.type_name()))),
        }
    }
    fn unary(&self, op: UnOp, v: &Value) -> Result<Value> {
        ops::unary(op, v)
    }
    fn binary(&self, op: BinOp, a: &Value, b: &Value) -> Result<Value> {
        ops::binary(op, a, b)
    }
    fn compare(&self, op: CmpOp, a: &Value, b: &Value) -> Result<Value> {
        ops::compare(op, a, b)
    }
    fn contains(&self, container: &Value, item: &Value) -> Result<bool> {
        ops::contains(container, item)
    }
    fn is(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::None, Value::None) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Int(x), Value::Int(y)) => x == y && (-5..=256).contains(x),
            (Value::Str(x), Value::Str(y)) => Arc::ptr_eq(x, y),
            (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => Arc::ptr_eq(x, y),
            (Value::Dict(x), Value::Dict(y)) => Arc::ptr_eq(x, y),
            (Value::Object(x), Value::Object(y)) => Arc::ptr_eq(x, y),
            (Value::Lazy(x), Value::Lazy(y)) => Arc::ptr_eq(x, y),
            (Value::Builtin(x), Value::Builtin(y)) => x == y,
            _ => false,
        }
    }
    fn truthy(&self, v: &Value) -> Result<bool> {
        v.truthy()
    }
    fn is_none_or_false(&self, v: &Value) -> bool {
        matches!(v, Value::None | Value::Bool(false))
    }
    fn is_str(&self, v: &Value) -> bool {
        matches!(v, Value::Str(_) | Value::Markup(_))
    }
    fn merge_dict(&self, base: Option<&Value>, items: Vec<(Value, Value)>) -> Result<Value> {
        let mut pairs = match base {
            Some(Value::Dict(d)) => d.values().cloned().collect(),
            Some(other) => return Err(Error::type_error(format!("'{}' object is not a mapping", other.type_name()))),
            None => Vec::new(),
        };
        pairs.extend(items);
        Value::dict_from(pairs)
    }
    fn text_value(&self, v: &Value) -> Result<Value> {
        Ok(match v {
            Value::None | Value::Bool(false) => Value::str(""),
            Value::Str(_) | Value::Markup(_) => v.clone(),
            Value::Bytes(b) => Value::str(&String::from_utf8_lossy(b)),
            other => Value::str(&other.to_str()?),
        })
    }
    fn write_text(&self, v: &Value, out: &mut String) -> Result<()> {
        match v {
            Value::None | Value::Bool(false) => Ok(()),
            Value::Bytes(b) => {
                out.push_str(&String::from_utf8_lossy(b));
                Ok(())
            }
            other => other.write_str(out),
        }
    }
    fn write_str(&self, v: &Value, out: &mut String) -> Result<()> {
        v.write_str(out)
    }
    fn write_escaped(&self, v: &Value, out: &mut String) -> Result<()> {
        match v {
            Value::Markup(s) => out.push_str(s),
            Value::Lazy(l) => out.push_str(l.render()?),
            Value::Str(s) => qweb_core::escape::escape_into(out, s),
            other => qweb_core::escape::escape_into(out, &other.to_str()?),
        }
        Ok(())
    }

    fn iterate(&self, v: Value) -> Result<LoopSource<Self::Iter>> {
        let (items, size, mapping) = match &v {
            Value::Int(n) => ((0..*n).map(Value::Int).collect(), Some((*n).max(0) as usize), false),
            Value::Dict(d) => (
                d.values().map(|(k, v)| Value::Tuple(Arc::new(vec![k.clone(), v.clone()]))).collect(),
                Some(d.len()),
                true,
            ),
            Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::Str(_) | Value::Markup(_) | Value::Bytes(_) => {
                let items = v.iter_items()?;
                let n = items.len();
                (items, Some(n), false)
            }
            Value::Object(_) => (v.iter_items()?, None, false),
            other => return Err(Error::type_error(format!("'{}' object is not iterable", other.type_name()))),
        };
        Ok(LoopSource { iter: items.into_iter().map(Ok).collect::<Vec<_>>().into_iter(), size, mapping })
    }
    fn unpack_pair(&self, item: Value) -> Result<(Value, Value)> {
        let items = item.iter_items()?;
        if items.len() != 2 {
            return Err(Error::value(format!("too many values to unpack (expected 2, got {})", items.len())));
        }
        let mut it = items.into_iter();
        Ok((it.next().unwrap(), it.next().unwrap()))
    }
    fn pairs(&self, v: &Value) -> Result<Option<Vec<(Value, Value)>>> {
        pairs_of(v)
    }

    fn wrap_lazy(&self, lazy: Arc<Lazy<Self>>) -> Value {
        Value::Lazy(lazy)
    }
    fn as_lazy(&self, v: &Value) -> Option<Arc<Lazy<Self>>> {
        match v {
            Value::Lazy(l) => Some(l.clone()),
            _ => None,
        }
    }

    fn compile_options(&self, ctx: &Context) -> CompileOptions {
        let truthy = |k: &str| ctx.options.get(k).map(|v| v.truthy().unwrap_or(false)).unwrap_or(false);
        let mut extra = Vec::new();
        if let Some(lang) = ctx.options.get("lang") {
            extra.push(format!("lang={}", lang.repr()));
        }
        CompileOptions {
            preserve_comments: truthy("preserve_comments"),
            edit_translations: truthy("edit_translations"),
            dev_mode: false,
            nsmap: ctx.nsmap.as_ref().map(|n| (**n).clone()).unwrap_or_default(),
            extra,
        }
    }
    fn call_context(&self, ctx: &Context, options: Option<&Value>, nsmap: Option<&NsMap>) -> Result<Context> {
        let mut opts = (*ctx.options).clone();
        if let Some(Value::Dict(d)) = options {
            for (k, v) in d.values() {
                opts.insert(k.to_str()?, v.clone());
            }
        }
        Ok(Context { options: Arc::new(opts), nsmap: nsmap.cloned().or_else(|| ctx.nsmap.clone()) })
    }
    fn load(&self, template: &TemplateRef, _ctx: &Context) -> Result<Loaded<Self>> {
        let name = template.to_string();
        let source = match self.inner.templates.read().unwrap().get(&name) {
            Some(s) => s.clone(),
            None => match &self.inner.loader {
                Some(l) => Arc::from(l(template)?),
                None => return Err(Error::value(format!("template not found: {name}"))),
            },
        };
        Ok(Loaded::Source { source, viewid: None, xmlid: None })
    }
    fn empty_options(&self) -> Value {
        Value::Dict(Arc::new(IndexMap::new()))
    }
    fn warn(&self, msg: &str) {
        self.inner.warnings.lock().unwrap().push(msg.to_owned());
    }
}

/// Convert a JSON value. `{"$markup": "..."}` becomes `Markup`, `{"$tuple": [...]}` a tuple
/// and `{"$object": {...}}` an attribute object.
pub fn from_json(v: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match v {
        J::Null => Value::None,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::str(s),
        J::Array(items) => Value::list(items.iter().map(from_json).collect()),
        J::Object(map) => {
            if map.len() == 1 {
                if let Some(J::String(s)) = map.get("$markup") {
                    return Value::markup(s);
                }
                if let Some(J::Array(items)) = map.get("$tuple") {
                    return Value::Tuple(Arc::new(items.iter().map(from_json).collect()));
                }
                if let Some(J::Object(fields)) = map.get("$object") {
                    return Value::Object(Arc::new(fields.iter().map(|(k, v)| (Arc::from(k.as_str()), from_json(v))).collect()));
                }
                if let Some(J::Number(n)) = map.get("$float") {
                    return Value::Float(n.as_f64().unwrap_or(0.0));
                }
            }
            Value::Dict(Arc::new(
                map.iter().map(|(k, v)| (HKey::Str(Arc::from(k.as_str())), (Value::str(k), from_json(v)))).collect(),
            ))
        }
    }
}
