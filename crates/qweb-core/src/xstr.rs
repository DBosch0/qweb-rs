//! [`XStr`]: the immutable string of the XML tree (tags, attributes, texts).
//!
//! Most strings of a template are short (tags, attribute names, indentation),
//! so strings of up to [`INLINE`] bytes are stored inline, without a heap
//! allocation. Longer strings take one exact-size allocation.

use std::borrow::{Borrow, Cow};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::NonNull;

/// Longest string stored inline.
pub const INLINE: usize = 23;

/// Immutable string, stored inline when short. Derefs to `str`.
///
/// Layout (24 bytes, no padding): 16 bytes that hold either the first inline
/// bytes or the heap pointer and length, 7 more inline bytes, then a `Tag`
/// byte that is the inline length or `Tag::Heap`. The tag is a `u8` enum
/// with 25 values, so the other 231 values are a niche: `Option<XStr>` is
/// also 24 bytes.
#[repr(C)]
pub struct XStr {
    data: Data,
    rest: [u8; INLINE - 16],
    tag: Tag,
}

#[repr(C)]
union Data {
    /// Inline bytes `0..16` (`tag` is the length).
    bytes: [u8; 16],
    /// Exact-size heap string (`tag` is [`Tag::Heap`]), owned like a `Box<str>`.
    heap: HeapStr,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct HeapStr {
    ptr: NonNull<u8>,
    len: usize,
}

/// Inline length (`L0`..`L23`) or `Heap`.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[rustfmt::skip]
enum Tag {
    L0, L1, L2, L3, L4, L5, L6, L7, L8, L9, L10, L11, L12, L13, L14, L15, L16,
    L17, L18, L19, L20, L21, L22, L23, Heap,
}

/// Tag of each inline length.
#[rustfmt::skip]
const LENS: [Tag; INLINE + 1] = {
    use Tag::{L0, L1, L2, L3, L4, L5, L6, L7, L8, L9, L10, L11, L12, L13, L14, L15, L16, L17, L18, L19, L20, L21, L22, L23};
    [L0, L1, L2, L3, L4, L5, L6, L7, L8, L9, L10, L11, L12, L13, L14, L15, L16,
     L17, L18, L19, L20, L21, L22, L23]
};

const _: () = assert!(std::mem::size_of::<XStr>() == 24);
const _: () = assert!(std::mem::size_of::<Option<XStr>>() == 24);
const _: () = assert!(std::mem::size_of::<Data>() == 16);
const _: () = assert!(Tag::Heap as usize == INLINE + 1);

// SAFETY: an `XStr` owns its heap string like a `Box<str>` (which is Send
// and Sync), and never shares it.
unsafe impl Send for XStr {}
// SAFETY: see `Send`; `&XStr` only gives `&str` access.
unsafe impl Sync for XStr {}

impl XStr {
    /// The empty string.
    pub const fn new() -> Self {
        XStr {
            data: Data { bytes: [0; 16] },
            rest: [0; INLINE - 16],
            tag: Tag::L0,
        }
    }

    /// A short string, stored inline (`s.len() <= INLINE`).
    #[inline]
    fn inline(s: &str) -> Self {
        debug_assert!(s.len() <= INLINE);
        let mut raw = [0u8; INLINE];
        raw[..s.len()].copy_from_slice(s.as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&raw[..16]);
        let mut rest = [0u8; INLINE - 16];
        rest.copy_from_slice(&raw[16..]);
        XStr {
            data: Data { bytes },
            rest,
            tag: LENS[s.len()],
        }
    }

    /// Take ownership of a heap string (any length).
    fn heap(s: Box<str>) -> Self {
        let len = s.len();
        let ptr = Box::into_raw(s).cast::<u8>();
        XStr {
            // SAFETY: `Box::into_raw` never returns null.
            data: Data {
                heap: HeapStr {
                    ptr: unsafe { NonNull::new_unchecked(ptr) },
                    len,
                },
            },
            rest: [0; INLINE - 16],
            tag: Tag::Heap,
        }
    }

    #[inline]
    fn is_inline(&self) -> bool {
        self.tag != Tag::Heap
    }

    /// The string.
    #[inline]
    pub fn as_str(&self) -> &str {
        if self.is_inline() {
            let len = self.tag as usize;
            // SAFETY: inline, `data.bytes` and `rest` are the first `INLINE`
            // bytes of the `repr(C)` struct, contiguous and initialised (see
            // `XStr::inline`), and `bytes[..len]` is a copy of a whole `&str`.
            unsafe {
                let bytes =
                    std::slice::from_raw_parts(std::ptr::from_ref::<XStr>(self).cast::<u8>(), len);
                std::str::from_utf8_unchecked(bytes)
            }
        } else {
            // SAFETY: on the heap, `data.heap` is the live string taken from
            // a `Box<str>` in `XStr::heap`.
            unsafe {
                let h = self.data.heap;
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(h.ptr.as_ptr(), h.len))
            }
        }
    }

    /// Convert to a `Box<str>`.
    pub fn into_boxed_str(self) -> Box<str> {
        if self.is_inline() {
            return self.as_str().into();
        }
        let this = ManuallyDrop::new(self);
        // SAFETY: on the heap, `data.heap` came from `Box::into_raw` (see
        // `XStr::heap`); `this` is not dropped, so the box is rebuilt once.
        unsafe {
            let h = this.data.heap;
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(h.ptr.as_ptr(), h.len) as *mut str)
        }
    }

    /// Convert to a `String`.
    pub fn into_string(self) -> String {
        if self.is_inline() {
            self.as_str().to_owned()
        } else {
            self.into_boxed_str().into_string()
        }
    }
}

impl Drop for XStr {
    fn drop(&mut self) {
        if !self.is_inline() {
            // SAFETY: on the heap, `data.heap` came from `Box::into_raw` and
            // is released here only.
            unsafe {
                let h = self.data.heap;
                drop(Box::from_raw(
                    std::ptr::slice_from_raw_parts_mut(h.ptr.as_ptr(), h.len) as *mut str,
                ));
            }
        }
    }
}

impl Clone for XStr {
    #[inline]
    fn clone(&self) -> Self {
        if self.is_inline() {
            XStr {
                // SAFETY: inline, `data.bytes` is the active, initialised field.
                data: Data {
                    bytes: unsafe { self.data.bytes },
                },
                rest: self.rest,
                tag: self.tag,
            }
        } else {
            XStr::heap(self.as_str().into())
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
            XStr::inline(s)
        } else {
            XStr::heap(s.into())
        }
    }
}

impl From<String> for XStr {
    fn from(s: String) -> Self {
        if s.len() <= INLINE {
            XStr::from(s.as_str())
        } else {
            XStr::heap(s.into_boxed_str())
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
            XStr::heap(s)
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
        self.as_str().hash(state);
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
        assert!(XStr::from("x".repeat(INLINE)).is_inline());
        assert!(!XStr::from("x".repeat(INLINE + 1)).is_inline());
    }

    #[test]
    fn every_length() {
        // Inline and heap, cloned, converted and dropped (run under Miri).
        let mut all = Vec::new();
        for n in 0..=INLINE + 3 {
            let s: String = "é".repeat(n / 2) + &"z".repeat(n % 2);
            let x = XStr::from(s.as_str());
            assert_eq!(x.as_str(), s);
            assert_eq!(x.is_inline(), s.len() <= INLINE);
            let y = x.clone();
            assert_eq!(y, x);
            assert_eq!(XStr::from(s.clone().into_boxed_str()), s);
            assert_eq!(y.into_boxed_str().as_ref(), s);
            all.push(Some(x));
        }
        all.push(None);
        let copies = all.clone();
        assert_eq!(all, copies);
        let moved = std::thread::spawn(move || copies.len()).join().unwrap();
        assert_eq!(moved, all.len());
    }
}
