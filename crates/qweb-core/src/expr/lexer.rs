//! Tokenizer for the Python expression dialect used in QWeb templates.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Name(String),
    /// Integer literal, normalised to decimal digits when it fits in i64,
    /// otherwise kept as the original source text (big integers).
    Int(i64),
    BigInt(String),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Op(&'static str),
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LexError {
    /// Construct valid in Python that the native parser does not handle
    /// (f-strings, imaginary numbers, `\N{...}` escapes, ...).
    Unsupported(String),
    Syntax(String),
}

const OPS3: [&str; 5] = ["**=", "//=", ">>=", "<<=", "..."];
const OPS2: [&str; 19] = [
    "**", "//", "<<", ">>", "<=", ">=", "==", "!=", "->", ":=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "@=",
];
const OPS1: [&str; 23] = [
    "+", "-", "*", "/", "%", "@", "&", "|", "^", "~", "<", ">", "(", ")", "[", "]", "{", "}", ",", ":", ".", ";",
    "=",
];

pub fn tokenize(src: &str) -> Result<Vec<Tok>, LexError> {
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
            let start = i;
            let rest = &src[i..];
            let mut end = start;
            for (off, ch) in rest.char_indices() {
                if ch == '_' || ch.is_alphanumeric() {
                    end = start + off + ch.len_utf8();
                } else {
                    break;
                }
            }
            if end == start {
                return Err(LexError::Syntax(format!("invalid character {:?}", rest.chars().next().unwrap())));
            }
            toks.push(Tok::Name(src[start..end].to_owned()));
            i = end;
            continue;
        }
        let rest = &src[i..];
        if let Some(op) = OPS3.iter().find(|op| rest.starts_with(**op)) {
            toks.push(Tok::Op(op));
            i += 3;
            continue;
        }
        if let Some(op) = OPS2.iter().find(|op| rest.starts_with(**op)) {
            toks.push(Tok::Op(op));
            i += 2;
            continue;
        }
        if let Some(op) = OPS1.iter().find(|op| rest.starts_with(**op)) {
            toks.push(Tok::Op(op));
            i += 1;
            continue;
        }
        return Err(LexError::Syntax(format!("invalid character {:?}", rest.chars().next().unwrap())));
    }
    toks.push(Tok::End);
    Ok(toks)
}

fn number(src: &str, start: usize) -> Result<(Tok, usize), LexError> {
    let b = src.as_bytes();
    let mut i = start;
    if b[i] == b'0' && i + 1 < b.len() && matches!(b[i + 1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
        let radix = match b[i + 1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            _ => 2,
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
        let v: f64 = text.parse().map_err(|_| LexError::Syntax("invalid float literal".into()))?;
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

/// Try to lex a string literal at `i`. Returns None if there is no string here.
fn try_string(src: &str, i: usize) -> Result<Option<(Tok, usize)>, LexError> {
    let b = src.as_bytes();
    // prefix: up to 2 letters among r, b, u, f
    let mut j = i;
    while j < b.len() && j - i < 2 && matches!(b[j].to_ascii_lowercase(), b'r' | b'b' | b'u' | b'f') {
        j += 1;
    }
    if j >= b.len() || (b[j] != b'\'' && b[j] != b'"') {
        return Ok(None);
    }
    let prefix = src[i..j].to_ascii_lowercase();
    let valid = matches!(prefix.as_str(), "" | "r" | "u" | "b" | "br" | "rb" | "f" | "fr" | "rf");
    if !valid {
        return Ok(None);
    }
    let raw = prefix.contains('r');
    let bytes = prefix.contains('b');
    let fstr = prefix.contains('f');
    let q = b[j];
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
    if fstr {
        return Err(LexError::Unsupported("f-string".into()));
    }
    let body = &src[body_start..end];
    if bytes {
        if !body.is_ascii() {
            return Err(LexError::Syntax("bytes can only contain ASCII literal characters".into()));
        }
        let v = if raw { body.as_bytes().to_vec() } else { unescape_bytes(body)? };
        return Ok(Some((Tok::Bytes(v), k)));
    }
    let s = if raw { body.to_owned() } else { unescape_str(body)? };
    Ok(Some((Tok::Str(s), k)))
}

fn hex_val(s: &str) -> Result<u32, LexError> {
    u32::from_str_radix(s, 16).map_err(|_| LexError::Syntax("truncated escape".into()))
}

fn unescape_str(body: &str) -> Result<String, LexError> {
    if !body.contains('\\') {
        return Ok(body.to_owned());
    }
    let mut out = String::with_capacity(body.len());
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
                let mut v = e.to_digit(8).unwrap();
                for _ in 0..2 {
                    match chars.peek() {
                        Some(&(_, d)) if d.is_digit(8) => {
                            v = v * 8 + d.to_digit(8).unwrap();
                            chars.next();
                        }
                        _ => break,
                    }
                }
                out.push(char::from_u32(v).ok_or_else(|| LexError::Syntax("bad escape".into()))?);
            }
            'x' | 'u' | 'U' => {
                let n = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let start = idx + 2;
                let hex = body.get(start..start + n).ok_or_else(|| LexError::Syntax("truncated escape".into()))?;
                if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(LexError::Syntax("truncated escape".into()));
                }
                let v = hex_val(hex)?;
                out.push(char::from_u32(v).ok_or_else(|| LexError::Syntax("illegal Unicode character".into()))?);
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
                let mut v = (e - b'0') as u32;
                let mut n = 0;
                while n < 2 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                    v = v * 8 + (b[i] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                out.push((v & 0xff) as u8);
            }
            b'x' => {
                let hex = body.get(i..i + 2).ok_or_else(|| LexError::Syntax("truncated escape".into()))?;
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
        assert_eq!(tokenize("99999999999999999999").unwrap()[0], Tok::BigInt("99999999999999999999".into()));
    }
}
