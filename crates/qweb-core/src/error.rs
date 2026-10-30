//! Errors raised while compiling or rendering templates.
//!
//! An [`Error`] has a kind ([`Error::kind`]), which hosts map onto their
//! native exception classes, a short message ([`Error::message`]), and QWeb
//! location information: the innermost element where it happened
//! ([`Error::location`]) and the template call stack ([`Error::stack`]).

use core::{error, fmt, result};
use std::sync::Arc;

/// A position in a template: an element of a compiled template.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Template name (`xmlid`, or reference).
    pub template: Arc<str>,
    /// Template reference (id), as text.
    pub reference: Arc<str>,
    /// XPath of the element.
    pub path: Arc<str>,
    /// Start tag of the element (with its original attributes).
    pub element: Arc<str>,
}

/// An error raised by host code, carried through the engine untouched.
pub type HostError = Box<dyn std::error::Error + Send + Sync>;

/// Error kinds, mapped by hosts onto their native exception classes.
#[derive(Debug)]
pub enum ErrorKind {
    /// Invalid template or expression syntax (`SyntaxError`).
    Syntax(String),
    /// `ValueError`.
    Value(String),
    /// `KeyError` (missing strict name lookup).
    Key(String),
    /// `TypeError`.
    Type(String),
    /// `AssertionError` (e.g. invalid `t-field` usage).
    Assertion(String),
    /// `NotImplementedError` (fields/widgets/assets without ORM).
    NotImplemented(String),
    /// `RecursionError` (t-call nesting too deep).
    Recursion(String),
    /// XML parse error (`lxml.etree.XMLSyntaxError`).
    Xml(String),
    /// Any other error raised by the engine itself.
    Other(String),
    /// An error raised by host code (e.g. a Python exception), passed
    /// through untouched. Hosts get their own type back with
    /// [`downcast`](HostError::downcast).
    Host(HostError),
}

/// Rendering/compilation error with QWeb location information.
///
/// The payload is boxed so that `Result<T, Error>` stays one pointer wide.
pub struct Error {
    inner: Box<Inner>,
}

struct Inner {
    kind: ErrorKind,
    /// Element where the error occurred (innermost).
    location: Option<Frame>,
    /// Call stack, innermost first: `t-call` sites, lazy contents (`t-set` /
    /// `t-call` bodies) and the `t-out` that rendered them.
    trace: Vec<Frame>,
}

impl Error {
    /// A new error of the given kind, without location.
    pub fn new(kind: ErrorKind) -> Self {
        Error {
            inner: Box::new(Inner {
                kind,
                location: None,
                trace: Vec::new(),
            }),
        }
    }

    /// A [`ErrorKind::Syntax`] error.
    pub fn syntax(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Syntax(msg.into()))
    }

    /// A [`ErrorKind::Value`] error.
    pub fn value(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Value(msg.into()))
    }

    /// A [`ErrorKind::Key`] error.
    pub fn key(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Key(msg.into()))
    }

    /// A [`ErrorKind::Type`] error.
    pub fn type_error(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Type(msg.into()))
    }

    /// A [`ErrorKind::Assertion`] error.
    pub fn assertion(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Assertion(msg.into()))
    }

    /// A [`ErrorKind::NotImplemented`] error.
    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotImplemented(msg.into()))
    }

    /// A [`ErrorKind::Other`] error.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other(msg.into()))
    }

    /// Wrap an error raised by host code ([`ErrorKind::Host`]).
    pub fn host(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::new(ErrorKind::Host(Box::new(err)))
    }

    /// The kind of the error.
    pub fn kind(&self) -> &ErrorKind {
        &self.inner.kind
    }

    /// Consume the error and return its kind.
    pub fn into_kind(self) -> ErrorKind {
        self.inner.kind
    }

    /// The innermost element where the error occurred, if known.
    pub fn location(&self) -> Option<&Frame> {
        self.inner.location.as_ref()
    }

    /// Name of the template where the error occurred, if known.
    pub fn template(&self) -> Option<&str> {
        self.inner.location.as_ref().map(|f| &*f.template)
    }

    /// XPath of the element where the error occurred, if known.
    pub fn path(&self) -> Option<&str> {
        self.inner.location.as_ref().map(|f| &*f.path)
    }

    /// Start tag of the element where the error occurred, if known.
    pub fn element(&self) -> Option<&str> {
        self.inner.location.as_ref().map(|f| &*f.element)
    }

    /// Whether the error has a location.
    pub fn has_location(&self) -> bool {
        self.inner.location.is_some()
    }

    /// Attach the location unless already present (innermost wins).
    pub fn with_location(mut self, frame: Frame) -> Self {
        if self.inner.location.is_none() {
            self.inner.location = Some(frame);
        }
        self
    }

    /// Add a caller frame (or the location if there is none yet).
    pub fn with_frame(mut self, frame: Frame) -> Self {
        if self.inner.location.is_none() {
            self.inner.location = Some(frame);
        } else {
            self.inner.trace.push(frame);
        }
        self
    }

    /// The template call stack, outermost first, ending with the location.
    pub fn stack(&self) -> Vec<&Frame> {
        let mut v: Vec<&Frame> = self.inner.trace.iter().rev().collect();
        v.extend(self.inner.location.as_ref());
        v
    }

    /// Short message without location.
    pub fn message(&self) -> String {
        match &self.inner.kind {
            ErrorKind::Syntax(m)
            | ErrorKind::Value(m)
            | ErrorKind::Key(m)
            | ErrorKind::Type(m)
            | ErrorKind::Assertion(m)
            | ErrorKind::NotImplemented(m)
            | ErrorKind::Recursion(m)
            | ErrorKind::Xml(m)
            | ErrorKind::Other(m) => m.clone(),
            ErrorKind::Host(e) => e.to_string(),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match &self.inner.kind {
            ErrorKind::Syntax(_) => "SyntaxError",
            ErrorKind::Value(_) => "ValueError",
            ErrorKind::Key(_) => "KeyError",
            ErrorKind::Type(_) => "TypeError",
            ErrorKind::Assertion(_) => "AssertionError",
            ErrorKind::NotImplemented(_) => "NotImplementedError",
            ErrorKind::Recursion(_) => "RecursionError",
            ErrorKind::Xml(_) => "XMLSyntaxError",
            ErrorKind::Other(_) => "Error",
            ErrorKind::Host(_) => "HostError",
        };
        write!(f, "{kind}: {}", self.message())?;
        if let Some(l) = &self.inner.location {
            write!(
                f,
                "\nTemplate: {}\nPath: {}\nNode: {}",
                l.template, l.path, l.element
            )?;
        }
        Ok(())
    }
}

impl error::Error for Error {
    /// The host error, for [`ErrorKind::Host`].
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.inner.kind {
            ErrorKind::Host(e) => Some(&**e),
            _ => None,
        }
    }
}

/// `Result` with [`Error`] as the error type.
pub type Result<T> = result::Result<T, Error>;
