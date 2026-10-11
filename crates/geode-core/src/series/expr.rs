//! The expression language: arithmetic over named series, functions
//! that fold a series to a number or map it to another series, and
//! `[k]` reading one value of a series. A hand-written recursive-descent
//! parser, pure; the resolved tree names slots only, so an identity
//! never reaches the compiler as text. A reference is a series name —
//! there is no slot handle — so an expression can name only a source
//! series, never another expression. A word followed immediately by
//! `(` is a call and must name a [`Function`]; any other word is a
//! series name, so a series called `max` still resolves.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

/// The functions the language knows. The set is closed: the tokenizer
/// refuses any other word before a `(` by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    First,
    Last,
    Min,
    Max,
    Mean,
    Median,
    Std,
    Sum,
    Count,
    Abs,
    Log,
    Exp,
    Sqrt,
    Diff,
    Pct,
    Cum,
    Lag,
    Sma,
    Ema,
    Rmin,
    Rmax,
    Rstd,
    Z,
}

/// How a function treats its arguments: what the shape check and the
/// compiler both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One series in, one number out, over the queried range.
    Fold,
    /// Bucket by bucket; a scalar in gives a scalar out.
    Pointwise,
    /// `min`/`max`: a fold with one argument, pointwise with two or more.
    MinMax,
    /// Series to series along the bucket order, with or without a count.
    Along { count: bool },
    /// Series to series over the last `n` points.
    Rolling,
}

impl Function {
    pub const ALL: [Function; 23] = [
        Function::First,
        Function::Last,
        Function::Min,
        Function::Max,
        Function::Mean,
        Function::Median,
        Function::Std,
        Function::Sum,
        Function::Count,
        Function::Abs,
        Function::Log,
        Function::Exp,
        Function::Sqrt,
        Function::Diff,
        Function::Pct,
        Function::Cum,
        Function::Lag,
        Function::Sma,
        Function::Ema,
        Function::Rmin,
        Function::Rmax,
        Function::Rstd,
        Function::Z,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Function::First => "first",
            Function::Last => "last",
            Function::Min => "min",
            Function::Max => "max",
            Function::Mean => "mean",
            Function::Median => "median",
            Function::Std => "std",
            Function::Sum => "sum",
            Function::Count => "count",
            Function::Abs => "abs",
            Function::Log => "log",
            Function::Exp => "exp",
            Function::Sqrt => "sqrt",
            Function::Diff => "diff",
            Function::Pct => "pct",
            Function::Cum => "cum",
            Function::Lag => "lag",
            Function::Sma => "sma",
            Function::Ema => "ema",
            Function::Rmin => "rmin",
            Function::Rmax => "rmax",
            Function::Rstd => "rstd",
            Function::Z => "z",
        }
    }

    pub fn parse(word: &str) -> Option<Function> {
        Function::ALL.iter().copied().find(|f| f.name() == word)
    }

    pub fn kind(self) -> Kind {
        match self {
            Function::First
            | Function::Last
            | Function::Mean
            | Function::Median
            | Function::Std
            | Function::Sum
            | Function::Count => Kind::Fold,
            Function::Min | Function::Max => Kind::MinMax,
            Function::Abs | Function::Log | Function::Exp | Function::Sqrt => Kind::Pointwise,
            Function::Diff | Function::Pct | Function::Cum => Kind::Along { count: false },
            Function::Lag => Kind::Along { count: true },
            Function::Sma
            | Function::Ema
            | Function::Rmin
            | Function::Rmax
            | Function::Rstd
            | Function::Z => Kind::Rolling,
        }
    }

    /// The call as the help line spells it: the name and its argument
    /// names, `series` for a series argument and `n` for a count. `min`
    /// and `max` show their variadic form.
    pub fn signature(self) -> &'static str {
        match self {
            Function::First => "first(series)",
            Function::Last => "last(series)",
            Function::Min => "min(series, …)",
            Function::Max => "max(series, …)",
            Function::Mean => "mean(series)",
            Function::Median => "median(series)",
            Function::Std => "std(series)",
            Function::Sum => "sum(series)",
            Function::Count => "count(series)",
            Function::Abs => "abs(x)",
            Function::Log => "log(x)",
            Function::Exp => "exp(x)",
            Function::Sqrt => "sqrt(x)",
            Function::Diff => "diff(series)",
            Function::Pct => "pct(series)",
            Function::Cum => "cum(series)",
            Function::Lag => "lag(series, n)",
            Function::Sma => "sma(series, n)",
            Function::Ema => "ema(series, n)",
            Function::Rmin => "rmin(series, n)",
            Function::Rmax => "rmax(series, n)",
            Function::Rstd => "rstd(series, n)",
            Function::Z => "z(series, n)",
        }
    }

    /// What a call evaluates to, from its kind: a fold gives `number`, a
    /// pointwise function `same as its argument`, `min`/`max` `number or
    /// series`, an along or rolling function `series`.
    pub fn result(self) -> &'static str {
        match self.kind() {
            Kind::Fold => "number",
            Kind::Pointwise => "same as its argument",
            Kind::MinMax => "number or series",
            Kind::Along { .. } | Kind::Rolling => "series",
        }
    }

    /// One sentence, under 72 characters, stating what the function
    /// computes and when it is blank, true of the lowering in
    /// `geode-data` (`query/series.rs`: `fold_sql`, `Lowering::call`,
    /// `window` and `ema`), which this copy is a claim about. A fold
    /// skips blank points, as do `cum` (a sum) and `ema` (a filtered
    /// list); every other window reads its rows as they are, so `diff`,
    /// `pct` and `lag` are blank wherever the point they read back to has
    /// no value, and a rolling value is blank unless each of the last `n`
    /// points has one, after a gap as much as at the start.
    pub fn describe(self) -> &'static str {
        match self {
            Function::First => "the first point in the range that has a value",
            Function::Last => "the last point in the range that has a value",
            Function::Min => "the smallest: over the range alone, per point with more arguments",
            Function::Max => "the largest: over the range alone, per point with more arguments",
            Function::Mean => "the mean over the range; blank with no valued points",
            Function::Median => "the median over the range, halfway between the middle two",
            Function::Std => "sample deviation over the range, blank under two points",
            Function::Sum => "the sum over the range; blank with no valued points",
            Function::Count => "how many points with a value the range holds",
            Function::Abs => "the absolute value, per point",
            Function::Log => "natural log; blank at or below zero",
            Function::Exp => "e to the power; blank above 709",
            Function::Sqrt => "square root; blank below zero",
            Function::Diff => "change from the previous point; blank at the first or after a gap",
            Function::Pct => "change over the previous point; blank when it is zero or a gap",
            Function::Cum => "running sum of the points so far",
            Function::Lag => "the value n points back; blank for the first n or if it has no value",
            Function::Sma => "mean of the last n points; blank unless all n have a value",
            Function::Ema => {
                "span-n exponential mean of 5n points; blank unless last n have a value"
            }
            Function::Rmin => "smallest of the last n points; blank unless all n have a value",
            Function::Rmax => "largest of the last n points; blank unless all n have a value",
            Function::Rstd => {
                "sample deviation of the last n points; blank unless all n have a value"
            }
            Function::Z => {
                "(value − sma) / rstd of the last n; blank unless all n valued, rstd > 0"
            }
        }
    }
}

/// The help line for a caret inside `[…]`: `index_sql` reads the point
/// that has a value at offset `k` (0-based) from the start for `k >= 0`
/// and from the end otherwise (`[-1]` is the last), NULL past either end.
pub const INDEX_HELP: &str =
    "A[k] · number · the point at offset k, 0 the first, from the end when k is negative";

/// The tree, generic over how a reference is spelled: `RefName` as
/// parsed, `u8` once resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Ast<R> {
    Ref(R),
    Num(f64),
    Neg(Box<Ast<R>>),
    Bin(Op, Box<Ast<R>>, Box<Ast<R>>),
    /// A call with its arguments as written; a count argument is a
    /// `Num` the shape check validates.
    Call(Function, Vec<Ast<R>>),
    /// `x[k]`: the k-th non-null value from the start (`k >= 0`) or
    /// from the end (`k < 0`).
    Index(Box<Ast<R>>, i64),
}

/// A reference as typed: an identity with an optional `@source`. Its
/// text is contiguous, `identity` then `@source`, so `display().len()`
/// is its byte length in the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefName {
    pub identity: String,
    pub source: Option<String>,
}

impl RefName {
    pub fn display(&self) -> String {
        match &self.source {
            None => self.identity.clone(),
            Some(s) => format!("{}@{s}", self.identity),
        }
    }
}

/// The resolved tree the compiler consumes: references are slot
/// numbers only.
pub type Expr = Ast<u8>;

/// What a node evaluates to: one value per bucket, or one number over
/// the queried range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Series,
    Scalar,
}

/// The largest count a rolling function or `lag` accepts.
pub const MAX_COUNT: u32 = 10_000;

/// The count argument of an along or rolling call: `args[1]` when it is
/// a whole-number literal in `1..=MAX_COUNT`. `None` for a call without
/// one, or one the shape check refuses.
pub fn count_arg<R>(args: &[Ast<R>]) -> Option<u32> {
    match args.get(1) {
        Some(Ast::Num(n)) if n.fract() == 0.0 && (1.0..=MAX_COUNT as f64).contains(n) => {
            Some(*n as u32)
        }
        _ => None,
    }
}

fn join(a: Shape, b: Shape) -> Shape {
    if a == Shape::Series || b == Shape::Series {
        Shape::Series
    } else {
        Shape::Scalar
    }
}

impl<R> Ast<R> {
    /// The node's shape, or the first violation in reading order as the
    /// message a trader reads: a fold, an index, an along or a rolling
    /// function of a scalar, a wrong argument count, or a count outside
    /// `1..=MAX_COUNT`. The tile runs this at Enter; the compiler runs it
    /// again and refuses the request on disagreement.
    pub fn shape(&self) -> Result<Shape, String> {
        Ok(match self {
            Ast::Ref(_) => Shape::Series,
            Ast::Num(_) => Shape::Scalar,
            Ast::Neg(x) => x.shape()?,
            Ast::Bin(_, l, r) => join(l.shape()?, r.shape()?),
            Ast::Index(x, _) => match x.shape()? {
                Shape::Series => Shape::Scalar,
                Shape::Scalar => return Err("[k] needs a series".into()),
            },
            Ast::Call(f, args) => f.check(args)?,
        })
    }
}

impl Function {
    /// The shape of a call to `self` with `args`, or the refusal.
    fn check<R>(self, args: &[Ast<R>]) -> Result<Shape, String> {
        let name = self.name();
        let one = |args: &[Ast<R>]| -> Result<Shape, String> {
            match args {
                [x] => x.shape(),
                _ => Err(format!("{name} takes one argument")),
            }
        };
        let series = |shape: Shape| -> Result<(), String> {
            match shape {
                Shape::Series => Ok(()),
                Shape::Scalar => Err(format!("{name} needs a series")),
            }
        };
        match self.kind() {
            Kind::Fold => {
                series(one(args)?)?;
                Ok(Shape::Scalar)
            }
            Kind::Pointwise => one(args),
            Kind::MinMax => match args {
                [] => Err(format!("{name} takes one or more arguments")),
                [x] => {
                    series(x.shape()?)?;
                    Ok(Shape::Scalar)
                }
                many => {
                    let mut shape = Shape::Scalar;
                    for x in many {
                        shape = join(shape, x.shape()?);
                    }
                    Ok(shape)
                }
            },
            Kind::Along { count: false } => {
                series(one(args)?)?;
                Ok(Shape::Series)
            }
            Kind::Along { count: true } | Kind::Rolling => {
                let [x, _] = args else {
                    return Err(format!("{name} takes a series and a count"));
                };
                series(x.shape()?)?;
                if count_arg(args).is_none() {
                    return Err(format!(
                        "{name}'s count must be a whole number from 1 to {MAX_COUNT}"
                    ));
                }
                Ok(Shape::Series)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset into the text where the parser stopped.
    pub position: usize,
    pub message: String,
}

/// Error text for a character the grammar has no use for.
pub const NOT_IN_GRAMMAR: &str = "only + - * /, parentheses, [k] and functions are allowed";

/// The largest `k` an index may name, either sign. Past the series it
/// reads NULL; the bound only keeps the literal sane.
pub const MAX_INDEX: i64 = 1_000_000;

/// The deepest a unary-minus/parenthesis/call/index nest may go before
/// `parse` refuses it. Bounds the recursion in `Parser::factor` (a
/// pasted wall of parentheses would otherwise overflow the stack — an
/// abort, not a panic, nothing can contain). It does NOT bound the tree
/// on its own: `expr`/`term` fold left-deep iteratively, so `1+1+…`
/// builds a tree as deep as it is long while nesting nothing.
/// `MAX_TOKENS` is what bounds the node count, and with it every
/// recursion OVER the tree — `Ast::resolve`, `Expr::collect_slots`, the
/// compiler's lowering and the `Box` drop glue. The two bounds together
/// are the guarantee; this one is kept because it gives the better
/// message for nesting.
pub const MAX_DEPTH: usize = 64;

/// The most tokens an expression may carry. The tree has at most one
/// node per token, so this bounds its depth however it is shaped — a
/// left-deep chain included, which `MAX_DEPTH` cannot see.
pub const MAX_TOKENS: usize = 256;

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Ref(RefName),
    /// A function name with `(` right behind it; the `(` is its own
    /// token.
    Call(Function),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
}

/// A character that can open a reference's identity. Public so a name
/// completer draws the same word boundaries the tokenizer does.
pub fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// A character that can continue a reference's identity (`SPX.close`).
pub fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// A character of the `@source` part after an identity. `-` is one only
/// there; before the `@` it is subtraction.
pub fn is_source_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn tokenize(text: &str) -> Result<Vec<(usize, Token)>, ParseError> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let tok = match c {
            '+' => {
                i += 1;
                Token::Plus
            }
            '-' => {
                i += 1;
                Token::Minus
            }
            '*' => {
                i += 1;
                Token::Star
            }
            '/' => {
                i += 1;
                Token::Slash
            }
            '(' => {
                i += 1;
                Token::LParen
            }
            ')' => {
                i += 1;
                Token::RParen
            }
            '[' => {
                i += 1;
                Token::LBracket
            }
            ']' => {
                i += 1;
                Token::RBracket
            }
            ',' => {
                i += 1;
                Token::Comma
            }
            c if c.is_ascii_digit() => {
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    let frac = i;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                    if i == frac {
                        return Err(ParseError {
                            position: i,
                            message: "a number needs digits after the point".into(),
                        });
                    }
                }
                let n: f64 = text[start..i].parse().map_err(|_| ParseError {
                    position: start,
                    message: "not a number".into(),
                })?;
                Token::Num(n)
            }
            c if is_ident_start(c) => {
                while i < bytes.len() && is_ident_char(bytes[i] as char) {
                    i += 1;
                }
                let word = &text[start..i];
                // A `(` right after a word makes it a call, and only a
                // known function may be called; the `(` is left for the
                // parser. Any other word, `(` or not behind a space, is a
                // series name.
                if i < bytes.len() && bytes[i] == b'(' {
                    match Function::parse(word) {
                        Some(f) => Token::Call(f),
                        None => {
                            return Err(ParseError {
                                position: start,
                                message: format!("unknown function '{word}'"),
                            });
                        }
                    }
                } else {
                    let source = if i < bytes.len() && bytes[i] == b'@' {
                        i += 1;
                        let s = i;
                        while i < bytes.len() && is_source_char(bytes[i] as char) {
                            i += 1;
                        }
                        if i == s {
                            return Err(ParseError {
                                position: i,
                                message: "a source name must follow '@'".into(),
                            });
                        }
                        Some(text[s..i].to_string())
                    } else {
                        None
                    };
                    Token::Ref(RefName {
                        identity: word.to_string(),
                        source,
                    })
                }
            }
            _ => {
                return Err(ParseError {
                    position: start,
                    message: NOT_IN_GRAMMAR.into(),
                });
            }
        };
        out.push((start, tok));
    }
    Ok(out)
}

struct Parser<'a> {
    toks: &'a [(usize, Token)],
    pos: usize,
    end: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.toks.get(self.pos).map(|(_, t)| t)
    }

    fn here(&self) -> usize {
        self.toks.get(self.pos).map(|(p, _)| *p).unwrap_or(self.end)
    }

    fn bump(&mut self) -> Option<&'a Token> {
        let t = self.peek();
        self.pos += 1;
        t
    }

    fn expr(&mut self) -> Result<Ast<RefName>, ParseError> {
        let mut lhs = self.term()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => Op::Add,
                Some(Token::Minus) => Op::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.term()?;
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn term(&mut self) -> Result<Ast<RefName>, ParseError> {
        let mut lhs = self.factor()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => Op::Mul,
                Some(Token::Slash) => Op::Div,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.factor()?;
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(rhs));
        }
    }

    /// Unary minus over a postfix chain: a primary and its `[k]`s, left
    /// to right. `-A[0]` is the negation of the index.
    fn factor(&mut self) -> Result<Ast<RefName>, ParseError> {
        let at = self.here();
        if let Some(Token::Minus) = self.peek() {
            self.bump();
            self.enter_nest(at)?;
            let inner = self.factor()?;
            self.depth -= 1;
            return Ok(Ast::Neg(Box::new(inner)));
        }
        let mut node = self.primary()?;
        while let Some(Token::LBracket) = self.peek() {
            let at = self.here();
            self.bump();
            self.enter_nest(at)?;
            let k = self.index()?;
            self.depth -= 1;
            node = Ast::Index(Box::new(node), k);
        }
        Ok(node)
    }

    /// The `k` and `]` after a `[`: a whole number, optionally negative,
    /// within `MAX_INDEX`.
    fn index(&mut self) -> Result<i64, ParseError> {
        let at = self.here();
        let negative = matches!(self.peek(), Some(Token::Minus));
        if negative {
            self.bump();
        }
        let k = match self.bump() {
            Some(Token::Num(n)) if n.fract() == 0.0 && *n <= MAX_INDEX as f64 => *n as i64,
            _ => {
                return Err(ParseError {
                    position: at,
                    message: format!("[k] takes a whole number up to {MAX_INDEX}"),
                });
            }
        };
        match self.bump() {
            Some(Token::RBracket) => Ok(if negative { -k } else { k }),
            _ => Err(ParseError {
                position: self.here(),
                message: "expected ']'".into(),
            }),
        }
    }

    fn primary(&mut self) -> Result<Ast<RefName>, ParseError> {
        let at = self.here();
        match self.bump() {
            Some(Token::LParen) => {
                self.enter_nest(at)?;
                let inner = self.expr()?;
                self.depth -= 1;
                match self.bump() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(ParseError {
                        position: self.here(),
                        message: "expected ')'".into(),
                    }),
                }
            }
            Some(Token::Num(n)) => Ok(Ast::Num(*n)),
            Some(Token::Ref(r)) => Ok(Ast::Ref(r.clone())),
            Some(Token::Call(f)) => {
                // The tokenizer makes a call only of a word with `(`
                // right behind it, so the paren is the next token.
                let open = self.here();
                match self.bump() {
                    Some(Token::LParen) => {}
                    _ => {
                        return Err(ParseError {
                            position: open,
                            message: "expected '('".into(),
                        });
                    }
                }
                self.enter_nest(open)?;
                let mut args = vec![self.expr()?];
                while let Some(Token::Comma) = self.peek() {
                    self.bump();
                    args.push(self.expr()?);
                }
                self.depth -= 1;
                match self.bump() {
                    Some(Token::RParen) => Ok(Ast::Call(*f, args)),
                    _ => Err(ParseError {
                        position: self.here(),
                        message: "expected ')'".into(),
                    }),
                }
            }
            Some(_) | None => Err(ParseError {
                position: at,
                message: "expected a value".into(),
            }),
        }
    }

    /// Enters one level of `-`/`(`/call/`[` nesting, refusing past
    /// `MAX_DEPTH`.
    fn enter_nest(&mut self, at: usize) -> Result<(), ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(ParseError {
                position: at,
                message: format!("expression nests too deeply (more than {MAX_DEPTH} levels)"),
            });
        }
        self.depth += 1;
        Ok(())
    }
}

pub fn parse(text: &str) -> Result<Ast<RefName>, ParseError> {
    let toks = tokenize(text)?;
    if toks.len() > MAX_TOKENS {
        return Err(ParseError {
            position: text.len(),
            message: format!("expression is too long (more than {MAX_TOKENS} tokens)"),
        });
    }
    let mut p = Parser {
        toks: &toks,
        pos: 0,
        end: text.len(),
        depth: 0,
    };
    let ast = p.expr()?;
    if p.pos != toks.len() {
        return Err(ParseError {
            position: p.here(),
            message: "unexpected token".into(),
        });
    }
    Ok(ast)
}

impl<R> Ast<R> {
    /// Map every reference through `lookup`; the first `None` is the
    /// error, naming the reference as typed.
    pub fn resolve(self, lookup: &mut impl FnMut(&R) -> Option<u8>) -> Result<Expr, R> {
        Ok(match self {
            Ast::Ref(r) => match lookup(&r) {
                Some(slot) => Ast::Ref(slot),
                None => return Err(r),
            },
            Ast::Num(n) => Ast::Num(n),
            Ast::Neg(inner) => Ast::Neg(Box::new(inner.resolve(lookup)?)),
            Ast::Bin(op, l, r) => Ast::Bin(
                op,
                Box::new(l.resolve(lookup)?),
                Box::new(r.resolve(lookup)?),
            ),
            Ast::Call(f, args) => Ast::Call(
                f,
                args.into_iter()
                    .map(|a| a.resolve(lookup))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Ast::Index(x, k) => Ast::Index(Box::new(x.resolve(lookup)?), k),
        })
    }
}

impl Expr {
    /// The slots this expression reads, ascending, deduplicated.
    pub fn slots(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.collect_slots(&mut out);
        out.sort_unstable();
        out.dedup();
        out
    }

    fn collect_slots(&self, out: &mut Vec<u8>) {
        match self {
            Ast::Ref(s) => out.push(*s),
            Ast::Num(_) => {}
            Ast::Neg(inner) => inner.collect_slots(out),
            Ast::Bin(_, l, r) => {
                l.collect_slots(out);
                r.collect_slots(out);
            }
            Ast::Call(_, args) => {
                for a in args {
                    a.collect_slots(out);
                }
            }
            Ast::Index(x, _) => x.collect_slots(out),
        }
    }

    /// The slots this expression reads as series: every reference
    /// outside a fold and an index, ascending, deduplicated. What the
    /// compiler joins; a slot read only inside a fold does not narrow
    /// the expression's buckets.
    pub fn series_slots(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.collect_series_slots(&mut out);
        out.sort_unstable();
        out.dedup();
        out
    }

    fn collect_series_slots(&self, out: &mut Vec<u8>) {
        match self {
            Ast::Ref(s) => out.push(*s),
            Ast::Num(_) => {}
            Ast::Neg(inner) => inner.collect_series_slots(out),
            Ast::Bin(_, l, r) => {
                l.collect_series_slots(out);
                r.collect_series_slots(out);
            }
            Ast::Index(_, _) => {}
            Ast::Call(f, args) => {
                let folds = match f.kind() {
                    Kind::Fold => true,
                    Kind::MinMax => args.len() == 1,
                    _ => false,
                };
                if !folds {
                    for a in args {
                        a.collect_series_slots(out);
                    }
                }
            }
        }
    }
}

/// Every reference in `text`, in text order, with its byte span. What a
/// caller that rewrites names inside an expression walks, so it follows
/// the tokenizer's word boundaries (`s1.x` is one name, `2s1` is a
/// number then a name) rather than a guess at them. A call is not a
/// reference.
pub fn references(text: &str) -> Result<Vec<(std::ops::Range<usize>, RefName)>, ParseError> {
    Ok(tokenize(text)?
        .into_iter()
        .filter_map(|(start, tok)| match tok {
            Token::Ref(r) => Some((start..start + r.display().len(), r)),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> RefName {
        RefName {
            identity: s.into(),
            source: None,
        }
    }

    /// Resolves `sN`-shaped identities to slot N: a test's shorthand
    /// for a tile that happens to hold identities with those names.
    fn by_s_number(r: &RefName) -> Option<u8> {
        r.identity.strip_prefix('s')?.parse().ok()
    }

    #[test]
    fn precedence_and_associativity() {
        // 1 + 2 * 3 - 4 / 2  ==  (1 + (2 * 3)) - (4 / 2)
        let ast = parse("1 + 2 * 3 - 4 / 2").unwrap();
        let expect = Ast::Bin(
            Op::Sub,
            Box::new(Ast::Bin(
                Op::Add,
                Box::new(Ast::Num(1.0)),
                Box::new(Ast::Bin(
                    Op::Mul,
                    Box::new(Ast::Num(2.0)),
                    Box::new(Ast::Num(3.0)),
                )),
            )),
            Box::new(Ast::Bin(
                Op::Div,
                Box::new(Ast::Num(4.0)),
                Box::new(Ast::Num(2.0)),
            )),
        );
        assert_eq!(ast, expect);
        // left-associative: a - b - c == (a - b) - c
        let ast = parse("A - B - C").unwrap();
        assert!(matches!(ast, Ast::Bin(Op::Sub, ref l, _) if matches!(**l, Ast::Bin(Op::Sub, ..))));
        assert_eq!(
            parse("-A * 2").unwrap(),
            Ast::Bin(
                Op::Mul,
                Box::new(Ast::Neg(Box::new(Ast::Ref(id("A"))))),
                Box::new(Ast::Num(2.0))
            ),
            "unary minus binds tighter than *"
        );
    }

    #[test]
    fn unary_minus_parentheses_and_every_reference_form() {
        assert_eq!(parse("-A").unwrap(), Ast::Neg(Box::new(Ast::Ref(id("A")))));
        assert_eq!(
            parse("-(A + 2.5)").unwrap(),
            Ast::Neg(Box::new(Ast::Bin(
                Op::Add,
                Box::new(Ast::Ref(id("A"))),
                Box::new(Ast::Num(2.5))
            )))
        );
        assert_eq!(parse("SPX.close").unwrap(), Ast::Ref(id("SPX.close")));
        assert_eq!(
            parse("SPX.close@kdb_hist / VIX").unwrap(),
            Ast::Bin(
                Op::Div,
                Box::new(Ast::Ref(RefName {
                    identity: "SPX.close".into(),
                    source: Some("kdb_hist".into())
                })),
                Box::new(Ast::Ref(id("VIX")))
            )
        );
        assert_eq!(parse("spx_1y").unwrap(), Ast::Ref(id("spx_1y")));
    }

    /// There is no slot handle: `s12` is a name like any other, and may
    /// carry a source.
    #[test]
    fn a_handle_shaped_word_is_an_identity() {
        assert_eq!(parse("  s12  ").unwrap(), Ast::Ref(id("s12")));
        assert_eq!(
            parse("s1@demo_rest").unwrap(),
            Ast::Ref(RefName {
                identity: "s1".into(),
                source: Some("demo_rest".into())
            })
        );
    }

    #[test]
    fn references_answer_each_name_with_its_byte_span_in_order() {
        let text = "(SPX.close@kdb - s1) / VIX";
        let refs = references(text).unwrap();
        let spans: Vec<(&str, String)> = refs
            .iter()
            .map(|(span, r)| (&text[span.clone()], r.display()))
            .collect();
        assert_eq!(
            spans,
            vec![
                ("SPX.close@kdb", "SPX.close@kdb".to_string()),
                ("s1", "s1".to_string()),
                ("VIX", "VIX".to_string()),
            ]
        );
        assert_eq!(references("2 * 3").unwrap(), vec![]);
        assert!(references("A ^ 2").is_err());
    }

    #[test]
    fn foreign_tokens_are_refused_with_the_grammar_message() {
        for text in ["A ^ 2", "A % 2", "A & B", "A ! B"] {
            let err = parse(text).unwrap_err();
            assert_eq!(err.message, NOT_IN_GRAMMAR, "{text}");
        }
        for text in ["", "A +", "(A", "A B", "1.", "+ A", "* 2"] {
            assert!(parse(text).is_err(), "{text:?} must not parse");
        }
        assert_eq!(parse("A ^ 2").unwrap_err().position, 2);
        assert_eq!(parse("a@b@c").unwrap_err().message, NOT_IN_GRAMMAR);
        assert_eq!(parse("a@b@c").unwrap_err().position, 3);
    }

    #[test]
    fn nesting_deeper_than_the_cap_is_a_parse_error() {
        // 100 levels: past `MAX_DEPTH` and inside `MAX_TOKENS`, so the
        // nesting message is the one a trader sees (the token bound is
        // checked first, and a wall of 200 parens would trip that one).
        let text = "(".repeat(100) + "A" + &")".repeat(100);
        let err = parse(&text).unwrap_err();
        assert_eq!(
            err.message,
            format!("expression nests too deeply (more than {MAX_DEPTH} levels)")
        );
        let text = "(".repeat(60) + "A" + &")".repeat(60);
        assert!(parse(&text).is_ok(), "60 levels must still parse");
        // A call is three tokens per level (`abs`, `(`, `)`) and an
        // index three per link, so 80 levels keep both inside
        // `MAX_TOKENS` while still past `MAX_DEPTH`.
        let text = "abs(".repeat(80) + "A" + &")".repeat(80);
        let err = parse(&text).unwrap_err();
        assert_eq!(
            err.message,
            format!("expression nests too deeply (more than {MAX_DEPTH} levels)"),
            "a call nests like a parenthesis"
        );
        let text = "abs(".repeat(60) + "A" + &")".repeat(60);
        assert!(parse(&text).is_ok());
        let text = "A".to_string() + &"[0]".repeat(80);
        assert!(parse(&text).is_ok(), "indexes chain without nesting");
    }

    #[test]
    fn a_long_chain_is_refused_by_the_token_bound() {
        // `expr`/`term` fold left-deep iteratively, so a chain nests
        // nothing and `MAX_DEPTH` never fires — but the tree is as deep
        // as the chain is long, and `resolve`, `collect_slots`, the
        // compiler's `lower` and the `Box` drop glue all recurse on it.
        let text = "s1".to_string() + &" + 1".repeat(200);
        let err = parse(&text).unwrap_err();
        assert_eq!(
            err.message,
            format!("expression is too long (more than {MAX_TOKENS} tokens)")
        );
        let text = "s1".to_string() + &" + 1".repeat(100);
        let ast = parse(&text).expect("100 terms are inside the bound");
        let expr = ast
            .resolve(&mut by_s_number)
            .expect("every reference resolves");
        assert_eq!(expr.slots(), vec![1]);
    }

    #[test]
    fn resolve_maps_references_to_slots_and_names_the_first_miss() {
        let ast = parse("SPX.close / B + VIX@rest").unwrap();
        let mut lookup = |r: &RefName| match (r.identity.as_str(), r.source.as_deref()) {
            ("SPX.close", None) => Some(1),
            ("B", None) => Some(2),
            _ => None,
        };
        let miss = ast.clone().resolve(&mut lookup).unwrap_err();
        assert_eq!(miss.display(), "VIX@rest");
        let mut lookup = |r: &RefName| match r.identity.as_str() {
            "SPX.close" => Some(1),
            "B" => Some(2),
            "VIX" => Some(3),
            _ => None,
        };
        let expr = ast.resolve(&mut lookup).unwrap();
        assert_eq!(expr.slots(), vec![1, 2, 3]);
    }

    #[test]
    fn display_spells_a_reference_as_typed() {
        assert_eq!(id("VIX").display(), "VIX");
        assert_eq!(
            RefName {
                identity: "SPX.close".into(),
                source: Some("kdb_hist".into())
            }
            .display(),
            "SPX.close@kdb_hist"
        );
    }

    #[test]
    fn calls_and_indexes_parse_into_call_and_index_nodes() {
        let a = || Ast::Ref(id("A"));
        assert_eq!(
            parse("mean(A)").unwrap(),
            Ast::Call(Function::Mean, vec![a()])
        );
        assert_eq!(
            parse("sma(A, 20)").unwrap(),
            Ast::Call(Function::Sma, vec![a(), Ast::Num(20.0)])
        );
        assert_eq!(
            parse("min(A, B, 3)").unwrap(),
            Ast::Call(Function::Min, vec![a(), Ast::Ref(id("B")), Ast::Num(3.0)])
        );
        assert_eq!(parse("A[0]").unwrap(), Ast::Index(Box::new(a()), 0));
        assert_eq!(parse("A[-1]").unwrap(), Ast::Index(Box::new(a()), -1));
        assert_eq!(
            parse("(A / B)[2]").unwrap(),
            Ast::Index(
                Box::new(Ast::Bin(
                    Op::Div,
                    Box::new(a()),
                    Box::new(Ast::Ref(id("B")))
                )),
                2
            )
        );
        assert_eq!(
            parse("-A[0]").unwrap(),
            Ast::Neg(Box::new(Ast::Index(Box::new(a()), 0))),
            "an index binds tighter than unary minus"
        );
        assert_eq!(
            parse("A / A[0]").unwrap(),
            Ast::Bin(
                Op::Div,
                Box::new(a()),
                Box::new(Ast::Index(Box::new(a()), 0))
            ),
            "and tighter than division"
        );
        assert_eq!(
            parse("sma(diff(A), 3)").unwrap(),
            Ast::Call(
                Function::Sma,
                vec![Ast::Call(Function::Diff, vec![a()]), Ast::Num(3.0)]
            )
        );
        assert_eq!(
            parse("A[0][1]").unwrap(),
            Ast::Index(Box::new(Ast::Index(Box::new(a()), 0)), 1),
            "indexes chain left to right (the shape check refuses this later)"
        );
    }

    #[test]
    fn a_word_before_a_paren_is_a_call_and_anywhere_else_a_name() {
        assert_eq!(parse("max").unwrap(), Ast::Ref(id("max")));
        assert_eq!(
            parse("max@kdb").unwrap(),
            Ast::Ref(RefName {
                identity: "max".into(),
                source: Some("kdb".into())
            })
        );
        assert_eq!(
            parse("max (A)").unwrap_err().message,
            "unexpected token",
            "a space before the paren makes a name, not a call"
        );
        let err = parse("foo(A)").unwrap_err();
        assert_eq!(err.message, "unknown function 'foo'");
        assert_eq!(err.position, 0);
        let err = parse("A + spx.close(2)").unwrap_err();
        assert_eq!(err.message, "unknown function 'spx.close'");
        assert_eq!(err.position, 4);
    }

    #[test]
    fn malformed_calls_and_indexes_are_refused() {
        for text in [
            "mean(", "mean(A", "mean(A,)", "mean(,A)", "min()", "A[", "A[1", "A[1.5]", "A[]",
            "A[B]", "A[--1]", "sma(A 3)", "A, B", "[0]",
        ] {
            assert!(parse(text).is_err(), "{text:?} must not parse");
        }
        let whole = format!("[k] takes a whole number up to {MAX_INDEX}");
        assert_eq!(parse("A[1.5]").unwrap_err().message, whole);
        assert_eq!(parse("A[]").unwrap_err().message, whole);
        assert_eq!(parse("A[1000001]").unwrap_err().message, whole);
        assert!(parse("A[1000000]").is_ok());
        assert_eq!(parse("mean(A").unwrap_err().message, "expected ')'");
        assert_eq!(parse("A[1").unwrap_err().message, "expected ']'");
        assert_eq!(parse("A, B").unwrap_err().message, "unexpected token");
    }

    #[test]
    fn references_skip_calls_and_indexes_but_keep_their_operands() {
        let text = "sma(A@kdb, 20) / A[0] + mean(B)";
        let refs = references(text).unwrap();
        let spans: Vec<(&str, String)> = refs
            .iter()
            .map(|(span, r)| (&text[span.clone()], r.display()))
            .collect();
        assert_eq!(
            spans,
            vec![
                ("A@kdb", "A@kdb".to_string()),
                ("A", "A".to_string()),
                ("B", "B".to_string()),
            ]
        );
    }

    #[test]
    fn shapes_follow_the_table() {
        let s = |t: &str| parse(t).unwrap().shape();
        assert_eq!(s("A"), Ok(Shape::Series));
        assert_eq!(s("2"), Ok(Shape::Scalar));
        assert_eq!(s("-2 * 3"), Ok(Shape::Scalar));
        assert_eq!(s("A * 2"), Ok(Shape::Series));
        assert_eq!(s("mean(A)"), Ok(Shape::Scalar));
        assert_eq!(s("A / mean(A)"), Ok(Shape::Series));
        assert_eq!(s("A[0]"), Ok(Shape::Scalar));
        assert_eq!(s("(A / B)[-1]"), Ok(Shape::Scalar));
        assert_eq!(s("log(A)"), Ok(Shape::Series));
        assert_eq!(s("log(mean(A))"), Ok(Shape::Scalar));
        assert_eq!(s("min(A)"), Ok(Shape::Scalar));
        assert_eq!(s("min(A, B)"), Ok(Shape::Series));
        assert_eq!(s("max(A, 2)"), Ok(Shape::Series));
        assert_eq!(s("max(mean(A), 2)"), Ok(Shape::Scalar));
        assert_eq!(s("diff(A)"), Ok(Shape::Series));
        assert_eq!(s("sma(A, 20)"), Ok(Shape::Series));
        assert_eq!(s("lag(A, 1)"), Ok(Shape::Series));
        assert_eq!(s("z(log(A), 5)"), Ok(Shape::Series));
        assert_eq!(s("mean(diff(A))"), Ok(Shape::Scalar));
        assert_eq!(s("mean(A) - mean(B)"), Ok(Shape::Scalar));
        assert_eq!(
            s("sma(A, 10000)"),
            Ok(Shape::Series),
            "the top of the count range"
        );
    }

    #[test]
    fn shape_refusals_name_the_function_and_what_it_takes() {
        let e = |t: &str| parse(t).unwrap().shape().unwrap_err();
        assert_eq!(e("mean(2)"), "mean needs a series");
        assert_eq!(e("mean(A[0])"), "mean needs a series");
        assert_eq!(e("min(2)"), "min needs a series");
        assert_eq!(e("2[0]"), "[k] needs a series");
        assert_eq!(e("A[0][0]"), "[k] needs a series");
        assert_eq!(e("diff(mean(A))"), "diff needs a series");
        assert_eq!(e("sma(2, 3)"), "sma needs a series");
        assert_eq!(e("mean(A, B)"), "mean takes one argument");
        assert_eq!(e("abs(A, B)"), "abs takes one argument");
        assert_eq!(e("diff(A, 1)"), "diff takes one argument");
        assert_eq!(e("sma(A)"), "sma takes a series and a count");
        assert_eq!(e("lag(A)"), "lag takes a series and a count");
        assert_eq!(e("lag(A, 1, 2)"), "lag takes a series and a count");
        let count = |f: &str| format!("{f}'s count must be a whole number from 1 to {MAX_COUNT}");
        assert_eq!(e("sma(A, 0)"), count("sma"));
        assert_eq!(e("sma(A, 2.5)"), count("sma"));
        assert_eq!(e("sma(A, 10001)"), count("sma"));
        assert_eq!(e("sma(A, -3)"), count("sma"));
        assert_eq!(e("lag(A, B)"), count("lag"));
        assert_eq!(e("ema(A, mean(A))"), count("ema"));
        assert_eq!(
            e("mean(2) + 3[0]"),
            "mean needs a series",
            "the first violation in reading order is the one named"
        );
    }

    #[test]
    fn series_slots_leave_out_what_a_fold_or_index_reads() {
        let e = |t: &str| parse(t).unwrap().resolve(&mut by_s_number).unwrap();
        assert_eq!(e("s1 / mean(s2)").series_slots(), vec![1]);
        assert_eq!(e("s1 / mean(s2)").slots(), vec![1, 2]);
        assert_eq!(e("mean(s1) - s2[0]").series_slots(), Vec::<u8>::new());
        assert_eq!(e("min(s1, s2)").series_slots(), vec![1, 2]);
        assert_eq!(e("min(s2)").series_slots(), Vec::<u8>::new());
        assert_eq!(e("sma(diff(s3), 5) + s1").series_slots(), vec![1, 3]);
        assert_eq!(e("mean(s1 * s2) * s1").series_slots(), vec![1]);
        assert_eq!(e("log(s2) + s2").series_slots(), vec![2]);
    }

    #[test]
    fn count_arg_reads_a_validated_count_and_nothing_else() {
        let args = |t: &str| match parse(t).unwrap() {
            Ast::Call(_, args) => args,
            other => panic!("{other:?}"),
        };
        assert_eq!(count_arg(&args("sma(A, 20)")), Some(20));
        assert_eq!(count_arg(&args("diff(A)")), None);
        assert_eq!(count_arg(&args("sma(A, 0)")), None);
        assert_eq!(count_arg(&args("sma(A, B)")), None);
    }

    /// The help line's copy for every function: a signature spelled from
    /// its name with the arity its kind takes, a result shape, and one
    /// short sentence without a trailing period, so the line stays one
    /// line.
    #[test]
    fn every_function_has_a_signature_result_and_short_description() {
        for f in Function::ALL {
            let sig = f.signature();
            assert!(
                sig.starts_with(&format!("{}(", f.name())) && sig.ends_with(')'),
                "{sig}"
            );
            assert!(!f.result().is_empty(), "{}", f.name());
            let d = f.describe();
            assert!(!d.is_empty(), "{}", f.name());
            assert!(d.chars().count() <= 72, "{}: {d}", f.name());
            assert!(!d.ends_with('.'), "{}: {d}", f.name());
            let commas = sig.matches(',').count();
            match f.kind() {
                Kind::Fold | Kind::Pointwise | Kind::Along { count: false } => {
                    assert_eq!(commas, 0, "{sig}")
                }
                Kind::Along { count: true } | Kind::Rolling => assert_eq!(commas, 1, "{sig}"),
                Kind::MinMax => {
                    assert_eq!(commas, 1, "{sig}");
                    assert!(sig.contains('…'), "{sig}");
                }
            }
        }
        assert_eq!(Function::Sma.signature(), "sma(series, n)");
        assert_eq!(Function::Abs.signature(), "abs(x)");
        assert_eq!(Function::Min.signature(), "min(series, …)");
        assert_eq!(Function::Diff.signature(), "diff(series)");
        assert_eq!(Function::Mean.result(), "number");
        assert_eq!(Function::Abs.result(), "same as its argument");
        assert_eq!(Function::Max.result(), "number or series");
        assert_eq!(Function::Lag.result(), "series");
        assert!(INDEX_HELP.starts_with("A[k] · number · "));
    }
}
