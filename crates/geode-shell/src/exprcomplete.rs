//! Suggestion state for a scope expression field. It is pure: the gpui
//! controller in `shell::expr_suggest` feeds it the field's text and
//! caret and paints what it holds.
//!
//! [`ExprCompletion::refresh`] derives context from the text and caret through
//! [`context_at`], then rebuilds rows, hints and warnings when either changes.
//! Rows use the shared fuzzy matcher and are capped at [`MAX_ROWS`]. Categorical
//! values come from asynchronous distinct queries and remain cached per column
//! for this completion state. Only a reply matching the column's current
//! loading tag is accepted. Superseded loading entries are discarded so returning
//! to their columns can request values again; ready and failed entries remain.

use std::collections::HashMap;
use std::ops::Range;

use geode_core::scope::complete::{Context, ExprVocab, Position, ValueKind, check, context_at};

use crate::listfilter;
use crate::vimnav::{self, NavCommand};

/// Maximum ranked rows retained. Categorical-value hints report the full cached count.
pub const MAX_ROWS: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub enum Values {
    Loading { tag: u64 },
    Ready(Vec<(String, u64)>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// What the row shows and what ranking matches against.
    pub label: String,
    /// What accepting the row writes over the token.
    pub insert: String,
    /// Right-aligned detail: a role and type, a count, or what an operator does.
    pub detail: String,
    /// Matched char offsets within `label`.
    pub indices: Vec<usize>,
}

/// One accepted row: `text` over the byte `range` of the line it was
/// computed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub range: Range<usize>,
    pub text: String,
}

impl Write {
    /// The line with the write applied, and the caret after it.
    pub fn apply(&self, line: &str) -> (String, usize) {
        crate::commandline::accept(line, self.range.clone(), &self.text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// Same text and caret as last time: nothing to repaint.
    Unchanged,
    Changed,
    /// Changed, and `column`'s values should be requested now.
    Request(String),
}

#[derive(Debug, Default)]
pub struct ExprCompletion {
    last: Option<(String, usize)>,
    context: Option<Context>,
    rows: Vec<Row>,
    candidates: usize,
    highlighted: usize,
    hint: String,
    warning: Option<String>,
    values: HashMap<String, Values>,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn op_detail(op: &str) -> &'static str {
    match op {
        "=" => "equals",
        "!=" => "differs from",
        "<" => "less than",
        "<=" => "at most",
        ">" => "greater than",
        ">=" => "at least",
        "in" => "one of a list",
        "like" => "contains text",
        _ => "",
    }
}

fn operators(kind: Option<&ValueKind>) -> &'static [&'static str] {
    match kind {
        Some(ValueKind::Categorical | ValueKind::Text) => &["=", "!=", "in", "like"],
        Some(ValueKind::Number | ValueKind::Date | ValueKind::Timestamp) => {
            &["=", "!=", "<", "<=", ">", ">=", "in"]
        }
        Some(ValueKind::Bool) => &["=", "!="],
        Some(ValueKind::Derived(_)) => &["=", "!=", "in"],
        None => &["=", "!=", "<", "<=", ">", ">=", "in", "like"],
    }
}

impl ExprCompletion {
    pub fn refresh(&mut self, text: &str, caret: usize, vocab: &ExprVocab) -> Refresh {
        if self
            .last
            .as_ref()
            .is_some_and(|(t, c)| t == text && *c == caret)
        {
            return Refresh::Unchanged;
        }
        self.last = Some((text.to_string(), caret));
        self.highlighted = 0;
        self.rebuild(vocab);
        match &self.context {
            Some(Context {
                position: Position::Value { column, .. },
                ..
            }) if matches!(
                vocab.get(column).map(|c| &c.kind),
                Some(ValueKind::Categorical)
            ) && !self.values.contains_key(column) =>
            {
                Refresh::Request(column.clone())
            }
            _ => Refresh::Changed,
        }
    }

    /// Track the latest request and discard other columns' loading entries.
    /// All columns share one pool key, so superseded work may never reply.
    /// Keeping its loading entry would prevent a later visit from requesting
    /// values again. Ready and failed entries remain cached.
    pub fn mark_loading(&mut self, column: &str, tag: u64, vocab: &ExprVocab) {
        self.values
            .retain(|c, v| c == column || !matches!(v, Values::Loading { .. }));
        self.values
            .insert(column.to_string(), Values::Loading { tag });
        self.rebuild(vocab);
    }

    pub fn deliver(
        &mut self,
        column: &str,
        tag: u64,
        values: Result<Vec<(String, u64)>, String>,
        vocab: &ExprVocab,
    ) -> bool {
        if self.values.get(column) != Some(&Values::Loading { tag }) {
            return false;
        }
        let state = match values {
            Ok(v) => Values::Ready(v),
            Err(e) => Values::Failed(e),
        };
        self.values.insert(column.to_string(), state);
        self.rebuild(vocab);
        true
    }

    /// Rebuild the context, rows, hint and warning from the last text and
    /// caret. The highlight is kept, clamped to the new rows.
    pub fn rebuild(&mut self, vocab: &ExprVocab) {
        let Some((text, caret)) = self.last.clone() else {
            return;
        };
        let context = context_at(&text, caret);
        let candidates = self.candidates(&context, vocab);
        let texts: Vec<String> = candidates.iter().map(|r| r.label.clone()).collect();
        self.candidates = candidates.len();
        self.rows = listfilter::rank(&texts, &context.typed)
            .into_iter()
            .take(MAX_ROWS)
            .map(|r| Row {
                indices: r.indices,
                ..candidates[r.row].clone()
            })
            .collect();
        self.highlighted = self.highlighted.min(self.rows.len().saturating_sub(1));
        self.hint = self.hint_for(&context, vocab);
        self.warning = check(&text, vocab, Some(caret))
            .into_iter()
            .next()
            .map(|w| w.message);
        self.context = Some(context);
    }

    fn candidates(&self, context: &Context, vocab: &ExprVocab) -> Vec<Row> {
        let row = |label: &str, insert: String, detail: &str| Row {
            label: label.to_string(),
            insert,
            detail: detail.to_string(),
            indices: Vec::new(),
        };
        match &context.position {
            Position::Column => {
                let mut rows: Vec<Row> = vocab
                    .columns()
                    .iter()
                    .map(|c| row(&c.name, format!("{} ", c.name), &c.detail()))
                    .collect();
                rows.push(row("not", "not ".into(), "negate what follows"));
                rows.push(row("(", "(".into(), "start a group"));
                rows
            }
            Position::Operator { column } => operators(vocab.get(column).map(|c| &c.kind))
                .iter()
                .map(|op| {
                    let insert = if *op == "in" {
                        "in (".to_string()
                    } else {
                        format!("{op} ")
                    };
                    row(op, insert, op_detail(op))
                })
                .collect(),
            Position::OpenList { .. } => vec![row("(", "(".into(), "start the list")],
            Position::Value { column, listed } => {
                let unlisted = |v: &&str| !listed.iter().any(|l| l == v);
                match vocab.get(column).map(|c| &c.kind) {
                    Some(ValueKind::Categorical) => match self.values.get(column) {
                        Some(Values::Ready(values)) => values
                            .iter()
                            .filter(|(v, _)| unlisted(&v.as_str()))
                            .map(|(v, n)| row(&quote(v), quote(v), &n.to_string()))
                            .collect(),
                        _ => Vec::new(),
                    },
                    Some(ValueKind::Derived(labels)) => labels
                        .iter()
                        .map(String::as_str)
                        .filter(unlisted)
                        .map(|l| row(&quote(l), quote(l), "label"))
                        .collect(),
                    Some(ValueKind::Bool) => ["true", "false"]
                        .into_iter()
                        .filter(unlisted)
                        .map(|b| row(b, b.to_string(), ""))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            Position::Connective { in_list: true, .. } => vec![
                row(",", ", ".into(), "another value"),
                row(")", ") ".into(), "end the list"),
            ],
            Position::Connective {
                open_parens,
                in_list: false,
            } => {
                let mut rows = vec![
                    row("and", "and ".into(), "both must hold"),
                    row("or", "or ".into(), "either may hold"),
                ];
                if *open_parens > 0 {
                    rows.push(row(")", ") ".into(), "close the group"));
                }
                rows
            }
            Position::Invalid => Vec::new(),
        }
    }

    fn hint_for(&self, context: &Context, vocab: &ExprVocab) -> String {
        match &context.position {
            Position::Column => "column".into(),
            Position::Operator { column } => format!("operator for {column}"),
            Position::OpenList { column } => format!("( starts the list for {column}"),
            Position::Value { column, .. } => {
                let tail = match vocab.get(column).map(|c| &c.kind) {
                    None => return format!("value for {column}"),
                    Some(ValueKind::Categorical) => match self.values.get(column) {
                        Some(Values::Loading { .. }) | None => "loading values…".to_string(),
                        Some(Values::Ready(v)) => format!("{} values", v.len()),
                        Some(Values::Failed(e)) => format!("values unavailable: {e}"),
                    },
                    Some(ValueKind::Text) => "text in quotes, e.g. 'ABC'".into(),
                    Some(ValueKind::Number) => "a number, e.g. 1000".into(),
                    Some(ValueKind::Bool) => "true or false".into(),
                    Some(ValueKind::Date | ValueKind::Timestamp) => {
                        "a date in quotes, e.g. '2026-09-26'".into()
                    }
                    Some(ValueKind::Derived(l)) => format!("{} labels", l.len()),
                };
                format!("value for {column} · {tail}")
            }
            Position::Connective { in_list: true, .. } => ", adds a value · ) ends the list".into(),
            Position::Connective { .. } => "and / or, or enter to apply".into(),
            Position::Invalid => "this can't continue; enter says what is wrong".into(),
        }
    }

    pub fn step(&mut self, delta: i64) {
        self.highlighted =
            vimnav::apply(self.highlighted, self.rows.len(), NavCommand::Move(delta));
    }

    /// The write for ranked row `i`, or `None` when there is no such row
    /// or the cached range no longer fits the last text.
    pub fn accept(&self, i: usize) -> Option<Write> {
        let row = self.rows.get(i)?;
        let range = self.context.as_ref()?.token.clone();
        let (text, _) = self.last.as_ref()?;
        let fits = range.start <= range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);
        fits.then(|| Write {
            range,
            text: row.insert.clone(),
        })
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    pub fn hint(&self) -> &str {
        &self.hint
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// There were candidates, and the typed text matched none of them.
    pub fn no_matches(&self) -> bool {
        self.rows.is_empty() && self.candidates > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::schema::SchemaSpec;

    fn vocab() -> ExprVocab {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
             [risk.columns.live]\ntype = \"bool\"\nrole = \"attribute\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let (schema, _) = SchemaSpec::from_doc(&merge_docs("datasets", &[datasets]));
        ExprVocab::new(&schema, &DerivedDimensions::default())
    }

    fn labels(c: &ExprCompletion) -> Vec<&str> {
        c.rows().iter().map(|r| r.label.as_str()).collect()
    }

    #[test]
    fn an_empty_field_lists_columns_then_not_and_paren() {
        let mut c = ExprCompletion::default();
        assert_eq!(c.refresh("", 0, &vocab()), Refresh::Changed);
        assert_eq!(labels(&c), ["book", "npv", "live", "not", "("]);
        assert_eq!(c.rows()[1].detail, "measure · number");
        assert_eq!(c.hint(), "column");
        assert_eq!(
            c.refresh("", 0, &vocab()),
            Refresh::Unchanged,
            "same text and caret"
        );
    }

    #[test]
    fn operators_follow_the_column_type() {
        let mut c = ExprCompletion::default();
        c.refresh("npv ", 4, &vocab());
        assert_eq!(labels(&c), ["=", "!=", "<", "<=", ">", ">=", "in"]);
        c.refresh("live ", 5, &vocab());
        assert_eq!(labels(&c), ["=", "!="]);
        c.refresh("book ", 5, &vocab());
        assert_eq!(labels(&c), ["=", "!=", "in", "like"]);
        assert_eq!(c.hint(), "operator for book");
    }

    #[test]
    fn a_categorical_value_position_requests_once_and_lists_after_delivery() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        assert_eq!(c.refresh("book = ", 7, &v), Refresh::Request("book".into()));
        c.mark_loading("book", 7, &v);
        assert_eq!(c.hint(), "value for book · loading values…");
        assert_eq!(
            c.refresh("book = '", 8, &v),
            Refresh::Changed,
            "no second request"
        );
        assert!(
            !c.deliver("book", 6, Ok(vec![("X".into(), 1)]), &v),
            "stale tag dropped"
        );
        assert!(c.deliver(
            "book",
            7,
            Ok(vec![("EMEA".into(), 12), ("O'Neil".into(), 3)]),
            &v
        ));
        assert_eq!(labels(&c), ["'EMEA'", "'O''Neil'"]);
        assert_eq!(c.rows()[0].detail, "12");
        assert_eq!(c.hint(), "value for book · 2 values");
    }

    #[test]
    fn a_second_columns_request_forgets_the_first_so_it_is_asked_again() {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.region]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\n\
             [risk.columns.desk]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let (schema, diagnostics) = SchemaSpec::from_doc(&merge_docs("datasets", &[datasets]));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let v = ExprVocab::new(&schema, &DerivedDimensions::default());
        let mut c = ExprCompletion::default();
        c.refresh("desk = ", 7, &v);
        c.mark_loading("desk", 1, &v);
        assert!(c.deliver("desk", 1, Ok(vec![("D1".into(), 1)]), &v));
        assert_eq!(c.refresh("book = ", 7, &v), Refresh::Request("book".into()));
        c.mark_loading("book", 2, &v);
        let text = "book = 'A' and region = ";
        assert_eq!(
            c.refresh(text, text.len(), &v),
            Refresh::Request("region".into())
        );
        c.mark_loading("region", 3, &v);
        assert!(
            !c.deliver("book", 2, Ok(vec![("A".into(), 1)]), &v),
            "the pool dropped book's request when region's replaced it"
        );
        assert_eq!(
            c.refresh("book = ", 7, &v),
            Refresh::Request("book".into()),
            "returning to book asks again"
        );
        assert_eq!(
            c.refresh("desk = ", 7, &v),
            Refresh::Changed,
            "a delivered column is kept"
        );
    }

    #[test]
    fn a_failed_request_says_why() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book = ", 7, &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Err("pool busy".into()), &v);
        assert_eq!(c.hint(), "value for book · values unavailable: pool busy");
        assert!(c.rows().is_empty());
    }

    #[test]
    fn listed_values_are_left_out() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book in ('A', ", 14, &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Ok(vec![("A".into(), 1), ("B".into(), 2)]), &v);
        assert_eq!(labels(&c), ["'B'"]);
    }

    #[test]
    fn accept_writes_over_the_token_with_its_insert_text() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("bo", 2, &v);
        let w = c.accept(0).unwrap();
        assert_eq!(w.apply("bo"), ("book ".to_string(), 5));
        c.refresh("book i", 6, &v);
        assert_eq!(labels(&c)[0], "in");
        assert_eq!(
            c.accept(0).unwrap().apply("book i"),
            ("book in (".to_string(), 9)
        );
    }

    #[test]
    fn a_value_with_a_quote_is_escaped_when_inserted() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book = 'O", 9, &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Ok(vec![("O'Neil".into(), 3)]), &v);
        assert_eq!(
            c.accept(0).unwrap().apply("book = 'O"),
            ("book = 'O''Neil'".to_string(), 16)
        );
    }

    #[test]
    fn a_multibyte_value_writes_on_char_boundaries() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        let text = "book = 'Zü";
        c.refresh(text, text.len(), &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Ok(vec![("Zürich".into(), 1)]), &v);
        assert_eq!(c.accept(0).unwrap().apply(text).0, "book = 'Zürich'");
    }

    #[test]
    fn connectives_after_a_term_and_close_paren_only_when_open() {
        let mut c = ExprCompletion::default();
        c.refresh("npv > 1 ", 8, &vocab());
        assert_eq!(labels(&c), ["and", "or"]);
        c.refresh("(npv > 1 ", 9, &vocab());
        assert_eq!(labels(&c), ["and", "or", ")"]);
        assert_eq!(c.hint(), "and / or, or enter to apply");
    }

    #[test]
    fn typed_text_ranks_and_no_matches_is_reported() {
        let mut c = ExprCompletion::default();
        c.refresh("np", 2, &vocab());
        assert_eq!(labels(&c)[0], "npv");
        c.refresh("zz", 2, &vocab());
        assert!(c.rows().is_empty());
        assert!(c.no_matches());
    }

    #[test]
    fn step_moves_and_clamps_the_highlight() {
        let mut c = ExprCompletion::default();
        c.refresh("", 0, &vocab());
        c.step(1);
        assert_eq!(c.highlighted(), 1);
        c.step(-5);
        assert_eq!(c.highlighted(), 0);
    }

    #[test]
    fn the_warning_is_the_first_schema_problem_off_the_caret() {
        let mut c = ExprCompletion::default();
        c.refresh("bokk = 'A' ", 11, &vocab());
        assert_eq!(
            c.warning(),
            Some("unknown column 'bokk'; did you mean 'book'?")
        );
    }

    #[test]
    fn rows_are_capped() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book = ", 7, &v);
        c.mark_loading("book", 1, &v);
        let many: Vec<(String, u64)> = (0..500).map(|i| (format!("V{i:03}"), 1)).collect();
        c.deliver("book", 1, Ok(many), &v);
        assert_eq!(c.rows().len(), MAX_ROWS);
        assert_eq!(c.hint(), "value for book · 500 values");
    }
}
