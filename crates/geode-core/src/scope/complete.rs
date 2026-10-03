//! Caret-aware reading of a partially typed scope expression, for the
//! suggestion lists. [`lex`] tokenizes what [`super::expr`]'s parser
//! accepts, but it never fails. An unterminated string, a lone `!` and an
//! open `in (` are tokens too. [`context_at`] replays the grammar over the
//! tokens before the caret and names what may come next. Tokens after the
//! caret are ignored, so a half-edited middle still gets suggestions.
//!
//! Unlike the expression parser, this lexer starts a number token at an
//! ASCII digit. Completion therefore does not support column names that
//! start with an ASCII digit, even though the parser accepts them.

use std::ops::Range;

use super::expr::KEYWORDS;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// A run of alphanumerics or `_` that is not a keyword.
    Word,
    /// `and`, `or`, `not`, `in`, `like`, `true` or `false`, lower-cased.
    Keyword(&'static str),
    /// `=`, `!=`, `<>`, `<`, `<=`, `>`, `>=`.
    Op(&'static str),
    /// A quoted string, decoded (`''` is one quote). `closed` is false when
    /// the input ends inside it.
    Str {
        value: String,
        closed: bool,
    },
    /// A run of `[0-9.+-]`, the parser's own number class.
    Num,
    LParen,
    RParen,
    Comma,
    /// Any other single character.
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// Byte range in the lexed text, on character boundaries.
    pub span: Range<usize>,
}

/// Longest first, so `<=` is not read as `<` followed by `=`.
const OPS: [&str; 7] = ["!=", "<>", "<=", ">=", "=", "<", ">"];

fn run_len(s: &str, class: impl Fn(char) -> bool) -> usize {
    s.char_indices()
        .find(|&(_, c)| !class(c))
        .map_or(s.len(), |(i, _)| i)
}

/// Scan a string opened at byte `open`: its decoded value, whether it
/// closed, and the byte just past it.
fn scan_string(text: &str, open: usize) -> (String, bool, usize) {
    let mut value = String::new();
    let mut pos = open + 1;
    while let Some(c) = text[pos..].chars().next() {
        pos += c.len_utf8();
        if c == '\'' {
            if text[pos..].starts_with('\'') {
                value.push('\'');
                pos += 1;
                continue;
            }
            return (value, true, pos);
        }
        value.push(c);
    }
    (value, false, pos)
}

pub fn lex(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut pos = 0;
    while let Some(c) = text[pos..].chars().next() {
        let start = pos;
        if c.is_whitespace() {
            pos += c.len_utf8();
            continue;
        }
        let kind = if c == '\'' {
            let (value, closed, end) = scan_string(text, pos);
            pos = end;
            TokenKind::Str { value, closed }
        } else if c.is_ascii_digit() || matches!(c, '.' | '+' | '-') {
            pos += run_len(&text[pos..], |c| {
                c.is_ascii_digit() || matches!(c, '.' | '+' | '-')
            });
            TokenKind::Num
        } else if c.is_alphanumeric() || c == '_' {
            pos += run_len(&text[pos..], |c| c.is_alphanumeric() || c == '_');
            let word = &text[start..pos];
            match KEYWORDS.iter().find(|k| k.eq_ignore_ascii_case(word)) {
                Some(k) => TokenKind::Keyword(k),
                None => TokenKind::Word,
            }
        } else if let Some(op) = OPS.iter().find(|op| text[pos..].starts_with(**op)) {
            pos += op.len();
            TokenKind::Op(op)
        } else {
            pos += c.len_utf8();
            match c {
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                ',' => TokenKind::Comma,
                _ => TokenKind::Other,
            }
        };
        tokens.push(Token {
            kind,
            span: start..pos,
        });
    }
    tokens
}

/// What may come next at the caret.
#[derive(Debug, Clone, PartialEq)]
pub enum Position {
    /// A column, `not` or `(`.
    Column,
    /// An operator for `column`: a comparison, `like` or `in`.
    Operator { column: String },
    /// The `(` that opens `column`'s `in` list.
    OpenList { column: String },
    /// A value for `column`. `listed` holds the values already in its `in`
    /// list, decoded.
    Value { column: String, listed: Vec<String> },
    /// A finished term. Outside a list: `and`, `or`, and `)` while
    /// `open_parens > 0`. Inside a list: `,` or `)`.
    Connective { open_parens: usize, in_list: bool },
    /// The text before the caret cannot continue into a valid expression.
    Invalid,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub position: Position,
    /// The byte range a suggestion replaces: the partial word, number or
    /// string under the caret, or an empty range at the caret.
    pub token: Range<usize>,
    /// The part of that token before the caret that ranking matches
    /// against. For a string it is decoded and has no quotes.
    pub typed: String,
}

/// A term `walk` found: its column, and its operator when it has one
/// (`in` is not an operator here).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Term {
    pub column: String,
    pub column_span: Range<usize>,
    pub op: Option<(&'static str, Range<usize>)>,
}

/// A word, keyword, number or unclosed string can still grow under the
/// caret. An operator or a bracket is already complete.
fn is_partial(t: &Token, caret: usize) -> bool {
    match &t.kind {
        TokenKind::Word | TokenKind::Keyword(_) | TokenKind::Num | TokenKind::Other => true,
        TokenKind::Str { closed, .. } => !(*closed && caret == t.span.end),
        _ => false,
    }
}

fn literal_value(text: &str, t: &Token) -> Option<String> {
    match &t.kind {
        TokenKind::Str {
            value,
            closed: true,
        } => Some(value.clone()),
        TokenKind::Num => Some(text[t.span.clone()].to_string()),
        TokenKind::Keyword(k @ ("true" | "false")) => Some((*k).to_string()),
        _ => None,
    }
}

enum St {
    Operand,
    Column(String, Range<usize>),
    In(String),
    Op(String),
    List(String, Vec<String>),
    ListValue(String, Vec<String>),
    Term,
}

/// Replay the grammar over `tokens`, reporting each term's column and
/// operator to `on_term`, and return what may come next.
pub(crate) fn walk(text: &str, tokens: &[Token], on_term: &mut impl FnMut(Term)) -> Position {
    let mut st = St::Operand;
    let mut depth = 0usize;
    for t in tokens {
        let lit = literal_value(text, t);
        st = match (st, &t.kind, lit) {
            (St::Operand, TokenKind::Keyword("not"), _) => St::Operand,
            (St::Operand, TokenKind::LParen, _) => {
                depth += 1;
                St::Operand
            }
            (St::Operand, TokenKind::Word, _) => {
                St::Column(text[t.span.clone()].to_string(), t.span.clone())
            }
            (St::Column(c, span), TokenKind::Op(op), _) => {
                on_term(Term {
                    column: c.clone(),
                    column_span: span,
                    op: Some((op, t.span.clone())),
                });
                St::Op(c)
            }
            (St::Column(c, span), TokenKind::Keyword("like"), _) => {
                on_term(Term {
                    column: c.clone(),
                    column_span: span,
                    op: Some(("like", t.span.clone())),
                });
                St::Op(c)
            }
            (St::Column(c, span), TokenKind::Keyword("in"), _) => {
                on_term(Term {
                    column: c.clone(),
                    column_span: span,
                    op: None,
                });
                St::In(c)
            }
            (St::In(c), TokenKind::LParen, _) => St::List(c, Vec::new()),
            (St::Op(_), _, Some(_)) => St::Term,
            (St::List(c, mut listed), _, Some(v)) => {
                listed.push(v);
                St::ListValue(c, listed)
            }
            (St::ListValue(c, listed), TokenKind::Comma, _) => St::List(c, listed),
            (St::ListValue(..), TokenKind::RParen, _) => St::Term,
            (St::Term, TokenKind::Keyword("and" | "or"), _) => St::Operand,
            (St::Term, TokenKind::RParen, _) if depth > 0 => {
                depth -= 1;
                St::Term
            }
            _ => return Position::Invalid,
        };
    }
    match st {
        St::Operand => Position::Column,
        St::Column(column, span) => {
            on_term(Term {
                column: column.clone(),
                column_span: span,
                op: None,
            });
            Position::Operator { column }
        }
        St::In(column) => Position::OpenList { column },
        St::Op(column) => Position::Value {
            column,
            listed: Vec::new(),
        },
        St::List(column, listed) => Position::Value { column, listed },
        St::ListValue(..) => Position::Connective {
            open_parens: depth,
            in_list: true,
        },
        St::Term => Position::Connective {
            open_parens: depth,
            in_list: false,
        },
    }
}

/// What may come next at byte `caret` of `text` (clamped to its length),
/// and the token a suggestion replaces. The clamped caret must lie on a
/// UTF-8 character boundary.
pub fn context_at(text: &str, caret: usize) -> Context {
    let caret = caret.min(text.len());
    let tokens = lex(text);
    let partial = tokens
        .iter()
        .find(|t| t.span.start < caret && caret <= t.span.end && is_partial(t, caret));
    let token = partial.map_or(caret..caret, |t| t.span.clone());
    let typed = match partial {
        Some(
            t @ Token {
                kind: TokenKind::Str { .. },
                ..
            },
        ) => text[t.span.start + 1..caret].replace("''", "'"),
        Some(t) => text[t.span.start..caret].to_string(),
        None => String::new(),
    };
    let before: Vec<Token> = tokens
        .into_iter()
        .take_while(|t| t.span.end <= token.start)
        .collect();
    let position = walk(text, &before, &mut |_| {});
    Context {
        position,
        token,
        typed,
    }
}

use crate::dimensions::DerivedDimensions;
use crate::schema::{ColumnRole, ColumnType, SchemaSpec};

/// What a column can hold, as far as suggestions care.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueKind {
    /// Text with a dictionary: values are listed from the data.
    Categorical,
    /// Text without one (keys, free text): typed, not listed.
    Text,
    Number,
    Bool,
    Date,
    Timestamp,
    /// A derived dimension's labels, sorted.
    Derived(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VocabColumn {
    pub name: String,
    /// `dimension`, `key`, `measure`, `attribute`, `axis`, `value` or `derived`.
    pub role: &'static str,
    pub kind: ValueKind,
}

impl VocabColumn {
    /// The row detail: role and type, or `derived`.
    pub fn detail(&self) -> String {
        let ty = match self.kind {
            ValueKind::Derived(_) => return "derived".to_string(),
            ValueKind::Categorical | ValueKind::Text => "text",
            ValueKind::Number => "number",
            ValueKind::Bool => "bool",
            ValueKind::Date => "date",
            ValueKind::Timestamp => "timestamp",
        };
        format!("{} · {ty}", self.role)
    }
}

/// Every column an expression may name, across all datasets, then every
/// derived dimension. The first dataset to declare a column decides its
/// role and kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExprVocab {
    columns: Vec<VocabColumn>,
}

impl ExprVocab {
    pub fn new(schema: &SchemaSpec, dims: &DerivedDimensions) -> Self {
        let mut columns: Vec<VocabColumn> = Vec::new();
        for dataset in &schema.datasets {
            for c in &dataset.columns {
                if columns.iter().any(|v| v.name == c.name) {
                    continue;
                }
                let role = match c.role {
                    ColumnRole::Key => "key",
                    ColumnRole::Dimension { .. } => "dimension",
                    ColumnRole::Measure { .. } => "measure",
                    ColumnRole::Attribute { .. } => "attribute",
                    ColumnRole::Axis => "axis",
                    ColumnRole::Value => "value",
                };
                let kind = match c.ty {
                    ColumnType::Utf8 if c.categorical => ValueKind::Categorical,
                    ColumnType::Utf8 => ValueKind::Text,
                    ColumnType::F64 | ColumnType::I64 => ValueKind::Number,
                    ColumnType::Bool => ValueKind::Bool,
                    ColumnType::Date => ValueKind::Date,
                    ColumnType::Timestamp => ValueKind::Timestamp,
                };
                columns.push(VocabColumn {
                    name: c.name.clone(),
                    role,
                    kind,
                });
            }
        }
        for d in dims.all() {
            if columns.iter().any(|v| v.name == d.name) {
                continue;
            }
            let labels: std::collections::BTreeSet<&String> = d.values.values().collect();
            columns.push(VocabColumn {
                name: d.name.clone(),
                role: "derived",
                kind: ValueKind::Derived(labels.into_iter().cloned().collect()),
            });
        }
        Self { columns }
    }

    pub fn columns(&self) -> &[VocabColumn] {
        &self.columns
    }

    pub fn get(&self, name: &str) -> Option<&VocabColumn> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The closest column name within two edits, ignoring case. On a tie,
    /// the earliest column wins.
    pub fn nearest(&self, name: &str) -> Option<&str> {
        let name = name.to_lowercase();
        self.columns
            .iter()
            .map(|c| {
                (
                    edit_distance(&name, &c.name.to_lowercase()),
                    c.name.as_str(),
                )
            })
            .filter(|(d, _)| *d <= 2)
            .min_by_key(|(d, _)| *d)
            .map(|(_, n)| n)
    }
}

/// Levenshtein distance over chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A schema problem in expression text, with the byte range it concerns.
#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    pub span: Range<usize>,
    pub message: String,
}

/// Schema problems in `text`, in text order: unknown columns (with a
/// did-you-mean suggestion) and ordering or `like` on a derived dimension.
/// With `caret`, a problem whose range touches the caret is left out,
/// because that word is still being typed. An empty vocab checks nothing.
/// The walk stops where the text stops being a valid prefix, so a syntax
/// error hides later warnings. Callers must parse separately before using
/// these warnings to accept or refuse a complete expression.
pub fn check(text: &str, vocab: &ExprVocab, caret: Option<usize>) -> Vec<Warning> {
    if vocab.is_empty() {
        return Vec::new();
    }
    let tokens = lex(text);
    let mut out = Vec::new();
    walk(
        text,
        &tokens,
        &mut |term: Term| match vocab.get(&term.column) {
            None => out.push(Warning {
                span: term.column_span.clone(),
                message: match vocab.nearest(&term.column) {
                    Some(near) => {
                        format!("unknown column '{}'; did you mean '{near}'?", term.column)
                    }
                    None => format!("unknown column '{}'", term.column),
                },
            }),
            Some(VocabColumn {
                kind: ValueKind::Derived(_),
                ..
            }) => {
                if let Some((op, span)) = &term.op
                    && let Some(message) = crate::scope::derived_op_error(&term.column, op)
                {
                    out.push(Warning {
                        span: span.clone(),
                        message,
                    });
                }
            }
            Some(_) => {}
        },
    );
    out.retain(|w| caret.is_none_or(|c| !(w.span.start <= c && c <= w.span.end)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::parse_expr;

    fn ctx(text: &str, caret: usize) -> (Position, std::ops::Range<usize>, String) {
        let c = context_at(text, caret);
        (c.position, c.token, c.typed)
    }

    fn col(c: &str) -> String {
        c.to_string()
    }

    #[test]
    fn empty_and_operand_positions_want_a_column() {
        assert_eq!(ctx("", 0), (Position::Column, 0..0, String::new()));
        assert_eq!(ctx("bo", 2), (Position::Column, 0..2, "bo".into()));
        assert_eq!(ctx("not (", 5).0, Position::Column);
        assert_eq!(ctx("a = 1 and ", 10).0, Position::Column);
    }

    #[test]
    fn after_a_column_an_operator() {
        assert_eq!(
            ctx("book ", 5),
            (
                Position::Operator {
                    column: col("book")
                },
                5..5,
                String::new()
            )
        );
        assert_eq!(
            ctx("book l", 6),
            (
                Position::Operator {
                    column: col("book")
                },
                5..6,
                "l".into()
            )
        );
        assert_eq!(
            ctx("book in ", 8).0,
            Position::OpenList {
                column: col("book")
            }
        );
    }

    #[test]
    fn after_an_operator_or_inside_a_list_a_value() {
        let value = |listed: &[&str]| Position::Value {
            column: col("book"),
            listed: listed.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(ctx("book =", 6), (value(&[]), 6..6, String::new()));
        assert_eq!(ctx("book = 'E", 9), (value(&[]), 7..9, "E".into()));
        assert_eq!(ctx("book = EM", 9), (value(&[]), 7..9, "EM".into()));
        assert_eq!(ctx("book in (", 9).0, value(&[]));
        assert_eq!(ctx("book in ('A', ", 14).0, value(&["A"]));
        assert_eq!(ctx("book = 'O''N", 12).2, "O'N");
    }

    #[test]
    fn a_finished_term_wants_a_connective() {
        let conn = |open_parens, in_list| Position::Connective {
            open_parens,
            in_list,
        };
        assert_eq!(
            ctx("book = 'E'", 10).0,
            conn(0, false),
            "a closed string is complete"
        );
        assert_eq!(ctx("book = 'E' ", 11).0, conn(0, false));
        assert_eq!(ctx("(a = 1 ", 7).0, conn(1, false));
        assert_eq!(ctx("book in ('A' ", 13).0, conn(0, true));
        assert_eq!(ctx("a = 1 an", 8), (conn(0, false), 6..8, "an".into()));
    }

    #[test]
    fn a_caret_mid_word_replaces_the_whole_word_but_ranks_the_part_before_it() {
        assert_eq!(
            ctx("book = 'EMEA' and region = 'X'", 3),
            (Position::Column, 0..4, "boo".into())
        );
    }

    #[test]
    fn text_that_cannot_continue_is_invalid() {
        assert_eq!(ctx("a ! ", 4).0, Position::Invalid);
        assert_eq!(
            ctx("a = 1 )", 7).0,
            Position::Invalid,
            "no open paren to close"
        );
    }

    #[test]
    fn multibyte_text_keeps_every_range_on_char_boundaries() {
        let text = "city = 'Zürich' and né";
        for caret in 0..=text.len() {
            if !text.is_char_boundary(caret) {
                continue;
            }
            let c = context_at(text, caret);
            assert!(
                text.is_char_boundary(c.token.start),
                "start at caret {caret}"
            );
            assert!(text.is_char_boundary(c.token.end), "end at caret {caret}");
        }
        assert_eq!(
            ctx(text, text.len()),
            (Position::Column, 21..text.len(), "né".into())
        );
    }

    /// Every prefix of a valid expression, followed by a space, must be read
    /// the way the parser reads it. Where the parser stops at the end of the
    /// input wanting X, `context_at` must say X.
    #[test]
    fn context_agrees_with_the_parser_on_every_prefix() {
        let corpus = [
            "book = 'EQ-DERIV'",
            "book = 'EQ-DERIV' and region = 'EMEA'",
            "not (a = 1 or b != 2) and c in ('x', 'y''z')",
            "delta01 >= -1.5",
            "flag = true or flag = false",
            "name like 'sp%'",
            "(a = 1)",
            "x <> 3 and not y <= 4",
            "a in (1, 2, 3)",
        ];
        for full in corpus {
            for end in 0..=full.len() {
                if !full.is_char_boundary(end) {
                    continue;
                }
                let text = format!("{} ", &full[..end]);
                let caret = text.len();
                let Err(e) = parse_expr(&text) else {
                    assert!(
                        matches!(
                            context_at(&text, caret).position,
                            Position::Connective { in_list: false, .. }
                        ),
                        "{text:?} parses, so a connective must come next"
                    );
                    continue;
                };
                if e.caret != caret {
                    continue; // the parser stopped before the end
                }
                let got = context_at(&text, caret).position;
                let ok = match e.message.as_str() {
                    "expected a column name" => got == Position::Column,
                    "expected a comparison operator" => matches!(got, Position::Operator { .. }),
                    "expected a value" => matches!(got, Position::Value { .. }),
                    "expected '(' after 'in'" => matches!(got, Position::OpenList { .. }),
                    "expected ',' or ')'" => {
                        matches!(got, Position::Connective { in_list: true, .. })
                    }
                    "expected ')'" => matches!(
                        got,
                        Position::Connective {
                            open_parens: 1..,
                            in_list: false
                        }
                    ),
                    _ => true,
                };
                assert!(
                    ok,
                    "{text:?}: parser says {:?}, context says {got:?}",
                    e.message
                );
            }
        }
    }

    use crate::config::{LayerDoc, merge_docs};
    use crate::dimensions::DerivedDimensions;
    use crate::schema::SchemaSpec;

    // `live` and `expiry` are position-grain attributes. Bare dimensions
    // must be built-in grain-key columns; the attribute role keeps these
    // fixture columns valid with their declared bool and date kinds.
    fn vocab() -> ExprVocab {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
             [risk.columns.live]\ntype = \"bool\"\nrole = \"attribute\"\ngrain = \"position\"\n\
             [risk.columns.expiry]\ntype = \"date\"\nrole = \"attribute\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let dims = LayerDoc::builtin(
            "dimensions",
            "[desk]\nfrom = \"book\"\n[desk.values]\nEQ = [\"BK000\"]\nRATES = [\"BK001\", \"BK002\"]\n",
        )
        .unwrap();
        let (schema, diags) = SchemaSpec::from_doc(&merge_docs("datasets", &[datasets]));
        assert!(
            diags
                .iter()
                .all(|d| d.severity != crate::config::Severity::Error),
            "the fixture must not lose columns to a schema error: {diags:?}"
        );
        let (dims, _) = DerivedDimensions::from_doc(&merge_docs("dimensions", &[dims]));
        ExprVocab::new(&schema, &dims)
    }

    #[test]
    fn the_vocab_reads_role_and_kind_from_the_schema_then_derived_labels() {
        let v = vocab();
        let names: Vec<&str> = v.columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["book", "position_ref", "npv", "live", "expiry", "desk"]
        );
        assert_eq!(v.get("book").unwrap().kind, ValueKind::Categorical);
        assert_eq!(
            v.get("position_ref").unwrap().kind,
            ValueKind::Text,
            "keys have no dictionary"
        );
        assert_eq!(v.get("npv").unwrap().detail(), "measure · number");
        assert_eq!(v.get("live").unwrap().kind, ValueKind::Bool);
        assert_eq!(v.get("expiry").unwrap().kind, ValueKind::Date);
        assert_eq!(
            v.get("desk").unwrap().kind,
            ValueKind::Derived(vec!["EQ".into(), "RATES".into()])
        );
        assert_eq!(v.get("desk").unwrap().detail(), "derived");
    }

    #[test]
    fn did_you_mean_proposes_only_within_two_edits() {
        let v = vocab();
        assert_eq!(v.nearest("bokk"), Some("book"));
        assert_eq!(v.nearest("BOOK"), Some("book"));
        assert_eq!(v.nearest("zzzzzz"), None);
    }

    #[test]
    fn check_flags_unknown_columns_and_derived_ordering() {
        let v = vocab();
        let w = check("bokk = 'A' and desk < 'EQ'", &v, None);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].span, 0..4);
        assert_eq!(w[0].message, "unknown column 'bokk'; did you mean 'book'?");
        assert!(
            w[1].message
                .starts_with("'desk' is a derived dimension, so '<'"),
            "{}",
            w[1].message
        );
        assert!(check("book = 'A' and desk in ('EQ')", &v, None).is_empty());
    }

    #[test]
    fn check_stays_quiet_about_the_word_under_the_caret() {
        let v = vocab();
        assert!(check("bok", &v, Some(3)).is_empty(), "still typing");
        assert_eq!(
            check("bok = 'A'", &v, Some(9)).len(),
            1,
            "caret has left it"
        );
    }

    #[test]
    fn an_empty_vocab_checks_nothing() {
        assert!(check("anything = 1", &ExprVocab::default(), None).is_empty());
    }
}
