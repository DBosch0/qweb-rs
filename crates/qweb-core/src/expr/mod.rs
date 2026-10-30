//! Python expression dialect: lexer, AST and parser.
//!
//! The parser handles the subset of Python expressions that can be evaluated
//! natively by any host. Anything else (lambda, comprehensions, f-strings,
//! starred arguments, ...) is reported as unsupported, and hosts
//! able to evaluate Python source (the Python binding) compile it themselves.
//!
//! Name resolution follows `IrQweb._compile_expr_tokens`:
//! * names in [`BUILTINS`] resolve to builtins, never to template values;
//! * a name directly followed by `.`, `(`, `[` or `{` is a *strict* lookup
//!   (`values['x']`, raising `KeyError`), other names are lenient
//!   (`values.get('x')`), unless `raise_on_missing` makes every name strict;
//! * any identifier containing `__` is rejected.
//!
//! The AST is crate-private: the compiler lowers it into [`crate::ir::Ex`],
//! resolving names to host keys and constants to host values.

mod lexer;

use core::fmt;

use lexer::{LexError, Tok};

/// Names never looked up in the template values (`ALLOWED_KEYWORD` minus the
/// keywords): the keys of `odoo.tools.safe_eval._BUILTINS`.
///
/// They compile to [`Host::builtin`](crate::Host::builtin) constants; a name
/// the host does not provide raises `NameError` when evaluated.
pub const BUILTINS: [&str; 32] = [
    "bytes",
    "str",
    "unicode",
    "bool",
    "int",
    "float",
    "enumerate",
    "dict",
    "list",
    "tuple",
    "map",
    "abs",
    "min",
    "max",
    "sum",
    "reduce",
    "filter",
    "sorted",
    "round",
    "len",
    "repr",
    "set",
    "all",
    "any",
    "ord",
    "chr",
    "divmod",
    "isinstance",
    "range",
    "xrange",
    "zip",
    "Exception",
];

const KEYWORDS: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

#[inline]
fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

/// Unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-x`
    Neg,
    /// `+x`
    Pos,
    /// `~x`
    Invert,
}

/// Binary arithmetic or bitwise operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `a + b`
    Add,
    /// `a - b`
    Sub,
    /// `a * b`
    Mul,
    /// `a @ b`
    MatMul,
    /// `a / b`
    Div,
    /// `a // b`
    FloorDiv,
    /// `a % b`
    Mod,
    /// `a ** b`
    Pow,
    /// `a << b`
    LShift,
    /// `a >> b`
    RShift,
    /// `a & b`
    BitAnd,
    /// `a | b`
    BitOr,
    /// `a ^ b`
    BitXor,
}

/// Comparison operator. Only the first six reach [`Host::compare`](crate::Host::compare);
/// membership and identity use [`Host::contains`](crate::Host::contains) and
/// [`Host::is`](crate::Host::is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// `a < b`
    Lt,
    /// `a <= b`
    Le,
    /// `a == b`
    Eq,
    /// `a != b`
    Ne,
    /// `a > b`
    Gt,
    /// `a >= b`
    Ge,
    /// `a in b`
    In,
    /// `a not in b`
    NotIn,
    /// `a is b`
    Is,
    /// `a is not b`
    IsNot,
}

/// Expression AST, as produced by [`parse`].
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(crate) enum Expr {
    /// `None`
    None,
    /// `True` / `False`
    Bool(bool),
    /// Integer literal that fits in an `i64`.
    Int(i64),
    /// Larger decimal integer literal (its digits).
    BigInt(String),
    /// Float literal.
    Float(f64),
    /// String literal (adjacent literals concatenated).
    Str(String),
    /// Bytes literal (adjacent literals concatenated).
    Bytes(Vec<u8>),
    /// Template value lookup. `strict`: `values['x']` (raises `KeyError`),
    /// otherwise `values.get('x')`.
    Name {
        /// Variable name.
        name: String,
        /// Whether a missing name is an error.
        strict: bool,
    },
    /// A name of [`BUILTINS`].
    Builtin(String),
    /// `obj.name`
    Attr(Box<Expr>, String),
    /// `obj[key]`
    Item(Box<Expr>, Box<Expr>),
    /// `lower:upper:step` inside a subscript.
    Slice(Option<Box<Expr>>, Option<Box<Expr>>, Option<Box<Expr>>),
    /// `func(*args, **kwargs)` with plain (non-starred) arguments.
    Call {
        /// Called expression.
        func: Box<Expr>,
        /// Positional arguments.
        args: Vec<Expr>,
        /// Keyword arguments, in source order.
        kwargs: Vec<(String, Expr)>,
    },
    /// Unary arithmetic operator.
    Unary(UnOp, Box<Expr>),
    /// `not x`
    Not(Box<Expr>),
    /// Binary arithmetic operator.
    Bin(BinOp, Box<Expr>, Box<Expr>),
    /// Comparison chain `first op1 e1 op2 e2 ...` (never empty).
    Cmp(Box<Expr>, Vec<(CmpOp, Expr)>),
    /// `a and b and ...` (at least two operands).
    And(Vec<Expr>),
    /// `a or b or ...` (at least two operands).
    Or(Vec<Expr>),
    /// `then if cond else orelse`
    IfElse {
        /// Condition.
        cond: Box<Expr>,
        /// Value when `cond` is true.
        then: Box<Expr>,
        /// Value otherwise.
        orelse: Box<Expr>,
    },
    /// Tuple display (also the empty expression).
    Tuple(Vec<Expr>),
    /// List display.
    List(Vec<Expr>),
    /// Set display.
    Set(Vec<Expr>),
    /// Dict display.
    Dict(Vec<(Expr, Expr)>),
}

/// Error from [`parse`].
#[derive(Debug)]
pub(crate) enum ParseError {
    /// Valid Python not handled natively.
    Unsupported(String),
    /// Invalid syntax (as far as the native parser can tell).
    Syntax(String),
    /// Use of a name containing `__`.
    Dunder(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Unsupported(m) => write!(f, "unsupported expression: {m}"),
            ParseError::Syntax(m) => write!(f, "invalid syntax: {m}"),
            ParseError::Dunder(n) => {
                write!(f, "Using variable names with '__' is not allowed: {n:?}")
            }
        }
    }
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        match e {
            LexError::Unsupported(m) => ParseError::Unsupported(m),
            LexError::Syntax(m) => ParseError::Syntax(m),
            LexError::Dunder(n) => ParseError::Dunder(n),
        }
    }
}

/// Parse an expression. `raise_on_missing` makes every name lookup strict.
/// An empty expression is the empty tuple.
pub(crate) fn parse(src: &str, raise_on_missing: bool) -> Result<Expr, ParseError> {
    let toks = lexer::tokenize(src)?;
    debug_assert!(!toks.is_empty(), "end token does not exist");
    debug_assert!(toks.last().unwrap() == &Tok::End, "last token is end");

    let mut p = Parser {
        toks,
        pos: 0,
        strict_all: raise_on_missing,
    };
    if p.peek() == &Tok::End {
        return Ok(Expr::Tuple(Vec::new()));
    }
    let e = p.expr_list()?;
    if p.peek() != &Tok::End {
        return Err(ParseError::Syntax(format!(
            "unexpected token {:?}",
            p.peek()
        )));
    }
    Ok(e)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    strict_all: bool,
}

type PResult = Result<Expr, ParseError>;

impl Parser {
    fn peek(&self) -> &Tok {
        debug_assert!(
            self.pos < self.toks.len(),
            "position outside of token buffer"
        );
        &self.toks[self.pos]
    }

    fn peek_at(&self, n: usize) -> &Tok {
        self.toks.get(self.pos + n).unwrap_or(&Tok::End)
    }

    /// Consume the current token. It is moved out of the buffer (the parser
    /// only ever looks ahead, never back); the final `End` stays in place.
    fn next(&mut self) -> Tok {
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
            std::mem::replace(&mut self.toks[self.pos - 1], Tok::End)
        } else {
            Tok::End
        }
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Name(n) if n == kw)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.next();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.next();
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<(), ParseError> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(ParseError::Syntax(format!(
                "expected '{op}', got {:?}",
                self.peek()
            )))
        }
    }

    fn check_comprehension(&self) -> Result<(), ParseError> {
        if self.is_kw("for") || self.is_kw("async") {
            return Err(ParseError::Unsupported("comprehension".into()));
        }
        Ok(())
    }

    fn check_star(&self) -> Result<(), ParseError> {
        if self.is_op("*") || self.is_op("**") {
            return Err(ParseError::Unsupported("starred expression".into()));
        }
        Ok(())
    }

    /// `a, b, c` (tuple when there is a comma).
    fn expr_list(&mut self) -> PResult {
        self.check_star()?;
        let first = self.test()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_op(",") {
            if self.at_list_end() {
                break;
            }
            self.check_star()?;
            items.push(self.test()?);
        }
        Ok(Expr::Tuple(items))
    }

    fn at_list_end(&self) -> bool {
        matches!(self.peek(), Tok::End) || self.is_op(")") || self.is_op("]") || self.is_op("}")
    }

    fn test(&mut self) -> PResult {
        if self.is_kw("lambda") {
            return Err(ParseError::Unsupported("lambda".into()));
        }
        let e = self.or_test()?;
        if self.is_op(":=") {
            return Err(ParseError::Unsupported("assignment expression".into()));
        }
        if self.eat_kw("if") {
            let cond = self.or_test()?;
            if !self.eat_kw("else") {
                return Err(ParseError::Syntax(
                    "expected 'else' after 'if' expression".into(),
                ));
            }
            let orelse = self.test()?;
            return Ok(Expr::IfElse {
                cond: Box::new(cond),
                then: Box::new(e),
                orelse: Box::new(orelse),
            });
        }
        Ok(e)
    }

    fn or_test(&mut self) -> PResult {
        let first = self.and_test()?;
        if !self.is_kw("or") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_kw("or") {
            items.push(self.and_test()?);
        }
        Ok(Expr::Or(items))
    }

    fn and_test(&mut self) -> PResult {
        let first = self.not_test()?;
        if !self.is_kw("and") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_kw("and") {
            items.push(self.not_test()?);
        }
        Ok(Expr::And(items))
    }

    fn not_test(&mut self) -> PResult {
        if self.eat_kw("not") {
            return Ok(Expr::Not(Box::new(self.not_test()?)));
        }
        self.comparison()
    }

    fn comp_op(&mut self) -> Option<CmpOp> {
        let op = match self.peek() {
            Tok::Op("<") => CmpOp::Lt,
            Tok::Op("<=") => CmpOp::Le,
            Tok::Op("==") => CmpOp::Eq,
            Tok::Op("!=") => CmpOp::Ne,
            Tok::Op(">") => CmpOp::Gt,
            Tok::Op(">=") => CmpOp::Ge,
            Tok::Name(n) if n == "in" => CmpOp::In,
            Tok::Name(n) if n == "is" => {
                if matches!(self.peek_at(1), Tok::Name(m) if m == "not") {
                    self.next();
                    self.next();
                    return Some(CmpOp::IsNot);
                }
                CmpOp::Is
            }
            Tok::Name(n) if n == "not" => {
                if matches!(self.peek_at(1), Tok::Name(m) if m == "in") {
                    self.next();
                    self.next();
                    return Some(CmpOp::NotIn);
                }
                return None;
            }
            _ => return None,
        };
        self.next();
        Some(op)
    }

    fn comparison(&mut self) -> PResult {
        let first = self.bitor()?;
        let mut rest = Vec::new();
        while let Some(op) = self.comp_op() {
            rest.push((op, self.bitor()?));
        }
        if rest.is_empty() {
            Ok(first)
        } else {
            Ok(Expr::Cmp(Box::new(first), rest))
        }
    }

    fn binary_level(&mut self, level: usize) -> PResult {
        const LEVELS: &[&[(&str, BinOp)]] = &[
            &[("|", BinOp::BitOr)],
            &[("^", BinOp::BitXor)],
            &[("&", BinOp::BitAnd)],
            &[("<<", BinOp::LShift), (">>", BinOp::RShift)],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[
                ("*", BinOp::Mul),
                ("/", BinOp::Div),
                ("//", BinOp::FloorDiv),
                ("%", BinOp::Mod),
                ("@", BinOp::MatMul),
            ],
        ];
        if level == LEVELS.len() {
            return self.factor();
        }
        let mut left = self.binary_level(level + 1)?;
        'outer: loop {
            for (op, bop) in LEVELS[level] {
                if self.is_op(op) {
                    self.next();
                    let right = self.binary_level(level + 1)?;
                    left = Expr::Bin(*bop, Box::new(left), Box::new(right));
                    continue 'outer;
                }
            }
            return Ok(left);
        }
    }

    fn bitor(&mut self) -> PResult {
        self.binary_level(0)
    }

    fn factor(&mut self) -> PResult {
        let op = match self.peek() {
            Tok::Op("-") => UnOp::Neg,
            Tok::Op("+") => UnOp::Pos,
            Tok::Op("~") => UnOp::Invert,
            _ => return self.power(),
        };
        self.next();
        let operand = self.factor()?;
        Ok(Expr::Unary(op, Box::new(operand)))
    }

    fn power(&mut self) -> PResult {
        if self.is_kw("await") {
            return Err(ParseError::Unsupported("await".into()));
        }
        let base = self.primary()?;
        if self.eat_op("**") {
            let exp = self.factor()?;
            return Ok(Expr::Bin(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn primary(&mut self) -> PResult {
        let mut e = self.atom()?;
        loop {
            if self.eat_op(".") {
                match self.next() {
                    Tok::Name(n) if !KEYWORDS.contains(&n.as_str()) => {
                        e = Expr::Attr(Box::new(e), n)
                    }
                    t => return Err(ParseError::Syntax(format!("invalid attribute name {t:?}"))),
                }
            } else if self.eat_op("(") {
                e = self.call_args(e)?;
            } else if self.eat_op("[") {
                let sub = self.subscript_list()?;
                self.expect_op("]")?;
                e = Expr::Item(Box::new(e), Box::new(sub));
            } else {
                return Ok(e);
            }
        }
    }

    fn call_args(&mut self, func: Expr) -> PResult {
        let mut args = Vec::new();
        let mut kwargs: Vec<(String, Expr)> = Vec::new();
        loop {
            if self.eat_op(")") {
                break;
            }
            self.check_star()?;
            let is_kw =
                matches!(self.peek(), Tok::Name(_)) && matches!(self.peek_at(1), Tok::Op("="));
            if is_kw {
                let Tok::Name(name) = self.next() else {
                    unreachable!()
                };
                if KEYWORDS.contains(&name.as_str()) {
                    return Err(ParseError::Syntax("invalid keyword argument".into()));
                }
                self.next(); // '='
                let value = self.test()?;
                if kwargs.iter().any(|(k, _)| *k == name) {
                    return Err(ParseError::Syntax(format!(
                        "keyword argument repeated: {name}"
                    )));
                }
                kwargs.push((name, value));
            } else {
                let value = self.test()?;
                self.check_comprehension()?;
                if !kwargs.is_empty() {
                    return Err(ParseError::Syntax(
                        "positional argument follows keyword argument".into(),
                    ));
                }
                args.push(value);
            }
            if !self.eat_op(",") {
                self.expect_op(")")?;
                break;
            }
        }
        Ok(Expr::Call {
            func: Box::new(func),
            args,
            kwargs,
        })
    }

    fn subscript_list(&mut self) -> PResult {
        let first = self.subscript()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_op(",") {
            if self.is_op("]") {
                break;
            }
            items.push(self.subscript()?);
        }
        Ok(Expr::Tuple(items))
    }

    fn subscript(&mut self) -> PResult {
        self.check_star()?;
        let lower = if self.is_op(":") {
            None
        } else {
            Some(Box::new(self.test()?))
        };
        if !self.eat_op(":") {
            return Ok(*lower.ok_or_else(|| ParseError::Syntax("empty subscript".into()))?);
        }
        let upper = if self.is_op(":") || self.is_op("]") || self.is_op(",") {
            None
        } else {
            Some(Box::new(self.test()?))
        };
        let step = if self.eat_op(":") {
            if self.is_op("]") || self.is_op(",") {
                None
            } else {
                Some(Box::new(self.test()?))
            }
        } else {
            None
        };
        Ok(Expr::Slice(lower, upper, step))
    }

    fn atom(&mut self) -> PResult {
        match self.next() {
            Tok::Name(n) => self.name(n),
            Tok::Int(v) => Ok(Expr::Int(v)),
            Tok::BigInt(s) => Ok(Expr::BigInt(s)),
            Tok::Float(v) => Ok(Expr::Float(v)),
            Tok::Str(s) => {
                let mut s = s;
                loop {
                    match self.peek() {
                        Tok::Str(t) => {
                            s.push_str(t);
                            self.next();
                        }
                        Tok::Bytes(_) => {
                            return Err(ParseError::Syntax(
                                "cannot mix bytes and nonbytes literals".into(),
                            ));
                        }
                        _ => break,
                    }
                }
                Ok(Expr::Str(s))
            }
            Tok::Bytes(b) => {
                let mut b = b;
                loop {
                    match self.peek() {
                        Tok::Bytes(t) => {
                            b.extend_from_slice(t);
                            self.next();
                        }
                        Tok::Str(_) => {
                            return Err(ParseError::Syntax(
                                "cannot mix bytes and nonbytes literals".into(),
                            ));
                        }
                        _ => break,
                    }
                }
                Ok(Expr::Bytes(b))
            }
            Tok::Op("(") => {
                if self.eat_op(")") {
                    return Ok(Expr::Tuple(Vec::new()));
                }
                if self.is_kw("yield") {
                    return Err(ParseError::Unsupported("yield".into()));
                }
                self.check_star()?;
                let first = self.test()?;
                self.check_comprehension()?;
                if self.eat_op(")") {
                    return Ok(first);
                }
                if !self.is_op(",") {
                    return Err(ParseError::Syntax(format!(
                        "expected ')', got {:?}",
                        self.peek()
                    )));
                }
                let mut items = vec![first];
                while self.eat_op(",") {
                    if self.is_op(")") {
                        break;
                    }
                    self.check_star()?;
                    items.push(self.test()?);
                }
                self.expect_op(")")?;
                Ok(Expr::Tuple(items))
            }
            Tok::Op("[") => {
                let mut items = Vec::new();
                if self.eat_op("]") {
                    return Ok(Expr::List(items));
                }
                self.check_star()?;
                items.push(self.test()?);
                self.check_comprehension()?;
                while self.eat_op(",") {
                    if self.is_op("]") {
                        break;
                    }
                    self.check_star()?;
                    items.push(self.test()?);
                }
                self.expect_op("]")?;
                Ok(Expr::List(items))
            }
            Tok::Op("{") => {
                if self.eat_op("}") {
                    return Ok(Expr::Dict(Vec::new()));
                }
                self.check_star()?;
                let first = self.test()?;
                if self.eat_op(":") {
                    let v = self.test()?;
                    self.check_comprehension()?;
                    let mut items = vec![(first, v)];
                    while self.eat_op(",") {
                        if self.is_op("}") {
                            break;
                        }
                        self.check_star()?;
                        let k = self.test()?;
                        self.expect_op(":")?;
                        let v = self.test()?;
                        items.push((k, v));
                    }
                    self.expect_op("}")?;
                    Ok(Expr::Dict(items))
                } else {
                    self.check_comprehension()?;
                    let mut items = vec![first];
                    while self.eat_op(",") {
                        if self.is_op("}") {
                            break;
                        }
                        self.check_star()?;
                        items.push(self.test()?);
                    }
                    self.expect_op("}")?;
                    Ok(Expr::Set(items))
                }
            }
            Tok::Op("...") => Err(ParseError::Unsupported("ellipsis".into())),
            t => Err(ParseError::Syntax(format!("unexpected token {t:?}"))),
        }
    }

    fn name(&mut self, n: String) -> PResult {
        match n.as_str() {
            "None" => return Ok(Expr::None),
            "True" => return Ok(Expr::Bool(true)),
            "False" => return Ok(Expr::Bool(false)),
            "lambda" | "yield" | "await" => return Err(ParseError::Unsupported(n)),
            _ => {}
        }
        if KEYWORDS.contains(&n.as_str()) {
            return Err(ParseError::Syntax(format!("unexpected keyword {n:?}")));
        }
        if is_builtin(&n) {
            return Ok(Expr::Builtin(n));
        }
        let strict = self.strict_all
            || self.is_op(".")
            || self.is_op("(")
            || self.is_op("[")
            || self.is_op("{");
        Ok(Expr::Name { name: n, strict })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str, strict: bool) -> Expr {
        Expr::Name {
            name: n.into(),
            strict,
        }
    }

    #[test]
    fn names() {
        assert_eq!(parse("a", false).unwrap(), name("a", false));
        assert_eq!(
            parse("a.b", false).unwrap(),
            Expr::Attr(Box::new(name("a", true)), "b".into())
        );
        assert_eq!(
            parse("(a).b", false).unwrap(),
            Expr::Attr(Box::new(name("a", false)), "b".into())
        );
        assert_eq!(
            parse("len(x)", false).unwrap(),
            Expr::Call {
                func: Box::new(Expr::Builtin("len".into())),
                args: vec![name("x", false)],
                kwargs: vec![]
            }
        );
        assert_eq!(parse("a", true).unwrap(), name("a", true));
        assert_eq!(parse("", false).unwrap(), Expr::Tuple(vec![]));
    }

    #[test]
    fn precedence() {
        let e = parse("not a == b and c or d if e else f", false).unwrap();
        assert!(matches!(e, Expr::IfElse { .. }));
        let e = parse("-2 ** 2", false).unwrap();
        assert!(matches!(e, Expr::Unary(UnOp::Neg, _)));
        let e = parse("a < b < c", false).unwrap();
        assert!(matches!(e, Expr::Cmp(_, ref v) if v.len() == 2));
        let e = parse("x not in y", false).unwrap();
        assert!(matches!(e, Expr::Cmp(_, ref v) if v[0].0 == CmpOp::NotIn));
        let e = parse("a, b", false).unwrap();
        assert!(matches!(e, Expr::Tuple(ref v) if v.len() == 2));
        let e = parse("x[1:2, ::3]", false).unwrap();
        assert!(matches!(e, Expr::Item(_, _)));
        // chained conditional expressions nest in the `else` branch
        let e = parse("a if b else c if d else e", false).unwrap();
        assert!(
            matches!(e, Expr::IfElse { ref orelse, .. } if matches!(**orelse, Expr::IfElse { .. }))
        );
        let e = parse("'a' 'b'", false).unwrap();
        assert_eq!(e, Expr::Str("ab".into()));
    }

    #[test]
    fn unsupported() {
        for src in [
            "lambda x: x",
            "[x for x in y]",
            "f'{x}'",
            "f(*a)",
            "{**a}",
            "(x := 1)",
            "1j",
        ] {
            assert!(
                matches!(parse(src, false), Err(ParseError::Unsupported(_))),
                "{src}"
            );
        }
        assert!(matches!(
            parse("a.__class__", false),
            Err(ParseError::Dunder(_))
        ));
        assert!(matches!(
            parse("x + __y", false),
            Err(ParseError::Dunder(_))
        ));
        assert!(parse("'__x' + y", false).is_ok());
        assert!(matches!(parse("a +", false), Err(ParseError::Syntax(_))));
    }
}
