//! Python-like dynamic values.

use core::cmp::Ordering;
use core::fmt::{self, Write as _};
use std::sync::Arc;

use indexmap::IndexMap;
use qweb_core::escape::escape_into;
use qweb_core::{Error, Lazy, Result};

use crate::ValueHost;

/// Builtin functions available to expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `len()`.
    Len,
    /// `str()`.
    Str,
    /// `int()`.
    Int,
    /// `float()`.
    Float,
    /// `bool()`.
    Bool,
    /// `abs()`.
    Abs,
    /// `min()`.
    Min,
    /// `max()`.
    Max,
    /// `sum()`.
    Sum,
    /// `sorted()`.
    Sorted,
    /// `range()`.
    Range,
    /// `enumerate()`.
    Enumerate,
    /// `zip()`.
    Zip,
    /// `list()`.
    List,
    /// `tuple()`.
    Tuple,
    /// `dict()`.
    Dict,
    /// `set()`.
    Set,
    /// `round()`.
    Round,
    /// `repr()`.
    Repr,
    /// `any()`.
    Any,
    /// `all()`.
    All,
    /// `ord()`.
    Ord,
    /// `chr()`.
    Chr,
    /// `divmod()`.
    Divmod,
}

impl Builtin {
    /// The builtin called `name` in a template (`unicode` and `xrange` are
    /// aliases of `str` and `range`).
    pub fn from_name(name: &str) -> Option<Builtin> {
        use Builtin::{
            Abs, All, Any, Bool, Chr, Dict, Divmod, Enumerate, Float, Int, Len, List, Max, Min,
            Ord, Range, Repr, Round, Set, Sorted, Str, Sum, Tuple, Zip,
        };
        Some(match name {
            "len" => Len,
            "str" | "unicode" => Str,
            "int" => Int,
            "float" => Float,
            "bool" => Bool,
            "abs" => Abs,
            "min" => Min,
            "max" => Max,
            "sum" => Sum,
            "sorted" => Sorted,
            "range" | "xrange" => Range,
            "enumerate" => Enumerate,
            "zip" => Zip,
            "list" => List,
            "tuple" => Tuple,
            "dict" => Dict,
            "set" => Set,
            "round" => Round,
            "repr" => Repr,
            "any" => Any,
            "all" => All,
            "ord" => Ord,
            "chr" => Chr,
            "divmod" => Divmod,
            _ => return None,
        })
    }

    /// Python name of the builtin.
    pub fn name(self) -> &'static str {
        use Builtin::{
            Abs, All, Any, Bool, Chr, Dict, Divmod, Enumerate, Float, Int, Len, List, Max, Min,
            Ord, Range, Repr, Round, Set, Sorted, Str, Sum, Tuple, Zip,
        };
        match self {
            Len => "len",
            Str => "str",
            Int => "int",
            Float => "float",
            Bool => "bool",
            Abs => "abs",
            Min => "min",
            Max => "max",
            Sum => "sum",
            Sorted => "sorted",
            Range => "range",
            Enumerate => "enumerate",
            Zip => "zip",
            List => "list",
            Tuple => "tuple",
            Dict => "dict",
            Set => "set",
            Round => "round",
            Repr => "repr",
            Any => "any",
            All => "all",
            Ord => "ord",
            Chr => "chr",
            Divmod => "divmod",
        }
    }
}

/// Hashable dict key (Python equality: `True == 1 == 1.0`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HKey {
    /// `None`.
    None,
    /// An int, a bool, or a float with an integral value.
    Int(i64),
    /// Bits of a non-integral float.
    Float(u64),
    /// A str or Markup.
    Str(Arc<str>),
    /// Bytes.
    Bytes(Arc<[u8]>),
    /// A tuple of hashable items.
    Tuple(Vec<HKey>),
}

/// A Python-like value. Containers are immutable and shared (`Arc`), so
/// cloning is cheap.
#[derive(Clone)]
pub enum Value {
    /// `None`.
    None,
    /// `bool`.
    Bool(bool),
    /// `int` (64 bits: overflow is an `OverflowError`).
    Int(i64),
    /// `float`.
    Float(f64),
    /// `str`.
    Str(Arc<str>),
    /// `markupsafe.Markup`.
    Markup(Arc<str>),
    /// `bytes`.
    Bytes(Arc<[u8]>),
    /// `list`.
    List(Arc<Vec<Value>>),
    /// `tuple`.
    Tuple(Arc<Vec<Value>>),
    /// `dict`: hash key to the original key and the value, in insertion order.
    Dict(Arc<IndexMap<HKey, (Value, Value)>>),
    /// `set`, in insertion order (items are unique by Python equality).
    Set(Arc<Vec<Value>>),
    /// Object with attributes (like a record / `SimpleNamespace`).
    Object(Arc<IndexMap<Arc<str>, Value>>),
    /// A builtin function.
    Builtin(Builtin),
    /// A bound method: receiver and method name.
    Method(Arc<(Value, Arc<str>)>),
    /// `slice(lower, upper, step)`.
    Slice(Arc<(Value, Value, Value)>),
    /// Lazy template content (`QwebContent`), rendered on first use.
    Lazy(Arc<Lazy<ValueHost>>),
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.repr())
    }
}

/// `AttributeError: 'type' object has no attribute 'attr'`.
pub fn attribute_error(type_name: &str, attr: &str) -> Error {
    Error::other(format!(
        "AttributeError: '{type_name}' object has no attribute '{attr}'"
    ))
}

/// Python `repr(float)`.
pub fn float_repr(f: f64) -> String {
    let mut out = String::new();
    write_float(f, &mut out);
    out
}

/// A string formatted in a fixed-size stack buffer (no allocation).
struct StackStr<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackStr<N> {
    fn new() -> Self {
        StackStr {
            buf: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        // Only whole `&str`s are copied in.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for StackStr<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        if end > N {
            return Err(fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Python `repr(float)` appended to `out`.
fn write_float(f: f64, out: &mut String) {
    if f.is_nan() {
        return out.push_str("nan");
    }
    if f.is_infinite() {
        return out.push_str(if f > 0.0 { "inf" } else { "-inf" });
    }
    if f == 0.0 {
        return out.push_str(if f.is_sign_negative() { "-0.0" } else { "0.0" });
    }
    // Shortest round-trip digits and exponent, then Python's layout rules.
    // Both are formatted on the stack (at most 17 digits plus the exponent).
    let mut e = StackStr::<32>::new();
    let _ = write!(e, "{:e}", f.abs());
    // `{:e}` always writes `<digits>e<exponent>`.
    let (mant, exp) = e.as_str().split_once('e').unwrap_or((e.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits = StackStr::<32>::new();
    for part in mant.split('.') {
        let _ = digits.write_str(part);
    }
    let digits = digits.as_str();
    let decpt = exp + 1;
    if f < 0.0 {
        out.push('-');
    }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..(-decpt) {
            out.push('0');
        }
        out.push_str(digits);
    } else {
        let d = decpt as usize;
        if digits.len() <= d {
            out.push_str(digits);
            for _ in digits.len()..d {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..d]);
            out.push('.');
            out.push_str(&digits[d..]);
        }
    }
}

/// Python `repr(str)`.
pub fn str_repr(s: &str) -> String {
    qweb_core::compile::py_repr_str(s)
}

impl Value {
    /// A `str`.
    pub fn str(s: &str) -> Value {
        Value::Str(Arc::from(s))
    }

    /// A `Markup` string.
    pub fn markup(s: &str) -> Value {
        Value::Markup(Arc::from(s))
    }

    /// A `list`.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Arc::new(items))
    }

    /// An object with the given attributes.
    pub fn object<K: AsRef<str>, I: IntoIterator<Item = (K, Value)>>(fields: I) -> Value {
        Value::Object(Arc::new(
            fields
                .into_iter()
                .map(|(k, v)| (Arc::from(k.as_ref()), v))
                .collect(),
        ))
    }

    /// `dict(pairs)`: a later duplicate key replaces the value but keeps the
    /// first key and its position.
    ///
    /// # Errors
    ///
    /// A key is unhashable (`TypeError`).
    pub fn dict_from(pairs: Vec<(Value, Value)>) -> Result<Value> {
        let mut m = IndexMap::with_capacity(pairs.len());
        for (k, v) in pairs {
            m.entry(k.hkey()?).or_insert_with(|| (k, Value::None)).1 = v;
        }
        Ok(Value::Dict(Arc::new(m)))
    }

    /// `type(self).__name__`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::None => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::Markup(_) => "Markup",
            Value::Bytes(_) => "bytes",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Dict(_) => "dict",
            Value::Set(_) => "set",
            Value::Object(_) => "object",
            Value::Builtin(_) | Value::Method(_) => "builtin_function_or_method",
            Value::Slice(_) => "slice",
            Value::Lazy(_) => "QwebContent",
        }
    }

    /// Hash key of the value.
    ///
    /// # Errors
    ///
    /// The value is unhashable (`TypeError`).
    pub fn hkey(&self) -> Result<HKey> {
        Ok(match self {
            Value::None => HKey::None,
            Value::Bool(b) => HKey::Int(i64::from(*b)),
            Value::Int(i) => HKey::Int(*i),
            Value::Float(f) => {
                if f.fract() == 0.0 && f.abs() < 9.2e18 {
                    HKey::Int(*f as i64)
                } else {
                    HKey::Float(f.to_bits())
                }
            }
            Value::Str(s) | Value::Markup(s) => HKey::Str(s.clone()),
            Value::Bytes(b) => HKey::Bytes(b.clone()),
            Value::Tuple(items) => {
                HKey::Tuple(items.iter().map(Value::hkey).collect::<Result<_>>()?)
            }
            other => {
                return Err(Error::type_error(format!(
                    "unhashable type: '{}'",
                    other.type_name()
                )));
            }
        })
    }

    /// `bool(self)`.
    ///
    /// # Errors
    ///
    /// Rendering a lazy value (`QwebContent`) failed.
    pub fn truthy(&self) -> Result<bool> {
        Ok(match self {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) | Value::Markup(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => !v.is_empty(),
            Value::Dict(d) => !d.is_empty(),
            Value::Lazy(l) => !l.render()?.is_empty(),
            _ => true,
        })
    }

    /// `str(self)` appended to `out`.
    ///
    /// # Errors
    ///
    /// Rendering a lazy value (`QwebContent`) failed.
    pub fn write_str(&self, out: &mut String) -> Result<()> {
        match self {
            Value::Str(s) | Value::Markup(s) => out.push_str(s),
            Value::Lazy(l) => out.push_str(l.render()?),
            other => other.write_repr(out),
        }
        Ok(())
    }

    /// `str(self)`.
    ///
    /// # Errors
    ///
    /// Rendering a lazy value (`QwebContent`) failed.
    pub fn to_str(&self) -> Result<String> {
        let mut s = String::new();
        self.write_str(&mut s)?;
        Ok(s)
    }

    /// Python `repr()`.
    pub fn repr(&self) -> String {
        let mut out = String::new();
        self.write_repr(&mut out);
        out
    }

    /// Python `repr()` appended to `out`.
    pub fn write_repr(&self, out: &mut String) {
        fn seq(out: &mut String, items: &[Value], open: &str, close: &str) {
            out.push_str(open);
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                v.write_repr(out);
            }
            out.push_str(close);
        }
        match self {
            Value::None => out.push_str("None"),
            Value::Bool(true) => out.push_str("True"),
            Value::Bool(false) => out.push_str("False"),
            Value::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Value::Float(f) => write_float(*f, out),
            Value::Str(s) => out.push_str(&str_repr(s)),
            Value::Markup(s) => {
                out.push_str("Markup(");
                out.push_str(&str_repr(s));
                out.push(')');
            }
            Value::Bytes(b) => {
                // Like Python: double quotes if there is a `'` and no `"`.
                let quote = if b.contains(&b'\'') && !b.contains(&b'"') {
                    b'"'
                } else {
                    b'\''
                };
                out.push('b');
                out.push(quote as char);
                for &c in b.iter() {
                    match c {
                        b'\\' => out.push_str("\\\\"),
                        c if c == quote => {
                            out.push('\\');
                            out.push(c as char);
                        }
                        b'\n' => out.push_str("\\n"),
                        b'\r' => out.push_str("\\r"),
                        b'\t' => out.push_str("\\t"),
                        0x20..=0x7e => out.push(c as char),
                        _ => {
                            let _ = write!(out, "\\x{c:02x}");
                        }
                    }
                }
                out.push(quote as char);
            }
            Value::List(v) => seq(out, v, "[", "]"),
            Value::Tuple(v) if v.len() == 1 => seq(out, v, "(", ",)"),
            Value::Tuple(v) => seq(out, v, "(", ")"),
            Value::Set(v) if v.is_empty() => out.push_str("set()"),
            Value::Set(v) => seq(out, v, "{", "}"),
            Value::Dict(d) => {
                out.push('{');
                for (i, (k, v)) in d.values().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    k.write_repr(out);
                    out.push_str(": ");
                    v.write_repr(out);
                }
                out.push('}');
            }
            Value::Object(o) => {
                out.push_str("namespace(");
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push('=');
                    v.write_repr(out);
                }
                out.push(')');
            }
            Value::Builtin(b) => {
                let _ = write!(out, "<built-in function {}>", b.name());
            }
            Value::Method(m) => {
                let _ = write!(
                    out,
                    "<built-in method {} of {} object>",
                    m.1,
                    m.0.type_name()
                );
            }
            Value::Slice(s) => {
                out.push_str("slice(");
                s.0.write_repr(out);
                out.push_str(", ");
                s.1.write_repr(out);
                out.push_str(", ");
                s.2.write_repr(out);
                out.push(')');
            }
            Value::Lazy(_) => out.push_str("<QwebContent>"),
        }
    }

    /// The value as a float, for numbers (bool, int, float).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Bool(b) => Some(i64::from(*b) as f64),
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// The value as an int, for bool and int.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Bool(b) => Some(i64::from(*b)),
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Items of an iterable.
    ///
    /// # Errors
    ///
    /// The value is not iterable (`TypeError`), or rendering a lazy value
    /// failed.
    pub fn iter_items(&self) -> Result<Vec<Value>> {
        Ok(match self {
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => (**v).clone(),
            Value::Dict(d) => d.values().map(|(k, _)| k.clone()).collect(),
            Value::Str(s) | Value::Markup(s) => s
                .chars()
                .map(|c| Value::str(c.encode_utf8(&mut [0; 4])))
                .collect(),
            Value::Bytes(b) => b.iter().map(|&c| Value::Int(i64::from(c))).collect(),
            Value::Object(o) => o.keys().map(|k| Value::Str(k.clone())).collect(),
            other => {
                return Err(Error::type_error(format!(
                    "'{}' object is not iterable",
                    other.type_name()
                )));
            }
        })
    }

    /// `len(self)`.
    ///
    /// # Errors
    ///
    /// The value has no length (`TypeError`), or rendering a lazy value
    /// failed.
    pub fn py_len(&self) -> Result<usize> {
        Ok(match self {
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => v.len(),
            Value::Dict(d) => d.len(),
            Value::Str(s) | Value::Markup(s) => s.chars().count(),
            Value::Bytes(b) => b.len(),
            Value::Lazy(l) => l.render()?.chars().count(),
            other => {
                return Err(Error::type_error(format!(
                    "object of type '{}' has no len()",
                    other.type_name()
                )));
            }
        })
    }

    /// `self == other`.
    pub fn py_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::None, Value::None) => true,
            (a, b) if a.as_f64().is_some() && b.as_f64().is_some() => {
                match (a.as_i64(), b.as_i64()) {
                    (Some(x), Some(y)) => x == y,
                    _ => a.as_f64() == b.as_f64(),
                }
            }
            (Value::Str(a) | Value::Markup(a), Value::Str(b) | Value::Markup(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.py_eq(y))
            }
            (Value::Set(a), Value::Set(b)) => {
                a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| x.py_eq(y)))
            }
            (Value::Dict(a), Value::Dict(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, (_, v))| b.get(k).is_some_and(|(_, w)| v.py_eq(w)))
            }
            (Value::Object(a), Value::Object(b)) => Arc::ptr_eq(a, b),
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            (Value::Lazy(a), Value::Lazy(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Ordering of `self` and `other`. Use [`crate::ops::compare`] for the
    /// comparison operators.
    ///
    /// # Errors
    ///
    /// The values are not comparable (`TypeError`), or one is NaN
    /// (`ValueError`).
    pub fn py_cmp(&self, other: &Value) -> Result<Ordering> {
        match (self, other) {
            (a, b) if a.as_f64().is_some() && b.as_f64().is_some() => {
                match (a.as_i64(), b.as_i64()) {
                    (Some(x), Some(y)) => Ok(x.cmp(&y)),
                    // (both are numbers: checked above)
                    _ => a
                        .as_f64()
                        .unwrap_or_default()
                        .partial_cmp(&b.as_f64().unwrap_or_default())
                        .ok_or_else(|| Error::value("nan")),
                }
            }
            (Value::Str(a) | Value::Markup(a), Value::Str(b) | Value::Markup(b)) => Ok(a.cmp(b)),
            (Value::Bytes(a), Value::Bytes(b)) => Ok(a.cmp(b)),
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    if !x.py_eq(y) {
                        return x.py_cmp(y);
                    }
                }
                Ok(a.len().cmp(&b.len()))
            }
            (a, b) => Err(Error::type_error(format!(
                "'<' not supported between instances of '{}' and '{}'",
                a.type_name(),
                b.type_name()
            ))),
        }
    }
}

/// `markupsafe.escape` returning a Markup string.
///
/// # Errors
///
/// Rendering a lazy value (`QwebContent`) failed.
pub fn escape_value(v: &Value) -> Result<Arc<str>> {
    match v {
        Value::Markup(s) => Ok(s.clone()),
        Value::Lazy(l) => Ok(Arc::from(l.render()?)),
        other => {
            let s = other.to_str()?;
            let mut out = String::with_capacity(s.len());
            escape_into(&mut out, &s);
            Ok(Arc::from(out))
        }
    }
}
