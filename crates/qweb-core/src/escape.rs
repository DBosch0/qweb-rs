//! HTML escaping compatible with Python's `markupsafe.escape`.
//!
//! markupsafe replaces `& < > " '` with `&amp; &lt; &gt; &#34; &#39;`; every
//! other character, including non-ASCII ones, is copied unchanged.

#[inline]
const fn needs_escape(b: u8) -> bool {
    matches!(b, b'&' | b'<' | b'>' | b'"' | b'\'')
}

/// Append `s` to `out`, escaped.
///
/// Runs of bytes that need no escaping are copied with a single `push_str`.
/// The five special characters are ASCII, so splitting at their byte offsets
/// always lands on UTF-8 character boundaries.
pub fn escape_into(out: &mut String, s: &str) {
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        if !needs_escape(b) {
            continue;
        }
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
    out.push_str(&s[start..]);
}

/// Escape `s`, returning a new string.
///
/// ```
/// assert_eq!(qweb_core::escape::escape("<a href='x'>"), "&lt;a href=&#39;x&#39;&gt;");
/// ```
pub fn escape(s: &str) -> String {
    // Room for a few entities before the first reallocation.
    const EXTRA_CAPACITY: usize = 16;
    let Some(first) = s.bytes().position(needs_escape) else {
        return s.to_owned();
    };
    let mut out = String::with_capacity(s.len() + EXTRA_CAPACITY);
    out.push_str(&s[..first]);
    escape_into(&mut out, &s[first..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic() {
        assert_eq!(
            escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&#34;x&#34;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
        assert_eq!(escape("été"), "été");
        assert_eq!(escape("été<"), "été&lt;");
    }
}
