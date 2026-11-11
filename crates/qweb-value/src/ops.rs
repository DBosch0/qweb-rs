//! Operators, builtins and methods with Python semantics.
//!
//! # Errors
//!
//! Every function fails where Python raises, with the matching
//! [`ErrorKind`](qweb_core::ErrorKind): `TypeError` for unsupported operand
//! types or arguments, `KeyError` or `IndexError` for missing items,
//! `ValueError`, `ZeroDivisionError`, and so on. Integers are 64-bit: an
//! overflow is an error where Python would switch to a big integer.
#![allow(
    clippy::missing_errors_doc,
    reason = "the errors are Python's, described above"
)]

use core::cmp::Ordering;
use std::sync::Arc;

use qweb_core::expr::{BinOp, CmpOp, UnOp};
use qweb_core::{Error, Result};

use crate::value::{Builtin, Value, attribute_error, escape_value, str_repr};

fn unsupported(op: &str, a: &Value, b: &Value) -> Error {
    Error::type_error(format!(
        "unsupported operand type(s) for {op}: '{}' and '{}'",
        a.type_name(),
        b.type_name()
    ))
}

fn overflow() -> Error {
    Error::other("OverflowError: integer overflow (big integers are not supported by qweb-value)")
}

fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::MatMul => "@",
        BinOp::Div => "/",
        BinOp::FloorDiv => "//",
        BinOp::Mod => "%",
        BinOp::Pow => "** or pow()",
        BinOp::LShift => "<<",
        BinOp::RShift => ">>",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
    }
}

fn py_mod_i(a: i64, b: i64) -> Result<i64> {
    if b == 0 {
        return Err(Error::other("ZeroDivisionError: integer modulo by zero"));
    }
    let r = a.wrapping_rem(b);
    Ok(if r != 0 && ((r < 0) != (b < 0)) {
        r + b
    } else {
        r
    })
}

fn py_floordiv_i(a: i64, b: i64) -> Result<i64> {
    if b == 0 {
        return Err(Error::other(
            "ZeroDivisionError: integer division or modulo by zero",
        ));
    }
    let q = a.checked_div(b).ok_or_else(overflow)?;
    Ok(if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    })
}

fn py_mod_f(a: f64, b: f64) -> Result<f64> {
    if b == 0.0 {
        return Err(Error::other("ZeroDivisionError: float modulo"));
    }
    let r = a % b;
    Ok(if r != 0.0 && ((r < 0.0) != (b < 0.0)) {
        r + b
    } else {
        r
    })
}

fn repeat(items: &[Value], n: i64) -> Vec<Value> {
    let n = n.max(0) as usize;
    let mut out = Vec::with_capacity(items.len() * n);
    for _ in 0..n {
        out.extend_from_slice(items);
    }
    out
}

/// `a <op> b`.
pub fn binary(op: BinOp, a: &Value, b: &Value) -> Result<Value> {
    use Value::{Bool, Bytes, Float, Int, Lazy, List, Markup, Str, Tuple};
    // Markup arithmetic: the other operand is escaped.
    if op == BinOp::Add {
        match (a, b) {
            (Markup(x), Str(_) | Markup(_) | Lazy(_)) => {
                return Ok(Value::Markup(Arc::from(format!("{x}{}", escape_value(b)?))));
            }
            (Str(_) | Lazy(_), Markup(y)) => {
                return Ok(Value::Markup(Arc::from(format!("{}{y}", escape_value(a)?))));
            }
            (Lazy(x), _) => return binary(op, &Value::Markup(Arc::from(x.render()?)), b),
            (_, Lazy(y)) => return binary(op, a, &Value::Markup(Arc::from(y.render()?))),
            _ => {}
        }
    }
    if op == BinOp::Mod {
        if let Lazy(x) = a {
            return binary(op, &Value::Markup(Arc::from(x.render()?)), b);
        }
        if let Lazy(y) = b {
            return binary(op, a, &Value::Markup(Arc::from(y.render()?)));
        }
        match a {
            Str(f) => return Ok(Value::Str(Arc::from(percent_format(f, b, false)?))),
            Markup(f) => return Ok(Value::Markup(Arc::from(percent_format(f, b, true)?))),
            _ => {}
        }
    }
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return Ok(match op {
            BinOp::Add => Int(x.checked_add(y).ok_or_else(overflow)?),
            BinOp::Sub => Int(x.checked_sub(y).ok_or_else(overflow)?),
            BinOp::Mul => Int(x.checked_mul(y).ok_or_else(overflow)?),
            BinOp::Div => {
                if y == 0 {
                    return Err(Error::other("ZeroDivisionError: division by zero"));
                }
                Float(x as f64 / y as f64)
            }
            BinOp::FloorDiv => Int(py_floordiv_i(x, y)?),
            BinOp::Mod => Int(py_mod_i(x, y)?),
            BinOp::Pow => {
                if y < 0 {
                    Float((x as f64).powf(y as f64))
                } else {
                    Int(x
                        .checked_pow(u32::try_from(y).map_err(|_| overflow())?)
                        .ok_or_else(overflow)?)
                }
            }
            BinOp::LShift => Int(x
                .checked_shl(u32::try_from(y).map_err(|_| overflow())?)
                .ok_or_else(overflow)?),
            BinOp::RShift => Int(x >> y.clamp(0, 63)),
            BinOp::BitAnd => {
                if let (Bool(p), Bool(q)) = (a, b) {
                    Bool(*p && *q)
                } else {
                    Int(x & y)
                }
            }
            BinOp::BitOr => {
                if let (Bool(p), Bool(q)) = (a, b) {
                    Bool(*p || *q)
                } else {
                    Int(x | y)
                }
            }
            BinOp::BitXor => {
                if let (Bool(p), Bool(q)) = (a, b) {
                    Bool(*p ^ *q)
                } else {
                    Int(x ^ y)
                }
            }
            BinOp::MatMul => return Err(unsupported("@", a, b)),
        });
    }
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
        return Ok(Float(match op {
            BinOp::Add => x + y,
            BinOp::Sub => x - y,
            BinOp::Mul => x * y,
            BinOp::Div => {
                if y == 0.0 {
                    return Err(Error::other("ZeroDivisionError: float division by zero"));
                }
                x / y
            }
            BinOp::FloorDiv => {
                if y == 0.0 {
                    return Err(Error::other(
                        "ZeroDivisionError: float floor division by zero",
                    ));
                }
                (x / y).floor()
            }
            BinOp::Mod => py_mod_f(x, y)?,
            BinOp::Pow => x.powf(y),
            _ => return Err(unsupported(op_symbol(op), a, b)),
        }));
    }
    match (op, a, b) {
        (BinOp::Add, Str(x), Str(y)) => Ok(Value::Str(Arc::from(format!("{x}{y}")))),
        (BinOp::Add, Bytes(x), Bytes(y)) => Ok(Value::Bytes(Arc::from([&x[..], &y[..]].concat()))),
        (BinOp::Add, List(x), List(y)) => Ok(Value::list([&x[..], &y[..]].concat())),
        (BinOp::Add, Tuple(x), Tuple(y)) => Ok(Value::Tuple(Arc::new([&x[..], &y[..]].concat()))),
        (BinOp::Mul, Str(s) | Markup(s), n) | (BinOp::Mul, n, Str(s) | Markup(s))
            if n.as_i64().is_some() =>
        {
            let r = s.repeat(n.as_i64().unwrap_or_default().max(0) as usize);
            Ok(if matches!(a, Markup(_)) || matches!(b, Markup(_)) {
                Value::markup(&r)
            } else {
                Value::str(&r)
            })
        }
        (BinOp::Mul, List(v), n) | (BinOp::Mul, n, List(v)) if n.as_i64().is_some() => {
            Ok(Value::list(repeat(v, n.as_i64().unwrap_or_default())))
        }
        (BinOp::Mul, Tuple(v), n) | (BinOp::Mul, n, Tuple(v)) if n.as_i64().is_some() => Ok(
            Value::Tuple(Arc::new(repeat(v, n.as_i64().unwrap_or_default()))),
        ),
        _ => {
            if op == BinOp::Add && matches!((a, b), (Str(_), _) | (_, Str(_))) {
                return Err(Error::type_error(format!(
                    "can only concatenate {} (not \"{}\") to {}",
                    a.type_name(),
                    b.type_name(),
                    a.type_name()
                )));
            }
            Err(unsupported(op_symbol(op), a, b))
        }
    }
}

/// `fmt % args` with the common conversions (`%s %r %d %i %f %%`).
fn percent_format(fmt: &str, args: &Value, markup: bool) -> Result<String> {
    let items: Vec<Value> = match args {
        Value::Tuple(v) => (**v).clone(),
        other => vec![other.clone()],
    };
    let mut it = items.into_iter();
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        while let Some(&n) = chars.peek() {
            if n.is_ascii_digit() || n == '.' || n == '-' || n == '+' || n == ' ' || n == '0' {
                spec.push(n);
                chars.next();
            } else {
                break;
            }
        }
        let Some(conv) = chars.next() else {
            return Err(Error::value("incomplete format"));
        };
        if conv == '%' {
            out.push('%');
            continue;
        }
        let arg = it
            .next()
            .ok_or_else(|| Error::type_error("not enough arguments for format string"))?;
        let text = match conv {
            's' => {
                if markup {
                    escape_value(&arg)?.to_string()
                } else {
                    arg.to_str()?
                }
            }
            'r' => arg.repr(),
            'd' | 'i' => match arg.as_f64() {
                Some(f) => format!("{}", f.trunc() as i64),
                None => {
                    return Err(Error::type_error(format!(
                        "%{conv} format: a real number is required, not {}",
                        arg.type_name()
                    )));
                }
            },
            'f' | 'F' => {
                let prec = spec
                    .split_once('.')
                    .and_then(|(_, p)| p.parse::<usize>().ok())
                    .unwrap_or(6);
                let f = arg
                    .as_f64()
                    .ok_or_else(|| Error::type_error("must be real number"))?;
                format!("{f:.prec$}")
            }
            _ => {
                return Err(Error::value(format!(
                    "unsupported format character '{conv}'"
                )));
            }
        };
        // width
        let width: usize = spec
            .split('.')
            .next()
            .unwrap_or("")
            .trim_start_matches(['-', '+', ' '])
            .parse()
            .unwrap_or(0);
        if text.chars().count() < width {
            let pad = width - text.chars().count();
            if spec.starts_with('-') {
                out.push_str(&text);
                out.extend(std::iter::repeat_n(' ', pad));
            } else {
                let fill = if spec.starts_with('0') && conv != 's' {
                    '0'
                } else {
                    ' '
                };
                out.extend(std::iter::repeat_n(fill, pad));
                out.push_str(&text);
            }
        } else {
            out.push_str(&text);
        }
    }
    if it.next().is_some() {
        return Err(Error::type_error(
            "not all arguments converted during string formatting",
        ));
    }
    Ok(out)
}

/// `<op> v`.
pub fn unary(op: UnOp, v: &Value) -> Result<Value> {
    Ok(match (op, v) {
        (UnOp::Neg, Value::Float(f)) => Value::Float(-f),
        (UnOp::Neg, x) if x.as_i64().is_some() => Value::Int(
            x.as_i64()
                .unwrap_or_default()
                .checked_neg()
                .ok_or_else(overflow)?,
        ),
        (UnOp::Pos, Value::Float(f)) => Value::Float(*f),
        (UnOp::Pos, x) if x.as_i64().is_some() => Value::Int(x.as_i64().unwrap_or_default()),
        (UnOp::Invert, x) if x.as_i64().is_some() => Value::Int(!x.as_i64().unwrap_or_default()),
        (_, x) => {
            return Err(Error::type_error(format!(
                "bad operand type for unary op: '{}'",
                x.type_name()
            )));
        }
    })
}

/// `a <op> b` for the equality and ordering operators.
pub fn compare(op: CmpOp, a: &Value, b: &Value) -> Result<Value> {
    // Mixed or float numbers: IEEE comparisons (any comparison with NaN is
    // false, except `!=`).
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64())
        && (matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)))
    {
        #[allow(clippy::float_cmp, reason = "Python compares floats exactly")]
        return Ok(Value::Bool(match op {
            CmpOp::Eq => x == y,
            CmpOp::Ne => x != y,
            CmpOp::Lt => x < y,
            CmpOp::Le => x <= y,
            CmpOp::Gt => x > y,
            CmpOp::Ge => x >= y,
            _ => unreachable!(),
        }));
    }
    Ok(Value::Bool(match op {
        CmpOp::Eq => a.py_eq(b),
        CmpOp::Ne => !a.py_eq(b),
        CmpOp::Lt => a.py_cmp(b)? == Ordering::Less,
        CmpOp::Le => a.py_cmp(b)? != Ordering::Greater,
        CmpOp::Gt => a.py_cmp(b)? == Ordering::Greater,
        CmpOp::Ge => a.py_cmp(b)? != Ordering::Less,
        _ => unreachable!(),
    }))
}

/// `item in container`.
pub fn contains(container: &Value, item: &Value) -> Result<bool> {
    Ok(match container {
        Value::Str(s) | Value::Markup(s) => match item {
            Value::Str(i) | Value::Markup(i) => s.contains(&**i),
            other => {
                return Err(Error::type_error(format!(
                    "'in <string>' requires string as left operand, not {}",
                    other.type_name()
                )));
            }
        },
        Value::List(v) | Value::Tuple(v) | Value::Set(v) => v.iter().any(|x| x.py_eq(item)),
        Value::Dict(d) => d.contains_key(&item.hkey()?),
        Value::Lazy(l) => {
            let s = l.render()?;
            match item {
                Value::Str(i) | Value::Markup(i) => s.contains(&**i),
                _ => false,
            }
        }
        other => {
            return Err(Error::type_error(format!(
                "argument of type '{}' is not iterable",
                other.type_name()
            )));
        }
    })
}

fn norm_index(i: i64, len: usize) -> Option<usize> {
    let idx = if i < 0 { i + len as i64 } else { i };
    if idx < 0 || idx >= len as i64 {
        None
    } else {
        Some(idx as usize)
    }
}

fn slice_indices(s: &(Value, Value, Value), len: usize) -> Result<Vec<usize>> {
    let step = match &s.2 {
        Value::None => 1,
        v => v
            .as_i64()
            .ok_or_else(|| Error::type_error("slice indices must be integers"))?,
    };
    if step == 0 {
        return Err(Error::value("slice step cannot be zero"));
    }
    let len = len as i64;
    let clamp = |v: &Value, default: i64, lo: i64, hi: i64| -> Result<i64> {
        match v {
            Value::None => Ok(default),
            v => {
                let mut i = v
                    .as_i64()
                    .ok_or_else(|| Error::type_error("slice indices must be integers"))?;
                if i < 0 {
                    i += len;
                }
                Ok(i.clamp(lo, hi))
            }
        }
    };
    let mut out = Vec::new();
    if step > 0 {
        let start = clamp(&s.0, 0, 0, len)?;
        let stop = clamp(&s.1, len, 0, len)?;
        let mut i = start;
        while i < stop {
            out.push(i as usize);
            i += step;
        }
    } else {
        let start = clamp(&s.0, len - 1, -1, len - 1)?;
        let stop = clamp(&s.1, -1, -1, len - 1)?;
        let mut i = start;
        while i > stop {
            out.push(i as usize);
            i += step;
        }
    }
    Ok(out)
}

/// `obj[key]`.
pub fn get_item(obj: &Value, key: &Value) -> Result<Value> {
    match (obj, key) {
        (Value::List(v) | Value::Tuple(v), Value::Slice(s)) => {
            let items: Vec<Value> = slice_indices(s, v.len())?
                .into_iter()
                .map(|i| v[i].clone())
                .collect();
            Ok(if matches!(obj, Value::List(_)) {
                Value::list(items)
            } else {
                Value::Tuple(Arc::new(items))
            })
        }
        (Value::Str(st) | Value::Markup(st), Value::Slice(s)) => {
            let chars: Vec<char> = st.chars().collect();
            let r: String = slice_indices(s, chars.len())?
                .into_iter()
                .map(|i| chars[i])
                .collect();
            Ok(if matches!(obj, Value::Markup(_)) {
                Value::markup(&r)
            } else {
                Value::str(&r)
            })
        }
        (Value::List(v) | Value::Tuple(v), k) => {
            let i = k.as_i64().ok_or_else(|| {
                Error::type_error(format!(
                    "list indices must be integers or slices, not {}",
                    k.type_name()
                ))
            })?;
            norm_index(i, v.len())
                .map(|i| v[i].clone())
                .ok_or_else(|| Error::other("IndexError: list index out of range"))
        }
        (Value::Str(s), k) => {
            let i = k
                .as_i64()
                .ok_or_else(|| Error::type_error("string indices must be integers"))?;
            let c = if i >= 0 {
                s.chars().nth(i as usize)
            } else {
                norm_index(i, s.chars().count()).and_then(|i| s.chars().nth(i))
            };
            c.map(|c| Value::str(c.encode_utf8(&mut [0; 4])))
                .ok_or_else(|| Error::other("IndexError: string index out of range"))
        }
        (Value::Dict(d), k) => d
            .get(&k.hkey()?)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| Error::key(k.repr())),
        (Value::Lazy(l), k) => get_item(&Value::markup(l.render()?), k),
        (o, _) => Err(Error::type_error(format!(
            "'{}' object is not subscriptable",
            o.type_name()
        ))),
    }
}

const STR_METHODS: &[&str] = &[
    "upper",
    "lower",
    "strip",
    "lstrip",
    "rstrip",
    "split",
    "join",
    "replace",
    "startswith",
    "endswith",
    "title",
    "capitalize",
    "format",
    "count",
    "find",
    "isdigit",
    "zfill",
];
const DICT_METHODS: &[&str] = &["get", "items", "keys", "values"];
const LIST_METHODS: &[&str] = &["index", "count"];

/// `obj.name`: object attributes and the supported str, dict and list
/// methods (as bound [`Value::Method`]s).
pub fn get_attr(obj: &Value, name: &Arc<str>) -> Result<Value> {
    let name_str: &str = name;
    let method = || Ok(Value::Method(Arc::new((obj.clone(), name.clone()))));
    match obj {
        Value::Object(o) => o
            .get(name_str)
            .cloned()
            .ok_or_else(|| attribute_error("object", name)),
        Value::Str(_) | Value::Markup(_) if STR_METHODS.contains(&name_str) => method(),
        Value::Dict(_) if DICT_METHODS.contains(&name_str) => method(),
        Value::List(_) | Value::Tuple(_) if LIST_METHODS.contains(&name_str) => method(),
        Value::Lazy(l) => get_attr(&Value::markup(l.render()?), name),
        other => Err(attribute_error(other.type_name(), name)),
    }
}

fn arg(args: &[Value], i: usize, fname: &str) -> Result<Value> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::type_error(format!("{fname}() missing required argument")))
}

fn as_str(v: &Value) -> Result<&str> {
    match v {
        Value::Str(s) | Value::Markup(s) => Ok(s),
        other => Err(Error::type_error(format!(
            "expected str, got {}",
            other.type_name()
        ))),
    }
}

/// Call the method `name` of `recv` (see [`get_attr`]).
pub fn call_method(recv: &Value, name: &str, args: &[Value]) -> Result<Value> {
    let markup = matches!(recv, Value::Markup(_));
    let wrap = |s: String| {
        if markup {
            Value::markup(&s)
        } else {
            Value::str(&s)
        }
    };
    match recv {
        Value::Str(s) | Value::Markup(s) => Ok(match name {
            "upper" => wrap(s.to_uppercase()),
            "lower" => wrap(s.to_lowercase()),
            "strip" => wrap(match args.first() {
                Some(c) => s
                    .trim_matches(|x| as_str(c).is_ok_and(|c| c.contains(x)))
                    .to_owned(),
                None => s.trim().to_owned(),
            }),
            "lstrip" => wrap(s.trim_start().to_owned()),
            "rstrip" => wrap(s.trim_end().to_owned()),
            "title" => wrap(
                s.split(' ')
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            Some(f) => {
                                f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase()
                            }
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            "capitalize" => wrap({
                let mut c = s.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                    None => String::new(),
                }
            }),
            "split" => {
                let parts: Vec<Value> = match args.first() {
                    Some(Value::None) | None => {
                        s.split_whitespace().map(|p| wrap(p.to_owned())).collect()
                    }
                    Some(sep) => s.split(as_str(sep)?).map(|p| wrap(p.to_owned())).collect(),
                };
                Value::list(parts)
            }
            "join" => {
                let items = arg(args, 0, "join")?.iter_items()?;
                let mut parts = Vec::with_capacity(items.len());
                for i in items {
                    parts.push(if markup {
                        escape_value(&i)?.to_string()
                    } else {
                        as_str(&i)?.to_owned()
                    });
                }
                wrap(parts.join(s))
            }
            "replace" => wrap(s.replace(
                as_str(&arg(args, 0, "replace")?)?,
                as_str(&arg(args, 1, "replace")?)?,
            )),
            "startswith" => Value::Bool(s.starts_with(as_str(&arg(args, 0, "startswith")?)?)),
            "endswith" => Value::Bool(s.ends_with(as_str(&arg(args, 0, "endswith")?)?)),
            "count" => Value::Int(s.matches(as_str(&arg(args, 0, "count")?)?).count() as i64),
            "find" => {
                let needle_v = arg(args, 0, "find")?;
                let needle = as_str(&needle_v)?;
                Value::Int(s.find(needle).map_or(-1, |b| s[..b].chars().count() as i64))
            }
            "isdigit" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())),
            "zfill" => {
                let w = arg(args, 0, "zfill")?.as_i64().unwrap_or(0).max(0) as usize;
                let n = s.chars().count();
                wrap(if n >= w {
                    s.to_string()
                } else {
                    "0".repeat(w - n) + s
                })
            }
            "format" => {
                let mut out = String::new();
                let mut idx = 0;
                let mut rest = &s[..];
                while let Some(p) = rest.find('{') {
                    out.push_str(&rest[..p]);
                    if rest[p..].starts_with("{{") {
                        out.push('{');
                        rest = &rest[p + 2..];
                        continue;
                    }
                    let end = rest[p..]
                        .find('}')
                        .ok_or_else(|| Error::value("Single '{' encountered in format string"))?
                        + p;
                    let field = &rest[p + 1..end];
                    let v = if field.is_empty() {
                        idx += 1;
                        arg(args, idx - 1, "format")?
                    } else {
                        arg(
                            args,
                            field
                                .parse()
                                .map_err(|_| Error::value("unsupported format field"))?,
                            "format",
                        )?
                    };
                    out.push_str(&if markup {
                        escape_value(&v)?.to_string()
                    } else {
                        v.to_str()?
                    });
                    rest = &rest[end + 1..];
                }
                out.push_str(&rest.replace("}}", "}"));
                wrap(out)
            }
            _ => return Err(attribute_error(recv.type_name(), name)),
        }),
        Value::Dict(d) => Ok(match name {
            "get" => {
                let k = arg(args, 0, "get")?;
                d.get(&k.hkey()?).map_or_else(
                    || args.get(1).cloned().unwrap_or(Value::None),
                    |(_, v)| v.clone(),
                )
            }
            "items" => Value::list(
                d.values()
                    .map(|(k, v)| Value::Tuple(Arc::new(vec![k.clone(), v.clone()])))
                    .collect(),
            ),
            "keys" => Value::list(d.values().map(|(k, _)| k.clone()).collect()),
            "values" => Value::list(d.values().map(|(_, v)| v.clone()).collect()),
            _ => return Err(attribute_error("dict", name)),
        }),
        Value::List(v) | Value::Tuple(v) => Ok(match name {
            "index" => {
                let x = arg(args, 0, "index")?;
                Value::Int(
                    v.iter()
                        .position(|i| i.py_eq(&x))
                        .ok_or_else(|| Error::value("x not in list"))? as i64,
                )
            }
            "count" => {
                let x = arg(args, 0, "count")?;
                Value::Int(v.iter().filter(|i| i.py_eq(&x)).count() as i64)
            }
            _ => return Err(attribute_error(recv.type_name(), name)),
        }),
        _ => Err(attribute_error(recv.type_name(), name)),
    }
}

fn minmax(args: &[Value], max: bool) -> Result<Value> {
    let items = if args.len() == 1 {
        args[0].iter_items()?
    } else {
        args.to_vec()
    };
    let mut it = items.into_iter();
    let mut best = it
        .next()
        .ok_or_else(|| Error::value("min/max() arg is an empty sequence"))?;
    for x in it {
        let ord = x.py_cmp(&best)?;
        if (max && ord == Ordering::Greater) || (!max && ord == Ordering::Less) {
            best = x;
        }
    }
    Ok(best)
}

fn to_int(v: &Value) -> Result<Value> {
    Ok(match v {
        Value::Bool(b) => Value::Int(i64::from(*b)),
        Value::Int(i) => Value::Int(*i),
        Value::Float(f) => Value::Int(f.trunc() as i64),
        Value::Str(s) => Value::Int(qweb_core::compile::py_int(s).ok_or_else(|| {
            Error::value(format!(
                "invalid literal for int() with base 10: {}",
                v.repr()
            ))
        })?),
        other => {
            return Err(Error::type_error(format!(
                "int() argument must be a string or a number, not '{}'",
                other.type_name()
            )));
        }
    })
}

/// Python `round()`: correctly rounded on the exact binary value, ties to even.
fn py_round(f: f64, ndigits: i32) -> f64 {
    if ndigits >= 0 {
        format!("{:.*}", ndigits as usize, f).parse().unwrap_or(f)
    } else {
        let m = 10f64.powi(-ndigits);
        format!("{:.0}", f / m).parse::<f64>().unwrap_or(f) * m
    }
}

/// Pairs of a dict, or of an iterable of 2-item iterables (`dict(v)`).
pub(crate) fn dict_pairs(v: &Value) -> Result<Vec<(Value, Value)>> {
    if let Value::Dict(d) = v {
        return Ok(d.values().cloned().collect());
    }
    let items = v.iter_items()?;
    let mut pairs = Vec::with_capacity(items.len());
    for item in items {
        let kv = item.iter_items()?;
        let [k, v] = <[Value; 2]>::try_from(kv)
            .map_err(|_| Error::value("dictionary update sequence element has wrong length"))?;
        pairs.push((k, v));
    }
    Ok(pairs)
}

/// Call a builtin function.
// TODO: take `args: &[Value]` in the next breaking release (0.2): the
// function never consumes the vector. Changing it now would break the API.
#[allow(
    clippy::needless_pass_by_value,
    reason = "public API of 0.1; see the NOTE above"
)]
pub fn call_builtin(b: Builtin, args: Vec<Value>) -> Result<Value> {
    use Builtin::{
        Abs, All, Any, Bool, Chr, Dict, Divmod, Enumerate, Float, Int, Len, List, Max, Min, Ord,
        Range, Repr, Round, Set, Sorted, Str, Sum, Tuple, Zip,
    };
    let args = args.as_slice();
    let a0 = || arg(args, 0, b.name());
    Ok(match b {
        Len => Value::Int(a0()?.py_len()? as i64),
        Str => match args.first() {
            None => Value::str(""),
            Some(v) => Value::Str(Arc::from(v.to_str()?)),
        },
        Int => match args.first() {
            None => Value::Int(0),
            Some(v) => to_int(v)?,
        },
        Float => match args.first() {
            None => Value::Float(0.0),
            Some(Value::Str(s)) => Value::Float(s.trim().parse().map_err(|_| {
                Error::value(format!(
                    "could not convert string to float: {}",
                    str_repr(s)
                ))
            })?),
            Some(v) => Value::Float(v.as_f64().ok_or_else(|| {
                Error::type_error("float() argument must be a string or a real number")
            })?),
        },
        Bool => Value::Bool(match args.first() {
            None => false,
            Some(v) => v.truthy()?,
        }),
        Abs => match a0()? {
            Value::Float(f) => Value::Float(f.abs()),
            v => Value::Int(
                v.as_i64()
                    .ok_or_else(|| Error::type_error("bad operand type for abs()"))?
                    .abs(),
            ),
        },
        Min => minmax(args, false)?,
        Max => minmax(args, true)?,
        Sum => {
            let mut total = args.get(1).cloned().unwrap_or(Value::Int(0));
            for x in a0()?.iter_items()? {
                total = binary(BinOp::Add, &total, &x)?;
            }
            total
        }
        Sorted => {
            let mut items = a0()?.iter_items()?;
            let mut err = None;
            items.sort_by(|x, y| match x.py_cmp(y) {
                Ok(o) => o,
                Err(e) => {
                    err.get_or_insert(e);
                    Ordering::Equal
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
            Value::list(items)
        }
        Range => {
            let ints: Vec<i64> = args
                .iter()
                .map(|v| {
                    v.as_i64().ok_or_else(|| {
                        Error::type_error("'float' object cannot be interpreted as an integer")
                    })
                })
                .collect::<Result<_>>()?;
            let (start, stop, step) = match ints.len() {
                1 => (0, ints[0], 1),
                2 => (ints[0], ints[1], 1),
                3 => (ints[0], ints[1], ints[2]),
                _ => return Err(Error::type_error("range expected at most 3 arguments")),
            };
            if step == 0 {
                return Err(Error::value("range() arg 3 must not be zero"));
            }
            let mut out = Vec::new();
            let mut i = start;
            while (step > 0 && i < stop) || (step < 0 && i > stop) {
                out.push(Value::Int(i));
                i += step;
            }
            Value::list(out)
        }
        Enumerate => {
            let start = args.get(1).and_then(Value::as_i64).unwrap_or(0);
            Value::list(
                a0()?
                    .iter_items()?
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| Value::Tuple(Arc::new(vec![Value::Int(i as i64 + start), v])))
                    .collect(),
            )
        }
        Zip => {
            let lists: Vec<Vec<Value>> =
                args.iter().map(Value::iter_items).collect::<Result<_>>()?;
            let n = lists.iter().map(Vec::len).min().unwrap_or(0);
            Value::list(
                (0..n)
                    .map(|i| Value::Tuple(Arc::new(lists.iter().map(|l| l[i].clone()).collect())))
                    .collect(),
            )
        }
        List => Value::list(match args.first() {
            None => Vec::new(),
            Some(v) => v.iter_items()?,
        }),
        Tuple => Value::Tuple(Arc::new(match args.first() {
            None => Vec::new(),
            Some(v) => v.iter_items()?,
        })),
        Set => {
            let mut out: Vec<Value> = Vec::new();
            if let Some(v) = args.first() {
                for x in v.iter_items()? {
                    if !out.iter().any(|y| y.py_eq(&x)) {
                        out.push(x);
                    }
                }
            }
            Value::Set(Arc::new(out))
        }
        Dict => match args.first() {
            None => Value::dict_from(Vec::new())?,
            Some(Value::Dict(d)) => Value::Dict(d.clone()),
            Some(v) => Value::dict_from(dict_pairs(v)?)?,
        },
        Round => {
            let v = a0()?;
            match args.get(1) {
                None | Some(Value::None) => match v {
                    Value::Float(f) => Value::Int(py_round(f, 0) as i64),
                    v => v,
                },
                Some(n) => match v {
                    Value::Float(f) => Value::Float(py_round(f, n.as_i64().unwrap_or(0) as i32)),
                    v => v,
                },
            }
        }
        Repr => Value::str(&a0()?.repr()),
        // Short-circuit like Python: items after the deciding one are not
        // evaluated (a lazy item would be rendered).
        Any => {
            let mut r = false;
            for x in a0()?.iter_items()? {
                if x.truthy()? {
                    r = true;
                    break;
                }
            }
            Value::Bool(r)
        }
        All => {
            let mut r = true;
            for x in a0()?.iter_items()? {
                if !x.truthy()? {
                    r = false;
                    break;
                }
            }
            Value::Bool(r)
        }
        Ord => {
            let s = a0()?;
            let s = as_str(&s)?;
            let mut c = s.chars();
            match (c.next(), c.next()) {
                (Some(ch), None) => Value::Int(ch as i64),
                _ => return Err(Error::type_error("ord() expected a character")),
            }
        }
        Chr => {
            let i = a0()?
                .as_i64()
                .ok_or_else(|| Error::type_error("an integer is required"))?;
            Value::str(
                char::from_u32(i as u32)
                    .ok_or_else(|| Error::value("chr() arg not in range(0x110000)"))?
                    .encode_utf8(&mut [0; 4]),
            )
        }
        Divmod => {
            let (a, b2) = (a0()?, arg(args, 1, "divmod")?);
            Value::Tuple(Arc::new(vec![
                binary(BinOp::FloorDiv, &a, &b2)?,
                binary(BinOp::Mod, &a, &b2)?,
            ]))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(b: &str, args: &[Value]) -> Result<Value> {
        call_builtin(Builtin::from_name(b).unwrap(), args.to_vec())
    }

    #[test]
    fn builtin_names() {
        for name in ["len", "str", "range", "divmod", "sorted", "any"] {
            assert_eq!(Builtin::from_name(name).unwrap().name(), name);
        }
        assert_eq!(Builtin::from_name("xrange"), Some(Builtin::Range));
        let err = call("len", &[]).unwrap_err();
        assert!(err.message().contains("len()"), "{}", err.message());
    }

    #[test]
    fn nan_comparisons() {
        let nan = Value::Float(f64::NAN);
        for op in [CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge, CmpOp::Eq] {
            assert!(!compare(op, &nan, &Value::Int(1)).unwrap().truthy().unwrap());
        }
        assert!(compare(CmpOp::Ne, &nan, &nan).unwrap().truthy().unwrap());
        assert!(
            compare(CmpOp::Lt, &Value::Bool(true), &Value::Float(1.5))
                .unwrap()
                .truthy()
                .unwrap()
        );
        assert!(
            compare(CmpOp::Ge, &Value::Int(3), &Value::Int(3))
                .unwrap()
                .truthy()
                .unwrap()
        );
    }

    #[test]
    fn any_all() {
        let l = |v: Vec<Value>| Value::list(v);
        assert!(
            call("any", &[l(vec![Value::Int(0), Value::str("x")])])
                .unwrap()
                .truthy()
                .unwrap()
        );
        assert!(!call("any", &[l(vec![])]).unwrap().truthy().unwrap());
        assert!(call("all", &[l(vec![])]).unwrap().truthy().unwrap());
        assert!(
            !call("all", &[l(vec![Value::Int(1), Value::None])])
                .unwrap()
                .truthy()
                .unwrap()
        );
    }

    #[test]
    fn str_index() {
        let s = Value::str("héllo");
        let at = |i| get_item(&s, &Value::Int(i)).map(|v| v.to_str().unwrap());
        assert_eq!(at(1).unwrap(), "é");
        assert_eq!(at(-1).unwrap(), "o");
        assert_eq!(at(-5).unwrap(), "h");
        assert!(at(5).is_err());
        assert!(at(-6).is_err());
    }

    #[test]
    fn dicts() {
        // A later duplicate key keeps the first key and its position.
        let d = Value::dict_from(vec![
            (Value::Int(1), Value::str("a")),
            (Value::str("k"), Value::Int(2)),
            (Value::Bool(true), Value::str("b")),
        ])
        .unwrap();
        assert_eq!(d.repr(), "{1: 'b', 'k': 2}");
        let pairs = Value::list(vec![Value::Tuple(Arc::new(vec![
            Value::str("a"),
            Value::Int(1),
        ]))]);
        assert_eq!(call("dict", &[pairs]).unwrap().repr(), "{'a': 1}");
        let bad = Value::list(vec![Value::list(vec![Value::Int(1)])]);
        assert!(call("dict", &[bad]).is_err());
    }

    #[test]
    fn reprs() {
        let t1 = Value::Tuple(Arc::new(vec![Value::Float(0.1)]));
        assert_eq!(t1.repr(), "(0.1,)");
        let v = Value::list(vec![
            Value::None,
            Value::Bool(false),
            Value::Float(1e16),
            Value::Float(1e17),
            Value::Float(-0.0),
            Value::Float(1.5e-5),
            Value::markup("<b>"),
            Value::Bytes(Arc::from(&b"a'\x00"[..])),
            Value::Bytes(Arc::from(&b"'\""[..])),
            Value::Set(Arc::new(vec![])),
            t1,
            Value::object([("x", Value::Int(1))]),
            Value::Builtin(Builtin::Len),
        ]);
        assert_eq!(
            v.repr(),
            "[None, False, 1e+16, 1e+17, -0.0, 1.5e-05, Markup('<b>'), b\"a\'\\x00\", b'\\'\"', set(), (0.1,), \
             namespace(x=1), <built-in function len>]"
        );
        assert_eq!(Value::Int(-7).to_str().unwrap(), "-7");
    }
}
