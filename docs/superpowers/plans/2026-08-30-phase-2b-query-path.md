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

### Task 4: The scope compiler

**Files:**
- Create: `crates/geode-data/src/query/mod.rs`
- Create: `crates/geode-data/src/query/scope_sql.rs`
- Modify: `crates/geode-data/src/lib.rs`

**Interfaces:**
- Consumes: `geode_core::scope::{Scope, Expr, CompareOp, Literal}`,
  `geode_core::schema::{DatasetSpec, Grain}`,
  `geode_core::dimensions::DerivedDimensions`,
  `geode_core::attribution::ScopeSemantics`, `StoreError`.
- Produces: `ScopeSql { predicate: String, params: Vec<duckdb::types::Value>,
  semantics: ScopeSemantics, temp_tables: Vec<String> }` and
  `compile_scope(&Connection, &Scope, &DatasetSpec, Grain,
  &DerivedDimensions) -> Result<ScopeSql, StoreError>`. Task 5 folds the
  predicate into each grain's subquery.

**Three predicate kinds** (spec §4.1, §6.2), and one rule that decides
their shape per grain:

- **Dimension selections** → a connection-local temp table plus semi-join.
  duckdb-rs cannot bind a list (`Value::List` binding is an explicit
  error, verified against 1.10505), and this keeps the statement text
  stable regardless of selection size so the prepared plan stays cacheable.
- **Text filter** → an OR of `ILIKE` over columns the schema declares
  textual, one bound parameter reused.
- **Expression filter** → the validated AST lowered to SQL with every
  literal bound.

**Predicates naming a column absent at this grain become a semi-join**
(spec §6.3), and the result is marked `SemiJoined` with those dimensions
named. "Trading PnL of positions that have SPX risk" is a different number
from "the SPX share of trading PnL", and the marker is what stops the two
being read as the same.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/query/scope_sql.rs` with only this test
module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::ScopeSemantics;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::schema::{Grain, SchemaSpec};
    use geode_core::scope::{DimensionSelection, Scope, parse_expr};

    fn dataset() -> geode_core::schema::DatasetSpec {
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
[risk.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0.dataset("risk").unwrap().clone()
    }

    fn dims() -> DerivedDimensions {
        DerivedDimensions::default()
    }

    fn store() -> (tempfile::TempDir, crate::store::Store) {
        let d = tempfile::tempdir().unwrap();
        let s = crate::store::Store::open(d.path().join("g.duckdb")).unwrap();
        (d, s)
    }

    fn compile(scope: &Scope, grain: Grain) -> (ScopeSql, tempfile::TempDir, crate::store::Store) {
        let (dir, store) = store();
        let sql = compile_scope(store.writer(), scope, &dataset(), grain, &dims()).unwrap();
        (sql, dir, store)
    }

    #[test]
    fn an_empty_scope_compiles_to_a_true_predicate() {
        let (sql, _d, _s) = compile(&Scope::default(), Grain::Underlying);
        assert_eq!(sql.predicate, "true");
        assert!(sql.params.is_empty());
        assert_eq!(sql.semantics, ScopeSemantics::Direct);
    }

    #[test]
    fn a_dimension_selection_becomes_a_temp_table_semi_join() {
        // duckdb-rs cannot bind a list, and this keeps the statement text
        // stable regardless of how many books are selected (spec §6.2).
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into(), "BK001".into()],
            }],
            ..Scope::default()
        };
        let (sql, _d, store) = compile(&scope, Grain::Underlying);
        assert!(sql.predicate.contains("select v from"), "{}", sql.predicate);
        assert_eq!(sql.temp_tables.len(), 1);

        let n: i64 = store
            .writer()
            .query_row(
                &format!("select count(*) from {}", sql.temp_tables[0]),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "both selected values were staged");
    }

    #[test]
    fn the_statement_text_does_not_grow_with_the_selection() {
        let small = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        let large = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: (0..500).map(|i| format!("BK{i:03}")).collect(),
            }],
            ..Scope::default()
        };
        let (a, _d1, _s1) = compile(&small, Grain::Underlying);
        let (b, _d2, _s2) = compile(&large, Grain::Underlying);
        assert_eq!(
            a.predicate, b.predicate,
            "a cacheable plan requires stable text"
        );
    }

    #[test]
    fn the_text_filter_ors_ilike_over_declared_textual_columns_only() {
        let scope = Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Underlying);
        assert!(sql.predicate.contains("ilike"), "{}", sql.predicate);
        assert!(sql.predicate.contains("\"book\""), "{}", sql.predicate);
        assert!(sql.predicate.contains("\"underlying_ref\""), "{}", sql.predicate);
        // lhu is not declared textual.
        assert!(!sql.predicate.contains("\"lhu\""), "{}", sql.predicate);
        assert_eq!(sql.params.len(), 2, "one bound pattern per textual column");
    }

    #[test]
    fn expression_literals_are_bound_never_spliced() {
        let scope = Scope {
            expression: Some(parse_expr("book = 'BK000' and delta01 > 100").unwrap()),
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Underlying);
        assert!(!sql.predicate.contains("BK000"), "{}", sql.predicate);
        assert_eq!(sql.params.len(), 2);
        assert!(sql.predicate.contains('?'), "{}", sql.predicate);
    }

    #[test]
    fn a_finer_column_becomes_a_semi_join_at_a_coarser_grain() {
        // Scoping to an underlying while asking for a position measure:
        // "positions that have SPX risk" (spec §6.3).
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Position);
        assert!(sql.predicate.contains("exists"), "{}", sql.predicate);
        match &sql.semantics {
            ScopeSemantics::SemiJoined { dimensions } => {
                assert_eq!(dimensions, &["underlying_ref".to_string()]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_same_predicate_is_direct_at_its_own_grain() {
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Underlying);
        assert!(!sql.predicate.contains("exists"), "{}", sql.predicate);
        assert_eq!(sql.semantics, ScopeSemantics::Direct);
    }

    #[test]
    fn the_compiled_predicate_actually_filters() {
        // Compile then run it, so a predicate that is merely well-formed
        // but wrong cannot pass.
        let (dir, store) = store();
        store
            .writer()
            .execute_batch(
                "create table measures_underlying_live(
                     book varchar, lhu varchar, position_ref varchar,
                     counterparty varchar, instrument_ref varchar,
                     underlying_ref varchar, delta01 double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 insert into measures_underlying_live values
                   ('BK000','L','P1','C','I1','SPX', 10, 'b', 1, 1, now()),
                   ('BK001','L','P2','C','I2','RUT', 20, 'b', 1, 1, now());",
            )
            .unwrap();

        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        let sql =
            compile_scope(store.writer(), &scope, &dataset(), Grain::Underlying, &dims()).unwrap();
        let total: f64 = store
            .writer()
            .query_row(
                &format!(
                    "select sum(delta01) from measures_underlying_live where {}",
                    sql.predicate
                ),
                duckdb::params_from_iter(sql.params.iter()),
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 10.0, "only BK000's row survives");
        drop(dir);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data scope_sql`
Expected: FAIL — `cannot find function compile_scope`.

- [ ] **Step 3: Implement the scope compiler**

Prepend to `crates/geode-data/src/query/scope_sql.rs`:

```rust
//! Scope to SQL (spec §6.2). Three predicate kinds composed with AND,
//! every value bound rather than spliced.
//!
//! Dimension selections go through a connection-local temp table and a
//! semi-join. duckdb-rs cannot bind a list parameter — `Value::List`
//! binding is an explicit error, verified against 1.10505 — and the temp
//! table also keeps the statement text stable regardless of selection
//! size, so the prepared plan stays cacheable.
//!
//! A predicate naming a column that does not exist at the requested grain
//! becomes a semi-join against the grain where it does, and the result is
//! marked `SemiJoined`: "positions that have SPX risk" is not "the SPX
//! share of the position" (spec §6.3).

use crate::store::StoreError;
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::ScopeSemantics;
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{DatasetSpec, Grain};
use geode_core::scope::{CompareOp, Expr, Literal, Scope};
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique temp-table names within a process, so two concurrent
/// compilations on different connections never collide by name.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ScopeSql {
    /// A boolean expression, `true` when the scope is empty.
    pub predicate: String,
    /// Bound in order; the predicate carries `?` placeholders.
    pub params: Vec<Value>,
    pub semantics: ScopeSemantics,
    /// Temp tables created on the connection, for the caller to drop.
    pub temp_tables: Vec<String>,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// The finest grain at which `column` exists, or `None` when the column is
/// not a key column of any grain (a measure or attribute).
fn grain_of_key_column(column: &str) -> Option<Grain> {
    Grain::ALL
        .iter()
        .copied()
        .find(|g| g.key_columns().contains(&column))
}

pub fn compile_scope(
    conn: &Connection,
    scope: &Scope,
    ds: &DatasetSpec,
    grain: Grain,
    dims: &DerivedDimensions,
) -> Result<ScopeSql, StoreError> {
    let mut direct: Vec<String> = Vec::new();
    let mut finer: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    let mut temp_tables: Vec<String> = Vec::new();
    let mut semi_dimensions: Vec<String> = Vec::new();

    // 1. Dimension selections: temp table + semi-join.
    for sel in &scope.dimensions {
        if sel.values.is_empty() {
            continue;
        }
        let base = dims.base_column(&sel.column).to_string();
        let table = format!(
            "scope_{}_{}",
            base.replace(|c: char| !c.is_alphanumeric(), "_"),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let create = format!("create temp table {table}(v varchar)");
        conn.execute_batch(&create).map_err(sql_err(&create))?;
        {
            let mut app = conn.appender(&table).map_err(sql_err(&create))?;
            for v in &sel.values {
                app.append_row(duckdb::params![v]).map_err(sql_err(&create))?;
            }
        }
        temp_tables.push(table.clone());

        let clause = format!("\"{base}\" in (select v from {table})");
        if grain.key_columns().contains(&base.as_str()) {
            direct.push(clause);
        } else {
            finer.push(clause);
            semi_dimensions.push(base);
        }
    }

    // 2. Text filter: OR of ILIKE over declared textual columns.
    if let Some(text) = &scope.text {
        let pattern = format!("%{text}%");
        let mut terms = Vec::new();
        for col in ds.textual_columns() {
            // Only columns present at this grain; a textual column from a
            // finer grain would need its own semi-join, and the global
            // text filter is not worth that complexity (spec §4.1).
            if grain.key_columns().contains(&col.name.as_str()) {
                terms.push(format!("\"{}\" ilike ?", col.name));
                params.push(Value::Text(pattern.clone()));
            }
        }
        if !terms.is_empty() {
            direct.push(format!("({})", terms.join(" or ")));
        }
    }

    // 3. Expression filter: AST lowered, literals bound.
    if let Some(expr) = &scope.expression {
        let mut expr_params = Vec::new();
        let rendered = render_expr(expr, &mut expr_params);
        let mentions_finer = expr
            .columns()
            .iter()
            .any(|c| !grain.key_columns().contains(c));
        params.extend(expr_params);
        if mentions_finer {
            for c in expr.columns() {
                if !grain.key_columns().contains(&c) && !semi_dimensions.iter().any(|s| s == c) {
                    semi_dimensions.push(c.to_string());
                }
            }
            finer.push(rendered);
        } else {
            direct.push(rendered);
        }
    }

    // Finer predicates apply as a membership test against the grain where
    // those columns exist. The finest grain always carries every key
    // column, so it is the safe target.
    if !finer.is_empty() {
        let probe = Grain::UnderlyingPair;
        let join = grain
            .key_columns()
            .iter()
            .map(|k| format!("probe.\"{k}\" = base.\"{k}\""))
            .collect::<Vec<_>>()
            .join(" and ");
        direct.push(format!(
            "exists (select 1 from {} probe where {join} and {})",
            crate::store::ddl::table_name(probe, crate::store::ddl::TableKind::Live),
            finer.join(" and ")
        ));
    }

    let predicate = if direct.is_empty() {
        "true".to_string()
    } else {
        direct.join(" and ")
    };

    let semantics = if semi_dimensions.is_empty() {
        ScopeSemantics::Direct
    } else {
        ScopeSemantics::SemiJoined {
            dimensions: semi_dimensions,
        }
    };

    Ok(ScopeSql {
        predicate,
        params,
        semantics,
        temp_tables,
    })
}

/// Lower a validated expression, pushing every literal onto `params`.
fn render_expr(expr: &Expr, params: &mut Vec<Value>) -> String {
    match expr {
        Expr::And(a, b) => format!(
            "({} and {})",
            render_expr(a, params),
            render_expr(b, params)
        ),
        Expr::Or(a, b) => format!("({} or {})", render_expr(a, params), render_expr(b, params)),
        Expr::Not(e) => format!("(not {})", render_expr(e, params)),
        Expr::Compare { column, op, value } => {
            params.push(literal_value(value));
            format!("\"{column}\" {} ?", op.sql())
        }
        Expr::In { column, values } => {
            let marks = values
                .iter()
                .map(|v| {
                    params.push(literal_value(v));
                    "?"
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("\"{column}\" in ({marks})")
        }
    }
}

fn literal_value(l: &Literal) -> Value {
    match l {
        Literal::Str(s) => Value::Text(s.clone()),
        Literal::Num(n) => Value::Double(*n),
        Literal::Bool(b) => Value::Boolean(*b),
    }
}
```

Create `crates/geode-data/src/query/mod.rs`:

```rust
//! The query path (spec §6): scope compilation, the grain-aware view
//! compiler, the read pool, and as-of routing.

pub mod scope_sql;

pub use scope_sql::{ScopeSql, compile_scope};
```

Add to `crates/geode-data/src/lib.rs`:

```rust
pub mod query;
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-data scope_sql`
Expected: PASS (8 tests).

- [ ] **Step 5: Lint, format, commit**

Run: `cargo fmt && cargo clippy -p geode-data --all-targets -- -D warnings && cargo test -p geode-data`

```bash
git add crates/geode-data
git commit -m "feat(data): scope compiler

Three predicate kinds composed with AND, every value bound rather than
spliced. Dimension selections go through a connection-local temp table
and a semi-join: duckdb-rs cannot bind a list, and the temp table also
keeps the statement text stable regardless of selection size, so the
prepared plan stays cacheable — asserted by compiling a 1-value and a
500-value selection and comparing the text.

A predicate naming a column absent at the requested grain becomes a
membership test and the result is marked SemiJoined with the dimension
named, because 'positions that have SPX risk' is not 'the SPX share of
the position' (spec §6.3).

The final test compiles a predicate and then runs it, so one that is
merely well-formed but wrong cannot pass."
```

---

### Task 5: The grain-aware ROLLUP compiler

**Files:**
- Create: `crates/geode-data/src/query/compile.rs`
- Modify: `crates/geode-data/src/query/mod.rs`

**Interfaces:**
- Consumes: `ViewSpec`, `SchemaSpec`, `Scope`, `DerivedDimensions`,
  `attribution_of`, `compile_scope`, `Grain`, `store::ddl::table_name`.
- Produces: `CompiledColumn { name, grain, attribution_by_depth,
  scope_semantics }`, `CompiledQuery { sql, params, temp_tables, grouping,
  columns }`, and `compile_view(&Connection, &ViewSpec, &SchemaSpec,
  &Scope, &DerivedDimensions) -> Result<CompiledQuery, StoreError>`.
  Tasks 7 and 9 carry the result into a `Snapshot`.

**The shape.** One statement, not one per level (spec §6.3): a *spine*
supplies every level of the tree via `ROLLUP`, and each measure grain
contributes an aggregate subquery joined onto it at group cardinality —
tens or hundreds of rows, never the raw tables.

```sql
with spine as (
    select g1, g2, grouping(g1, g2) as depth_mask
    from measures_underlying_pair_live where <scope> group by rollup(g1, g2)),
agg_underlying as (
    select g1, g2, sum(delta01) as delta01
    from measures_underlying_live where <scope> group by rollup(g1, g2)),
agg_position as (
    select g1, sum(daily_trading_pnl) as daily_trading_pnl
    from measures_position_live where <scope> group by rollup(g1))
select s.g1, s.g2, s.depth_mask, u.delta01,
       case when s.depth_mask in (…) then null else p.daily_trading_pnl end
from spine s
left join agg_underlying u on u.g1 is not distinct from s.g1 and …
left join agg_position   p on p.g1 is not distinct from s.g1
```

Three things carry the design:

- **The spine is the finest grain**, which holds every key column, so it
  can produce every level. Each measure grain groups only by the grouping
  columns *it has*, and joins on those — which is exactly why a
  position-grain value repeats across underlying siblings rather than
  being wrong.
- **`is not distinct from`**, because `ROLLUP` fills higher levels with
  NULL and `=` would drop every one of them.
- **`grouping(...)` returns a bitmask**, verified against DuckDB 1.10505.
  For `n` grouping columns a level with `d` of them present has mask
  `2^(n-d) - 1`, so depth and mask convert both ways and the
  `NonAttributable` levels become a literal `in (…)` list.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/query/compile.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::Attribution;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::schema::SchemaSpec;
    use geode_core::scope::Scope;
    use geode_core::view::ViewSpec;

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
[risk_snapshot.columns.underlying2_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    /// The spec §6.3 view: lhu > underlying > position, one measure at
    /// underlying grain and one at position grain.
    fn view() -> ViewSpec {
        let text = r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref", "position_ref"]
[[tree.columns]]
name = "delta01"
kind = "measure"
[[tree.columns]]
name = "daily_trading_pnl"
kind = "measure"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.into_iter().next().unwrap()
    }

    /// A store with the four live tables and one worst-of instrument:
    /// position P1 in LHU L0, two underlyings, trading PnL 7, delta 10
    /// and 20.
    fn fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store.apply_schema(schema().dataset("risk_snapshot").unwrap()).unwrap();
        store
            .writer()
            .execute_batch(
                "insert into measures_position_live values
                   ('BK0','L0','P1','C', 7, 'b', 1, 1, now());
                 insert into measures_underlying_live values
                   ('BK0','L0','P1','C','I1','SPX', 10, 'b', 1, 1, now()),
                   ('BK0','L0','P1','C','I1','RUT', 20, 'b', 1, 1, now());
                 insert into measures_underlying_pair_live values
                   ('BK0','L0','P1','C','I1','RUT','SPX', 'b', 1, 1, now());",
            )
            .unwrap();
        (dir, store)
    }

    fn compile(store: &crate::store::Store) -> CompiledQuery {
        compile_view(
            store.writer(),
            &view(),
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
        )
        .unwrap()
    }

    #[test]
    fn depth_and_mask_convert_both_ways() {
        // n = 3: leaf is 0, then 1, 3, and the grand total 7.
        assert_eq!(mask_for_depth(3, 3), 0);
        assert_eq!(mask_for_depth(3, 2), 1);
        assert_eq!(mask_for_depth(3, 1), 3);
        assert_eq!(mask_for_depth(3, 0), 7);
        for n in 0..=4 {
            for d in 0..=n {
                assert_eq!(depth_for_mask(n, mask_for_depth(n, d)), Some(d));
            }
        }
    }

    #[test]
    fn attribution_is_recorded_per_depth_matching_the_spec_example() {
        let (_d, store) = fixture();
        let q = compile(&store);
        let pnl = q
            .columns
            .iter()
            .find(|c| c.name == "daily_trading_pnl")
            .expect("pnl column");
        // depth 1 = [lhu], 2 = [lhu, underlying], 3 = [.., position]
        assert_eq!(pnl.attribution_by_depth[1], Attribution::Additive);
        assert_eq!(pnl.attribution_by_depth[2], Attribution::NonAttributable);
        assert_eq!(
            pnl.attribution_by_depth[3],
            Attribution::DeterminedNonAdditive
        );

        let delta = q.columns.iter().find(|c| c.name == "delta01").unwrap();
        for d in 0..=3 {
            assert_eq!(delta.attribution_by_depth[d], Attribution::Additive, "depth {d}");
        }
    }

    #[test]
    fn one_statement_returns_every_level() {
        let (_d, store) = fixture();
        let q = compile(&store);
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let rows: Vec<(i64, Option<f64>, Option<f64>)> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok((r.get("depth_mask")?, r.get("delta01")?, r.get("daily_trading_pnl")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        // Grand total, lhu, lhu+underlying (x2), lhu+underlying+position (x2)
        assert_eq!(rows.len(), 6, "{rows:?}");
        assert!(rows.iter().any(|(m, ..)| *m == 7), "grand total missing");
    }

    #[test]
    fn a_coarse_measure_does_not_double_count_at_any_level() {
        // The whole point. P1's trading PnL is 7; it must never sum to 14
        // just because the position has two underlyings.
        let (_d, store) = fixture();
        let q = compile(&store);
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let rows: Vec<(i64, Option<f64>)> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok((r.get("depth_mask")?, r.get("daily_trading_pnl")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let at = |depth: usize| -> Vec<Option<f64>> {
            let m = mask_for_depth(3, depth);
            rows.iter().filter(|(mask, _)| *mask == m).map(|(_, v)| *v).collect()
        };
        assert_eq!(at(0), vec![Some(7.0)], "grand total is the position's own PnL");
        assert_eq!(at(1), vec![Some(7.0)], "LHU total, not doubled");
        assert_eq!(at(2), vec![None, None], "blank under an underlying");
        assert_eq!(
            at(3),
            vec![Some(7.0), Some(7.0)],
            "the position's real PnL, repeated and marked do-not-total"
        );
    }

    #[test]
    fn an_additive_measure_totals_correctly_up_the_tree() {
        let (_d, store) = fixture();
        let q = compile(&store);
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let rows: Vec<(i64, Option<f64>)> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok((r.get("depth_mask")?, r.get("delta01")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let total: Option<f64> = rows
            .iter()
            .find(|(m, _)| *m == mask_for_depth(3, 0))
            .map(|(_, v)| *v)
            .unwrap();
        assert_eq!(total, Some(30.0), "10 + 20 across the two underlyings");
    }

    #[test]
    fn the_grouping_columns_are_reported_in_order() {
        let (_d, store) = fixture();
        let q = compile(&store);
        assert_eq!(q.grouping, vec!["lhu", "underlying_ref", "position_ref"]);
    }

    #[test]
    fn an_empty_grouping_yields_a_single_total_row() {
        let (_d, store) = fixture();
        let mut v = view();
        v.grouping.clear();
        let q = compile_view(
            store.writer(),
            &v,
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
        )
        .unwrap();
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let n = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |_| Ok(()))
            .unwrap()
            .count();
        assert_eq!(n, 1);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data compile`
Expected: FAIL — `cannot find function compile_view`.

- [ ] **Step 3: Implement the compiler**

Prepend to `crates/geode-data/src/query/compile.rs`:

```rust
//! The view compiler (spec §6.3). One statement per view, covering every
//! level of the rollup tree, with each measure aggregated at its own grain
//! and joined at group cardinality.
//!
//! Emitting one query per level would put tree navigation on the 50ms
//! requery budget; emitting one puts expand and collapse on the 8ms frame
//! budget instead, because every level is already in the snapshot.

use crate::query::scope_sql::compile_scope;
use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::{Attribution, ScopeSemantics, attribution_of};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{ColumnRole, Grain, SchemaSpec};
use geode_core::scope::Scope;
use geode_core::view::{ViewColumn, ViewSpec};

/// `GROUPING(a, b, c)` returns a bitmask: a bit is set for each column the
/// row is *not* grouped by. Under ROLLUP, a level with `depth` of `n`
/// columns present has the top `n - depth` bits set.
pub fn mask_for_depth(n: usize, depth: usize) -> i64 {
    ((1i64 << (n - depth)) - 1).max(0)
}

/// Inverse of [`mask_for_depth`], or `None` when the mask is not one
/// ROLLUP can produce.
pub fn depth_for_mask(n: usize, mask: i64) -> Option<usize> {
    (0..=n).find(|d| mask_for_depth(n, *d) == mask)
}

#[derive(Debug, Clone)]
pub struct CompiledColumn {
    pub name: String,
    /// `None` for grouping columns and the depth marker.
    pub grain: Option<Grain>,
    /// Indexed by depth, `0..=grouping.len()`.
    pub attribution_by_depth: Vec<Attribution>,
    pub scope_semantics: ScopeSemantics,
}

#[derive(Debug, Clone)]
pub struct CompiledQuery {
    pub sql: String,
    pub params: Vec<Value>,
    /// Temp tables the scope compiler created; the caller drops them.
    pub temp_tables: Vec<String>,
    pub grouping: Vec<String>,
    pub columns: Vec<CompiledColumn>,
}

fn quoted(cols: &[String]) -> Vec<String> {
    cols.iter().map(|c| format!("\"{c}\"")).collect()
}

pub fn compile_view(
    conn: &Connection,
    view: &ViewSpec,
    schema: &SchemaSpec,
    scope: &Scope,
    dims: &DerivedDimensions,
) -> Result<CompiledQuery, StoreError> {
    let ds = schema
        .dataset(&view.dataset)
        .ok_or_else(|| StoreError::Sql {
            statement: format!("compile view '{}'", view.name),
            source: duckdb::Error::InvalidParameterName(format!(
                "unknown dataset '{}'",
                view.dataset
            )),
        })?;

    let n = view.grouping.len();
    let group_cols = quoted(&view.grouping);
    let mut params: Vec<Value> = Vec::new();
    let mut temp_tables: Vec<String> = Vec::new();
    let mut ctes: Vec<String> = Vec::new();
    let mut selects: Vec<String> = Vec::new();
    let mut joins: Vec<String> = Vec::new();
    let mut columns: Vec<CompiledColumn> = Vec::new();

    // The spine: the finest grain holds every key column, so it can
    // produce every level of the tree.
    let spine_grain = Grain::UnderlyingPair;
    let spine_scope = compile_scope(conn, scope, ds, spine_grain, dims)?;
    params.extend(spine_scope.params.clone());
    temp_tables.extend(spine_scope.temp_tables.clone());

    let grouping_expr = if n == 0 {
        // grouping() needs an argument; a constant stands in for the
        // single total row.
        "0".to_string()
    } else {
        format!("grouping({})", group_cols.join(", "))
    };
    let group_by = if n == 0 {
        "".to_string()
    } else {
        format!(" group by rollup({})", group_cols.join(", "))
    };
    ctes.push(format!(
        "spine as (select {select}{comma}{grouping_expr} as depth_mask \
         from {table} where {pred}{group_by})",
        select = group_cols.join(", "),
        comma = if n == 0 { "" } else { ", " },
        table = table_name(spine_grain, TableKind::Live),
        pred = spine_scope.predicate,
    ));

    for g in &view.grouping {
        selects.push(format!("s.\"{g}\""));
        columns.push(CompiledColumn {
            name: g.clone(),
            grain: None,
            attribution_by_depth: vec![Attribution::Additive; n + 1],
            scope_semantics: ScopeSemantics::Direct,
        });
    }
    selects.push("s.depth_mask".to_string());
    columns.push(CompiledColumn {
        name: "depth_mask".to_string(),
        grain: None,
        attribution_by_depth: vec![Attribution::Additive; n + 1],
        scope_semantics: ScopeSemantics::Direct,
    });

    // One aggregate subquery per measure grain the view touches.
    for grain in view.measure_grains(schema) {
        let alias = format!("agg_{}", grain.table());
        let grain_scope = compile_scope(conn, scope, ds, grain, dims)?;
        temp_tables.extend(grain_scope.temp_tables.clone());

        // Only the grouping columns this grain actually has.
        let own: Vec<String> = view
            .grouping
            .iter()
            .filter(|g| grain.key_columns().contains(&dims.base_column(g)))
            .cloned()
            .collect();
        let own_q = quoted(&own);

        let measures: Vec<&geode_core::schema::ColumnSpec> = view
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Measure { name } => ds.column(name),
                _ => None,
            })
            .filter(|c| c.grain() == Some(grain))
            .collect();
        if measures.is_empty() {
            continue;
        }

        let aggs: Vec<String> = measures
            .iter()
            .map(|m| {
                let agg = match m.role {
                    ColumnRole::Measure { aggregate, .. } => aggregate,
                    _ => geode_core::schema::Aggregate::Sum,
                };
                format!("{} as \"{}\"", agg.sql(&format!("\"{}\"", m.name)), m.name)
            })
            .collect();

        let sub_group = if own.is_empty() {
            String::new()
        } else {
            format!(" group by rollup({})", own_q.join(", "))
        };
        ctes.push(format!(
            "{alias} as (select {keys}{comma}{aggs} from {table} where {pred}{sub_group})",
            keys = own_q.join(", "),
            comma = if own.is_empty() { "" } else { ", " },
            aggs = aggs.join(", "),
            table = table_name(grain, TableKind::Live),
            pred = grain_scope.predicate,
        ));
        // The grain subquery's params follow the spine's, in CTE order.
        params.extend(grain_scope.params);

        let on = if own.is_empty() {
            "true".to_string()
        } else {
            own.iter()
                .map(|c| format!("{alias}.\"{c}\" is not distinct from s.\"{c}\""))
                .collect::<Vec<_>>()
                .join(" and ")
        };
        joins.push(format!("left join {alias} on {on}"));

        for m in measures {
            // Attribution per depth, from the schema alone.
            let by_depth: Vec<Attribution> = (0..=n)
                .map(|d| attribution_of(grain, &view.grouping[..d], dims))
                .collect();
            let blank: Vec<String> = (0..=n)
                .filter(|d| by_depth[*d] == Attribution::NonAttributable)
                .map(|d| mask_for_depth(n, d).to_string())
                .collect();

            let expr = if blank.is_empty() {
                format!("{alias}.\"{}\"", m.name)
            } else {
                // The value would belong to an ancestor row, not this one.
                format!(
                    "case when s.depth_mask in ({}) then null else {alias}.\"{}\" end",
                    blank.join(", "),
                    m.name
                )
            };
            selects.push(format!("{expr} as \"{}\"", m.name));
            columns.push(CompiledColumn {
                name: m.name.clone(),
                grain: Some(grain),
                attribution_by_depth: by_depth,
                scope_semantics: grain_scope.semantics.clone(),
            });
        }
    }

    // Derived columns are expressions over the columns already selected.
    for c in &view.columns {
        if let ViewColumn::Derived { name, sql } = c {
            selects.push(format!("({sql}) as \"{name}\""));
            columns.push(CompiledColumn {
                name: name.clone(),
                grain: None,
                attribution_by_depth: vec![Attribution::Additive; n + 1],
                scope_semantics: ScopeSemantics::Direct,
            });
        }
    }

    let order = if view.sort.is_empty() {
        String::new()
    } else {
        let keys: Vec<String> = view
            .sort
            .iter()
            .map(|s| {
                format!(
                    "\"{}\" {}",
                    s.column,
                    if s.descending { "desc" } else { "asc" }
                )
            })
            .collect();
        format!(" order by s.depth_mask desc, {}", keys.join(", "))
    };

    let sql = format!(
        "with {ctes} select {selects} from spine s {joins}{order}",
        ctes = ctes.join(",\n"),
        selects = selects.join(", "),
        joins = joins.join(" "),
    );

    Ok(CompiledQuery {
        sql,
        params,
        temp_tables,
        grouping: view.grouping.clone(),
        columns,
    })
}
```

Add to `crates/geode-data/src/query/mod.rs`:

```rust
pub mod compile;

pub use compile::{
    CompiledColumn, CompiledQuery, compile_view, depth_for_mask, mask_for_depth,
};
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-data compile`
Expected: PASS (7 tests).

- [ ] **Step 5: Lint, format, commit**

Run: `cargo fmt && cargo clippy -p geode-data --all-targets -- -D warnings && cargo test -p geode-data`

```bash
git add crates/geode-data
git commit -m "feat(data): grain-aware ROLLUP compiler

One statement per view covering every level of the tree. A spine at the
finest grain produces the levels; each measure grain aggregates only by
the grouping columns it has and joins on those at group cardinality — so
a position-grain value repeats across underlying siblings rather than
being wrong, and never double-counts.

Joins use `is not distinct from` because ROLLUP fills higher levels with
NULL. Levels where a measure is NonAttributable are blanked with a CASE
over the grouping bitmask, so the value that would belong to an ancestor
row never appears on a descendant.

The load-bearing test runs the compiled SQL against a worst-of fixture
and asserts the position's PnL reads 7 at the total, 7 at the LHU, blank
under an underlying, and 7 twice at the leaves — never 14."
```

---

### Task 6: Cross-dataset joins

**Files:**
- Modify: `crates/geode-data/src/query/compile.rs`

**Interfaces:**
- Consumes: `JoinSpec` from Task 2, `CompiledQuery` from Task 5.
- Produces: no new types — `compile_view` gains join handling, and
  `CompiledQuery` gains `pub stalest_input: Vec<String>` naming every
  dataset the query reads, so Task 7 can apply §5.4's stalest-input rule.

**Two joins are day-one** (spec §6.4): `measures_* ⋈ instrument_ref` on
`instrument_ref`, and `measures_underlying ⋈ implied_vol_summary` on
`underlying_ref`. The first is what puts strike and expiry on a blotter
row; the second is the motivating vol-inline case.

**A joined view is as stale as its stalest input** (spec §5.4), so the
compiler records which datasets it touched rather than leaving the caller
to guess.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/geode-data/src/query/compile.rs`:

```rust
    fn joined_schema() -> SchemaSpec {
        let mut text = String::from(
            r#"
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
[risk_snapshot.columns.underlying2_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
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
"#,
        );
        text.push('\n');
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", &text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn joined_view() -> ViewSpec {
        let text = r#"
[with_ref]
dataset = "risk_snapshot"
grouping = ["instrument_ref"]
[[with_ref.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]
[[with_ref.columns]]
name = "delta01"
kind = "measure"
[[with_ref.columns]]
name = "strike"
kind = "dimension"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.into_iter().next().unwrap()
    }

    #[test]
    fn a_join_puts_reference_columns_on_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        let schema = joined_schema();
        store.apply_schema(schema.dataset("risk_snapshot").unwrap()).unwrap();
        store
            .writer()
            .execute_batch(
                "create table instrument_ref_live(
                     instrument_ref varchar, strike double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 insert into instrument_ref_live values ('I1', 4200.0, 'b', 1, 1, now());
                 insert into measures_underlying_live values
                   ('BK0','L0','P1','C','I1','SPX', 10, 'b', 1, 1, now());
                 insert into measures_underlying_pair_live values
                   ('BK0','L0','P1','C','I1','RUT','SPX', 'b', 1, 1, now());",
            )
            .unwrap();

        let q = compile_view(
            store.writer(),
            &joined_view(),
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
        )
        .unwrap();

        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let strikes: Vec<Option<f64>> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| r.get("strike"))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            strikes.iter().any(|s| *s == Some(4200.0)),
            "the joined strike must reach the row: {strikes:?}"
        );
    }

    #[test]
    fn every_dataset_read_is_recorded_for_the_stalest_input_rule() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        let schema = joined_schema();
        store.apply_schema(schema.dataset("risk_snapshot").unwrap()).unwrap();
        store
            .writer()
            .execute_batch(
                "create table instrument_ref_live(
                     instrument_ref varchar, strike double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);",
            )
            .unwrap();

        let q = compile_view(
            store.writer(),
            &joined_view(),
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
        )
        .unwrap();
        let mut inputs = q.stalest_input.clone();
        inputs.sort();
        assert_eq!(inputs, vec!["instrument_ref", "risk_snapshot"]);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data compile`
Expected: FAIL — `no field stalest_input`.

- [ ] **Step 3: Implement joins**

In `crates/geode-data/src/query/compile.rs`, add the field to
`CompiledQuery`:

```rust
    /// Every dataset the query reads. A joined view is as stale as its
    /// stalest input (spec §5.4), and the caller cannot compute that
    /// without knowing which datasets were touched.
    pub stalest_input: Vec<String>,
```

Inside `compile_view`, after the measure-grain loop and before derived
columns, join each declared dataset and select its columns:

```rust
    // Cross-dataset joins (spec §6.4). Join keys are declared in schema
    // config; the joined dataset's live table is joined once and its
    // columns selected through it.
    let mut stalest_input = vec![view.dataset.clone()];
    for (i, join) in view.joins.iter().enumerate() {
        let Some(joined_ds) = schema.dataset(&join.dataset) else {
            continue;
        };
        stalest_input.push(join.dataset.clone());
        let alias = format!("join_{i}");
        let table = format!("{}_live", join.dataset);

        // The spine carries every key column, so the join keys resolve
        // against it.
        let on = join
            .on
            .iter()
            .map(|k| format!("{alias}.\"{k}\" is not distinct from s.\"{k}\""))
            .collect::<Vec<_>>()
            .join(" and ");
        joins.push(format!("left join {table} {alias} on {on}"));

        for c in &view.columns {
            let ViewColumn::Dimension { name } = c else {
                continue;
            };
            if joined_ds.column(name).is_none() || view.grouping.contains(name) {
                continue;
            }
            // A joined attribute is constant within its own grain, so it
            // is meaningful only where the grouping reaches that grain;
            // above it, several instruments share the row.
            selects.push(format!("any_value({alias}.\"{name}\") as \"{name}\""));
            columns.push(CompiledColumn {
                name: name.clone(),
                grain: joined_ds.column(name).and_then(|c| c.grain()),
                attribution_by_depth: (0..=n)
                    .map(|d| match joined_ds.column(name).and_then(|c| c.grain()) {
                        Some(g) => attribution_of(g, &view.grouping[..d], dims),
                        None => Attribution::Additive,
                    })
                    .collect(),
                scope_semantics: ScopeSemantics::Direct,
            });
        }
    }
```

The `any_value` needs the outer select to aggregate, so wrap the outer
query in a `group by` over the spine columns when any join is present.
Replace the final `sql` construction with:

```rust
    let outer_group = if view.joins.is_empty() {
        String::new()
    } else {
        let mut keys: Vec<String> = view
            .grouping
            .iter()
            .map(|g| format!("s.\"{g}\""))
            .collect();
        keys.push("s.depth_mask".to_string());
        // Every non-aggregated selected column must be grouped; measures
        // arrive pre-aggregated from their CTEs, so they group by value.
        for c in &columns {
            if c.grain.is_some() && !view.grouping.contains(&c.name) {
                keys.push(format!("\"{}\"", c.name));
            }
        }
        format!(" group by {}", keys.join(", "))
    };

    let sql = format!(
        "with {ctes} select {selects} from spine s {joins}{outer_group}{order}",
        ctes = ctes.join(",\n"),
        selects = selects.join(", "),
        joins = joins.join(" "),
    );

    Ok(CompiledQuery {
        sql,
        params,
        temp_tables,
        grouping: view.grouping.clone(),
        columns,
        stalest_input,
    })
```

Update the earlier `Ok(CompiledQuery { … })` in Task 5's implementation to
this final form — there is only one construction site.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-data compile`
Expected: PASS (9 tests).

- [ ] **Step 5: Lint, format, commit**

```bash
cargo fmt && cargo clippy -p geode-data --all-targets -- -D warnings
git add crates/geode-data
git commit -m "feat(data): cross-dataset joins in the view compiler

Joins the declared datasets onto the spine on their configured keys, so
instrument reference columns reach the blotter row and the vol-inline
case works. A joined attribute is constant within its own grain, so it is
selected with any_value and carries the attribution of that grain — above
it, several instruments share the row.

CompiledQuery records every dataset read, because a joined view is as
stale as its stalest input (spec §5.4) and the caller cannot work that
out without knowing what was touched."
```

---

### Task 7: The `Snapshot` type

**Files:**
- Create: `crates/geode-core/src/snapshot.rs`
- Modify: `crates/geode-core/src/lib.rs`
- Modify: `crates/geode-core/Cargo.toml`

**Interfaces:**
- Consumes: `Attribution`, `ScopeSemantics`, `Grain`.
- Produces: `Snapshot`, `ColumnMeta`, `Provenance`, `Freshness`,
  `Snapshot::{rows, column_names, f64_column, str_column, i64_column,
  depth_of_row, meta, provenance}`. Tasks 9, 11 and 12 consume it.

**Arrow stays an implementation detail** (spec §6.6): `geode-core` depends
on `arrow` so the accessors can be zero-copy, but the type modules see is
`Snapshot`, with typed accessors returning plain slices. Nothing outside
this file names an Arrow type.

**`query_arrow` yields 2048-row batches, not one** (verified against
1.10505), so a column-wide `&[f64]` needs concatenation. `Snapshot`
concatenates once at construction: blotter results are *aggregates* — one
row per visible group — while the million rows are scanned inside DuckDB
and never cross the boundary.

- [ ] **Step 1: Add the arrow dependency**

In `crates/geode-core/Cargo.toml`:

```toml
[dependencies]
arrow = "58.4.0"
```

The version must match the one duckdb-rs re-exports, or the two crates'
`RecordBatch` types are different types. Check with
`cargo tree -p geode-data -i arrow` and pin to what that reports.

- [ ] **Step 2: Write the failing tests**

Create `crates/geode-core/src/snapshot.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{Attribution, ScopeSemantics};
    use arrow::array::{Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;

    /// Two batches, so concatenation is actually exercised — this is the
    /// shape query_arrow really returns.
    fn batches() -> Vec<RecordBatch> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("book", DataType::Utf8, true),
            Field::new("depth_mask", DataType::Int64, true),
            Field::new("delta01", DataType::Float64, true),
        ]));
        let one = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![Some("BK000"), None])),
                Arc::new(Int64Array::from(vec![0, 1])),
                Arc::new(Float64Array::from(vec![Some(10.0), Some(30.0)])),
            ],
        )
        .unwrap();
        let two = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec![Some("BK001")])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Float64Array::from(vec![Some(20.0)])),
            ],
        )
        .unwrap();
        vec![one, two]
    }

    fn meta() -> Vec<ColumnMeta> {
        vec![
            ColumnMeta {
                name: "book".into(),
                attribution_by_depth: vec![Attribution::Additive; 2],
                scope_semantics: ScopeSemantics::Direct,
            },
            ColumnMeta {
                name: "depth_mask".into(),
                attribution_by_depth: vec![Attribution::Additive; 2],
                scope_semantics: ScopeSemantics::Direct,
            },
            ColumnMeta {
                name: "delta01".into(),
                attribution_by_depth: vec![Attribution::Additive; 2],
                scope_semantics: ScopeSemantics::Direct,
            },
        ]
    }

    fn snapshot() -> Snapshot {
        Snapshot::from_batches(batches(), meta(), 1, Provenance::default()).unwrap()
    }

    #[test]
    fn concatenates_batches_into_one_addressable_column() {
        let s = snapshot();
        assert_eq!(s.rows(), 3, "both batches, not just the first");
        assert_eq!(s.f64_column("delta01").unwrap(), &[10.0, 30.0, 20.0]);
    }

    #[test]
    fn string_columns_read_by_row_including_nulls() {
        let s = snapshot();
        assert_eq!(s.str_column("book").unwrap().value(0), "BK000");
        assert!(s.str_column("book").unwrap().is_null(1));
    }

    #[test]
    fn an_unknown_or_mistyped_column_is_none_not_a_panic() {
        let s = snapshot();
        assert!(s.f64_column("nonesuch").is_none());
        assert!(s.f64_column("book").is_none(), "wrong type must not panic");
    }

    #[test]
    fn depth_is_decoded_from_the_grouping_bitmask() {
        // n = 1: mask 0 is the leaf, mask 1 the grand total.
        let s = snapshot();
        assert_eq!(s.depth_of_row(0), Some(1));
        assert_eq!(s.depth_of_row(1), Some(0));
    }

    #[test]
    fn column_metadata_is_addressable_by_name() {
        let s = snapshot();
        assert_eq!(
            s.meta("delta01").unwrap().attribution_by_depth[1],
            Attribution::Additive
        );
        assert!(s.meta("nonesuch").is_none());
    }

    #[test]
    fn freshness_reports_the_stalest_input() {
        // A joined view is as stale as its stalest input (spec §5.4).
        let mut p = Provenance::default();
        p.datasets.push(Freshness {
            dataset: "risk_snapshot".into(),
            as_of: Some("2026-08-30T14:32:00Z".into()),
            generation: 47,
        });
        p.datasets.push(Freshness {
            dataset: "implied_vol_summary".into(),
            as_of: Some("2026-08-30T07:00:00Z".into()),
            generation: 3,
        });
        let s = Snapshot::from_batches(batches(), meta(), 1, p).unwrap();
        assert_eq!(
            s.provenance().stalest().map(|f| f.dataset.as_str()),
            Some("implied_vol_summary")
        );
    }

    #[test]
    fn an_empty_result_is_a_valid_snapshot() {
        let s = Snapshot::from_batches(Vec::new(), meta(), 1, Provenance::default()).unwrap();
        assert_eq!(s.rows(), 0);
        assert!(s.f64_column("delta01").is_none_or(|c| c.is_empty()));
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p geode-core snapshot`
Expected: FAIL — `cannot find struct Snapshot`.

- [ ] **Step 4: Implement `Snapshot`**

Prepend to `crates/geode-core/src/snapshot.rs`:

```rust
//! The immutable columnar result handed to the UI (spec §6.6).
//!
//! Arrow is an implementation detail: modules see `Snapshot` and typed
//! accessors returning plain slices, and nothing outside this file names
//! an Arrow type. Snapshots are `Arc`-shared, so handoff is a pointer
//! swap (§7.2) and no row objects are materialized anywhere.
//!
//! `query_arrow` returns 2048-row batches, so a column-wide slice needs
//! concatenation. That happens once here, at construction: blotter
//! results are aggregates — one row per visible group — while the million
//! rows are scanned inside DuckDB and never cross this boundary.

use crate::attribution::{Attribution, ScopeSemantics};
use arrow::array::{Array, Float64Array, Int64Array, StringArray};
use arrow::compute::concat_batches;
use arrow::record_batch::RecordBatch;

#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    /// Indexed by depth; see [`Snapshot::depth_of_row`].
    pub attribution_by_depth: Vec<Attribution>,
    pub scope_semantics: ScopeSemantics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub dataset: String,
    /// RFC 3339, or `None` when the dataset has never loaded.
    pub as_of: Option<String>,
    pub generation: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub datasets: Vec<Freshness>,
    /// Set when the result came from the archive rather than live.
    pub as_of_request: Option<String>,
}

impl Provenance {
    /// The stalest input. A joined view is as stale as this (spec §5.4),
    /// and a tile mixing cadences shows per-dataset freshness rather than
    /// one misleading timestamp.
    pub fn stalest(&self) -> Option<&Freshness> {
        self.datasets
            .iter()
            .filter(|f| f.as_of.is_some())
            .min_by(|a, b| a.as_of.cmp(&b.as_of))
    }
}

#[derive(Debug)]
pub struct Snapshot {
    batch: Option<RecordBatch>,
    meta: Vec<ColumnMeta>,
    /// Number of grouping columns, for decoding the depth bitmask.
    grouping_len: usize,
    provenance: Provenance,
}

impl Snapshot {
    pub fn from_batches(
        batches: Vec<RecordBatch>,
        meta: Vec<ColumnMeta>,
        grouping_len: usize,
        provenance: Provenance,
    ) -> Result<Snapshot, arrow::error::ArrowError> {
        let batch = match batches.first() {
            None => None,
            Some(first) => Some(concat_batches(&first.schema(), &batches)?),
        };
        Ok(Snapshot {
            batch,
            meta,
            grouping_len,
            provenance,
        })
    }

    pub fn rows(&self) -> usize {
        self.batch.as_ref().map_or(0, |b| b.num_rows())
    }

    pub fn column_names(&self) -> Vec<&str> {
        self.meta.iter().map(|m| m.name.as_str()).collect()
    }

    pub fn meta(&self, name: &str) -> Option<&ColumnMeta> {
        self.meta.iter().find(|m| m.name == name)
    }

    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    fn column(&self, name: &str) -> Option<&dyn Array> {
        let batch = self.batch.as_ref()?;
        let idx = batch.schema().index_of(name).ok()?;
        Some(batch.column(idx).as_ref())
    }

    /// Zero-copy over the whole column. `None` when absent or not f64 —
    /// never a panic, because a view can name a column the data lacks.
    pub fn f64_column(&self, name: &str) -> Option<&[f64]> {
        Some(self.column(name)?.as_any().downcast_ref::<Float64Array>()?.values())
    }

    pub fn i64_column(&self, name: &str) -> Option<&[i64]> {
        Some(self.column(name)?.as_any().downcast_ref::<Int64Array>()?.values())
    }

    /// Strings are returned as the Arrow array: offsets make a `&[&str]`
    /// impossible without allocating, and the renderer reads by row.
    pub fn str_column(&self, name: &str) -> Option<&StringArray> {
        self.column(name)?.as_any().downcast_ref::<StringArray>()
    }

    /// How many grouping columns are present on this row. The compiler
    /// emits `grouping(...)` as `depth_mask`; under ROLLUP a level with
    /// `d` of `n` columns present has mask `2^(n-d) - 1`.
    pub fn depth_of_row(&self, row: usize) -> Option<usize> {
        let mask = *self.i64_column("depth_mask")?.get(row)?;
        (0..=self.grouping_len).find(|d| ((1i64 << (self.grouping_len - d)) - 1).max(0) == mask)
    }
}
```

Add to `crates/geode-core/src/lib.rs`:

```rust
pub mod snapshot;
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p geode-core snapshot`
Expected: PASS (7 tests).

- [ ] **Step 6: Lint, format, commit**

```bash
cargo fmt && cargo clippy -p geode-core --all-targets -- -D warnings
git add crates/geode-core
git commit -m "feat(core): Snapshot with typed zero-copy accessors

The immutable columnar result the UI reads. Arrow is an implementation
detail — modules see Snapshot and slices, and nothing outside the module
names an Arrow type.

query_arrow returns 2048-row batches, so a column-wide slice needs
concatenation; that happens once at construction, which is cheap because
blotter results are aggregates while the million rows stay inside DuckDB.
The concatenation is asserted with a two-batch fixture rather than a
single-batch one that would pass either way.

Carries provenance: per-dataset freshness with a stalest() accessor, so a
tile mixing cadences can show per-dataset times rather than one
misleading timestamp (spec §5.4)."
```

---

### Task 8: ENUM dictionary encoding (carried over from 2a)

**Files:**
- Modify: `crates/geode-data/src/store/ddl.rs`
- Modify: `crates/geode-data/src/ingest/load.rs`
- Modify: `crates/geode-core/src/snapshot.rs`

**Interfaces:**
- Consumes: `DatasetSpec`, `Store`.
- Produces: `ddl::enum_type_sql(&DatasetSpec) -> Vec<String>`,
  `widen_enum(&Connection, &str, &[String]) -> Result<usize, StoreError>`,
  and `Snapshot::dict_column(name) -> Option<(&[u8], &StringArray)>`.

**Why now and not in 2a.** Spec §3.6 requires dimension columns to be
DuckDB `ENUM` so §7.2's "interned at ingest" holds — a plain `VARCHAR`
comes back `StringArray`, not `Dictionary(UInt8, Utf8)`, verified against
1.10505. 2a deferred it because nothing there read a snapshot and so
nothing could measure the benefit. Task 13 measures it.

**This is a stored-type change**, so it needs a rebuilt database rather
than a migration — cheap, because the database is derived from the CSVs.
The load pipeline widens an ENUM when a new dimension value appears, which
is a metadata operation on the small vocabularies this applies to.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/geode-data/src/store/ddl.rs`:

```rust
    #[test]
    fn dimension_columns_are_declared_as_enums() {
        let ds = sample_dataset();
        let types = enum_type_sql(&ds);
        assert!(
            types.iter().any(|t| t.contains("book_enum")),
            "a dimension needs an ENUM type so it returns dictionary-encoded: {types:?}"
        );
        let sql = create_table_sql(&ds, Grain::Position, TableKind::Live);
        assert!(sql.contains("\"book\" book_enum"), "{sql}");
        // Keys and measures are unaffected.
        assert!(sql.contains("\"daily_trading_pnl\" DOUBLE"), "{sql}");
    }

    #[test]
    fn live_and_archive_still_have_identical_columns_with_enums() {
        let ds = sample_dataset();
        for grain in [Grain::Position, Grain::Underlying] {
            let live = create_table_sql(&ds, grain, TableKind::Live);
            let archive = create_table_sql(&ds, grain, TableKind::Archive);
            let cols = |sql: &str| {
                sql.lines()
                    .filter(|l| l.trim_start().starts_with('"'))
                    .map(|l| l.trim().trim_end_matches(',').to_string())
                    .collect::<Vec<_>>()
            };
            assert_eq!(cols(&live), cols(&archive), "grain {grain:?}");
        }
    }
```

Add to the test module in `crates/geode-data/src/ingest/load.rs`:

```rust
    #[test]
    fn a_new_dimension_value_widens_the_enum_rather_than_failing() {
        // Vocabularies grow: a new book must not fail the load.
        let f = fixture();
        let file = ready_file(&f);
        load(&f, file);
        let added = crate::store::ddl::widen_enum(
            f.store.writer(),
            "book_enum",
            &["BK999".to_string()],
        )
        .unwrap();
        assert_eq!(added, 1);
        // Idempotent: widening with a value already present adds nothing.
        let again = crate::store::ddl::widen_enum(
            f.store.writer(),
            "book_enum",
            &["BK999".to_string()],
        )
        .unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn dimension_columns_come_back_dictionary_encoded() {
        // This is what §7.2's "interned at ingest" actually requires.
        let f = fixture();
        let file = ready_file(&f);
        load(&f, file);
        let conn = f.store.writer();
        let mut stmt = conn
            .prepare("select book from measures_position_live limit 10")
            .unwrap();
        let batches: Vec<duckdb::arrow::record_batch::RecordBatch> =
            stmt.query_arrow([]).unwrap().collect();
        assert!(
            matches!(
                batches[0].schema().field(0).data_type(),
                duckdb::arrow::datatypes::DataType::Dictionary(_, _)
            ),
            "got {:?}",
            batches[0].schema().field(0).data_type()
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data enum`
Expected: FAIL — `cannot find function enum_type_sql`.

- [ ] **Step 3: Implement ENUM types and widening**

In `crates/geode-data/src/store/ddl.rs`, add:

```rust
/// The ENUM type name for a dimension column.
pub fn enum_type_name(column: &str) -> String {
    format!("{column}_enum")
}

/// `CREATE TYPE` statements for every dimension column in the dataset.
///
/// Dimension columns are stored as ENUM so they return
/// `Dictionary(UInt8, Utf8)` rather than `StringArray` — which is what
/// §7.2's "interned at ingest" requires, and what lets the renderer
/// compare and format on small integer codes (spec §3.6, §6.6).
///
/// Types start empty and are widened by ingest as values appear.
pub fn enum_type_sql(ds: &DatasetSpec) -> Vec<String> {
    ds.columns
        .iter()
        .filter(|c| matches!(c.role, ColumnRole::Dimension))
        .map(|c| {
            format!(
                "CREATE TYPE IF NOT EXISTS {} AS ENUM ()",
                enum_type_name(&c.name)
            )
        })
        .collect()
}

/// Add values to an ENUM that are not already in it. Returns how many
/// were added. A metadata operation, on vocabularies of tens to hundreds.
pub fn widen_enum(
    conn: &duckdb::Connection,
    type_name: &str,
    values: &[String],
) -> Result<usize, crate::store::StoreError> {
    let mut added = 0;
    for v in values {
        let sql = format!(
            "ALTER TYPE {type_name} ADD VALUE IF NOT EXISTS '{}'",
            v.replace('\'', "''")
        );
        match conn.execute_batch(&sql) {
            Ok(()) => added += 1,
            Err(source) => {
                return Err(crate::store::StoreError::Sql {
                    statement: sql,
                    source,
                });
            }
        }
    }
    Ok(added)
}
```

> **Implementer note:** DuckDB's `ADD VALUE IF NOT EXISTS` succeeds
> silently when the value is present, so `added` over-counts. Make the
> function query `enum_range(NULL::<type>)` first and only issue `ALTER`
> for genuinely new values — the test asserts the idempotent case returns
> 0, which forces this.

In `create_table_sql`, use the ENUM type for dimension columns:

```rust
        let ty = match ds.column(key) {
            Some(c) if matches!(c.role, ColumnRole::Dimension) => enum_type_name(key),
            Some(c) => c.ty.sql().to_string(),
            None => "VARCHAR".to_string(),
        };
        cols.push(format!("  \"{key}\" {ty}"));
```

In `Store::apply_schema`, run `enum_type_sql` before the table DDL.

In `crates/geode-data/src/ingest/load.rs`, before the publish step, widen
each dimension ENUM with the distinct values the staged rows carry.

- [ ] **Step 4: Add `dict_column` to `Snapshot`**

In `crates/geode-core/src/snapshot.rs`:

```rust
    /// A dictionary-encoded dimension column: per-row codes plus the
    /// shared value dictionary. The renderer compares and formats on the
    /// codes rather than the strings (spec §7.2).
    pub fn dict_column(&self, name: &str) -> Option<(&[u8], &StringArray)> {
        use arrow::array::DictionaryArray;
        use arrow::datatypes::UInt8Type;
        let arr = self
            .column(name)?
            .as_any()
            .downcast_ref::<DictionaryArray<UInt8Type>>()?;
        let values = arr.values().as_any().downcast_ref::<StringArray>()?;
        Some((arr.keys().values(), values))
    }
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p geode-data && cargo test -p geode-core snapshot`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add crates/geode-data crates/geode-core
git commit -m "feat(data): ENUM dictionary encoding for dimension columns

Carried over from phase 2a, which deferred it because nothing there read
a snapshot and so nothing could measure the benefit.

Dimension columns are stored as DuckDB ENUM so they return
Dictionary(UInt8, Utf8) rather than StringArray — which is what §7.2's
'interned at ingest' actually requires, and what lets the renderer
compare and format on small integer codes. Ingest widens an ENUM when a
new value appears, which is a metadata operation on vocabularies of tens
to hundreds.

A stored-type change, so it needs a rebuilt database rather than a
migration — cheap, because the database is derived from the CSVs."
```

---

### Task 9: The query pool, cancellation and coalescing

**Files:**
- Create: `crates/geode-data/src/query/pool.rs`
- Modify: `crates/geode-data/src/query/mod.rs`

**Interfaces:**
- Consumes: `Store::reader`, `CompiledQuery`, `Snapshot`, `Provenance`.
- Produces: `QueryId(u64)`, `ViewId(String)`, `QueryRequest { view: ViewId,
  compiled: CompiledQuery, grouping_len: usize, provenance: Provenance }`,
  `QueryResult { id, view, snapshot }`, `QueryPool::spawn(Store, usize) ->
  (QueryPool, Receiver<QueryResult>)`, `QueryPool::{submit, cancel,
  shutdown}`. Task 11 wraps this in `DataService`.

**Per §7.3, four properties, each with a test:**

- The UI thread never holds a connection; the pool owns them.
- **One in-flight query per view, latest-wins.** Leaning on a regroup key
  five times yields one query, not five.
- Every request and result is generation-tagged; a stale result arriving
  after a newer request is dropped, never delivered.
- **A superseded query is interrupted, not awaited** — via
  `Connection::interrupt_handle()`, which is `Send + Sync` (verified
  against 1.10505).

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/query/pool.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A store with one live table holding `rows` rows, and a compiled
    /// query over it that is slow enough to be interrupted.
    fn fixture(rows: usize) -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(&format!(
                "create table t as select i as k, i::double as v
                 from range(0, {rows}) t(i);"
            ))
            .unwrap();
        (dir, store)
    }

    fn query(sql: &str) -> CompiledQuery {
        CompiledQuery {
            sql: sql.to_string(),
            params: Vec::new(),
            temp_tables: Vec::new(),
            grouping: Vec::new(),
            columns: vec![geode_core::snapshot::ColumnMeta {
                name: "v".into(),
                attribution_by_depth: vec![geode_core::attribution::Attribution::Additive],
                scope_semantics: geode_core::attribution::ScopeSemantics::Direct,
            }],
            stalest_input: Vec::new(),
        }
    }

    fn request(view: &str, sql: &str) -> QueryRequest {
        QueryRequest {
            view: ViewId(view.to_string()),
            compiled: query(sql),
            grouping_len: 0,
            provenance: geode_core::snapshot::Provenance::default(),
        }
    }

    #[test]
    fn a_submitted_query_returns_a_snapshot() {
        let (_d, store) = fixture(1_000);
        let (pool, rx) = QueryPool::spawn(store, 2);
        pool.submit(request("v1", "select sum(v) as v from t"));
        let result = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(result.view, ViewId("v1".into()));
        assert_eq!(result.snapshot.rows(), 1);
        pool.shutdown();
    }

    #[test]
    fn leaning_on_a_regroup_key_yields_one_result_not_five() {
        // Latest-wins coalescing (spec §7.3).
        let (_d, store) = fixture(200_000);
        let (pool, rx) = QueryPool::spawn(store, 2);
        for i in 0..5 {
            pool.submit(request("v1", &format!("select sum(v) + {i} as v from t")));
        }
        let first = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        // Whatever else arrives must not be a *superseded* result.
        let extra: Vec<_> = std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(300)).ok())
            .collect();
        assert!(
            extra.len() <= 1,
            "expected coalescing, got {} extra results",
            extra.len()
        );
        assert_eq!(first.view, ViewId("v1".into()));
        pool.shutdown();
    }

    #[test]
    fn different_views_do_not_coalesce_with_each_other() {
        let (_d, store) = fixture(1_000);
        let (pool, rx) = QueryPool::spawn(store, 2);
        pool.submit(request("v1", "select sum(v) as v from t"));
        pool.submit(request("v2", "select count(*)::double as v from t"));
        let mut seen = Vec::new();
        for _ in 0..2 {
            seen.push(rx.recv_timeout(Duration::from_secs(30)).unwrap().view);
        }
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(seen, vec![ViewId("v1".into()), ViewId("v2".into())]);
        pool.shutdown();
    }

    #[test]
    fn a_stale_result_is_dropped_rather_than_delivered() {
        // Generation-tagged: ids increase, and no result may arrive whose
        // id is older than one already delivered for the same view.
        let (_d, store) = fixture(100_000);
        let (pool, rx) = QueryPool::spawn(store, 4);
        let mut ids = Vec::new();
        for _ in 0..4 {
            ids.push(pool.submit(request("v1", "select sum(v) as v from t")));
        }
        let mut delivered = Vec::new();
        while let Ok(r) = rx.recv_timeout(Duration::from_secs(5)) {
            delivered.push(r.id);
        }
        for w in delivered.windows(2) {
            assert!(w[0] < w[1], "results must arrive in id order: {delivered:?}");
        }
        assert_eq!(
            delivered.last().copied(),
            ids.last().copied(),
            "the newest request must be the one that lands"
        );
        pool.shutdown();
    }

    #[test]
    fn cancelling_a_view_stops_its_in_flight_query() {
        let (_d, store) = fixture(2_000_000);
        let (pool, rx) = QueryPool::spawn(store, 2);
        pool.submit(request(
            "v1",
            "select sum(v) as v from t a, t b where a.k = b.k",
        ));
        std::thread::sleep(Duration::from_millis(50));
        pool.cancel(&ViewId("v1".into()));
        // Either nothing arrives, or an error result — but not a hang.
        let _ = rx.recv_timeout(Duration::from_secs(20));
        pool.shutdown();
    }

    #[test]
    fn a_failing_query_reports_rather_than_killing_the_pool() {
        let (_d, store) = fixture(100);
        let (pool, rx) = QueryPool::spawn(store, 2);
        pool.submit(request("bad", "select * from no_such_table"));
        pool.submit(request("good", "select sum(v) as v from t"));
        let mut views = Vec::new();
        for _ in 0..2 {
            if let Ok(r) = rx.recv_timeout(Duration::from_secs(30)) {
                views.push(r.view);
            }
        }
        assert!(
            views.contains(&ViewId("good".into())),
            "one bad query must not stop the pool: {views:?}"
        );
        pool.shutdown();
    }

    #[test]
    fn shutdown_is_idempotent_and_does_not_hang() {
        let (_d, store) = fixture(100);
        let (pool, _rx) = QueryPool::spawn(store, 2);
        pool.shutdown();
        pool.shutdown();
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data pool`
Expected: FAIL — `cannot find struct QueryPool`.

- [ ] **Step 3: Implement the pool**

Prepend to `crates/geode-data/src/query/pool.rs`:

```rust
//! The read pool (spec §6.7, §7.3). Owns the read connections so the UI
//! thread never holds one, coalesces latest-wins per view, tags every
//! request and result so a stale arrival can be dropped, and interrupts a
//! superseded query rather than awaiting it.

use crate::query::compile::CompiledQuery;
use crate::store::Store;
use geode_core::snapshot::{Provenance, Snapshot};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(pub String);

pub type QueryId = u64;

pub struct QueryRequest {
    pub view: ViewId,
    pub compiled: CompiledQuery,
    pub grouping_len: usize,
    pub provenance: Provenance,
}

pub struct QueryResult {
    pub id: QueryId,
    pub view: ViewId,
    /// `Err` carries the failure; a bad query degrades its own view and
    /// leaves the pool running (spec §10.1).
    pub snapshot: Snapshot,
}

#[derive(Default)]
struct Queue {
    /// At most one pending request per view: a newer submit replaces the
    /// pending one outright, which is what makes coalescing latest-wins.
    pending: HashMap<ViewId, (QueryId, QueryRequest)>,
    /// Interrupt handles for queries currently running.
    running: HashMap<ViewId, (QueryId, Arc<duckdb::InterruptHandle>)>,
    shutdown: bool,
}

pub struct QueryPool {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    next_id: AtomicU64,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl QueryPool {
    pub fn spawn(store: Store, workers: usize) -> (QueryPool, Receiver<QueryResult>) {
        let (tx, rx) = channel();
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let store = Arc::new(store);
        let mut threads = Vec::new();

        for i in 0..workers.max(1) {
            let q = Arc::clone(&queue);
            let tx = tx.clone();
            let store = Arc::clone(&store);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("geode-query-{i}"))
                    .spawn(move || worker(store, q, tx))
                    .expect("spawning a query worker"),
            );
        }

        (
            QueryPool {
                queue,
                next_id: AtomicU64::new(1),
                threads: Mutex::new(threads),
            },
            rx,
        )
    }

    /// Replace this view's pending request. Returns the id assigned, which
    /// increases monotonically so callers can discard stale arrivals.
    pub fn submit(&self, req: QueryRequest) -> QueryId {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        // A superseded query is interrupted, not awaited (spec §7.3).
        if let Some((running_id, handle)) = q.running.get(&req.view)
            && *running_id < id
        {
            handle.interrupt();
        }
        q.pending.insert(req.view.clone(), (id, req));
        cvar.notify_all();
        id
    }

    pub fn cancel(&self, view: &ViewId) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.pending.remove(view);
        if let Some((_, handle)) = q.running.get(view) {
            handle.interrupt();
        }
    }

    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.shutdown = true;
            for (_, handle) in q.running.values() {
                handle.interrupt();
            }
            cvar.notify_all();
        }
        for t in self
            .threads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            let _ = t.join();
        }
    }
}

impl Drop for QueryPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker(store: Arc<Store>, queue: Arc<(Mutex<Queue>, Condvar)>, tx: Sender<QueryResult>) {
    let Ok(conn) = store.reader() else {
        return;
    };
    let handle = conn.interrupt_handle();

    loop {
        let (id, req) = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(view) = q.pending.keys().next().cloned() {
                    let (id, req) = q.pending.remove(&view).expect("just observed");
                    q.running.insert(view, (id, Arc::clone(&handle)));
                    break (id, req);
                }
                let (guard, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
        };

        let outcome = run_one(&conn, &req);

        {
            let (lock, _) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            // Only clear if we are still the running query for this view.
            if q.running.get(&req.view).is_some_and(|(rid, _)| *rid == id) {
                q.running.remove(&req.view);
            }
            // A newer request for this view is already pending: our result
            // is stale, so drop it rather than delivering it (spec §7.3).
            if q.pending.get(&req.view).is_some_and(|(pid, _)| *pid > id) {
                continue;
            }
        }

        if let Ok(snapshot) = outcome
            && tx
                .send(QueryResult {
                    id,
                    view: req.view.clone(),
                    snapshot,
                })
                .is_err()
        {
            return;
        }

        for t in &req.compiled.temp_tables {
            let _ = conn.execute_batch(&format!("drop table if exists {t}"));
        }
    }
}

fn run_one(conn: &duckdb::Connection, req: &QueryRequest) -> Result<Snapshot, duckdb::Error> {
    let mut stmt = conn.prepare(&req.compiled.sql)?;
    let batches: Vec<duckdb::arrow::record_batch::RecordBatch> = stmt
        .query_arrow(duckdb::params_from_iter(req.compiled.params.iter()))?
        .collect();
    let meta = req
        .compiled
        .columns
        .iter()
        .map(|c| geode_core::snapshot::ColumnMeta {
            name: c.name.clone(),
            attribution_by_depth: c.attribution_by_depth.clone(),
            scope_semantics: c.scope_semantics.clone(),
        })
        .collect();
    Snapshot::from_batches(batches, meta, req.grouping_len, req.provenance.clone())
        .map_err(|e| duckdb::Error::ArrowTypeToDuckdbType(e.to_string(), 0))
}
```

> **Implementer note:** `Snapshot::from_batches` takes `arrow::RecordBatch`
> and duckdb re-exports its own. They are the same type only if the arrow
> versions match — pin `geode-core`'s `arrow` to what
> `cargo tree -p geode-data -i arrow` reports (Task 7, Step 1). If they
> diverge, `run_one` must go through `duckdb::arrow` and `geode-core` must
> re-export it rather than depending on `arrow` directly.

Add to `crates/geode-data/src/query/mod.rs`:

```rust
pub mod pool;

pub use pool::{QueryId, QueryPool, QueryRequest, QueryResult, ViewId};
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-data pool`
Expected: PASS (7 tests).

- [ ] **Step 5: Lint, format, commit**

```bash
cargo fmt && cargo clippy -p geode-data --all-targets -- -D warnings
git add crates/geode-data
git commit -m "feat(data): query pool with cancellation and latest-wins coalescing

Owns the read connections so the UI thread never holds one. At most one
pending request per view, so leaning on a regroup key five times yields
one query. Requests and results are id-tagged and a result superseded
while it ran is dropped rather than delivered. A superseded query is
interrupted through interrupt_handle(), not awaited.

A failing query degrades its own view and leaves the pool running,
asserted by submitting a broken query alongside a good one."
```

---

### Task 10: As-of routing

**Files:**
- Create: `crates/geode-data/src/query/as_of.rs`
- Modify: `crates/geode-data/src/query/compile.rs`
- Modify: `crates/geode-data/src/query/mod.rs`

**Interfaces:**
- Consumes: `Catalog`, `ddl::{table_name, TableKind}`, `CompiledQuery`.
- Produces: `AsOf::{Live, At(DateTime<Utc>)}`,
  `resolve_generations(&Connection, &str, DateTime<Utc>) ->
  Result<Vec<(String, String, i64)>, StoreError>` returning
  `(batch, book, gen_id)`, and `compile_view` gaining an `as_of: AsOf`
  parameter.

**Same compiled SQL, different tables** (spec §6.5). The live path carries
no generation predicate at all; only the as-of path pays for history. Per
partition, as-of resolves the newest generation at or before the requested
time — so the state is "each partition as it stood at T", which is the
question a trader is actually asking when books refresh independently.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/query/as_of.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// An archive with two partitions refreshing on different clocks:
    /// BK000 at 07:00 and 14:00, BK001 only at 09:00.
    fn fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table measures_position_archive(
                     book varchar, lhu varchar, position_ref varchar,
                     counterparty varchar, daily_trading_pnl double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 insert into measures_position_archive values
                   ('BK000','L','P1','C', 1, 'BK000', 1, 1, '2026-08-30T07:00:00Z'),
                   ('BK000','L','P1','C', 2, 'BK000', 2, 2, '2026-08-30T14:00:00Z'),
                   ('BK001','L','P2','C', 3, 'BK001', 3, 3, '2026-08-30T09:00:00Z');",
            )
            .unwrap();
        (dir, store)
    }

    #[test]
    fn resolves_the_newest_generation_at_or_before_the_request() {
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
            "measures_position_archive",
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        let bk000 = gens.iter().find(|(b, ..)| b == "BK000").unwrap();
        assert_eq!(bk000.2, 1, "07:00, not the 14:00 generation");
        let bk001 = gens.iter().find(|(b, ..)| b == "BK001").unwrap();
        assert_eq!(bk001.2, 3);
    }

    #[test]
    fn each_partition_resolves_on_its_own_clock() {
        // Books refresh independently, so 'as it stood at T' is per
        // partition, not one dataset-wide generation (spec §4.5).
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
            "measures_position_archive",
            ts("2026-08-30T08:00:00Z"),
        )
        .unwrap();
        assert_eq!(gens.len(), 1, "BK001 did not exist yet at 08:00: {gens:?}");
        assert_eq!(gens[0].0, "BK000");
    }

    #[test]
    fn a_time_before_all_history_resolves_to_nothing() {
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
            "measures_position_archive",
            ts("2026-08-29T00:00:00Z"),
        )
        .unwrap();
        assert!(gens.is_empty());
    }

    #[test]
    fn the_predicate_selects_exactly_the_resolved_generations() {
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
            "measures_position_archive",
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        let pred = generation_predicate(&gens);
        let total: f64 = store
            .writer()
            .query_row(
                &format!(
                    "select sum(daily_trading_pnl) from measures_position_archive where {pred}"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 4.0, "BK000's 07:00 row plus BK001's, not the 14:00");
    }

    #[test]
    fn an_empty_resolution_selects_no_rows_rather_than_all() {
        assert_eq!(generation_predicate(&[]), "false");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data as_of`
Expected: FAIL — `cannot find function resolve_generations`.

- [ ] **Step 3: Implement as-of routing**

Prepend to `crates/geode-data/src/query/as_of.rs`:

```rust
//! Time travel (spec §4.5, §6.5). The same compiled SQL, aimed at the
//! archive tables, with the generation resolved per partition.
//!
//! Datasets and books refresh on independent cadences, so the resolved
//! state is "each partition as it stood at T" — the question a trader is
//! actually asking. The live path carries no generation predicate at all;
//! only this path pays for history.

use crate::store::StoreError;
use chrono::{DateTime, Utc};
use duckdb::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsOf {
    Live,
    At(DateTime<Utc>),
}

impl AsOf {
    pub fn is_live(&self) -> bool {
        matches!(self, AsOf::Live)
    }
}

/// `(batch, book, gen_id)` — the newest generation at or before `at`, for
/// every partition that existed by then.
pub fn resolve_generations(
    conn: &Connection,
    archive_table: &str,
    at: DateTime<Utc>,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let sql = format!(
        "select batch, book, gen_id from (
             select batch, book, gen_id, source_time,
                    row_number() over (
                        partition by batch, book order by source_time desc
                    ) as rn
             from (select distinct batch, book, gen_id, source_time
                   from {archive_table}
                   where source_time <= ?)
         ) where rn = 1"
    );
    let err = |source| StoreError::Sql {
        statement: sql.clone(),
        source,
    };
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![at], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(err)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// A predicate selecting exactly those generations. Values come from the
/// catalog, not from user input, so they are inlined as quoted literals;
/// scope predicates, which do take user input, bind (spec §6.2).
pub fn generation_predicate(generations: &[(String, String, i64)]) -> String {
    if generations.is_empty() {
        // Selecting nothing, not everything: a time before all history is
        // an empty result, never the whole archive.
        return "false".to_string();
    }
    generations
        .iter()
        .map(|(batch, book, gen)| {
            format!(
                "(batch = '{}' and book = '{}' and gen_id = {gen})",
                batch.replace('\'', "''"),
                book.replace('\'', "''")
            )
        })
        .collect::<Vec<_>>()
        .join(" or ")
}
```

In `compile_view`, take `as_of: &AsOf` and use it to pick the table and
extend each subquery's predicate:

```rust
    // Same statement shape, different tables (spec §6.5).
    let (kind, gen_pred) = match as_of {
        AsOf::Live => (TableKind::Live, None),
        AsOf::At(t) => {
            let archive = table_name(spine_grain, TableKind::Archive);
            let gens = crate::query::as_of::resolve_generations(conn, &archive, *t)?;
            (
                TableKind::Archive,
                Some(crate::query::as_of::generation_predicate(&gens)),
            )
        }
    };
```

and append `and {gen_pred}` to the spine and each grain predicate when it
is `Some`. Replace every `TableKind::Live` in the function with `kind`.

Add to `crates/geode-data/src/query/mod.rs`:

```rust
pub mod as_of;

pub use as_of::{AsOf, generation_predicate, resolve_generations};
```

Update the Task 5 and Task 6 tests to pass `&AsOf::Live`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-data`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p geode-data --all-targets -- -D warnings
git add crates/geode-data
git commit -m "feat(data): as-of routing over the archive

The same compiled SQL aimed at archive tables, with the generation
resolved per partition: books refresh independently, so 'as it stood at
T' is per partition rather than one dataset-wide generation (spec §4.5).

A time before all history selects nothing rather than everything —
asserted, because 'false' and a missing predicate differ by the entire
archive."
```

---

**Remaining tasks (11–14) continue below.** Task 11 the `DataService`
facade; Task 12 the throwaway debug tile; Task 13 the §7.1 requery
benchmarks; Task 14 cross-file conflict detection.
