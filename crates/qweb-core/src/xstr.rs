//! [`XStr`]: the immutable string of the XML tree (tags, attributes, texts).
//!
//! Most strings of a template are short (tags, attribute names, indentation),
//! so strings of up to [`INLINE`] bytes are stored inline, without a heap
//! allocation. Longer strings take one exact-size allocation.

use std::borrow::{Borrow, Cow};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;

/// Longest string stored inline.
pub const INLINE: usize = 22;

/// Immutable string, stored inline when short. Derefs to `str`.
#[derive(Clone)]
pub struct XStr(Repr);

#[derive(Clone)]
enum Repr {
    /// `buf[..len]` is a copy of a whole `&str`, so it is valid UTF-8.
    Inline {
        len: u8,
        buf: [u8; INLINE],
    },
    Heap(Box<str>),
}

const _: () = assert!(std::mem::size_of::<XStr>() == 24);
const _: () = assert!(std::mem::size_of::<Option<XStr>>() == 24);

impl XStr {
    /// The empty string.
    pub const fn new() -> Self {
        XStr(Repr::Inline {
            len: 0,
            buf: [0; INLINE],
        })
    }

    /// The string.
    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::Inline { len, buf } => {
                // SAFETY: `buf[..len]` is only ever written by `XStr::from(&str)`,
                // which copies a whole `&str` (see `Repr::Inline`).
                unsafe { std::str::from_utf8_unchecked(&buf[..*len as usize]) }
            }
            Repr::Heap(s) => s,
        }
    }

    /// Convert to a `Box<str>`.
    pub fn into_boxed_str(self) -> Box<str> {
        match self.0 {
            Repr::Heap(s) => s,
            Repr::Inline { .. } => self.as_str().into(),
        }
    }

    /// Convert to a `String`.
    pub fn into_string(self) -> String {
        match self.0 {
            Repr::Heap(s) => s.into_string(),
            Repr::Inline { .. } => self.as_str().to_owned(),
        }
    }
}

impl Default for XStr {
    fn default() -> Self {
        XStr::new()
    }
}

impl From<&str> for XStr {
    #[inline]
    fn from(s: &str) -> Self {
        if s.len() <= INLINE {
            let mut buf = [0; INLINE];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            XStr(Repr::Inline {
                len: s.len() as u8,
                buf,
            })
        } else {
            XStr(Repr::Heap(s.into()))
        }
    }
}

impl From<String> for XStr {
    fn from(s: String) -> Self {
        if s.len() <= INLINE {
            XStr::from(s.as_str())
        } else {
            XStr(Repr::Heap(s.into_boxed_str()))
        }
    }
}

impl From<&String> for XStr {
    fn from(s: &String) -> Self {
        XStr::from(s.as_str())
    }
}

impl From<Box<str>> for XStr {
    fn from(s: Box<str>) -> Self {
        if s.len() <= INLINE {
            XStr::from(&*s)
        } else {
            XStr(Repr::Heap(s))
        }
    }
}

impl From<Cow<'_, str>> for XStr {
    fn from(s: Cow<'_, str>) -> Self {
        match s {
            Cow::Borrowed(s) => XStr::from(s),
            Cow::Owned(s) => XStr::from(s),
        }
    }
}

impl From<&XStr> for XStr {
    fn from(s: &XStr) -> Self {
        s.clone()
    }
}

impl From<XStr> for String {
    fn from(s: XStr) -> Self {
        s.into_string()
    }
}

impl From<XStr> for Box<str> {
    fn from(s: XStr) -> Self {
        s.into_boxed_str()
    }
}

impl From<&XStr> for String {
    fn from(s: &XStr) -> Self {
        s.as_str().to_owned()
    }
}

impl Deref for XStr {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for XStr {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for XStr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for XStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl fmt::Debug for XStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for XStr {
    #[inline]
    fn eq(&self, other: &XStr) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for XStr {}

impl Hash for XStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

impl PartialOrd for XStr {
    fn partial_cmp(&self, other: &XStr) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for XStr {
    fn cmp(&self, other: &XStr) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

macro_rules! eq_str {
    ($($t:ty),*) => {$(
        impl PartialEq<$t> for XStr {
            #[inline]
            fn eq(&self, other: &$t) -> bool {
                self.as_str() == &other[..]
            }
        }
        impl PartialEq<XStr> for $t {
            #[inline]
            fn eq(&self, other: &XStr) -> bool {
                &self[..] == other.as_str()
            }
        }
    )*};
}
eq_str!(str, &str, String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_heap() {
        for s in [
            "",
            "a",
            "é€😀",
            &"x".repeat(INLINE),
            &"y".repeat(INLINE + 1),
        ] {
            let x = XStr::from(s);
            assert_eq!(x, s);
            assert_eq!(XStr::from(s.to_owned()), x);
            assert_eq!(x.clone().into_string(), s);
            assert_eq!(x.len(), s.len());
        }
        assert!(matches!(
            XStr::from("x".repeat(INLINE)).0,
            Repr::Inline { .. }
        ));
        assert!(matches!(
            XStr::from("x".repeat(INLINE + 1)).0,
            Repr::Heap(_)
        ));
    }
}
