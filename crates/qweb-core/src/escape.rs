//! HTML escaping compatible with `markupsafe.escape`.
//!
//! markupsafe replaces `& < > " '` with `&amp; &lt; &gt; &#34; &#39;`.

#[inline]
fn needs_escape(b: u8) -> bool {
    matches!(b, b'&' | b'<' | b'>' | b'"' | b'\'')
}

/// Append `s` to `out`, escaped like `markupsafe.escape`.
#[inline]
pub fn escape_into(out: &mut String, s: &str) {
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    // Fast path: look for the first byte needing escape with memchr3 on the
    // most common characters, then check the remaining two manually.
    while i < bytes.len() {
        let b = bytes[i];
        if needs_escape(b) {
            out.push_str(&s[start..i]);
            out.push_str(match b {
                b'&' => "&amp;",
                b'<' => "&lt;",
                b'>' => "&gt;",
                b'"' => "&#34;",
                _ => "&#39;",
            });
            start = i + 1;
        }
        i += 1;
    }
    out.push_str(&s[start..]);
}

/// Escape `s`, returning a new string.
pub fn escape(s: &str) -> String {
    if !s.bytes().any(needs_escape) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 16);
    escape_into(&mut out, s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic() {
        assert_eq!(escape(r#"<a href="x">'&'</a>"#), "&lt;a href=&#34;x&#34;&gt;&#39;&amp;&#39;&lt;/a&gt;");
        assert_eq!(escape("été"), "été");
    }
}
