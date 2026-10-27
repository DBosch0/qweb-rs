//! `MALICIOUS_SCHEMES(WHITESPACE_REGEX.sub('', unquote_plus(href)))` from `_post_processing_att`.

/// `urllib.parse.unquote_plus` (UTF-8, errors='replace').
pub fn unquote_plus(s: &str) -> String {
    let s = s.replace('+', " ");
    if !s.contains('%') {
        return s;
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut pending: Vec<u8> = Vec::new();
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                pending.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        if !pending.is_empty() {
            out.push_str(&String::from_utf8_lossy(&pending));
            pending.clear();
        }
        // copy one char
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    if !pending.is_empty() {
        out.push_str(&String::from_utf8_lossy(&pending));
    }
    out
}

/// `WHITESPACE_REGEX = re.compile(r'[\s\x00-\x08\x0B\x0C\x0E-\x19]+')`.
fn is_ws(c: char) -> bool {
    let u = c as u32;
    u <= 0x19 || (0x1c..=0x20).contains(&u) || c.is_whitespace()
}

/// Case-insensitive char comparison mimicking Python's `re.IGNORECASE` for
/// the ASCII letters used in the pattern.
fn ci_eq(c: char, p: char) -> bool {
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

fn ci_starts_with(s: &[char], pat: &str) -> bool {
    let mut it = s.iter();
    for p in pat.chars() {
        match it.next() {
            Some(c) if ci_eq(*c, p) => {}
            _ => return false,
        }
    }
    true
}

/// `javascript:(?!((window\.)?)history\.back\(\)$)` with IGNORECASE, findall non-empty.
pub fn is_malicious_href(href: &str) -> bool {
    if href.is_empty() {
        return false;
    }
    // Fast path: the scheme needs a ':' (possibly percent-encoded).
    if !href.contains(':') && !href.contains('%') {
        return false;
    }
    let unq = unquote_plus(href);
    let chars: Vec<char> = unq.chars().filter(|&c| !is_ws(c)).collect();
    let mut i = 0;
    while i < chars.len() {
        if ci_starts_with(&chars[i..], "javascript:") {
            let rest = &chars[i + 11..];
            let ok = |pat: &str| rest.len() == pat.len() && ci_starts_with(rest, pat);
            if !(ok("history.back()") || ok("window.history.back()")) {
                return true;
            }
            i += 11;
        } else {
            i += 1;
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
        assert!(!is_malicious_href("javascript:history.back()"));
        assert!(!is_malicious_href("javascript:window.history.back()"));
        assert!(is_malicious_href("javascript:history.back();x"));
        assert!(!is_malicious_href("/web#action=1"));
        assert_eq!(unquote_plus("a+b%20c%e9%"), "a b c\u{fffd}%");
    }
}
