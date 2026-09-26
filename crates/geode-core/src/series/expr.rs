//! The expression language: arithmetic
//! over named series, nothing else. A hand-written recursive-descent
//! parser, pure; the resolved tree names slots only, so an identity
//! never reaches the compiler as text. A reference is a series name —
//! there is no slot handle — so an expression can name only a source
//! series, never another expression.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

/// The tree, generic over how a reference is spelled: `RefName` as
/// parsed, `u8` once resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Ast<R> {
    Ref(R),
    Num(f64),
    Neg(Box<Ast<R>>),
    Bin(Op, Box<Ast<R>>, Box<Ast<R>>),
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

/// Error text describing the supported arithmetic grammar.
pub const ARITHMETIC_ONLY: &str = "arithmetic only: + - * / and parentheses";

/// The deepest a unary-minus/parenthesis nest may go before `parse`
/// refuses it. Bounds the recursion in `Parser::factor` (a pasted wall
/// of parentheses would otherwise overflow the stack — an abort, not a
/// panic, nothing can contain). It does NOT bound the tree on its own:
/// `expr`/`term` fold left-deep iteratively, so `1+1+…` builds a tree
/// as deep as it is long while nesting nothing. `MAX_TOKENS` is what
/// bounds the node count, and with it every recursion OVER the tree —
/// `Ast::resolve`, `Expr::collect_slots`, the compiler's `lower` and
/// the `Box` drop glue. The two bounds together are the guarantee;
/// this one is kept because it gives the better message for nesting.
pub const MAX_DEPTH: usize = 64;

/// The most tokens an expression may carry. The tree has at most one
/// node per token, so this bounds its depth however it is shaped — a
/// left-deep chain included, which `MAX_DEPTH` cannot see.
pub const MAX_TOKENS: usize = 256;

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Ref(RefName),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

fn is_source_char(c: char) -> bool {
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
                // A `(` right after a word is a function call, which is
                // not arithmetic.
                if i < bytes.len() && bytes[i] == b'(' {
                    return Err(ParseError {
                        position: i,
                        message: ARITHMETIC_ONLY.into(),
                    });
                }
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
            _ => {
                return Err(ParseError {
                    position: start,
                    message: ARITHMETIC_ONLY.into(),
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

    fn factor(&mut self) -> Result<Ast<RefName>, ParseError> {
        let at = self.here();
        match self.bump() {
            Some(Token::Minus) => {
                self.enter_nest(at)?;
                let inner = self.factor()?;
                self.depth -= 1;
                Ok(Ast::Neg(Box::new(inner)))
            }
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
            Some(_) => Err(ParseError {
                position: at,
                message: "expected a value".into(),
            }),
            None => Err(ParseError {
                position: at,
                message: "expected a value".into(),
            }),
        }
    }

    /// Enters one level of `-`/`(` nesting, refusing past `MAX_DEPTH`.
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
    /// Map every reference through `f`; the first `None` is the error,
    /// naming the reference as typed.
    pub fn resolve(self, f: &mut impl FnMut(&R) -> Option<u8>) -> Result<Expr, R> {
        Ok(match self {
            Ast::Ref(r) => match f(&r) {
                Some(slot) => Ast::Ref(slot),
                None => return Err(r),
            },
            Ast::Num(n) => Ast::Num(n),
            Ast::Neg(inner) => Ast::Neg(Box::new(inner.resolve(f)?)),
            Ast::Bin(op, l, r) => Ast::Bin(op, Box::new(l.resolve(f)?), Box::new(r.resolve(f)?)),
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
        }
    }
}

/// Every reference in `text`, in text order, with its byte span. What a
/// caller that rewrites names inside an expression walks, so it follows
/// the tokenizer's word boundaries (`s1.x` is one name, `2s1` is a
/// number then a name) rather than a guess at them.
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
    fn foreign_tokens_are_refused_with_the_arithmetic_only_message() {
        for text in ["A ^ 2", "A % 2", "log(A)", "A, B", "A & B", "max(A, B)"] {
            let err = parse(text).unwrap_err();
            assert_eq!(err.message, ARITHMETIC_ONLY, "{text}");
        }
        for text in ["", "A +", "(A", "A B", "1.", "+ A", "* 2"] {
            assert!(parse(text).is_err(), "{text:?} must not parse");
        }
        assert_eq!(parse("A ^ 2").unwrap_err().position, 2);
        assert_eq!(parse("a@b@c").unwrap_err().message, ARITHMETIC_ONLY);
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
}
