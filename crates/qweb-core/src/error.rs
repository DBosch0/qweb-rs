use std::any::Any;
use std::fmt;
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
    /// An error raised by host code (e.g. a Python exception), passed through untouched.
    Host(Box<dyn Any + Send + Sync>),
}

/// Rendering/compilation error with QWeb location information.
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
    pub fn new(kind: ErrorKind) -> Self {
        Error { inner: Box::new(Inner { kind, location: None, trace: Vec::new() }) }
    }
    pub fn syntax(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Syntax(msg.into()))
    }
    pub fn value(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Value(msg.into()))
    }
    pub fn key(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Key(msg.into()))
    }
    pub fn type_error(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Type(msg.into()))
    }
    pub fn assertion(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Assertion(msg.into()))
    }
    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotImplemented(msg.into()))
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other(msg.into()))
    }
    pub fn host(err: impl Any + Send + Sync) -> Self {
        Self::new(ErrorKind::Host(Box::new(err)))
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.inner.kind
    }
    pub fn into_kind(self) -> ErrorKind {
        self.inner.kind
    }
    pub fn location(&self) -> Option<&Frame> {
        self.inner.location.as_ref()
    }
    pub fn template(&self) -> Option<&str> {
        self.inner.location.as_ref().map(|f| &*f.template)
    }
    pub fn path(&self) -> Option<&str> {
        self.inner.location.as_ref().map(|f| &*f.path)
    }
    pub fn element(&self) -> Option<&str> {
        self.inner.location.as_ref().map(|f| &*f.element)
    }
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

    /// The call stack, outermost first, ending with the location.
    pub fn source(&self) -> Vec<&Frame> {
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
            ErrorKind::Host(_) => "host error".to_owned(),
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
            write!(f, "\nTemplate: {}\nPath: {}\nNode: {}", l.template, l.path, l.element)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
