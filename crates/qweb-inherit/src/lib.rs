//! Odoo template inheritance with its own XPath 1.0 engine, usable from
//! Python and WASM, in two dialects:
//! * [`inherit`]: port of `odoo/tools/template_inheritance.py` (server, lxml model);
//! * [`js`]: port of `web/static/src/core/template_inheritance.js` (browser, DOM model).

pub mod dom;
pub mod inherit;
pub mod js;
pub mod xpath;

pub use inherit::{ErrorKind, InheritError, Options, apply_inheritance_specs, apply_inheritance_str, locate_node};
pub use xpath::{Dialect, XNode, XPath, XPathError};
