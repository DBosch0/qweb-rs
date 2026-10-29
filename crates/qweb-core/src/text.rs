//! Exact re-implementations of the regular expressions used by the Python
//! compiler for whitespace handling. Python's `$` (without MULTILINE) also
//! matches just before a trailing `\n`, which these functions reproduce.

#[inline]
const fn is_sp(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// `RSTRIP_REGEXP = re.compile(r'\n[ \t]*$')`.
///
/// Returns `(new_len, strip)` where `new_len` is the length of the string
/// after `RSTRIP_REGEXP.sub('', s)` and `strip` is `RSTRIP_REGEXP.search(s).group(0)`
/// (empty when no match).
pub(crate) fn rstrip_newline(s: &str) -> (usize, &str) {
    let b = s.as_bytes();
    let n = b.len();
    if n == 0 {
        return (0, "");
    }
    if b[n - 1] == b'\n' {
        // Either `\n[ \t]*\n` at the end (two matches) or a single `\n`.
        let k = n - 1;
        let mut m = k;
        while m > 0 && is_sp(b[m - 1]) {
            m -= 1;
        }
        if m > 0 && b[m - 1] == b'\n' {
            let i = m - 1;
            return (i, &s[i..k]);
        }
        return (k, &s[k..]);
    }
    let mut m = n;
    while m > 0 && is_sp(b[m - 1]) {
        m -= 1;
    }
    if m > 0 && b[m - 1] == b'\n' {
        return (m - 1, &s[m - 1..]);
    }
    (n, "")
}

/// `LSTRIP_REGEXP = re.compile(r'^[ \t]*\n')` used with `search`.
pub(crate) const fn lstrip_matches(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_sp(b[i]) {
        i += 1;
    }
    i < b.len() && b[i] == b'\n'
}

/// `FIRST_RSTRIP_REGEXP.sub(r'\2', s)` with
/// `FIRST_RSTRIP_REGEXP = re.compile(r'^(\n[ \t]*)+(\n[ \t])')`.
/// Returns the byte offset where the kept text starts.
pub(crate) fn first_rstrip_offset(s: &str) -> usize {
    let b = s.as_bytes();
    // Split the prefix into segments `\n[ \t]*`.
    let mut segs: Vec<(usize, usize)> = Vec::new(); // (pos of \n, run length)
    let mut i = 0;
    while i < b.len() && b[i] == b'\n' {
        let start = i;
        i += 1;
        while i < b.len() && is_sp(b[i]) {
            i += 1;
        }
        segs.push((start, i - start - 1));
    }
    // Greedy: largest r >= 1 with segment r+1 (index r) having a non-empty run.
    let k = segs.len();
    let mut r = k.saturating_sub(1);
    while r >= 1 {
        if segs[r].1 > 0 {
            return segs[r].0;
        }
        r -= 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rstrip() {
        assert_eq!(rstrip_newline("abc\n  "), (3, "\n  "));
        assert_eq!(rstrip_newline("a\n  \n"), (1, "\n  "));
        assert_eq!(rstrip_newline("a\n\n"), (1, "\n"));
        assert_eq!(rstrip_newline("a \n"), (2, "\n"));
        assert_eq!(rstrip_newline("a  "), (3, ""));
        assert_eq!(rstrip_newline("\n"), (0, "\n"));
    }
    #[test]
    fn first() {
        assert_eq!(first_rstrip_offset("\n\n    <div>"), 1);
        assert_eq!(first_rstrip_offset("\n    <div>"), 0);
        assert_eq!(first_rstrip_offset("\n  \n\n  x"), 4);
        assert_eq!(first_rstrip_offset("\n\n\nx"), 0);
    }
}
