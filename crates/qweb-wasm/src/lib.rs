//! WebAssembly bindings (wasm-bindgen).
//!
//! * [`TemplateStore`]: the browser dialect of template inheritance
//!   (`template_inheritance.js`). Trees stay in WASM memory between calls and
//!   are referenced by handles, so translation contexts of text nodes and text
//!   node boundaries survive until the final tree is exported (`toJson`).
//! * [`apply_inheritance_server`]: the server dialect
//!   (`template_inheritance.py`) on serialized trees.
//! * [`Renderer`]: QWeb string rendering with plain JSON values.
//!
//! Build with `scripts/build_wasm.sh` (wasm-bindgen) or wasm-pack; see
//! README.md.

#![deny(missing_docs)]

use std::sync::Arc;

use qweb_core::runtime::Engine;
use qweb_inherit::dom::{Dom, DomId};
use qweb_inherit::js::{JsInheritance, apply_context_to_text_nodes};
use qweb_value::{Context, ValueHost, from_json};
use wasm_bindgen::prelude::*;

/// A JS `Error` with the given message.
fn js_err(msg: impl Into<String>) -> JsError {
    JsError::new(&msg.into())
}

/// Template trees for the browser dialect of template inheritance.
#[wasm_bindgen]
pub struct TemplateStore {
    dom: Dom,
    js: JsInheritance,
}

#[wasm_bindgen]
impl TemplateStore {
    /// An empty store (`debug`: `odoo.debug`, which adds the `CLASS_CONTAINS`
    /// warnings).
    #[wasm_bindgen(constructor)]
    pub fn new(debug: bool) -> TemplateStore {
        TemplateStore {
            dom: Dom::new(),
            js: JsInheritance::new(debug),
        }
    }

    /// Change `odoo.debug`.
    #[wasm_bindgen(js_name = setDebug)]
    pub fn set_debug(&mut self, debug: bool) {
        self.js.debug = debug;
    }

    /// The node of handle `h`, if it is live.
    fn check(&self, h: u32) -> Result<DomId, JsError> {
        let id = h as DomId;
        if self.dom.is_live(id) {
            Ok(id)
        } else {
            Err(js_err(format!("invalid template handle {h}")))
        }
    }

    /// Parse an XML string; returns the handle of its document element.
    pub fn parse(&mut self, xml: &str) -> Result<u32, JsError> {
        self.dom
            .parse(xml)
            .map(|id| id as u32)
            .map_err(|e| js_err(e.0))
    }

    /// `deepClone` (text translation contexts are kept).
    #[wasm_bindgen(js_name = cloneTree)]
    pub fn clone_tree(&mut self, h: u32) -> Result<u32, JsError> {
        let id = self.check(h)?;
        Ok(self.dom.deep_clone(id) as u32)
    }

    /// Release a tree.
    #[wasm_bindgen(js_name = freeTree)]
    pub fn free_tree(&mut self, h: u32) {
        if self.dom.is_live(h as DomId) {
            self.dom.free(h as DomId);
        }
    }

    /// `applyInheritance(root, operations, url)`; returns the new root.
    #[wasm_bindgen(js_name = applyInheritance)]
    pub fn apply_inheritance(
        &mut self,
        root: u32,
        operations: u32,
        url: &str,
    ) -> Result<u32, JsError> {
        let (root, ops) = (self.check(root)?, self.check(operations)?);
        self.js
            .apply(&mut self.dom, root, ops, url)
            .map(|r| r as u32)
            .map_err(|e| js_err(e.0))
    }

    /// `console.warn` messages of the last `applyInheritance`.
    #[wasm_bindgen(js_name = takeWarnings)]
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.js.warnings)
    }

    /// Qualified tag name of element `h`.
    #[wasm_bindgen(js_name = tagName)]
    pub fn tag_name(&self, h: u32) -> Result<String, JsError> {
        Ok(self.dom.node(self.check(h)?).name.to_string())
    }

    /// `getAttribute(name)` of element `h`.
    #[wasm_bindgen(js_name = getAttribute)]
    pub fn get_attribute(&self, h: u32, name: &str) -> Result<Option<String>, JsError> {
        Ok(self.dom.node(self.check(h)?).get(name).map(str::to_owned))
    }

    /// `setAttribute(name, value)` on element `h`.
    #[wasm_bindgen(js_name = setAttribute)]
    pub fn set_attribute(&mut self, h: u32, name: &str, value: &str) -> Result<(), JsError> {
        let id = self.check(h)?;
        self.dom.node_mut(id).set(name, value);
        Ok(())
    }

    /// End of the primary-inheritance step of `templates.js`: when the tag of
    /// `processed` differs from the tag of `element`, move its children into a
    /// new element named like `element`; then copy the attributes of `element`
    /// (except `t-inherit` and `t-inherit-mode`). Returns the resulting root.
    #[wasm_bindgen(js_name = finishPrimary)]
    pub fn finish_primary(&mut self, processed: u32, element: u32) -> Result<u32, JsError> {
        let (mut processed, element) = (self.check(processed)?, self.check(element)?);
        let tag = self.dom.node(element).name.clone();
        if self.dom.node(processed).name != tag {
            let new = self.dom.create_element(&tag, None);
            self.dom.node_mut(new).doc_child = true;
            let children = self.dom.node(processed).children.clone();
            self.dom.append(new, &children).map_err(|e| js_err(e.0))?;
            processed = new;
        }
        let attrs = self.dom.node(element).attrs.clone();
        for a in attrs {
            if a.qname != "t-inherit" && a.qname != "t-inherit-mode" {
                self.dom.node_mut(processed).set(&a.qname, a.value);
            }
        }
        Ok(processed as u32)
    }

    /// `applyContextToTextNode()` restricted to one tree.
    #[wasm_bindgen(js_name = applyTextContexts)]
    pub fn apply_text_contexts(&mut self, h: u32) -> Result<(), JsError> {
        let id = self.check(h)?;
        apply_context_to_text_nodes(&mut self.dom, id);
        Ok(())
    }

    /// JSON description of a tree (see `Dom::to_json`) to rebuild it in the DOM.
    #[wasm_bindgen(js_name = toJson)]
    pub fn to_json(&self, h: u32) -> Result<String, JsError> {
        Ok(self.dom.to_json(self.check(h)?))
    }

    /// `XMLSerializer().serializeToString(tree)`.
    pub fn serialize(&self, h: u32) -> Result<String, JsError> {
        Ok(self.dom.serialize(self.check(h)?))
    }

    /// Start recording the nodes allocated from now on.
    pub fn checkpoint(&mut self) {
        self.dom.checkpoint();
    }

    /// Free every node allocated since `checkpoint()` that is not part of a
    /// tree whose root handle is in `keep`.
    pub fn release(&mut self, keep: Vec<u32>) {
        let keep: Vec<DomId> = keep.into_iter().map(|h| h as DomId).collect();
        self.dom.release(&keep);
    }

    /// Number of live nodes (for leak checks).
    #[wasm_bindgen(js_name = nodeCount)]
    pub fn node_count(&self) -> usize {
        (0..self.dom.nodes.len())
            .filter(|&i| self.dom.is_live(i))
            .count()
    }
}

/// Server dialect (`odoo/tools/template_inheritance.py`) on serialized trees.
#[wasm_bindgen(js_name = applyInheritanceServer)]
pub fn apply_inheritance_server(
    source: &str,
    specs: Vec<String>,
    inherit_branding: bool,
) -> Result<String, JsError> {
    let specs: Vec<&str> = specs.iter().map(String::as_str).collect();
    qweb_inherit::apply_inheritance_str(source, &specs, qweb_inherit::Options { inherit_branding })
        .map_err(|e| js_err(e.message))
}

/// QWeb string renderer (server semantics) with JSON values.
#[wasm_bindgen]
pub struct Renderer {
    host: ValueHost,
    engine: Arc<Engine<ValueHost>>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl Renderer {
    /// A renderer without templates.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Renderer {
        let host = ValueHost::new();
        Renderer {
            engine: Engine::new(host.clone()),
            host,
        }
    }

    /// Register a template source under `name` (drops the compiled templates).
    #[wasm_bindgen(js_name = addTemplate)]
    pub fn add_template(&mut self, name: &str, source: &str) {
        self.host.add_template(name, source);
        self.engine.invalidate();
    }

    /// Render `name` with `values` and `options` (JSON objects; an empty
    /// string is `{}`). See `qweb_value::from_json` for markup, tuples and
    /// objects.
    pub fn render(
        &self,
        name: &str,
        values_json: &str,
        options_json: &str,
    ) -> Result<String, JsError> {
        let parse = |s: &str| -> Result<serde_json::Map<String, serde_json::Value>, JsError> {
            if s.is_empty() {
                return Ok(Default::default());
            }
            match serde_json::from_str(s) {
                Ok(serde_json::Value::Object(m)) => Ok(m),
                Ok(_) => Err(js_err("expected a JSON object")),
                Err(e) => Err(js_err(e.to_string())),
            }
        };
        let values = parse(values_json)?;
        let options = parse(options_json)?;
        let scope = self
            .host
            .make_scope(values.iter().map(|(k, v)| (k.as_str(), from_json(v))));
        let ctx = Context::new(options.iter().map(|(k, v)| (k.clone(), from_json(v))));
        self.engine
            .render(&qweb_core::TemplateRef::parse(name), scope, ctx)
            .map_err(|e| js_err(e.to_string()))
    }

    /// Warnings emitted since the last call (unknown directives, ...).
    #[wasm_bindgen(js_name = takeWarnings)]
    pub fn take_warnings(&self) -> Vec<String> {
        self.host.take_warnings()
    }
}
