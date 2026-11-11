//! Detection of `javascript:` URLs in dynamic `href` attributes.
//!
//! Port of the check in `IrQweb._post_processing_att`:
//! `MALICIOUS_SCHEMES.findall(WHITESPACE_REGEX.sub('', unquote_plus(href)))`.

/// `WHITESPACE_REGEX = re.compile(r'[\s\x00-\x08\x0B\x0C\x0E-\x19]+')`.
#[inline]
fn is_ws(c: char) -> bool {
    let u = c as u32;
    u <= 0x19 || (0x1c..=0x20).contains(&u) || c.is_whitespace()
}

/// Append the characters of `s` that are not whitespace.
fn push_non_ws(out: &mut String, s: &str) {
    out.extend(s.chars().filter(|&c| !is_ws(c)));
}

/// `WHITESPACE_REGEX.sub('', urllib.parse.unquote_plus(s))` in one pass.
///
/// `unquote_plus` decodes runs of `%XX` escapes as UTF-8 with
/// `errors='replace'`; `+` becomes a space, which is then removed with the
/// rest of the whitespace.
fn decode_href(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |i: usize| b.get(i).and_then(|&c| (c as char).to_digit(16));
    let mut out = String::with_capacity(s.len());
    // Bytes of the current run of `%XX` escapes, decoded together.
    let mut pending: Vec<u8> = Vec::new();
    let flush = |out: &mut String, pending: &mut Vec<u8>| {
        if !pending.is_empty() {
            push_non_ws(out, &String::from_utf8_lossy(pending));
            pending.clear();
        }
    };
    // Start of the current run of literal text. At most one of `pending`
    // and `s[lit..i]` is non-empty at any time.
    let mut lit = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (hex(i + 1), hex(i + 2))
        {
            push_non_ws(&mut out, &s[lit..i]);
            pending.push((h * 16 + l) as u8);
            i += 3;
            lit = i;
        } else if b[i] == b'+' {
            // A space: it ends the escape run and is then removed.
            flush(&mut out, &mut pending);
            push_non_ws(&mut out, &s[lit..i]);
            i += 1;
            lit = i;
        } else {
            flush(&mut out, &mut pending);
            i += 1;
        }
    }
    flush(&mut out, &mut pending);
    push_non_ws(&mut out, &s[lit..]);
    out
}

/// Case-insensitive char comparison mimicking Python's `re.IGNORECASE` for
/// the ASCII letters used in the pattern (including the non-ASCII characters
/// that Unicode case folding maps onto them).
const fn ci_eq(c: char, p: char) -> bool {
    if c.to_ascii_lowercase() == p {
        return true;
    }
    match p {
        's' => c == '\u{17f}',
        'k' => c == '\u{212a}',
        'i' => c == '\u{130}' || c == '\u{131}',
        _ => false,
    }
}

/// If `s` starts with `pat` (compared with [`ci_eq`]), return the rest of `s`.
fn strip_prefix_ci<'a>(s: &'a str, pat: &str) -> Option<&'a str> {
    let mut chars = s.chars();
    for p in pat.chars() {
        match chars.next() {
            Some(c) if ci_eq(c, p) => {}
            _ => return None,
        }
    }
    Some(chars.as_str())
}

/// Whether a dynamic `href` value must be neutralised: it contains
/// `javascript:(?!((window\.)?)history\.back\(\)$)` (case-insensitive) once
/// percent-decoded and stripped of whitespace.
///
/// ```
/// use qweb_core::url::is_malicious_href;
/// assert!(is_malicious_href("JaVa\tScRiPt:alert(1)"));
/// assert!(!is_malicious_href("javascript:history.back()"));
/// assert!(!is_malicious_href("/web#action=1"));
/// ```
pub fn is_malicious_href(href: &str) -> bool {
    const JAVASCRIPT: &str = "javascript:";
    // Fast path: the scheme needs a ':', which may be percent-encoded.
    if memchr::memchr2(b':', b'%', href.as_bytes()).is_none() {
        return false;
    }
    let decoded = decode_href(href);
    let mut rest = decoded.as_str();
    while !rest.is_empty() {
        if let Some(after) = strip_prefix_ci(rest, JAVASCRIPT) {
            let is_back = |pat: &str| strip_prefix_ci(after, pat).is_some_and(str::is_empty);
            if !(is_back("history.back()") || is_back("window.history.back()")) {
                return true;
            }
            rest = after;
        } else {
            let mut chars = rest.chars();
            chars.next();
            rest = chars.as_str();
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hrefs() {
        assert!(is_malicious_href("javascript:alert(1)"));
        assert!(is_malicious_href("JaVa\tScRiPt:alert(1)"));
        assert!(is_malicious_href("java%73cript:alert(1)"));
        assert!(is_malicious_href("java+script:alert(1)"));
        assert!(is_malicious_href("java%20script%3Aalert(1)"));
        assert!(is_malicious_href("x javascript:a"));
        assert!(is_malicious_href("javaſcript:a"));
        assert!(!is_malicious_href("javascript:history.back()"));
        assert!(!is_malicious_href("javascript:window.history.back()"));
        assert!(!is_malicious_href("javascript: history.back( )"));
        assert!(is_malicious_href("javascript:history.back();x"));
        assert!(!is_malicious_href("/web#action=1"));
        assert!(!is_malicious_href(""));
    }

    #[test]
    fn decode() {
        assert_eq!(decode_href("a+b%20c%e9%"), "abc\u{fffd}%");
        assert_eq!(decode_href("%c3%a9t%C3%A9"), "été");
        assert_eq!(decode_href("%41%"), "A%");
        assert_eq!(decode_href("%4"), "%4");
        assert_eq!(decode_href("%c3+%a9"), "\u{fffd}\u{fffd}");
        assert_eq!(decode_href("%c3x%a9"), "\u{fffd}x\u{fffd}");
        assert_eq!(decode_href("a\u{a0}b"), "ab");
    }
}
