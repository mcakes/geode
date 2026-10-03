//! Expression-filter grammar: column comparisons, membership, boolean
//! operators, and parentheses. Parsing is schema-free; `Scope::validate`
//! checks references separately. The data compiler binds literals as query
//! parameters. Statements, comments, function calls, and subqueries are not
//! part of this grammar.
//!
//! Precedence, loosest first: `or`, `and`, `not`, comparison.

/// The words the grammar reads as keywords, matched case-insensitively. The
/// completer lexes by this list, and a classification may not take one as its
/// name: `not = 'X'` would read as negation, not as a column.
pub(crate) const KEYWORDS: [&str; 7] = ["and", "or", "not", "in", "like", "true", "false"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
}

impl CompareOp {
    pub fn sql(self) -> &'static str {
        match self {
            CompareOp::Eq => "=",
            CompareOp::Ne => "<>",
            CompareOp::Lt => "<",
            CompareOp::Le => "<=",
            CompareOp::Gt => ">",
            CompareOp::Ge => ">=",
            CompareOp::Like => "ilike",
        }
    }
}

/// The refusal for a comparison operator's grammar text on a derived
/// dimension, or `None` for `=`, `!=`, or `<>`. Derived dimensions map
/// labels to members, so ordering and pattern comparisons are unsupported.
/// `in` is handled separately by the grammar. Scope validation and live
/// expression checks share this rule.
pub fn derived_op_error(column: &str, op: &str) -> Option<String> {
    (!matches!(op, "=" | "!=" | "<>")).then(|| {
        format!(
            "'{column}' is a derived dimension, so '{op}' has no meaning on it; use =, != or in"
        )
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Str(String),
    Num(f64),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Compare {
        column: String,
        op: CompareOp,
        value: Literal,
    },
    In {
        column: String,
        values: Vec<Literal>,
    },
}

impl Expr {
    /// Every column the expression mentions, in traversal order and with
    /// duplicates retained — callers that want a set say so.
    pub fn columns(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.walk_columns(&mut out);
        out
    }

    /// Visit every `Compare` node as `(column, op)`.
    ///
    /// `In` is deliberately not visited: membership is meaningful on a
    /// derived dimension, which is the only caller's whole question.
    pub fn for_each_comparison(&self, f: &mut impl FnMut(&str, CompareOp)) {
        match self {
            Expr::And(a, b) | Expr::Or(a, b) => {
                a.for_each_comparison(f);
                b.for_each_comparison(f);
            }
            Expr::Not(e) => e.for_each_comparison(f),
            Expr::Compare { column, op, .. } => f(column, *op),
            Expr::In { .. } => {}
        }
    }

    /// The expression's top-level conjuncts, left to right: every `And`
    /// on either side is flattened, and anything else (`Or`, `Not`, a
    /// comparison, a membership) is one term. `a and (b and c)` and
    /// `(a and b) and c` both give `[a, b, c]`; `a or b` gives itself.
    /// The toolbar paints one chip per term and edits them one at a time,
    /// so a term must be exactly what [`Expr::from_conjuncts`] can put
    /// back without changing which rows match.
    pub fn conjuncts(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        self.walk_conjuncts(&mut out);
        out
    }

    fn walk_conjuncts<'a>(&'a self, out: &mut Vec<&'a Expr>) {
        match self {
            Expr::And(a, b) => {
                a.walk_conjuncts(out);
                b.walk_conjuncts(out);
            }
            other => out.push(other),
        }
    }

    /// A left-folded `And` chain over `terms` in order (`((a and b) and
    /// c)`), or `None` for no terms. The inverse of [`Expr::conjuncts`]
    /// up to `And` association, which does not change the rows matched.
    pub fn from_conjuncts(terms: impl IntoIterator<Item = Expr>) -> Option<Expr> {
        terms
            .into_iter()
            .reduce(|acc, term| Expr::And(Box::new(acc), Box::new(term)))
    }

    fn walk_columns<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Expr::And(a, b) | Expr::Or(a, b) => {
                a.walk_columns(out);
                b.walk_columns(out);
            }
            Expr::Not(e) => e.walk_columns(out),
            Expr::Compare { column, .. } | Expr::In { column, .. } => out.push(column),
        }
    }
}

impl CompareOp {
    /// The grammar's own spelling — distinct from [`CompareOp::sql`], which
    /// is the SQL text the compiler emits (`Ne` as `<>`, `Like` as
    /// `ilike`). `parse_op` accepts both `!=` and `<>` for `Ne`; either
    /// round-trips, so `Display` just picks one. Visible to `scope::mod`,
    /// whose `validate` reports refusals in this spelling too.
    pub(crate) fn grammar(self) -> &'static str {
        match self {
            CompareOp::Eq => "=",
            CompareOp::Ne => "!=",
            CompareOp::Lt => "<",
            CompareOp::Le => "<=",
            CompareOp::Gt => ">",
            CompareOp::Ge => ">=",
            CompareOp::Like => "like",
        }
    }
}

impl std::fmt::Display for Literal {
    /// Strings are single-quoted with internal quotes doubled, matching
    /// `parse_literal`. Numbers use `f64` display syntax.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Literal::Str(s) => {
                write!(f, "'")?;
                for c in s.chars() {
                    if c == '\'' {
                        write!(f, "''")?;
                    } else {
                        write!(f, "{c}")?;
                    }
                }
                write!(f, "'")
            }
            Literal::Num(n) => write!(f, "{n}"),
            Literal::Bool(b) => write!(f, "{b}"),
        }
    }
}

impl std::fmt::Display for Expr {
    /// Grammar text such that `parse_expr(e.to_string()) == e` for every
    /// `Expr` this parser can produce. Every `And`/`Or`/`Not` operand is
    /// fully parenthesised regardless of whether precedence would already
    /// disambiguate it — the round trip is what matters here, not
    /// brevity, and parentheses are the one construct that can never be
    /// misread by the parser above.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expr::And(a, b) => write!(f, "({a}) and ({b})"),
            Expr::Or(a, b) => write!(f, "({a}) or ({b})"),
            Expr::Not(e) => write!(f, "not ({e})"),
            Expr::Compare { column, op, value } => {
                write!(f, "{column} {} {value}", op.grammar())
            }
            Expr::In { column, values } => {
                write!(f, "{column} in (")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Byte offset for the caret in an inline error display.
    pub caret: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at offset {})", self.message, self.caret)
    }
}

impl std::error::Error for ParseError {}

pub fn parse_expr(input: &str) -> Result<Expr, ParseError> {
    let mut p = Parser { src: input, pos: 0 };
    p.skip_ws();
    let e = p.parse_or()?;
    p.skip_ws();
    if p.pos < p.src.len() {
        return Err(p.err("unexpected trailing input"));
    }
    Ok(e)
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, message: &str) -> ParseError {
        ParseError {
            message: message.to_string(),
            caret: self.pos,
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// Consume `word` case-insensitively when it appears as a whole word.
    fn eat_keyword(&mut self, word: &str) -> bool {
        self.skip_ws();
        let rest = self.rest();
        if rest.len() < word.len() || !rest[..word.len()].eq_ignore_ascii_case(word) {
            return false;
        }
        let after = rest[word.len()..].chars().next();
        if after.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return false;
        }
        self.pos += word.len();
        true
    }

    fn eat_char(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.rest().starts_with(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while self.eat_keyword("or") {
            let rhs = self.parse_and()?;
            lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_not()?;
        while self.eat_keyword("and") {
            let rhs = self.parse_not()?;
            lhs = Expr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if self.eat_keyword("not") {
            return Ok(Expr::Not(Box::new(self.parse_not()?)));
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        if self.eat_char('(') {
            let e = self.parse_or()?;
            if !self.eat_char(')') {
                return Err(self.err("expected ')'"));
            }
            return Ok(e);
        }
        let column = self.parse_identifier()?;

        if self.eat_keyword("in") {
            if !self.eat_char('(') {
                return Err(self.err("expected '(' after 'in'"));
            }
            let mut values = Vec::new();
            loop {
                values.push(self.parse_literal()?);
                if self.eat_char(',') {
                    continue;
                }
                if self.eat_char(')') {
                    break;
                }
                return Err(self.err("expected ',' or ')'"));
            }
            return Ok(Expr::In { column, values });
        }

        let op = self.parse_op()?;
        let value = self.parse_literal()?;
        Ok(Expr::Compare { column, op, value })
    }

    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        let start = self.pos;
        while let Some(c) = self.rest().chars().next() {
            if c.is_alphanumeric() || c == '_' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err(self.err("expected a column name"));
        }
        Ok(self.src[start..self.pos].to_string())
    }

    fn parse_op(&mut self) -> Result<CompareOp, ParseError> {
        if self.eat_keyword("like") {
            return Ok(CompareOp::Like);
        }
        self.skip_ws();
        // Longest match first: `<=` before `<`.
        for (text, op) in [
            ("!=", CompareOp::Ne),
            ("<>", CompareOp::Ne),
            ("<=", CompareOp::Le),
            (">=", CompareOp::Ge),
            ("=", CompareOp::Eq),
            ("<", CompareOp::Lt),
            (">", CompareOp::Gt),
        ] {
            if self.rest().starts_with(text) {
                self.pos += text.len();
                // `==` is not an operator here; catch it rather than
                // parsing `=` and failing confusingly on the next token.
                if self.rest().starts_with('=') {
                    return Err(self.err("unknown operator"));
                }
                return Ok(op);
            }
        }
        Err(self.err("expected a comparison operator"))
    }

    fn parse_literal(&mut self) -> Result<Literal, ParseError> {
        self.skip_ws();
        if self.eat_keyword("true") {
            return Ok(Literal::Bool(true));
        }
        if self.eat_keyword("false") {
            return Ok(Literal::Bool(false));
        }
        if self.rest().starts_with('\'') {
            self.pos += 1;
            // Doubled quotes represent one literal quote. Build an owned string
            // so the result contains decoded characters rather than raw escapes.
            let mut text = String::new();
            loop {
                match self.rest().chars().next() {
                    Some('\'') => {
                        self.pos += 1;
                        if self.rest().starts_with('\'') {
                            text.push('\'');
                            self.pos += 1;
                            continue;
                        }
                        return Ok(Literal::Str(text));
                    }
                    Some(c) => {
                        text.push(c);
                        self.pos += c.len_utf8();
                    }
                    None => return Err(self.err("unterminated string")),
                }
            }
        }
        let start = self.pos;
        while let Some(c) = self.rest().chars().next() {
            if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err(self.err("expected a value"));
        }
        self.src[start..self.pos]
            .parse::<f64>()
            .map(Literal::Num)
            .map_err(|_| ParseError {
                message: "not a number".into(),
                caret: start,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Expr {
        parse_expr(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn parses_a_simple_comparison() {
        assert_eq!(
            parse("model_code = 'EURP'"),
            Expr::Compare {
                column: "model_code".into(),
                op: CompareOp::Eq,
                value: Literal::Str("EURP".into()),
            }
        );
    }

    #[test]
    fn parses_the_specs_own_example() {
        // Comparisons, membership, and grouping in one expression.
        let e = parse("model_code = 'EURP' and underlying_ref = 'SPX'");
        assert!(matches!(e, Expr::And(_, _)));
        let mut cols = e.columns();
        cols.sort_unstable();
        assert_eq!(cols, vec!["model_code", "underlying_ref"]);
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // a or b and c  ==  a or (b and c)
        let e = parse("book = 'A' or book = 'B' and lhu = 'L'");
        match e {
            Expr::Or(_, rhs) => assert!(matches!(*rhs, Expr::And(_, _))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parentheses_override_precedence() {
        let e = parse("(book = 'A' or book = 'B') and lhu = 'L'");
        match e {
            Expr::And(lhs, _) => assert!(matches!(*lhs, Expr::Or(_, _))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_not_and_in_and_numbers() {
        assert!(matches!(parse("not book = 'A'"), Expr::Not(_)));
        assert_eq!(
            parse("book in ('A', 'B')"),
            Expr::In {
                column: "book".into(),
                values: vec![Literal::Str("A".into()), Literal::Str("B".into())],
            }
        );
        assert_eq!(
            parse("delta01 > 1000"),
            Expr::Compare {
                column: "delta01".into(),
                op: CompareOp::Gt,
                value: Literal::Num(1000.0),
            }
        );
    }

    #[test]
    fn every_comparison_operator_round_trips() {
        for (text, op) in [
            ("=", CompareOp::Eq),
            ("!=", CompareOp::Ne),
            ("<", CompareOp::Lt),
            ("<=", CompareOp::Le),
            (">", CompareOp::Gt),
            (">=", CompareOp::Ge),
            ("like", CompareOp::Like),
        ] {
            assert_eq!(
                parse(&format!("book {text} 'A'")),
                Expr::Compare {
                    column: "book".into(),
                    op,
                    value: Literal::Str("A".into()),
                },
                "operator {text}"
            );
        }
    }

    #[test]
    fn errors_carry_a_caret_at_the_offending_position() {
        let e = parse_expr("book = ").unwrap_err();
        assert_eq!(e.caret, 7, "caret points past the operator: {e}");

        let e = parse_expr("book == 'A'").unwrap_err();
        assert!(e.caret >= 5, "{e}");
    }

    #[test]
    fn sql_injection_shaped_input_is_a_parse_error_not_a_query() {
        // The grammar has no statement separator, no comment, no subquery.
        for hostile in [
            "book = 'A'; drop table measures_position_live",
            "book = 'A' -- comment",
            "book = (select 1)",
            "book = version()",
        ] {
            assert!(parse_expr(hostile).is_err(), "accepted: {hostile}");
        }
    }

    #[test]
    fn rendering_an_expression_round_trips_through_the_parser() {
        for src in [
            "model_code = 'EURP'",
            "model_code = 'EURP' and underlying_ref = 'SPX'",
            "not (book = 'BK001' or lhu = 'X')",
            "npv > 100.5",
            "strike <= -3",
            "book in ('A', 'B')",
            "name like 'sp%'", // the grammar's own spelling of Like
            "flag = true",
            "note = 'its'",
        ] {
            let e = parse_expr(src).unwrap_or_else(|err| panic!("{src}: {err}"));
            let rendered = e.to_string();
            let again = parse_expr(&rendered).unwrap_or_else(|err| panic!("{rendered}: {err}"));
            assert_eq!(e, again, "{src} -> {rendered}");
        }
    }

    #[test]
    fn a_quote_inside_a_string_literal_escapes_as_a_doubled_quote_and_round_trips() {
        // Quoted strings round-trip through doubled internal quotes.
        let e = parse_expr("book = 'O''Neil'").unwrap();
        assert_eq!(
            e,
            Expr::Compare {
                column: "book".into(),
                op: CompareOp::Eq,
                value: Literal::Str("O'Neil".into()),
            }
        );
        let rendered = e.to_string();
        assert_eq!(rendered, "book = 'O''Neil'");
        let again = parse_expr(&rendered).unwrap_or_else(|err| panic!("{rendered}: {err}"));
        assert_eq!(e, again, "{rendered} must round-trip");
    }

    #[test]
    fn unknown_columns_are_caught_by_validation_not_parsing() {
        // Parsing is schema-free; validation needs the dataset.
        let e = parse("nonesuch = 'A'");
        assert_eq!(e.columns(), vec!["nonesuch"]);
    }

    fn conjunct_texts(e: &Expr) -> Vec<String> {
        e.conjuncts().iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn conjuncts_split_a_chain_and_rebuild_it() {
        let e = parse("a = 1 and b = 2 and c = 3");
        assert_eq!(conjunct_texts(&e), vec!["a = 1", "b = 2", "c = 3"]);
        let rebuilt = Expr::from_conjuncts(e.conjuncts().into_iter().cloned()).unwrap();
        assert_eq!(rebuilt, e, "a left-folded chain round-trips exactly");
    }

    #[test]
    fn a_single_term_is_one_conjunct() {
        let e = parse("a = 1 or b = 2");
        assert_eq!(e.conjuncts(), vec![&e], "an or is one term");
        let n = parse("not a = 1");
        assert_eq!(n.conjuncts(), vec![&n], "a not is one term");
        assert_eq!(Expr::from_conjuncts([e.clone()]), Some(e));
        assert_eq!(
            Expr::from_conjuncts(Vec::new()),
            None,
            "no terms, no expression"
        );
    }

    #[test]
    fn nested_ands_flatten_on_both_sides_in_order() {
        let right = parse("a = 1 and (b = 2 and (c = 3 or d = 4))");
        assert_eq!(
            conjunct_texts(&right),
            vec!["a = 1", "b = 2", "(c = 3) or (d = 4)"]
        );
        let left = parse("(a = 1 and b = 2) and c = 3");
        assert_eq!(conjunct_texts(&left), vec!["a = 1", "b = 2", "c = 3"]);
    }

    #[test]
    fn dropping_a_term_keeps_the_others_in_order() {
        let e = parse("a = 1 and b = 2 and c = 3");
        let drop = |i: usize| {
            let terms = e
                .conjuncts()
                .into_iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, t)| t.clone());
            Expr::from_conjuncts(terms).map(|x| conjunct_texts(&x))
        };
        assert_eq!(drop(0), Some(vec!["b = 2".into(), "c = 3".into()]));
        assert_eq!(drop(1), Some(vec!["a = 1".into(), "c = 3".into()]));
        assert_eq!(drop(2), Some(vec!["a = 1".into(), "b = 2".into()]));
        let one = parse("a = 1");
        assert_eq!(
            Expr::from_conjuncts(one.conjuncts().into_iter().skip(1).cloned()),
            None,
            "dropping the only term leaves no expression"
        );
    }
}
