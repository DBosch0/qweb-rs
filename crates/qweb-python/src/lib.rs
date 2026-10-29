//! Python bindings (`qweb_rs._qweb_rs`): a [`Host`] implemented on the
//! CPython object protocol.
//!
//! Every value operation goes through the same C-API calls the generated
//! Python code would make (`getattr`, `getitem`, rich comparison, calls,
//! truthiness, `str()`), so template semantics are exactly Python's.
//!
//! The environment (template loading, ORM callbacks, compile hooks) is a
//! Python *bridge* object (`qweb_rs.Bridge`); `python/qweb_rs/_helpers.py`
//! holds the Python-side helpers (opaque expressions, default bridges).
//!
//! Exposed classes: `Engine`, `CompiledTemplate`, `QwebContent`, and the
//! compile-time proxies given to custom directives (`Element`, `Attrib`,
//! `Compiler`).

#![deny(missing_docs)]

use std::sync::Arc;

use pyo3::PyTypeInfo;
use pyo3::basic::CompareOp;
use pyo3::exceptions::{
    PyAssertionError, PyKeyError, PyNotImplementedError, PyRecursionError, PyRuntimeError,
    PySyntaxError, PyTypeError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyBytes, PyDict, PyFloat, PyInt, PyIterator, PyList, PySet, PyString, PyTuple,
};
use qweb_core::compile::CompileApi;
use qweb_core::escape::escape_into;
use qweb_core::expr::{BinOp, CmpOp, UnOp};
use qweb_core::host::{CompileHooks, CompileOptions, Converted, Loaded, LoopSource, TemplateRef};
use qweb_core::ir::Program;
use qweb_core::runtime::AttrVal;
use qweb_core::xml::{Document, NodeId, NsMap};
use qweb_core::{Engine, Error, ErrorKind, Host, Lazy, Result};

/// The GIL is always held while rendering: every entry point is a Python call.
#[inline(always)]
fn py() -> Python<'static> {
    unsafe { Python::assume_attached() }
}

/// A Python exception as a core error (converted back by [`to_pyerr`]).
#[inline]
fn herr(e: PyErr) -> Error {
    Error::host(e)
}

/// Python objects used by the host, resolved once.
struct Globals {
    markup: Py<PyAny>,
    builtins: Py<PyDict>,
    sized: Py<PyAny>,
    mapping: Py<PyAny>,
    compile_opaque: Py<PyAny>,
    load_bridge: Py<PyAny>,
    warn: Py<PyAny>,
    slot: Py<PyAny>,
    slice: Py<PyAny>,
    dict_type: Py<PyAny>,
    range: Py<PyAny>,
}

/// The [`Host`] of the Python binding: values are Python objects.
#[derive(Clone)]
pub(crate) struct PyHost {
    g: Arc<Globals>,
}

impl PyHost {
    fn new(py: Python<'_>) -> PyResult<Self> {
        let helpers = py.import("qweb_rs._helpers")?;
        let abc = py.import("collections.abc")?;
        let builtins = py.import("builtins")?;
        let g = Globals {
            markup: py.import("markupsafe")?.getattr("Markup")?.unbind(),
            builtins: helpers.getattr("BUILTINS")?.cast_into::<PyDict>()?.unbind(),
            sized: abc.getattr("Sized")?.unbind(),
            mapping: abc.getattr("Mapping")?.unbind(),
            compile_opaque: helpers.getattr("compile_opaque")?.unbind(),
            load_bridge: helpers.getattr("LoadBridge")?.unbind(),
            warn: helpers.getattr("warn")?.unbind(),
            slot: 0i64.into_pyobject(py)?.into_any().unbind(),
            slice: builtins.getattr("slice")?.unbind(),
            dict_type: builtins.getattr("dict")?.unbind(),
            range: builtins.getattr("range")?.unbind(),
        };
        Ok(PyHost { g: Arc::new(g) })
    }

    fn markup_of<'py>(
        &self,
        py: Python<'py>,
        v: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.g.markup.bind(py).call1((v,))
    }
}

/// Bridge flags (`bridge.flags`): which optional bridge methods are used.
const FLAG_POST_PROCESSING: u32 = 1;
const FLAG_COMPILE_HOOKS: u32 = 2;
const FLAG_WITH_CONTEXT: u32 = 4;

/// Render context: the context dict (`env.context` / render options), the
/// bridge object (template loader and callbacks) and the derived compile options.
pub(crate) struct PyCtx {
    options: Py<PyDict>,
    bridge: Py<PyAny>,
    flags: u32,
    nsmap: Option<NsMap>,
    compile: CompileOptions,
    cache_keys: Arc<Vec<String>>,
}

fn truthy_opt(d: &Bound<'_, PyDict>, key: &str) -> PyResult<bool> {
    match d.get_item(key)? {
        Some(v) => v.is_truthy(),
        None => Ok(false),
    }
}

impl PyCtx {
    fn new(
        py: Python<'_>,
        options: Py<PyDict>,
        bridge: Py<PyAny>,
        nsmap: Option<NsMap>,
        cache_keys: Arc<Vec<String>>,
    ) -> PyResult<Self> {
        let d = options.bind(py);
        let mut extra = Vec::with_capacity(cache_keys.len());
        for k in cache_keys.iter() {
            let repr = match d.get_item(k)? {
                Some(v) if v.is_truthy()? => v.repr()?.to_str()?.to_owned(),
                _ => "False".to_owned(),
            };
            extra.push(repr);
        }
        let compile = CompileOptions {
            preserve_comments: truthy_opt(d, "preserve_comments")?,
            edit_translations: truthy_opt(d, "edit_translations")?,
            dev_mode: truthy_opt(d, "dev_mode")?,
            nsmap: nsmap.as_ref().map(|n| (**n).clone()).unwrap_or_default(),
            extra,
        };
        let flags = match bridge.bind(py).getattr(pyo3::intern!(py, "flags")) {
            Ok(f) => f.extract::<u32>().unwrap_or(0),
            Err(_) => 0,
        };
        Ok(PyCtx {
            options,
            bridge,
            flags,
            nsmap,
            compile,
            cache_keys,
        })
    }

    fn bridge<'py>(&self, py: Python<'py>) -> &Bound<'py, PyAny> {
        self.bridge.bind(py)
    }
}

/// Iterator over a Python iterable (fast paths for exact lists and tuples).
pub(crate) enum PyIter {
    /// Exact list and the next index (re-reads the length like `list_iterator`).
    List(Py<PyList>, usize),
    /// Exact tuple and the next index.
    Tuple(Py<PyTuple>, usize),
    /// Any other iterator.
    Generic(Py<PyIterator>),
}

impl Iterator for PyIter {
    type Item = Result<Py<PyAny>>;
    fn next(&mut self) -> Option<Self::Item> {
        let py = py();
        match self {
            PyIter::List(l, i) => {
                let l = l.bind(py);
                if *i < l.len() {
                    let item = l.get_item(*i).map(Bound::unbind).map_err(herr);
                    *i += 1;
                    Some(item)
                } else {
                    None
                }
            }
            PyIter::Tuple(t, i) => {
                let t = t.bind(py);
                if *i < t.len() {
                    let item = t.get_item(*i).map(Bound::unbind).map_err(herr);
                    *i += 1;
                    Some(item)
                } else {
                    None
                }
            }
            PyIter::Generic(it) => {
                let mut it = it.bind(py).clone();
                it.next().map(|r| r.map(Bound::unbind).map_err(herr))
            }
        }
    }
}

fn template_obj<'py>(py: Python<'py>, t: &TemplateRef) -> PyResult<Bound<'py, PyAny>> {
    Ok(match t {
        TemplateRef::Id(i) => i.into_pyobject(py)?.into_any(),
        TemplateRef::Name(n) => PyString::new(py, n).into_any(),
    })
}

/// Exact int, float or bool: `str()` is the text, no `__html__`/buffer protocol.
#[inline]
fn is_plain_scalar(b: &Bound<'_, PyAny>) -> bool {
    b.is_exact_instance_of::<PyInt>()
        || b.is_exact_instance_of::<PyFloat>()
        || b.is_exact_instance_of::<PyBool>()
}

/// Append a Python str (`to_cow` copies only for non-UTF-8 surrogates).
fn push_str(out: &mut String, s: &Bound<'_, PyString>) -> Result<()> {
    out.push_str(&s.to_cow().map_err(herr)?);
    Ok(())
}

impl Host for PyHost {
    type Value = Py<PyAny>;
    type Key = Py<PyAny>;
    type Scope = Py<PyDict>;
    type Opaque = Py<PyAny>;
    type Context = Arc<PyCtx>;
    type Iter = PyIter;
    type Hook = Py<PyAny>;

    fn key(&self, name: &str) -> Py<PyAny> {
        PyString::intern(py(), name).into_any().unbind()
    }
    fn slot_key(&self) -> Py<PyAny> {
        self.g.slot.clone()
    }
    fn none(&self) -> Py<PyAny> {
        py().None()
    }
    fn bool(&self, b: bool) -> Py<PyAny> {
        PyBool::new(py(), b).to_owned().into_any().unbind()
    }
    fn int(&self, i: i64) -> Py<PyAny> {
        i.into_pyobject(py()).unwrap().into_any().unbind()
    }
    fn big_int(&self, digits: &str) -> Result<Py<PyAny>> {
        let py = py();
        let int = py.get_type::<PyInt>();
        int.call1((digits,)).map(Bound::unbind).map_err(herr)
    }
    fn float(&self, f: f64) -> Py<PyAny> {
        PyFloat::new(py(), f).into_any().unbind()
    }
    fn str(&self, s: &str) -> Py<PyAny> {
        PyString::new(py(), s).into_any().unbind()
    }
    fn markup(&self, s: &str) -> Py<PyAny> {
        let py = py();
        self.g
            .markup
            .bind(py)
            .call1((s,))
            .expect("Markup()")
            .unbind()
    }
    fn bytes(&self, b: &[u8]) -> Py<PyAny> {
        PyBytes::new(py(), b).into_any().unbind()
    }
    fn tuple(&self, items: Vec<Py<PyAny>>) -> Py<PyAny> {
        PyTuple::new(py(), items).unwrap().into_any().unbind()
    }
    fn list(&self, items: Vec<Py<PyAny>>) -> Py<PyAny> {
        PyList::new(py(), items).unwrap().into_any().unbind()
    }
    fn set(&self, items: Vec<Py<PyAny>>) -> Result<Py<PyAny>> {
        PySet::new(py(), items)
            .map(|s| s.into_any().unbind())
            .map_err(herr)
    }
    fn dict(&self, items: Vec<(Py<PyAny>, Py<PyAny>)>) -> Result<Py<PyAny>> {
        let py = py();
        let d = PyDict::new(py);
        for (k, v) in items {
            d.set_item(k, v).map_err(herr)?;
        }
        Ok(d.into_any().unbind())
    }
    fn slice(
        &self,
        lower: Option<Py<PyAny>>,
        upper: Option<Py<PyAny>>,
        step: Option<Py<PyAny>>,
    ) -> Py<PyAny> {
        let py = py();
        let n = || py.None();
        self.g
            .slice
            .bind(py)
            .call1((
                lower.unwrap_or_else(n),
                upper.unwrap_or_else(n),
                step.unwrap_or_else(n),
            ))
            .unwrap()
            .unbind()
    }
    fn builtin(&self, name: &str) -> Option<Py<PyAny>> {
        self.g
            .builtins
            .bind(py())
            .get_item(name)
            .ok()
            .flatten()
            .map(Bound::unbind)
    }
    fn compile_opaque(&self, src: &str, raise_on_missing: bool) -> Result<Option<Py<PyAny>>> {
        let py = py();
        self.g
            .compile_opaque
            .bind(py)
            .call1((src, raise_on_missing))
            .map(|f| Some(f.unbind()))
            .map_err(herr)
    }
    fn eval_opaque(&self, code: &Py<PyAny>, scope: &Py<PyDict>) -> Result<Py<PyAny>> {
        let py = py();
        code.bind(py)
            .call1((scope.bind(py),))
            .map(Bound::unbind)
            .map_err(herr)
    }

    // ----- scope -----------------------------------------------------------
    fn scope_new(&self) -> Py<PyDict> {
        PyDict::new(py()).unbind()
    }
    fn scope_copy(&self, scope: &Py<PyDict>) -> Py<PyDict> {
        scope.bind(py()).copy().expect("dict.copy").unbind()
    }
    fn scope_get(&self, scope: &Py<PyDict>, key: &Py<PyAny>) -> Option<Py<PyAny>> {
        let py = py();
        scope
            .bind(py)
            .get_item(key.bind(py))
            .ok()
            .flatten()
            .map(Bound::unbind)
    }
    fn scope_contains(&self, scope: &Py<PyDict>, key: &Py<PyAny>) -> bool {
        let py = py();
        scope.bind(py).contains(key.bind(py)).unwrap_or(false)
    }
    fn scope_set(&self, scope: &mut Py<PyDict>, key: &Py<PyAny>, value: Py<PyAny>) {
        let py = py();
        let _ = scope.bind(py).set_item(key.bind(py), value);
    }
    fn scope_remove(&self, scope: &mut Py<PyDict>, key: &Py<PyAny>) {
        let py = py();
        let _ = scope.bind(py).del_item(key.bind(py));
    }
    fn scope_update(&self, scope: &mut Py<PyDict>, other: &Py<PyDict>) {
        let py = py();
        let _ = scope.bind(py).update(other.bind(py).as_mapping());
    }
    fn scope_update_value(&self, scope: &mut Py<PyDict>, value: &Py<PyAny>) -> Result<()> {
        let py = py();
        scope
            .bind(py)
            .call_method1(pyo3::intern!(py, "update"), (value.bind(py),))
            .map(|_| ())
            .map_err(herr)
    }
    fn scope_update_pairs(&self, scope: &mut Py<PyDict>, value: &Py<PyAny>) -> Result<()> {
        let py = py();
        let d = scope.bind(py);
        let v = value.bind(py);
        (|| -> PyResult<()> {
            let update = pyo3::intern!(py, "update");
            if v.is_instance_of::<PyDict>() {
                d.call_method1(update, (v,))?;
            } else if v.is_instance_of::<PyList>() || v.is_instance_of::<PyTuple>() {
                let first = v.get_item(0)?;
                if !(first.is_instance_of::<PyList>() || first.is_instance_of::<PyTuple>()) {
                    d.call_method1(update, (PyList::new(py, [v])?,))?;
                } else {
                    let as_dict = self.g.dict_type.bind(py).call1((v,))?;
                    d.call_method1(update, (as_dict,))?;
                }
            }
            Ok(())
        })()
        .map_err(herr)
    }
    fn missing_name(&self, name: &str) -> Error {
        herr(PyKeyError::new_err(name.to_owned()))
    }

    // ----- object protocol -------------------------------------------------
    fn get_attr(&self, obj: &Py<PyAny>, name: &Py<PyAny>) -> Result<Py<PyAny>> {
        let py = py();
        obj.bind(py)
            .getattr(name.bind(py).cast::<PyString>().unwrap())
            .map(Bound::unbind)
            .map_err(herr)
    }
    fn get_item(&self, obj: &Py<PyAny>, key: &Py<PyAny>) -> Result<Py<PyAny>> {
        let py = py();
        obj.bind(py)
            .get_item(key.bind(py))
            .map(Bound::unbind)
            .map_err(herr)
    }
    fn call(
        &self,
        func: &Py<PyAny>,
        args: Vec<Py<PyAny>>,
        kwargs: Vec<(Py<PyAny>, Py<PyAny>)>,
    ) -> Result<Py<PyAny>> {
        let py = py();
        let f = func.bind(py);
        let args = PyTuple::new(py, args).map_err(herr)?;
        let res = if kwargs.is_empty() {
            f.call1(args)
        } else {
            let kw = PyDict::new(py);
            for (k, v) in kwargs {
                kw.set_item(k, v).map_err(herr)?;
            }
            f.call(args, Some(&kw))
        };
        res.map(Bound::unbind).map_err(herr)
    }
    fn unary(&self, op: UnOp, v: &Py<PyAny>) -> Result<Py<PyAny>> {
        let py = py();
        let v = v.bind(py);
        match op {
            UnOp::Neg => v.neg(),
            UnOp::Pos => v.pos(),
            UnOp::Invert => v.bitnot(),
        }
        .map(Bound::unbind)
        .map_err(herr)
    }
    fn binary(&self, op: BinOp, a: &Py<PyAny>, b: &Py<PyAny>) -> Result<Py<PyAny>> {
        let py = py();
        let (a, b) = (a.bind(py), b.bind(py));
        match op {
            BinOp::Add => a.add(b),
            BinOp::Sub => a.sub(b),
            BinOp::Mul => a.mul(b),
            BinOp::MatMul => a.matmul(b),
            BinOp::Div => a.div(b),
            BinOp::FloorDiv => a.floor_div(b),
            BinOp::Mod => a.rem(b),
            BinOp::Pow => a.pow(b, py.None()),
            BinOp::LShift => a.lshift(b),
            BinOp::RShift => a.rshift(b),
            BinOp::BitAnd => a.bitand(b),
            BinOp::BitOr => a.bitor(b),
            BinOp::BitXor => a.bitxor(b),
        }
        .map(Bound::unbind)
        .map_err(herr)
    }
    fn compare(&self, op: CmpOp, a: &Py<PyAny>, b: &Py<PyAny>) -> Result<Py<PyAny>> {
        let py = py();
        let op = match op {
            CmpOp::Lt => CompareOp::Lt,
            CmpOp::Le => CompareOp::Le,
            CmpOp::Eq => CompareOp::Eq,
            CmpOp::Ne => CompareOp::Ne,
            CmpOp::Gt => CompareOp::Gt,
            CmpOp::Ge => CompareOp::Ge,
            _ => unreachable!(),
        };
        a.bind(py)
            .rich_compare(b.bind(py), op)
            .map(Bound::unbind)
            .map_err(herr)
    }
    fn contains(&self, container: &Py<PyAny>, item: &Py<PyAny>) -> Result<bool> {
        let py = py();
        container.bind(py).contains(item.bind(py)).map_err(herr)
    }
    fn is(&self, a: &Py<PyAny>, b: &Py<PyAny>) -> bool {
        a.as_ptr() == b.as_ptr()
    }
    fn truthy(&self, v: &Py<PyAny>) -> Result<bool> {
        v.bind(py()).is_truthy().map_err(herr)
    }
    fn is_none_or_false(&self, v: &Py<PyAny>) -> bool {
        let py = py();
        let b = v.bind(py);
        b.is_none() || b.is(PyBool::new(py, false))
    }
    fn is_str(&self, v: &Py<PyAny>) -> bool {
        v.bind(py()).is_instance_of::<PyString>()
    }
    fn merge_dict(
        &self,
        base: Option<&Py<PyAny>>,
        items: Vec<(Py<PyAny>, Py<PyAny>)>,
    ) -> Result<Py<PyAny>> {
        let py = py();
        let d = PyDict::new(py);
        if let Some(b) = base {
            d.call_method1(pyo3::intern!(py, "update"), (b.bind(py),))
                .map_err(herr)?;
        }
        for (k, v) in items {
            d.set_item(k, v).map_err(herr)?;
        }
        Ok(d.into_any().unbind())
    }

    // ----- text --------------------------------------------------------------
    fn text_value(&self, v: &Py<PyAny>) -> Result<Py<PyAny>> {
        let py = py();
        let b = v.bind(py);
        if self.is_none_or_false(v) {
            return Ok(PyString::new(py, "").into_any().unbind());
        }
        if b.is_instance_of::<PyString>() {
            return Ok(v.clone());
        }
        if is_plain_scalar(b) {
            return b.str().map(|s| s.into_any().unbind()).map_err(herr);
        }
        let decode = pyo3::intern!(py, "decode");
        if let Ok(bytes) = b.cast::<PyBytes>() {
            return bytes.call_method0(decode).map(Bound::unbind).map_err(herr);
        }
        if b.hasattr(pyo3::intern!(py, "__buffer__")).unwrap_or(false) {
            let bytes = py.get_type::<PyBytes>().call1((b,)).map_err(herr)?;
            return bytes.call_method0(decode).map(Bound::unbind).map_err(herr);
        }
        b.str().map(|s| s.into_any().unbind()).map_err(herr)
    }
    fn write_text(&self, v: &Py<PyAny>, out: &mut String) -> Result<()> {
        let py = py();
        let b = v.bind(py);
        if let Ok(s) = b.cast::<PyString>() {
            return push_str(out, s);
        }
        if self.is_none_or_false(v) {
            return Ok(());
        }
        if is_plain_scalar(b) {
            return push_str(out, &b.str().map_err(herr)?);
        }
        let t = self.text_value(v)?;
        let t = t.bind(py);
        match t.cast::<PyString>() {
            Ok(s) => push_str(out, s),
            Err(_) => push_str(out, &t.str().map_err(herr)?),
        }
    }
    fn write_str(&self, v: &Py<PyAny>, out: &mut String) -> Result<()> {
        let py = py();
        let b = v.bind(py);
        if let Ok(s) = b.cast_exact::<PyString>() {
            return push_str(out, s);
        }
        push_str(out, &b.str().map_err(herr)?)
    }
    fn write_escaped(&self, v: &Py<PyAny>, out: &mut String) -> Result<()> {
        let py = py();
        let b = v.bind(py);
        if let Ok(s) = b.cast_exact::<PyString>() {
            escape_into(out, &s.to_cow().map_err(herr)?);
            return Ok(());
        }
        if is_plain_scalar(b) {
            // str() of int/float/bool never contains characters to escape
            return push_str(out, &b.str().map_err(herr)?);
        }
        // markupsafe.escape: objects with __html__ are trusted.
        if let Ok(html) = b.getattr(pyo3::intern!(py, "__html__")) {
            let r = html.call0().map_err(herr)?;
            return push_str(out, &r.str().map_err(herr)?);
        }
        let s = b.str().map_err(herr)?;
        escape_into(out, &s.to_cow().map_err(herr)?);
        Ok(())
    }

    // ----- foreach / attributes ----------------------------------------------
    fn iterate(&self, v: Py<PyAny>) -> Result<LoopSource<PyIter>> {
        let py = py();
        let b = v.bind(py);
        if let Ok(l) = b.cast_exact::<PyList>() {
            let n = l.len();
            return Ok(LoopSource {
                iter: PyIter::List(l.clone().unbind(), 0),
                size: Some(n),
                mapping: false,
            });
        }
        if let Ok(t) = b.cast_exact::<PyTuple>() {
            let n = t.len();
            return Ok(LoopSource {
                iter: PyIter::Tuple(t.clone().unbind(), 0),
                size: Some(n),
                mapping: false,
            });
        }
        (|| -> PyResult<LoopSource<PyIter>> {
            let mut iterable = b.clone();
            let mut size = None;
            if b.is_instance(self.g.sized.bind(py))? {
                size = Some(b.len()?);
            } else if b.is_exact_instance_of::<PyInt>() {
                let n: i64 = b.extract()?;
                size = Some(n.max(0) as usize);
                iterable = self.g.range.bind(py).call1((n,))?;
            }
            let mut mapping = false;
            if b.is_instance(self.g.mapping.bind(py))? {
                iterable = b.call_method0(pyo3::intern!(py, "items"))?;
                mapping = true;
            }
            Ok(LoopSource {
                iter: PyIter::Generic(iterable.try_iter()?.unbind()),
                size,
                mapping,
            })
        })()
        .map_err(herr)
    }
    fn unpack_pair(&self, item: Py<PyAny>) -> Result<(Py<PyAny>, Py<PyAny>)> {
        let py = py();
        let b = item.bind(py);
        if let Ok(t) = b.cast_exact::<PyTuple>()
            && t.len() == 2
        {
            return Ok((
                t.get_item(0).map_err(herr)?.unbind(),
                t.get_item(1).map_err(herr)?.unbind(),
            ));
        }
        let items: Vec<Bound<PyAny>> = b
            .try_iter()
            .map_err(herr)?
            .collect::<PyResult<_>>()
            .map_err(herr)?;
        match items.len() {
            2 => {
                let mut it = items.into_iter();
                Ok((it.next().unwrap().unbind(), it.next().unwrap().unbind()))
            }
            n if n < 2 => Err(herr(PyValueError::new_err(format!(
                "not enough values to unpack (expected 2, got {n})"
            )))),
            n => Err(herr(PyValueError::new_err(format!(
                "too many values to unpack (expected 2, got {n})"
            )))),
        }
    }
    fn pairs(&self, v: &Py<PyAny>) -> Result<Option<Vec<(Py<PyAny>, Py<PyAny>)>>> {
        let py = py();
        let b = v.bind(py);
        let is_seq =
            |x: &Bound<PyAny>| x.is_instance_of::<PyList>() || x.is_instance_of::<PyTuple>();
        if !(b.is_instance_of::<PyDict>() || is_seq(b)) {
            return Ok(None);
        }
        let mut d = PyDict::new(py).unbind();
        self.scope_update_pairs(&mut d, v)?;
        let d = d.bind(py);
        Ok(Some(
            d.iter().map(|(k, v)| (k.unbind(), v.unbind())).collect(),
        ))
    }

    // ----- lazy content --------------------------------------------------------
    fn wrap_lazy(&self, lazy: Arc<Lazy<Self>>) -> Py<PyAny> {
        Py::new(py(), QwebContent { lazy }).unwrap().into_any()
    }
    fn as_lazy(&self, v: &Py<PyAny>) -> Option<Arc<Lazy<Self>>> {
        let py = py();
        v.bind(py)
            .cast_exact::<QwebContent>()
            .ok()
            .map(|c| c.get().lazy.clone())
    }

    // ----- context / loading ---------------------------------------------------
    fn compile_options(&self, ctx: &Arc<PyCtx>) -> CompileOptions {
        ctx.compile.clone()
    }
    fn call_context(
        &self,
        ctx: &Arc<PyCtx>,
        options: Option<&Py<PyAny>>,
        nsmap: Option<&NsMap>,
    ) -> Result<Arc<PyCtx>> {
        let py = py();
        let d = ctx.options.bind(py).copy().map_err(herr)?;
        if let Some(o) = options {
            let o = o.bind(py);
            // `with_context(**t_call_options)`
            let o = o
                .cast::<PyDict>()
                .map_err(|e| herr(PyTypeError::new_err(e.to_string())))?;
            d.update(o.as_mapping()).map_err(herr)?;
        }
        if let Some(ns) = nsmap {
            // `t_call_options.update(nsmap={...})`
            let nd = PyDict::new(py);
            for (p, u) in ns.iter() {
                nd.set_item(p.as_deref(), u).map_err(herr)?;
            }
            d.set_item("nsmap", nd).map_err(herr)?;
        }
        let bridge = if ctx.flags & FLAG_WITH_CONTEXT != 0 {
            ctx.bridge(py)
                .call_method1(pyo3::intern!(py, "with_context"), (&d,))
                .map_err(herr)?
                .unbind()
        } else {
            ctx.bridge.clone_ref(py)
        };
        let nsmap = nsmap.cloned().or_else(|| ctx.nsmap.clone());
        let new =
            PyCtx::new(py, d.unbind(), bridge, nsmap, ctx.cache_keys.clone()).map_err(herr)?;
        Ok(Arc::new(new))
    }
    fn load(&self, template: &TemplateRef, ctx: &Arc<PyCtx>) -> Result<Loaded<Self>> {
        let py = py();
        let name = template_obj(py, template).map_err(herr)?;
        let res = ctx
            .bridge(py)
            .call_method1(pyo3::intern!(py, "load"), (name,))
            .map_err(herr)?;
        loaded_from_py(&res).map_err(herr)
    }
    fn empty_options(&self) -> Py<PyAny> {
        PyDict::new(py()).into_any().unbind()
    }
    fn has_groups(&self, ctx: &Arc<PyCtx>, groups: &str) -> Result<bool> {
        let py = py();
        let r = ctx
            .bridge(py)
            .call_method1(pyo3::intern!(py, "has_groups"), (groups,))
            .map_err(herr)?;
        r.is_truthy().map_err(herr)
    }
    fn get_field(
        &self,
        ctx: &Arc<PyCtx>,
        record: &Py<PyAny>,
        field: &str,
        expression: &str,
        tag: &str,
        options: Py<PyAny>,
        scope: &Py<PyDict>,
    ) -> Result<Converted<Py<PyAny>>> {
        let py = py();
        let r = ctx
            .bridge(py)
            .call_method1(
                pyo3::intern!(py, "get_field"),
                (record, field, expression, tag, options, scope),
            )
            .map_err(herr)?;
        converted_from_py(&r).map_err(herr)
    }
    fn get_widget(
        &self,
        ctx: &Arc<PyCtx>,
        value: &Py<PyAny>,
        expression: &str,
        tag: &str,
        options: Py<PyAny>,
        scope: &Py<PyDict>,
    ) -> Result<Converted<Py<PyAny>>> {
        let py = py();
        let r = ctx
            .bridge(py)
            .call_method1(
                pyo3::intern!(py, "get_widget"),
                (value, expression, tag, options, scope),
            )
            .map_err(herr)?;
        converted_from_py(&r).map_err(herr)
    }
    fn get_asset_nodes(
        &self,
        ctx: &Arc<PyCtx>,
        bundle: &str,
        css: bool,
        js: bool,
        binary: bool,
        autoprefix: bool,
        debug: Option<&Py<PyAny>>,
        defer_load: bool,
        lazy_load: bool,
        media: Option<&str>,
    ) -> Result<Vec<(String, Vec<(Py<PyAny>, Py<PyAny>)>)>> {
        let py = py();
        (|| -> PyResult<_> {
            let kw = PyDict::new(py);
            kw.set_item("css", css)?;
            kw.set_item("js", js)?;
            kw.set_item("binary", binary)?;
            kw.set_item("debug", debug)?;
            kw.set_item("defer_load", defer_load)?;
            kw.set_item("lazy_load", lazy_load)?;
            match media {
                Some(m) => kw.set_item("media", m)?,
                None => kw.set_item("media", false)?,
            }
            kw.set_item("autoprefix", autoprefix)?;
            let nodes = ctx.bridge(py).call_method(
                pyo3::intern!(py, "get_asset_nodes"),
                (bundle,),
                Some(&kw),
            )?;
            let mut out = Vec::new();
            for node in nodes.try_iter()? {
                let node = node?;
                let tag: String = node.get_item(0)?.str()?.to_cow()?.into_owned();
                let attrs = node.get_item(1)?;
                out.push((tag, mapping_items(&attrs)?));
            }
            Ok(out)
        })()
        .map_err(herr)
    }
    fn warn(&self, msg: &str) {
        let py = py();
        let _ = self.g.warn.bind(py).call1((msg,));
    }

    // ----- hooks -----------------------------------------------------------------
    fn compile_hooks(&self, ctx: &Arc<PyCtx>) -> Result<CompileHooks> {
        if ctx.flags & FLAG_COMPILE_HOOKS == 0 {
            return Ok(CompileHooks::default());
        }
        let py = py();
        (|| -> PyResult<CompileHooks> {
            let r = ctx
                .bridge(py)
                .call_method0(pyo3::intern!(py, "compile_hooks"))?;
            if r.is_none() {
                return Ok(CompileHooks::default());
            }
            let flag = |k: &str| -> PyResult<bool> {
                match r.get_item(k) {
                    Ok(v) => v.is_truthy(),
                    Err(_) => Ok(false),
                }
            };
            let order = match r.get_item("order") {
                Ok(o) if !o.is_none() => Some(
                    o.try_iter()?
                        .map(|n| Ok(n?.str()?.to_cow()?.into_owned()))
                        .collect::<PyResult<Vec<String>>>()?,
                ),
                _ => None,
            };
            Ok(CompileHooks {
                order,
                check_directive: flag("check_directive")?,
                check_expr: flag("check_expr")?,
                pre_processing: flag("pre_processing")?,
                post_processing: flag("post_processing")?,
            })
        })()
        .map_err(herr)
    }
    fn has_directive(&self, ctx: &Arc<PyCtx>, name: &str) -> bool {
        if ctx.flags & FLAG_COMPILE_HOOKS == 0 {
            return false;
        }
        let py = py();
        ctx.bridge(py)
            .call_method1(pyo3::intern!(py, "has_directive"), (name,))
            .and_then(|r| r.is_truthy())
            .unwrap_or(false)
    }
    fn compile_directive(
        &self,
        ctx: &Arc<PyCtx>,
        name: &str,
        api: &mut dyn CompileApi<Self>,
    ) -> Result<()> {
        let py = py();
        with_api(api, |shared, node| {
            let el = Py::new(
                py,
                PyElement {
                    api: shared.clone(),
                    id: node,
                },
            )?;
            let compiler = Py::new(py, PyCompiler { api: shared })?;
            ctx.bridge(py)
                .call_method1(pyo3::intern!(py, "compile_directive"), (name, el, compiler))?;
            Ok(())
        })
    }
    fn check_directive(
        &self,
        ctx: &Arc<PyCtx>,
        name: &str,
        api: &mut dyn CompileApi<Self>,
    ) -> Result<()> {
        let py = py();
        with_api(api, |shared, node| {
            let el = Py::new(
                py,
                PyElement {
                    api: shared,
                    id: node,
                },
            )?;
            ctx.bridge(py)
                .call_method1(pyo3::intern!(py, "check_directive"), (name, el))?;
            Ok(())
        })
    }
    fn check_expr(&self, ctx: &Arc<PyCtx>, expr: &str) -> Result<()> {
        let py = py();
        ctx.bridge(py)
            .call_method1(pyo3::intern!(py, "check_expr"), (expr,))
            .map(|_| ())
            .map_err(herr)
    }
    fn pre_processing_att(
        &self,
        ctx: &Arc<PyCtx>,
        attrs: &mut Vec<(String, String)>,
    ) -> Result<()> {
        let py = py();
        (|| -> PyResult<()> {
            let d = PyDict::new(py);
            for (k, v) in attrs.iter() {
                d.set_item(k, v)?;
            }
            ctx.bridge(py)
                .call_method1(pyo3::intern!(py, "pre_processing_att"), (&d,))?;
            attrs.clear();
            for (k, v) in d.iter() {
                attrs.push((
                    k.str()?.to_cow()?.into_owned(),
                    v.str()?.to_cow()?.into_owned(),
                ));
            }
            Ok(())
        })()
        .map_err(herr)
    }
    fn post_processing_static(
        &self,
        ctx: &Arc<PyCtx>,
        tag: &str,
        attrs: Vec<(String, String)>,
    ) -> Result<Vec<(String, String)>> {
        let py = py();
        (|| -> PyResult<Vec<(String, String)>> {
            let d = PyDict::new(py);
            for (k, v) in attrs.iter() {
                d.set_item(k, v)?;
            }
            d.set_item("__is_static_node", true)?;
            let r = ctx
                .bridge(py)
                .call_method1(pyo3::intern!(py, "post_processing_att"), (tag, &d))?;
            let mut out = Vec::new();
            for (k, v) in mapping_items(&r)? {
                let (k, v) = (k.bind(py), v.bind(py));
                if v.is_truthy()? || v.is_instance_of::<PyString>() {
                    out.push((
                        k.str()?.to_cow()?.into_owned(),
                        v.str()?.to_cow()?.into_owned(),
                    ));
                }
            }
            Ok(out)
        })()
        .map_err(herr)
    }
    fn custom_post_processing(&self, ctx: &Arc<PyCtx>) -> bool {
        ctx.flags & FLAG_POST_PROCESSING != 0
    }
    fn post_processing_att(
        &self,
        ctx: &Arc<PyCtx>,
        tag: &str,
        attrs: Vec<(Arc<str>, AttrVal<Self>)>,
    ) -> Result<Vec<(Py<PyAny>, Py<PyAny>)>> {
        let py = py();
        (|| -> PyResult<_> {
            let d = PyDict::new(py);
            for (k, v) in attrs {
                match v {
                    AttrVal::Static(s) => d.set_item(&*k, &*s)?,
                    AttrVal::Text(s) => d.set_item(&*k, s)?,
                    AttrVal::Value(v) => d.set_item(&*k, v)?,
                }
            }
            let r = ctx
                .bridge(py)
                .call_method1(pyo3::intern!(py, "post_processing_att"), (tag, &d))?;
            mapping_items(&r)
        })()
        .map_err(herr)
    }
    fn run_hook(
        &self,
        ctx: &Arc<PyCtx>,
        hook: &Py<PyAny>,
        scope: &Py<PyDict>,
        out: &mut String,
    ) -> Result<()> {
        let py = py();
        let r = ctx
            .bridge(py)
            .call_method1(pyo3::intern!(py, "run_hook"), (hook, scope))
            .map_err(herr)?;
        if let Ok(c) = r.cast_exact::<QwebContent>() {
            out.push_str(c.get().lazy.render()?);
            return Ok(());
        }
        self.write_text(&r.unbind(), out)
    }
}

/// `dict.items()` of a mapping, or the pairs of a sequence of pairs.
fn mapping_items(m: &Bound<'_, PyAny>) -> PyResult<Vec<(Py<PyAny>, Py<PyAny>)>> {
    if let Ok(d) = m.cast::<PyDict>() {
        return Ok(d.iter().map(|(k, v)| (k.unbind(), v.unbind())).collect());
    }
    let items = if m.hasattr("items")? {
        m.call_method0("items")?
    } else {
        m.clone()
    };
    let mut out = Vec::new();
    for item in items.try_iter()? {
        let item = item?;
        out.push((item.get_item(0)?.unbind(), item.get_item(1)?.unbind()));
    }
    Ok(out)
}

/// `(attrs, content, force_display)` returned by `_get_field` / `_get_widget`.
fn converted_from_py(r: &Bound<'_, PyAny>) -> PyResult<Converted<Py<PyAny>>> {
    let attrs = r.get_item(0)?;
    let attrs = if attrs.is_none() {
        Vec::new()
    } else {
        mapping_items(&attrs)?
    };
    Ok(Converted {
        attrs,
        content: r.get_item(1)?.unbind(),
        force_display: r.get_item(2)?.is_truthy()?,
    })
}

/// Result of `bridge.load(ref)`: a compiled template, a source string or a
/// `(source, viewid, xmlid)` tuple.
fn loaded_from_py(r: &Bound<'_, PyAny>) -> PyResult<Loaded<PyHost>> {
    if let Ok(t) = r.cast::<CompiledTemplate>() {
        return Ok(Loaded::Program(t.get().program.clone()));
    }
    if let Ok(s) = r.cast::<PyString>() {
        return Ok(Loaded::source(&*s.to_cow()?));
    }
    if let Ok(t) = r.cast::<PyTuple>() {
        let source: String = t.get_item(0)?.str()?.to_cow()?.into_owned();
        let viewid = t.get_item(1)?;
        let xmlid = t.get_item(2)?;
        return Ok(Loaded::Source {
            source: Arc::from(source),
            viewid: if viewid.is_none() {
                None
            } else {
                Some(viewid.unbind())
            },
            xmlid: if xmlid.is_none() {
                None
            } else {
                Some(xmlid.str()?.to_cow()?.into_owned())
            },
        });
    }
    Err(PyTypeError::new_err(
        "load() must return a str, a (source, viewid, xmlid) tuple or a CompiledTemplate",
    ))
}

// ---------------------------------------------------------------------------
// Compile-time proxies for custom directives
// ---------------------------------------------------------------------------

/// Pointer to the compiler API, valid only during a hook call.
///
/// The pointer is taken out while a proxy method uses it, so a re-entrant use
/// (a nested directive handler calling a proxy of an outer one while the
/// outer `compile_node` runs) is an error instead of a second `&mut`.
struct ApiCell {
    ptr: std::cell::Cell<Option<*mut (dyn CompileApi<PyHost> + 'static)>>,
    busy: std::cell::Cell<bool>,
}

type SharedApi = std::rc::Rc<ApiCell>;

/// Run `f` with Python proxies over `api`; the proxies are invalidated after.
fn with_api(
    api: &mut dyn CompileApi<PyHost>,
    f: impl FnOnce(SharedApi, NodeId) -> PyResult<()>,
) -> Result<()> {
    let node = api.node();
    let ptr: *mut (dyn CompileApi<PyHost> + '_) = api;
    // SAFETY: the pointer is cleared before `api` goes out of scope, and every
    // use checks it is still set (proxies kept by Python code become inert).
    let ptr: *mut (dyn CompileApi<PyHost> + 'static) = unsafe { std::mem::transmute(ptr) };
    let shared = std::rc::Rc::new(ApiCell {
        ptr: std::cell::Cell::new(Some(ptr)),
        busy: std::cell::Cell::new(false),
    });
    let res = f(shared.clone(), node);
    shared.ptr.set(None);
    res.map_err(herr)
}

/// Run `f` on the compiler API of a proxy.
fn api_of<R>(
    cell: &ApiCell,
    f: impl FnOnce(&mut dyn CompileApi<PyHost>) -> PyResult<R>,
) -> PyResult<R> {
    let Some(p) = cell.ptr.take() else {
        return Err(PyRuntimeError::new_err(if cell.busy.get() {
            "the compiler of this directive is busy (re-entrant use)"
        } else {
            "the compiler is only available during the directive compilation"
        }));
    };
    cell.busy.set(true);
    // SAFETY: see `with_api`; the GIL serializes access, and the pointer is
    // out of the cell while `f` runs, so this is the only `&mut`.
    let res = f(unsafe { &mut *p });
    cell.busy.set(false);
    // `with_api` only expires the cell after its handler returned, so it is
    // still valid here.
    cell.ptr.set(Some(p));
    res
}

fn py_to_text(v: &Bound<'_, PyAny>) -> PyResult<String> {
    if v.is_none() || v.is(PyBool::new(v.py(), false)) {
        return Ok(String::new());
    }
    Ok(v.str()?.to_cow()?.into_owned())
}

/// Element being compiled, with a subset of the lxml element API.
#[pyclass(name = "Element", module = "qweb_rs", unsendable)]
pub struct PyElement {
    api: SharedApi,
    id: NodeId,
}

impl PyElement {
    fn with<R>(&self, f: impl FnOnce(&Document, NodeId) -> R) -> PyResult<R> {
        api_of(&self.api, |a| Ok(f(a.doc(), self.id)))
    }
    fn with_mut<R>(&self, f: impl FnOnce(&mut Document, NodeId) -> R) -> PyResult<R> {
        api_of(&self.api, |a| Ok(f(a.doc_mut(), self.id)))
    }
    fn wrap_id(&self, id: NodeId) -> PyElement {
        PyElement {
            api: self.api.clone(),
            id,
        }
    }
}

#[pymethods]
impl PyElement {
    /// Tag name.
    #[getter]
    fn tag(&self) -> PyResult<String> {
        self.with(|d, id| d.node(id).tag.clone())
    }
    /// Text before the first child.
    #[getter]
    fn text(&self) -> PyResult<Option<String>> {
        self.with(|d, id| d.node(id).text.clone())
    }
    /// Set the text.
    #[setter]
    fn set_text(&self, text: Option<String>) -> PyResult<()> {
        self.with_mut(|d, id| d.node_mut(id).text = text)
    }
    /// Text after the element.
    #[getter]
    fn tail(&self) -> PyResult<Option<String>> {
        self.with(|d, id| d.node(id).tail.clone())
    }
    /// Attributes, as a live dict-like `Attrib`.
    #[getter]
    fn attrib(&self) -> PyAttrib {
        PyAttrib {
            api: self.api.clone(),
            id: self.id,
        }
    }
    /// Attribute `key`, or `default`.
    #[pyo3(signature = (key, default=None))]
    fn get(&self, py: Python<'_>, key: &str, default: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self.with(|d, id| d.node(id).get(key).map(str::to_owned))? {
            Some(v) => Ok(PyString::new(py, &v).into_any().unbind()),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }
    /// Set attribute `key` to `str(value)`.
    fn set(&self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let value = value.str()?.to_cow()?.into_owned();
        self.with_mut(|d, id| d.node_mut(id).set(key, value))
    }
    /// Attribute names, in order.
    fn keys(&self) -> PyResult<Vec<String>> {
        self.with(|d, id| d.node(id).attrs.iter().map(|(k, _)| k.clone()).collect())
    }
    /// `(name, value)` attribute pairs, in order.
    fn items(&self) -> PyResult<Vec<(String, String)>> {
        self.with(|d, id| d.node(id).attrs.clone())
    }
    /// Text of the first element matching `path` (`ElementPath`).
    #[pyo3(signature = (path, default=None))]
    fn findtext(
        &self,
        py: Python<'_>,
        path: &str,
        default: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        match self.with(|d, id| d.findtext(id, path))? {
            Some(t) => Ok(PyString::new(py, &t).into_any().unbind()),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }
    /// Parent element, or `None`.
    fn getparent(&self) -> PyResult<Option<PyElement>> {
        Ok(self
            .with(|d, id| d.node(id).parent)?
            .map(|p| self.wrap_id(p)))
    }
    /// Number of children.
    fn __len__(&self) -> PyResult<usize> {
        self.with(|d, id| d.node(id).children.len())
    }
    /// Child at `index` (negative indexes count from the end).
    fn __getitem__(&self, index: isize) -> PyResult<PyElement> {
        let children = self.with(|d, id| d.node(id).children.clone())?;
        let i = if index < 0 {
            children.len() as isize + index
        } else {
            index
        };
        if i < 0 || i as usize >= children.len() {
            return Err(pyo3::exceptions::PyIndexError::new_err(
                "list index out of range",
            ));
        }
        Ok(self.wrap_id(children[i as usize]))
    }
    fn __bool__(&self) -> PyResult<bool> {
        // lxml: an element is falsy when it has no children (deprecated)
        self.__len__().map(|n| n > 0)
    }
    fn __repr__(&self) -> PyResult<String> {
        self.with(|d, id| format!("<Element {} (qweb_rs)>", d.node(id).tag))
    }
}

/// `el.attrib` proxy (dict-like).
#[pyclass(name = "Attrib", module = "qweb_rs", unsendable)]
pub struct PyAttrib {
    api: SharedApi,
    id: NodeId,
}

impl PyAttrib {
    fn with<R>(&self, f: impl FnOnce(&mut Document, NodeId) -> R) -> PyResult<R> {
        api_of(&self.api, |a| Ok(f(a.doc_mut(), self.id)))
    }
}

#[pymethods]
impl PyAttrib {
    fn __getitem__(&self, key: &str) -> PyResult<String> {
        self.with(|d, id| d.node(id).get(key).map(str::to_owned))?
            .ok_or_else(|| PyKeyError::new_err(key.to_owned()))
    }
    fn __setitem__(&self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let value = value.str()?.to_cow()?.into_owned();
        self.with(|d, id| d.node_mut(id).set(key, value))
    }
    fn __delitem__(&self, key: &str) -> PyResult<()> {
        self.with(|d, id| d.node_mut(id).pop(key))?
            .map(|_| ())
            .ok_or_else(|| PyKeyError::new_err(key.to_owned()))
    }
    fn __contains__(&self, key: &str) -> PyResult<bool> {
        self.with(|d, id| d.node(id).has(key))
    }
    fn __len__(&self) -> PyResult<usize> {
        self.with(|d, id| d.node(id).attrs.len())
    }
    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let keys = self.keys()?;
        Ok(PyList::new(py, keys)?.try_iter()?.into_any().unbind())
    }
    /// Attribute `key`, or `default`.
    #[pyo3(signature = (key, default=None))]
    fn get(&self, py: Python<'_>, key: &str, default: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self.with(|d, id| d.node(id).get(key).map(str::to_owned))? {
            Some(v) => Ok(PyString::new(py, &v).into_any().unbind()),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }
    /// Remove attribute `key` and return its value (or `default`).
    #[pyo3(signature = (key, *default))]
    fn pop(&self, py: Python<'_>, key: &str, default: &Bound<'_, PyTuple>) -> PyResult<Py<PyAny>> {
        match self.with(|d, id| d.node_mut(id).pop(key))? {
            Some(v) => Ok(PyString::new(py, &v).into_any().unbind()),
            None if !default.is_empty() => Ok(default.get_item(0)?.unbind()),
            None => Err(PyKeyError::new_err(key.to_owned())),
        }
    }
    /// Attribute names, in order.
    fn keys(&self) -> PyResult<Vec<String>> {
        self.with(|d, id| d.node(id).attrs.iter().map(|(k, _)| k.clone()).collect())
    }
    /// Attribute values, in order.
    fn values(&self) -> PyResult<Vec<String>> {
        self.with(|d, id| d.node(id).attrs.iter().map(|(_, v)| v.clone()).collect())
    }
    /// `(name, value)` attribute pairs, in order.
    fn items(&self) -> PyResult<Vec<(String, String)>> {
        self.with(|d, id| d.node(id).attrs.clone())
    }
}

/// Compiler handle given to custom directive handlers.
#[pyclass(name = "Compiler", module = "qweb_rs", unsendable)]
pub struct PyCompiler {
    api: SharedApi,
}

#[pymethods]
impl PyCompiler {
    /// `_append_text`: add static text (`None`/`False` add nothing).
    fn append_text(&self, text: &Bound<'_, PyAny>) -> PyResult<()> {
        let t = py_to_text(text)?;
        api_of(&self.api, |a| {
            a.append_text(&t);
            Ok(())
        })
    }
    /// `_compile_node`: compile an element at this point.
    fn compile_node(&self, el: PyRef<'_, PyElement>) -> PyResult<()> {
        let id = el.id;
        api_of(&self.api, |a| {
            a.compile_node(id).map_err(|e| to_pyerr(el.py(), e))
        })
    }
    /// Insert a new element `tag` in place of `el` and move `el` (with its
    /// tail) inside it. Returns the new element.
    #[pyo3(signature = (el, tag, attrib=None))]
    fn wrap(
        &self,
        el: PyRef<'_, PyElement>,
        tag: &str,
        attrib: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyElement> {
        let mut attrs = Vec::new();
        if let Some(a) = attrib {
            for (k, v) in a.iter() {
                attrs.push((
                    k.str()?.to_cow()?.into_owned(),
                    v.str()?.to_cow()?.into_owned(),
                ));
            }
        }
        let id = el.id;
        let new = api_of(&self.api, |a| Ok(a.doc_mut().wrap(id, tag, attrs)))?;
        Ok(PyElement {
            api: self.api.clone(),
            id: new,
        })
    }
    /// Add a render-time call: `bridge.run_hook((func, args), values)`; its
    /// result is output without escaping (like `yield func(...)`).
    #[pyo3(signature = (func, *args))]
    fn emit(&self, py: Python<'_>, func: Py<PyAny>, args: Py<PyTuple>) -> PyResult<()> {
        let hook = PyTuple::new(py, [func, args.into_any()])?
            .into_any()
            .unbind();
        api_of(&self.api, |a| {
            a.emit(hook);
            Ok(())
        })
    }
    /// Name of the template being compiled.
    #[getter]
    fn template(&self) -> PyResult<String> {
        api_of(&self.api, |a| Ok(a.template_name().to_owned()))
    }
}

/// Convert a core error into a Python exception, with QWeb location attributes:
/// `qweb_template`, `qweb_ref`, `qweb_path`, `qweb_element` (innermost element)
/// and `qweb_source` (call stack as `(ref, path, element)` tuples, outermost first).
fn to_pyerr(py: Python<'_>, e: Error) -> PyErr {
    let location = e.location().cloned();
    let source: Vec<(String, String, String)> = e
        .stack()
        .into_iter()
        .map(|f| {
            (
                f.reference.to_string(),
                f.path.to_string(),
                f.element.to_string(),
            )
        })
        .collect();
    let strip_quotes = |m: String| {
        if m.len() >= 2 && m.starts_with('\'') && m.ends_with('\'') {
            m[1..m.len() - 1].to_owned()
        } else {
            m
        }
    };
    let err = match e.into_kind() {
        ErrorKind::Host(b) => match b.downcast::<PyErr>() {
            Ok(err) => *err,
            Err(_) => PyRuntimeError::new_err("unknown host error"),
        },
        ErrorKind::Syntax(m) => PySyntaxError::new_err(m),
        ErrorKind::Value(m) => PyValueError::new_err(m),
        ErrorKind::Key(m) => PyKeyError::new_err(strip_quotes(m)),
        ErrorKind::Type(m) => PyTypeError::new_err(m),
        ErrorKind::Assertion(m) => PyAssertionError::new_err(m),
        ErrorKind::NotImplemented(m) => PyNotImplementedError::new_err(m),
        ErrorKind::Recursion(m) => PyRecursionError::new_err(m),
        ErrorKind::Xml(m) => match py
            .import("lxml.etree")
            .and_then(|m2| m2.getattr("XMLSyntaxError"))
        {
            Ok(cls) => match cls.call1((m.clone(), 0, 0, 0)) {
                Ok(inst) => PyErr::from_value(inst),
                Err(_) => PyValueError::new_err(m),
            },
            Err(_) => PyValueError::new_err(m),
        },
        ErrorKind::Other(m) => PyRuntimeError::new_err(m),
    };
    let value = err.value(py);
    if !value.hasattr("qweb_path").unwrap_or(false) {
        let (t, r, p, el) = match &location {
            Some(f) => (
                Some(f.template.to_string()),
                Some(f.reference.to_string()),
                Some(f.path.to_string()),
                Some(f.element.to_string()),
            ),
            None => (None, None, None, None),
        };
        let _ = value.setattr("qweb_template", t);
        let _ = value.setattr("qweb_ref", r);
        let _ = value.setattr("qweb_path", p);
        let _ = value.setattr("qweb_element", el);
    }
    // an inner render (e.g. lazy content converted to str in an expression)
    // already set its stack: the outer frames come first
    let mut full = source;
    if let Ok(prev) = value.getattr("qweb_source")
        && let Ok(prev) = prev.extract::<Vec<(String, String, String)>>()
    {
        full.extend(prev);
    }
    let _ = value.setattr("qweb_source", full);
    err
}

/// Lazily rendered template fragment (`t-set` / `t-call` body), like Odoo's `QwebContent`.
#[pyclass(name = "QwebContent", module = "qweb_rs", frozen)]
pub struct QwebContent {
    lazy: Arc<Lazy<PyHost>>,
}

#[pymethods]
impl QwebContent {
    /// Render the content (once; later calls reuse the result).
    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        self.lazy
            .render()
            .map(str::to_owned)
            .map_err(|e| to_pyerr(py, e))
    }
    /// Same as `str()`: the content is markup.
    fn __html__(&self, py: Python<'_>) -> PyResult<String> {
        self.__str__(py)
    }
    /// The rendered content, or `None` if it was not rendered yet.
    #[getter]
    fn html(&self, py: Python<'_>) -> Option<String> {
        if self.lazy.is_rendered() {
            self.__str__(py).ok()
        } else {
            None
        }
    }
    /// Length of the rendered content (in code points, like `len(str)`).
    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        self.lazy
            .render()
            .map(|s| s.chars().count())
            .map_err(|e| to_pyerr(py, e))
    }
    fn __repr__(&self) -> String {
        "<QwebContent>".to_owned()
    }
    fn __contains__(slf: &Bound<'_, Self>, key: &Bound<'_, PyAny>) -> PyResult<bool> {
        markup_self(slf)?.contains(key)
    }
    fn __getattr__<'py>(
        slf: &Bound<'py, Self>,
        name: &Bound<'py, PyString>,
    ) -> PyResult<Bound<'py, PyAny>> {
        markup_self(slf)?.getattr(name)
    }
    fn __getitem__<'py>(
        slf: &Bound<'py, Self>,
        key: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        markup_self(slf)?.get_item(key)
    }
    fn __add__<'py>(
        slf: &Bound<'py, Self>,
        other: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        markup_self(slf)?.call_method1("__add__", (other,))
    }
    fn __radd__<'py>(
        slf: &Bound<'py, Self>,
        other: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        markup_self(slf)?.call_method1("__radd__", (other,))
    }
    fn __mod__<'py>(
        slf: &Bound<'py, Self>,
        other: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        markup_self(slf)?.call_method1("__mod__", (other,))
    }
    fn __rmod__<'py>(
        slf: &Bound<'py, Self>,
        other: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        markup_self(slf)?.call_method1("__rmod__", (other,))
    }
}

fn markup_self<'py>(slf: &Bound<'py, QwebContent>) -> PyResult<Bound<'py, PyAny>> {
    let py = slf.py();
    let host = slf.get().lazy_host();
    host.markup_of(py, slf.as_any())
}

impl QwebContent {
    fn lazy_host(&self) -> PyHost {
        self.lazy.host().clone()
    }
}

/// `Engine.render` template argument: an int id or a name.
fn template_ref_of(t: &Bound<'_, PyAny>) -> PyResult<TemplateRef> {
    if let Ok(i) = t.cast::<PyInt>() {
        return Ok(TemplateRef::Id(i.extract()?));
    }
    let s = t.str()?;
    Ok(qweb_core::TemplateRef::parse(&s.to_cow()?))
}

/// Context keys that change the compiled code by default.
const DEFAULT_CACHE_KEYS: &[&str] = &["lang"];

/// A compiled template (see `Engine.compile`), usable as a `load()` result
/// or rendered directly.
#[pyclass(name = "CompiledTemplate", module = "qweb_rs", frozen)]
pub struct CompiledTemplate {
    program: Arc<Program<PyHost>>,
}

#[pymethods]
impl CompiledTemplate {
    /// Template name (used in error messages).
    #[getter]
    fn name(&self) -> String {
        self.program.name.to_string()
    }
    fn __repr__(&self) -> String {
        format!("<CompiledTemplate {}>", self.program.name)
    }
}

/// QWeb engine holding a compiled-template cache.
#[pyclass(name = "Engine", module = "qweb_rs", frozen)]
pub struct PyEngine {
    engine: Arc<Engine<PyHost>>,
    cache_keys: Arc<Vec<String>>,
    load_bridge: Py<PyAny>,
}

impl PyEngine {
    /// The bridge: `bridge` itself, or a `LoadBridge` around a `load` function.
    fn bridge_of(
        &self,
        py: Python<'_>,
        load: Option<Py<PyAny>>,
        bridge: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        match (bridge, load) {
            (Some(b), _) => Ok(b),
            (None, Some(l)) => Ok(self.load_bridge.bind(py).call1((l,))?.unbind()),
            (None, None) => Ok(self.load_bridge.bind(py).call1((py.None(),))?.unbind()),
        }
    }

    fn ctx(
        &self,
        py: Python<'_>,
        options: Option<&Bound<'_, PyAny>>,
        bridge: Py<PyAny>,
        nsmap: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Arc<PyCtx>> {
        let options = match options {
            Some(o) if !o.is_none() => match o.cast_exact::<PyDict>() {
                Ok(d) => d.copy()?.unbind(),
                // any mapping (e.g. Odoo's frozen context)
                Err(_) => PyDict::type_object(py)
                    .call1((o,))?
                    .cast_into::<PyDict>()?
                    .unbind(),
            },
            _ => PyDict::new(py).unbind(),
        };
        let nsmap = match nsmap {
            Some(n) if !n.is_none() && n.is_truthy()? => {
                let mut v = Vec::new();
                for (p, u) in mapping_items(n)? {
                    let p = p.bind(py);
                    let p = if p.is_none() {
                        None
                    } else {
                        Some(p.str()?.to_cow()?.into_owned())
                    };
                    v.push((p, u.bind(py).str()?.to_cow()?.into_owned()));
                }
                Some(Arc::new(v))
            }
            _ => None,
        };
        Ok(Arc::new(PyCtx::new(
            py,
            options,
            bridge,
            nsmap,
            self.cache_keys.clone(),
        )?))
    }
}

#[pymethods]
impl PyEngine {
    /// `trust_names`: reuse compiled templates by name without calling `load`
    /// (call `invalidate()` when templates change). `cache_keys`: context keys
    /// that change the compiled code (`_get_template_cache_keys`).
    #[new]
    #[pyo3(signature = (trust_names=false, cache_keys=None))]
    fn new(py: Python<'_>, trust_names: bool, cache_keys: Option<Vec<String>>) -> PyResult<Self> {
        let host = PyHost::new(py)?;
        let load_bridge = host.g.load_bridge.clone_ref(py);
        Ok(PyEngine {
            engine: Engine::with_options(host, trust_names),
            cache_keys: Arc::new(
                cache_keys
                    .unwrap_or_else(|| DEFAULT_CACHE_KEYS.iter().map(|s| s.to_string()).collect()),
            ),
            load_bridge,
        })
    }

    /// Render `template` (a name, an id or a `CompiledTemplate`) with
    /// `values` (used as the render scope, mutated in place like `ir.qweb`).
    ///
    /// Templates are loaded with `load(name) -> (etree|str, ref)`, or through
    /// `bridge` (see `qweb_rs.Bridge`) which also provides the ORM callbacks.
    /// `options` is the rendering context (`env.context`).
    #[pyo3(signature = (template, values, load=None, options=None, bridge=None, nsmap=None))]
    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        py: Python<'_>,
        template: &Bound<'_, PyAny>,
        values: &Bound<'_, PyDict>,
        load: Option<Py<PyAny>>,
        options: Option<&Bound<'_, PyAny>>,
        bridge: Option<Py<PyAny>>,
        nsmap: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        let bridge = self.bridge_of(py, load, bridge)?;
        let ctx = self.ctx(py, options, bridge, nsmap)?;
        let scope = values.clone().unbind();
        let res = if let Ok(t) = template.cast::<CompiledTemplate>() {
            self.engine.render_program(&t.get().program, scope, ctx)
        } else {
            let tref = template_ref_of(template)?;
            self.engine.render(&tref, scope, ctx)
        };
        res.map_err(|e| to_pyerr(py, e))
    }

    /// Render a template from its XML source (the root element is the template).
    #[pyo3(signature = (source, values, load=None, options=None, bridge=None))]
    fn render_source(
        &self,
        py: Python<'_>,
        source: &str,
        values: &Bound<'_, PyDict>,
        load: Option<Py<PyAny>>,
        options: Option<&Bound<'_, PyAny>>,
        bridge: Option<Py<PyAny>>,
    ) -> PyResult<String> {
        let bridge = self.bridge_of(py, load, bridge)?;
        let ctx = self.ctx(py, options, bridge, None)?;
        self.engine
            .render_source(source, values.clone().unbind(), ctx)
            .map_err(|e| to_pyerr(py, e))
    }

    /// Compile a template source in the context `options` (with the compile
    /// hooks of `bridge`). `ref` / `ref_name` are the `viewid` / `xmlid`
    /// values set when rendering; `name` is used in error messages.
    #[pyo3(signature = (source, options=None, bridge=None, r#ref=None, ref_name=None, name=None, nsmap=None))]
    #[allow(clippy::too_many_arguments)]
    fn compile(
        &self,
        py: Python<'_>,
        source: &str,
        options: Option<&Bound<'_, PyAny>>,
        bridge: Option<Py<PyAny>>,
        r#ref: Option<Py<PyAny>>,
        ref_name: Option<String>,
        name: Option<String>,
        nsmap: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<CompiledTemplate> {
        let bridge = self.bridge_of(py, None, bridge)?;
        let ctx = self.ctx(py, options, bridge, nsmap)?;
        let name = match (name, &ref_name, &r#ref) {
            (Some(n), _, _) => n,
            (None, Some(n), _) => n.clone(),
            (None, None, Some(r)) => r.bind(py).str()?.to_cow()?.into_owned(),
            _ => "etree._Element".to_owned(),
        };
        let program = self
            .engine
            .compile_program(source, &ctx, ref_name, r#ref, &name)
            .map_err(|e| to_pyerr(py, e))?;
        Ok(CompiledTemplate {
            program: Arc::new(program),
        })
    }

    /// Drop all compiled templates.
    fn invalidate(&self) {
        self.engine.invalidate();
    }
}

/// ``apply_inheritance_specs`` on serialized trees: apply each spec tree of
/// ``specs`` in order to ``source`` and return the serialized result.
/// Errors raise ``ValueError(message)`` with a ``kind`` attribute
/// (``'validation'`` or ``'value'``).
#[pyfunction]
#[pyo3(signature = (source, specs, inherit_branding=false))]
fn apply_inheritance(
    py: Python<'_>,
    source: &str,
    specs: Vec<String>,
    inherit_branding: bool,
) -> PyResult<String> {
    let specs: Vec<&str> = specs.iter().map(String::as_str).collect();
    let opts = qweb_inherit::Options { inherit_branding };
    py.detach(|| qweb_inherit::apply_inheritance_str(source, &specs, opts))
        .map_err(|e| {
            let err = PyValueError::new_err(e.message.clone());
            let kind = match e.kind {
                qweb_inherit::ErrorKind::Validation => "validation",
                qweb_inherit::ErrorKind::Value => "value",
            };
            let _ = err.value(py).setattr("kind", kind);
            err
        })
}

/// The extension module.
#[pymodule]
fn _qweb_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(apply_inheritance, m)?)?;
    m.add_class::<PyEngine>()?;
    m.add_class::<QwebContent>()?;
    m.add_class::<CompiledTemplate>()?;
    m.add_class::<PyElement>()?;
    m.add_class::<PyAttrib>()?;
    m.add_class::<PyCompiler>()?;
    m.add("FLAG_POST_PROCESSING", FLAG_POST_PROCESSING)?;
    m.add("FLAG_COMPILE_HOOKS", FLAG_COMPILE_HOOKS)?;
    m.add("FLAG_WITH_CONTEXT", FLAG_WITH_CONTEXT)?;
    Ok(())
}
