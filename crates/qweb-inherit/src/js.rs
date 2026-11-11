//! Port of `addons/web/static/src/core/template_inheritance.js` (browser
//! dialect) on the [`Dom`] model.

use crate::dom::{DNode, Dom, DomError, DomId, DomKind, XHTML_NS, json_str};
use crate::xpath::{Dialect, XPath};
use qweb_core::xml::XStr;

const TCTX: &str = "t-translation-context";

/// JS `String.prototype.trim` whitespace.
fn js_is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn trim_end(s: &str) -> String {
    s.trim_end_matches(js_is_space).to_owned()
}

fn trim(s: &str) -> &str {
    s.trim_matches(js_is_space)
}

/// `data.split(/(?=\n[ \t]*$)/)`: `(text1, text2)`.
fn split_rstrip(s: &str) -> (String, Option<String>) {
    let Some(p) = s.rfind('\n') else {
        return (s.to_owned(), None);
    };
    if p == 0 || !s[p + 1..].bytes().all(|b| b == b' ' || b == b'\t') {
        // a zero-width match at index 0 does not split
        return (s.to_owned(), None);
    }
    (s[..p].to_owned(), Some(s[p..].to_owned()))
}

/// Error thrown by `applyInheritance`.
#[derive(Debug, Clone, PartialEq)]
pub struct JsError(pub String);

impl From<DomError> for JsError {
    fn from(e: DomError) -> Self {
        JsError(e.0)
    }
}

type Res<T> = Result<T, JsError>;

/// Rewrite `hasclass(...)` like `getXpath`.
pub fn rewrite_hasclass(xpath: &str) -> String {
    let mut out = String::new();
    let mut rest = xpath;
    while let Some(i) = rest.find("hasclass(") {
        let after = &rest[i + "hasclass(".len()..];
        let Some(close) = after.find(')') else { break };
        out.push_str(&rest[..i]);
        let parts: Vec<String> = after[..close]
            .split(',')
            .map(|c| {
                let c: Vec<char> = trim(c).chars().collect();
                let inner: String = if c.len() >= 2 {
                    c[1..c.len() - 1].iter().collect()
                } else {
                    String::new()
                };
                format!("contains(concat(' ', @class, ' '), ' {inner} ')")
            })
            .collect();
        out.push_str(&parts.join(" and "));
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// `CLASS_CONTAINS_REGEX = /contains\(@class.*\)/g` used with `.test()`:
/// the `lastIndex` state (in UTF-16 units) persists between calls.
#[derive(Debug, Default, Clone)]
#[allow(
    missing_copy_implementations,
    reason = "stateful: an implicit copy would fork `lastIndex`"
)]
pub struct ClassContainsRegex {
    last_index: usize,
}

impl ClassContainsRegex {
    /// `CLASS_CONTAINS_REGEX.test(s)`, updating `lastIndex`.
    pub fn test(&mut self, s: &str) -> bool {
        let units: Vec<u16> = s.encode_utf16().collect();
        if self.last_index > units.len() {
            self.last_index = 0;
            return false;
        }
        let pat: Vec<u16> = "contains(@class".encode_utf16().collect();
        let is_lt = |u: u16| matches!(u, 0x0a | 0x0d | 0x2028 | 0x2029);
        let mut i = self.last_index;
        while i + pat.len() <= units.len() {
            if units[i..i + pat.len()] == pat[..] {
                // `.*\)` greedy within the line
                let mut end = i + pat.len();
                while end < units.len() && !is_lt(units[end]) {
                    end += 1;
                }
                if let Some(j) = (i + pat.len()..end)
                    .rev()
                    .find(|&j| units[j] == u16::from(b')'))
                {
                    self.last_index = j + 1;
                    return true;
                }
            }
            i += 1;
        }
        self.last_index = 0;
        false
    }
}

/// State of the `template_inheritance.js` module.
#[derive(Debug, Default, Clone)]
pub struct JsInheritance {
    /// `odoo.debug`.
    pub debug: bool,
    class_contains: ClassContainsRegex,
    /// `console.warn` messages produced by the last call.
    pub warnings: Vec<String>,
    translation_context: String,
}

impl JsInheritance {
    /// Fresh module state (`odoo.debug` = `debug`).
    pub fn new(debug: bool) -> Self {
        JsInheritance {
            debug,
            ..Default::default()
        }
    }

    fn get_translation_context(dom: &Dom, mut node: DomId) -> Res<String> {
        loop {
            if let Some(v) = dom.node(node).get(TCTX) {
                return Ok(v.to_owned());
            }
            match dom.node(node).parent {
                Some(p) => node = p,
                None => {
                    return Err(JsError(
                        "TypeError: Cannot read properties of null (reading 'hasAttribute')".into(),
                    ));
                }
            }
        }
    }

    fn set_translation_context(&self, dom: &mut Dom, node: DomId) {
        let ctx = self.translation_context.clone();
        let n = dom.node_mut(node);
        match n.kind {
            DomKind::Text => {
                if !trim(&n.data).is_empty() {
                    n.tctx = Some(ctx);
                }
            }
            DomKind::Element => n.set(TCTX, ctx),
            _ => {}
        }
    }

    fn get_xpath(&mut self, dom: &Dom, operation: DomId) -> Res<String> {
        let xpath = dom.node(operation).get("expr").unwrap_or("null").to_owned();
        if self.debug && self.class_contains.test(&xpath) {
            // operation.closest("t[t-inherit]")
            let mut cur = Some(operation);
            let mut parent = None;
            while let Some(c) = cur {
                let n = dom.node(c);
                if n.kind == DomKind::Element && n.name == "t" && n.has("t-inherit") {
                    parent = Some(c);
                    break;
                }
                cur = n.parent;
            }
            let Some(parent) = parent else {
                return Err(JsError(
                    "TypeError: Cannot read properties of null (reading 'getAttribute')".into(),
                ));
            };
            let p = dom.node(parent);
            let name = p
                .get("t-name")
                .filter(|s| !s.is_empty())
                .or(p.get("t-inherit"))
                .unwrap_or("null");
            self.warnings.push(format!(
                "Error-prone use of @class in template \"{name}\" (or one of its inheritors). \
                 Use the hasclass(*classes) function to filter elements by their classes"
            ));
        }
        Ok(rewrite_hasclass(&xpath))
    }

    fn outer_html(dom: &Dom, id: DomId) -> String {
        dom.serialize(id)
    }

    /// `getNode(element, operation)`.
    fn get_node(&mut self, dom: &mut Dom, element: DomId, operation: DomId) -> Res<Option<DNode>> {
        let root = dom.top(element);
        dom.node_mut(root).doc_child = true; // new Document().appendChild(root)
        if dom.node(operation).name == "xpath" {
            let xpath = self.get_xpath(dom, operation)?;
            let compiled = XPath::parse(&xpath)
                .map_err(|e| JsError(format!("SyntaxError: Failed to execute 'evaluate' on 'Document': The string '{xpath}' is not a valid XPath expression. ({e})")))?;
            let nodes = compiled
                .select_in(dom, DNode::Node(root), Dialect::Browser)
                .map_err(|e| {
                    JsError(format!(
                        "TypeError: Failed to execute 'evaluate' on 'Document': {e}"
                    ))
                })?;
            return Ok(nodes.first().copied());
        }
        let op = dom.node(operation);
        let tag = op.name.clone();
        let attrs: Vec<(XStr, XStr)> = op
            .attrs
            .iter()
            .filter(|a| !a.qname.starts_with(TCTX))
            .map(|a| (a.qname.clone(), a.value.clone()))
            .collect();
        // root.querySelectorAll(tag): descendants only, document order
        let mut stack: Vec<DomId> = dom.node(root).children.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let n = dom.node(id);
            if n.kind == DomKind::Element {
                if n.name == tag
                    && attrs
                        .iter()
                        .all(|(k, v)| k == "position" || n.get(k) == Some(v.as_str()))
                {
                    return Ok(Some(DNode::Node(id)));
                }
                stack.extend(n.children.iter().rev().copied());
            }
        }
        Ok(None)
    }

    /// `getElement(element, operation)`.
    fn get_element(&mut self, dom: &mut Dom, element: DomId, operation: DomId) -> Res<DomId> {
        match self.get_node(dom, element, operation)? {
            None => Err(JsError(format!(
                "Element '{}' cannot be located in element tree",
                Self::outer_html(dom, operation)
            ))),
            Some(DNode::Node(id)) if dom.node(id).kind == DomKind::Element => Ok(id),
            Some(_) => Err(JsError(
                "Found node [object Node] instead of an element".into(),
            )),
        }
    }

    /// `getNodes(element, operation)`.
    fn get_nodes(&mut self, dom: &mut Dom, element: DomId, operation: DomId) -> Res<Vec<DomId>> {
        let mut nodes = Vec::new();
        let children = dom.node(operation).children.clone();
        for child in children {
            let c = dom.node(child);
            if c.kind == DomKind::Element && c.name == "xpath" && c.get("position") == Some("move")
            {
                let node = self.get_element(dom, element, child)?;
                let ctx = Self::get_translation_context(dom, node)?;
                dom.node_mut(node).set(TCTX, ctx);
                remove_node(dom, node);
                nodes.push(node);
            } else {
                self.set_translation_context(dom, child);
                nodes.push(child);
            }
        }
        Ok(nodes)
    }

    /// `addBefore(target, operation)`.
    fn add_before(&mut self, dom: &mut Dom, target: DomId, operation: DomId) -> Res<()> {
        let nodes = self.get_nodes(dom, target, operation)?;
        if nodes.is_empty() {
            return Ok(());
        }
        let previous = dom.previous_sibling(target);
        dom.before(target, &nodes)?;
        if let Some(prev) = previous
            && dom.node(prev).kind == DomKind::Text
        {
            let (text1, text2) = split_rstrip(&dom.node(prev).data);
            dom.node_mut(prev).data = trim_end(&text1).into();
            if let Some(text2) = text2
                && !text2.is_empty()
                && nodes.iter().any(|&n| dom.node(n).kind != DomKind::Text)
            {
                let text_node = dom.create_text(&text2);
                dom.before(target, &[text_node])?;
                if let Some(p) = dom.previous_sibling(text_node)
                    && dom.node(p).kind == DomKind::Text
                {
                    let t = trim_end(&dom.node(p).data);
                    dom.node_mut(p).data = t.into();
                }
            }
        }
        Ok(())
    }

    fn modify_attributes(&mut self, dom: &mut Dom, target: DomId, operation: DomId) -> Res<()> {
        let children: Vec<DomId> = dom
            .node(operation)
            .children
            .iter()
            .copied()
            .filter(|&c| dom.node(c).kind == DomKind::Element)
            .collect();
        for child in children {
            let c = dom.node(child);
            if c.name != "attribute" {
                continue;
            }
            let name = c.get("name").unwrap_or("null").to_owned();
            let first = c
                .children
                .first()
                .copied()
                .filter(|&f| dom.node(f).kind == DomKind::Text);
            let mut value = first.map(|f| dom.node(f).data.clone()).unwrap_or_default();
            let add = c.get("add").unwrap_or("").to_owned();
            let remove = c.get("remove").unwrap_or("").to_owned();
            if !add.is_empty() || !remove.is_empty() {
                if first.is_some() {
                    return Err(JsError("Useless element content undefined".into()));
                }
                let separator = c
                    .get("separator")
                    .filter(|s| !s.is_empty())
                    .unwrap_or(",")
                    .to_owned();
                let to_remove: Vec<&str> = remove.split(separator.as_str()).map(trim).collect();
                let current = dom.node(target).get(&name).unwrap_or("").to_owned();
                let mut values: Vec<&str> = current
                    .split(separator.as_str())
                    .map(trim)
                    .filter(|s| !to_remove.contains(s))
                    .collect();
                values.extend(
                    add.split(separator.as_str())
                        .map(trim)
                        .filter(|s| !s.is_empty()),
                );
                value = values.join(&separator).into();
            }
            let ctx = self.translation_context.clone();
            let t = dom.node_mut(target);
            if value.is_empty() {
                t.remove_attr(&name);
            } else {
                t.set(&name, value);
                if add.is_empty() && remove.is_empty() {
                    t.set(&format!("{TCTX}-{name}"), ctx);
                }
            }
        }
        Ok(())
    }

    fn replace(
        &mut self,
        dom: &mut Dom,
        mut root: DomId,
        target: DomId,
        operation: DomId,
    ) -> Res<DomId> {
        let mode = dom
            .node(operation)
            .get("mode")
            .filter(|s| !s.is_empty())
            .unwrap_or("outer")
            .to_owned();
        match mode.as_str() {
            "outer" => {
                // operation.ownerDocument.evaluate(".//*[text()='$0']", operation, ...)
                let locs: Vec<DomId> = {
                    let mut out = Vec::new();
                    let mut stack: Vec<DomId> =
                        dom.node(operation).children.iter().rev().copied().collect();
                    while let Some(id) = stack.pop() {
                        let n = dom.node(id);
                        if n.kind == DomKind::Element {
                            if n.children.iter().any(|&c| {
                                dom.node(c).kind == DomKind::Text && dom.node(c).data == "$0"
                            }) {
                                out.push(id);
                            }
                            stack.extend(n.children.iter().rev().copied());
                        }
                    }
                    out
                };
                let ctx = Self::get_translation_context(dom, target)?;
                dom.node_mut(target).set(TCTX, ctx);
                for loc in locs {
                    let first = dom.node(loc).children[0];
                    let clone = dom.deep_clone(target);
                    dom.replace_with(first, &[clone])?;
                }
                if dom.node(target).parent.is_some() {
                    let nodes = self.get_nodes(dom, target, operation)?;
                    dom.replace_with(target, &nodes)?;
                } else {
                    let mut content = None;
                    let mut comment = None;
                    for c in dom.node(operation).children.clone() {
                        match dom.node(c).kind {
                            DomKind::Element => {
                                self.set_translation_context(dom, c);
                                content = Some(c);
                                break;
                            }
                            DomKind::Comment => comment = Some(c),
                            _ => {}
                        }
                    }
                    let Some(content) = content else {
                        return Err(JsError(
                            "TypeError: Cannot read properties of null (reading 'cloneNode')"
                                .into(),
                        ));
                    };
                    root = dom.deep_clone(content);
                    if let Some(t_name) = dom.node(target).get("t-name").map(str::to_owned) {
                        dom.node_mut(root).set("t-name", t_name);
                    }
                    if let Some(comment) = comment {
                        dom.prepend(root, &[comment])?;
                    }
                }
            }
            "inner" => {
                while let Some(&last) = dom.node(target).children.last() {
                    dom.detach(last);
                }
                for node in dom.node(operation).children.clone() {
                    self.set_translation_context(dom, node);
                    dom.append(target, &[node])?;
                }
            }
            _ => return Err(JsError(format!("Invalid mode attribute: '{mode}'"))),
        }
        Ok(root)
    }

    /// `applyInheritance(root, operations, url)`: returns the (possibly new) root.
    ///
    /// # Errors
    ///
    /// The errors `applyInheritance` throws: an invalid operation (unknown
    /// `position` or `mode`, invalid `XPath` expression), a target that cannot
    /// be found, or a DOM `HierarchyRequestError`.
    pub fn apply(
        &mut self,
        dom: &mut Dom,
        mut root: DomId,
        operations: DomId,
        url: &str,
    ) -> Res<DomId> {
        self.warnings.clear();
        url.split('/')
            .nth(1)
            .unwrap_or("")
            .clone_into(&mut self.translation_context);
        let ops: Vec<DomId> = dom
            .node(operations)
            .children
            .iter()
            .copied()
            .filter(|&c| dom.node(c).kind == DomKind::Element)
            .collect();
        let result = (|| {
            for operation in ops {
                let target = self.get_element(dom, root, operation)?;
                let position = dom
                    .node(operation)
                    .get("position")
                    .filter(|s| !s.is_empty())
                    .unwrap_or("inside")
                    .to_owned();

                if self.debug && !url.is_empty() {
                    let attributes: Vec<String> = dom
                        .node(operation)
                        .attrs
                        .iter()
                        .map(|a| {
                            let mut v = String::new();
                            json_str(
                                if a.qname == "position" {
                                    &position
                                } else {
                                    &a.value
                                },
                                &mut v,
                            );
                            format!("{}={v}", a.qname)
                        })
                        .collect();
                    let comment = dom.create_comment(&format!(
                        " From file: {url} ; {} ",
                        attributes.join(" ; ")
                    ));
                    if position == "attributes" {
                        dom.before(target, &[comment])?;
                    } else {
                        dom.prepend(operation, &[comment])?;
                    }
                }

                match position.as_str() {
                    "replace" => root = self.replace(dom, root, target, operation)?,
                    "attributes" => self.modify_attributes(dom, target, operation)?,
                    "inside" => {
                        let sentinel = dom.create_element("sentinel", Some(XHTML_NS));
                        dom.append(target, &[sentinel])?;
                        self.add_before(dom, sentinel, operation)?;
                        remove_node(dom, sentinel);
                    }
                    "after" => {
                        let sentinel = dom.create_element("sentinel", Some(XHTML_NS));
                        dom.after(target, &[sentinel])?;
                        self.add_before(dom, sentinel, operation)?;
                        remove_node(dom, sentinel);
                    }
                    "before" => self.add_before(dom, target, operation)?,
                    _ => return Err(JsError(format!("Invalid position attribute: '{position}'"))),
                }
            }
            Ok(root)
        })();
        self.translation_context.clear();
        result
    }
}

/// `removeNode(node)`.
fn remove_node(dom: &mut Dom, node: DomId) {
    let next = dom.next_sibling(node);
    let prev = dom.previous_sibling(node);
    dom.detach(node);
    if let (Some(n), Some(p)) = (next, prev)
        && dom.node(n).kind == DomKind::Text
        && dom.node(p).kind == DomKind::Text
        && dom
            .node(p)
            .parent
            .is_some_and(|pp| dom.node(pp).children.first() == Some(&p))
    {
        let t = trim_end(&dom.node(p).data);
        dom.node_mut(p).data = t.into();
    }
}

/// `applyContextToTextNode()` for one tree: wrap every text node that has a
/// translation context in `<t t-translation-context="...">`.
pub fn apply_context_to_text_nodes(dom: &mut Dom, root: DomId) {
    let mut stack = vec![root];
    let mut texts = Vec::new();
    while let Some(id) = stack.pop() {
        let n = dom.node(id);
        if n.kind == DomKind::Text && n.tctx.is_some() {
            texts.push(id);
        }
        stack.extend(n.children.iter().copied());
    }
    for text in texts {
        let Some(ctx) = dom.node_mut(text).tctx.take() else {
            continue;
        };
        let wrapper = dom.create_element("t", Some(XHTML_NS));
        dom.node_mut(wrapper).set(TCTX, ctx);
        if dom.before(text, &[wrapper]).is_ok() && dom.node(wrapper).parent.is_some() {
            let _ = dom.append(wrapper, &[text]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `_applyInheritance` of `template_inheritance.test.js`.
    fn apply(arch: &str, ops: &str, url: &str, debug: bool) -> Result<String, JsError> {
        let mut dom = Dom::new();
        let root = dom.parse(arch).unwrap();
        let ops = dom.parse(ops).unwrap();
        dom.node_mut(root).set(TCTX, "from_target");
        let mut js = JsInheritance::new(debug);
        let r = js.apply(&mut dom, root, ops, url)?;
        Ok(dom.serialize(r))
    }

    fn t(arch: &str, ops: &str) -> String {
        apply(arch, ops, "test/from_op", false).unwrap()
    }

    #[test]
    fn upstream_cases() {
        assert_eq!(
            t(
                r#"<t t-name="web.A"> <div><h2>Title</h2>text</div> </t>"#,
                r#"
                <t>
                    <xpath expr="./div/h2" position="replace"><h3>Other title</h3></xpath>
                </t>"#
            ),
            r#"<t t-name="web.A" t-translation-context="from_target"> <div><h3 t-translation-context="from_op">Other title</h3>text</div> </t>"#
        );
        assert_eq!(
            apply(
                r#"<t t-name="web.A"> <div><h2>Title</h2>text</div> </t>"#,
                r#"
                <t>
                    <xpath expr="./div/h2" position="replace"><h3>Other title</h3></xpath>
                </t>"#,
                "test/from_op",
                true
            )
            .unwrap(),
            r#"<t t-name="web.A" t-translation-context="from_target"> <div><!-- From file: test/from_op ; expr="./div/h2" ; position="replace" --><h3 t-translation-context="from_op">Other title</h3>text</div> </t>"#
        );
        assert_eq!(
            t(
                r#"<t t-name="web.A"> <div>I was petrified</div> </t>"#,
                r#"
                <t>
                    <xpath expr="." position="replace"> <div>$0</div><div>At first I was afraid</div> </xpath>
                </t>"#
            ),
            r#"<div t-translation-context="from_op" t-name="web.A"><t t-name="web.A" t-translation-context="from_target"> <div>I was petrified</div> </t></div>"#
        );
        assert_eq!(
            t(
                r#"<t t-name="web.A"> <div> A <span/> B <span/> C </div> </t>"#,
                r#"
                <t>
                    <xpath expr="./div" position="replace" mode="inner"> E <div/> F <span attr1="12"/> </xpath>
                </t>"#
            ),
            r#"<t t-name="web.A" t-translation-context="from_target"> <div> E <div t-translation-context="from_op"/> F <span attr1="12" t-translation-context="from_op"/> </div> </t>"#
        );
        assert_eq!(
            t(
                r#"<t t-name="web.A"> <div>AAB is the best<h2>Title</h2>text</div> </t>"#,
                r#"
                <t>
                    <xpath expr="./div/h2" position="before"> <h3>Other title</h3>Yooplahoo!<h4>Yet another title</h4> </xpath>
                </t>"#
            ),
            r#"<t t-name="web.A" t-translation-context="from_target"> <div>AAB is the best <h3 t-translation-context="from_op">Other title</h3>Yooplahoo!<h4 t-translation-context="from_op">Yet another title</h4> <h2>Title</h2>text</div> </t>"#
        );
        assert_eq!(
            t(
                r#"<t t-name="web.A"> <div>AAB is the best<h2>Title</h2><div><span>Ola</span></div></div> </t>"#,
                r#"
                <t>
                    <xpath expr="./div/h2" position="before"> <xpath expr="./div/div/span" position="move" /> </xpath>
                </t>"#
            ),
            r#"<t t-name="web.A" t-translation-context="from_target"> <div>AAB is the best <span t-translation-context="from_target">Ola</span> <h2>Title</h2><div/></div> </t>"#
        );
        assert_eq!(
            t(
                r#"<t t-name="web.A" t-translation-context="from_target"> <div>AAB is the best <h2>Title</h2> <div/> </div> </t>"#,
                r#"
                <t>
                    <xpath expr="./div/div" position="inside"> Hop! <xpath expr="./div/h2" position="move" /> <span>Yellow</span> </xpath>
                </t>"#
            ),
            r#"<t t-name="web.A" t-translation-context="from_target"> <div>AAB is the best <div> Hop! <h2 t-translation-context="from_target">Title</h2> <span t-translation-context="from_op">Yellow</span> </div> </div> </t>"#
        );
        assert_eq!(
            t(
                "<t><div>\na \n </div></t>",
                "<t><xpath expr=\"./div\" position=\"inside\">4</xpath></t>"
            ),
            "<t t-translation-context=\"from_target\"><div>\na4</div></t>"
        );
        assert_eq!(
            t(
                r#"<t t-name="web.A" t-translation-context="from_target"> <div> AAB is the best <h2>Title</h2> <div id="1"/> <div id="2"/> </div> </t>"#,
                r#"
                <t>
                    <xpath expr="./div/h2" position="after"> Hop! <xpath expr="./div/div[@id=2]" position="move" /> <span>Yellow</span> </xpath>
                </t>"#
            ),
            r#"<t t-name="web.A" t-translation-context="from_target"> <div> AAB is the best <h2>Title</h2> Hop! <div id="2" t-translation-context="from_target"/> <span t-translation-context="from_op">Yellow</span>  <div id="1"/>  </div> </t>"#
        );
        assert_eq!(
            apply(
                r#"<t t-name="web.A"> <div attr1="12" attr2="a b" attr3="to remove" /> </t>"#,
                r#"
                <t>
                    <xpath expr="./div" position="attributes">
                        <attribute name="attr1">45</attribute>
                        <attribute name="attr3"></attribute>
                        <attribute name="attr2" add="c" separator=" "></attribute>
                        <attribute name="attr2" remove="a" separator=" "></attribute>
                        <attribute name="attr4">new</attribute>
                    </xpath>
                </t>"#,
                "test/from_op",
                true
            )
            .unwrap(),
            r#"<t t-name="web.A" t-translation-context="from_target"> <!-- From file: test/from_op ; expr="./div" ; position="attributes" --><div attr1="45" attr2="b c" t-translation-context-attr1="from_op" attr4="new" t-translation-context-attr4="from_op"/> </t>"#
        );
    }

    #[test]
    fn hasclass() {
        assert_eq!(
            t(
                r#"<t><div class="abcd e abc"/></t>"#,
                r#"<t><xpath expr="./div[hasclass('abc', 'abcd' )]" position="replace"></xpath></t>"#
            ),
            r#"<t t-translation-context="from_target"/>"#
        );
        assert!(
            apply(
                r#"<t><div class="dabc"/></t>"#,
                r#"<t><xpath expr="./div[hasclass('abc')]" position="replace"></xpath></t>"#,
                "test/from_op",
                false
            )
            .is_err()
        );
        assert!(apply(r#"<t><div class="dabc"/></t>"#, r#"<t><xpath expr="./div[ends-with(@class, 'bc')]" position="replace"></xpath></t>"#, "test/from_op", false).is_err());
    }

    #[test]
    fn warnings() {
        let mut dom = Dom::new();
        let root = dom
            .parse(r#"<t t-name="web.A"><div class="my-class" /></t>"#)
            .unwrap();
        let ops = dom.parse(r#"<t t-inherit="web.A"><xpath expr="*[contains(@class, 'my-class')]" position="inside"><span/></xpath></t>"#).unwrap();
        dom.node_mut(root).set(TCTX, "from_target");
        let mut js = JsInheritance::new(true);
        let r = js.apply(&mut dom, root, ops, "").unwrap();
        assert_eq!(
            dom.serialize(r),
            r#"<t t-name="web.A" t-translation-context="from_target"><div class="my-class"><span t-translation-context=""/></div></t>"#
        );
        assert_eq!(js.warnings.len(), 1);
    }
}
