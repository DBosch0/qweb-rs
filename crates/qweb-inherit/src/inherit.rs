//! Port of `odoo/tools/template_inheritance.py` (`apply_inheritance_specs`).

use core::fmt::Write as _;
use core::{error, fmt};

use qweb_core::compile::py_repr_str as py_repr;
use qweb_core::escape::escape as html_escape;
use qweb_core::xml::{Document, NodeId, NodeKind, XStr};

use crate::xpath::{XPath, py_is_space};

/// Attribute names that contain Python expressions.
const PYTHON_ATTRIBUTES: &[&str] = &[
    "readonly",
    "required",
    "invisible",
    "column_invisible",
    "t-if",
    "t-elif",
];

const REMOVAL_PI: &str = "apply-inheritance-specs-node-removal";

/// Python exception class of an [`InheritError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// `odoo.exceptions.ValidationError` (invalid xpath expression).
    Validation,
    /// `ValueError` (spec cannot be applied).
    Value,
}

/// An inheritance spec failed to apply.
#[derive(Debug, Clone)]
pub struct InheritError {
    /// Exception class to raise.
    pub kind: ErrorKind,
    /// Error message.
    pub message: String,
}

impl fmt::Display for InheritError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl error::Error for InheritError {}

fn value_err<T>(msg: String) -> Result<T, InheritError> {
    Err(InheritError {
        kind: ErrorKind::Value,
        message: msg,
    })
}

/// Options of [`apply_inheritance_specs`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Mark removed nodes and copied `$0` nodes for branding distribution.
    pub inherit_branding: bool,
}

/// Python `str.rstrip()`.
fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_is_space)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_is_space)
}

/// `RSTRIP_REGEXP.search(s).group(0)` (`RSTRIP_REGEXP = r'\n[ \t]*$'`), or
/// `""` when there is no match. Python's `$` also matches before a final
/// `\n`, so in `"a\n  \n"` the match is `"\n  "`.
fn rstrip_match(s: Option<&str>) -> &str {
    let s = s.unwrap_or("");
    let body = s.strip_suffix('\n').unwrap_or(s);
    let spaces = body.trim_end_matches([' ', '\t']);
    if spaces.ends_with('\n') {
        &s[spaces.len() - 1..body.len()]
    } else if body.len() < s.len() {
        "\n"
    } else {
        ""
    }
}

/// Locate a node of `arch` matching `spec` (`locate_node`).
///
/// # Errors
///
/// The spec is invalid: an `xpath` without `expr`, or an invalid `XPath`
/// expression ([`ErrorKind::Validation`]); or evaluating the expression
/// failed ([`ErrorKind::Value`]).
pub fn locate_node(
    doc: &Document,
    arch: NodeId,
    spec: NodeId,
) -> Result<Option<NodeId>, InheritError> {
    let s = doc.node(spec);
    if s.tag == "xpath" {
        let Some(expr) = s.get("expr") else {
            return Err(InheritError {
                kind: ErrorKind::Validation,
                message: "Missing 'expr' attribute in xpath specification".into(),
            });
        };
        let xpath = XPath::parse(expr).map_err(|_| InheritError {
            kind: ErrorKind::Validation,
            message: format!("Invalid Expression while parsing xpath “{expr}”"),
        })?;
        return xpath.first_element(doc, arch).map_err(|e| InheritError {
            kind: ErrorKind::Value,
            message: e.0,
        });
    }
    // lazy pre-order walk: stops at the first match
    let mut elements = doc
        .descendants(arch)
        .filter(|&n| doc.node(n).kind == NodeKind::Element && doc.node(n).tag == s.tag);
    if s.tag == "field" {
        let name = s.get("name");
        return Ok(elements.find(|&n| doc.node(n).get("name") == name));
    }
    for n in elements {
        if s.attrs
            .iter()
            .filter(|(k, _)| k != "position")
            .all(|(k, v)| doc.node(n).get(k) == Some(v.as_str()))
        {
            return Ok(Some(n));
        }
    }
    Ok(None)
}

/// `add_text_before(node, text)`. Fails like Odoo (`parent.text` on `None`)
/// when `node` is detached.
fn add_text_before(
    doc: &mut Document,
    node: NodeId,
    text: Option<XStr>,
) -> Result<(), InheritError> {
    let Some(text) = text else { return Ok(()) };
    if let Some(prev) = doc.prev_sibling(node) {
        let n = doc.node_mut(prev);
        n.tail = Some(match n.tail.take() {
            None => text,
            Some(t) => (t.into_string() + &text).into(),
        });
    } else {
        let Some(parent) = doc.node(node).parent else {
            return value_err("'NoneType' object has no attribute 'text'".into());
        };
        let n = doc.node_mut(parent);
        n.text = Some((py_rstrip(n.text.as_deref().unwrap_or("")).to_owned() + &text).into());
    }
    Ok(())
}

/// `remove_element(node)`: remove the node but keep its tail. Fails like Odoo
/// when `node` is detached (e.g. a sentinel extracted by a `move` spec).
fn remove_element(doc: &mut Document, node: NodeId) -> Result<(), InheritError> {
    let tail = doc.node_mut(node).tail.take();
    add_text_before(doc, node, tail)?;
    if doc.node(node).parent.is_none() {
        return value_err("'NoneType' object has no attribute 'remove'".into());
    }
    doc.detach(node);
    Ok(())
}

/// Hook called with each spec before it is located (see [`apply_inheritance_specs`]).
pub type PreLocate = dyn FnMut(&mut Document, NodeId);

struct Applier<'a> {
    doc: &'a mut Document,
    source: NodeId,
    pre_locate: Option<&'a mut PreLocate>,
}

impl Applier<'_> {
    fn locate(&mut self, spec: NodeId) -> Result<Option<NodeId>, InheritError> {
        if let Some(f) = self.pre_locate.as_mut() {
            f(self.doc, spec);
        }
        locate_node(self.doc, self.source, spec)
    }

    /// `extract(spec)`: locate a node, remove it from the source and return it.
    fn extract(&mut self, spec: NodeId) -> Result<NodeId, InheritError> {
        if !self.doc.node(spec).children.is_empty() {
            return value_err(format!(
                "Invalid specification for moved nodes: “{}”",
                self.doc.to_string(spec, true)
            ));
        }
        match self.locate(spec)? {
            Some(n) => {
                remove_element(self.doc, n)?;
                Ok(n)
            }
            None => value_err(format!(
                "Element “{}” cannot be located in parent view",
                self.doc.to_string(spec, true)
            )),
        }
    }

    /// `target.addprevious(new)`. lxml raises for the root of the tree but
    /// silently links `new` out of the tree when `target` has been detached
    /// (e.g. moved by a `position="move"` spec).
    fn add_previous(&mut self, target: NodeId, new: NodeId) -> Result<(), InheritError> {
        if new == target {
            return Ok(());
        }
        if self.doc.node(target).parent.is_none() && target != self.source {
            self.doc.detach(new);
            return Ok(());
        }
        self.doc
            .add_previous(target, new)
            .or_else(|e| value_err(e.into()))
    }

    fn is_move(&self, n: NodeId) -> bool {
        let node = self.doc.node(n);
        node.kind == NodeKind::Element && node.get("position") == Some("move")
    }

    /// `add_stripped_items_before(node, spec, extract)`.
    fn add_stripped_items_before(
        &mut self,
        node: NodeId,
        spec: NodeId,
    ) -> Result<(), InheritError> {
        let Some(parent) = self.doc.node(node).parent else {
            return value_err("'NoneType' object has no attribute 'text'".into());
        };
        let spec_text = self.doc.node(spec).text.clone();
        let text = spec_text.clone().unwrap_or_default();

        let prev = {
            let doc = &*self.doc;
            let idx = doc.index_in_parent(node).unwrap_or(0);
            doc.node(parent).children[..idx]
                .iter()
                .rev()
                .copied()
                .find(|&n| !(doc.node(n).kind == NodeKind::Pi && doc.node(n).tag == REMOVAL_PI))
        };
        let before_text;
        match prev {
            None => {
                let p = self.doc.node_mut(parent);
                before_text = rstrip_match(p.text.as_deref()).to_owned();
                let new = py_rstrip(p.text.as_deref().unwrap_or("")).to_owned() + &text;
                p.text = if new.is_empty() {
                    spec_text.as_ref().map(|_| XStr::new())
                } else {
                    Some(new.into())
                };
            }
            Some(prev) => {
                let p = self.doc.node_mut(prev);
                before_text = rstrip_match(p.tail.as_deref()).to_owned();
                p.tail =
                    Some((py_rstrip(p.tail.as_deref().unwrap_or("")).to_owned() + &text).into());
            }
        }

        let children = self.doc.node(spec).children.clone();
        if let Some(&last) = children.last() {
            let n = self.doc.node_mut(last);
            n.tail =
                Some((py_rstrip(n.tail.as_deref().unwrap_or("")).to_owned() + &before_text).into());
        } else {
            let n = self.doc.node_mut(spec);
            n.text =
                Some((py_rstrip(n.text.as_deref().unwrap_or("")).to_owned() + &before_text).into());
        }

        for mut child in children {
            if self.is_move(child) {
                let tail = self.doc.node(child).tail.clone();
                child = self.extract(child)?;
                self.doc.node_mut(child).tail = tail;
            }
            self.add_previous(node, child)?;
        }
        Ok(())
    }

    fn apply(&mut self, specs: &[NodeId], opts: Options) -> Result<NodeId, InheritError> {
        let mut queue: std::collections::VecDeque<NodeId> = specs.iter().copied().collect();
        while let Some(spec) = queue.pop_front() {
            if self.doc.node(spec).kind != NodeKind::Element {
                continue;
            }
            if self.doc.node(spec).tag == "data" {
                queue.extend(self.doc.node(spec).children.iter().copied());
                continue;
            }
            let Some(node) = self.locate(spec)? else {
                let s = self.doc.node(spec);
                let mut attrs = String::new();
                for (k, v) in s.attrs.iter().filter(|(k, _)| k != "position") {
                    let _ = write!(attrs, " {k}=\"{}\"", html_escape(v));
                }
                return value_err(format!(
                    "Element '<{}{attrs}>' cannot be located in parent view",
                    s.tag
                ));
            };
            let pos = self
                .doc
                .node(spec)
                .get("position")
                .unwrap_or("inside")
                .to_owned();
            match pos.as_str() {
                "replace" => {
                    let mode = self
                        .doc
                        .node(spec)
                        .get("mode")
                        .unwrap_or("outer")
                        .to_owned();
                    match mode.as_str() {
                        "outer" => self.replace_outer(spec, node, opts)?,
                        "inner" => self.replace_inner(spec, node)?,
                        _ => return value_err(format!("Invalid mode attribute: “{mode}”")),
                    }
                }
                "attributes" => self.attributes(spec, node)?,
                "inside" => {
                    let sentinel = self.doc.new_element("sentinel", Vec::new());
                    self.doc.append(node, sentinel);
                    self.add_stripped_items_before(sentinel, spec)?;
                    remove_element(self.doc, sentinel)?;
                }
                "after" => {
                    if self.doc.node(node).parent.is_none() {
                        return value_err("Only processing instructions and comments can be siblings of the root element".into());
                    }
                    let sentinel = self.doc.new_element("sentinel", Vec::new());
                    self.doc
                        .add_next(node, sentinel)
                        .or_else(|e| value_err(e.into()))?;
                    if let Some(tail) = self.doc.node_mut(node).tail.take() {
                        self.doc.node_mut(sentinel).tail = Some(tail);
                    }
                    self.add_stripped_items_before(sentinel, spec)?;
                    remove_element(self.doc, sentinel)?;
                }
                "before" => self.add_stripped_items_before(node, spec)?,
                _ => return value_err(format!("Invalid position attribute: '{pos}'")),
            }
        }
        Ok(self.source)
    }

    fn replace_outer(
        &mut self,
        spec: NodeId,
        node: NodeId,
        opts: Options,
    ) -> Result<(), InheritError> {
        // spec.xpath(".//*[text()='$0']")
        let locs: Vec<NodeId> = self
            .doc
            .iter(spec)
            .into_iter()
            .skip(1)
            .filter(|&n| {
                let d = &*self.doc;
                d.node(n).kind == NodeKind::Element
                    && (d.node(n).text.as_deref() == Some("$0")
                        || d.node(n)
                            .children
                            .iter()
                            .any(|&c| d.node(c).tail.as_deref() == Some("$0")))
            })
            .collect();
        for loc in locs {
            self.doc.node_mut(loc).text = Some(XStr::new());
            let copied = self.doc.deep_copy(node);
            if opts.inherit_branding {
                self.doc.node_mut(copied).set("data-oe-no-branding", "1");
            }
            self.doc.append(loc, copied);
        }

        if self.doc.node(node).parent.is_none() {
            let mut spec_content = None;
            let mut comment = None;
            for &c in &self.doc.node(spec).children {
                if self.doc.node(c).kind != NodeKind::Comment {
                    spec_content = Some(c);
                    break;
                }
                comment = Some(c);
            }
            let Some(content) = spec_content else {
                return value_err("'NoneType' object has no attribute 'set'".into());
            };
            let source = self.doc.deep_copy(content);
            if let Some(t_name) = self
                .doc
                .node(node)
                .get("t-name")
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
            {
                self.doc.node_mut(source).set("t-name", t_name);
            }
            if let Some(comment) = comment {
                let text = self.doc.node_mut(source).text.take();
                self.doc.node_mut(comment).tail = text;
                self.doc.insert(source, 0, comment);
            }
            self.source = source;
        } else {
            if opts.inherit_branding
                && self
                    .doc
                    .node(node)
                    .get("data-oe-xpath")
                    .is_none_or(str::is_empty)
            {
                let tag = self.doc.node(node).tag.clone();
                let pi = self.doc.new_pi(REMOVAL_PI, &tag);
                self.doc
                    .add_previous(node, pi)
                    .or_else(|e| value_err(e.into()))?;
            }
            let children = self.doc.node(spec).children.clone();
            for mut child in children {
                if self.is_move(child) {
                    child = self.extract(child)?;
                }
                self.add_previous(node, child)?;
            }
            if self.doc.node(node).parent.is_none() {
                return value_err("'NoneType' object has no attribute 'remove'".into());
            }
            self.doc.detach(node);
        }
        Ok(())
    }

    fn replace_inner(&mut self, spec: NodeId, node: NodeId) -> Result<(), InheritError> {
        let sentinel = self.doc.new_element("sentinel", Vec::new());
        match self.doc.node(node).children.first().copied() {
            Some(first) => self
                .doc
                .add_previous(first, sentinel)
                .or_else(|e| value_err(e.into()))?,
            None => self.doc.append(node, sentinel),
        }
        self.doc.node_mut(node).text = None;
        let copy = self.doc.deep_copy(spec);
        self.add_stripped_items_before(sentinel, copy)?;
        while let Some(child) = self.doc.node(node).children.last().copied() {
            self.doc.detach(child);
            if child == sentinel {
                break;
            }
        }
        Ok(())
    }

    fn attributes(&mut self, spec: NodeId, node: NodeId) -> Result<(), InheritError> {
        let children: Vec<NodeId> = self
            .doc
            .iter(spec)
            .into_iter()
            .filter(|&n| {
                self.doc.node(n).kind == NodeKind::Element && self.doc.node(n).tag == "attribute"
            })
            .collect();
        for child in children {
            let c = self.doc.node(child);
            let unknown: Vec<String> = c
                .attrs
                .iter()
                .map(|(k, _)| k)
                .filter(|k| {
                    !matches!(k.as_str(), "name" | "add" | "remove" | "separator")
                        && !k.starts_with("data-oe-")
                })
                .map(|k| py_repr(k))
                .collect();
            if !unknown.is_empty() {
                return value_err(format!(
                    "Invalid attributes {} in element <attribute>",
                    unknown.join(", ")
                ));
            }
            let Some(attribute) = c.get("name").map(str::to_owned) else {
                return value_err("Element <attribute> has no 'name'".into());
            };
            let add = c.get("add").unwrap_or("").to_owned();
            let remove = c.get("remove").unwrap_or("").to_owned();
            let value = if !add.is_empty() || !remove.is_empty() {
                if let Some(t) = c.text.as_deref().filter(|t| !t.is_empty()) {
                    return value_err(format!(
                        "Element <attribute> with 'add' or 'remove' cannot contain text {}",
                        py_repr(t)
                    ));
                }
                let mut value = self.doc.node(node).get(&attribute).unwrap_or("").to_owned();
                let separator = c.get("separator").map(str::to_owned);
                if PYTHON_ATTRIBUTES.contains(&attribute.as_str())
                    || attribute.starts_with("decoration-")
                {
                    let Some(separator) = separator else {
                        return value_err("'NoneType' object has no attribute 'strip'".into());
                    };
                    let separator = py_strip(&separator);
                    if separator != "and" && separator != "or" {
                        return value_err(format!(
                            "Invalid separator {} for python expression {}; valid values are 'and' and 'or'",
                            py_repr(separator),
                            py_repr(&attribute)
                        ));
                    }
                    if !remove.is_empty() {
                        if full_match_parens(&remove, &value)? {
                            value = String::new();
                        } else {
                            let patterns = [
                                format!("({remove}) {separator} "),
                                format!(" {separator} ({remove})"),
                                format!("{remove} {separator} "),
                                format!(" {separator} {remove}"),
                            ];
                            for p in patterns {
                                if let Some(i) = value.find(&p) {
                                    value = format!("{}{}", &value[..i], &value[i + p.len()..]);
                                    break;
                                }
                            }
                        }
                    }
                    if !add.is_empty() {
                        value = if value.is_empty() {
                            add
                        } else {
                            format!("({value}) {separator} ({add})")
                        };
                    }
                    value
                } else {
                    let separator = match separator.as_deref() {
                        None => Some(","),
                        Some(" ") => None,
                        Some(s) => Some(s),
                    };
                    if separator == Some("") {
                        return value_err("empty separator".into());
                    }
                    let to_remove: Vec<String> = py_split(&remove, separator)
                        .map(|s| py_strip(s).to_owned())
                        .collect();
                    let mut out: Vec<String> = py_split(&value, separator)
                        .map(|s| py_strip(s).to_owned())
                        .filter(|v| !v.is_empty() && !to_remove.contains(v))
                        .collect();
                    out.extend(
                        py_split(&add, separator)
                            .map(|s| py_strip(s).to_owned())
                            .filter(|s| !s.is_empty()),
                    );
                    out.join(separator.unwrap_or(" "))
                }
            } else {
                c.text.as_deref().unwrap_or("").to_owned()
            };
            let n = self.doc.node_mut(node);
            if value.is_empty() {
                n.pop(&attribute);
            } else {
                n.set(&attribute, value);
            }
        }
        Ok(())
    }
}

/// Python `str.split(sep)` (`None`: split on whitespace runs).
fn py_split<'s>(s: &'s str, sep: Option<&'s str>) -> Box<dyn Iterator<Item = &'s str> + 's> {
    match sep {
        Some(sep) => Box::new(s.split(sep)),
        None => Box::new(s.split(py_is_space).filter(|p| !p.is_empty())),
    }
}

/// `re.match(rf'^\(*{remove}\)*$', value)`; `remove` is used as a regular expression.
fn full_match_parens(remove: &str, value: &str) -> Result<bool, InheritError> {
    // Python's `$` also matches before a trailing newline.
    let pattern = format!(r"^\(*(?:{remove})\)*\n?\z");
    match regex_lite::Regex::new(&pattern) {
        Ok(re) => Ok(re.is_match(value)),
        Err(e) => value_err(format!("invalid regular expression {remove:?}: {e}")),
    }
}

/// Apply the inheritance `specs` (nodes of `doc`, e.g. imported with
/// [`Document::import`]) to the tree rooted at `source`.
///
/// Returns the root of the resulting tree (a new node when the root itself is
/// replaced). `pre_locate` is called with each spec before it is located.
///
/// # Errors
///
/// As in Odoo: an invalid spec or `XPath` expression
/// ([`ErrorKind::Validation`]), or a spec that cannot be applied, such as a
/// node that cannot be located in the parent view ([`ErrorKind::Value`]).
pub fn apply_inheritance_specs<'a>(
    doc: &'a mut Document,
    source: NodeId,
    specs: &[NodeId],
    opts: Options,
    pre_locate: Option<&'a mut PreLocate>,
) -> Result<NodeId, InheritError> {
    let mut applier = Applier {
        doc,
        source,
        pre_locate,
    };
    applier.apply(specs, opts)
}

/// String API: parse `source` and each spec tree, apply them in order and
/// serialize the result (like `etree.tostring(result, encoding='unicode')`).
///
/// # Errors
///
/// The source or a spec is not well-formed XML ([`ErrorKind::Value`]), or a
/// spec fails to apply (see [`apply_inheritance_specs`]).
pub fn apply_inheritance_str(
    source: &str,
    specs: &[&str],
    opts: Options,
) -> Result<String, InheritError> {
    let parse = |s: &str| {
        Document::parse(s).map_err(|e| InheritError {
            kind: ErrorKind::Value,
            message: e.to_string(),
        })
    };
    let mut doc = parse(source)?;
    let mut root = doc.root;
    for spec in specs {
        let sdoc = parse(spec)?;
        let s = doc.import(&sdoc, sdoc.root);
        root = apply_inheritance_specs(&mut doc, root, &[s], opts, None)?;
    }
    Ok(doc.to_string(root, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(src: &str, spec: &str) -> String {
        apply_inheritance_str(src, &[spec], Options::default()).unwrap()
    }

    /// A nested `move` that extracts Odoo's temporary sentinel: errors (with
    /// Odoo's `AttributeError` message), never a panic. See `README_PRIV.md`,
    /// "Odoo edge cases".
    #[test]
    fn moved_sentinel() {
        let src = r#"<t t-name="tmpl"><p id="2" name="y"></p></t>"#;
        let err = |spec: &str| {
            apply_inheritance_str(src, &[spec], Options::default())
                .unwrap_err()
                .message
        };
        let text = "'NoneType' object has no attribute 'text'";
        let remove = "'NoneType' object has no attribute 'remove'";
        assert_eq!(
            err(
                r#"<xpath expr="//p" position="after"><xpath expr="//t/*[2]" position="move"/></xpath>"#
            ),
            text
        );
        assert_eq!(
            err(
                r#"<xpath expr="//p" position="inside"><xpath expr="//p/*[1]" position="move"/></xpath>"#
            ),
            text
        );
        assert_eq!(
            err(
                "<xpath expr=\"//*[@id='2']\" position=\"after\"><xpath expr=\"//p[@id='2']\" position=\"move\"/>\n\n<xpath expr=\"//t/*[2]\" position=\"move\"/><b>$0</b></xpath>"
            ),
            remove
        );
        // Odoo silently loses the content here: reproduced, not fixed.
        assert_eq!(
            apply(
                src,
                r#"<xpath expr="//t" position="replace" mode="inner"><b/><xpath expr="//t/*[2]" position="move"/></xpath>"#
            ),
            r#"<t t-name="tmpl"/>"#
        );
    }

    #[test]
    fn positions() {
        let src = "<t>\n    <div>\n        <p/>\n    </div>\n</t>";
        assert_eq!(
            apply(src, r#"<xpath expr="//p" position="after"><span/></xpath>"#),
            "<t>\n    <div>\n        <p/><span/>\n    </div>\n</t>"
        );
        assert_eq!(
            apply(
                src,
                "<xpath expr=\"//div\" position=\"inside\">\n    <span/>\n</xpath>"
            ),
            "<t>\n    <div>\n        <p/>\n    <span/>\n    </div>\n</t>"
        );
        assert_eq!(
            apply(src, r#"<xpath expr="//p" position="replace"/>"#),
            "<t>\n    <div>\n        </div>\n</t>"
        );
        assert_eq!(
            apply(
                src,
                r#"<div position="attributes"><attribute name="class" add="a b" separator=" "/></div>"#
            ),
            "<t>\n    <div class=\"a b\">\n        <p/>\n    </div>\n</t>"
        );
    }

    #[test]
    fn rstrip() {
        assert_eq!(rstrip_match(Some("abc\n  ")), "\n  ");
        assert_eq!(rstrip_match(Some("a\n  \n")), "\n  ");
        assert_eq!(rstrip_match(Some("a\n\n")), "\n");
        assert_eq!(rstrip_match(Some("a \n")), "\n");
        assert_eq!(rstrip_match(Some("a  ")), "");
        assert_eq!(rstrip_match(Some("\n")), "\n");
        assert_eq!(rstrip_match(Some("")), "");
        assert_eq!(rstrip_match(None), "");
    }

    #[test]
    fn not_found() {
        let e = apply_inheritance_str(
            "<t/>",
            &[r#"<xpath expr="//x" position="after"/>"#],
            Options::default(),
        )
        .unwrap_err();
        assert_eq!(
            e.message,
            "Element '<xpath expr=\"//x\">' cannot be located in parent view"
        );
    }
}
