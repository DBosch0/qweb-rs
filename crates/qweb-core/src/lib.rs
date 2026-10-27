//! # qweb-core
//!
//! A fast, host-agnostic implementation of Odoo's QWeb templating engine with
//! output byte-identical to the Python implementation (`ir.qweb`).
//!
//! Values and expression semantics are delegated to a [`Host`]: the Python
//! binding evaluates expressions through the CPython object protocol, while
//! `qweb-value` provides a pure-Rust host.

pub mod compile;
pub mod error;
pub mod escape;
pub mod expr;
pub mod format;
pub mod host;
pub mod ir;
pub mod runtime;
pub mod text;
pub mod url;
pub mod xml;

pub use error::{Error, ErrorKind, Frame, Result};
pub use host::{CompileOptions, Converted, Host, Loaded, LoopSource, TemplateRef};
pub use runtime::{Engine, Lazy};
