//! Format strings (`t-attf-*`, `t-valuef`, `t-call`, `name.f`).
//!
//! Equivalent to `FORMAT_REGEX = re.compile(r'(?:#\{(.+?)\})|(?:\{\{(.+?)\}\})')`.

#[derive(Debug, Clone, PartialEq)]
pub enum FormatPart<'a> {
    Lit(&'a str),
    Expr(&'a str),
}

/// Split a format string into literal and expression parts.
pub fn split_format(s: &str) -> Vec<FormatPart<'_>> {
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut lit_start = 0;
    let mut i = 0;
    while i + 1 < b.len() {
        let found = if b[i] == b'#' && b[i + 1] == b'{' {
            find_close(b, i + 2, false).map(|k| (i + 2, k, k + 1))
        } else if b[i] == b'{' && b[i + 1] == b'{' {
            find_close(b, i + 2, true).map(|k| (i + 2, k, k + 2))
        } else {
            None
        };
        if let Some((es, ee, end)) = found {
            if lit_start < i {
                parts.push(FormatPart::Lit(&s[lit_start..i]));
            }
            parts.push(FormatPart::Expr(&s[es..ee]));
            i = end;
            lit_start = end;
        } else {
            i += 1;
        }
    }
    if lit_start < b.len() {
        parts.push(FormatPart::Lit(&s[lit_start..]));
    }
    parts
}

/// Find the end of `.+?` followed by `}` (or `}}`), with no newline inside.
fn find_close(b: &[u8], start: usize, double: bool) -> Option<usize> {
    let mut k = start + 1; // at least one char (any byte position > start is fine: '}' is ASCII)
    if start >= b.len() || b[start] == b'\n' {
        return None;
    }
    while k < b.len() {
        if b[k] == b'}' && (!double || (k + 1 < b.len() && b[k + 1] == b'}')) {
            return Some(k);
        }
        if b[k] == b'\n' {
            return None;
        }
        k += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use FormatPart::*;
    #[test]
    fn split() {
        assert_eq!(split_format("a #{b} c {{d}}"), vec![Lit("a "), Expr("b"), Lit(" c "), Expr("d")]);
        assert_eq!(split_format("#{}}"), vec![Expr("}")]);
        assert_eq!(split_format("{{a}"), vec![Lit("{{a}")]);
        assert_eq!(split_format("plain"), vec![Lit("plain")]);
        assert_eq!(split_format("{{ {'a': 1}['a'] }}"), vec![Expr(" {'a': 1}['a'] ")]);
    }
}
