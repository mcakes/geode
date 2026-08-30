# Phase 2b — Query Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the stored, grain-split data of Phase 2a into scope-framed,
grouped, joined snapshots on screen — and prove the <50ms requery contract.

**Architecture:** `geode-core` gains the pure, window-free vocabulary the
query path is expressed in: the scope model with its expression parser, view
definitions, the attribution rules, and the `Snapshot` type. `geode-data`
gains the compiler that turns a view plus a scope into one `ROLLUP` query per
tree, the query pool that runs it cancellably, and as-of routing at the
archive. `geode-shell` gains a deliberately throwaway debug tile, because the
§7.1 requery budget is specified end-to-end through a painted frame and
cannot be measured headless.

**Tech Stack:** Rust 2024 edition, stable toolchain. `duckdb` 1.10505
(`bundled`, `chrono`) — already a dependency. Arrow comes through duckdb-rs
and stays an implementation detail. `criterion` 0.8.2. No async runtime: the
query pool is OS threads and channels, delivered into gpui's executor.

**Spec:** `docs/superpowers/specs/2026-08-30-geode-phase-2-data-design.md`
(sections §6, §7, §9.3). Phase 2a is `2026-08-30-phase-2a-storage-and-ingest.md`.

## Global Constraints

- **Layering:** `geode-data` never depends on `geode-shell` and vice versa;
  the scope *value type* therefore lives in `geode-core` (spec §6.2).
  `geode-data` is the only crate permitted to open a file or socket.
- **Every new lib/bin target needs `bench = false`**; every `[[bench]]`
  target needs `harness = false`.
- **CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace`, and `cargo bench --workspace
  --no-run` on both macOS and Windows.**
- **Values are bound as parameters, never spliced into SQL text** (spec
  §6.2). duckdb-rs cannot bind a list, verified against 1.10505 — dimension
  selections use a connection-local temp table plus semi-join.
- **`query_arrow` yields 2048-row batches, not one** (spec §6.6). A
  column-wide `&[f64]` requires concatenation.
- **Nothing in this plan runs on the UI thread** except reading a prepared
  `Snapshot`. The query pool owns its own connections.
- **Grain vocabulary is fixed** (spec §3.3): `K = (book, lhu, position_ref,
  counterparty)`; `Instrument = K + instrument_ref`; `Underlying = K +
  instrument_ref + underlying_ref`; `UnderlyingPair = K + instrument_ref +
  underlying_ref + underlying2_ref`.
- **Reserved column names** (`geode_core::schema::RESERVED_COLUMNS`):
  `batch`, `source_file_id`, `gen_id`, `source_time`.

## What Phase 2a already provides

Do not rebuild these; the plan consumes them as-is.

- `geode_core::schema::{SchemaSpec, DatasetSpec, ColumnSpec, ColumnRole,
  ColumnType, Aggregate, Grain, RESERVED_COLUMNS}` —
  `DatasetSpec::{column, grains, measures_at, attributes_at,
  textual_columns}`, `Grain::{key_columns, table, parse, ALL}`.
- `geode_data::store::{Store, StoreError, Catalog, FileGeneration, FileId}`
  — `Store::{open, writer, reader, apply_schema}`,
  `Catalog::{book_freshness, dataset_as_of, live_source_time,
  lookup_by_path, reserve_file_id, next_gen_id, record}`.
- `geode_data::store::ddl::{table_name, TableKind, create_table_sql}` —
  live and archive have **identical columns**, including `gen_id` and
  `source_time`.
- `geode_data::ingest::{load_file, build_plan, IngestRunner, IngestHandle,
  IngestEvent}`; `geode_data::source::{discover, SourceSpec, Sentinel}`.
- `geode_demo_data::{generate, GeneratorConfig, emit_directory, EmitOptions,
  EmittedDirectory, EmittedFile, RiskBatch}` — scales to any requested row
  count.

## Carried over from Phase 2a

Two items 2a deliberately deferred, scheduled here:

1. **ENUM dictionary encoding** (spec §3.6, §6.6). Dimension columns are
   stored as plain `VARCHAR` today, so they return `StringArray` rather than
   `Dictionary(UInt8, Utf8)` and §7.2's "interned at ingest" does not hold.
   2a deferred it because nothing there read a snapshot. Task 8.
2. **Cross-file `instrument_ref` conflict detection.** 2a detects
   disagreement *within* a file; the same instrument arriving with different
   attributes from two books' files is undetected. Task 14.

## File Structure

**`geode-core`** — pure, window-free, no I/O:

- `src/scope/mod.rs` — `Scope`, `DimensionSelection`, composition.
- `src/scope/expr.rs` — the expression AST, `parse_expr`, `ParseError`.
- `src/view.rs` — `ViewSpec`, `JoinSpec`, `ViewColumn`, `SortKey`.
- `src/hierarchy.rs` — declared containment; `identity_columns`.
- `src/attribution.rs` — `Attribution`, `ScopeSemantics`, the rules.
- `src/snapshot.rs` — `Snapshot`, typed column accessors, provenance.

**`geode-data`** — the compiler and the pool:

- `src/query/mod.rs` — re-exports, `QueryRequest`.
- `src/query/scope_sql.rs` — scope → predicates, temp-table semi-joins.
- `src/query/compile.rs` — view + scope + grouping → one `ROLLUP` statement.
- `src/query/pool.rs` — read connections, cancellation, latest-wins.
- `src/query/as_of.rs` — archive routing and per-partition resolution.
- `src/service.rs` — `DataService`: the single door modules use.
- `benches/query.rs` — the §7.1 requery benchmarks.

**`geode-shell`** — the throwaway slice:

- `src/dataprobe.rs` — debug tile. Deleted when the blotter lands.

---

### Task 1: The scope model and its expression parser

**Files:**
- Create: `crates/geode-core/src/scope/mod.rs`
- Create: `crates/geode-core/src/scope/expr.rs`
- Modify: `crates/geode-core/src/lib.rs`

**Interfaces:**
- Consumes: `geode_core::schema::{DatasetSpec, ColumnType}`,
  `geode_core::config::Diagnostic`.
- Produces: `Scope { dimensions: Vec<DimensionSelection>, text:
  Option<String>, expression: Option<Expr> }`, `DimensionSelection { column:
  String, values: Vec<String> }`, `Scope::{is_empty, and_then, columns}`,
  `Expr`, `CompareOp`, `Literal`, `ParseError { message, caret }`,
  `parse_expr(&str) -> Result<Expr, ParseError>`,
  `Expr::{columns, validate}`. Task 4 compiles these to SQL.

**Why the parser lives in core** (spec §6.2): scope *state* is the shell's,
the scope *compiler* is the data layer's, and those two crates may never
depend on each other — so the value type and its parser sit below both. It
is also the natural home for §10.3's property tests.

**This is a restricted WHERE grammar, not SQL.** Comparisons, `in`, `and`,
`or`, `not`, parentheses. No subqueries, no function calls, no arithmetic.
Anything else is a parse error with a caret, reported at the point of entry
(spec §10.1: never a dialog for a typo).

- [ ] **Step 1: Write the failing parser tests**

Create `crates/geode-core/src/scope/expr.rs` with only this test module:

```rust
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
        // spec §4.1's worked example.
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
    fn unknown_columns_are_caught_by_validation_not_parsing() {
        // Parsing is schema-free; validation needs the dataset.
        let e = parse("nonesuch = 'A'");
        assert_eq!(e.columns(), vec!["nonesuch"]);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-core expr`
Expected: FAIL — `cannot find function parse_expr`.

- [ ] **Step 3: Implement the AST and a recursive-descent parser**

Prepend to `crates/geode-core/src/scope/expr.rs`:

```rust
//! The expression filter's grammar (spec §4.1, §6.2): a restricted WHERE
//! clause, parsed and validated against the schema — **not** raw SQL.
//!
//! The grammar has no statement separator, no comment syntax, no function
//! calls and no subqueries, so hostile input fails at the parser rather
//! than reaching the database. Literals still bind as parameters (§6.2);
//! the grammar is defence in depth, not the defence.
//!
//! Precedence, loosest first: `or`, `and`, `not`, comparison.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Byte offset the caret points at, for inline reporting (spec §10.1).
    pub caret: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at offset {})", self.message, self.caret)
    }
}

impl std::error::Error for ParseError {}

pub fn parse_expr(input: &str) -> Result<Expr, ParseError> {
    let mut p = Parser {
        src: input,
        pos: 0,
    };
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
            let start = self.pos;
            loop {
                match self.rest().chars().next() {
                    Some('\'') => {
                        let text = self.src[start..self.pos].to_string();
                        self.pos += 1;
                        return Ok(Literal::Str(text));
                    }
                    Some(c) => self.pos += c.len_utf8(),
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
```

- [ ] **Step 4: Run to verify the parser tests pass**

Run: `cargo test -p geode-core expr`
Expected: PASS (9 tests).

- [ ] **Step 5: Write the failing scope tests**

Create `crates/geode-core/src/scope/mod.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;

    fn dataset() -> crate::schema::DatasetSpec {
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
textual = true
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0.dataset("risk").unwrap().clone()
    }

    #[test]
    fn an_empty_scope_selects_everything() {
        assert!(Scope::default().is_empty());
        assert!(Scope::default().columns().is_empty());
    }

    #[test]
    fn layers_compose_by_conjunction() {
        // global AND workspace AND tile (spec §4.2).
        let global = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        let tile = Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        };
        let effective = global.and_then(&tile);
        assert_eq!(effective.dimensions.len(), 1);
        assert_eq!(effective.text.as_deref(), Some("SPX"));
    }

    #[test]
    fn composing_the_same_dimension_intersects_its_values() {
        // Narrowing twice must narrow, never widen.
        let a = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into(), "BK001".into()],
            }],
            ..Scope::default()
        };
        let b = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK001".into(), "BK002".into()],
            }],
            ..Scope::default()
        };
        let e = a.and_then(&b);
        assert_eq!(e.dimensions.len(), 1);
        assert_eq!(e.dimensions[0].values, vec!["BK001".to_string()]);
    }

    #[test]
    fn columns_lists_every_dimension_the_scope_touches() {
        let s = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            expression: Some(crate::scope::parse_expr("lhu = 'L0'").unwrap()),
            text: None,
        };
        let mut cols = s.columns();
        cols.sort_unstable();
        assert_eq!(cols, vec!["book".to_string(), "lhu".to_string()]);
    }

    #[test]
    fn validation_rejects_unknown_columns_with_a_diagnostic() {
        let s = Scope {
            expression: Some(crate::scope::parse_expr("nonesuch = 'x'").unwrap()),
            ..Scope::default()
        };
        let diags = s.validate(&dataset());
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("nonesuch"), "{}", diags[0].message);
    }

    #[test]
    fn validation_accepts_a_well_formed_scope() {
        let s = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            text: Some("SPX".into()),
            expression: Some(crate::scope::parse_expr("delta01 > 100").unwrap()),
        };
        assert!(s.validate(&dataset()).is_empty());
    }

    #[test]
    fn the_text_filter_targets_only_declared_textual_columns() {
        // spec §4.1: matched against columns declared textual, not all.
        let ds = dataset();
        let textual: Vec<&str> = ds.textual_columns().map(|c| c.name.as_str()).collect();
        assert_eq!(textual, vec!["book", "underlying_ref"]);
    }
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p geode-core scope`
Expected: FAIL — `cannot find struct Scope`.

- [ ] **Step 7: Implement `Scope`**

Prepend to `crates/geode-core/src/scope/mod.rs`:

```rust
//! What every tile is looking at (spec §4.1). Three predicate kinds
//! composed with AND: dimension selections, a text filter, and a validated
//! expression.
//!
//! This is the *value*. Scope state lives in the shell and the compiler
//! lives in the data layer, and those crates may never depend on each
//! other — so the type they share sits below both (spec §6.2).

pub mod expr;

pub use expr::{CompareOp, Expr, Literal, ParseError, parse_expr};

use crate::config::{Diagnostic, Severity};
use crate::schema::DatasetSpec;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DimensionSelection {
    pub column: String,
    /// Empty means "no constraint", not "match nothing" — an empty
    /// selection is dropped during composition rather than emitting a
    /// predicate that excludes every row.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope {
    pub dimensions: Vec<DimensionSelection>,
    /// Matched case-insensitively against columns declared textual.
    pub text: Option<String>,
    pub expression: Option<Expr>,
}

impl Scope {
    pub fn is_empty(&self) -> bool {
        self.dimensions.iter().all(|d| d.values.is_empty())
            && self.text.is_none()
            && self.expression.is_none()
    }

    /// Compose two layers (spec §4.2: global AND workspace AND tile).
    /// Selections on the same dimension **intersect**: narrowing twice
    /// narrows, and a layer can never widen what a coarser layer allowed.
    pub fn and_then(&self, inner: &Scope) -> Scope {
        let mut dimensions = self.dimensions.clone();
        for sel in &inner.dimensions {
            if sel.values.is_empty() {
                continue;
            }
            match dimensions.iter_mut().find(|d| d.column == sel.column) {
                Some(existing) => {
                    existing.values.retain(|v| sel.values.contains(v));
                }
                None => dimensions.push(sel.clone()),
            }
        }
        dimensions.retain(|d| !d.values.is_empty());

        Scope {
            dimensions,
            text: inner.text.clone().or_else(|| self.text.clone()),
            expression: match (&self.expression, &inner.expression) {
                (Some(a), Some(b)) => Some(Expr::And(Box::new(a.clone()), Box::new(b.clone()))),
                (Some(a), None) => Some(a.clone()),
                (None, b) => b.clone(),
            },
        }
    }

    /// Every column the scope constrains. The text filter is excluded: it
    /// targets whatever the schema declares textual, not a named column.
    pub fn columns(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .dimensions
            .iter()
            .filter(|d| !d.values.is_empty())
            .map(|d| d.column.clone())
            .collect();
        if let Some(e) = &self.expression {
            out.extend(e.columns().into_iter().map(str::to_string));
        }
        out
    }

    /// Columns must exist in the dataset. Failures are Diagnostics, never
    /// panics — a bad scope is a user error reported at the point of entry
    /// (spec §10.1).
    pub fn validate(&self, ds: &DatasetSpec) -> Vec<Diagnostic> {
        self.columns()
            .into_iter()
            .filter(|c| ds.column(c).is_none())
            .map(|c| Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("scope references unknown column '{c}'"),
            })
            .collect()
    }
}
```

Add to `crates/geode-core/src/lib.rs`:

```rust
pub mod scope;
```

- [ ] **Step 8: Run to verify it passes**

Run: `cargo test -p geode-core scope`
Expected: PASS (7 tests).

- [ ] **Step 9: Lint, format, commit**

Run: `cargo fmt && cargo clippy -p geode-core --all-targets -- -D warnings && cargo test -p geode-core`

```bash
git add crates/geode-core
git commit -m "feat(core): scope model and restricted expression grammar

The scope value type and its parser live in core because scope state is
the shell's and the compiler is the data layer's, and those crates may
never depend on each other (spec §6.2).

The grammar is a restricted WHERE clause — comparisons, in, and/or/not,
parentheses — with no statement separator, comment syntax, function call
or subquery, so injection-shaped input fails at the parser rather than
reaching the database. Literals still bind as parameters; the grammar is
defence in depth, not the defence.

Layer composition intersects selections on the same dimension, so a
narrower layer can never widen what a coarser one allowed."
```

---

**Remaining tasks (2–14) continue below.** Task 2 adds view definitions;
Task 3 the declared hierarchy and the attribution rules; Tasks 4–6 the scope
compiler, the grain-aware `ROLLUP` compiler, and joins; Task 7 the `Snapshot`
type; Task 8 the ENUM interning carried over from 2a; Tasks 9–10 the query
pool with cancellation and as-of routing; Task 11 the `DataService` facade;
Task 12 the throwaway debug tile; Task 13 the §7.1 requery benchmarks; Task
14 cross-file conflict detection.
