//! A minimal W3C-DOM-like tree (separate text nodes, qualified names) used by
//! the browser dialect of template inheritance (`template_inheritance.js`).
//!
//! Nodes live in an arena ([`Dom`]); a tree is referenced by its root id.
//! Freed subtrees are recycled.

use core::hash::Hash;

use qweb_core::xml::{Document, NodeKind, XML_NS, XStr, local_name, namespace};

use crate::xpath::{Kind, Tree};

/// Index of a node in a [`Dom`] arena.
pub type DomId = usize;

/// Namespace of `xmlns` attributes.
pub const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
/// XHTML namespace (`DOMParser` text/html elements, wrapper `t` elements).
pub const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// DOM `nodeType` of a [`DomNode`].
pub enum DomKind {
    /// `ELEMENT_NODE`.
    Element,
    /// `TEXT_NODE` (also CDATA, which the parser turns into text).
    Text,
    /// `COMMENT_NODE`.
    Comment,
    /// `PROCESSING_INSTRUCTION_NODE`.
    Pi,
}

#[derive(Debug, Clone)]
/// An attribute (`Attr`).
pub struct DomAttr {
    /// Qualified name, as written (`prefix:local`).
    pub qname: XStr,
    /// Namespace URI (`null` when created by `setAttribute`).
    pub ns: Option<String>,
    /// Attribute value.
    pub value: XStr,
}

#[derive(Debug, Clone)]
/// A node of a [`Dom`] arena.
pub struct DomNode {
    /// Node type.
    pub kind: DomKind,
    /// Element qualified name, PI target.
    pub name: XStr,
    /// Element namespace URI.
    pub ns: Option<String>,
    /// Element attributes, in order.
    pub attrs: Vec<DomAttr>,
    /// Text, comment or PI data.
    pub data: XStr,
    /// Translation context of a text node (`contextByTextNode`).
    pub tctx: Option<String>,
    /// Child nodes, in order.
    pub children: Vec<DomId>,
    /// Parent node, `None` for a detached or document-level node.
    pub parent: Option<DomId>,
    /// The node is the document element of some `Document`.
    pub doc_child: bool,
    free: bool,
}

impl DomNode {
    fn new(kind: DomKind) -> DomNode {
        DomNode {
            kind,
            name: XStr::new(),
            ns: None,
            attrs: Vec::new(),
            data: XStr::new(),
            tctx: None,
            children: Vec::new(),
            parent: None,
            doc_child: false,
            free: false,
        }
    }

    /// `localName`: the qualified name without its prefix.
    pub fn local_name(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// `getAttribute(qname)`.
    pub fn get(&self, qname: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.qname == qname)
            .map(|a| a.value.as_str())
    }

    /// `hasAttribute(qname)`.
    pub fn has(&self, qname: &str) -> bool {
        self.attrs.iter().any(|a| a.qname == qname)
    }

    /// `setAttribute(qname, value)` (null namespace when created).
    pub fn set(&mut self, qname: &str, value: impl Into<XStr>) {
        let value = value.into();
        match self.attrs.iter_mut().find(|a| a.qname == qname) {
            Some(a) => a.value = value,
            None => self.attrs.push(DomAttr {
                qname: qname.into(),
                ns: None,
                value,
            }),
        }
    }

    /// `removeAttribute(qname)`.
    pub fn remove_attr(&mut self, qname: &str) {
        self.attrs.retain(|a| a.qname != qname);
    }
}

/// DOM `HierarchyRequestError` and friends.
#[derive(Debug, Clone, PartialEq)]
pub struct DomError(pub String);

fn hierarchy<T>() -> Result<T, DomError> {
    Err(DomError(
        "HierarchyRequestError: The operation would yield an incorrect node tree.".into(),
    ))
}

/// Arena holding any number of DOM trees.
#[derive(Debug, Default, Clone)]
pub struct Dom {
    /// All nodes, live and free (see [`Dom::is_live`]).
    pub nodes: Vec<DomNode>,
    free: Vec<DomId>,
    /// Nodes allocated since the last [`Dom::checkpoint`].
    log: Option<Vec<DomId>>,
}

impl Dom {
    /// An empty arena.
    pub fn new() -> Dom {
        Dom::default()
    }

    /// The node `id`.
    #[inline]
    pub fn node(&self, id: DomId) -> &DomNode {
        &self.nodes[id]
    }

    /// The node `id`, mutably.
    #[inline]
    pub fn node_mut(&mut self, id: DomId) -> &mut DomNode {
        &mut self.nodes[id]
    }

    fn alloc(&mut self, node: DomNode) -> DomId {
        let id = self.alloc_inner(node);
        if let Some(log) = &mut self.log {
            log.push(id);
        }
        id
    }

    fn alloc_inner(&mut self, node: DomNode) -> DomId {
        match self.free.pop() {
            Some(id) => {
                self.nodes[id] = node;
                id
            }
            None => {
                self.nodes.push(node);
                self.nodes.len() - 1
            }
        }
    }

    /// `createElementNS(ns, qname)`: a detached element.
    pub fn create_element(&mut self, qname: &str, ns: Option<&str>) -> DomId {
        let mut n = DomNode::new(DomKind::Element);
        n.name = qname.into();
        n.ns = ns.map(str::to_owned);
        self.alloc(n)
    }

    /// `createTextNode(data)`.
    pub fn create_text(&mut self, data: &str) -> DomId {
        let mut n = DomNode::new(DomKind::Text);
        n.data = data.into();
        self.alloc(n)
    }

    /// `createComment(data)`.
    pub fn create_comment(&mut self, data: &str) -> DomId {
        let mut n = DomNode::new(DomKind::Comment);
        n.data = data.into();
        self.alloc(n)
    }

    /// Release a detached subtree.
    pub fn free(&mut self, id: DomId) {
        if self.nodes.get(id).is_none_or(|n| n.free) {
            return;
        }
        self.detach(id);
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            let n = &mut self.nodes[x];
            stack.extend(std::mem::take(&mut n.children));
            *n = DomNode::new(DomKind::Text);
            n.free = true;
            self.free.push(x);
        }
    }

    /// Start recording allocations.
    pub fn checkpoint(&mut self) {
        self.log = Some(Vec::new());
    }

    /// Free every node allocated since [`Dom::checkpoint`] whose tree root is
    /// not in `keep` (temporary clones, sentinels, removed nodes...).
    pub fn release(&mut self, keep: &[DomId]) {
        let Some(log) = self.log.take() else { return };
        let keep: std::collections::HashSet<DomId> = keep.iter().copied().collect();
        let doomed: Vec<DomId> = log
            .into_iter()
            .filter(|&id| self.is_live(id) && !keep.contains(&self.top(id)))
            .collect();
        for &id in &doomed {
            if let Some(p) = self.nodes[id].parent
                && !self.nodes[p].free
                && keep.contains(&self.top(p))
            {
                self.detach(id);
            }
        }
        for id in doomed {
            let n = &mut self.nodes[id];
            *n = DomNode::new(DomKind::Text);
            n.free = true;
            self.free.push(id);
        }
    }

    /// The node exists and has not been freed.
    pub fn is_live(&self, id: DomId) -> bool {
        self.nodes.get(id).is_some_and(|n| !n.free)
    }

    /// Parse an XML string (like `DOMParser.parseFromString(s, "text/xml")`)
    /// and return the document element (marked as a document child).
    pub fn parse(&mut self, xml: &str) -> Result<DomId, DomError> {
        let doc = Document::parse(xml).map_err(|e| DomError(format!("XML parsing error: {e}")))?;
        let root = self.import_lxml(&doc, doc.root, None);
        self.nodes[root].doc_child = true;
        Ok(root)
    }

    fn import_lxml(
        &mut self,
        doc: &Document,
        id: usize,
        parent_ns: Option<&qweb_core::xml::NsMap>,
    ) -> DomId {
        let src = doc.node(id);
        match src.kind {
            NodeKind::Comment => {
                let mut n = DomNode::new(DomKind::Comment);
                n.data = src.text.clone().unwrap_or_default();
                self.alloc(n)
            }
            NodeKind::Pi => {
                let mut n = DomNode::new(DomKind::Pi);
                n.name = src.tag.clone();
                n.data = src.text.clone().unwrap_or_default();
                self.alloc(n)
            }
            NodeKind::Element => {
                let ns = namespace(&src.tag).map(str::to_owned);
                let local = local_name(&src.tag);
                let qname: XStr = match (&ns, &src.prefix) {
                    (Some(_), Some(p)) => format!("{p}:{local}").into(),
                    // the tag itself when it has no namespace (no copy for short tags)
                    _ if ns.is_none() => src.tag.clone(),
                    _ => local.into(),
                };
                let mut n = DomNode::new(DomKind::Element);
                n.name = qname;
                n.ns = ns;
                n.attrs.reserve_exact(src.attrs.len());
                for (prefix, uri) in src.nsmap.iter() {
                    let inherited =
                        parent_ns.is_some_and(|p| p.iter().any(|(q, u)| q == prefix && u == uri));
                    if !inherited {
                        let qname: XStr = match prefix {
                            Some(p) => format!("xmlns:{p}").into(),
                            None => "xmlns".into(),
                        };
                        n.attrs.push(DomAttr {
                            qname,
                            ns: Some(XMLNS_NS.into()),
                            value: uri.as_str().into(),
                        });
                    }
                }
                for (k, v) in &src.attrs {
                    let ans = namespace(k);
                    let local = local_name(k);
                    let qname: XStr = match ans {
                        Some(XML_NS) => format!("xml:{local}").into(),
                        Some(uri) => {
                            match src.nsmap.iter().find(|(q, u)| q.is_some() && u == uri) {
                                Some((Some(p), _)) => format!("{p}:{local}").into(),
                                _ => local.into(),
                            }
                        }
                        // no namespace: the name is the key itself
                        None => k.clone(),
                    };
                    n.attrs.push(DomAttr {
                        qname,
                        ns: ans.map(str::to_owned),
                        value: v.clone(),
                    });
                }
                // children: the text, each child and its tail
                let kids = usize::from(src.text.is_some())
                    + src
                        .children
                        .iter()
                        .map(|&c| 1 + usize::from(doc.node(c).tail.is_some()))
                        .sum::<usize>();
                n.children.reserve_exact(kids);
                let el = self.alloc(n);
                if let Some(t) = &src.text {
                    let t = self.create_text(t);
                    self.append_raw(el, t);
                }
                for &c in &src.children {
                    let cc = self.import_lxml(doc, c, Some(&src.nsmap));
                    self.append_raw(el, cc);
                    if let Some(t) = &doc.node(c).tail {
                        let t = self.create_text(t);
                        self.append_raw(el, t);
                    }
                }
                el
            }
        }
    }

    fn append_raw(&mut self, parent: DomId, child: DomId) {
        self.nodes[parent].children.push(child);
        self.nodes[child].parent = Some(parent);
    }

    /// `node.cloneNode(true)` keeping text translation contexts (`deepClone`).
    pub fn deep_clone(&mut self, id: DomId) -> DomId {
        let mut n = self.nodes[id].clone();
        let children = std::mem::take(&mut n.children);
        n.parent = None;
        n.doc_child = false;
        let new = self.alloc(n);
        for c in children {
            let cc = self.deep_clone(c);
            self.append_raw(new, cc);
        }
        new
    }

    /// Position of `id` among its parent's children.
    pub fn index_in_parent(&self, id: DomId) -> Option<usize> {
        let p = self.nodes[id].parent?;
        self.nodes[p].children.iter().position(|&c| c == id)
    }

    /// `previousSibling`.
    pub fn previous_sibling(&self, id: DomId) -> Option<DomId> {
        let p = self.nodes[id].parent?;
        let i = self.index_in_parent(id)?;
        i.checked_sub(1).map(|i| self.nodes[p].children[i])
    }

    /// `nextSibling`.
    pub fn next_sibling(&self, id: DomId) -> Option<DomId> {
        let p = self.nodes[id].parent?;
        let i = self.index_in_parent(id)?;
        self.nodes[p].children.get(i + 1).copied()
    }

    /// `node.remove()`.
    pub fn detach(&mut self, id: DomId) {
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p].children.retain(|&c| c != id);
        }
        self.nodes[id].doc_child = false;
    }

    fn is_inclusive_ancestor(&self, anc: DomId, mut node: DomId) -> bool {
        loop {
            if node == anc {
                return true;
            }
            match self.nodes[node].parent {
                Some(p) => node = p,
                None => return false,
            }
        }
    }

    /// Insert `nodes` (moved) into `parent` before `reference` (or at the end).
    fn insert_nodes(
        &mut self,
        parent: DomId,
        nodes: &[DomId],
        reference: Option<DomId>,
    ) -> Result<(), DomError> {
        for &n in nodes {
            if self.is_inclusive_ancestor(n, parent) {
                return hierarchy();
            }
        }
        for &n in nodes {
            self.detach(n);
        }
        let idx = match reference {
            Some(r) => self
                .index_in_parent(r)
                .unwrap_or(self.nodes[parent].children.len()),
            None => self.nodes[parent].children.len(),
        };
        self.nodes[parent]
            .children
            .splice(idx..idx, nodes.iter().copied());
        for &n in nodes {
            self.nodes[n].parent = Some(parent);
        }
        Ok(())
    }

    /// Siblings of a document element: only comments and PIs are allowed
    /// (and they leave the element tree).
    fn doc_level(&mut self, nodes: &[DomId]) -> Result<(), DomError> {
        if nodes
            .iter()
            .any(|&n| matches!(self.nodes[n].kind, DomKind::Element | DomKind::Text))
        {
            return hierarchy();
        }
        for &n in nodes {
            self.detach(n);
        }
        Ok(())
    }

    /// `target.before(...nodes)`.
    pub fn before(&mut self, target: DomId, nodes: &[DomId]) -> Result<(), DomError> {
        let Some(parent) = self.nodes[target].parent else {
            return if self.nodes[target].doc_child {
                self.doc_level(nodes)
            } else {
                Ok(())
            };
        };
        let i = self.index_in_parent(target).unwrap();
        let viable_prev = self.nodes[parent].children[..i]
            .iter()
            .rev()
            .copied()
            .find(|c| !nodes.contains(c));
        for &n in nodes {
            if self.is_inclusive_ancestor(n, parent) {
                return hierarchy();
            }
        }
        for &n in nodes {
            self.detach(n);
        }
        let reference = match viable_prev {
            Some(p) => self.next_sibling(p),
            None => self.nodes[parent].children.first().copied(),
        };
        self.insert_nodes(parent, nodes, reference)
    }

    /// `target.after(...nodes)`.
    pub fn after(&mut self, target: DomId, nodes: &[DomId]) -> Result<(), DomError> {
        let Some(parent) = self.nodes[target].parent else {
            return if self.nodes[target].doc_child {
                self.doc_level(nodes)
            } else {
                Ok(())
            };
        };
        let i = self.index_in_parent(target).unwrap();
        let viable_next = self.nodes[parent].children[i + 1..]
            .iter()
            .copied()
            .find(|c| !nodes.contains(c));
        self.insert_nodes(parent, nodes, viable_next)
    }

    /// `target.replaceWith(...nodes)`.
    pub fn replace_with(&mut self, target: DomId, nodes: &[DomId]) -> Result<(), DomError> {
        let Some(parent) = self.nodes[target].parent else {
            if self.nodes[target].doc_child {
                self.doc_level(nodes)?;
                self.detach(target);
            }
            return Ok(());
        };
        let i = self.index_in_parent(target).unwrap();
        let viable_next = self.nodes[parent].children[i + 1..]
            .iter()
            .copied()
            .find(|c| !nodes.contains(c));
        for &n in nodes {
            if self.is_inclusive_ancestor(n, parent) {
                return hierarchy();
            }
        }
        for &n in nodes {
            self.detach(n);
        }
        if self.nodes[target].parent == Some(parent) {
            let idx = self.index_in_parent(target).unwrap();
            self.detach(target);
            let reference = self.nodes[parent].children.get(idx).copied();
            self.insert_nodes(parent, nodes, reference)
        } else {
            self.insert_nodes(parent, nodes, viable_next)
        }
    }

    /// `parent.append(...nodes)`.
    pub fn append(&mut self, parent: DomId, nodes: &[DomId]) -> Result<(), DomError> {
        self.insert_nodes(parent, nodes, None)
    }

    /// `parent.prepend(...nodes)`.
    pub fn prepend(&mut self, parent: DomId, nodes: &[DomId]) -> Result<(), DomError> {
        let first = self.nodes[parent]
            .children
            .first()
            .copied()
            .filter(|f| !nodes.contains(f));
        self.insert_nodes(parent, nodes, first)
    }

    /// Topmost ancestor (`getRoot`: climbs `parentElement`).
    pub fn top(&self, mut id: DomId) -> DomId {
        while let Some(p) = self.nodes[id].parent {
            id = p;
        }
        id
    }

    /// `XMLSerializer.serializeToString(node)` (no namespace fix-ups).
    pub fn serialize(&self, id: DomId) -> String {
        let mut out = String::new();
        self.ser(id, &mut out);
        out
    }

    fn ser(&self, id: DomId, out: &mut String) {
        let n = &self.nodes[id];
        match n.kind {
            DomKind::Text => escape(&n.data, out, false),
            DomKind::Comment => {
                out.push_str("<!--");
                out.push_str(&n.data);
                out.push_str("-->");
            }
            DomKind::Pi => {
                out.push_str("<?");
                out.push_str(&n.name);
                if !n.data.is_empty() {
                    out.push(' ');
                    out.push_str(&n.data);
                }
                out.push_str("?>");
            }
            DomKind::Element => {
                out.push('<');
                out.push_str(&n.name);
                for a in &n.attrs {
                    out.push(' ');
                    out.push_str(&a.qname);
                    out.push_str("=\"");
                    escape(&a.value, out, true);
                    out.push('"');
                }
                if n.children.is_empty() {
                    out.push_str("/>");
                    return;
                }
                out.push('>');
                for &c in &n.children {
                    self.ser(c, out);
                }
                out.push_str("</");
                out.push_str(&n.name);
                out.push('>');
            }
        }
    }

    /// JSON description of a tree, to rebuild it node for node in a browser:
    /// element `[1, qname, ns, [[qname, ns, value], ...], [children]]`,
    /// text `[3, data]`, comment `[8, data]`, PI `[7, target, data]`.
    pub fn to_json(&self, id: DomId) -> String {
        let mut out = String::new();
        self.json(id, &mut out);
        out
    }

    fn json(&self, id: DomId, out: &mut String) {
        let n = &self.nodes[id];
        match n.kind {
            DomKind::Text => {
                out.push_str("[3,");
                json_str(&n.data, out);
                out.push(']');
            }
            DomKind::Comment => {
                out.push_str("[8,");
                json_str(&n.data, out);
                out.push(']');
            }
            DomKind::Pi => {
                out.push_str("[7,");
                json_str(&n.name, out);
                out.push(',');
                json_str(&n.data, out);
                out.push(']');
            }
            DomKind::Element => {
                out.push_str("[1,");
                json_str(&n.name, out);
                out.push(',');
                json_opt(n.ns.as_deref(), out);
                out.push_str(",[");
                for (i, a) in n.attrs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    json_str(&a.qname, out);
                    out.push(',');
                    json_opt(a.ns.as_deref(), out);
                    out.push(',');
                    json_str(&a.value, out);
                    out.push(']');
                }
                out.push_str("],[");
                for (i, &c) in n.children.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.json(c, out);
                }
                out.push_str("]]");
            }
        }
    }
}

/// `XMLSerializer` escaping of text (`attr`: of an attribute value).
fn escape(s: &str, out: &mut String, attr: bool) {
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' if attr => "&quot;",
            b'\t' if attr => "&#9;",
            b'\n' if attr => "&#10;",
            b'\r' if attr => "&#13;",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        out.push_str(rep);
        start = i + 1;
    }
    out.push_str(&s[start..]);
}

fn json_opt(s: Option<&str>, out: &mut String) {
    match s {
        Some(s) => json_str(s, out),
        None => out.push_str("null"),
    }
}

/// `JSON.stringify(string)`.
pub fn json_str(s: &str, out: &mut String) {
    out.push('"');
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            0x08 => "\\b",
            0x0c => "\\f",
            0..0x20 => {
                out.push_str(&s[start..i]);
                out.push_str(&format!("\\u{b:04x}"));
                start = i + 1;
                continue;
            }
            _ => continue,
        };
        out.push_str(&s[start..i]);
        out.push_str(rep);
        start = i + 1;
    }
    out.push_str(&s[start..]);
    out.push('"');
}

/// XPath view of a [`Dom`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A node of the XPath data model over a [`Dom`].
pub enum DNode {
    /// Document node above a top-level node.
    Doc(DomId),
    /// Element, text, comment or processing instruction.
    Node(DomId),
    /// Attribute `index` of an element.
    Attr(DomId, u32),
}

impl Tree for Dom {
    type N = DNode;

    fn kind(&self, n: DNode) -> Kind {
        match n {
            DNode::Doc(_) => Kind::Root,
            DNode::Attr(..) => Kind::Attr,
            DNode::Node(id) => match self.nodes[id].kind {
                DomKind::Element => Kind::Element,
                DomKind::Text => Kind::Text,
                DomKind::Comment => Kind::Comment,
                DomKind::Pi => Kind::Pi,
            },
        }
    }

    fn parent(&self, n: DNode) -> Option<DNode> {
        match n {
            DNode::Doc(_) => None,
            DNode::Attr(id, _) => Some(DNode::Node(id)),
            DNode::Node(id) => Some(match self.nodes[id].parent {
                Some(p) => DNode::Node(p),
                None => DNode::Doc(id),
            }),
        }
    }

    fn children(&self, n: DNode, out: &mut Vec<DNode>) {
        match n {
            DNode::Doc(top) => out.push(DNode::Node(top)),
            DNode::Node(id) if self.nodes[id].kind == DomKind::Element => {
                out.extend(self.nodes[id].children.iter().map(|&c| DNode::Node(c)))
            }
            _ => {}
        }
    }

    fn attributes(&self, n: DNode, out: &mut Vec<DNode>) {
        if let DNode::Node(id) = n {
            for (i, a) in self.nodes[id].attrs.iter().enumerate() {
                if a.ns.as_deref() != Some(XMLNS_NS) {
                    out.push(DNode::Attr(id, i as u32));
                }
            }
        }
    }

    fn name(&self, n: DNode) -> (Option<&str>, &str) {
        match n {
            DNode::Node(id) => {
                let node = &self.nodes[id];
                (node.ns.as_deref(), node.local_name())
            }
            DNode::Attr(id, i) => {
                let a = &self.nodes[id].attrs[i as usize];
                (
                    a.ns.as_deref(),
                    a.qname.rsplit(':').next().unwrap_or(&a.qname),
                )
            }
            DNode::Doc(_) => (None, ""),
        }
    }

    fn qname(&self, n: DNode) -> String {
        match n {
            DNode::Node(id) => self.nodes[id].name.to_string(),
            DNode::Attr(id, i) => self.nodes[id].attrs[i as usize].qname.to_string(),
            DNode::Doc(_) => String::new(),
        }
    }

    fn value(&self, n: DNode) -> &str {
        match n {
            DNode::Node(id) => &self.nodes[id].data,
            DNode::Attr(id, i) => &self.nodes[id].attrs[i as usize].value,
            DNode::Doc(_) => "",
        }
    }

    fn pi_target(&self, n: DNode) -> &str {
        match n {
            DNode::Node(id) => &self.nodes[id].name,
            _ => "",
        }
    }

    fn document(&self, n: DNode) -> DNode {
        match n {
            DNode::Doc(t) => DNode::Doc(t),
            DNode::Node(id) | DNode::Attr(id, _) => DNode::Doc(self.top(id)),
        }
    }

    fn class(&self, n: DNode) -> Option<&str> {
        match n {
            DNode::Node(id) => self.nodes[id].get("class"),
            _ => None,
        }
    }
}
