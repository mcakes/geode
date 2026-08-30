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

### Task 2: View definitions and derived dimensions

**Files:**
- Create: `crates/geode-core/src/view.rs`
- Create: `crates/geode-core/src/dimensions.rs`
- Modify: `crates/geode-core/src/lib.rs`
- Modify: `crates/geode-core/src/config/merge.rs` (atomic doc list)

**Interfaces:**
- Consumes: `SchemaSpec`, `DatasetSpec`, `MergedDoc`, `Diagnostic`.
- Produces: `ViewSpec { name, dataset, joins, columns, grouping, sort }`,
  `JoinSpec { dataset, on }`, `ViewColumn::{Dimension, Measure, Derived}`,
  `SortKey { column, descending }`, `ViewSpec::from_doc(&MergedDoc) ->
  (Vec<ViewSpec>, Vec<Diagnostic>)`, `ViewSpec::validate(&SchemaSpec)`;
  and `DerivedDimension { name, from, values }`, `DerivedDimensions`,
  `DerivedDimensions::{from_doc, get, base_column, all}`. Tasks 3–6
  consume both.

**Why derived dimensions live beside views** (spec §6.8): `desk` is not in
the source files. The config map that supplies it also declares `book →
desk`, and that dependency is what makes grouping by desk *additive* in
Task 3 — without it the rule sees a grouping column outside every grain key
and blanks the top row of the most natural rollup.

- [ ] **Step 1: Write the failing derived-dimension tests**

Create `crates/geode-core/src/dimensions.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("dimensions", &[LayerDoc::builtin("dimensions", text).unwrap()])
    }

    const SAMPLE: &str = r#"
[desk]
from = "book"
[desk.values]
IDX_EXO_EU = ["BK000", "BK001"]
IDX_EXO_US = ["BK003"]
"#;

    #[test]
    fn parses_a_many_to_one_map() {
        let (dims, diags) = DerivedDimensions::from_doc(&doc(SAMPLE));
        assert!(diags.is_empty(), "{diags:?}");
        let desk = dims.get("desk").expect("desk");
        assert_eq!(desk.from, "book");
        assert_eq!(desk.values.get("BK000").map(String::as_str), Some("IDX_EXO_EU"));
        assert_eq!(desk.values.get("BK003").map(String::as_str), Some("IDX_EXO_US"));
    }

    #[test]
    fn base_column_resolves_a_derived_dimension_to_its_source() {
        let (dims, _) = DerivedDimensions::from_doc(&doc(SAMPLE));
        // This is the functional dependency the attribution rule needs.
        assert_eq!(dims.base_column("desk"), "book");
        // A column that is not derived resolves to itself.
        assert_eq!(dims.base_column("book"), "book");
        assert_eq!(dims.base_column("lhu"), "lhu");
    }

    #[test]
    fn a_book_mapped_to_two_desks_is_a_diagnostic() {
        // Many-to-one, not many-to-many: otherwise `book` would not
        // determine `desk` and grouping by desk could not be additive.
        let (_dims, diags) = DerivedDimensions::from_doc(&doc(
            "[desk]\nfrom = \"book\"\n[desk.values]\nA = [\"BK000\"]\nB = [\"BK000\"]\n",
        ));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("BK000"), "{}", diags[0].message);
    }

    #[test]
    fn a_missing_from_clause_is_a_diagnostic() {
        let (dims, diags) = DerivedDimensions::from_doc(&doc(
            "[desk]\n[desk.values]\nA = [\"BK000\"]\n",
        ));
        assert!(dims.get("desk").is_none());
        assert!(diags.iter().any(|d| d.message.contains("from")), "{diags:?}");
    }

    #[test]
    fn an_empty_document_yields_no_dimensions() {
        let (dims, diags) = DerivedDimensions::from_doc(&doc(""));
        assert!(dims.all().next().is_none());
        assert!(diags.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-core dimensions`
Expected: FAIL — `cannot find struct DerivedDimensions`.

- [ ] **Step 3: Implement derived dimensions**

Prepend to `crates/geode-core/src/dimensions.rs`:

```rust
//! Dimensions the desk groups by that are not in the source files
//! (spec §6.8). `desk` is the standing case: the CSVs carry `book`, and
//! which desk a book belongs to is desk knowledge kept in config.
//!
//! The `from` clause does two jobs. It says where the values come from,
//! and it declares a functional dependency — `book` determines `desk` —
//! which is what lets the attribution rule treat a desk-level rollup as
//! additive rather than blanking it (§6.3).

use crate::config::{Diagnostic, MergedDoc, Severity};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedDimension {
    pub name: String,
    /// The column this is computed from. Must be a real dataset column.
    pub from: String,
    /// Source value -> derived value. Many-to-one by construction.
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct DerivedDimensions {
    dims: Vec<DerivedDimension>,
}

impl DerivedDimensions {
    pub fn get(&self, name: &str) -> Option<&DerivedDimension> {
        self.dims.iter().find(|d| d.name == name)
    }

    pub fn all(&self) -> impl Iterator<Item = &DerivedDimension> {
        self.dims.iter()
    }

    /// The column a grouping or scope column ultimately resolves to: a
    /// derived dimension resolves to its source, anything else to itself.
    /// The attribution rule (§6.3) compares base columns, which is how a
    /// desk-level grouping counts as determined by `book`.
    pub fn base_column<'a>(&'a self, column: &'a str) -> &'a str {
        match self.get(column) {
            Some(d) => &d.from,
            None => column,
        }
    }

    pub fn from_doc(doc: &MergedDoc) -> (DerivedDimensions, Vec<Diagnostic>) {
        let mut out = DerivedDimensions::default();
        let mut diags = Vec::new();

        for (name, value) in &doc.value {
            let bad = |m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("dimension '{name}': {m}"),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("not a table".into()));
                continue;
            };
            let Some(from) = table.get("from").and_then(|v| v.as_str()) else {
                diags.push(bad("missing 'from'".into()));
                continue;
            };

            let mut values: BTreeMap<String, String> = BTreeMap::new();
            if let Some(map) = table.get("values").and_then(|v| v.as_table()) {
                for (derived_value, sources) in map {
                    let Some(list) = sources.as_array() else {
                        diags.push(bad(format!("'{derived_value}' is not an array")));
                        continue;
                    };
                    for source in list.iter().filter_map(|s| s.as_str()) {
                        if let Some(existing) = values.get(source)
                            && existing != derived_value
                        {
                            // Many-to-many would break the functional
                            // dependency the additivity rule rests on.
                            diags.push(bad(format!(
                                "'{source}' is mapped to both '{existing}' and \
                                 '{derived_value}'; the map must be many-to-one"
                            )));
                            continue;
                        }
                        values.insert(source.to_string(), derived_value.clone());
                    }
                }
            }

            out.dims.push(DerivedDimension {
                name: name.clone(),
                from: from.to_string(),
                values,
            });
        }

        (out, diags)
    }
}
```

Add to `crates/geode-core/src/lib.rs`:

```rust
pub mod dimensions;
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-core dimensions`
Expected: PASS (5 tests).

- [ ] **Step 5: Write the failing view tests**

Create `crates/geode-core/src/view.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()])
    }

    const SAMPLE: &str = r#"
[desk_risk]
dataset = "risk_snapshot"
grouping = ["book", "lhu", "position_ref"]

[[desk_risk.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]

[[desk_risk.columns]]
name = "book"
kind = "dimension"

[[desk_risk.columns]]
name = "delta01"
kind = "measure"

[[desk_risk.columns]]
name = "delta_per_vega"
kind = "derived"
sql = "delta01 / nullif(vega01, 0)"

[[desk_risk.sort]]
column = "delta01"
descending = true
"#;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.vega01]
type = "f64"
role = "measure"
grain = "underlying"
[instrument_ref.columns.instrument_ref]
type = "utf8"
role = "key"
[instrument_ref.columns.strike]
type = "f64"
role = "attribute"
grain = "instrument"
"#;
        let d = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&d).0
    }

    #[test]
    fn parses_a_view_with_joins_columns_grouping_and_sort() {
        let (views, diags) = ViewSpec::from_doc(&doc(SAMPLE));
        assert!(diags.is_empty(), "{diags:?}");
        let v = views.iter().find(|v| v.name == "desk_risk").expect("view");
        assert_eq!(v.dataset, "risk_snapshot");
        assert_eq!(v.grouping, vec!["book", "lhu", "position_ref"]);
        assert_eq!(v.joins.len(), 1);
        assert_eq!(v.joins[0].dataset, "instrument_ref");
        assert_eq!(v.joins[0].on, vec!["instrument_ref"]);
        assert_eq!(v.sort, vec![SortKey { column: "delta01".into(), descending: true }]);
    }

    #[test]
    fn distinguishes_the_three_column_kinds() {
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let v = &views[0];
        assert_eq!(
            v.columns,
            vec![
                ViewColumn::Dimension { name: "book".into() },
                ViewColumn::Measure { name: "delta01".into() },
                ViewColumn::Derived {
                    name: "delta_per_vega".into(),
                    sql: "delta01 / nullif(vega01, 0)".into(),
                },
            ]
        );
    }

    #[test]
    fn a_derived_column_without_sql_is_a_diagnostic() {
        let (_v, diags) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"d\"\n[[v.columns]]\nname = \"x\"\nkind = \"derived\"\n",
        ));
        assert!(diags.iter().any(|d| d.message.contains("sql")), "{diags:?}");
    }

    #[test]
    fn validation_catches_unknown_columns_grouping_and_datasets() {
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"nosuch\"\ngrouping = [\"nocolumn\"]\n",
        ));
        let diags = views[0].validate(&schema());
        assert!(diags.iter().any(|d| d.message.contains("nosuch")), "{diags:?}");
    }

    #[test]
    fn a_well_formed_view_validates_clean() {
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let diags = views[0].validate(&schema());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn measure_grains_reports_the_distinct_grains_the_view_touches() {
        // The compiler builds one aggregate subquery per grain (§6.3), so
        // this is what decides how many it emits.
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let ds = schema();
        assert_eq!(
            views[0].measure_grains(&ds),
            vec![crate::schema::Grain::Underlying]
        );
    }
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p geode-core view`
Expected: FAIL — `cannot find struct ViewSpec`.

- [ ] **Step 7: Implement `ViewSpec`**

Prepend to `crates/geode-core/src/view.rs`:

```rust
//! View definitions (spec §5.1, §6.1): dataset, joins, columns, derived
//! columns, grouping and sort, declared as config. Users create views
//! through the UI or by writing config; both produce the same file
//! (PHILOSOPHY §5).
//!
//! A view is data, not code — it names columns and expressions, and the
//! compiler (geode-data) turns it into one statement.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::schema::{ColumnRole, Grain, SchemaSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinSpec {
    pub dataset: String,
    /// Join key columns, declared in schema config (spec §5.4).
    pub on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewColumn {
    Dimension { name: String },
    Measure { name: String },
    /// A SQL expression over other columns of the same view.
    Derived { name: String, sql: String },
}

impl ViewColumn {
    pub fn name(&self) -> &str {
        match self {
            ViewColumn::Dimension { name }
            | ViewColumn::Measure { name }
            | ViewColumn::Derived { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub column: String,
    pub descending: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ViewSpec {
    pub name: String,
    pub dataset: String,
    pub joins: Vec<JoinSpec>,
    pub columns: Vec<ViewColumn>,
    /// Ordered: each prefix is one level of the rollup tree (§6.3).
    pub grouping: Vec<String>,
    pub sort: Vec<SortKey>,
}

impl ViewSpec {
    /// Distinct grains of the measures this view selects, coarse first.
    /// The compiler emits one aggregate subquery per grain.
    pub fn measure_grains(&self, schema: &SchemaSpec) -> Vec<Grain> {
        let Some(ds) = schema.dataset(&self.dataset) else {
            return Vec::new();
        };
        let mut out: Vec<Grain> = self
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Measure { name } => ds.column(name),
                _ => None,
            })
            .filter(|c| matches!(c.role, ColumnRole::Measure { .. }))
            .filter_map(|c| c.grain())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn validate(&self, schema: &SchemaSpec) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        let bad = |m: String| Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!("view '{}': {m}", self.name),
        };

        let Some(ds) = schema.dataset(&self.dataset) else {
            diags.push(bad(format!("unknown dataset '{}'", self.dataset)));
            return diags;
        };

        for j in &self.joins {
            if schema.dataset(&j.dataset).is_none() {
                diags.push(bad(format!("join names unknown dataset '{}'", j.dataset)));
            }
        }

        // A derived column may reference other view columns, so only
        // dimension and measure columns are checked against the schema.
        for c in &self.columns {
            match c {
                ViewColumn::Derived { .. } => {}
                other => {
                    if ds.column(other.name()).is_none()
                        && !self.joins.iter().any(|j| {
                            schema
                                .dataset(&j.dataset)
                                .is_some_and(|d| d.column(other.name()).is_some())
                        })
                    {
                        diags.push(bad(format!("unknown column '{}'", other.name())));
                    }
                }
            }
        }

        for g in &self.grouping {
            if ds.column(g).is_none() {
                diags.push(bad(format!("grouping names unknown column '{g}'")));
            }
        }

        diags
    }

    pub fn from_doc(doc: &MergedDoc) -> (Vec<ViewSpec>, Vec<Diagnostic>) {
        let mut out = Vec::new();
        let mut diags = Vec::new();

        for (name, value) in &doc.value {
            let bad = |m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("view '{name}': {m}"),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("not a table".into()));
                continue;
            };

            let mut view = ViewSpec {
                name: name.clone(),
                dataset: table
                    .get("dataset")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                ..ViewSpec::default()
            };
            if view.dataset.is_empty() {
                diags.push(bad("missing 'dataset'".into()));
            }

            view.grouping = table
                .get("grouping")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            if let Some(joins) = table.get("joins").and_then(|v| v.as_array()) {
                for j in joins.iter().filter_map(|v| v.as_table()) {
                    let Some(dataset) = j.get("dataset").and_then(|v| v.as_str()) else {
                        diags.push(bad("join missing 'dataset'".into()));
                        continue;
                    };
                    view.joins.push(JoinSpec {
                        dataset: dataset.to_string(),
                        on: j
                            .get("on")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                }
            }

            if let Some(cols) = table.get("columns").and_then(|v| v.as_array()) {
                for c in cols.iter().filter_map(|v| v.as_table()) {
                    let Some(col_name) = c.get("name").and_then(|v| v.as_str()) else {
                        diags.push(bad("column missing 'name'".into()));
                        continue;
                    };
                    let kind = c.get("kind").and_then(|v| v.as_str()).unwrap_or("measure");
                    let column = match kind {
                        "dimension" => ViewColumn::Dimension {
                            name: col_name.to_string(),
                        },
                        "measure" => ViewColumn::Measure {
                            name: col_name.to_string(),
                        },
                        "derived" => match c.get("sql").and_then(|v| v.as_str()) {
                            Some(sql) => ViewColumn::Derived {
                                name: col_name.to_string(),
                                sql: sql.to_string(),
                            },
                            None => {
                                diags.push(bad(format!(
                                    "derived column '{col_name}' has no 'sql'"
                                )));
                                continue;
                            }
                        },
                        other => {
                            diags.push(bad(format!(
                                "column '{col_name}' has unknown kind '{other}'"
                            )));
                            continue;
                        }
                    };
                    view.columns.push(column);
                }
            }

            if let Some(sorts) = table.get("sort").and_then(|v| v.as_array()) {
                for s in sorts.iter().filter_map(|v| v.as_table()) {
                    let Some(column) = s.get("column").and_then(|v| v.as_str()) else {
                        diags.push(bad("sort entry missing 'column'".into()));
                        continue;
                    };
                    view.sort.push(SortKey {
                        column: column.to_string(),
                        descending: s
                            .get("descending")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    });
                }
            }

            out.push(view);
        }

        (out, diags)
    }
}
```

Add to `crates/geode-core/src/lib.rs`:

```rust
pub mod view;
```

In `crates/geode-core/src/config/merge.rs`, add `dimensions` to the atomic
doc list so a derived dimension is overridden whole-object by name, like
views and datasets already are:

```rust
fn atomic_depth(doc_name: &str) -> Option<u32> {
    match doc_name {
        "views" | "layouts" | "groupings" | "scopes" | "datasets" | "sources"
        | "dimensions" => Some(1),
        _ => None,
    }
}
```

- [ ] **Step 8: Run to verify it passes**

Run: `cargo test -p geode-core view`
Expected: PASS (6 tests).

- [ ] **Step 9: Lint, format, commit**

Run: `cargo fmt && cargo clippy -p geode-core --all-targets -- -D warnings && cargo test -p geode-core`

```bash
git add crates/geode-core
git commit -m "feat(core): view definitions and derived dimensions

Views are config: dataset, joins, columns (dimension, measure, derived
SQL), grouping and sort. measure_grains reports the distinct grains a
view touches, which is what decides how many aggregate subqueries the
compiler emits.

Derived dimensions supply columns the source files lack — desk, from a
config-maintained book mapping. The 'from' clause also declares the
functional dependency book -> desk, which is what makes a desk-level
rollup additive rather than blank (spec §6.8). A book mapped to two desks
is a diagnostic, because many-to-many would break that dependency."
```

---

### Task 3: Attribution and scope semantics

**Files:**
- Create: `crates/geode-core/src/attribution.rs`
- Modify: `crates/geode-core/src/schema/grain.rs`
- Modify: `crates/geode-core/src/lib.rs`

**Interfaces:**
- Consumes: `Grain`, `DerivedDimensions`.
- Produces: `Grain::identity_columns() -> &'static [&'static str]`,
  `Attribution::{Additive, DeterminedNonAdditive, NonAttributable}`,
  `ScopeSemantics::{Direct, SemiJoined}`, and
  `attribution_of(grain, grouping, &DerivedDimensions) -> Attribution`.
  Tasks 5 and 7 carry the results into SQL and onto the snapshot.

**The rule** (spec §6.3). Split the grouping tuple `G` into the columns the
measure's key determines (`A`) and those it does not (`E`), resolving each
through `DerivedDimensions::base_column` first:

- **Additive** iff `E` is empty — every measure row falls in exactly one
  group, so summing is correct and children total to their parent.
- **DeterminedNonAdditive** iff `A` pins the grain's identity columns — the
  group names one entity, so the value is real, but it repeats across the
  sibling groups that `E` creates.
- **NonAttributable** otherwise — the number would belong to an ancestor
  row, not this one.

**`counterparty` needs no special handling.** It is a key column but not an
identity column: a position spans counterparties, so counterparty
subdivides a position's rows rather than naming it, and summing across it
is ordinary aggregation — the same thing a book-level row does to the
positions inside it.

- [ ] **Step 1: Write the failing identity-column test**

Add to the test module in `crates/geode-core/src/schema/grain.rs`:

```rust
    #[test]
    fn identity_columns_name_the_entity_not_the_whole_key() {
        // The entity a measure belongs to. `book`/`lhu` are containers and
        // `counterparty` subdivides a position, so none of them identify.
        assert_eq!(Grain::Position.identity_columns(), &["position_ref"]);
        assert_eq!(Grain::Instrument.identity_columns(), &["instrument_ref"]);
        assert_eq!(
            Grain::Underlying.identity_columns(),
            &["instrument_ref", "underlying_ref"]
        );
        assert_eq!(
            Grain::UnderlyingPair.identity_columns(),
            &["instrument_ref", "underlying_ref", "underlying2_ref"]
        );

        // Identity is always a subset of the key.
        for g in Grain::ALL {
            for c in g.identity_columns() {
                assert!(g.key_columns().contains(c), "{g:?} / {c}");
            }
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-core identity_columns`
Expected: FAIL — `no method named identity_columns`.

- [ ] **Step 3: Implement `identity_columns`**

In `crates/geode-core/src/schema/grain.rs`, add the constants beside the
existing key constants and the method inside `impl Grain`:

```rust
const ID_POSITION: [&str; 1] = ["position_ref"];
const ID_INSTRUMENT: [&str; 1] = ["instrument_ref"];
const ID_UNDERLYING: [&str; 2] = ["instrument_ref", "underlying_ref"];
const ID_PAIR: [&str; 3] = ["instrument_ref", "underlying_ref", "underlying2_ref"];
```

```rust
    /// The columns naming the entity a measure belongs to — a subset of
    /// the key. `book` and `lhu` are containers, and `counterparty`
    /// subdivides a position rather than naming it, so none of them
    /// identify. This is what decides `DeterminedNonAdditive` (spec §6.3).
    pub fn identity_columns(self) -> &'static [&'static str] {
        match self {
            Grain::Position => &ID_POSITION,
            Grain::Instrument => &ID_INSTRUMENT,
            Grain::Underlying => &ID_UNDERLYING,
            Grain::UnderlyingPair => &ID_PAIR,
        }
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-core grain`
Expected: PASS (3 tests).

- [ ] **Step 5: Write the failing attribution tests**

Create `crates/geode-core/src/attribution.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::dimensions::DerivedDimensions;
    use crate::schema::Grain;

    fn dims() -> DerivedDimensions {
        let text = r#"
[desk]
from = "book"
[desk.values]
IDX_EXO_EU = ["BK000", "BK001"]
"#;
        let doc = merge_docs("dimensions", &[LayerDoc::builtin("dimensions", text).unwrap()]);
        DerivedDimensions::from_doc(&doc).0
    }

    fn attribution(grain: Grain, grouping: &[&str]) -> Attribution {
        let g: Vec<String> = grouping.iter().map(|s| s.to_string()).collect();
        attribution_of(grain, &g, &dims())
    }

    #[test]
    fn the_specs_worked_example_reads_additive_blank_determined() {
        // spec §6.3: grouping lhu > underlying > position, trading PnL,
        // which is position grain. Each level is a prefix of the grouping.
        assert_eq!(
            attribution(Grain::Position, &["lhu"]),
            Attribution::Additive,
            "level 1: every position sits in exactly one LHU"
        );
        assert_eq!(
            attribution(Grain::Position, &["lhu", "underlying_ref"]),
            Attribution::NonAttributable,
            "level 2: a position has several underlyings"
        );
        assert_eq!(
            attribution(Grain::Position, &["lhu", "underlying_ref", "position_ref"]),
            Attribution::DeterminedNonAdditive,
            "level 3: the position is named, but repeats across underlyings"
        );
    }

    #[test]
    fn greeks_are_additive_at_every_level_of_that_same_grouping() {
        // Underlying-grain measures have no mismatch with this grouping.
        for level in [
            &["lhu"][..],
            &["lhu", "underlying_ref"],
            &["lhu", "underlying_ref", "position_ref"],
        ] {
            assert_eq!(
                attribution(Grain::Underlying, level),
                Attribution::Additive,
                "{level:?}"
            );
        }
    }

    #[test]
    fn a_derived_dimension_is_additive_through_its_source_column() {
        // desk is not a key column, but book determines it (spec §6.8), so
        // every position sits in exactly one desk.
        assert_eq!(
            attribution(Grain::Position, &["desk"]),
            Attribution::Additive
        );
        assert_eq!(
            attribution(Grain::Underlying, &["desk", "book"]),
            Attribution::Additive
        );
    }

    #[test]
    fn an_undeclared_outside_column_is_not_attributable() {
        // Without a declared dependency the compiler cannot know a
        // position sits in exactly one of these.
        assert_eq!(
            attribution(Grain::Position, &["region"]),
            Attribution::NonAttributable
        );
    }

    #[test]
    fn grouping_by_an_instrument_attribute_cannot_attribute_position_pnl() {
        // A position's legs may carry different model codes.
        assert_eq!(
            attribution(Grain::Position, &["book", "model_code"]),
            Attribution::NonAttributable
        );
        // Adding the position back makes it determined, not additive.
        assert_eq!(
            attribution(Grain::Position, &["book", "model_code", "position_ref"]),
            Attribution::DeterminedNonAdditive
        );
    }

    #[test]
    fn an_empty_grouping_is_additive_for_every_grain() {
        // The grand total. Nothing outside the key, so nothing to break.
        for g in Grain::ALL {
            assert_eq!(attribution(g, &[]), Attribution::Additive, "{g:?}");
        }
    }

    #[test]
    fn counterparty_never_needs_special_handling() {
        // It is a key column, so grouping by it is additive; omitting it
        // just sums across it, which is ordinary aggregation.
        assert_eq!(
            attribution(Grain::Position, &["book", "counterparty"]),
            Attribution::Additive
        );
        assert_eq!(
            attribution(Grain::Position, &["book", "position_ref"]),
            Attribution::Additive
        );
    }

    #[test]
    fn scope_semantics_names_the_dimensions_applied_by_membership() {
        let direct = ScopeSemantics::Direct;
        assert!(direct.is_direct());
        let semi = ScopeSemantics::SemiJoined {
            dimensions: vec!["underlying_ref".into()],
        };
        assert!(!semi.is_direct());
        assert_eq!(semi.dimensions(), &["underlying_ref".to_string()]);
    }
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p geode-core attribution`
Expected: FAIL — `cannot find function attribution_of`.

- [ ] **Step 7: Implement the rules**

Prepend to `crates/geode-core/src/attribution.rs`:

```rust
//! Whether a measure can be summed at a given grouping level, and how a
//! scope predicate reached it (spec §6.3).
//!
//! Both are decidable from the schema alone — no data is consulted — which
//! is why they live in core beside the grain vocabulary rather than in the
//! compiler.

use crate::dimensions::DerivedDimensions;
use crate::schema::Grain;

/// Whether a measure's value at one grouping level can be summed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// Every measure row belongs to exactly one group. Children total to
    /// their parent.
    Additive,
    /// The group names one entity, so the value is real — but it repeats
    /// across sibling groups and must never be totalled.
    DeterminedNonAdditive,
    /// The value would belong to an ancestor row, not this one. NULL.
    NonAttributable,
}

/// How a scope predicate was applied to a measure's grain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeSemantics {
    /// Every predicate names a column present at this grain.
    Direct,
    /// Some predicate names a finer column, applied as a membership test:
    /// "positions that have SPX risk", not "the SPX share".
    SemiJoined { dimensions: Vec<String> },
}

impl ScopeSemantics {
    pub fn is_direct(&self) -> bool {
        matches!(self, ScopeSemantics::Direct)
    }

    pub fn dimensions(&self) -> &[String] {
        match self {
            ScopeSemantics::Direct => &[],
            ScopeSemantics::SemiJoined { dimensions } => dimensions,
        }
    }
}

/// Decide a measure's attribution at one grouping level.
///
/// `grouping` is the prefix of the view's grouping tuple for this level,
/// so a three-level tree calls this three times with growing slices.
pub fn attribution_of(
    grain: Grain,
    grouping: &[String],
    dims: &DerivedDimensions,
) -> Attribution {
    let key = grain.key_columns();

    // Resolve derived dimensions to their source before testing: `desk`
    // counts as `book`, which is what makes a desk rollup additive.
    let base: Vec<&str> = grouping
        .iter()
        .map(|c| dims.base_column(c.as_str()))
        .collect();

    // A = determined by the measure's key, E = everything else.
    let extra: Vec<&&str> = base.iter().filter(|c| !key.contains(c)).collect();
    if extra.is_empty() {
        return Attribution::Additive;
    }

    // Not additive. Does what *is* attributable still name the entity?
    let attributable: Vec<&&str> = base.iter().filter(|c| key.contains(c)).collect();
    let names_entity = grain
        .identity_columns()
        .iter()
        .all(|id| attributable.iter().any(|c| **c == *id));

    if names_entity {
        Attribution::DeterminedNonAdditive
    } else {
        Attribution::NonAttributable
    }
}
```

Add to `crates/geode-core/src/lib.rs`:

```rust
pub mod attribution;
```

- [ ] **Step 8: Run to verify it passes**

Run: `cargo test -p geode-core attribution`
Expected: PASS (8 tests).

- [ ] **Step 9: Lint, format, commit**

Run: `cargo fmt && cargo clippy -p geode-core --all-targets -- -D warnings && cargo test -p geode-core`

```bash
git add crates/geode-core
git commit -m "feat(core): attribution and scope semantics

Whether a measure can be summed at a grouping level, decided from the
schema alone. Split the grouping into columns the measure's key
determines and columns it does not: nothing left over is Additive; the
grain's identity columns still pinned is DeterminedNonAdditive; neither
is NonAttributable.

Tested against spec §6.3's own worked example, which must read additive,
blank, determined down the three levels of lhu > underlying > position.

counterparty needs no special handling: it is a key column but not an
identity column, so summing across it is ordinary aggregation. Derived
dimensions resolve to their source column first, which is what makes a
desk-level rollup additive."
```

---

**Remaining tasks (4–14) continue below.** Tasks 4–6 build the scope
compiler, the grain-aware `ROLLUP` compiler, and joins; Task 7 the
`Snapshot` type; Task 8 the ENUM interning carried over from 2a; Tasks 9–10
the query pool with cancellation and as-of routing; Task 11 the
`DataService` facade; Task 12 the throwaway debug tile; Task 13 the §7.1
requery benchmarks; Task 14 cross-file conflict detection.
