//! Tokenizer for the Python expression dialect used in `QWeb` templates.

/// A token. Keywords are [`Tok::Name`]s; the parser tells them apart.
#[derive(Debug, PartialEq)]
pub(crate) enum Tok {
    /// Identifier or keyword.
    Name(String),
    /// Integer literal that fits in an `i64`.
    Int(i64),
    /// Decimal integer literal too large for an `i64`: its digits, without
    /// `_` separators and leading zeros.
    BigInt(String),
    /// Float literal.
    Float(f64),
    /// String literal (adjacent literals are concatenated by the parser).
    Str(String),
    /// Bytes literal.
    Bytes(Vec<u8>),
    /// Operator or delimiter.
    Op(&'static str),
    /// End of input. Always the last token.
    End,
}

#[cfg_attr(test, derive(Debug))]
pub(crate) enum LexError {
    /// Construct valid in Python that the native parser does not handle
    /// (f-strings, imaginary numbers, `\N{...}` escapes, ...).
    Unsupported(String),
    /// Invalid syntax.
    Syntax(String),
    /// Identifier containing `__`.
    Dunder(String),
}

/// The operator or delimiter at the start of `b` (longest match), if any.
fn operator(b: &[u8]) -> Option<&'static str> {
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    Some(match (at(0), at(1), at(2)) {
        (b'*', b'*', b'=') => "**=",
        (b'/', b'/', b'=') => "//=",
        (b'>', b'>', b'=') => ">>=",
        (b'<', b'<', b'=') => "<<=",
        (b'.', b'.', b'.') => "...",
        (b'*', b'*', _) => "**",
        (b'/', b'/', _) => "//",
        (b'<', b'<', _) => "<<",
        (b'>', b'>', _) => ">>",
        (b'<', b'=', _) => "<=",
        (b'>', b'=', _) => ">=",
        (b'=', b'=', _) => "==",
        (b'!', b'=', _) => "!=",
        (b'-', b'>', _) => "->",
        (b':', b'=', _) => ":=",
        (b'+', b'=', _) => "+=",
        (b'-', b'=', _) => "-=",
        (b'*', b'=', _) => "*=",
        (b'/', b'=', _) => "/=",
        (b'%', b'=', _) => "%=",
        (b'&', b'=', _) => "&=",
        (b'|', b'=', _) => "|=",
        (b'^', b'=', _) => "^=",
        (b'@', b'=', _) => "@=",
        (b'+', _, _) => "+",
        (b'-', _, _) => "-",
        (b'*', _, _) => "*",
        (b'/', _, _) => "/",
        (b'%', _, _) => "%",
        (b'@', _, _) => "@",
        (b'&', _, _) => "&",
        (b'|', _, _) => "|",
        (b'^', _, _) => "^",
        (b'~', _, _) => "~",
        (b'<', _, _) => "<",
        (b'>', _, _) => ">",
        (b'(', _, _) => "(",
        (b')', _, _) => ")",
        (b'[', _, _) => "[",
        (b']', _, _) => "]",
        (b'{', _, _) => "{",
        (b'}', _, _) => "}",
        (b',', _, _) => ",",
        (b':', _, _) => ":",
        (b'.', _, _) => ".",
        (b';', _, _) => ";",
        (b'=', _, _) => "=",
        _ => return None,
    })
}

/// Split `src` into tokens, ending with [`Tok::End`].
///
/// Identifiers containing `__` are rejected here ([`LexError::Dunder`]), as
/// `_compile_expr_tokens` does for every `NAME` token. String bodies are not
/// checked: names inside f-strings are checked by the host when it compiles
/// the expression itself.
pub(crate) fn tokenize(src: &str) -> Result<Vec<Tok>, LexError> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut toks = Vec::new();
    while i < b.len() {
        let c = b[i];
        // whitespace and line continuations (the expression is parenthesised
        // by QWeb, so newlines are insignificant)
        if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c') {
            i += 1;
            continue;
        }
        if c == b'\\' && i + 1 < b.len() && (b[i + 1] == b'\n' || b[i + 1] == b'\r') {
            i += 2;
            continue;
        }
        if c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // strings (with optional prefix)
        if let Some((tok, next)) = try_string(src, i)? {
            toks.push(tok);
            i = next;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
            let (tok, next) = number(src, i)?;
            toks.push(tok);
            i = next;
            continue;
        }
        if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 {
            let rest = &src[i..];
            let len = rest
                .find(|ch: char| ch != '_' && !ch.is_alphanumeric())
                .unwrap_or(rest.len());
            if len == 0 {
                return Err(LexError::Syntax(format!(
                    "invalid character {:?}",
                    rest.chars().next().unwrap_or_default()
                )));
            }
            let name = &rest[..len];
            if name.contains("__") {
                return Err(LexError::Dunder(name.to_owned()));
            }
            toks.push(Tok::Name(name.to_owned()));
            i += len;
            continue;
        }
        let Some(op) = operator(&b[i..]) else {
            return Err(LexError::Syntax(format!(
                "invalid character {:?}",
                src[i..].chars().next().unwrap_or_default()
            )));
        };
        toks.push(Tok::Op(op));
        i += op.len();
    }
    toks.push(Tok::End);
    Ok(toks)
}

fn number(src: &str, start: usize) -> Result<(Tok, usize), LexError> {
    let b = src.as_bytes();
    let mut i = start;
    if b[i] == b'0'
        && i + 1 < b.len()
        && matches!(b[i + 1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
    {
        let radix = match b[i + 1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => unreachable!(),
        };
        i += 2;
        let ds = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        let digits: String = src[ds..i].chars().filter(|&c| c != '_').collect();
        if digits.is_empty() {
            return Err(LexError::Syntax("invalid number literal".into()));
        }
        return match i64::from_str_radix(&digits, radix) {
            Ok(v) => Ok((Tok::Int(v), i)),
            Err(_) => {
                if digits.chars().all(|c| c.is_digit(radix)) {
                    Err(LexError::Unsupported("big non-decimal integer".into()))
                } else {
                    Err(LexError::Syntax("invalid number literal".into()))
                }
            }
        };
    }
    let mut is_float = false;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' && !(i + 1 < b.len() && b[i + 1] == b'.') {
        is_float = true;
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let save = i;
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        if i < b.len() && b[i].is_ascii_digit() {
            is_float = true;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
        } else {
            i = save;
        }
    }
    if i < b.len() && (b[i] == b'j' || b[i] == b'J') {
        return Err(LexError::Unsupported("imaginary literal".into()));
    }
    if i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'_') {
        return Err(LexError::Syntax("invalid decimal literal".into()));
    }
    let text: String = src[start..i].chars().filter(|&c| c != '_').collect();
    if is_float {
        let v: f64 = text
            .parse()
            .map_err(|_| LexError::Syntax("invalid float literal".into()))?;
        return Ok((Tok::Float(v), i));
    }
    if text.len() > 1 && text.starts_with('0') && text.chars().any(|c| c != '0') {
        return Err(LexError::Syntax(
            "leading zeros in decimal integer literals are not permitted".into(),
        ));
    }
    match text.parse::<i64>() {
        Ok(v) => Ok((Tok::Int(v), i)),
        Err(_) => Ok((Tok::BigInt(text.trim_start_matches('0').to_owned()), i)),
    }
}

/// Try to lex a string literal at `i`, returning the token and the position
/// after it.
///
/// `Ok(None)` means that no string literal starts at `i`, so the caller lexes
/// a name instead: `i` is the start of every identifier, and identifiers such
/// as `record` or `bar` begin with prefix letters. An invalid prefix before a
/// quote (`ub'x'`) is also `Ok(None)`: it lexes as a name followed by a
/// string, which the parser rejects, as Python does.
fn try_string(src: &str, i: usize) -> Result<Option<(Tok, usize)>, LexError> {
    let b = src.as_bytes();
    // prefix: up to 2 letters among r, b, u, f
    let mut j = i;
    while j < b.len() && j - i < 2 && matches!(b[j].to_ascii_lowercase(), b'r' | b'b' | b'u' | b'f')
    {
        j += 1;
    }
    if j >= b.len() || (b[j] != b'\'' && b[j] != b'"') {
        return Ok(None);
    }
    let prefix = src[i..j].to_ascii_lowercase();
    let valid = matches!(
        prefix.as_str(),
        "" | "r" | "u" | "b" | "br" | "rb" | "f" | "fr" | "rf"
    );
    if !valid {
        return Ok(None);
    }
    if prefix.contains('f') {
        // Evaluated by the host (`Ex::Opaque`); its tokenizer reports any
        // syntax error in the literal.
        return Err(LexError::Unsupported("f-string".into()));
    }
    let raw = prefix.contains('r');
    let bytes = prefix.contains('b');
    let q = b[j];
    debug_assert!(matches!(q, b'\'' | b'\"'), "quote is not a \' or a \".");
    let triple = j + 2 < b.len() && b[j + 1] == q && b[j + 2] == q;
    let body_start = if triple { j + 3 } else { j + 1 };
    // find the end
    let mut k = body_start;
    let end;
    loop {
        if k >= b.len() {
            return Err(LexError::Syntax("unterminated string literal".into()));
        }
        let c = b[k];
        if c == b'\\' {
            k += 2;
            continue;
        }
        if !triple && c == b'\n' {
            return Err(LexError::Syntax("unterminated string literal".into()));
        }
        if c == q {
            if !triple {
                end = k;
                k += 1;
                break;
            }
            if k + 2 < b.len() && b[k + 1] == q && b[k + 2] == q {
                end = k;
                k += 3;
                break;
            }
        }
        k += 1;
    }
    let body = &src[body_start..end];
    if bytes {
        // `is_ascii` checks a word at a time, so a separate pass is cheaper
        // than a per-byte check in the scan above (which all strings pay).
        if !body.is_ascii() {
            return Err(LexError::Syntax(
                "bytes can only contain ASCII literal characters".into(),
            ));
        }
        let v = if raw {
            body.as_bytes().to_vec()
        } else {
            unescape_bytes(body)?
        };
        return Ok(Some((Tok::Bytes(v), k)));
    }
    let s = if raw {
        body.to_owned()
    } else {
        unescape_str(body)?
    };
    Ok(Some((Tok::Str(s), k)))
}

#[inline]
fn hex_val(s: &str) -> Result<u32, LexError> {
    u32::from_str_radix(s, 16).map_err(|_| LexError::Syntax("truncated escape".into()))
}

fn unescape_str(body: &str) -> Result<String, LexError> {
    // Copy everything before the first backslash at once (memchr), then
    // decode the rest char by char.
    let Some(first) = body.find('\\') else {
        return Ok(body.to_owned());
    };
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..first]);
    let body = &body[first..];
    let mut chars = body.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some((_, e)) = chars.next() else {
            out.push('\\');
            break;
        };
        match e {
            '\n' => {}
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '0'..='7' => {
                let mut v = u32::from(e) - u32::from('0');
                for _ in 0..2 {
                    match chars.peek().and_then(|&(_, d)| d.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(v).ok_or_else(|| LexError::Syntax("bad escape".into()))?);
            }
            'x' | 'u' | 'U' => {
                let n = match e {
                    'x' => 2,
                    'u' => 4,
                    'U' => 8,
                    _ => unreachable!(),
                };
                let start = idx + 2;
                let hex = body
                    .get(start..start + n)
                    .ok_or_else(|| LexError::Syntax("truncated escape".into()))?;
                if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(LexError::Syntax("truncated escape".into()));
                }
                let v = hex_val(hex)?;
                out.push(
                    char::from_u32(v)
                        .ok_or_else(|| LexError::Syntax("illegal Unicode character".into()))?,
                );
                for _ in 0..n {
                    chars.next();
                }
            }
            'N' => return Err(LexError::Unsupported("\\N escape".into())),
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

fn unescape_bytes(body: &str) -> Result<Vec<u8>, LexError> {
    let b = body.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c != b'\\' || i + 1 >= b.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let e = b[i + 1];
        i += 2;
        match e {
            b'\n' => {}
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = u32::from(e - b'0');
                let mut n = 0;
                while n < 2 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                    v = v * 8 + u32::from(b[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push((v & 0xff) as u8);
            }
            b'x' => {
                let hex = body
                    .get(i..i + 2)
                    .ok_or_else(|| LexError::Syntax("truncated escape".into()))?;
                out.push(hex_val(hex)? as u8);
                i += 2;
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic() {
        let t = tokenize("a.b(1, 'x\\n') + 2.5 >= 0x10").unwrap();
        assert_eq!(t[0], Tok::Name("a".into()));
        assert_eq!(t[6], Tok::Str("x\n".into()));
        assert_eq!(t[9], Tok::Float(2.5));
        assert_eq!(t[11], Tok::Int(16));
        assert!(matches!(tokenize("f'x'"), Err(LexError::Unsupported(_))));
        assert_eq!(tokenize("r'\\d'").unwrap()[0], Tok::Str("\\d".into()));
        assert_eq!(tokenize("'''a'b'''").unwrap()[0], Tok::Str("a'b".into()));
        assert_eq!(
            tokenize("99999999999999999999").unwrap()[0],
            Tok::BigInt("99999999999999999999".into())
        );
        assert_eq!(tokenize("'ab\\tc'").unwrap()[0], Tok::Str("ab\tc".into()));
        assert_eq!(
            tokenize("'é\\x41\\u00e9'").unwrap()[0],
            Tok::Str("éAé".into())
        );
        assert!(matches!(tokenize("a.__class__"), Err(LexError::Dunder(n)) if n == "__class__"));
        assert!(tokenize("'__x'").is_ok());
        assert!(matches!(tokenize("f'x"), Err(LexError::Unsupported(_))));
    }

    /// `operator` gives the same result as a longest-match search in the
    /// operator lists of the Python tokenizer.
    #[test]
    fn operators() {
        const OPS3: [&str; 5] = ["**=", "//=", ">>=", "<<=", "..."];
        const OPS2: [&str; 19] = [
            "**", "//", "<<", ">>", "<=", ">=", "==", "!=", "->", ":=", "+=", "-=", "*=", "/=",
            "%=", "&=", "|=", "^=", "@=",
        ];
        const OPS1: [&str; 23] = [
            "+", "-", "*", "/", "%", "@", "&", "|", "^", "~", "<", ">", "(", ")", "[", "]", "{",
            "}", ",", ":", ".", ";", "=",
        ];
        let reference = |rest: &str| {
            OPS3.iter()
                .chain(OPS2.iter())
                .chain(OPS1.iter())
                .find(|op| rest.starts_with(**op))
                .copied()
        };
        let alphabet: Vec<char> = "+-*/%@&|^~<>()[]{},:.;=!a ".chars().collect();
        let mut buf = String::new();
        for &a in &alphabet {
            for &b in &alphabet {
                for &c in &alphabet {
                    for len in 1..=3 {
                        buf.clear();
                        buf.extend([a, b, c].iter().take(len));
                        assert_eq!(operator(buf.as_bytes()), reference(&buf), "{buf:?}");
                    }
                }
            }
        }
        assert_eq!(operator(b""), None);
    }
}
