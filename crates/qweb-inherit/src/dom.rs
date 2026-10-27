//! A minimal W3C-DOM-like tree (separate text nodes, qualified names) used by
//! the browser dialect of template inheritance (`template_inheritance.js`).
//!
//! Nodes live in an arena ([`Dom`]); a tree is referenced by its root id.
//! Freed subtrees are recycled.

use std::hash::Hash;

use qweb_core::xml::{Document, NodeKind, XML_NS, local_name, namespace};

use crate::xpath::{Kind, Tree};

pub type DomId = usize;

pub const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
pub const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomKind {
    Element,
    Text,
    Comment,
    Pi,
}

#[derive(Debug, Clone)]
pub struct DomAttr {
    pub qname: String,
    pub ns: Option<String>,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct DomNode {
    pub kind: DomKind,
    /// Element qualified name, PI target.
    pub name: String,
    pub ns: Option<String>,
    pub attrs: Vec<DomAttr>,
    /// Text, comment or PI data.
    pub data: String,
    /// Translation context of a text node (`contextByTextNode`).
    pub tctx: Option<String>,
    pub children: Vec<DomId>,
    pub parent: Option<DomId>,
    /// The node is the document element of some `Document`.
    pub doc_child: bool,
    free: bool,
}

impl DomNode {
    fn new(kind: DomKind) -> DomNode {
        DomNode {
            kind,
            name: String::new(),
            ns: None,
            attrs: Vec::new(),
            data: String::new(),
            tctx: None,
            children: Vec::new(),
            parent: None,
            doc_child: false,
            free: false,
        }
    }

    pub fn local_name(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    pub fn get(&self, qname: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.qname == qname).map(|a| a.value.as_str())
    }

    pub fn has(&self, qname: &str) -> bool {
        self.attrs.iter().any(|a| a.qname == qname)
    }

    /// `setAttribute(qname, value)` (null namespace when created).
    pub fn set(&mut self, qname: &str, value: impl Into<String>) {
        let value = value.into();
        match self.attrs.iter_mut().find(|a| a.qname == qname) {
            Some(a) => a.value = value,
            None => self.attrs.push(DomAttr { qname: qname.to_owned(), ns: None, value }),
        }
    }

    pub fn remove_attr(&mut self, qname: &str) {
        self.attrs.retain(|a| a.qname != qname);
    }
}

/// DOM `HierarchyRequestError` and friends.
#[derive(Debug, Clone, PartialEq)]
pub struct DomError(pub String);

fn hierarchy<T>() -> Result<T, DomError> {
    Err(DomError("HierarchyRequestError: The operation would yield an incorrect node tree.".into()))
}

#[derive(Debug, Default, Clone)]
pub struct Dom {
    pub nodes: Vec<DomNode>,
    free: Vec<DomId>,
    /// Nodes allocated since the last [`Dom::checkpoint`].
    log: Option<Vec<DomId>>,
}

impl Dom {
    pub fn new() -> Dom {
        Dom::default()
    }

    #[inline]
    pub fn node(&self, id: DomId) -> &DomNode {
        &self.nodes[id]
    }

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

    pub fn create_element(&mut self, qname: &str, ns: Option<&str>) -> DomId {
        let mut n = DomNode::new(DomKind::Element);
        n.name = qname.to_owned();
        n.ns = ns.map(str::to_owned);
        self.alloc(n)
    }

    pub fn create_text(&mut self, data: &str) -> DomId {
        let mut n = DomNode::new(DomKind::Text);
        n.data = data.to_owned();
        self.alloc(n)
    }

    pub fn create_comment(&mut self, data: &str) -> DomId {
        let mut n = DomNode::new(DomKind::Comment);
        n.data = data.to_owned();
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
        let doomed: Vec<DomId> = log.into_iter().filter(|&id| self.is_live(id) && !keep.contains(&self.top(id))).collect();
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

    fn import_lxml(&mut self, doc: &Document, id: usize, parent_ns: Option<&qweb_core::xml::NsMap>) -> DomId {
        let src = doc.node(id);
        let new = match src.kind {
            NodeKind::Comment => self.create_comment(src.text.as_deref().unwrap_or("")),
            NodeKind::Pi => {
                let mut n = DomNode::new(DomKind::Pi);
                n.name = src.tag.clone();
                n.data = src.text.clone().unwrap_or_default();
                self.alloc(n)
            }
            NodeKind::Element => {
                let ns = namespace(&src.tag).map(str::to_owned);
                let local = local_name(&src.tag);
                let qname = match (&ns, &src.prefix) {
                    (Some(_), Some(p)) => format!("{p}:{local}"),
                    _ => local.to_owned(),
                };
                let mut n = DomNode::new(DomKind::Element);
                n.name = qname;
                n.ns = ns;
                for (prefix, uri) in src.nsmap.iter() {
                    let inherited = parent_ns.is_some_and(|p| p.iter().any(|(q, u)| q == prefix && u == uri));
                    if !inherited {
                        let qname = match prefix {
                            Some(p) => format!("xmlns:{p}"),
                            None => "xmlns".to_owned(),
                        };
                        n.attrs.push(DomAttr { qname, ns: Some(XMLNS_NS.into()), value: uri.clone() });
                    }
                }
                for (k, v) in &src.attrs {
                    let ans = namespace(k);
                    let local = local_name(k);
                    let qname = match ans {
                        Some(XML_NS) => format!("xml:{local}"),
                        Some(uri) => match src.nsmap.iter().find(|(q, u)| q.is_some() && u == uri) {
                            Some((Some(p), _)) => format!("{p}:{local}"),
                            _ => local.to_owned(),
                        },
                        None => local.to_owned(),
                    };
                    n.attrs.push(DomAttr { qname, ns: ans.map(str::to_owned), value: v.clone() });
                }
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
        };
        new
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

    pub fn index_in_parent(&self, id: DomId) -> Option<usize> {
        let p = self.nodes[id].parent?;
        self.nodes[p].children.iter().position(|&c| c == id)
    }

    pub fn previous_sibling(&self, id: DomId) -> Option<DomId> {
        let p = self.nodes[id].parent?;
        let i = self.index_in_parent(id)?;
        i.checked_sub(1).map(|i| self.nodes[p].children[i])
    }

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
    fn insert_nodes(&mut self, parent: DomId, nodes: &[DomId], reference: Option<DomId>) -> Result<(), DomError> {
        for &n in nodes {
            if self.is_inclusive_ancestor(n, parent) {
                return hierarchy();
            }
        }
        for &n in nodes {
            self.detach(n);
        }
        let mut idx = match reference {
            Some(r) => self.index_in_parent(r).unwrap_or(self.nodes[parent].children.len()),
            None => self.nodes[parent].children.len(),
        };
        for &n in nodes {
            self.nodes[parent].children.insert(idx, n);
            self.nodes[n].parent = Some(parent);
            idx += 1;
        }
        Ok(())
    }

    /// Siblings of a document element: only comments and PIs are allowed
    /// (and they leave the element tree).
    fn doc_level(&mut self, nodes: &[DomId]) -> Result<(), DomError> {
        if nodes.iter().any(|&n| matches!(self.nodes[n].kind, DomKind::Element | DomKind::Text)) {
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
            return if self.nodes[target].doc_child { self.doc_level(nodes) } else { Ok(()) };
        };
        let i = self.index_in_parent(target).unwrap();
        let viable_prev = self.nodes[parent].children[..i].iter().rev().copied().find(|c| !nodes.contains(c));
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
            return if self.nodes[target].doc_child { self.doc_level(nodes) } else { Ok(()) };
        };
        let i = self.index_in_parent(target).unwrap();
        let viable_next = self.nodes[parent].children[i + 1..].iter().copied().find(|c| !nodes.contains(c));
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
        let viable_next = self.nodes[parent].children[i + 1..].iter().copied().find(|c| !nodes.contains(c));
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
        let first = self.nodes[parent].children.first().copied().filter(|f| !nodes.contains(f));
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

fn escape(s: &str, out: &mut String, attr: bool) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\t' if attr => out.push_str("&#9;"),
            '\n' if attr => out.push_str("&#10;"),
            '\r' if attr => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
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
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// XPath view of a [`Dom`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DNode {
    /// Document node above a top-level node.
    Doc(DomId),
    Node(DomId),
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
                (a.ns.as_deref(), a.qname.rsplit(':').next().unwrap_or(&a.qname))
            }
            DNode::Doc(_) => (None, ""),
        }
    }

    fn qname(&self, n: DNode) -> String {
        match n {
            DNode::Node(id) => self.nodes[id].name.clone(),
            DNode::Attr(id, i) => self.nodes[id].attrs[i as usize].qname.clone(),
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
