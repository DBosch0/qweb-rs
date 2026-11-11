//! `XPath` 1.0 over a [`qweb_core::xml::Document`] (lxml data model).
//!
//! Supports the full expression grammar (axes, predicates, unions, operators,
//! the core function library) plus Odoo's `hasclass(*classes)` extension and
//! lxml `ETXPath` names in Clark notation (`{uri}local`). Prefixed names and
//! variables are not supported (Odoo has no namespace mapping for them).

// XPath compares numbers exactly, as IEEE 754 doubles.
#![allow(clippy::float_cmp)]

use core::{fmt, hash::Hash};
use std::collections::HashMap;

use qweb_core::xml::{Document, NodeId, NodeKind, XStr, local_name, namespace};

/// A node of the `XPath` data model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XNode {
    /// Document node above the tree whose root element is the given node.
    Root(NodeId),
    /// Element, comment or processing instruction.
    Node(NodeId),
    /// Text of an element (`.text`).
    Text(NodeId),
    /// Tail of a node (`.tail`), a child of its parent.
    Tail(NodeId),
    /// Attribute `index` of an element.
    Attr(NodeId, u32),
}

/// Invalid `XPath` expression or evaluation error.
#[derive(Debug, Clone, PartialEq)]
pub struct XPathError(pub String);

impl fmt::Display for XPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type Res<T> = Result<T, XPathError>;

fn err<T>(msg: impl Into<String>) -> Res<T> {
    Err(XPathError(msg.into()))
}

// ---------------------------------------------------------------- lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Slash,
    DSlash,
    LBrack,
    RBrack,
    LParen,
    RParen,
    At,
    Comma,
    DColon,
    Dot,
    DDot,
    Pipe,
    Plus,
    Minus,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Star,
    /// Operator names: and, or, div, mod.
    OpName(&'static str),
    Lit(String),
    Num(f64),
    /// Name (`NCName`, `prefix:local`, `{uri}local` or `{uri}*`, `prefix:*`).
    Name(String),
    Var(String),
}

fn lex(src: &str) -> Res<Vec<Tok>> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();
    // XPath 1.0 §3.7: `*` and operator names are operators when there is a
    // preceding token that is not @ :: ( [ , or an operator.
    let operator_ctx = |out: &Vec<Tok>| match out.last() {
        None => false,
        Some(t) => !matches!(
            t,
            Tok::At
                | Tok::DColon
                | Tok::LParen
                | Tok::LBrack
                | Tok::Comma
                | Tok::Slash
                | Tok::DSlash
                | Tok::Pipe
                | Tok::Plus
                | Tok::Minus
                | Tok::Eq
                | Tok::Neq
                | Tok::Lt
                | Tok::Le
                | Tok::Gt
                | Tok::Ge
                | Tok::Star
                | Tok::OpName(_)
        ),
    };
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                out.push(Tok::DSlash);
                i += 2;
            }
            b'/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            b'[' => {
                out.push(Tok::LBrack);
                i += 1;
            }
            b']' => {
                out.push(Tok::RBrack);
                i += 1;
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'@' => {
                out.push(Tok::At);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b':' if b.get(i + 1) == Some(&b':') => {
                out.push(Tok::DColon);
                i += 2;
            }
            b'|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            b'+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            b'-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            b'=' => {
                out.push(Tok::Eq);
                i += 1;
            }
            b'!' if b.get(i + 1) == Some(&b'=') => {
                out.push(Tok::Neq);
                i += 2;
            }
            b'<' if b.get(i + 1) == Some(&b'=') => {
                out.push(Tok::Le);
                i += 2;
            }
            b'<' => {
                out.push(Tok::Lt);
                i += 1;
            }
            b'>' if b.get(i + 1) == Some(&b'=') => {
                out.push(Tok::Ge);
                i += 2;
            }
            b'>' => {
                out.push(Tok::Gt);
                i += 1;
            }
            b'*' => {
                if operator_ctx(&out) {
                    out.push(Tok::Star);
                } else {
                    out.push(Tok::Name("*".into()));
                }
                i += 1;
            }
            b'"' | b'\'' => {
                let end = src[i + 1..].find(c as char).map(|e| i + 1 + e);
                let Some(end) = end else {
                    return err("unterminated string literal");
                };
                out.push(Tok::Lit(src[i + 1..end].to_owned()));
                i = end + 1;
            }
            b'.' if b.get(i + 1) == Some(&b'.') => {
                out.push(Tok::DDot);
                i += 2;
            }
            b'.' if !b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                out.push(Tok::Dot);
                i += 1;
            }
            b'0'..=b'9' | b'.' => {
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i < b.len() && b[i] == b'.' {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                out.push(Tok::Num(src[start..i].parse().unwrap_or(f64::NAN)));
            }
            b'$' => {
                let start = i + 1;
                i = start;
                while i < b.len() && is_name_char(src, i) {
                    i += next_len(src, i);
                }
                out.push(Tok::Var(src[start..i].to_owned()));
            }
            b'{' => {
                // Clark notation (lxml ETXPath)
                let Some(close) = src[i..].find('}') else {
                    return err("unterminated namespace URI");
                };
                let mut j = i + close + 1;
                if b.get(j) == Some(&b'*') {
                    j += 1;
                } else {
                    while j < b.len() && is_name_char(src, j) {
                        j += next_len(src, j);
                    }
                }
                out.push(Tok::Name(src[i..j].to_owned()));
                i = j;
            }
            _ if is_name_start(src, i) => {
                let start = i;
                while i < b.len() && is_name_char(src, i) {
                    i += next_len(src, i);
                }
                // QName / prefix:*
                if b.get(i) == Some(&b':') && b.get(i + 1) != Some(&b':') {
                    if b.get(i + 1) == Some(&b'*') {
                        i += 2;
                    } else if i + 1 < b.len() && is_name_start(src, i + 1) {
                        i += 1;
                        while i < b.len() && is_name_char(src, i) {
                            i += next_len(src, i);
                        }
                    }
                }
                let name = &src[start..i];
                if operator_ctx(&out) {
                    let op = match name {
                        "and" => "and",
                        "or" => "or",
                        "div" => "div",
                        "mod" => "mod",
                        _ => return err(format!("unexpected name {name:?}")),
                    };
                    out.push(Tok::OpName(op));
                } else {
                    out.push(Tok::Name(name.to_owned()));
                }
            }
            _ => {
                return err(format!(
                    "unexpected character {:?}",
                    &src[i..i + next_len(src, i)]
                ));
            }
        }
    }
    Ok(out)
}

fn next_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(1, char::len_utf8)
}

fn is_name_start(s: &str, i: usize) -> bool {
    s[i..]
        .chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_alphabetic())
}

fn is_name_char(s: &str, i: usize) -> bool {
    s[i..]
        .chars()
        .next()
        .is_some_and(|c| c == '_' || c == '-' || c == '.' || c.is_alphanumeric() || c == '\u{B7}')
}

// ---------------------------------------------------------------- AST

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
    Ancestor,
    AncestorOrSelf,
    Attribute,
    Child,
    Descendant,
    DescendantOrSelf,
    Following,
    FollowingSibling,
    Namespace,
    Parent,
    Preceding,
    PrecedingSibling,
    Self_,
}

impl Axis {
    fn reverse(self) -> bool {
        matches!(
            self,
            Axis::Ancestor | Axis::AncestorOrSelf | Axis::Preceding | Axis::PrecedingSibling
        )
    }
}

#[derive(Debug, Clone)]
enum NodeTest {
    /// `*` (any principal node).
    Any,
    /// `{uri}*` (`None` means no namespace — not reachable from syntax).
    AnyIn(String),
    /// Name with optional namespace URI.
    Name(Option<String>, String),
    Text,
    Comment,
    Node,
    Pi(Option<String>),
}

#[derive(Debug, Clone)]
struct Step {
    axis: Axis,
    test: NodeTest,
    preds: Vec<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Or,
    And,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone)]
enum Expr {
    Bin(Op, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    /// Location path; `absolute` starts at the document node.
    Path {
        absolute: bool,
        steps: Vec<Step>,
    },
    /// Filter expression followed by an optional relative path.
    Filter {
        primary: Box<Expr>,
        preds: Vec<Expr>,
        steps: Vec<Step>,
    },
    Lit(String),
    Num(f64),
    Call(String, Vec<Expr>),
}

// ---------------------------------------------------------------- parser

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok) -> Res<()> {
        if self.eat(t) {
            Ok(())
        } else {
            err(format!("expected {t:?}, found {:?}", self.peek()))
        }
    }

    fn expr(&mut self) -> Res<Expr> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Res<Expr> {
        let mut l = self.and_expr()?;
        while self.eat(&Tok::OpName("or")) {
            let r = self.and_expr()?;
            l = Expr::Bin(Op::Or, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> Res<Expr> {
        let mut l = self.eq_expr()?;
        while self.eat(&Tok::OpName("and")) {
            let r = self.eq_expr()?;
            l = Expr::Bin(Op::And, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn eq_expr(&mut self) -> Res<Expr> {
        let mut l = self.rel_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Eq) => Op::Eq,
                Some(Tok::Neq) => Op::Neq,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.rel_expr()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn rel_expr(&mut self) -> Res<Expr> {
        let mut l = self.add_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Lt) => Op::Lt,
                Some(Tok::Le) => Op::Le,
                Some(Tok::Gt) => Op::Gt,
                Some(Tok::Ge) => Op::Ge,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.add_expr()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn add_expr(&mut self) -> Res<Expr> {
        let mut l = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => Op::Add,
                Some(Tok::Minus) => Op::Sub,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.mul_expr()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn mul_expr(&mut self) -> Res<Expr> {
        let mut l = self.unary_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => Op::Mul,
                Some(Tok::OpName("div")) => Op::Div,
                Some(Tok::OpName("mod")) => Op::Mod,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.unary_expr()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn unary_expr(&mut self) -> Res<Expr> {
        if self.eat(&Tok::Minus) {
            return Ok(Expr::Neg(Box::new(self.unary_expr()?)));
        }
        self.union_expr()
    }

    fn union_expr(&mut self) -> Res<Expr> {
        let mut l = self.path_expr()?;
        while self.eat(&Tok::Pipe) {
            let r = self.path_expr()?;
            l = Expr::Union(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn is_primary_start(&self) -> bool {
        match self.peek() {
            Some(Tok::Lit(_) | Tok::Num(_) | Tok::LParen | Tok::Var(_)) => true,
            Some(Tok::Name(n)) => {
                self.peek_at(1) == Some(&Tok::LParen)
                    && !matches!(
                        n.as_str(),
                        "node" | "text" | "comment" | "processing-instruction"
                    )
            }
            _ => false,
        }
    }

    fn path_expr(&mut self) -> Res<Expr> {
        if self.is_primary_start() {
            let primary = self.primary()?;
            let mut preds = Vec::new();
            while self.peek() == Some(&Tok::LBrack) {
                preds.push(self.predicate()?);
            }
            let mut steps = Vec::new();
            if matches!(self.peek(), Some(Tok::Slash | Tok::DSlash)) {
                self.rel_steps(&mut steps, true)?;
            }
            if preds.is_empty() && steps.is_empty() {
                return Ok(primary);
            }
            return Ok(Expr::Filter {
                primary: Box::new(primary),
                preds,
                steps,
            });
        }
        let mut steps = Vec::new();
        match self.peek() {
            Some(Tok::Slash) => {
                self.pos += 1;
                if self.is_step_start() {
                    self.rel_steps(&mut steps, false)?;
                }
                Ok(Expr::Path {
                    absolute: true,
                    steps,
                })
            }
            Some(Tok::DSlash) => {
                self.rel_steps(&mut steps, true)?;
                Ok(Expr::Path {
                    absolute: true,
                    steps,
                })
            }
            _ => {
                self.rel_steps(&mut steps, false)?;
                Ok(Expr::Path {
                    absolute: false,
                    steps,
                })
            }
        }
    }

    fn is_step_start(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Name(_) | Tok::At | Tok::Dot | Tok::DDot)
        )
    }

    /// Steps of a relative path; `leading_sep` means a `/` or `//` comes first.
    fn rel_steps(&mut self, steps: &mut Vec<Step>, leading_sep: bool) -> Res<()> {
        if !leading_sep {
            steps.push(self.step()?);
        }
        loop {
            match self.peek() {
                Some(Tok::Slash) => {
                    self.pos += 1;
                }
                Some(Tok::DSlash) => {
                    self.pos += 1;
                    let next = self.step()?;
                    if next.axis == Axis::Child && !next.preds.iter().any(positional) {
                        // `//x[p]` is `descendant::x[p]` when `p` does not
                        // depend on the position: one pass, already in
                        // document order.
                        steps.push(Step {
                            axis: Axis::Descendant,
                            ..next
                        });
                    } else {
                        steps.push(Step {
                            axis: Axis::DescendantOrSelf,
                            test: NodeTest::Node,
                            preds: Vec::new(),
                        });
                        steps.push(next);
                    }
                    continue;
                }
                _ => return Ok(()),
            }
            steps.push(self.step()?);
        }
    }

    fn step(&mut self) -> Res<Step> {
        if self.eat(&Tok::Dot) {
            return Ok(Step {
                axis: Axis::Self_,
                test: NodeTest::Node,
                preds: Vec::new(),
            });
        }
        if self.eat(&Tok::DDot) {
            return Ok(Step {
                axis: Axis::Parent,
                test: NodeTest::Node,
                preds: Vec::new(),
            });
        }
        let mut axis = Axis::Child;
        if self.eat(&Tok::At) {
            axis = Axis::Attribute;
        } else if let (Some(Tok::Name(n)), Some(Tok::DColon)) = (self.peek(), self.peek_at(1)) {
            axis = match n.as_str() {
                "ancestor" => Axis::Ancestor,
                "ancestor-or-self" => Axis::AncestorOrSelf,
                "attribute" => Axis::Attribute,
                "child" => Axis::Child,
                "descendant" => Axis::Descendant,
                "descendant-or-self" => Axis::DescendantOrSelf,
                "following" => Axis::Following,
                "following-sibling" => Axis::FollowingSibling,
                "namespace" => Axis::Namespace,
                "parent" => Axis::Parent,
                "preceding" => Axis::Preceding,
                "preceding-sibling" => Axis::PrecedingSibling,
                "self" => Axis::Self_,
                _ => return err(format!("unknown axis {n:?}")),
            };
            self.pos += 2;
        }
        let test = match self.peek().cloned() {
            Some(Tok::Name(n)) => {
                self.pos += 1;
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    let t = match n.as_str() {
                        "node" => NodeTest::Node,
                        "text" => NodeTest::Text,
                        "comment" => NodeTest::Comment,
                        "processing-instruction" => {
                            if let Some(Tok::Lit(l)) = self.peek().cloned() {
                                self.pos += 1;
                                NodeTest::Pi(Some(l))
                            } else {
                                NodeTest::Pi(None)
                            }
                        }
                        _ => return err(format!("invalid node test {n:?}")),
                    };
                    self.expect(&Tok::RParen)?;
                    t
                } else if n == "*" {
                    NodeTest::Any
                } else if let Some(rest) = n.strip_prefix('{') {
                    let (uri, local) = rest.split_once('}').unwrap_or((rest, ""));
                    if local == "*" {
                        NodeTest::AnyIn(uri.to_owned())
                    } else {
                        NodeTest::Name(Some(uri.to_owned()), local.to_owned())
                    }
                } else if n.contains(':') {
                    return err(format!("undefined namespace prefix in {n:?}"));
                } else {
                    NodeTest::Name(None, n)
                }
            }
            t => return err(format!("expected a node test, found {t:?}")),
        };
        let mut preds = Vec::new();
        while self.peek() == Some(&Tok::LBrack) {
            preds.push(self.predicate()?);
        }
        Ok(Step { axis, test, preds })
    }

    fn predicate(&mut self) -> Res<Expr> {
        self.expect(&Tok::LBrack)?;
        let e = self.expr()?;
        self.expect(&Tok::RBrack)?;
        Ok(e)
    }

    fn primary(&mut self) -> Res<Expr> {
        match self.peek().cloned() {
            Some(Tok::Lit(s)) => {
                self.pos += 1;
                Ok(Expr::Lit(s))
            }
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::Var(v)) => err(format!("undefined variable ${v}")),
            Some(Tok::Name(n)) => {
                self.pos += 2; // name (
                let mut args = Vec::new();
                if !self.eat(&Tok::RParen) {
                    loop {
                        args.push(self.expr()?);
                        if self.eat(&Tok::RParen) {
                            break;
                        }
                        self.expect(&Tok::Comma)?;
                    }
                }
                Ok(Expr::Call(n, args))
            }
            t => err(format!("unexpected token {t:?}")),
        }
    }
}

/// Whether a predicate may depend on the context position or size: it may
/// evaluate to a number (`[1]`, `[count(x)]`), or it calls `position()` or
/// `last()` for its own context. Conservative: unknown functions count.
fn positional(e: &Expr) -> bool {
    returns_number(e) || uses_position(e)
}

fn returns_number(e: &Expr) -> bool {
    match e {
        Expr::Num(_) | Expr::Neg(_) => true,
        Expr::Bin(op, ..) => matches!(op, Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod),
        Expr::Lit(_) | Expr::Path { .. } | Expr::Filter { .. } | Expr::Union(..) => false,
        Expr::Call(name, _) => !matches!(
            name.as_str(),
            "string"
                | "concat"
                | "starts-with"
                | "contains"
                | "substring-before"
                | "substring-after"
                | "substring"
                | "normalize-space"
                | "translate"
                | "local-name"
                | "name"
                | "namespace-uri"
                | "boolean"
                | "not"
                | "true"
                | "false"
                | "hasclass"
        ),
    }
}

/// Whether `e` calls `position()` or `last()` in the current context
/// (predicates of nested steps have their own context).
fn uses_position(e: &Expr) -> bool {
    match e {
        Expr::Call(name, args) => {
            matches!(name.as_str(), "position" | "last") || args.iter().any(uses_position)
        }
        Expr::Bin(_, a, b) | Expr::Union(a, b) => uses_position(a) || uses_position(b),
        Expr::Neg(a) => uses_position(a),
        Expr::Filter { primary, .. } => uses_position(primary),
        Expr::Path { .. } | Expr::Lit(_) | Expr::Num(_) => false,
    }
}

/// A compiled `XPath` expression.
#[derive(Debug, Clone)]
pub struct XPath {
    expr: Expr,
}

/// Function dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    /// lxml with Odoo's `hasclass` (class attribute split on whitespace).
    #[default]
    Lxml,
    /// Browser `XPath` with `hasclass` rewritten like `template_inheritance.js`
    /// (`contains(concat(' ', @class, ' '), ' c ')`).
    Browser,
}

impl XPath {
    /// Parse an `XPath` 1.0 expression.
    ///
    /// # Errors
    ///
    /// The expression is empty or not valid `XPath` 1.0.
    pub fn parse(src: &str) -> Res<XPath> {
        let toks = lex(src)?;
        if toks.is_empty() {
            return err("empty expression");
        }
        let mut p = Parser { toks, pos: 0 };
        let expr = p.expr()?;
        if p.pos != p.toks.len() {
            return err(format!("unexpected token {:?}", p.toks[p.pos]));
        }
        Ok(XPath { expr })
    }

    /// Evaluate on any [`Tree`]; returns the node-set in document order.
    ///
    /// # Errors
    ///
    /// The expression does not evaluate to a node-set, or calls an unknown
    /// function or one with wrong arguments.
    pub fn select_in<T: Tree>(&self, tree: &T, context: T::N, dialect: Dialect) -> Res<Vec<T::N>> {
        let mut ev = Eval {
            t: tree,
            order: None,
            dialect,
        };
        match ev.eval(
            &self.expr,
            &Ctx {
                node: context,
                pos: 1,
                size: 1,
            },
        )? {
            Val::Nodes(n) => Ok(n),
            _ => err("expression does not evaluate to a node-set"),
        }
    }

    /// Evaluate with `context` as context node (lxml model); returns the
    /// node-set in document order, without the document node (like lxml).
    ///
    /// # Errors
    ///
    /// As for [`XPath::select_in`].
    pub fn select(&self, doc: &Document, context: NodeId) -> Res<Vec<XNode>> {
        let n = self.select_in(&LxmlTree(doc), XNode::Node(context), Dialect::Lxml)?;
        Ok(n.into_iter()
            .filter(|n| !matches!(n, XNode::Root(_)))
            .collect())
    }

    /// First element of the node-set (lxml `xpath(...)[0]`), `None` if empty.
    ///
    /// # Errors
    ///
    /// As for [`XPath::select_in`], or the first node is not an element.
    pub fn first_element(&self, doc: &Document, context: NodeId) -> Res<Option<NodeId>> {
        match self.select(doc, context)?.first() {
            None => Ok(None),
            Some(XNode::Node(id)) => Ok(Some(*id)),
            Some(other) => err(format!("xpath result is not an element: {other:?}")),
        }
    }
}

// ---------------------------------------------------------------- tree model

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// `XPath` node type.
pub enum Kind {
    /// Document node.
    Root,
    /// Element.
    Element,
    /// Text node.
    Text,
    /// Comment.
    Comment,
    /// Processing instruction.
    Pi,
    /// Attribute.
    Attr,
}

/// The `XPath` data model over some tree representation.
pub trait Tree {
    /// Node handle.
    type N: Copy + Eq + Hash + std::fmt::Debug;
    /// Node type of `n`.
    fn kind(&self, n: Self::N) -> Kind;
    /// Parent of `n` (the owner element for an attribute).
    fn parent(&self, n: Self::N) -> Option<Self::N>;
    /// Children (document and element nodes only).
    fn children(&self, n: Self::N, out: &mut Vec<Self::N>);
    /// Attributes of an element (namespace declarations excluded).
    fn attributes(&self, n: Self::N, out: &mut Vec<Self::N>);
    /// Namespace URI and local name of an element or attribute.
    fn name(&self, n: Self::N) -> (Option<&str>, &str);
    /// Qualified name (for `name()`).
    fn qname(&self, n: Self::N) -> String;
    /// Text, comment, PI or attribute content.
    fn value(&self, n: Self::N) -> &str;
    /// Target of a processing instruction.
    fn pi_target(&self, n: Self::N) -> &str;
    /// Document node of the tree containing `n`.
    fn document(&self, n: Self::N) -> Self::N;
    /// Value of the `class` attribute of an element.
    fn class(&self, n: Self::N) -> Option<&str>;
}

/// The lxml model of a [`Document`].
#[derive(Debug)]
pub struct LxmlTree<'a>(pub &'a Document);

impl LxmlTree<'_> {
    fn has_text(s: Option<&XStr>) -> bool {
        s.is_some_and(|s| !s.is_empty())
    }
}

impl Tree for LxmlTree<'_> {
    type N = XNode;

    fn kind(&self, n: XNode) -> Kind {
        match n {
            XNode::Root(_) => Kind::Root,
            XNode::Node(id) => match self.0.nodes[id].kind {
                NodeKind::Element => Kind::Element,
                NodeKind::Comment => Kind::Comment,
                NodeKind::Pi => Kind::Pi,
            },
            XNode::Text(_) | XNode::Tail(_) => Kind::Text,
            XNode::Attr(..) => Kind::Attr,
        }
    }

    fn parent(&self, n: XNode) -> Option<XNode> {
        match n {
            XNode::Root(_) => None,
            XNode::Node(id) => Some(match self.0.nodes[id].parent {
                Some(p) => XNode::Node(p),
                None => XNode::Root(id),
            }),
            XNode::Text(id) | XNode::Attr(id, _) => Some(XNode::Node(id)),
            XNode::Tail(id) => self.0.nodes[id].parent.map(XNode::Node),
        }
    }

    fn children(&self, n: XNode, out: &mut Vec<XNode>) {
        match n {
            XNode::Root(top) => out.push(XNode::Node(top)),
            XNode::Node(id) if self.0.nodes[id].kind == NodeKind::Element => {
                let node = &self.0.nodes[id];
                if Self::has_text(node.text.as_ref()) {
                    out.push(XNode::Text(id));
                }
                for &c in &node.children {
                    out.push(XNode::Node(c));
                    if Self::has_text(self.0.nodes[c].tail.as_ref()) {
                        out.push(XNode::Tail(c));
                    }
                }
            }
            _ => {}
        }
    }

    fn attributes(&self, n: XNode, out: &mut Vec<XNode>) {
        if let XNode::Node(id) = n
            && self.0.nodes[id].kind == NodeKind::Element
        {
            out.extend((0..self.0.nodes[id].attrs.len() as u32).map(|i| XNode::Attr(id, i)));
        }
    }

    fn name(&self, n: XNode) -> (Option<&str>, &str) {
        let full: &str = match n {
            XNode::Node(id) => &self.0.nodes[id].tag,
            XNode::Attr(id, i) => &self.0.nodes[id].attrs[i as usize].0,
            _ => "",
        };
        (namespace(full), local_name(full))
    }

    fn qname(&self, n: XNode) -> String {
        let (ns, local) = self.name(n);
        match (n, ns) {
            (XNode::Node(id), Some(_)) => match &self.0.nodes[id].prefix {
                Some(p) => format!("{p}:{local}"),
                None => local.to_owned(),
            },
            _ => local.to_owned(),
        }
    }

    fn value(&self, n: XNode) -> &str {
        match n {
            XNode::Node(id) | XNode::Text(id) => self.0.nodes[id].text.as_deref().unwrap_or(""),
            XNode::Tail(id) => self.0.nodes[id].tail.as_deref().unwrap_or(""),
            XNode::Attr(id, i) => &self.0.nodes[id].attrs[i as usize].1,
            XNode::Root(_) => "",
        }
    }

    fn pi_target(&self, n: XNode) -> &str {
        match n {
            XNode::Node(id) => &self.0.nodes[id].tag,
            _ => "",
        }
    }

    fn document(&self, n: XNode) -> XNode {
        let id = match n {
            XNode::Root(t) => return XNode::Root(t),
            XNode::Node(id) | XNode::Text(id) | XNode::Tail(id) | XNode::Attr(id, _) => id,
        };
        XNode::Root(self.0.top(id))
    }

    fn class(&self, n: XNode) -> Option<&str> {
        match n {
            XNode::Node(id) => self.0.nodes[id].get("class"),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- evaluation

#[derive(Debug, Clone)]
enum Val<N> {
    Nodes(Vec<N>),
    Str(String),
    Num(f64),
    Bool(bool),
}

struct Ctx<N> {
    node: N,
    pos: usize,
    size: usize,
}

struct Eval<'a, T: Tree> {
    t: &'a T,
    order: Option<HashMap<T::N, u32>>,
    dialect: Dialect,
}

impl<T: Tree> Eval<'_, T> {
    fn key(&mut self, n: T::N) -> u32 {
        let t = self.t;
        let order = self.order.get_or_insert_with(|| {
            let mut map = HashMap::new();
            let mut counter = 0u32;
            let mut stack = vec![t.document(n)];
            let mut buf = Vec::new();
            while let Some(x) = stack.pop() {
                map.insert(x, counter);
                counter += 1;
                buf.clear();
                t.attributes(x, &mut buf);
                for &a in &buf {
                    map.insert(a, counter);
                    counter += 1;
                }
                buf.clear();
                t.children(x, &mut buf);
                stack.extend(buf.iter().rev());
            }
            map
        });
        order.get(&n).copied().unwrap_or(u32::MAX)
    }

    fn sort_dedup(&mut self, mut nodes: Vec<T::N>) -> Vec<T::N> {
        if nodes.len() < 2 {
            return nodes;
        }
        let mut keyed: Vec<(u32, T::N)> = nodes.drain(..).map(|n| (self.key(n), n)).collect();
        keyed.sort_by_key(|k| k.0);
        keyed.dedup_by(|a, b| a.1 == b.1);
        keyed.into_iter().map(|(_, n)| n).collect()
    }

    /// Descendants of `n` in document order (pre-order).
    fn descendants(&self, n: T::N, out: &mut Vec<T::N>) {
        let mut stack = Vec::new();
        let mut kids = Vec::new();
        self.t.children(n, &mut kids);
        stack.extend(kids.drain(..).rev());
        while let Some(x) = stack.pop() {
            out.push(x);
            self.t.children(x, &mut kids);
            stack.extend(kids.drain(..).rev());
        }
    }

    /// Siblings after (`forward`) or before (nearest first) a node.
    fn siblings(&self, n: T::N, forward: bool) -> Vec<T::N> {
        if matches!(self.t.kind(n), Kind::Attr | Kind::Root) {
            return Vec::new();
        }
        let Some(parent) = self.t.parent(n) else {
            return Vec::new();
        };
        let mut all = Vec::new();
        self.t.children(parent, &mut all);
        let Some(pos) = all.iter().position(|&x| x == n) else {
            return Vec::new();
        };
        if forward {
            all[pos + 1..].to_vec()
        } else {
            all[..pos].iter().rev().copied().collect()
        }
    }

    fn axis(&self, axis: Axis, n: T::N) -> Res<Vec<T::N>> {
        let mut out = Vec::new();
        match axis {
            Axis::Child => self.t.children(n, &mut out),
            Axis::Descendant => self.descendants(n, &mut out),
            Axis::DescendantOrSelf => {
                out.push(n);
                self.descendants(n, &mut out);
            }
            Axis::Self_ => out.push(n),
            Axis::Parent => out.extend(self.t.parent(n)),
            Axis::Ancestor | Axis::AncestorOrSelf => {
                if axis == Axis::AncestorOrSelf {
                    out.push(n);
                }
                let mut cur = self.t.parent(n);
                while let Some(p) = cur {
                    out.push(p);
                    cur = self.t.parent(p);
                }
            }
            Axis::FollowingSibling => out = self.siblings(n, true),
            Axis::PrecedingSibling => out = self.siblings(n, false),
            Axis::Following => {
                let mut cur = Some(n);
                if self.t.kind(n) == Kind::Attr
                    && let Some(el) = self.t.parent(n)
                {
                    self.descendants(el, &mut out);
                    cur = Some(el);
                }
                while let Some(c) = cur {
                    for s in self.siblings(c, true) {
                        out.push(s);
                        self.descendants(s, &mut out);
                    }
                    cur = self.t.parent(c);
                }
            }
            Axis::Preceding => {
                // reverse document order, excluding ancestors
                let mut cur = Some(match (self.t.kind(n), self.t.parent(n)) {
                    (Kind::Attr, Some(el)) => el,
                    _ => n,
                });
                while let Some(c) = cur {
                    for s in self.siblings(c, false) {
                        let mut sub = Vec::new();
                        self.descendants(s, &mut sub);
                        out.extend(sub.into_iter().rev());
                        out.push(s);
                    }
                    cur = self.t.parent(c);
                }
            }
            Axis::Attribute => self.t.attributes(n, &mut out),
            Axis::Namespace => return err("the namespace axis is not supported"),
        }
        Ok(out)
    }

    fn matches(&self, axis: Axis, test: &NodeTest, n: T::N) -> bool {
        let kind = self.t.kind(n);
        match test {
            NodeTest::Node => true,
            NodeTest::Text => kind == Kind::Text,
            NodeTest::Comment => kind == Kind::Comment,
            NodeTest::Pi(target) => {
                kind == Kind::Pi && target.as_ref().is_none_or(|t| t == self.t.pi_target(n))
            }
            NodeTest::Any | NodeTest::AnyIn(_) | NodeTest::Name(..) => {
                let principal = if axis == Axis::Attribute {
                    Kind::Attr
                } else {
                    Kind::Element
                };
                if kind != principal {
                    return false;
                }
                let (ns, local) = self.t.name(n);
                match test {
                    NodeTest::Any => true,
                    NodeTest::AnyIn(uri) => ns == Some(uri.as_str()),
                    NodeTest::Name(uri, l) => ns == uri.as_deref() && local == l,
                    _ => unreachable!(),
                }
            }
        }
    }

    fn string_value(&self, n: T::N) -> String {
        match self.t.kind(n) {
            Kind::Root | Kind::Element => {
                let mut s = String::new();
                self.collect_text(n, &mut s);
                s
            }
            _ => self.t.value(n).to_owned(),
        }
    }

    /// Append the text descendants of `n`, in document order.
    fn collect_text(&self, n: T::N, s: &mut String) {
        let mut desc = Vec::new();
        self.descendants(n, &mut desc);
        for k in desc {
            if self.t.kind(k) == Kind::Text {
                s.push_str(self.t.value(k));
            }
        }
    }

    /// `f(string-value(n))`, without allocating for attribute and text nodes.
    fn with_string_value<R>(&self, n: T::N, f: impl FnOnce(&str) -> R) -> R {
        match self.t.kind(n) {
            Kind::Root | Kind::Element => f(&self.string_value(n)),
            _ => f(self.t.value(n)),
        }
    }

    fn step(&mut self, input: Vec<T::N>, step: &Step) -> Res<Vec<T::N>> {
        let multi = input.len() > 1;
        let mut out = Vec::new();
        for n in input {
            let mut nodes = self.axis(step.axis, n)?;
            nodes.retain(|&x| self.matches(step.axis, &step.test, x));
            for pred in &step.preds {
                nodes = self.filter(nodes, pred)?;
            }
            if step.axis.reverse() {
                nodes.reverse();
            }
            out.extend(nodes);
        }
        if multi || matches!(step.axis, Axis::Following | Axis::Preceding) {
            out = self.sort_dedup(out);
        }
        Ok(out)
    }

    /// Filter a node list (in axis order) by a predicate.
    fn filter(&mut self, nodes: Vec<T::N>, pred: &Expr) -> Res<Vec<T::N>> {
        let size = nodes.len();
        let mut out = Vec::new();
        for (i, n) in nodes.into_iter().enumerate() {
            let ctx = Ctx {
                node: n,
                pos: i + 1,
                size,
            };
            let keep = match self.eval(pred, &ctx)? {
                Val::Num(x) => x == (i + 1) as f64,
                v => Self::boolean(&v),
            };
            if keep {
                out.push(n);
            }
        }
        Ok(out)
    }

    fn steps(&mut self, mut nodes: Vec<T::N>, steps: &[Step]) -> Res<Vec<T::N>> {
        for s in steps {
            nodes = self.step(nodes, s)?;
        }
        Ok(nodes)
    }

    fn eval(&mut self, e: &Expr, ctx: &Ctx<T::N>) -> Res<Val<T::N>> {
        Ok(match e {
            Expr::Lit(s) => Val::Str(s.clone()),
            Expr::Num(n) => Val::Num(*n),
            Expr::Neg(x) => {
                let v = self.eval(x, ctx)?;
                Val::Num(-self.number(&v))
            }
            Expr::Path { absolute, steps } => {
                let start = if *absolute {
                    self.t.document(ctx.node)
                } else {
                    ctx.node
                };
                Val::Nodes(self.steps(vec![start], steps)?)
            }
            Expr::Filter {
                primary,
                preds,
                steps,
            } => {
                let Val::Nodes(mut nodes) = self.eval(primary, ctx)? else {
                    return err("predicate or path applied to a non node-set");
                };
                for p in preds {
                    nodes = self.filter(nodes, p)?;
                }
                Val::Nodes(self.steps(nodes, steps)?)
            }
            Expr::Union(a, b) => {
                let (Val::Nodes(mut x), Val::Nodes(y)) = (self.eval(a, ctx)?, self.eval(b, ctx)?)
                else {
                    return err("union of non node-sets");
                };
                x.extend(y);
                Val::Nodes(self.sort_dedup(x))
            }
            Expr::Bin(Op::Or, a, b) => {
                let l = self.eval(a, ctx)?;
                Val::Bool(
                    Self::boolean(&l) || {
                        let r = self.eval(b, ctx)?;
                        Self::boolean(&r)
                    },
                )
            }
            Expr::Bin(Op::And, a, b) => {
                let l = self.eval(a, ctx)?;
                Val::Bool(
                    Self::boolean(&l) && {
                        let r = self.eval(b, ctx)?;
                        Self::boolean(&r)
                    },
                )
            }
            Expr::Bin(op @ (Op::Eq | Op::Neq | Op::Lt | Op::Le | Op::Gt | Op::Ge), a, b)
                if matches!(**b, Expr::Lit(_)) || matches!(**a, Expr::Lit(_)) =>
            {
                // `@x = 'lit'` (the usual predicate): compare without copies.
                let (lit, other, swapped) = match (&**a, &**b) {
                    (_, Expr::Lit(s)) => (s, a, false),
                    (Expr::Lit(s), _) => (s, b, true),
                    _ => unreachable!(),
                };
                let v = self.eval(other, ctx)?;
                Val::Bool(self.compare_lit(*op, &v, lit, swapped))
            }
            Expr::Bin(op @ (Op::Eq | Op::Neq | Op::Lt | Op::Le | Op::Gt | Op::Ge), a, b) => {
                let l = self.eval(a, ctx)?;
                let r = self.eval(b, ctx)?;
                Val::Bool(self.compare(*op, &l, &r))
            }
            Expr::Bin(op, a, b) => {
                let l = self.eval(a, ctx)?;
                let r = self.eval(b, ctx)?;
                let (x, y) = (self.number(&l), self.number(&r));
                Val::Num(match op {
                    Op::Add => x + y,
                    Op::Sub => x - y,
                    Op::Mul => x * y,
                    Op::Div => x / y,
                    Op::Mod => x % y,
                    _ => unreachable!(),
                })
            }
            Expr::Call(name, args) => self.call(name, args, ctx)?,
        })
    }

    fn compare(&self, op: Op, l: &Val<T::N>, r: &Val<T::N>) -> bool {
        let eqop = matches!(op, Op::Eq | Op::Neq);
        match (l, r) {
            (Val::Nodes(a), Val::Nodes(b)) => {
                let bs: Vec<String> = b.iter().map(|&n| self.string_value(n)).collect();
                a.iter().any(|&n| {
                    let s = self.string_value(n);
                    bs.iter().any(|t| {
                        if eqop {
                            cmp_eq(op, s == *t)
                        } else {
                            cmp_num(op, str_to_num(&s), str_to_num(t))
                        }
                    })
                })
            }
            (Val::Nodes(a), other) | (other, Val::Nodes(a)) => {
                let swapped = !matches!(l, Val::Nodes(_));
                if let Val::Bool(b) = other {
                    let nb = Val::Bool(!a.is_empty());
                    let b = Val::Bool(*b);
                    return if swapped {
                        self.compare(op, &b, &nb)
                    } else {
                        self.compare(op, &nb, &b)
                    };
                }
                a.iter().any(|&n| {
                    let s = self.string_value(n);
                    let nv = match other {
                        Val::Num(_) => Val::Num(str_to_num(&s)),
                        _ => Val::Str(s),
                    };
                    if swapped {
                        self.compare(op, other, &nv)
                    } else {
                        self.compare(op, &nv, other)
                    }
                })
            }
            _ if eqop => {
                let eq = match (l, r) {
                    (Val::Bool(_), _) | (_, Val::Bool(_)) => Self::boolean(l) == Self::boolean(r),
                    (Val::Num(_), _) | (_, Val::Num(_)) => self.number(l) == self.number(r),
                    _ => self.string(l) == self.string(r),
                };
                cmp_eq(op, eq)
            }
            _ => cmp_num(op, self.number(l), self.number(r)),
        }
    }

    /// `v op lit` (or `lit op v` when `swapped`), like [`Eval::compare`] with
    /// a string operand.
    fn compare_lit(&self, op: Op, v: &Val<T::N>, lit: &str, swapped: bool) -> bool {
        let Val::Nodes(nodes) = v else {
            let lit = Val::Str(lit.to_owned());
            return if swapped {
                self.compare(op, &lit, v)
            } else {
                self.compare(op, v, &lit)
            };
        };
        if matches!(op, Op::Eq | Op::Neq) {
            return nodes
                .iter()
                .any(|&n| self.with_string_value(n, |s| cmp_eq(op, s == lit)));
        }
        let y = str_to_num(lit);
        nodes.iter().any(|&n| {
            let x = self.with_string_value(n, str_to_num);
            if swapped {
                cmp_num(op, y, x)
            } else {
                cmp_num(op, x, y)
            }
        })
    }

    fn boolean(v: &Val<T::N>) -> bool {
        match v {
            Val::Nodes(n) => !n.is_empty(),
            Val::Str(s) => !s.is_empty(),
            Val::Num(x) => *x != 0.0 && !x.is_nan(),
            Val::Bool(b) => *b,
        }
    }

    fn number(&self, v: &Val<T::N>) -> f64 {
        match v {
            Val::Num(x) => *x,
            Val::Bool(b) => f64::from(u8::from(*b)),
            _ => str_to_num(&self.string(v)),
        }
    }

    fn string(&self, v: &Val<T::N>) -> String {
        match v {
            Val::Nodes(n) => n.first().map(|&x| self.string_value(x)).unwrap_or_default(),
            Val::Str(s) => s.clone(),
            Val::Num(x) => num_to_str(*x),
            Val::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        }
    }

    fn call(&mut self, name: &str, args: &[Expr], ctx: &Ctx<T::N>) -> Res<Val<T::N>> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.eval(a, ctx)?);
        }
        let argc = |min: usize, max: usize| -> Res<()> {
            if vals.len() < min || vals.len() > max {
                err(format!("invalid number of arguments for {name}()"))
            } else {
                Ok(())
            }
        };
        // Argument or context node string value.
        let str_arg = |this: &Self, i: usize| -> String {
            match vals.get(i) {
                Some(v) => this.string(v),
                None => this.string_value(ctx.node),
            }
        };
        Ok(match name {
            "last" => {
                argc(0, 0)?;
                Val::Num(ctx.size as f64)
            }
            "position" => {
                argc(0, 0)?;
                Val::Num(ctx.pos as f64)
            }
            "count" => {
                argc(1, 1)?;
                match &vals[0] {
                    Val::Nodes(n) => Val::Num(n.len() as f64),
                    _ => return err("count() expects a node-set"),
                }
            }
            "local-name" | "name" | "namespace-uri" => {
                argc(0, 1)?;
                let n = match vals.first() {
                    Some(Val::Nodes(n)) => n.first().copied(),
                    Some(_) => return err(format!("{name}() expects a node-set")),
                    None => Some(ctx.node),
                };
                let n =
                    n.filter(|&n| matches!(self.t.kind(n), Kind::Element | Kind::Attr | Kind::Pi));
                Val::Str(match n {
                    None => String::new(),
                    Some(n) if self.t.kind(n) == Kind::Pi => self.t.pi_target(n).to_owned(),
                    Some(n) => match name {
                        "local-name" => self.t.name(n).1.to_owned(),
                        "namespace-uri" => self.t.name(n).0.unwrap_or("").to_owned(),
                        _ => self.t.qname(n),
                    },
                })
            }
            "string" => {
                argc(0, 1)?;
                Val::Str(str_arg(self, 0))
            }
            "concat" => {
                if vals.len() < 2 {
                    return err("concat() needs at least 2 arguments");
                }
                Val::Str(vals.iter().map(|v| self.string(v)).collect())
            }
            "starts-with" => {
                argc(2, 2)?;
                Val::Bool(self.string(&vals[0]).starts_with(&self.string(&vals[1])))
            }
            "contains" => {
                argc(2, 2)?;
                Val::Bool(self.string(&vals[0]).contains(&self.string(&vals[1])))
            }
            "substring-before" => {
                argc(2, 2)?;
                let (s, t) = (self.string(&vals[0]), self.string(&vals[1]));
                Val::Str(s.find(&t).map(|i| s[..i].to_owned()).unwrap_or_default())
            }
            "substring-after" => {
                argc(2, 2)?;
                let (s, t) = (self.string(&vals[0]), self.string(&vals[1]));
                Val::Str(
                    s.find(&t)
                        .map(|i| s[i + t.len()..].to_owned())
                        .unwrap_or_default(),
                )
            }
            "substring" => {
                argc(2, 3)?;
                let s: Vec<char> = self.string(&vals[0]).chars().collect();
                let start = xpath_round(self.number(&vals[1]));
                let end = match vals.get(2) {
                    Some(v) => start + xpath_round(self.number(v)),
                    None => f64::INFINITY,
                };
                Val::Str(
                    s.iter()
                        .enumerate()
                        .filter(|(i, _)| {
                            let p = (*i + 1) as f64;
                            p >= start && p < end
                        })
                        .map(|(_, c)| c)
                        .collect(),
                )
            }
            "string-length" => {
                argc(0, 1)?;
                Val::Num(str_arg(self, 0).chars().count() as f64)
            }
            "normalize-space" => {
                argc(0, 1)?;
                Val::Str(
                    str_arg(self, 0)
                        .split([' ', '\t', '\n', '\r'])
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            }
            "translate" => {
                argc(3, 3)?;
                let from: Vec<char> = self.string(&vals[1]).chars().collect();
                let to: Vec<char> = self.string(&vals[2]).chars().collect();
                Val::Str(
                    self.string(&vals[0])
                        .chars()
                        .filter_map(|c| match from.iter().position(|&f| f == c) {
                            Some(i) => to.get(i).copied(),
                            None => Some(c),
                        })
                        .collect(),
                )
            }
            "boolean" => {
                argc(1, 1)?;
                Val::Bool(Self::boolean(&vals[0]))
            }
            "not" => {
                argc(1, 1)?;
                Val::Bool(!Self::boolean(&vals[0]))
            }
            "true" => {
                argc(0, 0)?;
                Val::Bool(true)
            }
            "false" => {
                argc(0, 0)?;
                Val::Bool(false)
            }
            "number" => {
                argc(0, 1)?;
                Val::Num(match vals.first() {
                    Some(v) => self.number(v),
                    None => str_to_num(&self.string_value(ctx.node)),
                })
            }
            "sum" => {
                argc(1, 1)?;
                match &vals[0] {
                    Val::Nodes(n) => {
                        Val::Num(n.iter().map(|&x| str_to_num(&self.string_value(x))).sum())
                    }
                    _ => return err("sum() expects a node-set"),
                }
            }
            "floor" => {
                argc(1, 1)?;
                Val::Num(self.number(&vals[0]).floor())
            }
            "ceiling" => {
                argc(1, 1)?;
                Val::Num(self.number(&vals[0]).ceil())
            }
            "round" => {
                argc(1, 1)?;
                Val::Num(xpath_round(self.number(&vals[0])))
            }
            "hasclass" => {
                if self.t.kind(ctx.node) != Kind::Element {
                    return Ok(Val::Bool(false));
                }
                let class = self.t.class(ctx.node).unwrap_or("");
                match self.dialect {
                    // Odoo: set(node.attrib.get('class', '').split()).issuperset(cls)
                    Dialect::Lxml => {
                        let classes: Vec<&str> =
                            class.split(py_is_space).filter(|s| !s.is_empty()).collect();
                        Val::Bool(
                            vals.iter()
                                .all(|v| classes.contains(&self.string(v).as_str())),
                        )
                    }
                    // contains(concat(' ', @class, ' '), ' c ') and ...
                    Dialect::Browser => {
                        let padded = format!(" {class} ");
                        Val::Bool(
                            vals.iter()
                                .all(|v| padded.contains(&format!(" {} ", self.string(v)))),
                        )
                    }
                }
            }
            _ => return err(format!("Unregistered function {name}")),
        })
    }
}

fn cmp_eq(op: Op, eq: bool) -> bool {
    if op == Op::Eq { eq } else { !eq }
}

fn cmp_num(op: Op, a: f64, b: f64) -> bool {
    match op {
        Op::Lt => a < b,
        Op::Le => a <= b,
        Op::Gt => a > b,
        Op::Ge => a >= b,
        Op::Eq => a == b,
        Op::Neq => a != b,
        _ => false,
    }
}

/// Python `str.isspace()` for one character.
pub fn py_is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn str_to_num(s: &str) -> f64 {
    let t = s.trim_matches([' ', '\t', '\n', '\r']);
    let body = t.strip_prefix('-').unwrap_or(t);
    let valid = !body.is_empty()
        && body.chars().all(|c| c.is_ascii_digit() || c == '.')
        && body.matches('.').count() <= 1
        && body != ".";
    if valid {
        t.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

fn num_to_str(x: f64) -> String {
    if x.is_nan() {
        "NaN".into()
    } else if x.is_infinite() {
        if x > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if x == x.trunc() && x.abs() < 1e18 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

fn xpath_round(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        x
    } else {
        (x + 0.5).floor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(xml: &str, xp: &str) -> Vec<String> {
        let d = Document::parse(xml).unwrap();
        let r = XPath::parse(xp).unwrap().select(&d, d.root).unwrap();
        r.into_iter()
            .map(|n| match n {
                XNode::Node(id) => {
                    let n = d.node(id);
                    match n.get("id") {
                        Some(i) => format!("{}#{i}", n.tag),
                        None => n.tag.to_string(),
                    }
                }
                XNode::Text(id) => format!("text:{}", d.node(id).text.as_deref().unwrap()),
                XNode::Tail(id) => format!("tail:{}", d.node(id).tail.as_deref().unwrap()),
                XNode::Attr(id, i) => format!("@{}", d.node(id).attrs[i as usize].1),
                XNode::Root(_) => "/".into(),
            })
            .collect()
    }

    const DOC: &str = r#"<t t-name="x"><div id="1" class="a b"><span id="2">hi</span>tail<p id="3"/></div><div id="4" class="b"><span id="5"/></div></t>"#;

    #[test]
    fn basics() {
        assert_eq!(sel(DOC, "//div"), ["div#1", "div#4"]);
        assert_eq!(sel(DOC, "//div[hasclass('a')]"), ["div#1"]);
        assert_eq!(sel(DOC, "//div[hasclass('b', 'a')]"), ["div#1"]);
        assert_eq!(sel(DOC, "//*[@id='5']/.."), ["div#4"]);
        assert_eq!(sel(DOC, "//span[last()]"), ["span#2", "span#5"]);
        assert_eq!(sel(DOC, "(//span)[last()]"), ["span#5"]);
        assert_eq!(sel(DOC, "//div[1]/*"), ["span#2", "p#3"]);
        assert_eq!(sel(DOC, "."), ["t"]);
        assert_eq!(sel(DOC, "/t/div[2]"), ["div#4"]);
        assert_eq!(sel(DOC, "//div[@id='1']/text()"), ["tail:tail"]);
        assert_eq!(sel(DOC, "//p | //span"), ["span#2", "p#3", "span#5"]);
        assert_eq!(sel(DOC, "//p/preceding-sibling::*[1]"), ["span#2"]);
        assert_eq!(sel(DOC, "//span[@id='5']/ancestor::*"), ["t", "div#4"]);
        assert_eq!(sel(DOC, "//span[@id='5']/ancestor::*[1]"), ["div#4"]);
        assert_eq!(sel(DOC, "//span[.='hi']"), ["span#2"]);
        assert_eq!(
            sel(DOC, "//div[contains(@class, 'a') and not(@id=4)]"),
            ["div#1"]
        );
        assert_eq!(sel(DOC, "//*[local-name()='p']"), ["p#3"]);
        assert_eq!(sel(DOC, "//div[count(*) = 1]"), ["div#4"]);
        assert_eq!(
            sel(DOC, "//span[@id='2']/following::*"),
            ["p#3", "div#4", "span#5"]
        );
        assert_eq!(sel(DOC, "//span[@id > 3]"), ["span#5"]);
        assert_eq!(sel(DOC, "//div/@class"), ["@a b", "@b"]);
    }

    /// `//x[p]` is evaluated as `descendant::x[p]` only when `p` does not
    /// depend on the position.
    #[test]
    fn descendant_shortcut() {
        const D: &str = r#"<t><a id="1"><b id="2"/><b id="3"/></a><a id="4"><b id="5"/></a></t>"#;
        assert_eq!(sel(D, "//b[1]"), ["b#2", "b#5"]);
        assert_eq!(sel(D, "//b[last()]"), ["b#3", "b#5"]);
        assert_eq!(sel(D, "//b[position() = 2]"), ["b#3"]);
        assert_eq!(sel(D, "//b[count(../b) - 1]"), ["b#2"]);
        assert_eq!(sel(D, "(//b)[2]"), ["b#3"]);
        assert_eq!(sel(D, "//b[@id = '3' or @id = 5]"), ["b#3", "b#5"]);
        assert_eq!(sel(D, "//a[b[2]]"), ["a#1"]);
        assert_eq!(sel(D, "//a//b"), ["b#2", "b#3", "b#5"]);
        assert_eq!(sel(D, "//a[1]//b[1]"), ["b#2"]);
        assert_eq!(sel(D, "//*[@id > '3']"), ["a#4", "b#5"]);
        assert_eq!(sel(D, "//*['3' > @id]"), ["a#1", "b#2"]);
        assert_eq!(sel(D, "//*['3' = @id]"), ["b#3"]);
        assert_eq!(sel(D, "//a[. = '']"), ["a#1", "a#4"]);
        assert_eq!(sel(D, "//b/.."), ["a#1", "a#4"]);
    }

    #[test]
    fn errors() {
        assert!(XPath::parse("//div[").is_err());
        assert!(XPath::parse("//x:div").is_err());
        assert!(XPath::parse("").is_err());
        assert!(XPath::parse("//div[@a=$v]").is_err());
    }
}
