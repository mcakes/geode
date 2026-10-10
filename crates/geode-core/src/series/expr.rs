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
}

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
}
