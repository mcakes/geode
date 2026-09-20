//! The expression language (timeseries spec §7, ruling 8): arithmetic
//! over slots, nothing else. A hand-written recursive-descent parser,
//! pure; the resolved tree names slots only, so an identity never
//! reaches the compiler as text.

use super::SeriesSpec;
use super::SlotKind;

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

/// A reference as typed: a slot handle `s3`, or an identity with an
/// optional `@source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefName {
    Handle(u8),
    Identity {
        identity: String,
        source: Option<String>,
    },
}

impl RefName {
    pub fn display(&self) -> String {
        match self {
            RefName::Handle(n) => format!("s{n}"),
            RefName::Identity {
                identity,
                source: None,
            } => identity.clone(),
            RefName::Identity {
                identity,
                source: Some(s),
            } => format!("{identity}@{s}"),
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

/// The boundary, said where a trader would cross it (spec §7).
pub const ARITHMETIC_ONLY: &str = "arithmetic only: + - * / and parentheses";

/// The deepest a unary-minus/parenthesis nest may go before `parse`
/// refuses it. Bounds the recursion in `Parser::factor` (a pasted wall
/// of parentheses would otherwise overflow the stack — an abort, not a
/// panic, nothing can contain) and, because the tree it produces can
/// then be no deeper than this, bounds the recursion in `Ast::resolve`
/// and `Expr::slots` too.
pub const MAX_DEPTH: usize = 64;

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
                let handle = word
                    .strip_prefix('s')
                    .filter(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|rest| rest.parse::<u8>().ok());
                match handle {
                    Some(n) => Token::Ref(RefName::Handle(n)),
                    None => {
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
                        Token::Ref(RefName::Identity {
                            identity: word.to_string(),
                            source,
                        })
                    }
                }
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

/// A topological order over the expression slots of `specs`, operands
/// first, so the compiler can lower each expression over CTEs that
/// already exist. `Err(slot)` is a slot on a cycle (a self-reference
/// included). Source slots are leaves and are not listed.
/// A reference to a slot absent from `specs` is treated as a leaf
/// here; the compiler's validation, not this function, refuses it.
pub fn expression_order(specs: &[SeriesSpec]) -> Result<Vec<u8>, u8> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unseen,
        Visiting,
        Done,
    }
    let exprs: Vec<(u8, &Expr)> = specs
        .iter()
        .filter_map(|s| match &s.kind {
            SlotKind::Expr(e) => Some((s.slot, e)),
            SlotKind::Source { .. } => None,
        })
        .collect();
    let mut marks = vec![Mark::Unseen; exprs.len()];
    let mut order = Vec::with_capacity(exprs.len());
    fn visit(
        i: usize,
        exprs: &[(u8, &Expr)],
        marks: &mut [Mark],
        order: &mut Vec<u8>,
    ) -> Result<(), u8> {
        match marks[i] {
            Mark::Done => return Ok(()),
            Mark::Visiting => return Err(exprs[i].0),
            Mark::Unseen => {}
        }
        marks[i] = Mark::Visiting;
        for dep in exprs[i].1.slots() {
            if let Some(j) = exprs.iter().position(|(s, _)| *s == dep) {
                visit(j, exprs, marks, order)?;
            }
        }
        marks[i] = Mark::Done;
        order.push(exprs[i].0);
        Ok(())
    }
    for i in 0..exprs.len() {
        visit(i, &exprs, &mut marks, &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::series::{BucketRule, SeriesSpec, SlotKind};

    fn id(s: &str) -> RefName {
        RefName::Identity {
            identity: s.into(),
            source: None,
        }
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
        let ast = parse("s1 - s2 - s3").unwrap();
        assert!(matches!(ast, Ast::Bin(Op::Sub, ref l, _) if matches!(**l, Ast::Bin(Op::Sub, ..))));
        assert_eq!(
            parse("-s1 * 2").unwrap(),
            Ast::Bin(
                Op::Mul,
                Box::new(Ast::Neg(Box::new(Ast::Ref(RefName::Handle(1))))),
                Box::new(Ast::Num(2.0))
            ),
            "unary minus binds tighter than *"
        );
    }

    #[test]
    fn unary_minus_parentheses_and_every_reference_form() {
        assert_eq!(
            parse("-s1").unwrap(),
            Ast::Neg(Box::new(Ast::Ref(RefName::Handle(1))))
        );
        assert_eq!(
            parse("-(s1 + 2.5)").unwrap(),
            Ast::Neg(Box::new(Ast::Bin(
                Op::Add,
                Box::new(Ast::Ref(RefName::Handle(1))),
                Box::new(Ast::Num(2.5))
            )))
        );
        assert_eq!(parse("SPX.close").unwrap(), Ast::Ref(id("SPX.close")));
        assert_eq!(
            parse("SPX.close@kdb_hist / VIX").unwrap(),
            Ast::Bin(
                Op::Div,
                Box::new(Ast::Ref(RefName::Identity {
                    identity: "SPX.close".into(),
                    source: Some("kdb_hist".into())
                })),
                Box::new(Ast::Ref(id("VIX")))
            )
        );
        assert_eq!(parse("  s12  ").unwrap(), Ast::Ref(RefName::Handle(12)));
        assert_eq!(
            parse("spx_1y").unwrap(),
            Ast::Ref(id("spx_1y")),
            "an identity may start with s and not be a handle"
        );
        assert_eq!(
            parse("s1x").unwrap(),
            Ast::Ref(id("s1x")),
            "a handle is s followed by digits and nothing else"
        );
        assert_eq!(
            parse("s999").unwrap(),
            Ast::Ref(id("s999")),
            "a handle past u8 is an identity, not an error"
        );
    }

    #[test]
    fn foreign_tokens_are_refused_with_the_arithmetic_only_message() {
        for text in [
            "s1 ^ 2",
            "s1 % 2",
            "log(s1)",
            "s1, s2",
            "s1 & s2",
            "max(s1, s2)",
        ] {
            let err = parse(text).unwrap_err();
            assert_eq!(err.message, ARITHMETIC_ONLY, "{text}");
        }
        for text in ["", "s1 +", "(s1", "s1 s2", "1.", "+ s1", "* 2"] {
            assert!(parse(text).is_err(), "{text:?} must not parse");
        }
        assert_eq!(parse("s1 ^ 2").unwrap_err().position, 3);
        assert_eq!(parse("a@b@c").unwrap_err().message, ARITHMETIC_ONLY);
        assert_eq!(parse("a@b@c").unwrap_err().position, 3);
    }

    #[test]
    fn nesting_deeper_than_the_cap_is_a_parse_error() {
        let text = "(".repeat(200) + "s1" + &")".repeat(200);
        let err = parse(&text).unwrap_err();
        assert_eq!(
            err.message,
            format!("expression nests too deeply (more than {MAX_DEPTH} levels)")
        );
        let text = "(".repeat(60) + "s1" + &")".repeat(60);
        assert!(parse(&text).is_ok(), "60 levels must still parse");
    }

    #[test]
    fn resolve_maps_references_to_slots_and_names_the_first_miss() {
        let ast = parse("SPX.close / s2 + VIX@rest").unwrap();
        let mut lookup = |r: &RefName| match r {
            RefName::Handle(n) => Some(*n),
            RefName::Identity { identity, source }
                if identity == "SPX.close" && source.is_none() =>
            {
                Some(1)
            }
            _ => None,
        };
        let miss = ast.clone().resolve(&mut lookup).unwrap_err();
        assert_eq!(miss.display(), "VIX@rest");
        let mut lookup = |r: &RefName| match r {
            RefName::Handle(n) => Some(*n),
            RefName::Identity { identity, .. } if identity == "SPX.close" => Some(1),
            RefName::Identity { identity, .. } if identity == "VIX" => Some(3),
            _ => None,
        };
        let expr = ast.resolve(&mut lookup).unwrap();
        assert_eq!(expr.slots(), vec![1, 2, 3]);
    }

    fn spec(slot: u8, kind: SlotKind) -> SeriesSpec {
        SeriesSpec { slot, kind }
    }
    fn source(slot: u8) -> SeriesSpec {
        spec(
            slot,
            SlotKind::Source {
                source: "k".into(),
                identity: format!("id{slot}"),
                rule: BucketRule::Last,
            },
        )
    }
    fn expr_over(slot: u8, text: &str) -> SeriesSpec {
        let e = parse(text)
            .unwrap()
            .resolve(&mut |r: &RefName| match r {
                RefName::Handle(n) => Some(*n),
                _ => None,
            })
            .unwrap();
        spec(slot, SlotKind::Expr(e))
    }

    #[test]
    fn expression_order_puts_operands_first_and_names_a_cycle() {
        // s4 = s3 / s1, s3 = s1 - s2: s3 must come before s4 whatever the request order.
        let specs = vec![
            source(1),
            source(2),
            expr_over(4, "s3 / s1"),
            expr_over(3, "s1 - s2"),
        ];
        assert_eq!(expression_order(&specs).unwrap(), vec![3, 4]);
        let cyclic = vec![source(1), expr_over(2, "s3 + s1"), expr_over(3, "s2 * 2")];
        let bad = expression_order(&cyclic).unwrap_err();
        assert!(bad == 2 || bad == 3);
        let self_ref = vec![expr_over(2, "s2 + 1")];
        assert_eq!(expression_order(&self_ref).unwrap_err(), 2);
        assert_eq!(expression_order(&[source(1)]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn display_spells_a_reference_as_typed() {
        assert_eq!(RefName::Handle(7).display(), "s7");
        assert_eq!(id("VIX").display(), "VIX");
        assert_eq!(
            RefName::Identity {
                identity: "SPX.close".into(),
                source: Some("kdb_hist".into())
            }
            .display(),
            "SPX.close@kdb_hist"
        );
    }
}
