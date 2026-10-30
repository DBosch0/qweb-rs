//! Minimal XML parser producing an lxml-like tree (`text`/`tail` model).
//!
//! It reproduces the lxml behaviour the QWeb compiler relies on:
//! * entities and character references are decoded, CDATA is merged into text;
//! * line endings are normalised (`\r\n` and `\r` become `\n`);
//! * literal tabs/newlines in attribute values become spaces;
//! * namespaced tags and attributes use the `{uri}local` notation, `nsmap`
//!   holds every namespace in scope (own declarations first, then ancestors).
//!
//! Nodes live in an arena ([`Document::nodes`]) and refer to each other by
//! index. Detached nodes stay in the arena.

use core::{error, fmt};
use std::borrow::Cow;
use std::sync::Arc;

/// Index of a node in [`Document::nodes`].
pub type NodeId = usize;

/// The namespace bound to the `xml` prefix.
pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Kind of a [`Node`]. Text is not a node: it is stored in `text` / `tail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Element.
    Element,
    /// Comment (`<!--text-->`).
    Comment,
    /// Processing instruction (`<?tag text?>`).
    Pi,
}

/// In-scope namespace map, `(prefix, uri)`; `None` is the default namespace.
pub type NsMap = Arc<Vec<(Option<String>, String)>>;

/// A node of a [`Document`].
#[derive(Clone)]
pub struct Node {
    /// Node kind.
    pub kind: NodeKind,
    /// Element: lxml-style tag (`{uri}local` or `local`). PI: target. Comment: empty.
    pub tag: String,
    /// Prefix of the element namespace as written in the source.
    pub prefix: Option<String>,
    /// Attributes in source order, names in lxml notation.
    pub attrs: Vec<(String, String)>,
    /// Namespaces in scope. Shared with the parent (same `Arc`) when the
    /// element declares none.
    pub nsmap: NsMap,
    /// Element text, comment text or PI text.
    pub text: Option<String>,
    /// Text after the node, up to the next sibling or the parent's end.
    pub tail: Option<String>,
    /// Child nodes, in document order.
    pub children: Vec<NodeId>,
    /// Parent element; `None` for the root and detached nodes.
    pub parent: Option<NodeId>,
    /// Line number of the node start (1-based) for error messages.
    pub line: u32,
}

impl Node {
    /// Tag without its namespace.
    pub fn local_name(&self) -> &str {
        local_name(&self.tag)
    }

    /// Attribute value (lxml `get`).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the attribute exists.
    pub fn has(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }

    /// Like lxml `attrib.pop(name)`.
    pub fn pop(&mut self, name: &str) -> Option<String> {
        let idx = self.attrs.iter().position(|(k, _)| k == name)?;
        // Not `swap_remove`: attribute order is output order.
        Some(self.attrs.remove(idx).1)
    }

    /// Like lxml `attrib[name] = value`: replace in place or append.
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(slot) = self.attrs.iter_mut().find(|(k, _)| k == name) {
            slot.1 = value;
        } else {
            self.attrs.push((name.to_owned(), value));
        }
    }
}

/// Split an lxml-style name into its namespace URI (if any) and local name.
///
/// ```
/// use qweb_core::xml::split_name;
/// assert_eq!(split_name("{http://x}a"), (Some("http://x"), "a"));
/// assert_eq!(split_name("a"), (None, "a"));
/// ```
#[inline]
pub fn split_name(name: &str) -> (Option<&str>, &str) {
    if let Some(rest) = name.strip_prefix('{')
        && let Some(i) = rest.find('}')
    {
        return (Some(&rest[..i]), &rest[i + 1..]);
    }
    (None, name)
}

/// Local name of an lxml-style name (`{uri}local` or `local`).
#[inline]
pub fn local_name(tag: &str) -> &str {
    split_name(tag).1
}

/// Namespace URI of an lxml-style name, if any.
#[inline]
pub fn namespace(name: &str) -> Option<&str> {
    split_name(name).0
}

/// A parsed XML tree.
pub struct Document {
    /// Node arena; [`NodeId`]s index it.
    pub nodes: Vec<Node>,
    /// Root element.
    pub root: NodeId,
}

impl Document {
    /// Parse an XML document (like `etree.fromstring`); only the root element
    /// is kept, the prolog and trailing comments/PIs are skipped.
    pub fn parse(src: &str) -> Result<Document, XmlError> {
        Parser::new(src).parse()
    }

    /// A node.
    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    /// A node, mutably.
    #[inline]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    /// Next sibling (any kind), like lxml `getnext()`.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.nodes[id].parent?;
        let siblings = &self.nodes[parent].children;
        let pos = siblings.iter().position(|&c| c == id)?;
        siblings.get(pos + 1).copied()
    }

    /// Remove `child` from the children of `parent` (its tail goes with it).
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        let children = &mut self.nodes[parent].children;
        if let Some(pos) = children.iter().position(|&c| c == child) {
            // Not `swap_remove`: children are in document order.
            children.remove(pos);
        }
        self.nodes[child].parent = None;
    }

    /// Position of `id` among its parent's children.
    pub fn index_in_parent(&self, id: NodeId) -> Option<usize> {
        let parent = self.nodes[id].parent?;
        self.nodes[parent].children.iter().position(|&c| c == id)
    }

    /// Create a detached element (`etree.Element(tag, attrib)`).
    pub fn new_element(&mut self, tag: &str, attrs: Vec<(String, String)>) -> NodeId {
        self.nodes.push(Node {
            kind: NodeKind::Element,
            tag: tag.to_owned(),
            prefix: None,
            attrs,
            nsmap: Arc::new(Vec::new()),
            text: None,
            tail: None,
            children: Vec::new(),
            parent: None,
            line: 0,
        });
        self.nodes.len() - 1
    }

    /// Insert `child` into `parent` at `index` (lxml `parent.insert(index, child)`):
    /// the child is moved with its tail, and inherits the parent namespaces.
    pub fn insert(&mut self, parent: NodeId, index: usize, child: NodeId) {
        if let Some(old) = self.nodes[child].parent {
            self.remove_child(old, child);
        }
        let children = &mut self.nodes[parent].children;
        let index = index.min(children.len());
        children.insert(index, child);
        self.nodes[child].parent = Some(parent);
        if self.nodes[child].nsmap.is_empty() && !self.nodes[parent].nsmap.is_empty() {
            self.nodes[child].nsmap = self.nodes[parent].nsmap.clone();
        }
    }

    /// Wrap `id` in a new element: the new element takes the place of `id`
    /// in its parent and `id` (with its tail) becomes its only child.
    pub fn wrap(&mut self, id: NodeId, tag: &str, attrs: Vec<(String, String)>) -> NodeId {
        let new = self.new_element(tag, attrs);
        if let (Some(parent), Some(index)) = (self.nodes[id].parent, self.index_in_parent(id)) {
            self.insert(parent, index, new);
        }
        self.insert(new, 0, id);
        new
    }

    /// Previous sibling (any kind), like lxml `getprevious()`.
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.nodes[id].parent?;
        let siblings = &self.nodes[parent].children;
        let pos = siblings.iter().position(|&c| c == id)?;
        pos.checked_sub(1).map(|p| siblings[p])
    }

    /// Detach `id` from its parent (lxml `parent.remove(id)`: the tail goes with it).
    pub fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id].parent {
            self.remove_child(parent, id);
        }
    }

    /// lxml `parent.append(child)`.
    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        let len = self.nodes[parent].children.len();
        self.insert(parent, len, child);
    }

    /// lxml `target.addprevious(new)` (`new` is moved with its tail).
    pub fn add_previous(&mut self, target: NodeId, new: NodeId) -> Result<(), &'static str> {
        self.add_sibling(target, new, 0)
    }

    /// lxml `target.addnext(new)` (`new` is moved with its tail).
    pub fn add_next(&mut self, target: NodeId, new: NodeId) -> Result<(), &'static str> {
        self.add_sibling(target, new, 1)
    }

    fn add_sibling(
        &mut self,
        target: NodeId,
        new: NodeId,
        offset: usize,
    ) -> Result<(), &'static str> {
        if new == target {
            return Ok(());
        }
        let Some(_) = self.nodes[target].parent else {
            return Err(
                "Only processing instructions and comments can be siblings of the root element",
            );
        };
        let mut cur = Some(target);
        while let Some(c) = cur {
            if c == new {
                return Err("cannot add ancestor as sibling, please break cycle first");
            }
            cur = self.nodes[c].parent;
        }
        self.detach(new);
        let parent = self.nodes[target].parent.unwrap();
        let index = self.index_in_parent(target).unwrap_or(0);
        self.insert(parent, index + offset, new);
        Ok(())
    }

    /// Create a detached processing instruction.
    pub fn new_pi(&mut self, target: &str, text: &str) -> NodeId {
        let id = self.new_element(target, Vec::new());
        self.nodes[id].kind = NodeKind::Pi;
        self.nodes[id].text = Some(text.to_owned());
        id
    }

    /// `copy.deepcopy(node)`: a detached copy of the subtree, tail included.
    pub fn deep_copy(&mut self, id: NodeId) -> NodeId {
        let copy = self.copy_from_doc(None, id);
        self.nodes[copy].parent = None;
        copy
    }

    /// Copy a subtree of another document into this one (tail included).
    pub fn import(&mut self, other: &Document, id: NodeId) -> NodeId {
        let copy = self.copy_from_doc(Some(other), id);
        self.nodes[copy].parent = None;
        copy
    }

    fn copy_from_doc(&mut self, other: Option<&Document>, id: NodeId) -> NodeId {
        let mut src = match other {
            Some(d) => d.nodes[id].clone(),
            None => self.nodes[id].clone(),
        };
        let children = std::mem::take(&mut src.children);
        self.nodes.push(src);
        let new = self.nodes.len() - 1;
        for c in children {
            let cc = self.copy_from_doc(other, c);
            self.nodes[cc].parent = Some(new);
            self.nodes[new].children.push(cc);
        }
        new
    }

    /// Topmost ancestor of `id` (itself if detached).
    pub fn top(&self, mut id: NodeId) -> NodeId {
        while let Some(p) = self.nodes[id].parent {
            id = p;
        }
        id
    }

    /// `etree.tostring(node, encoding='unicode', with_tail=...)`.
    pub fn to_string(&self, id: NodeId, with_tail: bool) -> String {
        let mut out = String::new();
        self.serialize(id, None, &mut out);
        if with_tail && let Some(t) = &self.nodes[id].tail {
            escape_xml_text(t, &mut out);
        }
        out
    }

    fn serialize(&self, id: NodeId, parent_ns: Option<&NsMap>, out: &mut String) {
        let node = &self.nodes[id];
        match node.kind {
            NodeKind::Comment => {
                out.push_str("<!--");
                out.push_str(node.text.as_deref().unwrap_or(""));
                out.push_str("-->");
            }
            NodeKind::Pi => {
                out.push_str("<?");
                out.push_str(&node.tag);
                if let Some(t) = node.text.as_deref().filter(|t| !t.is_empty()) {
                    out.push(' ');
                    out.push_str(t);
                }
                out.push_str("?>");
            }
            NodeKind::Element => {
                let name = qualified(&node.tag, node.prefix.as_deref(), &node.nsmap, true);
                out.push('<');
                out.push_str(&name);
                // Declare the namespaces not declared by an ancestor. Elements
                // without declarations share their parent's map (fast path);
                // otherwise both maps hold a handful of entries.
                let same_as_parent = parent_ns.is_some_and(|p| Arc::ptr_eq(p, &node.nsmap));
                for (prefix, uri) in node.nsmap.iter().filter(|_| !same_as_parent) {
                    let inherited =
                        parent_ns.is_some_and(|p| p.iter().any(|(q, u)| q == prefix && u == uri));
                    if inherited {
                        continue;
                    }
                    match prefix {
                        Some(p) => {
                            out.push_str(" xmlns:");
                            out.push_str(p);
                        }
                        None => out.push_str(" xmlns"),
                    }
                    out.push_str("=\"");
                    escape_xml_attr(uri, out);
                    out.push('"');
                }
                for (k, v) in &node.attrs {
                    out.push(' ');
                    out.push_str(&qualified(k, None, &node.nsmap, false));
                    out.push_str("=\"");
                    escape_xml_attr(v, out);
                    out.push('"');
                }
                if node.text.is_none() && node.children.is_empty() {
                    out.push_str("/>");
                    return;
                }
                out.push('>');
                if let Some(t) = &node.text {
                    escape_xml_text(t, out);
                }
                for &c in &node.children {
                    self.serialize(c, Some(&node.nsmap), out);
                    if let Some(t) = &self.nodes[c].tail {
                        escape_xml_text(t, out);
                    }
                }
                out.push_str("</");
                out.push_str(&name);
                out.push('>');
            }
        }
    }

    /// lxml `findtext(tag)` for a direct child tag: text of the first matching
    /// child (`""` when it has none), `None` when there is no such child.
    pub fn findtext(&self, id: NodeId, tag: &str) -> Option<String> {
        self.nodes[id]
            .children
            .iter()
            .find(|&&c| self.nodes[c].kind == NodeKind::Element && self.nodes[c].tag == tag)
            .map(|&c| self.nodes[c].text.clone().unwrap_or_default())
    }

    /// Pre-order iteration over elements (like lxml `iter()`, comments included).
    pub fn iter(&self, start: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            out.push(id);
            for &c in self.nodes[id].children.iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// lxml `getroottree().getpath(el)` (used for error messages only).
    pub fn path(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            let node = &self.nodes[c];
            let name = match node.kind {
                NodeKind::Element => {
                    let local = node.local_name();
                    match &node.prefix {
                        Some(p) => format!("{p}:{local}"),
                        None if node.tag.starts_with('{') => "*".to_owned(),
                        None => local.to_owned(),
                    }
                }
                NodeKind::Comment => "comment()".to_owned(),
                NodeKind::Pi => "processing-instruction()".to_owned(),
            };
            let seg = match node.parent {
                Some(p) => {
                    let same: Vec<_> = self.nodes[p]
                        .children
                        .iter()
                        .filter(|&&s| {
                            self.nodes[s].kind == node.kind && self.nodes[s].tag == node.tag
                        })
                        .collect();
                    if same.len() > 1 {
                        let idx = same.iter().position(|&&s| s == c).unwrap_or(0) + 1;
                        format!("{name}[{idx}]")
                    } else {
                        name
                    }
                }
                None => name,
            };
            parts.push(seg);
            cur = node.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// Serialize the start tag of an element with its current attributes,
    /// like `etree.tostring(etree.Element(el.tag, el.attrib))` (error messages).
    pub fn start_tag_repr(&self, id: NodeId) -> String {
        let node = &self.nodes[id];
        let mut s = String::from("<");
        s.push_str(node.local_name());
        for (k, v) in &node.attrs {
            s.push(' ');
            s.push_str(local_name(k));
            s.push_str("=\"");
            for ch in v.chars() {
                match ch {
                    '&' => s.push_str("&amp;"),
                    '<' => s.push_str("&lt;"),
                    '>' => s.push_str("&gt;"),
                    '"' => s.push_str("&quot;"),
                    c => s.push(c),
                }
            }
            s.push('"');
        }
        s.push_str("/>");
        s
    }
}

/// `prefix:local` name of an lxml-style `{uri}local` name, for serialization.
fn qualified<'a>(
    name: &'a str,
    prefix: Option<&str>,
    nsmap: &NsMap,
    element: bool,
) -> Cow<'a, str> {
    let (Some(uri), local) = split_name(name) else {
        return Cow::Borrowed(name);
    };
    if uri == XML_NS {
        return format!("xml:{local}").into();
    }
    if let Some(p) = prefix
        && nsmap
            .iter()
            .any(|(q, u)| q.as_deref() == Some(p) && u == uri)
    {
        return format!("{p}:{local}").into();
    }
    // attributes cannot use the default namespace
    if element && nsmap.iter().any(|(q, u)| q.is_none() && u == uri) {
        return Cow::Borrowed(local);
    }
    match nsmap.iter().find(|(q, u)| q.is_some() && u == uri) {
        Some((Some(p), _)) => format!("{p}:{local}").into(),
        _ => Cow::Borrowed(local),
    }
}

/// libxml2 text escaping.
pub fn escape_xml_text(s: &str, out: &mut String) {
    let b = s.as_bytes();
    // `\r` is rare: without it, memchr (vectorised) can jump between the
    // three remaining special bytes.
    if memchr::memchr(b'\r', b).is_none() {
        let mut last = 0;
        for i in memchr::memchr3_iter(b'&', b'<', b'>', b) {
            out.push_str(&s[last..i]);
            out.push_str(match b[i] {
                b'&' => "&amp;",
                b'<' => "&lt;",
                _ => "&gt;",
            });
            last = i + 1;
        }
        out.push_str(&s[last..]);
        return;
    }
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'\r' => "&#13;",
            _ => continue,
        };
        out.push_str(&s[last..i]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&s[last..]);
}

/// libxml2 attribute value escaping.
pub fn escape_xml_attr(s: &str, out: &mut String) {
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\n' => "&#10;",
            b'\r' => "&#13;",
            b'\t' => "&#9;",
            _ => continue,
        };
        out.push_str(&s[last..i]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&s[last..]);
}

/// XML syntax error.
#[derive(Debug)]
pub struct XmlError {
    /// Description (libxml2-like).
    pub message: String,
    /// Line (1-based).
    pub line: u32,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}, line {}", self.message, self.line)
    }
}

impl error::Error for XmlError {}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    /// Line number at `line_pos`, advanced lazily by [`Parser::node_line`].
    line: u32,
    line_pos: usize,
    nodes: Vec<Node>,
}

fn is_name_end(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>' | b'=' | b'?')
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            src,
            b: src.as_bytes(),
            pos: 0,
            line: 1,
            line_pos: 0,
            nodes: Vec::new(),
        }
    }

    /// Line of the current position, counted from the start (for errors).
    fn error_line(&self) -> u32 {
        1 + memchr::memchr_iter(b'\n', &self.b[..self.pos]).count() as u32
    }

    /// Line of the current position. Lines are counted from the previous
    /// call, so a whole parse scans the input once.
    fn node_line(&mut self) -> u32 {
        let skipped = &self.b[self.line_pos..self.pos];
        self.line += memchr::memchr_iter(b'\n', skipped).count() as u32;
        self.line_pos = self.pos;
        self.line
    }

    #[cold]
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, XmlError> {
        Err(XmlError {
            message: msg.into(),
            line: self.error_line(),
        })
    }

    #[inline]
    fn starts_with(&self, s: &str) -> bool {
        self.b[self.pos..].starts_with(s.as_bytes())
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    fn skip_ws(&mut self) {
        while self.pos < self.b.len() && matches!(self.b[self.pos], b' ' | b'\t' | b'\n' | b'\r') {
            self.advance(1);
        }
    }

    fn find(&self, pat: &str) -> Option<usize> {
        memchr::memmem::find(&self.b[self.pos..], pat.as_bytes()).map(|i| self.pos + i)
    }

    fn read_name(&mut self) -> Result<&'a str, XmlError> {
        let start = self.pos;
        while self.pos < self.b.len() && !is_name_end(self.b[self.pos]) {
            self.pos += 1;
        }
        if start == self.pos {
            return self.err("expected a name");
        }
        Ok(&self.src[start..self.pos])
    }

    fn parse(mut self) -> Result<Document, XmlError> {
        // Prolog: XML declaration, comments, PIs, doctype, whitespace.
        loop {
            self.skip_ws();
            if self.starts_with("<?") {
                let end = self.find("?>").ok_or_else(|| XmlError {
                    message: "unterminated PI".into(),
                    line: self.error_line(),
                })?;
                self.advance(end + 2 - self.pos);
            } else if self.starts_with("<!--") {
                let end = self.find("-->").ok_or_else(|| XmlError {
                    message: "unterminated comment".into(),
                    line: self.error_line(),
                })?;
                self.advance(end + 3 - self.pos);
            } else if self.starts_with("<!DOCTYPE") {
                self.skip_doctype()?;
            } else {
                break;
            }
        }
        if !self.starts_with("<") {
            return self.err("Start tag expected, '<' not found");
        }
        let root_ns: NsMap = Arc::new(Vec::new());
        let root = self.parse_element(None, &root_ns)?;
        // Trailing misc is ignored (like lxml.fromstring, which only returns the root).
        Ok(Document {
            nodes: self.nodes,
            root,
        })
    }

    fn skip_doctype(&mut self) -> Result<(), XmlError> {
        let mut depth = 0i32;
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            self.advance(1);
            match c {
                b'[' => depth += 1,
                b']' => depth -= 1,
                b'>' if depth <= 0 => return Ok(()),
                _ => {}
            }
        }
        self.err("unterminated DOCTYPE")
    }

    fn new_node(&mut self, kind: NodeKind, parent: Option<NodeId>, nsmap: NsMap) -> NodeId {
        let line = self.node_line();
        self.nodes.push(Node {
            kind,
            tag: String::new(),
            prefix: None,
            attrs: Vec::new(),
            nsmap,
            text: None,
            tail: None,
            children: Vec::new(),
            parent,
            line,
        });
        self.nodes.len() - 1
    }

    fn parse_element(
        &mut self,
        parent: Option<NodeId>,
        parent_ns: &NsMap,
    ) -> Result<NodeId, XmlError> {
        let line = self.node_line();
        self.advance(1); // '<'
        let qname = self.read_name()?;
        // Raw attributes first: namespace declarations must be known before
        // resolving prefixes.
        let mut raw_attrs: Vec<(&'a str, String)> = Vec::new();
        let mut own_ns: Vec<(Option<String>, String)> = Vec::new();
        let self_closing;
        loop {
            self.skip_ws();
            if self.pos >= self.b.len() {
                return self.err("unexpected end of document in start tag");
            }
            if self.starts_with("/>") {
                self.advance(2);
                self_closing = true;
                break;
            }
            if self.b[self.pos] == b'>' {
                self.advance(1);
                self_closing = false;
                break;
            }
            let name = self.read_name()?;
            self.skip_ws();
            if self.pos >= self.b.len() || self.b[self.pos] != b'=' {
                return self.err(format!("Specification mandates value for attribute {name}"));
            }
            self.advance(1);
            self.skip_ws();
            let value = self.read_attr_value()?;
            if name == "xmlns" {
                own_ns.push((None, value));
            } else if let Some(p) = name.strip_prefix("xmlns:") {
                own_ns.push((Some(p.to_owned()), value));
            } else {
                if raw_attrs.iter().any(|(k, _)| *k == name) {
                    return self.err(format!("Attribute {name} redefined"));
                }
                raw_attrs.push((name, value));
            }
        }

        let nsmap: NsMap = if own_ns.is_empty() {
            parent_ns.clone()
        } else {
            let mut m = own_ns.clone();
            for (p, u) in parent_ns.iter() {
                if !m.iter().any(|(q, _)| q == p) {
                    m.push((p.clone(), u.clone()));
                }
            }
            Arc::new(m)
        };

        let resolve = |prefix: Option<&str>, this: &Self| -> Result<Option<String>, XmlError> {
            match prefix {
                Some("xml") => Ok(Some(XML_NS.to_owned())),
                Some(p) => match nsmap.iter().find(|(q, _)| q.as_deref() == Some(p)) {
                    Some((_, u)) => Ok(Some(u.clone())),
                    None => this.err(format!("Namespace prefix {p} is not defined")),
                },
                None => Ok(nsmap
                    .iter()
                    .find(|(q, _)| q.is_none())
                    .map(|(_, u)| u.clone())),
            }
        };

        let (prefix, local) = match qname.split_once(':') {
            Some((p, l)) => (Some(p), l),
            None => (None, qname),
        };
        let ns = resolve(prefix, self)?;
        let tag = match ns {
            Some(u) if !u.is_empty() => format!("{{{u}}}{local}"),
            _ => local.to_owned(),
        };
        let mut attrs = Vec::with_capacity(raw_attrs.len());
        for (name, value) in raw_attrs {
            let key = match name.split_once(':') {
                Some((p, l)) => match resolve(Some(p), self)? {
                    Some(u) => format!("{{{u}}}{l}"),
                    None => l.to_owned(),
                },
                None => name.to_owned(), // unprefixed attributes have no namespace
            };
            attrs.push((key, value));
        }

        let id = self.new_node(NodeKind::Element, parent, nsmap.clone());
        {
            let n = &mut self.nodes[id];
            n.tag = tag;
            n.prefix = prefix.map(str::to_owned);
            n.attrs = attrs;
            n.line = line;
        }
        if let Some(p) = parent {
            self.nodes[p].children.push(id);
        }
        if self_closing {
            return Ok(id);
        }

        // Content
        let mut text = String::new();
        let mut has_text = false;
        let mut last_child: Option<NodeId> = None;
        macro_rules! flush_text {
            () => {
                if has_text {
                    let t = std::mem::take(&mut text);
                    match last_child {
                        Some(c) => self.nodes[c].tail = Some(t),
                        None => self.nodes[id].text = Some(t),
                    }
                }
                #[allow(unused_assignments)]
                {
                    has_text = false;
                }
            };
        }
        loop {
            if self.pos >= self.b.len() {
                return self.err(format!("Premature end of data in tag {qname}"));
            }
            if self.b[self.pos] == b'<' {
                if self.starts_with("</") {
                    self.advance(2);
                    let name = self.read_name()?;
                    if name != qname {
                        return self.err(format!(
                            "Opening and ending tag mismatch: {qname} and {name}"
                        ));
                    }
                    self.skip_ws();
                    if self.pos >= self.b.len() || self.b[self.pos] != b'>' {
                        return self.err("expected '>'");
                    }
                    self.advance(1);
                    flush_text!();
                    return Ok(id);
                } else if self.starts_with("<!--") {
                    flush_text!();
                    let end = self.find("-->").ok_or_else(|| XmlError {
                        message: "Comment not terminated".into(),
                        line: self.error_line(),
                    })?;
                    let mut body = String::new();
                    normalize_newlines(&self.src[self.pos + 4..end], &mut body);
                    let c = self.new_node(NodeKind::Comment, Some(id), nsmap.clone());
                    self.nodes[c].text = Some(body);
                    self.nodes[id].children.push(c);
                    self.advance(end + 3 - self.pos);
                    last_child = Some(c);
                } else if self.starts_with("<![CDATA[") {
                    let end = self.find("]]>").ok_or_else(|| XmlError {
                        message: "CData section not finished".into(),
                        line: self.error_line(),
                    })?;
                    normalize_newlines(&self.src[self.pos + 9..end], &mut text);
                    has_text = true;
                    self.advance(end + 3 - self.pos);
                } else if self.starts_with("<?") {
                    flush_text!();
                    let end = self.find("?>").ok_or_else(|| XmlError {
                        message: "PI not terminated".into(),
                        line: self.error_line(),
                    })?;
                    let body = &self.src[self.pos + 2..end];
                    let (target, rest) = match body.find(|c: char| c.is_ascii_whitespace()) {
                        Some(i) => (&body[..i], body[i..].trim_start()),
                        None => (body, ""),
                    };
                    let c = self.new_node(NodeKind::Pi, Some(id), nsmap.clone());
                    self.nodes[c].tag = target.to_owned();
                    let mut text = String::new();
                    normalize_newlines(rest, &mut text);
                    self.nodes[c].text = Some(text);
                    self.nodes[id].children.push(c);
                    self.advance(end + 2 - self.pos);
                    last_child = Some(c);
                } else {
                    flush_text!();
                    let c = self.parse_element(Some(id), &nsmap)?;
                    last_child = Some(c);
                }
            } else {
                let end = memchr::memchr(b'<', &self.b[self.pos..])
                    .map_or(self.b.len(), |i| self.pos + i);
                let raw = &self.src[self.pos..end];
                decode_text(raw, &mut text, false).map_err(|m| XmlError {
                    message: m,
                    line: self.error_line(),
                })?;
                has_text = true;
                self.advance(end - self.pos);
            }
        }
    }

    fn read_attr_value(&mut self) -> Result<String, XmlError> {
        if self.pos >= self.b.len() {
            return self.err("AttValue: \" or ' expected");
        }
        let q = self.b[self.pos];
        if q != b'"' && q != b'\'' {
            return self.err("AttValue: \" or ' expected");
        }
        let start = self.pos + 1;
        let end = memchr::memchr(q, &self.b[start..]).map(|i| start + i);
        let Some(end) = end else {
            return self.err("AttValue: ' expected");
        };
        let raw = &self.src[start..end];
        if raw.contains('<') {
            return self.err("Unescaped '<' not allowed in attributes values");
        }
        let mut out = String::with_capacity(raw.len());
        decode_text(raw, &mut out, true).map_err(|m| XmlError {
            message: m,
            line: self.error_line(),
        })?;
        self.advance(end + 1 - self.pos);
        Ok(out)
    }
}

/// Append `s` to `out` with `\r\n` and `\r` replaced by `\n`.
fn normalize_newlines(s: &str, out: &mut String) {
    let mut rest = s;
    while let Some(i) = memchr::memchr(b'\r', rest.as_bytes()) {
        out.push_str(&rest[..i]);
        out.push('\n');
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('\n') {
            rest = r;
        }
    }
    out.push_str(rest);
}

/// Decode entities; `attr` additionally normalises literal whitespace to spaces.
fn decode_text(raw: &str, out: &mut String, attr: bool) -> Result<(), String> {
    let b = raw.as_bytes();
    let mut i = 0;
    let mut start = 0;
    while i < b.len() {
        match b[i] {
            b'&' => {
                out.push_str(&raw[start..i]);
                let semi = raw[i..]
                    .find(';')
                    .ok_or_else(|| "EntityRef: expecting ';'".to_owned())?
                    + i;
                let ent = &raw[i + 1..semi];
                match ent {
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "amp" => out.push('&'),
                    "quot" => out.push('"'),
                    "apos" => out.push('\''),
                    _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                        let cp = u32::from_str_radix(&ent[2..], 16)
                            .map_err(|_| format!("invalid char ref &{ent};"))?;
                        out.push(
                            char::from_u32(cp)
                                .ok_or_else(|| format!("invalid char ref &{ent};"))?,
                        );
                    }
                    _ if ent.starts_with('#') => {
                        let cp: u32 = ent[1..]
                            .parse()
                            .map_err(|_| format!("invalid char ref &{ent};"))?;
                        out.push(
                            char::from_u32(cp)
                                .ok_or_else(|| format!("invalid char ref &{ent};"))?,
                        );
                    }
                    _ => return Err(format!("Entity '{ent}' not defined")),
                }
                i = semi + 1;
                start = i;
            }
            b'\r' => {
                out.push_str(&raw[start..i]);
                if i + 1 < b.len() && b[i + 1] == b'\n' {
                    i += 1;
                }
                out.push(if attr { ' ' } else { '\n' });
                i += 1;
                start = i;
            }
            b'\n' | b'\t' if attr => {
                out.push_str(&raw[start..i]);
                out.push(' ');
                i += 1;
                start = i;
            }
            // Attribute values are short: a byte loop beats memchr there.
            _ if attr => i += 1,
            _ => i = memchr::memchr2(b'&', b'\r', &b[i..]).map_or(b.len(), |j| i + j),
        }
    }
    out.push_str(&raw[start..]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_tail() {
        let d = Document::parse("<a>x<b k=\"1\"/>y<!--c-->z</a>").unwrap();
        let a = d.node(d.root);
        assert_eq!(a.text.as_deref(), Some("x"));
        let b = d.node(a.children[0]);
        assert_eq!(b.tag, "b");
        assert_eq!(b.tail.as_deref(), Some("y"));
        assert_eq!(d.node(a.children[1]).tail.as_deref(), Some("z"));
    }

    #[test]
    fn entities_and_attrs() {
        let d = Document::parse("<a t=\"&#13;&#9;\r\n\" u='x\ny'>&amp;lt;<![CDATA[<&>]]>\r\n</a>")
            .unwrap();
        let a = d.node(d.root);
        assert_eq!(a.get("t"), Some("\r\t "));
        assert_eq!(a.get("u"), Some("x y"));
        assert_eq!(a.text.as_deref(), Some("&lt;<&>\n"));
    }

    #[test]
    fn newlines() {
        let mut out = String::new();
        normalize_newlines("a\r\nb\rc\r\r\nd\r", &mut out);
        assert_eq!(out, "a\nb\nc\n\nd\n");
        let d = Document::parse("<a><!--x\r\ny--><?p a\rb?><![CDATA[1\r\n2]]></a>").unwrap();
        let a = d.node(d.root);
        assert_eq!(d.node(a.children[0]).text.as_deref(), Some("x\ny"));
        assert_eq!(d.node(a.children[1]).text.as_deref(), Some("a\nb"));
        assert_eq!(d.node(a.children[1]).tail.as_deref(), Some("1\n2"));
        assert_eq!(d.node(a.children[0]).line, 1);
    }

    #[test]
    fn lines() {
        let d = Document::parse("<a>\n<b/>\n\n<c>\n</c><!--\n--><d/></a>").unwrap();
        let a = d.node(d.root);
        let line = |i: usize| d.node(a.children[i]).line;
        assert_eq!(
            (a.line, line(0), line(1), line(2), line(3)),
            (1, 2, 4, 5, 6)
        );
        let e = Document::parse("<a>\n\n<b></a>").err().unwrap();
        assert_eq!(e.line, 3);
    }

    #[test]
    fn serialize_namespaces() {
        let src = "<a xmlns:x=\"X\"><x:b><c x:k=\"1\"/></x:b><d xmlns:y=\"Y\"/></a>";
        let d = Document::parse(src).unwrap();
        assert_eq!(d.to_string(d.root, false), src);
    }

    #[test]
    fn namespaces() {
        let d = Document::parse("<a xmlns:x=\"X\" xmlns=\"D\"><b xmlns:y=\"Y\" x:k=\"1\" y:j=\"2\" xml:space=\"p\"/></a>").unwrap();
        let b = d.node(d.node(d.root).children[0]);
        assert_eq!(b.tag, "{D}b");
        assert_eq!(b.prefix, None);
        assert_eq!(
            *b.nsmap,
            vec![
                (Some("y".into()), "Y".into()),
                (Some("x".into()), "X".into()),
                (None, "D".into())
            ]
        );
        assert_eq!(b.attrs[0].0, "{X}k");
        assert_eq!(b.attrs[2].0, format!("{{{XML_NS}}}space"));
    }
}
