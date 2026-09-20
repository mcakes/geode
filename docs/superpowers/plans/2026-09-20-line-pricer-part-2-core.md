# Line Pricer Part 2 (Core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure core of the line pricer as a new crate, `geode-pricer`: the `Sheet` struct of arrays, every `Edit` with its inverse, the shorthand parser and renderer, the seven package templates, package folding, the column vocabulary with per-cell text, the `pricer_views` config doc and `ColumnPlan`, the storage round trip (`to_rows`/`from_rows`) and the criterion benches. No tile, no gpui, no `DataHandle`, no storage wiring (Parts 3 and 4).

**Architecture:** `geode-pricer` is a module crate (spec §4) that in this part depends on `geode-core` alone; Part 3 adds `geode-shell` and gpui when the tile lands. Everything here lives under `geode_pricer::core` and names no element, entity or window, so every test is a plain `#[test]`. The `Sheet` is struct-of-arrays in sheet order with a package's legs contiguous after it (spec §6.1); `Sheet::apply(Edit) -> Result<Undo, EditError>` is the one mutation door and decides which lines are re-requested by comparing `Sheet::request(row)` before and after (spec §9.3). The parser and renderer are pure functions over `geode_core::pricing`'s vocabulary; the views doc is read like every other named config doc (`from_doc(&MergedDoc)` with path-carrying diagnostics); storage is `DocumentRows` in and out.

**Tech Stack:** Rust 2024, `chrono 0.4.42` (third-Friday arithmetic and `priced_at`), `toml 1.1.4` (the views doc), `geode_core::{pricing, view, format, document, config, schema}`, criterion `0.8.2` benches with `harness = false`.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-line-pricer-design.md` — §2 (rulings), §6 (the sheet, this part's whole subject), §7.2 (the row shape `to_rows` must produce), §9.3 (what changes a request), §12 (the `geode-pricer` core test list and the harness list), §14 part 2, and §16 (as built, Part 1: the vocabulary is `geode_core::pricing`). Read §6 and §9.3 before starting.

## Decisions made in planning

Each is a place where §6/§7 as written could not be implemented literally, resolved here so the executor does not re-decide it. Task 11 records them all in the spec's as-built section.

1. **`RowSpec` is an enum**, `Line(LineSpec) | Package { template, legs: Vec<LineSpec> }`, and `Parsed` (the parser's answer) IS a `RowSpec`. §6.2's `Vec<RowSpec>` on `Insert` is then a list of roots (or, at a leg place, of lines).
2. **An insert position is a `Place`, not a bare index.** `Place::Root { at }` (a flat index that is a root boundary) or `Place::Leg { package, leg }` (a leg index inside the package at flat row `package`). §6.2's `Insert { at }` cannot tell "the first leg of package P" from "a root after P" when P has no legs, and §8.4's `o` needs both.
3. **The inverse of `Remove` is `Edit::Restore { at, rows: Vec<RowRecord> }`**, a full-row record (id, kind, parent id, instrument, qty, own shifts, revision, result, state, priced_at). `Undo { inverse: Vec<Edit> }` is applied in order by `Sheet::undo`, which returns the redo `Undo` (the collected inverses, reversed). `Group` carries `id: Option<LineId>` so the inverse of `Ungroup` restores the same package id; a caller passes `None` and `apply` refuses an id already in use.
4. **`SetSpotOverride` marks stale inside `apply`**, not in the tile: §9.3 says the request is unchanged for an override (overrides ride in `PriceParams`), so `apply` bumps and stales every line on that underlying explicitly when the level changed. The core is the right owner; §9.3's "the tile marks" reads "the sheet marks".
5. **`Sheet::refresh` is `Refresh { Default, Off, Every(Duration) }`**, not `Option<Duration>`: §7.2 stores three states (`""`, `30s`, `off`) and §9.4 distinguishes them.
6. **Spot overrides persist as one utf8 attribute, `spot_overrides`**, encoded `UND=LEVEL;UND=LEVEL` (empty when none) and parsed back by `storage::parse_overrides`. Ruling 1 was amended after §7.2's column list was written, which does not mention overrides; a map has no per-row home in a document, and an attribute is the document-level slot. Plain data, no arithmetic. **Flagged for the user in the handoff** — a second attribute per underlying is not possible, so this encoding is the alternative to not persisting them.
7. **`ColumnPlan::build(view)` takes no sheet.** §6.5 writes `build(view, sheet)`, but the column set never depends on the sheet (a column that does not apply to a row paints blank, §6.5), and an unused parameter is a clippy error. Part 3 adds one if the tree column turns out to need it.
8. **A package may be empty.** `Remove` of a package's last leg leaves the package; `Group` requires `count >= 1`; an empty package is `Fresh` with no result and paints blank sums. This removes every "and the package too" special case from `Remove`/`Restore`/undo.
9. **Package state precedence is `Failed` > `Stale` > `Fresh`**, and a package's result is `Some` only when every leg has one. A failed leg makes the sum uncomputable, so it must win over a leg that is merely repricing.
10. **`cell_text` lives in the core** (`core::columns`), answering a `CellText { text, state }` per (row, column); Part 3 wraps it in `SharedString` and colours. It is the pure half of §8.2's `GridModel::build`, testable without a theme.
11. **A custom package's shorthand is its legs one per line**, joined with `\n` (§6.3), and `Sheet::shorthand(row)` on a package answers the template form only when the legs still match the table.
12. **The two bundled views are the crate's `BUILTIN_VIEWS` TOML constant**; Part 3 pushes it into `ConfigSources.builtin` when the factory lands, and §11's "the demo layer adds `pricer_views`" is unnecessary (the builtin layer already carries them; the demo adds nothing).
13. **`geode_core::pricing::Instrument` gains `vanilla()` (made `pub`) and `expiry()`**: the renderer and `cell_text` read the expiry through both variants.
14. **`config::merge::atomic_depth` gains `"pricer_views"`** at depth 1, so a desk or user layer overrides a view whole by name (§6.5 "Desk and user layers override by name").
15. **The `pricer_sheets` dataset declaration is a TOML constant in `core::storage` now**, parsed by `SchemaSpec::from_doc` in a test so `to_rows` is validated against the real declaration; Part 4 pushes it into the builtin layer.

## Global Constraints

- `geode-pricer` depends on `geode-core` (workspace), `chrono = "0.4.42"`, `toml = "1.1.4"` and nothing else in this part; dev-dependencies are `geode-core` with `features = ["test-support"]` and `criterion = "0.8.2"`. Never `geode-blotter`, never `geode-marketdata`, never `geode-diagnostics`, never `geode-pricing` (the mock is reached only through the data tier).
- `[lib] bench = false`; the bench target is `[[bench]] name = "core" harness = false`.
- The app performs no financial arithmetic (PHILOSOPHY §1 as amended). A package summing `qty × value` over its legs is aggregation; the third-Friday rule is a date convention (spec §6.3). Nothing here resolves a percent strike or a tenor; `Strike::Percent` and `Expiry::Tenor` pass through.
- Every mutation of a `Sheet` goes through `Sheet::apply`; there is no other `pub` mutator except `deliver` (a result landing) and `fold_packages` (a recompute). Test code constructs sheets through `apply` too.
- Struct-of-arrays: `Sheet` holds one `Vec` per field in sheet order; no per-row struct is stored (a `RowRecord` exists only in transit, inside an `Undo`).
- Diagnostics from the views reader carry `path: Some("pricer_views.<view>[.columns.<i>[.format.<key>]]")`, the 4c §19.5 grammar; `layer` and `file` are `None` (the loader fills them like every other reader's).
- Shorthand is case-insensitive on input and renders upper-case tokens (`-5 SPX Z26 95%/105% CS`); a tenor renders as `Expiry::tenor` stored it (lower-case).
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `cargo check -p geode-shell --features test-support --all-targets` pass at the end of every task. Skip `cargo bench --workspace --no-run` locally (CI runs it; `clippy --all-targets` type-checks the bench). Commit at the end of every task.
- `zsh scripts/mutation-check.sh --anchors-only` before the final merge (Task 11).
- Do not touch `geode-data`, `geode-shell`, the blotter, the market-data panel or the bridge. This part ends at a crate the app does not yet depend on; the only edits outside the new crate are `geode-core`'s `pricing.rs` (two accessors), `config/merge.rs` (one match arm), the workspace `Cargo.toml` (one member) and the docs.
- This plan is written against `main` at `99b2d95`.

---

## File map

| File | Responsibility |
|---|---|
| `Cargo.toml` (workspace) | `crates/geode-pricer` as a member |
| `crates/geode-core/src/pricing.rs` | `Instrument::vanilla` made `pub`, `Instrument::expiry` |
| `crates/geode-core/src/config/merge.rs` | `"pricer_views"` in `atomic_depth` |
| `crates/geode-pricer/Cargo.toml`, `src/lib.rs` (new) | the crate; `pub mod core;` |
| `crates/geode-pricer/src/core/mod.rs` (new) | module list and re-exports |
| `crates/geode-pricer/src/core/template.rs` (new) | `Template`, `LegSpec`, the seven tables |
| `crates/geode-pricer/src/core/shorthand.rs` (new) | `parse`, `ParseError`, `render_line`, `render_package`, `render_expiry`, `render_strike`, `third_friday`, `IMM_MONTHS` |
| `crates/geode-pricer/src/core/sheet.rs` (new) | `Sheet`, `LineId`, `RowKind`, `OwnShifts`, `LineState`, `Refresh`, `LineSpec`, `RowSpec`, `RowRecord`, `Place`, `Delivered`; `request`, `children`, `roots`, `deliver`, `fold_packages`, `shorthand` |
| `crates/geode-pricer/src/core/edit.rs` (new) | `Edit`, `Undo`, `EditError`, `Sheet::apply`, `Sheet::undo` |
| `crates/geode-pricer/src/core/columns.rs` (new) | `ColumnDef`, `ColumnKind`, `Applies`, `COLUMNS`, `column`, `CellText`, `CellState`, `cell_text` |
| `crates/geode-pricer/src/core/views.rs` (new) | `PRICER_VIEWS_DOC`, `BUILTIN_VIEWS`, `Views`, `PricerView`, `ViewColumn`, `ColumnPlan`, `PlannedColumn` |
| `crates/geode-pricer/src/core/storage.rs` (new) | `PRICER_SHEETS_DATASET`, `PRICER_SHEETS_DECLARATION`, `to_rows`, `from_rows`, `encode_overrides`, `parse_overrides` |
| `crates/geode-pricer/benches/core.rs` (new) | parse 1,000 lines; `apply` + undo at 1,000 lines; `to_rows`/`from_rows` at 1,000 lines |
| `docs/perf.md`, `CLAUDE.md`, `docs/phase-history.md`, the spec, `scripts/mutation-check.sh` | Task 10 (perf) and Task 11 (the rest) |

---

### Task 1: The crate, two core accessors, and `Template`

**Files:**
- Modify: `Cargo.toml` (workspace `members`: add `"crates/geode-pricer"` after `"crates/geode-pricing"`)
- Modify: `crates/geode-core/src/pricing.rs:103-118` (`impl Instrument`)
- Create: `crates/geode-pricer/Cargo.toml`, `crates/geode-pricer/src/lib.rs`, `crates/geode-pricer/src/core/mod.rs`, `crates/geode-pricer/src/core/template.rs`
- Test: `crates/geode-core/src/pricing.rs` (`mod tests`), `crates/geode-pricer/src/core/template.rs` (`mod tests`)

**Interfaces:**
- Produces (in `geode_core::pricing`):
```rust
impl Instrument {
    pub fn vanilla(&self) -> &Vanilla;   // was private
    pub fn expiry(&self) -> &Expiry;     // new
}
```
- Produces (in `geode_pricer::core::template`):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Template { Custom, CS, PS, STRD, STRG, RR, FLY, CAL }
/// One leg of a template: its weight (sign and ratio), which strike and expiry index it takes, its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegSpec { pub weight: i64, pub strike: usize, pub expiry: usize, pub kind: OptionKind }
impl Template {
    pub const ALL: [Template; 8];
    pub fn parse(token: &str) -> Option<Template>;       // case-insensitive; "CUSTOM" and every table token; never None for a table token
    pub fn token(self) -> &'static str;                  // "CUSTOM" | "CS" | "PS" | "STRD" | "STRG" | "RR" | "FLY" | "CAL"
    pub fn storage_name(self) -> &'static str;           // lower-case token: "custom" | "cs" | …
    pub fn legs(self) -> &'static [LegSpec];             // Custom → &[]
    pub fn strikes(self) -> usize;                       // how many strikes the shorthand takes (Custom → 0)
    pub fn expiries(self) -> usize;                      // 1 for every table but CAL (2); Custom → 0
}
```

- [ ] **Step 1: Write the failing tests**

Append to `crates/geode-core/src/pricing.rs`'s `mod tests`:

```rust
    #[test]
    fn an_instrument_answers_its_expiry_and_vanilla_through_a_barrier() {
        let call = spx_call();
        let b = Instrument::Barrier(Barrier {
            vanilla: match &call {
                Instrument::Vanilla(v) => v.clone(),
                _ => unreachable!(),
            },
            level: 4200.0,
            barrier: BarrierKind::DownOut,
        });
        assert_eq!(
            b.expiry(),
            &Expiry::Date(NaiveDate::from_ymd_opt(2026, 12, 18).unwrap())
        );
        assert_eq!(b.vanilla().strike, Strike::Absolute(5000.0));
        assert_eq!(call.expiry(), b.expiry());
    }
```

Create `crates/geode-pricer/src/core/template.rs` with only this tests block (items come in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::pricing::OptionKind;

    #[test]
    fn every_template_token_parses_case_insensitively_and_round_trips() {
        for t in Template::ALL {
            assert_eq!(Template::parse(t.token()), Some(t), "{t:?}");
            assert_eq!(Template::parse(&t.token().to_lowercase()), Some(t), "{t:?}");
            assert_eq!(t.storage_name(), t.token().to_lowercase());
        }
        assert_eq!(Template::parse("C"), None, "a single leg is not a template");
        assert_eq!(Template::parse("BUTTERFLY"), None);
    }

    #[test]
    fn the_seven_tables_have_the_documented_legs() {
        let cs = Template::CS.legs();
        assert_eq!(cs.len(), 2);
        assert_eq!((cs[0].weight, cs[0].strike, cs[0].kind), (1, 0, OptionKind::Call));
        assert_eq!((cs[1].weight, cs[1].strike, cs[1].kind), (-1, 1, OptionKind::Call));
        let ps = Template::PS.legs();
        assert_eq!((ps[0].weight, ps[0].kind), (1, OptionKind::Put));
        assert_eq!((ps[1].weight, ps[1].kind), (-1, OptionKind::Put));
        let strd = Template::STRD.legs();
        assert_eq!(strd.len(), 2);
        assert!(strd.iter().all(|l| l.weight == 1 && l.strike == 0));
        assert_eq!((strd[0].kind, strd[1].kind), (OptionKind::Call, OptionKind::Put));
        let strg = Template::STRG.legs();
        assert_eq!((strg[0].weight, strg[0].strike, strg[0].kind), (1, 0, OptionKind::Put));
        assert_eq!((strg[1].weight, strg[1].strike, strg[1].kind), (1, 1, OptionKind::Call));
        let rr = Template::RR.legs();
        assert_eq!((rr[0].weight, rr[0].strike, rr[0].kind), (-1, 0, OptionKind::Put));
        assert_eq!((rr[1].weight, rr[1].strike, rr[1].kind), (1, 1, OptionKind::Call));
        let fly = Template::FLY.legs();
        assert_eq!(fly.iter().map(|l| l.weight).collect::<Vec<_>>(), vec![1, -2, 1]);
        assert_eq!(fly.iter().map(|l| l.strike).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(fly.iter().all(|l| l.kind == OptionKind::Call));
        // CAL: +far −near calls on one strike; the far expiry is index 1.
        let cal = Template::CAL.legs();
        assert_eq!((cal[0].weight, cal[0].expiry, cal[0].kind), (1, 1, OptionKind::Call));
        assert_eq!((cal[1].weight, cal[1].expiry, cal[1].kind), (-1, 0, OptionKind::Call));
        assert!(cal.iter().all(|l| l.strike == 0));
    }

    #[test]
    fn strike_and_expiry_counts_follow_the_tables() {
        assert_eq!((Template::CS.strikes(), Template::CS.expiries()), (2, 1));
        assert_eq!((Template::PS.strikes(), Template::PS.expiries()), (2, 1));
        assert_eq!((Template::STRD.strikes(), Template::STRD.expiries()), (1, 1));
        assert_eq!((Template::STRG.strikes(), Template::STRG.expiries()), (2, 1));
        assert_eq!((Template::RR.strikes(), Template::RR.expiries()), (2, 1));
        assert_eq!((Template::FLY.strikes(), Template::FLY.expiries()), (3, 1));
        assert_eq!((Template::CAL.strikes(), Template::CAL.expiries()), (1, 2));
        assert_eq!((Template::Custom.strikes(), Template::Custom.expiries()), (0, 0));
        assert!(Template::Custom.legs().is_empty());
        // Every table's indices are in range of its own counts.
        for t in Template::ALL {
            for l in t.legs() {
                assert!(l.strike < t.strikes(), "{t:?}");
                assert!(l.expiry < t.expiries(), "{t:?}");
                assert_ne!(l.weight, 0, "{t:?}");
            }
        }
    }
}
```

- [ ] **Step 2: Scaffold the crate so the tests can fail on missing items rather than a missing crate**

`crates/geode-pricer/Cargo.toml`:

```toml
[package]
name = "geode-pricer"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

# Part 2 (line-pricer spec §14): the pure core alone. `geode-core` for the
# pricing vocabulary, `ColumnFormat`/`format_number`, `DocumentRows` and
# the config model; `chrono` for the third-Friday rule and `priced_at`;
# `toml` for the views doc. Part 3 adds `geode-shell` and gpui with the
# tile. Never `geode-pricing`: a module reaches the pricer only through
# the data tier's request door (PHILOSOPHY §1, "In-process calculation").
[dependencies]
geode-core.workspace = true
chrono = "0.4.42"
toml = "1.1.4"

# Dev-dependencies ask for the same geode-* features `--workspace`
# resolves (test-feature parity, 2026-09-19), or this crate builds a
# private copy of the stack.
[dev-dependencies]
geode-core = { workspace = true, features = ["test-support"] }
criterion = "0.8.2"

[[bench]]
name = "core"
harness = false
```

`crates/geode-pricer/src/lib.rs`:

```rust
//! The line pricer module (line-pricer spec). Part 2 is the pure core
//! alone: [`core`] names no element, entity or window, so every test
//! here is a plain `#[test]`. The tile (Part 3) and the storage wiring
//! (Part 4) land beside it.

pub mod core;
```

`crates/geode-pricer/src/core/mod.rs` (grows a line per task):

```rust
//! The sheet's pure core (line-pricer spec §6): the row model, the one
//! edit door with its inverses, the shorthand grammar both ways, the
//! package templates, the column vocabulary, the views doc and the
//! storage row shape. No gpui type appears here.

pub mod template;

pub use template::{LegSpec, Template};
```

Create an empty `crates/geode-pricer/benches/core.rs` for now so the `[[bench]]` target resolves:

```rust
//! Filled in by Task 10.
fn main() {}
```

Add `"crates/geode-pricer",` to the workspace `members` after `"crates/geode-pricing",`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p geode-core expiry_and_vanilla 2>&1 | tail -5 && cargo test -p geode-pricer 2>&1 | tail -5`
Expected: FAIL — `no method named expiry`; `cannot find type Template`.

- [ ] **Step 4: Implement**

In `crates/geode-core/src/pricing.rs`, make `vanilla` public and add `expiry`:

```rust
impl Instrument {
    /// The vanilla every variant is built on (a barrier wraps one).
    pub fn vanilla(&self) -> &Vanilla {
        match self {
            Instrument::Vanilla(v) => v,
            Instrument::Barrier(b) => &b.vanilla,
        }
    }

    pub fn underlying(&self) -> &str {
        &self.vanilla().underlying
    }

    pub fn expiry(&self) -> &Expiry {
        &self.vanilla().expiry
    }

    pub fn kind(&self) -> OptionKind {
        self.vanilla().kind
    }

    pub fn strike(&self) -> Strike {
        self.vanilla().strike
    }
}
```

`crates/geode-pricer/src/core/template.rs`, above the tests:

```rust
//! The seven package templates (line-pricer spec §6.3): a template is a
//! TABLE — for each leg, its weight, which strike and expiry index it
//! takes and its option kind. The parser expands a template over the
//! typed strikes and expiries; the renderer recognises legs that still
//! match a table and prints the template form back.

use geode_core::pricing::OptionKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Template {
    /// A `Group` over a run of roots, or a package whose legs no longer
    /// match any table.
    Custom,
    CS,
    PS,
    STRD,
    STRG,
    RR,
    FLY,
    CAL,
}

/// One leg of a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegSpec {
    /// Sign and ratio: `+1`, `-1`, `-2` (the fly's body). Never zero.
    pub weight: i64,
    /// Index into the typed strikes.
    pub strike: usize,
    /// Index into the typed expiries (`0` on every table but `CAL`).
    pub expiry: usize,
    pub kind: OptionKind,
}

const fn leg(weight: i64, strike: usize, expiry: usize, kind: OptionKind) -> LegSpec {
    LegSpec {
        weight,
        strike,
        expiry,
        kind,
    }
}

use OptionKind::{Call, Put};

const CS: [LegSpec; 2] = [leg(1, 0, 0, Call), leg(-1, 1, 0, Call)];
const PS: [LegSpec; 2] = [leg(1, 0, 0, Put), leg(-1, 1, 0, Put)];
const STRD: [LegSpec; 2] = [leg(1, 0, 0, Call), leg(1, 0, 0, Put)];
const STRG: [LegSpec; 2] = [leg(1, 0, 0, Put), leg(1, 1, 0, Call)];
const RR: [LegSpec; 2] = [leg(-1, 0, 0, Put), leg(1, 1, 0, Call)];
const FLY: [LegSpec; 3] = [leg(1, 0, 0, Call), leg(-2, 1, 0, Call), leg(1, 2, 0, Call)];
/// `+far −near` calls on one strike; the shorthand's `E1/E2` is
/// near/far, so the far expiry is index 1.
const CAL: [LegSpec; 2] = [leg(1, 0, 1, Call), leg(-1, 0, 0, Call)];

impl Template {
    pub const ALL: [Template; 8] = [
        Template::Custom,
        Template::CS,
        Template::PS,
        Template::STRD,
        Template::STRG,
        Template::RR,
        Template::FLY,
        Template::CAL,
    ];

    /// Case-insensitive. `None` for anything that is not a template
    /// token (`C` and `P` are single legs, not templates).
    pub fn parse(token: &str) -> Option<Template> {
        let upper = token.to_ascii_uppercase();
        Template::ALL.into_iter().find(|t| t.token() == upper)
    }

    pub fn token(self) -> &'static str {
        match self {
            Template::Custom => "CUSTOM",
            Template::CS => "CS",
            Template::PS => "PS",
            Template::STRD => "STRD",
            Template::STRG => "STRG",
            Template::RR => "RR",
            Template::FLY => "FLY",
            Template::CAL => "CAL",
        }
    }

    /// The lower-case spelling `pricer_sheets.template` stores (spec §7.2).
    pub fn storage_name(self) -> &'static str {
        match self {
            Template::Custom => "custom",
            Template::CS => "cs",
            Template::PS => "ps",
            Template::STRD => "strd",
            Template::STRG => "strg",
            Template::RR => "rr",
            Template::FLY => "fly",
            Template::CAL => "cal",
        }
    }

    pub fn legs(self) -> &'static [LegSpec] {
        match self {
            Template::Custom => &[],
            Template::CS => &CS,
            Template::PS => &PS,
            Template::STRD => &STRD,
            Template::STRG => &STRG,
            Template::RR => &RR,
            Template::FLY => &FLY,
            Template::CAL => &CAL,
        }
    }

    /// How many strikes the shorthand takes: one more than the largest
    /// strike index any leg names.
    pub fn strikes(self) -> usize {
        self.legs().iter().map(|l| l.strike + 1).max().unwrap_or(0)
    }

    pub fn expiries(self) -> usize {
        self.legs().iter().map(|l| l.expiry + 1).max().unwrap_or(0)
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p geode-core pricing && cargo test -p geode-pricer && cargo clippy -p geode-pricer -p geode-core --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/geode-core/src/pricing.rs crates/geode-pricer
git commit -m "pricer: the geode-pricer crate, Instrument::expiry, and the seven package templates"
```

---

### Task 2: The shorthand parser

**Files:**
- Create: `crates/geode-pricer/src/core/shorthand.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs` (add `pub mod shorthand;` and the re-exports below)
- Test: `crates/geode-pricer/src/core/shorthand.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Template`, `LegSpec` (Task 1); `geode_core::pricing::{Instrument, Vanilla, Barrier, OptionKind, BarrierKind, Expiry, Strike}`.
- Produces (in `geode_pricer::core::shorthand`; `LineSpec`/`RowSpec` are DEFINED here in this task and moved to `sheet.rs` by Task 4 — see Task 4 Step 2):
```rust
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpec { pub instrument: Instrument, pub qty: i64, pub shift: OwnShifts }
#[derive(Debug, Clone, PartialEq)]
pub enum RowSpec { Line(LineSpec), Package { template: Template, legs: Vec<LineSpec> } }
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OwnShifts { pub spot_pct: Option<f64>, pub vol_pts: Option<f64> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError { pub offset: usize, pub message: String }
pub const IMM_MONTHS: [char; 12];                          // F G H J K M N Q U V X Z
pub const MONTH_NAMES: [&str; 12];                         // JAN … DEC
pub fn third_friday(year: i32, month: u32) -> Option<NaiveDate>;
pub fn parse_expiry(token: &str) -> Result<Expiry, String>;   // Z26 | DEC26 | 20DEC26 | 3m
pub fn parse_strike(token: &str) -> Result<Strike, String>;   // 5000 | 95%
pub fn parse(text: &str) -> Result<RowSpec, ParseError>;
```

The grammar (spec §6.3), one line, whitespace-separated, case-insensitive:

```
[qty] UNDERLYING EXPIRY STRIKES TYPE [BARRIER level]
```

Token by token: if the first token parses as a signed integer it is `qty` (zero is an error); the next is the underlying (stored upper-case); the next is one expiry, or `E1/E2` for `CAL`; the next is `/`-separated strikes; the next is the type — `C`/`P` for a line, a template token for a package; a line may then carry `UI|UO|DI|DO level`. Every error names the offending token's byte offset (`text.len()` when a token is missing). Note `Expiry::tenor` accepts `d` as well as `w|m|y`; the parser passes the token to it unchanged.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-pricer/src/core/shorthand.rs` with only this tests block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use geode_core::pricing::{BarrierKind, OptionKind, Strike};

    fn d(y: i32, m: u32, day: u32) -> Expiry {
        Expiry::Date(NaiveDate::from_ymd_opt(y, m, day).unwrap())
    }

    fn line(text: &str) -> LineSpec {
        match parse(text).unwrap() {
            RowSpec::Line(l) => l,
            other => panic!("{text:?} parsed as a package: {other:?}"),
        }
    }

    fn package(text: &str) -> (Template, Vec<LineSpec>) {
        match parse(text).unwrap() {
            RowSpec::Package { template, legs } => (template, legs),
            other => panic!("{text:?} parsed as a line: {other:?}"),
        }
    }

    #[test]
    fn a_month_code_resolves_to_the_third_friday() {
        // December 2026: the 1st is a Tuesday, the first Friday the 4th,
        // the third the 18th.
        assert_eq!(
            third_friday(2026, 12),
            Some(NaiveDate::from_ymd_opt(2026, 12, 18).unwrap())
        );
        // A month starting on a Friday: the 1st IS the first Friday.
        // May 2026 starts on a Friday → third Friday the 15th.
        assert_eq!(
            third_friday(2026, 5),
            Some(NaiveDate::from_ymd_opt(2026, 5, 15).unwrap())
        );
        // A month starting on a Saturday: the first Friday is the 7th.
        // August 2026 starts on a Saturday → the 21st.
        assert_eq!(
            third_friday(2026, 8),
            Some(NaiveDate::from_ymd_opt(2026, 8, 21).unwrap())
        );
        assert_eq!(third_friday(2026, 13), None);
        assert_eq!(parse_expiry("Z26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("z26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("DEC26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("dec26").unwrap(), d(2026, 12, 18));
        assert_eq!(parse_expiry("K26").unwrap(), d(2026, 5, 15));
    }

    #[test]
    fn the_imm_table_is_the_twelve_letters_in_month_order() {
        assert_eq!(
            IMM_MONTHS.iter().collect::<String>(),
            "FGHJKMNQUVXZ"
        );
        assert_eq!(MONTH_NAMES[0], "JAN");
        assert_eq!(MONTH_NAMES[11], "DEC");
        for (i, c) in IMM_MONTHS.iter().enumerate() {
            assert_eq!(
                parse_expiry(&format!("{c}27")).unwrap(),
                Expiry::Date(third_friday(2027, i as u32 + 1).unwrap())
            );
        }
    }

    #[test]
    fn every_expiry_form_parses() {
        assert_eq!(parse_expiry("20DEC26").unwrap(), d(2026, 12, 20));
        assert_eq!(parse_expiry("5dec26").unwrap(), d(2026, 12, 5));
        assert_eq!(parse_expiry("05DEC26").unwrap(), d(2026, 12, 5));
        assert_eq!(parse_expiry("3m").unwrap(), Expiry::Tenor("3m".into()));
        assert_eq!(parse_expiry("6W").unwrap(), Expiry::Tenor("6w".into()));
        assert_eq!(parse_expiry("1y").unwrap(), Expiry::Tenor("1y".into()));
        assert!(parse_expiry("31FEB26").is_err(), "no such day");
        assert!(parse_expiry("XYZ26").is_err(), "not a month");
        assert!(parse_expiry("DEC").is_err(), "no year");
        assert!(parse_expiry("3q").is_err(), "not a tenor unit");
        assert!(parse_expiry("").is_err());
    }

    #[test]
    fn a_strike_is_absolute_or_percent() {
        assert_eq!(parse_strike("5000").unwrap(), Strike::Absolute(5000.0));
        assert_eq!(parse_strike("4250.5").unwrap(), Strike::Absolute(4250.5));
        assert_eq!(parse_strike("95%").unwrap(), Strike::Percent(95.0));
        assert_eq!(parse_strike("102.5%").unwrap(), Strike::Percent(102.5));
        assert!(parse_strike("abc").is_err());
        assert!(parse_strike("%").is_err());
        assert!(parse_strike("").is_err());
        assert!(parse_strike("-5").is_err(), "a strike is positive");
    }

    #[test]
    fn a_vanilla_line_parses_with_and_without_a_quantity() {
        let l = line("SPX DEC26 5000 C");
        assert_eq!(l.qty, 1);
        assert_eq!(l.shift, OwnShifts::default());
        assert_eq!(
            l.instrument,
            Instrument::Vanilla(Vanilla {
                underlying: "SPX".into(),
                expiry: d(2026, 12, 18),
                strike: Strike::Absolute(5000.0),
                kind: OptionKind::Call,
            })
        );
        let l = line("-5 spx z26 95% p");
        assert_eq!(l.qty, -5);
        assert_eq!(l.instrument.underlying(), "SPX");
        assert_eq!(l.instrument.kind(), OptionKind::Put);
        assert_eq!(l.instrument.strike(), Strike::Percent(95.0));
        let l = line("10 NDX 3m 100% C");
        assert_eq!(l.qty, 10);
        assert_eq!(l.instrument.expiry(), &Expiry::Tenor("3m".into()));
    }

    #[test]
    fn a_barrier_line_parses_only_after_c_or_p() {
        let l = line("SPX DEC26 5000 C DO 4200");
        match l.instrument {
            Instrument::Barrier(b) => {
                assert_eq!(b.level, 4200.0);
                assert_eq!(b.barrier, BarrierKind::DownOut);
                assert_eq!(b.vanilla.kind, OptionKind::Call);
            }
            other => panic!("{other:?}"),
        }
        for (token, kind) in [
            ("UI", BarrierKind::UpIn),
            ("uo", BarrierKind::UpOut),
            ("DI", BarrierKind::DownIn),
            ("do", BarrierKind::DownOut),
        ] {
            let l = line(&format!("SPX DEC26 5000 P {token} 4000"));
            match l.instrument {
                Instrument::Barrier(b) => assert_eq!(b.barrier, kind),
                other => panic!("{other:?}"),
            }
        }
        let e = parse("SPX DEC26 95%/105% CS DO 4200").unwrap_err();
        assert_eq!(e.offset, 22, "the barrier token: {e:?}");
        assert!(e.message.contains("barrier"), "{e:?}");
        let e = parse("SPX DEC26 5000 C DO").unwrap_err();
        assert_eq!(e.offset, 19, "a missing level points past the end: {e:?}");
        let e = parse("SPX DEC26 5000 C XX 4200").unwrap_err();
        assert_eq!(e.offset, 17, "{e:?}");
        assert!(e.message.contains("barrier"), "{e:?}");
        let e = parse("SPX DEC26 5000 C DO abc").unwrap_err();
        assert_eq!(e.offset, 20, "{e:?}");
    }

    #[test]
    fn every_template_expands_over_its_strikes_and_expiries() {
        let (t, legs) = package("-5 SPX DEC26 95%/105% CS");
        assert_eq!(t, Template::CS);
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].qty, -5);
        assert_eq!(legs[1].qty, 5);
        assert_eq!(legs[0].instrument.strike(), Strike::Percent(95.0));
        assert_eq!(legs[1].instrument.strike(), Strike::Percent(105.0));
        assert!(legs.iter().all(|l| l.instrument.kind() == OptionKind::Call));
        assert!(legs.iter().all(|l| l.instrument.underlying() == "SPX"));

        let (t, legs) = package("SPX DEC26 4800/5200 PS");
        assert_eq!(t, Template::PS);
        assert_eq!((legs[0].qty, legs[1].qty), (1, -1));
        assert!(legs.iter().all(|l| l.instrument.kind() == OptionKind::Put));

        let (t, legs) = package("2 SPX DEC26 5000 STRD");
        assert_eq!(t, Template::STRD);
        assert_eq!((legs[0].qty, legs[1].qty), (2, 2));
        assert_eq!((legs[0].instrument.kind(), legs[1].instrument.kind()), (OptionKind::Call, OptionKind::Put));

        let (t, legs) = package("SPX DEC26 4800/5200 STRG");
        assert_eq!(t, Template::STRG);
        assert_eq!(legs[0].instrument.kind(), OptionKind::Put);
        assert_eq!(legs[0].instrument.strike(), Strike::Absolute(4800.0));
        assert_eq!(legs[1].instrument.kind(), OptionKind::Call);
        assert_eq!(legs[1].instrument.strike(), Strike::Absolute(5200.0));

        let (t, legs) = package("SPX DEC26 4800/5200 RR");
        assert_eq!(t, Template::RR);
        assert_eq!((legs[0].qty, legs[1].qty), (-1, 1));
        assert_eq!(legs[0].instrument.kind(), OptionKind::Put);

        let (t, legs) = package("3 SPX DEC26 4800/5000/5200 FLY");
        assert_eq!(t, Template::FLY);
        assert_eq!(legs.iter().map(|l| l.qty).collect::<Vec<_>>(), vec![3, -6, 3]);

        let (t, legs) = package("SPX DEC26/MAR27 5000 CAL");
        assert_eq!(t, Template::CAL);
        assert_eq!(legs[0].qty, 1);
        assert_eq!(legs[0].instrument.expiry(), &d(2027, 3, 19), "+far first");
        assert_eq!(legs[1].qty, -1);
        assert_eq!(legs[1].instrument.expiry(), &d(2026, 12, 18), "−near second");
    }

    #[test]
    fn every_error_names_the_offending_offset() {
        let e = parse("").unwrap_err();
        assert_eq!(e.offset, 0);
        assert!(e.message.contains("empty"), "{e:?}");
        let e = parse("   ").unwrap_err();
        assert!(e.message.contains("empty"), "{e:?}");

        let e = parse("0 SPX DEC26 5000 C").unwrap_err();
        assert_eq!(e.offset, 0, "{e:?}");
        assert!(e.message.contains("zero"), "{e:?}");

        let e = parse("SPX").unwrap_err();
        assert_eq!(e.offset, 3, "missing expiry points past the end: {e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");

        let e = parse("SPX DEX26 5000 C").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");

        let e = parse("SPX DEC26 abc C").unwrap_err();
        assert_eq!(e.offset, 10, "{e:?}");
        assert!(e.message.contains("strike"), "{e:?}");

        let e = parse("SPX DEC26 5000").unwrap_err();
        assert_eq!(e.offset, 14, "missing type: {e:?}");
        assert!(e.message.contains("type"), "{e:?}");

        let e = parse("SPX DEC26 5000 XYZ").unwrap_err();
        assert_eq!(e.offset, 15, "{e:?}");
        assert!(e.message.contains("unknown type"), "{e:?}");

        let e = parse("SPX DEC26 95%/105%/110% CS").unwrap_err();
        assert_eq!(e.offset, 10, "the strikes token: {e:?}");
        assert!(e.message.contains("CS takes 2 strikes"), "{e:?}");

        let e = parse("SPX DEC26 5000/5200 C").unwrap_err();
        assert_eq!(e.offset, 10, "{e:?}");
        assert!(e.message.contains("1 strike"), "{e:?}");

        let e = parse("SPX DEC26/MAR27 5000 CS").unwrap_err();
        assert_eq!(e.offset, 4, "the expiries token: {e:?}");
        assert!(e.message.contains("CS takes 1 expiry"), "{e:?}");

        let e = parse("SPX DEC26 5000 CAL").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("CAL takes 2 expiries"), "{e:?}");

        let e = parse("SPX DEC26 5000 C extra").unwrap_err();
        assert_eq!(e.offset, 17, "{e:?}");
        assert!(e.message.contains("unexpected"), "{e:?}");
    }

    #[test]
    fn quantities_that_are_not_integers_are_the_underlying() {
        // "1.5" is not an integer, so it is read as an underlying named
        // "1.5" — the grammar has no fractional quantity, and the trader
        // sees the error at the next token.
        let e = parse("1.5 SPX DEC26 5000 C").unwrap_err();
        assert_eq!(e.offset, 4, "{e:?}");
        assert!(e.message.contains("expiry"), "{e:?}");
        // "+3" is a quantity.
        assert_eq!(line("+3 SPX DEC26 5000 C").qty, 3);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer shorthand 2>&1 | grep -E 'error\[|^test result' | head`
Expected: compile errors — `parse`, `parse_expiry`, `third_friday`, `RowSpec` not found.

- [ ] **Step 3: Implement**

Above the tests in `crates/geode-pricer/src/core/shorthand.rs`:

```rust
//! The one-line shorthand (line-pricer spec §6.3), parsed here and
//! rendered here (Task 3), so the two halves share one table of tokens.
//!
//! `[qty] UNDERLYING EXPIRY STRIKES TYPE [BARRIER level]`, whitespace-
//! separated, case-insensitive. A month form (`Z26`, `DEC26`) resolves to
//! the third Friday of its month at parse time: a date CONVENTION, not a
//! financial calculation (the spec says so); holidays are not considered.
//! A tenor (`3m`) is validated by `Expiry::tenor` and never resolved —
//! that is the library's calendar.

use crate::core::template::Template;
use chrono::{Datelike, NaiveDate, Weekday};
use geode_core::pricing::{Barrier, BarrierKind, Expiry, Instrument, OptionKind, Strike, Vanilla};

/// A line's own shifts; `None` inherits the sheet's (spec ruling 8).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OwnShifts {
    pub spot_pct: Option<f64>,
    pub vol_pts: Option<f64>,
}

/// One line as the parser or a caller describes it, before it has an id.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpec {
    pub instrument: Instrument,
    /// Signed; a sell is negative; never zero.
    pub qty: i64,
    pub shift: OwnShifts,
}

/// What one shorthand line means: a line, or a package with its legs
/// (planning decision 1).
#[derive(Debug, Clone, PartialEq)]
pub enum RowSpec {
    Line(LineSpec),
    Package {
        template: Template,
        legs: Vec<LineSpec>,
    },
}

/// `offset` is the byte offset of the offending token in the text the
/// caller passed (`text.len()` when a token is missing), for the footer's
/// caret (spec §8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

/// IMM month codes, January to December (spec §6.3). One constant, one test.
pub const IMM_MONTHS: [char; 12] = ['F', 'G', 'H', 'J', 'K', 'M', 'N', 'Q', 'U', 'V', 'X', 'Z'];
pub const MONTH_NAMES: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

/// The third Friday of `month` in `year`; `None` for a month outside 1–12.
pub fn third_friday(year: i32, month: u32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let to_friday = (Weekday::Fri.num_days_from_monday() + 7 - first.weekday().num_days_from_monday()) % 7;
    first.checked_add_days(chrono::Days::new(u64::from(to_friday) + 14))
}

fn month_index(name: &str) -> Option<u32> {
    MONTH_NAMES
        .iter()
        .position(|m| *m == name)
        .map(|i| i as u32 + 1)
}

/// A two-digit year is `20yy` (spec §6.3).
fn year_of(two: &str) -> Option<i32> {
    if two.len() != 2 || !two.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    two.parse::<i32>().ok().map(|y| 2000 + y)
}

/// `Z26` | `DEC26` | `20DEC26` | `3m` — the month forms resolve to the
/// third Friday; a full date is itself; anything else must be a tenor.
pub fn parse_expiry(token: &str) -> Result<Expiry, String> {
    let upper = token.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    // IMM code: one letter, two digits.
    if bytes.len() == 3 && bytes[0].is_ascii_alphabetic() {
        if let Some(m) = IMM_MONTHS.iter().position(|c| *c as u8 == bytes[0]) {
            if let Some(y) = year_of(&upper[1..]) {
                return third_friday(y, m as u32 + 1)
                    .map(Expiry::Date)
                    .ok_or_else(|| format!("expiry '{token}': no third Friday"));
            }
        }
        return Err(format!("expiry '{token}': not a month code, a month name, a date or a tenor"));
    }
    // Month name: three letters, two digits.
    if bytes.len() == 5 && bytes[..3].iter().all(u8::is_ascii_alphabetic) {
        let month = month_index(&upper[..3])
            .ok_or_else(|| format!("expiry '{token}': '{}' is not a month", &upper[..3]))?;
        let year = year_of(&upper[3..])
            .ok_or_else(|| format!("expiry '{token}': expected a two-digit year"))?;
        return third_friday(year, month)
            .map(Expiry::Date)
            .ok_or_else(|| format!("expiry '{token}': no third Friday"));
    }
    // Full date: one or two digits, three letters, two digits.
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if (1..=2).contains(&digits) && bytes.len() == digits + 5 && bytes[digits..digits + 3].iter().all(u8::is_ascii_alphabetic) {
        let day: u32 = upper[..digits].parse().map_err(|_| format!("expiry '{token}': bad day"))?;
        let month = month_index(&upper[digits..digits + 3])
            .ok_or_else(|| format!("expiry '{token}': '{}' is not a month", &upper[digits..digits + 3]))?;
        let year = year_of(&upper[digits + 3..])
            .ok_or_else(|| format!("expiry '{token}': expected a two-digit year"))?;
        return NaiveDate::from_ymd_opt(year, month, day)
            .map(Expiry::Date)
            .ok_or_else(|| format!("expiry '{token}': no such day"));
    }
    Expiry::tenor(token)
}

/// `5000` | `4250.5` | `95%` — positive numbers only.
pub fn parse_strike(token: &str) -> Result<Strike, String> {
    let (number, percent) = match token.strip_suffix('%') {
        Some(n) => (n, true),
        None => (token, false),
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("strike '{token}': not a number"))?;
    if !(value.is_finite() && value > 0.0) {
        return Err(format!("strike '{token}': must be a positive number"));
    }
    Ok(if percent {
        Strike::Percent(value)
    } else {
        Strike::Absolute(value)
    })
}

fn parse_barrier_kind(token: &str) -> Option<BarrierKind> {
    match token.to_ascii_uppercase().as_str() {
        "UI" => Some(BarrierKind::UpIn),
        "UO" => Some(BarrierKind::UpOut),
        "DI" => Some(BarrierKind::DownIn),
        "DO" => Some(BarrierKind::DownOut),
        _ => None,
    }
}

/// A token and where it starts.
struct Tok<'a> {
    text: &'a str,
    offset: usize,
}

fn tokens(text: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push(Tok {
                    text: &text[s..i],
                    offset: s,
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(Tok {
            text: &text[s..],
            offset: s,
        });
    }
    out
}

fn err(offset: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        offset,
        message: message.into(),
    }
}

/// One line of shorthand to a line or a package (spec §6.3).
pub fn parse(text: &str) -> Result<RowSpec, ParseError> {
    let toks = tokens(text);
    let end = text.len();
    if toks.is_empty() {
        return Err(err(0, "empty line: [qty] UNDERLYING EXPIRY STRIKES TYPE [BARRIER level]"));
    }
    let mut i = 0;
    // qty: a signed integer, else 1 and the token is the underlying.
    let qty = match toks[0].text.parse::<i64>() {
        Ok(0) => return Err(err(toks[0].offset, "quantity must not be zero")),
        Ok(q) => {
            i += 1;
            q
        }
        Err(_) => 1,
    };
    /// The token at `i`, or "expected <what>" pointing past the end.
    fn next<'t>(toks: &'t [Tok<'t>], i: usize, end: usize, what: &str) -> Result<&'t Tok<'t>, ParseError> {
        toks.get(i).ok_or_else(|| err(end, format!("expected {what}")))
    }
    let underlying = next(&toks, i, end, "an underlying")?.text.to_ascii_uppercase();
    i += 1;
    let expiry_tok = next(&toks, i, end, "an expiry")?;
    i += 1;
    let expiries: Vec<Expiry> = expiry_tok
        .text
        .split('/')
        .map(|t| parse_expiry(t).map_err(|m| err(expiry_tok.offset, m)))
        .collect::<Result<_, _>>()?;
    let strikes_tok = next(&toks, i, end, "one or more strikes")?;
    i += 1;
    let strikes: Vec<Strike> = strikes_tok
        .text
        .split('/')
        .map(|t| parse_strike(t).map_err(|m| err(strikes_tok.offset, m)))
        .collect::<Result<_, _>>()?;
    let type_tok = next(&toks, i, end, "a type: C P CS PS STRD STRG RR FLY CAL")?;
    i += 1;
    let type_upper = type_tok.text.to_ascii_uppercase();

    let count = |n: usize, noun: &str| -> String {
        if n == 1 {
            format!("1 {noun}")
        } else {
            let plural = if noun == "expiry" { "expiries".to_string() } else { format!("{noun}s") };
            format!("{n} {plural}")
        }
    };

    let single = match type_upper.as_str() {
        "C" => Some(OptionKind::Call),
        "P" => Some(OptionKind::Put),
        _ => None,
    };
    if let Some(kind) = single {
        if strikes.len() != 1 {
            return Err(err(strikes_tok.offset, format!("{type_upper} takes 1 strike, got {}", strikes.len())));
        }
        if expiries.len() != 1 {
            return Err(err(expiry_tok.offset, format!("{type_upper} takes 1 expiry, got {}", expiries.len())));
        }
        let vanilla = Vanilla {
            underlying,
            expiry: expiries.into_iter().next().expect("one expiry"),
            strike: strikes[0],
            kind,
        };
        let instrument = match toks.get(i) {
            None => Instrument::Vanilla(vanilla),
            Some(bk) => {
                let barrier = parse_barrier_kind(bk.text)
                    .ok_or_else(|| err(bk.offset, format!("'{}' is not a barrier kind (UI UO DI DO)", bk.text)))?;
                i += 1;
                let level_tok = next(&toks, i, end, "a barrier level")?;
                i += 1;
                let level: f64 = level_tok
                    .text
                    .parse()
                    .ok()
                    .filter(|l: &f64| l.is_finite() && *l > 0.0)
                    .ok_or_else(|| err(level_tok.offset, format!("barrier level '{}': not a positive number", level_tok.text)))?;
                Instrument::Barrier(Barrier { vanilla, level, barrier })
            }
        };
        if let Some(extra) = toks.get(i) {
            return Err(err(extra.offset, format!("unexpected token '{}'", extra.text)));
        }
        return Ok(RowSpec::Line(LineSpec {
            instrument,
            qty,
            shift: OwnShifts::default(),
        }));
    }

    let template = Template::parse(&type_upper)
        .filter(|t| *t != Template::Custom)
        .ok_or_else(|| err(type_tok.offset, format!("unknown type '{}': C P CS PS STRD STRG RR FLY CAL", type_tok.text)))?;
    if strikes.len() != template.strikes() {
        return Err(err(
            strikes_tok.offset,
            format!("{} takes {}, got {}", template.token(), count(template.strikes(), "strike"), strikes.len()),
        ));
    }
    if expiries.len() != template.expiries() {
        return Err(err(
            expiry_tok.offset,
            format!("{} takes {}, got {}", template.token(), count(template.expiries(), "expiry"), expiries.len()),
        ));
    }
    if let Some(extra) = toks.get(i) {
        let message = if parse_barrier_kind(extra.text).is_some() {
            "a barrier belongs on a single C or P leg, not on a package".to_string()
        } else {
            format!("unexpected token '{}'", extra.text)
        };
        return Err(err(extra.offset, message));
    }
    let legs = template
        .legs()
        .iter()
        .map(|l| LineSpec {
            instrument: Instrument::Vanilla(Vanilla {
                underlying: underlying.clone(),
                expiry: expiries[l.expiry].clone(),
                strike: strikes[l.strike],
                kind: l.kind,
            }),
            qty: qty * l.weight,
            shift: OwnShifts::default(),
        })
        .collect();
    Ok(RowSpec::Package { template, legs })
}
```

In `core/mod.rs` add `pub mod shorthand;` and `pub use shorthand::{LineSpec, OwnShifts, ParseError, RowSpec, parse};` (Task 4 moves the three types; the re-export paths stay).

Check the offsets the tests assert by counting: in `SPX DEC26 95%/105% CS DO 4200` the `DO` token starts at byte 22; in `SPX DEC26 5000 C DO` the text length is 19; in `SPX DEC26 5000 C XX 4200` `XX` is at 17; `abc` in `SPX DEC26 5000 C DO abc` is at 20; `extra` in `SPX DEC26 5000 C extra` is at 17. If a test's offset disagrees with your count of the literal, fix the TEST's number — the rule is "the offending token's start".

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer shorthand && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo fmt --check`
Expected: 9 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-pricer
git commit -m "pricer: the shorthand parser — every template, every expiry form, offsets on every error"
```

---

### Task 3: The shorthand renderer

**Files:**
- Modify: `crates/geode-pricer/src/core/shorthand.rs`
- Test: `crates/geode-pricer/src/core/shorthand.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 2's items.
- Produces (in `geode_pricer::core::shorthand`):
```rust
pub fn imm_code(date: NaiveDate) -> Option<String>;            // "Z26" when `date` is a third Friday, else None
pub fn render_expiry(expiry: &Expiry) -> String;                // Z26 | 20DEC26 | 3m
pub fn render_strike(strike: Strike) -> String;                 // 5000 | 4250.5 | 95%
pub fn render_barrier_kind(kind: BarrierKind) -> &'static str;  // UI UO DI DO
pub fn render_line(qty: i64, instrument: &Instrument) -> String;
/// `legs` as `(qty, instrument)`; `None` when they do not match `template`'s table
/// (or `template` is `Custom`), so the caller falls back to one line per leg.
pub fn render_package(template: Template, legs: &[(i64, &Instrument)]) -> Option<String>;
```

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `shorthand.rs`:

```rust
    #[test]
    fn an_expiry_renders_as_an_imm_code_a_full_date_or_the_tenor() {
        assert_eq!(render_expiry(&d(2026, 12, 18)), "Z26");
        assert_eq!(render_expiry(&d(2026, 5, 15)), "K26");
        assert_eq!(render_expiry(&d(2026, 12, 20)), "20DEC26");
        assert_eq!(render_expiry(&d(2026, 12, 5)), "05DEC26");
        assert_eq!(render_expiry(&Expiry::Tenor("3m".into())), "3m");
        assert_eq!(imm_code(NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()), Some("Z26".into()));
        assert_eq!(imm_code(NaiveDate::from_ymd_opt(2026, 12, 11).unwrap()), None, "the second Friday");
    }

    #[test]
    fn a_strike_renders_without_trailing_zeros() {
        assert_eq!(render_strike(Strike::Absolute(5000.0)), "5000");
        assert_eq!(render_strike(Strike::Absolute(4250.5)), "4250.5");
        assert_eq!(render_strike(Strike::Percent(95.0)), "95%");
        assert_eq!(render_strike(Strike::Percent(102.5)), "102.5%");
    }

    #[test]
    fn a_line_renders_and_round_trips_through_parse() {
        for text in [
            "SPX Z26 5000 C",
            "-5 SPX Z26 95% P",
            "10 NDX 3m 100% C",
            "SPX 20DEC26 5000 C DO 4200",
            "-2 SPX Z26 4800 P UI 5500",
        ] {
            let l = line(text);
            let rendered = render_line(l.qty, &l.instrument);
            assert_eq!(rendered, text, "renders as typed");
            assert_eq!(line(&rendered), l, "round trip");
        }
        // Lower-case input renders upper-case tokens; a qty of 1 is omitted.
        let l = line("1 spx dec26 5000 c");
        assert_eq!(render_line(l.qty, &l.instrument), "SPX Z26 5000 C");
    }

    #[test]
    fn every_template_renders_and_round_trips_through_parse() {
        for text in [
            "-5 SPX Z26 95%/105% CS",
            "SPX Z26 4800/5200 PS",
            "2 SPX Z26 5000 STRD",
            "SPX Z26 4800/5200 STRG",
            "SPX Z26 4800/5200 RR",
            "3 SPX Z26 4800/5000/5200 FLY",
            "SPX Z26/H27 5000 CAL",
        ] {
            let (template, legs) = package(text);
            let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
            let rendered = render_package(template, &pairs).expect(text);
            assert_eq!(rendered, text);
            assert_eq!(package(&rendered), (template, legs), "round trip");
        }
    }

    #[test]
    fn a_package_whose_legs_left_the_table_does_not_render_as_the_template() {
        let (template, mut legs) = package("SPX Z26 4800/5200 CS");
        // A 1×2 ratio: the second leg's qty edited (spec §6.4).
        legs[1].qty = -2;
        let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(render_package(template, &pairs), None);
        // Legs on two underlyings.
        let (template, mut legs) = package("SPX Z26 4800/5200 CS");
        if let Instrument::Vanilla(v) = &mut legs[1].instrument {
            v.underlying = "NDX".into();
        }
        let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(render_package(template, &pairs), None);
        // A wrong leg count.
        let (template, legs) = package("SPX Z26 4800/5200 CS");
        let pairs: Vec<(i64, &Instrument)> = legs.iter().take(1).map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(render_package(template, &pairs), None);
        // Custom never renders as a template.
        let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(render_package(Template::Custom, &pairs), None);
        // A barrier leg is never a template leg.
        let b = line("SPX Z26 5000 C DO 4200");
        let c = line("SPX Z26 5200 C");
        assert_eq!(render_package(Template::CS, &[(1, &b.instrument), (-1, &c.instrument)]), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer shorthand 2>&1 | grep -E 'error\[|^test result' | head`
Expected: compile errors — `render_expiry` etc. not found.

- [ ] **Step 3: Implement**

Add to `shorthand.rs` (above the tests):

```rust
/// `Z26` when `date` is the third Friday of its month, else `None`.
pub fn imm_code(date: NaiveDate) -> Option<String> {
    if third_friday(date.year(), date.month()) != Some(date) {
        return None;
    }
    let letter = IMM_MONTHS[date.month0() as usize];
    Some(format!("{letter}{:02}", date.year() % 100))
}

/// A third Friday as its IMM code, any other date as `20DEC26`, a tenor
/// as stored (spec §6.3).
pub fn render_expiry(expiry: &Expiry) -> String {
    match expiry {
        Expiry::Date(d) => imm_code(*d).unwrap_or_else(|| {
            format!("{:02}{}{:02}", d.day(), MONTH_NAMES[d.month0() as usize], d.year() % 100)
        }),
        Expiry::Tenor(t) => t.clone(),
    }
}

/// `f64`'s own `Display` prints `5000.0` as `5000` and `4250.5` as
/// `4250.5`: the shortest text that parses back to the same number.
pub fn render_strike(strike: Strike) -> String {
    match strike {
        Strike::Absolute(k) => format!("{k}"),
        Strike::Percent(p) => format!("{p}%"),
    }
}

pub fn render_barrier_kind(kind: BarrierKind) -> &'static str {
    match kind {
        BarrierKind::UpIn => "UI",
        BarrierKind::UpOut => "UO",
        BarrierKind::DownIn => "DI",
        BarrierKind::DownOut => "DO",
    }
}

fn kind_token(kind: OptionKind) -> &'static str {
    match kind {
        OptionKind::Call => "C",
        OptionKind::Put => "P",
    }
}

fn qty_prefix(qty: i64) -> String {
    if qty == 1 {
        String::new()
    } else {
        format!("{qty} ")
    }
}

/// One line back in the grammar. A qty of 1 is omitted, as the grammar
/// defaults it.
pub fn render_line(qty: i64, instrument: &Instrument) -> String {
    let v = instrument.vanilla();
    let mut out = format!(
        "{}{} {} {} {}",
        qty_prefix(qty),
        v.underlying,
        render_expiry(&v.expiry),
        render_strike(v.strike),
        kind_token(v.kind)
    );
    if let Instrument::Barrier(b) = instrument {
        out.push_str(&format!(" {} {}", render_barrier_kind(b.barrier), b.level));
    }
    out
}

/// The template form (`-5 SPX Z26 95%/105% CS`) when `legs` still match
/// `template`'s table: same count, every leg a vanilla on one
/// underlying, each leg's qty the package qty times its weight, each
/// leg's kind the table's, and one strike per strike index and one
/// expiry per expiry index across the legs. `None` otherwise — the
/// caller prints the legs one per line (planning decision 11).
pub fn render_package(template: Template, legs: &[(i64, &Instrument)]) -> Option<String> {
    let table = template.legs();
    if table.is_empty() || table.len() != legs.len() {
        return None;
    }
    let first = table[0];
    let (q0, _) = legs[0];
    if q0 % first.weight != 0 {
        return None;
    }
    let qty = q0 / first.weight;
    if qty == 0 {
        return None;
    }
    let underlying = legs[0].1.underlying();
    let mut strikes: Vec<Option<Strike>> = vec![None; template.strikes()];
    let mut expiries: Vec<Option<&Expiry>> = vec![None; template.expiries()];
    for (spec, (leg_qty, instrument)) in table.iter().zip(legs) {
        let Instrument::Vanilla(v) = instrument else {
            return None;
        };
        if *leg_qty != qty * spec.weight || v.kind != spec.kind || v.underlying != underlying {
            return None;
        }
        match strikes[spec.strike] {
            None => strikes[spec.strike] = Some(v.strike),
            Some(k) if k == v.strike => {}
            Some(_) => return None,
        }
        match expiries[spec.expiry] {
            None => expiries[spec.expiry] = Some(&v.expiry),
            Some(e) if *e == v.expiry => {}
            Some(_) => return None,
        }
    }
    let strikes: Vec<String> = strikes.into_iter().map(|k| k.map(render_strike)).collect::<Option<_>>()?;
    let expiries: Vec<String> = expiries.into_iter().map(|e| e.map(render_expiry)).collect::<Option<_>>()?;
    Some(format!(
        "{}{} {} {} {}",
        qty_prefix(qty),
        underlying,
        expiries.join("/"),
        strikes.join("/"),
        template.token()
    ))
}
```

`render_strike` for `Strike::Absolute(k)` — `format!("{k}")` on an `f64` of `5000.0` prints `5000` in Rust. Verify with the test; if the toolchain prints `5000.0`, use `if k.fract() == 0.0 { format!("{k:.0}") } else { format!("{k}") }` for both arms.

Add `render_line`, `render_package`, `render_expiry`, `render_strike` to the `mod.rs` re-export line.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer shorthand && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo fmt --check`
Expected: 14 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-pricer
git commit -m "pricer: the shorthand renderer — IMM codes, template recognition, round trip through parse"
```

---

### Task 4: `Sheet` — the struct of arrays, `Insert`/`Remove`/`Restore`, requests, deliveries and package folding

**Files:**
- Create: `crates/geode-pricer/src/core/sheet.rs`, `crates/geode-pricer/src/core/edit.rs`
- Modify: `crates/geode-pricer/src/core/shorthand.rs` (move `OwnShifts`, `LineSpec`, `RowSpec` out; import them from `sheet`), `crates/geode-pricer/src/core/mod.rs`
- Test: `crates/geode-pricer/src/core/sheet.rs` (`mod tests`), `crates/geode-pricer/src/core/edit.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Template` (Task 1); `render_line`, `render_package` (Task 3); `geode_core::pricing::{Instrument, MarketOverrides, PriceRequest, PriceResult, Shifts}`.
- Produces (in `geode_pricer::core::sheet`):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)] pub struct LineId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum RowKind { Line, Package { template: Template }, /// reserved, slice 2; never constructed here
 Underlying }
#[derive(Debug, Clone, Copy, PartialEq, Default)] pub struct OwnShifts { pub spot_pct: Option<f64>, pub vol_pts: Option<f64> }   // moved from shorthand
#[derive(Debug, Clone, PartialEq, Eq)] pub enum LineState { Fresh, Stale, Failed(String) }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Refresh { Default, Off, Every(Duration) }
pub struct LineSpec { .. }  pub enum RowSpec { .. }                                            // moved from shorthand, unchanged
#[derive(Debug, Clone, PartialEq)] pub struct RowRecord { pub id: LineId, pub kind: RowKind, pub parent: Option<LineId>, pub instrument: Option<Instrument>, pub qty: i64, pub shift: OwnShifts, pub revision: u64, pub result: Option<PriceResult>, pub state: LineState, pub priced_at: Option<DateTime<Utc>> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Place { Root { at: usize }, Leg { package: usize, leg: usize } }
#[derive(Debug, Clone, PartialEq, Eq)] pub enum Delivered { Installed, UnknownLine, NotALine, OldRevision { current: u64 }, FutureRevision { current: u64 } }
pub struct Sheet { pub name: String, pub view: String, pub sheet_shift: OwnShifts, pub overrides: MarketOverrides, pub refresh: Refresh, /* private SoA */ }
impl Sheet {
    pub fn new(name: &str) -> Sheet;                     // view "vanilla", no rows
    pub fn len(&self) -> usize;  pub fn is_empty(&self) -> bool;
    pub fn id(&self, row: usize) -> LineId;  pub fn index_of(&self, id: LineId) -> Option<usize>;
    pub fn kind(&self, row: usize) -> RowKind;  pub fn parent(&self, row: usize) -> Option<usize>;  pub fn depth(&self, row: usize) -> usize;
    pub fn instrument(&self, row: usize) -> Option<&Instrument>;  pub fn qty(&self, row: usize) -> i64;  pub fn shift(&self, row: usize) -> OwnShifts;
    pub fn revision(&self, row: usize) -> u64;  pub fn result(&self, row: usize) -> Option<&PriceResult>;  pub fn state(&self, row: usize) -> &LineState;  pub fn priced_at(&self, row: usize) -> Option<DateTime<Utc>>;
    pub fn is_line(&self, row: usize) -> bool;  pub fn is_package(&self, row: usize) -> bool;
    pub fn children(&self, row: usize) -> Range<usize>;  pub fn roots(&self) -> impl Iterator<Item = usize> + '_;
    pub fn effective_shifts(&self, row: usize) -> Shifts;  pub fn request(&self, row: usize) -> Option<PriceRequest>;
    pub fn stale_lines(&self) -> impl Iterator<Item = usize> + '_;
    pub fn record(&self, row: usize) -> RowRecord;
    pub fn deliver(&mut self, id: LineId, revision: u64, result: Result<PriceResult, String>, at: DateTime<Utc>) -> Delivered;
    pub fn fold_packages(&mut self);
    pub fn shorthand(&self, row: usize) -> String;
}
```
- Produces (in `geode_pricer::core::edit`, this task's subset):
```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Insert { place: Place, rows: Vec<RowSpec> },
    Remove { at: usize },
    Restore { at: usize, rows: Vec<RowRecord> },
    SetInstrument { row: usize, instrument: Instrument },      // Task 5
    SetQty { row: usize, qty: i64 },                           // Task 5
    SetShift { row: usize, shift: OwnShifts },                 // Task 5
    Move { row: usize, delta: isize },                         // Task 6
    Group { first: usize, count: usize, template: Template, id: Option<LineId> },   // Task 6
    Ungroup { row: usize },                                    // Task 6
    SetSheetShift(OwnShifts),                                  // Task 5
    SetSpotOverride { underlying: String, level: Option<f64> },   // Task 5
}
#[derive(Debug, Clone, PartialEq)] pub struct Undo { pub inverse: Vec<Edit> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError { NoSuchRow(usize), NotAPackage(usize), NotALine(usize), NotARootBoundary(usize), PackageInsidePackage, LegOutOfRange { package: usize, leg: usize }, IdInUse(LineId), NoSuchParent(LineId), ZeroQty, EmptyInsert, NotContiguousRoots, MoveOffEnd }
impl std::fmt::Display for EditError { .. }   // the footer's text
impl Sheet {
    pub fn apply(&mut self, edit: Edit) -> Result<Undo, EditError>;
    pub fn undo(&mut self, undo: &Undo) -> Result<Undo, EditError>;   // Task 6
}
```

Row order invariants the code keeps: a package's legs follow it contiguously; depth is 0 or 1; `parent[i]` is the flat index of the package a leg belongs to, recomputed by `reindex_parents` after every structural edit (an insert, a remove, later a move/group/ungroup) by walking the rows once — a row whose `parent` is `Some(_)` belongs to the most recent package row before it. New rows start at `revision = 1`, `state = Stale`, no result.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-pricer/src/core/sheet.rs` with only this tests block:

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::edit::{Edit, EditError};
    use chrono::TimeZone;
    use geode_core::pricing::{Expiry, OptionKind, Strike, Vanilla};

    pub(crate) fn spx(strike: f64, kind: OptionKind) -> Instrument {
        Instrument::Vanilla(Vanilla {
            underlying: "SPX".into(),
            expiry: Expiry::Date(chrono::NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()),
            strike: Strike::Absolute(strike),
            kind,
        })
    }

    pub(crate) fn line(instrument: Instrument, qty: i64) -> RowSpec {
        RowSpec::Line(LineSpec {
            instrument,
            qty,
            shift: OwnShifts::default(),
        })
    }

    pub(crate) fn callspread(qty: i64) -> RowSpec {
        crate::core::shorthand::parse(&format!("{qty} SPX Z26 4800/5200 CS")).unwrap()
    }

    pub(crate) fn result(price: f64) -> PriceResult {
        PriceResult {
            price,
            delta: price / 10.0,
            gamma: 0.01,
            vega: 1.0,
            theta: -0.5,
            rho: 0.1,
        }
    }

    pub(crate) fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    /// `Insert` at the end of the roots.
    pub(crate) fn push(sheet: &mut Sheet, rows: Vec<RowSpec>) {
        let at = sheet.len();
        sheet
            .apply(Edit::Insert {
                place: Place::Root { at },
                rows,
            })
            .unwrap();
    }

    #[test]
    fn a_new_sheet_is_empty_and_named() {
        let s = Sheet::new("untitled-1");
        assert_eq!(s.name, "untitled-1");
        assert_eq!(s.view, "vanilla");
        assert!(s.is_empty());
        assert_eq!(s.roots().count(), 0);
        assert_eq!(s.refresh, Refresh::Default);
        assert_eq!(s.sheet_shift, OwnShifts::default());
    }

    #[test]
    fn inserted_rows_take_fresh_ids_start_stale_and_a_package_keeps_its_legs_contiguous() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 2)]);
        push(&mut s, vec![callspread(-5)]);
        assert_eq!(s.len(), 4);
        assert_eq!(s.roots().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(s.kind(0), RowKind::Line);
        assert_eq!(s.kind(1), RowKind::Package { template: Template::CS });
        assert_eq!(s.children(1), 2..4);
        assert_eq!(s.children(0), 1..1, "a line has no children");
        assert_eq!((s.depth(0), s.depth(1), s.depth(2), s.depth(3)), (0, 0, 1, 1));
        assert_eq!((s.parent(2), s.parent(3)), (Some(1), Some(1)));
        assert_eq!(
            (0..4).map(|r| s.id(r)).collect::<Vec<_>>(),
            vec![LineId(1), LineId(2), LineId(3), LineId(4)],
            "ids are monotonic in insertion order"
        );
        assert_eq!(s.index_of(LineId(4)), Some(3));
        assert_eq!(s.index_of(LineId(99)), None);
        assert_eq!(s.qty(0), 2);
        assert_eq!((s.qty(2), s.qty(3)), (-5, 5));
        assert_eq!(s.instrument(1), None, "a package has no instrument");
        assert!(s.instrument(2).is_some());
        for r in 0..4 {
            assert_eq!(s.revision(r), 1, "row {r}");
            assert_eq!(s.result(r), None);
        }
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(s.state(2), &LineState::Stale);
        assert_eq!(s.state(1), &LineState::Stale, "a package is stale while a leg is");
        assert_eq!(s.stale_lines().collect::<Vec<_>>(), vec![0, 2, 3], "lines only, never the package");
    }

    #[test]
    fn a_place_is_a_root_boundary_or_a_leg_slot() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        // Root { at } inside the leg run is refused.
        let e = s
            .apply(Edit::Insert {
                place: Place::Root { at: 1 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err();
        assert_eq!(e, EditError::NotARootBoundary(1));
        // Root { at } past the end is refused; at == len is the end.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Root { at: 4 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err(),
            EditError::NoSuchRow(4)
        );
        // A package spec at a leg place is refused (depth is at most two).
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Leg { package: 0, leg: 0 },
                rows: vec![callspread(1)],
            })
            .unwrap_err(),
            EditError::PackageInsidePackage
        );
        // A leg place on a line is refused.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Leg { package: 1, leg: 0 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err(),
            EditError::NotAPackage(1)
        );
        // leg may be 0..=children.len(); one past is refused.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Leg { package: 0, leg: 3 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err(),
            EditError::LegOutOfRange { package: 0, leg: 3 }
        );
        // A leg inserted at leg 1 lands between the two, as a leg.
        s.apply(Edit::Insert {
            place: Place::Leg { package: 0, leg: 1 },
            rows: vec![line(spx(5000.0, OptionKind::Put), 3)],
        })
        .unwrap();
        assert_eq!(s.children(0), 1..4);
        assert_eq!(s.qty(2), 3);
        assert_eq!(s.parent(2), Some(0));
        // A leg at leg == len appends; a root at len appends.
        s.apply(Edit::Insert {
            place: Place::Leg { package: 0, leg: 3 },
            rows: vec![line(spx(5100.0, OptionKind::Put), 4)],
        })
        .unwrap();
        assert_eq!(s.children(0), 1..5);
        assert_eq!(s.roots().collect::<Vec<_>>(), vec![0]);
        // Empty inserts and zero quantities are refused.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![],
            })
            .unwrap_err(),
            EditError::EmptyInsert
        );
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 0)],
            })
            .unwrap_err(),
            EditError::ZeroQty
        );
    }

    #[test]
    fn remove_takes_a_package_with_its_legs_and_a_leg_alone() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), callspread(1), line(spx(5100.0, OptionKind::Put), 1)]);
        assert_eq!(s.len(), 5);
        let undo = s.apply(Edit::Remove { at: 1 }).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.roots().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(s.id(1), LineId(5));
        match &undo.inverse[..] {
            [Edit::Restore { at: 1, rows }] => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].id, LineId(2));
                assert_eq!(rows[0].parent, None);
                assert_eq!(rows[1].parent, Some(LineId(2)));
                assert_eq!(rows[2].parent, Some(LineId(2)));
            }
            other => panic!("{other:?}"),
        }
        // A leg alone: the package stays, possibly empty (planning decision 8).
        push(&mut s, vec![callspread(1)]);
        s.apply(Edit::Remove { at: 3 }).unwrap();
        assert_eq!(s.children(2), 3..4);
        s.apply(Edit::Remove { at: 3 }).unwrap();
        assert_eq!(s.children(2), 3..3, "an empty package may exist");
        assert!(s.is_package(2));
        assert_eq!(s.state(2), &LineState::Fresh, "an empty package is fresh");
        assert_eq!(s.result(2), None);
        assert_eq!(s.apply(Edit::Remove { at: 9 }).unwrap_err(), EditError::NoSuchRow(9));
    }

    #[test]
    fn a_request_is_the_instrument_with_effective_shifts() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), callspread(1)]);
        assert_eq!(s.request(1), None, "a package has no request");
        let r = s.request(0).unwrap();
        assert_eq!(r.instrument, spx(5000.0, OptionKind::Call));
        assert_eq!(r.shifts, Shifts::default(), "both None → 0.0");
        s.sheet_shift = OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        };
        assert_eq!(s.effective_shifts(0), Shifts { spot_pct: 2.0, vol_pts: 0.0 }, "inherits the sheet's");
        // The direct field write above is test-only; the edit door is Task 5.
        // An own value wins per field.
        let mut own = s;
        own.sheet_shift = OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: Some(-1.0),
        };
        // A row's own shift is set through Task 5's SetShift; here use the
        // record/restore door to build one with an own vol shift.
        let mut rec = own.record(0);
        rec.shift = OwnShifts {
            spot_pct: None,
            vol_pts: Some(3.0),
        };
        own.apply(Edit::Remove { at: 0 }).unwrap();
        own.apply(Edit::Restore { at: 0, rows: vec![rec] }).unwrap();
        assert_eq!(own.effective_shifts(0), Shifts { spot_pct: 2.0, vol_pts: 3.0 });
    }

    #[test]
    fn a_delivery_for_an_old_revision_is_dropped_and_the_current_one_installed() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        let id = s.id(0);
        // Pretend an edit bumped the revision (Task 5 does this through apply).
        let mut rec = s.record(0);
        rec.revision = 2;
        s.apply(Edit::Remove { at: 0 }).unwrap();
        s.apply(Edit::Restore { at: 0, rows: vec![rec] }).unwrap();
        assert_eq!(s.revision(0), 2);
        assert_eq!(s.deliver(id, 1, Ok(result(10.0)), at(0)), Delivered::OldRevision { current: 2 });
        assert_eq!(s.result(0), None);
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(s.deliver(id, 3, Ok(result(10.0)), at(0)), Delivered::FutureRevision { current: 2 }, "a bug, dropped");
        assert_eq!(s.deliver(id, 2, Ok(result(10.0)), at(5)), Delivered::Installed);
        assert_eq!(s.result(0), Some(&result(10.0)));
        assert_eq!(s.state(0), &LineState::Fresh);
        assert_eq!(s.priced_at(0), Some(at(5)));
        assert_eq!(s.deliver(LineId(77), 1, Ok(result(1.0)), at(0)), Delivered::UnknownLine);
        // A failure installs Failed and keeps the last good result.
        assert_eq!(s.deliver(id, 2, Err("refused by the mock".into()), at(6)), Delivered::Installed);
        assert_eq!(s.state(0), &LineState::Failed("refused by the mock".into()));
        assert_eq!(s.result(0), Some(&result(10.0)));
        assert_eq!(s.priced_at(0), Some(at(6)));
        // A package row is not a line.
        push(&mut s, vec![callspread(1)]);
        assert_eq!(s.deliver(s.id(1), 1, Ok(result(1.0)), at(0)), Delivered::NotALine);
    }

    #[test]
    fn a_package_sums_qty_times_value_over_its_legs_with_signed_quantities() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(-5)]); // legs: -5 × 4800 call, +5 × 5200 call
        let (long, short) = (s.id(1), s.id(2));
        assert_eq!(s.result(0), None, "no sum until every leg has a result");
        s.deliver(long, 1, Ok(result(100.0)), at(0));
        assert_eq!(s.result(0), None);
        assert_eq!(s.state(0), &LineState::Stale);
        s.deliver(short, 1, Ok(result(40.0)), at(1));
        let sum = s.result(0).unwrap();
        // -5 × 100 + 5 × 40 = -300; delta: -5 × 10 + 5 × 4 = -30; gamma: 0 (−5 + 5 = 0 × 0.01)
        assert_eq!(sum.price, -300.0);
        assert_eq!(sum.delta, -30.0);
        assert_eq!(sum.gamma, 0.0);
        assert_eq!(sum.vega, 0.0);
        assert_eq!(sum.theta, 0.0);
        assert_eq!(sum.rho, 0.0);
        assert_eq!(s.state(0), &LineState::Fresh);
        assert_eq!(s.priced_at(0), Some(at(0)), "a package is as old as its oldest leg");
        // Failed wins over Stale (planning decision 9) and names the leg.
        s.deliver(long, 1, Err("refused by the mock".into()), at(2));
        match s.state(0) {
            LineState::Failed(m) => {
                assert!(m.contains("-5 SPX Z26 4800 C"), "{m}");
                assert!(m.contains("refused by the mock"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s.result(0), None, "a failed leg makes the sum uncomputable");
    }

    #[test]
    fn shorthand_renders_a_line_a_template_package_and_a_custom_one() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), -2), callspread(3)]);
        assert_eq!(s.shorthand(0), "-2 SPX Z26 5000 C");
        assert_eq!(s.shorthand(1), "3 SPX Z26 4800/5200 CS");
        assert_eq!(s.shorthand(2), "3 SPX Z26 4800 C");
        // A leg removed leaves the table: one line per remaining leg.
        s.apply(Edit::Remove { at: 3 }).unwrap();
        assert_eq!(s.shorthand(1), "3 SPX Z26 4800 C");
        push(&mut s, vec![callspread(1)]);
        s.apply(Edit::Remove { at: 4 }).unwrap();
        s.apply(Edit::Remove { at: 4 }).unwrap();
        assert_eq!(s.shorthand(3), "", "an empty package renders nothing");
    }
}
```

Create `crates/geode-pricer/src/core/edit.rs` with only this tests block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::sheet::{Delivered, LineId, LineState, Place, Sheet};
    use geode_core::pricing::OptionKind;

    #[test]
    fn undo_of_a_remove_reinstates_rows_with_ids_and_results_and_requests_nothing() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), callspread(2)]);
        for r in [0, 2, 3] {
            assert_eq!(s.deliver(s.id(r), 1, Ok(result(10.0 * r as f64 + 1.0)), at(r as i64)), Delivered::Installed);
        }
        let before: Vec<_> = (0..4).map(|r| s.record(r)).collect();
        let undo = s.apply(Edit::Remove { at: 1 }).unwrap();
        assert_eq!(s.len(), 1);
        // A new insert in between takes a NEW id — the removed ids are never reused.
        push(&mut s, vec![line(spx(5300.0, OptionKind::Put), 1)]);
        assert_eq!(s.id(1), LineId(5));
        s.apply(Edit::Remove { at: 1 }).unwrap();
        // Apply the inverse.
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(s.len(), 4);
        let after: Vec<_> = (0..4).map(|r| s.record(r)).collect();
        assert_eq!(after, before, "ids, results, states and priced_at all return");
        assert_eq!(s.stale_lines().count(), 0, "nothing is re-requested");
        assert_eq!(s.state(1), &LineState::Fresh, "the package folds back to fresh");
        // Restoring an id that is in use is refused.
        let rec = s.record(0);
        assert_eq!(
            s.apply(Edit::Restore { at: 4, rows: vec![rec] }).unwrap_err(),
            EditError::IdInUse(LineId(1))
        );
        // Restoring a leg whose parent is gone is refused.
        let mut leg = s.record(2);
        leg.id = LineId(50);
        leg.parent = Some(LineId(60));
        assert_eq!(
            s.apply(Edit::Restore { at: 2, rows: vec![leg] }).unwrap_err(),
            EditError::NoSuchParent(LineId(60))
        );
    }

    #[test]
    fn insert_then_its_inverse_is_identity_for_roots_and_legs() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        let before: Vec<_> = (0..s.len()).map(|r| s.record(r)).collect();
        let undo = s
            .apply(Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![line(spx(1.0, OptionKind::Call), 1), callspread(2)],
            })
            .unwrap();
        assert_eq!(s.len(), 7);
        assert_eq!(undo.inverse, vec![Edit::Remove { at: 0 }, Edit::Remove { at: 0 }]);
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!((0..s.len()).map(|r| s.record(r)).collect::<Vec<_>>(), before);
        let undo = s
            .apply(Edit::Insert {
                place: Place::Leg { package: 0, leg: 1 },
                rows: vec![line(spx(2.0, OptionKind::Put), 1), line(spx(3.0, OptionKind::Put), 1)],
            })
            .unwrap();
        assert_eq!(s.children(0), 1..5);
        assert_eq!(undo.inverse, vec![Edit::Remove { at: 2 }, Edit::Remove { at: 2 }]);
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!((0..s.len()).map(|r| s.record(r)).collect::<Vec<_>>(), before);
    }
}
```

- [ ] **Step 2: Move the three types**

Cut `OwnShifts`, `LineSpec` and `RowSpec` (with their doc comments) out of `shorthand.rs` and into `sheet.rs`; in `shorthand.rs` add `use crate::core::sheet::{LineSpec, OwnShifts, RowSpec};`. In `mod.rs` replace the shorthand re-export with `pub use shorthand::{ParseError, parse, render_expiry, render_line, render_package, render_strike};` and add `pub mod edit; pub mod sheet;` plus `pub use edit::{Edit, EditError, Undo}; pub use sheet::{Delivered, LineId, LineSpec, LineState, OwnShifts, Place, Refresh, RowKind, RowRecord, RowSpec, Sheet};`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer 2>&1 | grep -E 'error\[|^test result' | head`
Expected: compile errors — `Sheet`, `Edit` not found.

- [ ] **Step 4: Implement `sheet.rs`**

Above the tests:

```rust
//! The sheet (line-pricer spec §6.1): struct of arrays in sheet order, a
//! package's legs contiguous after it, depth 0 or 1. There is no separate
//! index to keep in step: `children` is a scan of the following rows'
//! `parent`, and `parent` itself is rebuilt by one walk after every
//! structural edit.
//!
//! Mutation goes through [`Sheet::apply`] (`edit.rs`); the two other
//! `pub` mutators are [`Sheet::deliver`] (a result landing) and
//! [`Sheet::fold_packages`] (a recompute), and both are called by
//! `apply` where they matter.

use crate::core::shorthand::{render_line, render_package};
use crate::core::template::Template;
use chrono::{DateTime, Utc};
use geode_core::pricing::{Instrument, MarketOverrides, PriceRequest, PriceResult, Shifts};
use std::ops::Range;
use std::time::Duration;

/// Per-sheet, monotonic, never reused (spec §6.1). `u64` so it is the
/// `id` a `PriceLine` carries and the `line` axis a document stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Line,
    Package { template: Template },
    /// Reserved for slice 2's per-underlying children (spec ruling 5).
    /// Nothing in slice 1 constructs it; `from_rows` refuses it.
    Underlying,
}

/// A line's own shifts; `None` inherits the sheet's (spec ruling 8).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OwnShifts {
    pub spot_pct: Option<f64>,
    pub vol_pts: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineState {
    Fresh,
    Stale,
    Failed(String),
}

/// The sheet's periodic reprice (spec §9.4): the app default, off, or its
/// own interval. Three states, because storage keeps three (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    Default,
    Off,
    Every(Duration),
}

/// One line as the parser or a caller describes it, before it has an id.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpec {
    pub instrument: Instrument,
    /// Signed; a sell is negative; never zero.
    pub qty: i64,
    pub shift: OwnShifts,
}

/// What one shorthand line means: a line, or a package with its legs.
#[derive(Debug, Clone, PartialEq)]
pub enum RowSpec {
    Line(LineSpec),
    Package {
        template: Template,
        legs: Vec<LineSpec>,
    },
}

/// One row in transit: what `Remove` records and `Restore` reinstates,
/// ids and results included, so undo of a removal re-requests nothing
/// (spec §6.2). Exists only inside an [`crate::core::edit::Undo`]; the
/// sheet never stores one.
#[derive(Debug, Clone, PartialEq)]
pub struct RowRecord {
    pub id: LineId,
    pub kind: RowKind,
    /// The parent's id (not index — indices move).
    pub parent: Option<LineId>,
    pub instrument: Option<Instrument>,
    pub qty: i64,
    pub shift: OwnShifts,
    pub revision: u64,
    pub result: Option<PriceResult>,
    pub state: LineState,
    pub priced_at: Option<DateTime<Utc>>,
}

/// Where an insert lands (planning decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A flat index that is a root boundary: `0`, `len()`, or the first
    /// row of a root. The new rows become roots.
    Root { at: usize },
    /// Leg slot `leg` (`0..=children.len()`) of the package at flat row
    /// `package`. The new rows become its legs.
    Leg { package: usize, leg: usize },
}

/// What `deliver` did with a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    Installed,
    UnknownLine,
    NotALine,
    /// An edit landed during the round trip; the answer is for an older
    /// request (spec §9.2). Dropped.
    OldRevision { current: u64 },
    /// A bug (spec §10.1): dropped and, by the tile, logged.
    FutureRevision { current: u64 },
}

pub struct Sheet {
    pub name: String,
    pub view: String,
    pub sheet_shift: OwnShifts,
    /// Sheet-wide, by underlying: spot levels now (spec ruling 1).
    pub overrides: MarketOverrides,
    pub refresh: Refresh,
    // per row, in sheet order
    ids: Vec<LineId>,
    kind: Vec<RowKind>,
    parent: Vec<Option<u32>>,
    instrument: Vec<Option<Instrument>>,
    qty: Vec<i64>,
    shift: Vec<OwnShifts>,
    revision: Vec<u64>,
    result: Vec<Option<PriceResult>>,
    state: Vec<LineState>,
    priced_at: Vec<Option<DateTime<Utc>>>,
    next_id: u64,
}

impl Sheet {
    pub fn new(name: &str) -> Sheet {
        Sheet {
            name: name.to_string(),
            view: "vanilla".to_string(),
            sheet_shift: OwnShifts::default(),
            overrides: MarketOverrides::default(),
            refresh: Refresh::Default,
            ids: Vec::new(),
            kind: Vec::new(),
            parent: Vec::new(),
            instrument: Vec::new(),
            qty: Vec::new(),
            shift: Vec::new(),
            revision: Vec::new(),
            result: Vec::new(),
            state: Vec::new(),
            priced_at: Vec::new(),
            next_id: 1,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn id(&self, row: usize) -> LineId {
        self.ids[row]
    }

    pub fn index_of(&self, id: LineId) -> Option<usize> {
        self.ids.iter().position(|i| *i == id)
    }

    pub fn kind(&self, row: usize) -> RowKind {
        self.kind[row]
    }

    pub fn parent(&self, row: usize) -> Option<usize> {
        self.parent[row].map(|p| p as usize)
    }

    pub fn depth(&self, row: usize) -> usize {
        usize::from(self.parent[row].is_some())
    }

    pub fn instrument(&self, row: usize) -> Option<&Instrument> {
        self.instrument[row].as_ref()
    }

    pub fn qty(&self, row: usize) -> i64 {
        self.qty[row]
    }

    pub fn shift(&self, row: usize) -> OwnShifts {
        self.shift[row]
    }

    pub fn revision(&self, row: usize) -> u64 {
        self.revision[row]
    }

    pub fn result(&self, row: usize) -> Option<&PriceResult> {
        self.result[row].as_ref()
    }

    pub fn state(&self, row: usize) -> &LineState {
        &self.state[row]
    }

    pub fn priced_at(&self, row: usize) -> Option<DateTime<Utc>> {
        self.priced_at[row]
    }

    pub fn is_line(&self, row: usize) -> bool {
        self.kind[row] == RowKind::Line
    }

    pub fn is_package(&self, row: usize) -> bool {
        matches!(self.kind[row], RowKind::Package { .. })
    }

    /// The contiguous run of legs after a package; empty for a line.
    pub fn children(&self, row: usize) -> Range<usize> {
        let start = row + 1;
        if !self.is_package(row) {
            return start..start;
        }
        let mut end = start;
        while end < self.len() && self.parent[end] == Some(row as u32) {
            end += 1;
        }
        start..end
    }

    pub fn roots(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(|r| self.parent[*r].is_none())
    }

    /// `own.or(sheet)` per field, `0.0` when both are `None` (spec §6.1).
    pub fn effective_shifts(&self, row: usize) -> Shifts {
        let own = self.shift[row];
        Shifts {
            spot_pct: own.spot_pct.or(self.sheet_shift.spot_pct).unwrap_or(0.0),
            vol_pts: own.vol_pts.or(self.sheet_shift.vol_pts).unwrap_or(0.0),
        }
    }

    /// The one place a line's request is assembled (spec §6.1). `None`
    /// on a package.
    pub fn request(&self, row: usize) -> Option<PriceRequest> {
        self.instrument[row].as_ref().map(|instrument| PriceRequest {
            instrument: instrument.clone(),
            shifts: self.effective_shifts(row),
        })
    }

    /// Lines (never packages) that are `Stale`: what the tile submits.
    pub fn stale_lines(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(|r| self.is_line(*r) && self.state[*r] == LineState::Stale)
    }

    pub fn record(&self, row: usize) -> RowRecord {
        RowRecord {
            id: self.ids[row],
            kind: self.kind[row],
            parent: self.parent(row).map(|p| self.ids[p]),
            instrument: self.instrument[row].clone(),
            qty: self.qty[row],
            shift: self.shift[row],
            revision: self.revision[row],
            result: self.result[row],
            state: self.state[row].clone(),
            priced_at: self.priced_at[row],
        }
    }

    /// A result landing (spec §9.2): installed only for a line that
    /// exists at exactly the answered revision. A failure installs
    /// `Failed` and keeps the last good result (the row paints `—`
    /// either way). Folds packages.
    pub fn deliver(
        &mut self,
        id: LineId,
        revision: u64,
        result: Result<PriceResult, String>,
        at: DateTime<Utc>,
    ) -> Delivered {
        let Some(row) = self.index_of(id) else {
            return Delivered::UnknownLine;
        };
        if !self.is_line(row) {
            return Delivered::NotALine;
        }
        let current = self.revision[row];
        if revision < current {
            return Delivered::OldRevision { current };
        }
        if revision > current {
            return Delivered::FutureRevision { current };
        }
        match result {
            Ok(r) => {
                self.result[row] = Some(r);
                self.state[row] = LineState::Fresh;
            }
            Err(message) => self.state[row] = LineState::Failed(message),
        }
        self.priced_at[row] = Some(at);
        self.fold_packages();
        Delivered::Installed
    }

    /// Every package's painted numbers are `Σ qty_leg × value_leg` over
    /// its legs, its state `Failed` (naming the first failed leg) if any
    /// leg is, else `Stale` if any leg is, else `Fresh`; its result is
    /// `Some` only when every leg has one; its `priced_at` the oldest
    /// leg's (spec §6.4, planning decision 9). Aggregation, not
    /// arithmetic (PHILOSOPHY §1).
    pub fn fold_packages(&mut self) {
        for p in 0..self.len() {
            if !self.is_package(p) {
                continue;
            }
            let legs = self.children(p);
            // An empty package has no sum (planning decision 8).
            let mut complete = !legs.is_empty();
            let mut sum = PriceResult {
                price: 0.0,
                delta: 0.0,
                gamma: 0.0,
                vega: 0.0,
                theta: 0.0,
                rho: 0.0,
            };
            let mut stale = false;
            let mut failed: Option<String> = None;
            let mut oldest: Option<DateTime<Utc>> = None;
            for leg in legs {
                match &self.state[leg] {
                    LineState::Failed(m) if failed.is_none() => {
                        failed = Some(format!(
                            "{}: {m}",
                            render_line(self.qty[leg], self.instrument[leg].as_ref().expect("a leg is a line"))
                        ));
                    }
                    LineState::Failed(_) => {}
                    LineState::Stale => stale = true,
                    LineState::Fresh => {}
                }
                match self.result[leg] {
                    Some(r) => {
                        let q = self.qty[leg] as f64;
                        sum.price += q * r.price;
                        sum.delta += q * r.delta;
                        sum.gamma += q * r.gamma;
                        sum.vega += q * r.vega;
                        sum.theta += q * r.theta;
                        sum.rho += q * r.rho;
                    }
                    None => complete = false,
                }
                oldest = match (oldest, self.priced_at[leg]) {
                    (None, t) => t,
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) => Some(a),
                };
            }
            self.result[p] = if complete && failed.is_none() { Some(sum) } else { None };
            self.state[p] = match failed {
                Some(m) => LineState::Failed(m),
                None if stale => LineState::Stale,
                None => LineState::Fresh,
            };
            self.priced_at[p] = oldest;
        }
    }

    /// The row in the grammar (spec §6.3): a line; a package in template
    /// form while its legs match the table, else its legs one per line;
    /// an empty package as nothing.
    pub fn shorthand(&self, row: usize) -> String {
        match self.kind[row] {
            RowKind::Line | RowKind::Underlying => match &self.instrument[row] {
                Some(i) => render_line(self.qty[row], i),
                None => String::new(),
            },
            RowKind::Package { template } => {
                let legs: Vec<(i64, &Instrument)> = self
                    .children(row)
                    .filter_map(|l| self.instrument[l].as_ref().map(|i| (self.qty[l], i)))
                    .collect();
                render_package(template, &legs).unwrap_or_else(|| {
                    legs.iter()
                        .map(|(q, i)| render_line(*q, i))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
        }
    }

    // ---- the structural primitives `edit.rs` builds on (pub(crate)) ----

    /// The next fresh id; never reused (spec §6.1).
    pub(crate) fn fresh_id(&mut self) -> LineId {
        let id = LineId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Whether `id` is currently a row.
    pub(crate) fn has_id(&self, id: LineId) -> bool {
        self.ids.contains(&id)
    }

    /// Insert one row at flat `at`, with `parent` as a flat index (or
    /// `None` for a root). The caller calls `reindex_parents` afterwards
    /// once its whole batch is in. Bumps `next_id` past a restored id.
    pub(crate) fn splice_in(&mut self, at: usize, rec: RowRecord, parent: Option<usize>) {
        self.ids.insert(at, rec.id);
        self.kind.insert(at, rec.kind);
        self.parent.insert(at, parent.map(|p| p as u32));
        self.instrument.insert(at, rec.instrument);
        self.qty.insert(at, rec.qty);
        self.shift.insert(at, rec.shift);
        self.revision.insert(at, rec.revision);
        self.result.insert(at, rec.result);
        self.state.insert(at, rec.state);
        self.priced_at.insert(at, rec.priced_at);
        self.next_id = self.next_id.max(rec.id.0 + 1);
    }

    /// Remove one row at flat `at`, answering its record (with the
    /// parent's ID, resolved before the removal).
    pub(crate) fn take_out(&mut self, at: usize) -> RowRecord {
        let rec = self.record(at);
        self.ids.remove(at);
        self.kind.remove(at);
        self.parent.remove(at);
        self.instrument.remove(at);
        self.qty.remove(at);
        self.shift.remove(at);
        self.revision.remove(at);
        self.result.remove(at);
        self.state.remove(at);
        self.priced_at.remove(at);
        rec
    }

    /// Rebuild `parent` after a structural edit: a row marked as a leg
    /// (`Some(_)`) belongs to the most recent package before it. One
    /// walk, no allocation.
    pub(crate) fn reindex_parents(&mut self) {
        let mut package: Option<u32> = None;
        for i in 0..self.len() {
            if self.is_package(i) {
                package = Some(i as u32);
                // A package is always a root in slice 1.
                self.parent[i] = None;
            } else if self.parent[i].is_some() {
                self.parent[i] = package;
            }
        }
    }

    /// Mark a leg (`Some`) or root (`None`) ahead of a `reindex_parents`.
    pub(crate) fn set_leg_marker(&mut self, row: usize, leg: bool) {
        self.parent[row] = if leg { Some(u32::MAX) } else { None };
    }

    pub(crate) fn set_instrument(&mut self, row: usize, instrument: Instrument) {
        self.instrument[row] = Some(instrument);
    }

    pub(crate) fn set_qty(&mut self, row: usize, qty: i64) {
        self.qty[row] = qty;
    }

    pub(crate) fn set_shift(&mut self, row: usize, shift: OwnShifts) {
        self.shift[row] = shift;
    }

    pub(crate) fn set_kind(&mut self, row: usize, kind: RowKind) {
        self.kind[row] = kind;
    }

    /// Bump the revision and mark stale: the line's request changed.
    pub(crate) fn touch(&mut self, row: usize) {
        self.revision[row] += 1;
        self.state[row] = LineState::Stale;
    }

    /// Rotate the flat range `a.start..b.end` so block `b` comes before
    /// block `a` (the two are adjacent: `a.end == b.start`).
    pub(crate) fn swap_adjacent_blocks(&mut self, a: Range<usize>, b: Range<usize>) {
        debug_assert_eq!(a.end, b.start);
        let whole = a.start..b.end;
        let by = a.len();
        self.ids[whole.clone()].rotate_left(by);
        self.kind[whole.clone()].rotate_left(by);
        self.parent[whole.clone()].rotate_left(by);
        self.instrument[whole.clone()].rotate_left(by);
        self.qty[whole.clone()].rotate_left(by);
        self.shift[whole.clone()].rotate_left(by);
        self.revision[whole.clone()].rotate_left(by);
        self.result[whole.clone()].rotate_left(by);
        self.state[whole.clone()].rotate_left(by);
        self.priced_at[whole].rotate_left(by);
    }

    /// A fresh record for a spec, at `revision = 1`, `Stale`, unpriced.
    pub(crate) fn new_record(&mut self, spec: &LineSpec, parent: Option<LineId>) -> RowRecord {
        RowRecord {
            id: self.fresh_id(),
            kind: RowKind::Line,
            parent,
            instrument: Some(spec.instrument.clone()),
            qty: spec.qty,
            shift: spec.shift,
            revision: 1,
            result: None,
            state: LineState::Stale,
            priced_at: None,
        }
    }

    pub(crate) fn new_package_record(&mut self, template: Template, id: Option<LineId>) -> RowRecord {
        RowRecord {
            id: id.unwrap_or_else(|| self.fresh_id()),
            kind: RowKind::Package { template },
            parent: None,
            instrument: None,
            qty: 1,
            shift: OwnShifts::default(),
            revision: 1,
            result: None,
            state: LineState::Fresh,
            priced_at: None,
        }
    }
}
```

- [ ] **Step 5: Implement `edit.rs` (this task's subset; Tasks 5 and 6 fill the other arms)**

```rust
//! The one mutation door (line-pricer spec §6.2): every change to a
//! sheet is an [`Edit`], `apply` answers the inverse as an [`Undo`], and
//! decides which lines are re-requested by comparing `Sheet::request`
//! before and after (spec §9.3). Undo of a removal reinstates ids and
//! results (a `Restore`), so it requests nothing.

use crate::core::sheet::{LineId, LineSpec, OwnShifts, Place, RowKind, RowRecord, RowSpec, Sheet};
use crate::core::template::Template;
use geode_core::pricing::{Instrument, PriceRequest};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// A line, or a package with its legs, or several roots (decision 1).
    Insert { place: Place, rows: Vec<RowSpec> },
    /// A package removes its legs.
    Remove { at: usize },
    /// The inverse of `Remove`: rows back with their ids, results and
    /// states (decision 3). A caller other than `undo` has no reason to
    /// build one.
    Restore { at: usize, rows: Vec<RowRecord> },
    SetInstrument { row: usize, instrument: Instrument },
    SetQty { row: usize, qty: i64 },
    SetShift { row: usize, shift: OwnShifts },
    /// Within the parent; `delta` in sibling steps.
    Move { row: usize, delta: isize },
    /// `first` and the next `count − 1` roots (all lines) become one
    /// package; `id: None` takes a fresh id, `Some` is undo's.
    Group { first: usize, count: usize, template: Template, id: Option<LineId> },
    Ungroup { row: usize },
    SetSheetShift(OwnShifts),
    /// `None` clears (spec ruling 1).
    SetSpotOverride { underlying: String, level: Option<f64> },
}

/// The inverse of one `apply`, in the order to apply it.
#[derive(Debug, Clone, PartialEq)]
pub struct Undo {
    pub inverse: Vec<Edit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    NoSuchRow(usize),
    NotAPackage(usize),
    NotALine(usize),
    /// A `Place::Root { at }` inside a package's leg run.
    NotARootBoundary(usize),
    /// A package spec at a leg place: depth is at most two (ruling 5).
    PackageInsidePackage,
    LegOutOfRange { package: usize, leg: usize },
    IdInUse(LineId),
    NoSuchParent(LineId),
    ZeroQty,
    EmptyInsert,
    NotContiguousRoots,
    MoveOffEnd,
}

impl fmt::Display for EditError {
    /// The footer's text (spec §6.2, §8.3).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NoSuchRow(r) => write!(f, "no row {r}"),
            EditError::NotAPackage(r) => write!(f, "row {r} is not a package"),
            EditError::NotALine(r) => write!(f, "row {r} is not a line"),
            EditError::NotARootBoundary(r) => write!(f, "row {r} is inside a package"),
            EditError::PackageInsidePackage => write!(f, "a package cannot hold a package"),
            EditError::LegOutOfRange { package, leg } => write!(f, "package {package} has no leg slot {leg}"),
            EditError::IdInUse(id) => write!(f, "line {} already exists", id.0),
            EditError::NoSuchParent(id) => write!(f, "no package {} to restore into", id.0),
            EditError::ZeroQty => write!(f, "quantity must not be zero"),
            EditError::EmptyInsert => write!(f, "nothing to insert"),
            EditError::NotContiguousRoots => write!(f, "group needs a contiguous run of top-level lines"),
            EditError::MoveOffEnd => write!(f, "cannot move past the end"),
        }
    }
}

impl Sheet {
    /// The one door (spec §6.2). Bumps `revision` and sets `Stale` on
    /// every line whose `request()` changed (spec §9.3), folds packages,
    /// and answers the inverse. On an error nothing has changed.
    pub fn apply(&mut self, edit: Edit) -> Result<Undo, EditError> {
        let touched = self.touched_by(&edit);
        let before: Vec<(LineId, Option<PriceRequest>)> = touched
            .iter()
            .map(|id| (*id, self.index_of(*id).and_then(|r| self.request(r))))
            .collect();
        let undo = self.apply_inner(edit)?;
        for (id, old) in before {
            if let Some(row) = self.index_of(id)
                && self.request(row) != old
            {
                self.touch(row);
            }
        }
        self.fold_packages();
        Ok(undo)
    }

    /// The lines whose request an edit CAN change, by id (so the compare
    /// survives the edit moving rows). `SetSpotOverride` is not here: its
    /// request is unchanged by design (§9.3) and `apply_inner` stales
    /// its lines explicitly (decision 4).
    fn touched_by(&self, edit: &Edit) -> Vec<LineId> {
        match edit {
            Edit::SetInstrument { row, .. } | Edit::SetShift { row, .. } => {
                (*row < self.len()).then(|| self.id(*row)).into_iter().collect()
            }
            Edit::SetSheetShift(_) => (0..self.len()).filter(|r| self.is_line(*r)).map(|r| self.id(r)).collect(),
            Edit::Insert { .. }
            | Edit::Remove { .. }
            | Edit::Restore { .. }
            | Edit::SetQty { .. }
            | Edit::Move { .. }
            | Edit::Group { .. }
            | Edit::Ungroup { .. }
            | Edit::SetSpotOverride { .. } => Vec::new(),
        }
    }

    fn apply_inner(&mut self, edit: Edit) -> Result<Undo, EditError> {
        match edit {
            Edit::Insert { place, rows } => self.insert(place, rows),
            Edit::Remove { at } => self.remove(at),
            Edit::Restore { at, rows } => self.restore(at, rows),
            Edit::SetInstrument { row, instrument } => todo!("Task 5"),
            Edit::SetQty { row, qty } => todo!("Task 5"),
            Edit::SetShift { row, shift } => todo!("Task 5"),
            Edit::Move { row, delta } => todo!("Task 6"),
            Edit::Group { first, count, template, id } => todo!("Task 6"),
            Edit::Ungroup { row } => todo!("Task 6"),
            Edit::SetSheetShift(shift) => todo!("Task 5"),
            Edit::SetSpotOverride { underlying, level } => todo!("Task 5"),
        }
    }

    fn row_exists(&self, row: usize) -> Result<(), EditError> {
        if row < self.len() {
            Ok(())
        } else {
            Err(EditError::NoSuchRow(row))
        }
    }

    fn insert(&mut self, place: Place, rows: Vec<RowSpec>) -> Result<Undo, EditError> {
        if rows.is_empty() {
            return Err(EditError::EmptyInsert);
        }
        let zero = rows.iter().any(|r| match r {
            RowSpec::Line(l) => l.qty == 0,
            RowSpec::Package { legs, .. } => legs.iter().any(|l| l.qty == 0),
        });
        if zero {
            return Err(EditError::ZeroQty);
        }
        match place {
            Place::Root { at } => {
                if at > self.len() {
                    return Err(EditError::NoSuchRow(at));
                }
                if at < self.len() && self.depth(at) != 0 {
                    return Err(EditError::NotARootBoundary(at));
                }
                let mut cursor = at;
                for spec in &rows {
                    match spec {
                        RowSpec::Line(l) => {
                            let rec = self.new_record(l, None);
                            self.splice_in(cursor, rec, None);
                            cursor += 1;
                        }
                        RowSpec::Package { template, legs } => {
                            let pkg = self.new_package_record(*template, None);
                            let pkg_id = pkg.id;
                            let pkg_row = cursor;
                            self.splice_in(cursor, pkg, None);
                            cursor += 1;
                            for l in legs {
                                let rec = self.new_record(l, Some(pkg_id));
                                self.splice_in(cursor, rec, Some(pkg_row));
                                cursor += 1;
                            }
                        }
                    }
                }
                self.reindex_parents();
                Ok(Undo {
                    inverse: vec![Edit::Remove { at }; rows.len()],
                })
            }
            Place::Leg { package, leg } => {
                self.row_exists(package)?;
                if !self.is_package(package) {
                    return Err(EditError::NotAPackage(package));
                }
                let children = self.children(package);
                if leg > children.len() {
                    return Err(EditError::LegOutOfRange { package, leg });
                }
                let mut lines: Vec<&LineSpec> = Vec::with_capacity(rows.len());
                for spec in &rows {
                    match spec {
                        RowSpec::Line(l) => lines.push(l),
                        RowSpec::Package { .. } => return Err(EditError::PackageInsidePackage),
                    }
                }
                let at = children.start + leg;
                let pkg_id = self.id(package);
                let mut cursor = at;
                for l in lines {
                    let rec = self.new_record(l, Some(pkg_id));
                    self.splice_in(cursor, rec, Some(package));
                    cursor += 1;
                }
                self.reindex_parents();
                Ok(Undo {
                    inverse: vec![Edit::Remove { at }; rows.len()],
                })
            }
        }
    }

    fn remove(&mut self, at: usize) -> Result<Undo, EditError> {
        self.row_exists(at)?;
        let count = 1 + self.children(at).len();
        let mut rows = Vec::with_capacity(count);
        for _ in 0..count {
            rows.push(self.take_out(at));
        }
        self.reindex_parents();
        Ok(Undo {
            inverse: vec![Edit::Restore { at, rows }],
        })
    }

    fn restore(&mut self, at: usize, rows: Vec<RowRecord>) -> Result<Undo, EditError> {
        if rows.is_empty() {
            return Err(EditError::EmptyInsert);
        }
        if at > self.len() {
            return Err(EditError::NoSuchRow(at));
        }
        if let Some(rec) = rows.iter().find(|r| self.has_id(r.id)) {
            return Err(EditError::IdInUse(rec.id));
        }
        // Every leg's parent must be a package in the sheet or in this batch.
        for rec in &rows {
            if let Some(pid) = rec.parent
                && !self.has_id(pid)
                && !rows.iter().any(|r| r.id == pid && matches!(r.kind, RowKind::Package { .. }))
            {
                return Err(EditError::NoSuchParent(pid));
            }
        }
        let n = rows.len();
        let mut cursor = at;
        for rec in rows {
            let leg = rec.parent.is_some();
            self.splice_in(cursor, rec, None);
            self.set_leg_marker(cursor, leg);
            cursor += 1;
        }
        self.reindex_parents();
        // The inverse removes what was restored: each `Remove { at }`
        // takes one ROOT (with its legs) or one leg. Count the top-level
        // records restored: roots, plus legs whose parent was NOT in the
        // batch.
        let restored_top = (at..at + n)
            .filter(|r| self.parent(*r).is_none_or(|p| p < at))
            .count();
        Ok(Undo {
            inverse: vec![Edit::Remove { at }; restored_top],
        })
    }
}
```

`restore`'s top-level count: a root restored with its legs is one `Remove`; a leg restored into an existing package (its parent index is before `at`) is one `Remove` of its own; a leg whose package is in the batch is covered by the package's `Remove`. `Option::is_none_or` is stable since Rust 1.82.

The `todo!` arms are a compile-time placeholder for two tasks; clippy allows `todo!`. Keep the variable names so the arms read as intended.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo fmt --check`
Expected: every test in `sheet::tests` and `edit::tests` PASS (11 new), the earlier 17 still green. If `unused variable` warnings fire on the `todo!` arms, prefix the bindings with `_` for now (Tasks 5/6 rename them back).

- [ ] **Step 7: Commit**

```bash
git add crates/geode-pricer
git commit -m "pricer: Sheet as struct of arrays — insert, remove, restore, requests, deliveries, package folding"
```

---

### Task 5: The cell edits — `SetInstrument`, `SetQty`, `SetShift`, `SetSheetShift`, `SetSpotOverride` and the revision rule

**Files:**
- Modify: `crates/geode-pricer/src/core/edit.rs`
- Test: `crates/geode-pricer/src/core/edit.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 4's `Sheet` primitives (`set_instrument`, `set_qty`, `set_shift`, `touch`, `index_of`).
- Produces: the five arms of `apply_inner`; no new public items.

- [ ] **Step 1: Write the failing tests**

Append to `edit.rs`'s tests:

```rust
    #[test]
    fn set_instrument_bumps_the_revision_and_stales_only_that_line() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), line(spx(5100.0, OptionKind::Call), 1)]);
        s.deliver(s.id(0), 1, Ok(result(1.0)), at(0));
        s.deliver(s.id(1), 1, Ok(result(2.0)), at(0));
        assert_eq!(s.stale_lines().count(), 0);
        let undo = s
            .apply(Edit::SetInstrument { row: 0, instrument: spx(5050.0, OptionKind::Call) })
            .unwrap();
        assert_eq!(s.revision(0), 2);
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(s.result(0), Some(&result(1.0)), "the old result stays painted, muted, until the new one lands");
        assert_eq!(s.revision(1), 1);
        assert_eq!(s.state(1), &LineState::Fresh);
        assert_eq!(undo.inverse, vec![Edit::SetInstrument { row: 0, instrument: spx(5000.0, OptionKind::Call) }]);
        // The same instrument again is no change: no bump.
        s.apply(Edit::SetInstrument { row: 0, instrument: spx(5050.0, OptionKind::Call) }).unwrap();
        assert_eq!(s.revision(0), 2);
        // Undo restores the old instrument, which IS a request change: re-requested (spec §9.3).
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(s.revision(0), 3);
        assert_eq!(s.state(0), &LineState::Stale);
        // On a package it is refused.
        push(&mut s, vec![callspread(1)]);
        assert_eq!(
            s.apply(Edit::SetInstrument { row: 2, instrument: spx(1.0, OptionKind::Call) }).unwrap_err(),
            EditError::NotALine(2)
        );
        assert_eq!(
            s.apply(Edit::SetInstrument { row: 9, instrument: spx(1.0, OptionKind::Call) }).unwrap_err(),
            EditError::NoSuchRow(9)
        );
    }

    #[test]
    fn set_qty_and_move_change_no_request() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), callspread(1)]);
        for r in [0, 2, 3] {
            s.deliver(s.id(r), 1, Ok(result(10.0)), at(0));
        }
        let undo = s.apply(Edit::SetQty { row: 0, qty: -3 }).unwrap();
        assert_eq!(s.qty(0), -3);
        assert_eq!(s.revision(0), 1);
        assert_eq!(s.state(0), &LineState::Fresh);
        assert_eq!(undo.inverse, vec![Edit::SetQty { row: 0, qty: 1 }]);
        // A leg's qty re-sums the package at once (a 1×2 ratio, spec §6.4).
        s.apply(Edit::SetQty { row: 3, qty: -2 }).unwrap();
        assert_eq!(s.result(1).unwrap().price, 10.0 - 20.0);
        assert_eq!(s.state(1), &LineState::Fresh);
        assert_eq!(s.apply(Edit::SetQty { row: 0, qty: 0 }).unwrap_err(), EditError::ZeroQty);
        assert_eq!(s.apply(Edit::SetQty { row: 1, qty: 2 }).unwrap_err(), EditError::NotALine(1));
        assert_eq!(s.stale_lines().count(), 0, "nothing was re-requested by any of it");
    }

    #[test]
    fn set_shift_changes_the_request_only_when_the_effective_value_moves() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        s.deliver(s.id(0), 1, Ok(result(1.0)), at(0));
        let undo = s
            .apply(Edit::SetShift { row: 0, shift: OwnShifts { spot_pct: Some(2.0), vol_pts: None } })
            .unwrap();
        assert_eq!(s.revision(0), 2);
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(undo.inverse, vec![Edit::SetShift { row: 0, shift: OwnShifts::default() }]);
        s.deliver(s.id(0), 2, Ok(result(1.0)), at(1));
        // Setting the own value to what the sheet already gives changes nothing.
        s.apply(Edit::SetSheetShift(OwnShifts { spot_pct: Some(2.0), vol_pts: None })).unwrap();
        assert_eq!(s.revision(0), 2, "own 2.0 over sheet 2.0: the effective value did not move");
        s.apply(Edit::SetShift { row: 0, shift: OwnShifts::default() }).unwrap();
        assert_eq!(s.revision(0), 2, "clearing the own value: still 2.0 through the sheet");
        assert_eq!(s.state(0), &LineState::Fresh);
    }

    #[test]
    fn a_sheet_shift_reprices_only_lines_that_inherit_it() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), line(spx(5100.0, OptionKind::Call), 1), callspread(1)]);
        for r in [0, 1, 3, 4] {
            s.deliver(s.id(r), 1, Ok(result(1.0)), at(0));
        }
        s.apply(Edit::SetShift { row: 1, shift: OwnShifts { spot_pct: Some(5.0), vol_pts: None } }).unwrap();
        s.deliver(s.id(1), 2, Ok(result(1.0)), at(1));
        assert_eq!(s.stale_lines().count(), 0);
        let undo = s.apply(Edit::SetSheetShift(OwnShifts { spot_pct: Some(2.0), vol_pts: None })).unwrap();
        assert_eq!(s.sheet_shift, OwnShifts { spot_pct: Some(2.0), vol_pts: None });
        assert_eq!(s.stale_lines().collect::<Vec<_>>(), vec![0, 3, 4], "row 1 has its own spot shift");
        assert_eq!(s.revision(1), 2);
        assert_eq!(s.state(2), &LineState::Stale, "the package follows its legs");
        assert_eq!(undo.inverse, vec![Edit::SetSheetShift(OwnShifts::default())]);
        // Vol alone touches the rows that inherit vol — all of them here.
        for r in [0, 3, 4] {
            s.deliver(s.id(r), 2, Ok(result(1.0)), at(2));
        }
        s.apply(Edit::SetSheetShift(OwnShifts { spot_pct: Some(2.0), vol_pts: Some(-1.0) })).unwrap();
        assert_eq!(s.stale_lines().collect::<Vec<_>>(), vec![0, 1, 3, 4]);
    }

    #[test]
    fn a_spot_override_stales_every_line_on_that_underlying_and_only_a_changed_level_does() {
        let mut s = Sheet::new("t");
        let ndx = crate::core::shorthand::parse("NDX Z26 20000 C").unwrap();
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), ndx, callspread(1)]);
        for r in [0, 1, 3, 4] {
            s.deliver(s.id(r), 1, Ok(result(1.0)), at(0));
        }
        let undo = s
            .apply(Edit::SetSpotOverride { underlying: "SPX".into(), level: Some(5100.0) })
            .unwrap();
        assert_eq!(s.overrides.spot.get("SPX"), Some(&5100.0));
        assert_eq!(s.stale_lines().collect::<Vec<_>>(), vec![0, 3, 4], "NDX is untouched");
        assert_eq!(s.revision(0), 2);
        assert_eq!(s.revision(1), 1);
        assert_eq!(undo.inverse, vec![Edit::SetSpotOverride { underlying: "SPX".into(), level: None }]);
        for r in [0, 3, 4] {
            s.deliver(s.id(r), 2, Ok(result(1.0)), at(1));
        }
        // The same level again is no change.
        s.apply(Edit::SetSpotOverride { underlying: "SPX".into(), level: Some(5100.0) }).unwrap();
        assert_eq!(s.stale_lines().count(), 0);
        // Clearing an override that is not set is no change either.
        s.apply(Edit::SetSpotOverride { underlying: "RTY".into(), level: None }).unwrap();
        assert_eq!(s.stale_lines().count(), 0);
        assert!(!s.overrides.spot.contains_key("RTY"));
        // Clearing SPX stales SPX again and the inverse carries the old level.
        let undo = s.apply(Edit::SetSpotOverride { underlying: "SPX".into(), level: None }).unwrap();
        assert_eq!(s.stale_lines().collect::<Vec<_>>(), vec![0, 3, 4]);
        assert_eq!(undo.inverse, vec![Edit::SetSpotOverride { underlying: "SPX".into(), level: Some(5100.0) }]);
        // The underlying is matched case-insensitively, stored upper-case.
        s.apply(Edit::SetSpotOverride { underlying: "ndx".into(), level: Some(1.0) }).unwrap();
        assert_eq!(s.overrides.spot.get("NDX"), Some(&1.0));
        assert_eq!(s.state(1), &LineState::Stale);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer edit:: 2>&1 | grep -E 'panicked|^test result' | head`
Expected: the five new tests FAIL on `not yet implemented: Task 5`.

- [ ] **Step 3: Implement the five arms**

Replace the five `todo!("Task 5")` arms in `apply_inner`:

```rust
            Edit::SetInstrument { row, instrument } => {
                self.row_exists(row)?;
                if !self.is_line(row) {
                    return Err(EditError::NotALine(row));
                }
                let old = self.instrument(row).cloned().expect("a line has an instrument");
                self.set_instrument(row, instrument);
                Ok(Undo {
                    inverse: vec![Edit::SetInstrument { row, instrument: old }],
                })
            }
            Edit::SetQty { row, qty } => {
                self.row_exists(row)?;
                if !self.is_line(row) {
                    return Err(EditError::NotALine(row));
                }
                if qty == 0 {
                    return Err(EditError::ZeroQty);
                }
                let old = self.qty(row);
                self.set_qty(row, qty);
                Ok(Undo {
                    inverse: vec![Edit::SetQty { row, qty: old }],
                })
            }
            Edit::SetShift { row, shift } => {
                self.row_exists(row)?;
                if !self.is_line(row) {
                    return Err(EditError::NotALine(row));
                }
                let old = self.shift(row);
                self.set_shift(row, shift);
                Ok(Undo {
                    inverse: vec![Edit::SetShift { row, shift: old }],
                })
            }
            Edit::SetSheetShift(shift) => {
                let old = self.sheet_shift;
                self.sheet_shift = shift;
                Ok(Undo {
                    inverse: vec![Edit::SetSheetShift(old)],
                })
            }
            Edit::SetSpotOverride { underlying, level } => {
                let key = underlying.to_ascii_uppercase();
                let old = self.overrides.spot.get(&key).copied();
                if old != level {
                    match level {
                        Some(l) => {
                            self.overrides.spot.insert(key.clone(), l);
                        }
                        None => {
                            self.overrides.spot.remove(&key);
                        }
                    }
                    // The request is unchanged by design (§9.3: overrides
                    // ride in `PriceParams`), so the compare in `apply`
                    // cannot see this; stale the lines explicitly.
                    for row in 0..self.len() {
                        if self.is_line(row) && self.instrument(row).is_some_and(|i| i.underlying() == key) {
                            self.touch(row);
                        }
                    }
                }
                Ok(Undo {
                    inverse: vec![Edit::SetSpotOverride { underlying: key, level: old }],
                })
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-pricer
git commit -m "pricer: the cell edits and the revision rule — a request change stales its line, qty does not"
```

---

### Task 6: `Move`, `Group`, `Ungroup`, and `Sheet::undo`

**Files:**
- Modify: `crates/geode-pricer/src/core/edit.rs`
- Test: `crates/geode-pricer/src/core/edit.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 4's primitives (`swap_adjacent_blocks`, `set_leg_marker`, `reindex_parents`, `splice_in`, `take_out`, `new_package_record`, `has_id`).
- Produces:
```rust
impl Sheet {
    /// Apply an `Undo` (its edits in order) and answer the redo.
    pub fn undo(&mut self, undo: &Undo) -> Result<Undo, EditError>;
}
```

- [ ] **Step 1: Write the failing tests**

Append to `edit.rs`'s tests:

```rust
    fn ids(s: &Sheet) -> Vec<u64> {
        (0..s.len()).map(|r| s.id(r).0).collect()
    }

    #[test]
    fn move_stays_within_the_parent_and_carries_a_packages_legs() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(1.0, OptionKind::Call), 1), callspread(1), line(spx(2.0, OptionKind::Call), 1)]);
        // ids: 1 | 2 (3 4) | 5
        let undo = s.apply(Edit::Move { row: 0, delta: 1 }).unwrap();
        assert_eq!(ids(&s), vec![2, 3, 4, 1, 5], "the line hops the whole package");
        assert_eq!(s.parent(1), Some(0));
        assert_eq!(s.parent(2), Some(0));
        assert_eq!(undo.inverse, vec![Edit::Move { row: 3, delta: -1 }]);
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(ids(&s), vec![1, 2, 3, 4, 5]);
        // The package moves down, legs with it.
        s.apply(Edit::Move { row: 1, delta: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5, 2, 3, 4]);
        assert_eq!(s.children(2), 3..5);
        // A leg moves within its package only.
        s.apply(Edit::Move { row: 3, delta: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5, 2, 4, 3]);
        assert_eq!(s.apply(Edit::Move { row: 4, delta: 1 }).unwrap_err(), EditError::MoveOffEnd);
        s.apply(Edit::Move { row: 4, delta: -1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5, 2, 3, 4]);
        assert_eq!(s.apply(Edit::Move { row: 3, delta: -1 }).unwrap_err(), EditError::MoveOffEnd, "the first leg cannot leave the package");
        assert_eq!(s.apply(Edit::Move { row: 0, delta: -1 }).unwrap_err(), EditError::MoveOffEnd);
        assert_eq!(s.apply(Edit::Move { row: 2, delta: 1 }).unwrap_err(), EditError::MoveOffEnd, "the last root");
        // A delta of 2 is two hops.
        s.apply(Edit::Move { row: 0, delta: 2 }).unwrap();
        assert_eq!(ids(&s), vec![5, 2, 3, 4, 1]);
        assert_eq!(s.apply(Edit::Move { row: 9, delta: 1 }).unwrap_err(), EditError::NoSuchRow(9));
        assert_eq!(s.stale_lines().count(), 5, "still stale from insertion: a move changes no request");
        assert!((0..5).all(|r| s.revision(r) == 1));
    }

    #[test]
    fn group_makes_a_custom_package_of_a_contiguous_run_of_root_lines() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(1.0, OptionKind::Call), 1), line(spx(2.0, OptionKind::Put), -1), line(spx(3.0, OptionKind::Call), 1)]);
        for r in 0..3 {
            s.deliver(s.id(r), 1, Ok(result(10.0)), at(0));
        }
        let undo = s.apply(Edit::Group { first: 0, count: 2, template: Template::Custom, id: None }).unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(s.kind(0), RowKind::Package { template: Template::Custom });
        assert_eq!(s.id(0), LineId(4), "a fresh id");
        assert_eq!(s.children(0), 1..3);
        assert_eq!(ids(&s), vec![4, 1, 2, 3]);
        assert_eq!(s.result(0).unwrap().price, 10.0 - 10.0);
        assert_eq!(s.state(0), &LineState::Fresh, "grouping re-requests nothing");
        assert_eq!(s.stale_lines().count(), 0);
        assert_eq!(undo.inverse, vec![Edit::Ungroup { row: 0 }]);
        let redo = s.undo(&undo).unwrap();
        assert_eq!(ids(&s), vec![1, 2, 3]);
        assert!(s.roots().eq(0..3));
        assert_eq!(redo.inverse, vec![Edit::Group { first: 0, count: 2, template: Template::Custom, id: Some(LineId(4)) }]);
        s.undo(&redo).unwrap();
        assert_eq!(ids(&s), vec![4, 1, 2, 3], "redo restores the same package id");
    }

    #[test]
    fn group_refuses_a_run_that_is_not_contiguous_roots() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(1.0, OptionKind::Call), 1), callspread(1), line(spx(2.0, OptionKind::Call), 1)]);
        // Over a package.
        assert_eq!(
            s.apply(Edit::Group { first: 0, count: 2, template: Template::Custom, id: None }).unwrap_err(),
            EditError::NotContiguousRoots
        );
        // Starting on a leg.
        assert_eq!(
            s.apply(Edit::Group { first: 2, count: 1, template: Template::Custom, id: None }).unwrap_err(),
            EditError::NotContiguousRoots
        );
        // Past the end.
        assert_eq!(
            s.apply(Edit::Group { first: 4, count: 2, template: Template::Custom, id: None }).unwrap_err(),
            EditError::NotContiguousRoots
        );
        assert_eq!(
            s.apply(Edit::Group { first: 0, count: 0, template: Template::Custom, id: None }).unwrap_err(),
            EditError::EmptyInsert
        );
        assert_eq!(
            s.apply(Edit::Group { first: 9, count: 1, template: Template::Custom, id: None }).unwrap_err(),
            EditError::NoSuchRow(9)
        );
        // An id in use is refused.
        assert_eq!(
            s.apply(Edit::Group { first: 0, count: 1, template: Template::Custom, id: Some(LineId(1)) }).unwrap_err(),
            EditError::IdInUse(LineId(1))
        );
        assert_eq!(s.len(), 5, "nothing changed");
        // A single root is a valid group.
        s.apply(Edit::Group { first: 4, count: 1, template: Template::Custom, id: None }).unwrap();
        assert_eq!(s.children(4), 5..6);
    }

    #[test]
    fn ungroup_promotes_the_legs_in_place_and_refuses_a_line() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(1.0, OptionKind::Call), 1), callspread(2), line(spx(2.0, OptionKind::Call), 1)]);
        let undo = s.apply(Edit::Ungroup { row: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 3, 4, 5]);
        assert!(s.roots().eq(0..4));
        assert_eq!(undo.inverse, vec![Edit::Group { first: 1, count: 2, template: Template::CS, id: Some(LineId(2)) }]);
        assert_eq!(s.apply(Edit::Ungroup { row: 0 }).unwrap_err(), EditError::NotAPackage(0));
        assert_eq!(s.apply(Edit::Ungroup { row: 9 }).unwrap_err(), EditError::NoSuchRow(9));
        s.undo(&undo).unwrap();
        assert_eq!(ids(&s), vec![1, 2, 3, 4, 5]);
        assert_eq!(s.kind(1), RowKind::Package { template: Template::CS }, "the template survives the round trip");
        // An empty package ungroups to nothing.
        s.apply(Edit::Remove { at: 2 }).unwrap();
        s.apply(Edit::Remove { at: 2 }).unwrap();
        s.apply(Edit::Ungroup { row: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5]);
    }

    /// Spec §12: `apply` then its `Undo` is identity for every `Edit`.
    #[test]
    fn apply_then_undo_is_identity_for_every_edit() {
        fn fixture() -> Sheet {
            let mut s = Sheet::new("t");
            push(&mut s, vec![line(spx(1.0, OptionKind::Call), 1), callspread(2), line(spx(2.0, OptionKind::Put), -1), line(spx(3.0, OptionKind::Call), 1)]);
            for r in [0, 2, 3, 4, 5] {
                s.deliver(s.id(r), 1, Ok(result(r as f64)), at(r as i64));
            }
            s.apply(Edit::SetSpotOverride { underlying: "SPX".into(), level: Some(5000.0) }).unwrap();
            for r in [0, 2, 3, 4, 5] {
                s.deliver(s.id(r), 2, Ok(result(r as f64)), at(10 + r as i64));
            }
            s
        }
        fn snapshot(s: &Sheet) -> (Vec<RowRecord>, OwnShifts, Vec<(String, f64)>) {
            (
                (0..s.len()).map(|r| s.record(r)).collect(),
                s.sheet_shift,
                s.overrides.spot.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            )
        }
        let edits = vec![
            Edit::Insert { place: Place::Root { at: 1 }, rows: vec![line(spx(9.0, OptionKind::Call), 1), callspread(1)] },
            Edit::Insert { place: Place::Leg { package: 1, leg: 0 }, rows: vec![line(spx(9.0, OptionKind::Call), 1)] },
            Edit::Remove { at: 1 },
            Edit::Remove { at: 2 },
            Edit::SetInstrument { row: 0, instrument: spx(7.0, OptionKind::Put) },
            Edit::SetQty { row: 2, qty: 5 },
            Edit::SetShift { row: 3, shift: OwnShifts { spot_pct: Some(1.0), vol_pts: Some(2.0) } },
            Edit::Move { row: 0, delta: 1 },
            Edit::Move { row: 2, delta: 1 },
            Edit::Group { first: 4, count: 2, template: Template::Custom, id: None },
            Edit::Ungroup { row: 1 },
            Edit::SetSheetShift(OwnShifts { spot_pct: Some(3.0), vol_pts: None }),
            Edit::SetSpotOverride { underlying: "SPX".into(), level: Some(5200.0) },
            Edit::SetSpotOverride { underlying: "SPX".into(), level: None },
            Edit::SetSpotOverride { underlying: "NDX".into(), level: Some(1.0) },
        ];
        for edit in edits {
            let mut s = fixture();
            let before = snapshot(&s);
            let label = format!("{edit:?}");
            let undo = s.apply(edit).unwrap_or_else(|e| panic!("{label}: {e}"));
            let redo = s.undo(&undo).unwrap_or_else(|e| panic!("undo of {label}: {e}"));
            assert!(!redo.inverse.is_empty(), "{label}: an undo always has a redo");
            let after = snapshot(&s);
            // Revisions may have moved (a request change and its reversal
            // are two bumps) and states may be Stale; ids, kinds, parents,
            // instruments, quantities, shifts, results and priced_at are identical.
            assert_eq!(after.0.len(), before.0.len(), "{label}");
            for (a, b) in after.0.iter().zip(&before.0) {
                assert_eq!((a.id, a.kind, a.parent, &a.instrument, a.qty, a.shift, a.result, a.priced_at),
                           (b.id, b.kind, b.parent, &b.instrument, b.qty, b.shift, b.result, b.priced_at), "{label}");
            }
            assert_eq!(after.1, before.1, "{label}");
            assert_eq!(after.2, before.2, "{label}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer edit:: 2>&1 | grep -E 'panicked|error\[|^test result' | head`
Expected: `undo` not found; the three `todo!("Task 6")` arms panic.

- [ ] **Step 3: Implement**

Replace the three `todo!("Task 6")` arms:

```rust
            Edit::Move { row, delta } => self.move_row(row, delta),
            Edit::Group { first, count, template, id } => self.group(first, count, template, id),
            Edit::Ungroup { row } => self.ungroup(row),
```

Add to the `impl Sheet` block in `edit.rs`:

```rust
    /// Apply `undo`'s edits in order; the redo is their inverses in
    /// reverse (the last one applied is the first to take back).
    pub fn undo(&mut self, undo: &Undo) -> Result<Undo, EditError> {
        let mut inverses = Vec::with_capacity(undo.inverse.len());
        for edit in &undo.inverse {
            inverses.extend(self.apply(edit.clone())?.inverse);
        }
        inverses.reverse();
        Ok(Undo { inverse: inverses })
    }

    /// The flat block a row occupies: itself plus its legs.
    fn block(&self, row: usize) -> std::ops::Range<usize> {
        row..self.children(row).end
    }

    /// The siblings of `row`, in order: the roots, or the legs of its package.
    fn siblings(&self, row: usize) -> Vec<usize> {
        match self.parent(row) {
            None => self.roots().collect(),
            Some(p) => self.children(p).collect(),
        }
    }

    fn move_row(&mut self, row: usize, delta: isize) -> Result<Undo, EditError> {
        self.row_exists(row)?;
        let siblings = self.siblings(row);
        let pos = siblings.iter().position(|s| *s == row).expect("a row is among its siblings");
        let target = pos as isize + delta;
        if target < 0 || target as usize >= siblings.len() {
            return Err(EditError::MoveOffEnd);
        }
        let id = self.id(row);
        let mut current = row;
        for _ in 0..delta.unsigned_abs() {
            let sibs = self.siblings(current);
            let p = sibs.iter().position(|s| *s == current).expect("still a sibling");
            if delta > 0 {
                let next = sibs[p + 1];
                let (a, b) = (self.block(current), self.block(next));
                self.swap_adjacent_blocks(a.clone(), b.clone());
                current = a.start + b.len();
            } else {
                let prev = sibs[p - 1];
                let (a, b) = (self.block(prev), self.block(current));
                self.swap_adjacent_blocks(a.clone(), b);
                current = a.start;
            }
            self.reindex_parents();
        }
        debug_assert_eq!(self.id(current), id);
        Ok(Undo {
            inverse: vec![Edit::Move { row: current, delta: -delta }],
        })
    }

    fn group(&mut self, first: usize, count: usize, template: Template, id: Option<LineId>) -> Result<Undo, EditError> {
        self.row_exists(first)?;
        if count == 0 {
            return Err(EditError::EmptyInsert);
        }
        if let Some(id) = id
            && self.has_id(id)
        {
            return Err(EditError::IdInUse(id));
        }
        // `first` and the next `count − 1` rows must each be a root line:
        // a root line has no legs, so consecutive root lines are
        // consecutive rows.
        let end = first + count;
        if end > self.len() {
            return Err(EditError::NotContiguousRoots);
        }
        if (first..end).any(|r| self.depth(r) != 0 || !self.is_line(r)) {
            return Err(EditError::NotContiguousRoots);
        }
        let pkg = self.new_package_record(template, id);
        self.splice_in(first, pkg, None);
        for r in first + 1..=end {
            self.set_leg_marker(r, true);
        }
        self.reindex_parents();
        Ok(Undo {
            inverse: vec![Edit::Ungroup { row: first }],
        })
    }

    fn ungroup(&mut self, row: usize) -> Result<Undo, EditError> {
        self.row_exists(row)?;
        let RowKind::Package { template } = self.kind(row) else {
            return Err(EditError::NotAPackage(row));
        };
        let legs = self.children(row);
        let count = legs.len();
        for r in legs {
            self.set_leg_marker(r, false);
        }
        let pkg = self.take_out(row);
        self.reindex_parents();
        Ok(Undo {
            inverse: vec![Edit::Group {
                first: row,
                count,
                template,
                id: Some(pkg.id),
            }],
        })
    }
```

`siblings` allocates a `Vec` per call; a move is a keystroke, not a frame, so this is acceptable (the bench in Task 10 pins `apply`'s cost at 1,000 lines through `SetSheetShift`, the widest-touching edit).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS, including `apply_then_undo_is_identity_for_every_edit`.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-pricer
git commit -m "pricer: move, group, ungroup and undo — every Edit's inverse is identity"
```

---

### Task 7: The column vocabulary and per-cell text

**Files:**
- Create: `crates/geode-pricer/src/core/columns.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs`
- Test: `crates/geode-pricer/src/core/columns.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Sheet` (Task 4); `render_expiry`, `render_strike`, `render_barrier_kind` (Task 3); `geode_core::view::{ColumnFormat, Colour, Negative, Scale}`, `geode_core::format::format_number`.
- Produces (in `geode_pricer::core::columns`):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnKind { Qty, Underlying, Expiry, Strike, Type, Barrier, BarrierType, SpotShift, VolShift, Price, Delta, Gamma, Vega, Theta, Rho, PricedAt, Status }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies { EveryLine, BarrierLines, EveryRow }
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef { pub name: &'static str, pub kind: ColumnKind, pub editable: bool, pub applies_to: Applies, pub default_format: ColumnFormat, pub default_width: f32 }
pub static COLUMNS: [ColumnDef; 17];                      // spec §6.5's table, in its order; a `static`, not a `const` — `column()` hands out `&'static` into it, and a const holding a `Colour` (a `String` variant) is not promotable
pub fn column(name: &str) -> Option<&'static ColumnDef>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState { Blank, Own, Inherited, Stale, Failed }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellText { pub text: String, pub state: CellState }
pub fn cell_text(sheet: &Sheet, row: usize, def: &ColumnDef, format: &ColumnFormat) -> CellText;
```

The rules `cell_text` implements (spec §6.5, §8.2): a column that does not apply to the row is `Blank` with empty text; on a package the instrument and shift columns are `Blank`; a result column on a `Failed` row is `—` in `Failed`, on a row with no result `Blank`, else the formatted number, `Stale` when the row is `Stale` and `Own` otherwise; a shift column with an own value paints it `Own`, with only the sheet's value paints that `Inherited`, with neither is `Blank`; `priced_at` is the trader's local clock `HH:MM:SS` (`Local`, the Phase 4a ruling); `status` is empty for `Fresh`, `pricing…` for `Stale`, the message in `Failed` for `Failed`. Numbers go through `format_number(value, format).text`; a shift prints with a sign (`+2`, `-1`) at the format's precision.

- [ ] **Step 1: Write the failing tests**

Create `columns.rs` with only this tests block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::sheet::{OwnShifts, Sheet};
    use geode_core::pricing::OptionKind;

    fn cell(sheet: &Sheet, row: usize, name: &str) -> CellText {
        let def = column(name).unwrap_or_else(|| panic!("no column {name}"));
        cell_text(sheet, row, def, &def.default_format)
    }

    #[test]
    fn the_vocabulary_is_the_specs_table_in_order() {
        let names: Vec<&str> = COLUMNS.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec![
                "qty", "underlying", "expiry", "strike", "type", "barrier", "barrier_type", "spot_shift", "vol_shift",
                "price", "delta", "gamma", "vega", "theta", "rho", "priced_at", "status",
            ]
        );
        for c in &COLUMNS {
            assert_eq!(column(c.name), Some(c), "{}", c.name);
            assert!(c.default_width > 0.0, "{}", c.name);
        }
        assert_eq!(column("npv"), None);
        let editable: Vec<&str> = COLUMNS.iter().filter(|c| c.editable).map(|c| c.name).collect();
        assert_eq!(editable, vec!["qty", "underlying", "expiry", "strike", "type", "barrier", "barrier_type", "spot_shift", "vol_shift"]);
        assert_eq!(column("barrier").unwrap().applies_to, Applies::BarrierLines);
        assert_eq!(column("barrier_type").unwrap().applies_to, Applies::BarrierLines);
        assert_eq!(column("price").unwrap().applies_to, Applies::EveryRow);
        assert_eq!(column("status").unwrap().applies_to, Applies::EveryRow);
        assert_eq!(column("qty").unwrap().applies_to, Applies::EveryLine);
        assert_eq!(column("spot_shift").unwrap().applies_to, Applies::EveryLine);
    }

    #[test]
    fn instrument_cells_render_the_grammar_and_a_package_paints_them_blank() {
        let mut s = Sheet::new("t");
        let barrier = crate::core::shorthand::parse("-3 SPX 20DEC26 5000 P DO 4200").unwrap();
        push(&mut s, vec![line(spx(4250.5, OptionKind::Call), 2), barrier, callspread(1)]);
        assert_eq!(cell(&s, 0, "qty"), CellText { text: "2".into(), state: CellState::Own });
        assert_eq!(cell(&s, 0, "underlying").text, "SPX");
        assert_eq!(cell(&s, 0, "expiry").text, "Z26");
        assert_eq!(cell(&s, 0, "strike").text, "4250.5");
        assert_eq!(cell(&s, 0, "type").text, "C");
        assert_eq!(cell(&s, 0, "barrier"), CellText { text: String::new(), state: CellState::Blank }, "not a barrier line");
        assert_eq!(cell(&s, 0, "barrier_type").state, CellState::Blank);
        assert_eq!(cell(&s, 1, "qty").text, "-3");
        assert_eq!(cell(&s, 1, "expiry").text, "20DEC26");
        assert_eq!(cell(&s, 1, "type").text, "P");
        assert_eq!(cell(&s, 1, "barrier"), CellText { text: "4200".into(), state: CellState::Own });
        assert_eq!(cell(&s, 1, "barrier_type"), CellText { text: "DO".into(), state: CellState::Own });
        // The package row.
        for name in ["qty", "underlying", "expiry", "strike", "type", "barrier", "barrier_type", "spot_shift", "vol_shift"] {
            assert_eq!(cell(&s, 2, name), CellText { text: String::new(), state: CellState::Blank }, "{name}");
        }
        // Its legs are lines.
        assert_eq!(cell(&s, 3, "strike").text, "4800");
        assert_eq!(cell(&s, 4, "qty").text, "-1");
    }

    #[test]
    fn a_shift_cell_is_own_inherited_or_blank() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), line(spx(5100.0, OptionKind::Call), 1)]);
        assert_eq!(cell(&s, 0, "spot_shift"), CellText { text: String::new(), state: CellState::Blank });
        s.apply(Edit::SetSheetShift(OwnShifts { spot_pct: Some(2.0), vol_pts: Some(-1.5) })).unwrap();
        assert_eq!(cell(&s, 0, "spot_shift"), CellText { text: "+2.0".into(), state: CellState::Inherited });
        assert_eq!(cell(&s, 0, "vol_shift"), CellText { text: "-1.5".into(), state: CellState::Inherited });
        s.apply(Edit::SetShift { row: 1, shift: OwnShifts { spot_pct: Some(-5.0), vol_pts: None } }).unwrap();
        assert_eq!(cell(&s, 1, "spot_shift"), CellText { text: "-5.0".into(), state: CellState::Own });
        assert_eq!(cell(&s, 1, "vol_shift"), CellText { text: "-1.5".into(), state: CellState::Inherited });
        // Zero is still a value, own or inherited.
        s.apply(Edit::SetShift { row: 1, shift: OwnShifts { spot_pct: Some(0.0), vol_pts: None } }).unwrap();
        assert_eq!(cell(&s, 1, "spot_shift"), CellText { text: "+0.0".into(), state: CellState::Own });
    }

    #[test]
    fn result_cells_follow_the_rows_state() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), callspread(-2)]);
        // Unpriced: blank, and status says pricing.
        assert_eq!(cell(&s, 0, "price"), CellText { text: String::new(), state: CellState::Blank });
        assert_eq!(cell(&s, 0, "status"), CellText { text: "pricing…".into(), state: CellState::Stale });
        assert_eq!(cell(&s, 0, "priced_at").state, CellState::Blank);
        s.deliver(s.id(0), 1, Ok(result(1234.5678)), at(0));
        assert_eq!(cell(&s, 0, "price"), CellText { text: "1,234.57".into(), state: CellState::Own });
        assert_eq!(cell(&s, 0, "delta"), CellText { text: "123.4568".into(), state: CellState::Own });
        assert_eq!(cell(&s, 0, "status"), CellText { text: String::new(), state: CellState::Own });
        assert_eq!(cell(&s, 0, "priced_at").state, CellState::Own);
        assert_eq!(cell(&s, 0, "priced_at").text.len(), 8, "HH:MM:SS");
        // Stale after an edit: the old number, muted.
        s.apply(Edit::SetInstrument { row: 0, instrument: spx(5050.0, OptionKind::Call) }).unwrap();
        assert_eq!(cell(&s, 0, "price"), CellText { text: "1,234.57".into(), state: CellState::Stale });
        assert_eq!(cell(&s, 0, "status").state, CellState::Stale);
        // Failed: a dash, and the message.
        s.deliver(s.id(0), 2, Err("refused by the mock".into()), at(1));
        assert_eq!(cell(&s, 0, "price"), CellText { text: "—".into(), state: CellState::Failed });
        assert_eq!(cell(&s, 0, "rho"), CellText { text: "—".into(), state: CellState::Failed });
        assert_eq!(cell(&s, 0, "status"), CellText { text: "refused by the mock".into(), state: CellState::Failed });
        // A package paints its sums like a line.
        s.deliver(s.id(2), 1, Ok(result(100.0)), at(2));
        s.deliver(s.id(3), 1, Ok(result(40.0)), at(3));
        assert_eq!(cell(&s, 1, "price"), CellText { text: "-120.00".into(), state: CellState::Own });
        assert_eq!(cell(&s, 1, "status").text, "");
        // A custom format from a view applies.
        let def = column("price").unwrap();
        let precise = geode_core::view::ColumnFormat { precision: 4, ..def.default_format };
        assert_eq!(cell_text(&s, 1, def, &precise).text, "-120.0000");
    }
}
```

`-2 × 100 + 2 × 40 = -120`: the callspread's legs are `-2 × 4800 C` and `+2 × 5200 C`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer columns 2>&1 | grep -E 'error\[|^test result' | head`
Expected: compile errors — `COLUMNS`, `cell_text` not found.

- [ ] **Step 3: Implement**

```rust
//! The fixed column vocabulary (line-pricer spec §6.5) and the pure
//! text of one cell — the half of §8.2's grid model that needs no theme,
//! so it is tested here without one (planning decision 10). Part 3 wraps
//! each `CellText` in a `SharedString` and picks the paint from its
//! `CellState`.

use crate::core::sheet::{LineState, Sheet};
use crate::core::shorthand::{render_barrier_kind, render_expiry, render_strike};
use chrono::Local;
use geode_core::format::format_number;
use geode_core::pricing::{Instrument, OptionKind};
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    Qty,
    Underlying,
    Expiry,
    Strike,
    Type,
    Barrier,
    BarrierType,
    SpotShift,
    VolShift,
    Price,
    Delta,
    Gamma,
    Vega,
    Theta,
    Rho,
    PricedAt,
    Status,
}

/// Which rows a column has a value for (spec §6.5's "Applies to").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    EveryLine,
    BarrierLines,
    EveryRow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: &'static str,
    pub kind: ColumnKind,
    pub editable: bool,
    pub applies_to: Applies,
    pub default_format: ColumnFormat,
    /// Pixels — the `view_presentation.toml` contract the blotter's
    /// widths follow; not on the rem scale (the known gap CLAUDE.md
    /// records for `TableDelegate::column`).
    pub default_width: f32,
}

/// Text columns: no grouping, no sign colour.
const TEXT: ColumnFormat = ColumnFormat::TEXT;
/// A strike or barrier level: two places, no thousands separator (a
/// strike reads as `5000`, not `5,000`).
const LEVEL: ColumnFormat = ColumnFormat {
    precision: 2,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};
/// A shift: one place, signed by `cell_text` itself.
const SHIFT: ColumnFormat = ColumnFormat {
    precision: 1,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};
/// A price: the measure default (two places, grouped, sign-coloured).
const PRICE: ColumnFormat = ColumnFormat::MEASURE;
/// A greek: four places, grouped, sign-coloured.
const GREEK: ColumnFormat = ColumnFormat {
    precision: 4,
    thousands: true,
    negative: Negative::Minus,
    colour: Colour::Sign,
    scale: Scale::None,
};

const fn def(
    name: &'static str,
    kind: ColumnKind,
    editable: bool,
    applies_to: Applies,
    default_format: ColumnFormat,
    default_width: f32,
) -> ColumnDef {
    ColumnDef {
        name,
        kind,
        editable,
        applies_to,
        default_format,
        default_width,
    }
}

use Applies::{BarrierLines, EveryLine, EveryRow};

/// Spec §6.5's table, in its order. A `static`, not a `const`:
/// `column()` answers `&'static ColumnDef` into it, and a `const` whose
/// type carries a `String` (`Colour::Named`) is not promotable to a
/// `'static` borrow.
pub static COLUMNS: [ColumnDef; 17] = [
    def("qty", ColumnKind::Qty, true, EveryLine, TEXT, 48.0),
    def("underlying", ColumnKind::Underlying, true, EveryLine, TEXT, 80.0),
    def("expiry", ColumnKind::Expiry, true, EveryLine, TEXT, 72.0),
    def("strike", ColumnKind::Strike, true, EveryLine, LEVEL, 88.0),
    def("type", ColumnKind::Type, true, EveryLine, TEXT, 48.0),
    def("barrier", ColumnKind::Barrier, true, BarrierLines, LEVEL, 80.0),
    def("barrier_type", ColumnKind::BarrierType, true, BarrierLines, TEXT, 64.0),
    def("spot_shift", ColumnKind::SpotShift, true, EveryLine, SHIFT, 72.0),
    def("vol_shift", ColumnKind::VolShift, true, EveryLine, SHIFT, 72.0),
    def("price", ColumnKind::Price, false, EveryRow, PRICE, 96.0),
    def("delta", ColumnKind::Delta, false, EveryRow, GREEK, 96.0),
    def("gamma", ColumnKind::Gamma, false, EveryRow, GREEK, 96.0),
    def("vega", ColumnKind::Vega, false, EveryRow, GREEK, 96.0),
    def("theta", ColumnKind::Theta, false, EveryRow, GREEK, 96.0),
    def("rho", ColumnKind::Rho, false, EveryRow, GREEK, 96.0),
    def("priced_at", ColumnKind::PricedAt, false, EveryRow, TEXT, 80.0),
    def("status", ColumnKind::Status, false, EveryRow, TEXT, 160.0),
];

pub fn column(name: &str) -> Option<&'static ColumnDef> {
    COLUMNS.iter().find(|c| c.name == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    /// No value here: the column does not apply, or nothing has arrived.
    Blank,
    /// A value of the row's own.
    Own,
    /// A shift inherited from the sheet (paints muted, spec ruling 8).
    Inherited,
    /// A result the row is repricing (paints muted, spec §8.2).
    Stale,
    /// A failed row's result cells and status (paints danger text).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellText {
    pub text: String,
    pub state: CellState,
}

fn blank() -> CellText {
    CellText {
        text: String::new(),
        state: CellState::Blank,
    }
}

fn own(text: impl Into<String>) -> CellText {
    CellText {
        text: text.into(),
        state: CellState::Own,
    }
}

/// `+2.0` / `-1.5` at the format's precision: a shift is a delta from
/// the market, so its sign is the information.
fn signed(value: f64, format: &ColumnFormat) -> String {
    let n = format_number(value.abs(), format).text;
    if value < 0.0 { format!("-{n}") } else { format!("+{n}") }
}

fn number(sheet: &Sheet, row: usize, pick: fn(&geode_core::pricing::PriceResult) -> f64, format: &ColumnFormat) -> CellText {
    match sheet.state(row) {
        LineState::Failed(_) => CellText {
            text: "—".into(),
            state: CellState::Failed,
        },
        state => match sheet.result(row) {
            None => blank(),
            Some(r) => CellText {
                text: format_number(pick(r), format).text,
                state: if *state == LineState::Stale { CellState::Stale } else { CellState::Own },
            },
        },
    }
}

/// The text and state of one cell (spec §6.5, §8.2).
pub fn cell_text(sheet: &Sheet, row: usize, def: &ColumnDef, format: &ColumnFormat) -> CellText {
    let instrument: Option<&Instrument> = sheet.instrument(row);
    let applies = match def.applies_to {
        Applies::EveryRow => true,
        Applies::EveryLine => instrument.is_some(),
        Applies::BarrierLines => matches!(instrument, Some(Instrument::Barrier(_))),
    };
    if !applies {
        return blank();
    }
    match def.kind {
        ColumnKind::Qty => own(sheet.qty(row).to_string()),
        ColumnKind::Underlying => own(instrument.expect("applies").underlying()),
        ColumnKind::Expiry => own(render_expiry(instrument.expect("applies").expiry())),
        ColumnKind::Strike => own(render_strike(instrument.expect("applies").strike())),
        ColumnKind::Type => own(match instrument.expect("applies").kind() {
            OptionKind::Call => "C",
            OptionKind::Put => "P",
        }),
        ColumnKind::Barrier => match instrument {
            Some(Instrument::Barrier(b)) => own(render_strike(geode_core::pricing::Strike::Absolute(b.level))),
            _ => blank(),
        },
        ColumnKind::BarrierType => match instrument {
            Some(Instrument::Barrier(b)) => own(render_barrier_kind(b.barrier)),
            _ => blank(),
        },
        ColumnKind::SpotShift => shift_cell(sheet.shift(row).spot_pct, sheet.sheet_shift.spot_pct, format),
        ColumnKind::VolShift => shift_cell(sheet.shift(row).vol_pts, sheet.sheet_shift.vol_pts, format),
        ColumnKind::Price => number(sheet, row, |r| r.price, format),
        ColumnKind::Delta => number(sheet, row, |r| r.delta, format),
        ColumnKind::Gamma => number(sheet, row, |r| r.gamma, format),
        ColumnKind::Vega => number(sheet, row, |r| r.vega, format),
        ColumnKind::Theta => number(sheet, row, |r| r.theta, format),
        ColumnKind::Rho => number(sheet, row, |r| r.rho, format),
        ColumnKind::PricedAt => match sheet.priced_at(row) {
            // The trader's local clock, like every displayed time (Phase 4a ruling).
            Some(t) => own(t.with_timezone(&Local).format("%H:%M:%S").to_string()),
            None => blank(),
        },
        ColumnKind::Status => match sheet.state(row) {
            LineState::Fresh => own(""),
            LineState::Stale => CellText {
                text: "pricing…".into(),
                state: CellState::Stale,
            },
            LineState::Failed(m) => CellText {
                text: m.clone(),
                state: CellState::Failed,
            },
        },
    }
}

fn shift_cell(own_value: Option<f64>, sheet_value: Option<f64>, format: &ColumnFormat) -> CellText {
    match (own_value, sheet_value) {
        (Some(v), _) => own(signed(v, format)),
        (None, Some(v)) => CellText {
            text: signed(v, format),
            state: CellState::Inherited,
        },
        (None, None) => blank(),
    }
}
```

`format_number(4200.0, &LEVEL)` prints `4200.00`, but the test expects the barrier cell to read `4200` — the plan uses `render_strike(Strike::Absolute(level))` for the barrier and `render_strike` for the strike, so both print the grammar's spelling, and `LEVEL` is the format a view may override them with in Part 3's editor. Keep `LEVEL` on the two defs (it is what `ColumnPlan` resolves), and keep the cells on `render_strike`.

Add to `mod.rs`: `pub mod columns;` and `pub use columns::{Applies, COLUMNS, CellState, CellText, ColumnDef, ColumnKind, cell_text, column};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS. If `1,234.57`/`123.4568` disagree with `format_number`'s rounding of `1234.5678` and `123.45678`, the expected strings are wrong, not the code: `1234.5678` at two places is `1,234.57` and `123.45678` at four is `123.4568`.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-pricer
git commit -m "pricer: the column vocabulary and per-cell text with its state"
```

---

### Task 8: The `pricer_views` doc, the two bundled views and `ColumnPlan`

**Files:**
- Create: `crates/geode-pricer/src/core/views.rs`
- Modify: `crates/geode-core/src/config/merge.rs:24-34` (`atomic_depth`), `crates/geode-pricer/src/core/mod.rs`
- Test: `crates/geode-pricer/src/core/views.rs` (`mod tests`), `crates/geode-core/src/config/merge.rs` (existing tests module)

**Interfaces:**
- Consumes: `COLUMNS`, `column`, `ColumnDef` (Task 7); `geode_core::config::{Diagnostic, MergedDoc, Severity, LayerDoc, merge_docs}`; `geode_core::view::{ColumnFormat, ColumnPresentation}`.
- Produces (in `geode_pricer::core::views`):
```rust
pub const PRICER_VIEWS_DOC: &str = "pricer_views";
pub const BUILTIN_VIEWS: &str;                             // the two bundled views as TOML (below)
#[derive(Debug, Clone, PartialEq)]
pub struct ViewColumn { pub def: &'static ColumnDef, pub presentation: ColumnPresentation }
#[derive(Debug, Clone, PartialEq)]
pub struct PricerView { pub name: String, pub columns: Vec<ViewColumn> }
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Views { views: Vec<PricerView> }               // doc order
impl Views {
    pub fn from_doc(doc: &MergedDoc) -> (Views, Vec<Diagnostic>);
    pub fn builtin() -> Views;                             // BUILTIN_VIEWS parsed; panics if the constant is malformed (a test pins it)
    pub fn get(&self, name: &str) -> Option<&PricerView>;
    pub fn names(&self) -> impl Iterator<Item = &str>;
    pub fn is_empty(&self) -> bool;
}
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedColumn { pub def: &'static ColumnDef, pub label: String, pub width: f32, pub format: ColumnFormat }
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnPlan { pub columns: Vec<PlannedColumn> }  // view order; the tree column is the delegate's own, not here
impl ColumnPlan { pub fn build(view: &PricerView) -> ColumnPlan; }
```

The doc (spec §6.5): one table per view; `columns` is an array whose elements are each either a string (a column name) or a table (`name` plus the blotter's presentation keys: `label`, `width`, and a `format` sub-table read by `ColumnPresentation::parse_format_keys`). An unknown column name is an error diagnostic and the column is dropped; a view with no valid column is dropped with an error; a repeated column is a warning and the repeat is dropped; `config_version` is skipped. Paths: `pricer_views.<view>`, `pricer_views.<view>.columns.<i>`, `pricer_views.<view>.columns.<i>.name`, `pricer_views.<view>.columns.<i>.<key>`, `pricer_views.<view>.columns.<i>.format.<key>`.

```toml
# BUILTIN_VIEWS
[vanilla]
columns = ["qty", "underlying", "expiry", "strike", "type", "spot_shift", "vol_shift",
           "price", "delta", "gamma", "vega", "theta", "rho"]

[barrier]
columns = ["qty", "underlying", "expiry", "strike", "type", "barrier", "barrier_type", "spot_shift", "vol_shift",
           "price", "delta", "gamma", "vega", "theta", "rho"]
```

- [ ] **Step 1: Write the failing tests**

Create `views.rs` with only this tests block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, Severity, merge_docs};

    fn doc(text: &str) -> MergedDoc {
        merge_docs(
            PRICER_VIEWS_DOC,
            &[LayerDoc::builtin(PRICER_VIEWS_DOC, text).expect("well-formed test TOML")],
        )
    }

    fn names(v: &PricerView) -> Vec<&str> {
        v.columns.iter().map(|c| c.def.name).collect()
    }

    #[test]
    fn the_two_bundled_views_load_clean() {
        let (views, diags) = Views::from_doc(&doc(BUILTIN_VIEWS));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(views.names().collect::<Vec<_>>(), vec!["vanilla", "barrier"]);
        let vanilla = views.get("vanilla").unwrap();
        assert_eq!(
            names(vanilla),
            vec!["qty", "underlying", "expiry", "strike", "type", "spot_shift", "vol_shift", "price", "delta", "gamma", "vega", "theta", "rho"]
        );
        let barrier = views.get("barrier").unwrap();
        assert_eq!(
            names(barrier),
            vec!["qty", "underlying", "expiry", "strike", "type", "barrier", "barrier_type", "spot_shift", "vol_shift", "price", "delta", "gamma", "vega", "theta", "rho"]
        );
        assert_eq!(Views::builtin(), views);
        assert!(views.get("npv").is_none());
    }

    #[test]
    fn an_unknown_column_is_an_error_and_dropped() {
        let (views, diags) = Views::from_doc(&doc(
            "[v]\ncolumns = [\"qty\", \"npv\", \"price\"]\n",
        ));
        assert_eq!(names(views.get("v").unwrap()), vec!["qty", "price"]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].path.as_deref(), Some("pricer_views.v.columns.1"));
        assert!(diags[0].message.contains("npv"), "{}", diags[0].message);
        assert!(diags[0].message.contains("dropped"), "{}", diags[0].message);
    }

    #[test]
    fn a_view_with_no_valid_column_is_dropped_with_an_error() {
        let (views, diags) = Views::from_doc(&doc(
            "config_version = 1\n[empty]\ncolumns = []\n[bad]\ncolumns = [\"npv\"]\n[ok]\ncolumns = [\"price\"]\n[notatable]\n",
        ));
        assert_eq!(views.names().collect::<Vec<_>>(), vec!["ok"]);
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert!(paths.contains(&"pricer_views.empty"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.bad"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.bad.columns.0"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.notatable"), "{diags:?}");
        assert!(
            diags.iter().filter(|d| d.path.as_deref() == Some("pricer_views.empty")).all(|d| d.severity == Severity::Error)
        );
        // A view whose `columns` is missing or not an array.
        let (views, diags) = Views::from_doc(&doc("[v]\nname = \"x\"\n[w]\ncolumns = \"price\"\n"));
        assert!(views.is_empty());
        assert_eq!(diags.iter().filter(|d| d.severity == Severity::Error).count(), 2, "{diags:?}");
    }

    #[test]
    fn the_table_form_carries_label_width_and_format() {
        let (views, diags) = Views::from_doc(&doc(
            "[v]\ncolumns = [\n  \"qty\",\n  { name = \"price\", label = \"PX\", width = 120, format = { precision = 4, thousands = false } },\n  { name = \"delta\", format = { precision = 99 } },\n  { label = \"no name\" },\n  { name = \"qty\" },\n  { name = \"vega\", width = -1 },\n]\n",
        ));
        let v = views.get("v").unwrap();
        assert_eq!(names(v), vec!["qty", "price", "delta", "vega"], "the nameless and the repeat are dropped");
        let price = &v.columns[1];
        assert_eq!(price.presentation.label.as_deref(), Some("PX"));
        assert_eq!(price.presentation.width, Some(120.0));
        assert_eq!(price.presentation.precision, Some(4));
        assert_eq!(price.presentation.thousands, Some(false));
        assert_eq!(v.columns[2].presentation.precision, None, "99 is out of range and warned");
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert!(paths.contains(&"pricer_views.v.columns.2.format.precision"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.v.columns.3.name"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.v.columns.4"), "the repeat: {diags:?}");
        assert!(paths.contains(&"pricer_views.v.columns.5.width"), "{diags:?}");
        assert!(
            diags.iter().filter(|d| d.path.as_deref() == Some("pricer_views.v.columns.4")).all(|d| d.severity == Severity::Warning)
        );
        // A non-string, non-table element.
        let (views, diags) = Views::from_doc(&doc("[v]\ncolumns = [\"qty\", 3]\n"));
        assert_eq!(names(views.get("v").unwrap()), vec!["qty"]);
        assert_eq!(diags[0].path.as_deref(), Some("pricer_views.v.columns.1"));
    }

    #[test]
    fn a_plan_resolves_label_width_and_format_from_the_defaults_under_the_presentation() {
        let (views, _) = Views::from_doc(&doc(
            "[v]\ncolumns = [\"qty\", { name = \"price\", label = \"PX\", width = 120, format = { precision = 4 } }, \"barrier\"]\n",
        ));
        let plan = ColumnPlan::build(views.get("v").unwrap());
        assert_eq!(plan.columns.len(), 3);
        let qty = &plan.columns[0];
        assert_eq!(qty.def.name, "qty");
        assert_eq!(qty.label, "qty", "the name when no label");
        assert_eq!(qty.width, column("qty").unwrap().default_width);
        assert_eq!(qty.format, column("qty").unwrap().default_format);
        let price = &plan.columns[1];
        assert_eq!(price.label, "PX");
        assert_eq!(price.width, 120.0);
        assert_eq!(price.format.precision, 4);
        assert!(price.format.thousands, "the default fills what the presentation left");
        assert_eq!(plan.columns[2].def.name, "barrier", "a column is planned whether or not any row is a barrier");
        // Both bundled views plan every column they name.
        for name in ["vanilla", "barrier"] {
            let v = Views::builtin();
            let view = v.get(name).unwrap();
            assert_eq!(ColumnPlan::build(view).columns.len(), view.columns.len(), "{name}");
        }
    }
}
```

Append to `crates/geode-core/src/config/merge.rs`'s tests module:

```rust
    #[test]
    fn pricer_views_is_atomic_by_view_name() {
        let desk = LayerDoc::builtin("pricer_views", "[v]\ncolumns = [\"qty\", \"price\"]\n").unwrap();
        let mut user = LayerDoc::builtin("pricer_views", "[v]\ncolumns = [\"delta\"]\n").unwrap();
        user.layer = Layer::User;
        let merged = merge_docs("pricer_views", &[desk, user]);
        let cols = merged.value["v"]["columns"].as_array().unwrap();
        assert_eq!(cols.len(), 1, "the user's view replaced the desk's whole: {cols:?}");
        assert_eq!(merged.provenance.get("v"), Some(&Layer::User));
    }
```

(`merge.rs`'s tests module opens with `use super::*;`; add `use crate::config::{Layer, LayerDoc};` beside it if those two are not already in scope.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer views 2>&1 | grep -E 'error\[|^test result' | head; cargo test -p geode-core pricer_views_is_atomic 2>&1 | grep -E 'panicked|^test result'`
Expected: compile errors in the pricer; the core test FAILS (two columns merged, not one).

- [ ] **Step 3: Implement**

In `crates/geode-core/src/config/merge.rs`, add `"pricer_views"` to the depth-1 list:

```rust
        // pricer_views (line-pricer spec §6.5): one table per view name,
        // like `views` — a desk or user layer overrides a view whole.
        "pricer_views" => Some(1),
```

placed after the `"colours" => Some(1),` arm.

`crates/geode-pricer/src/core/views.rs`:

```rust
//! The `pricer_views` doc (line-pricer spec §6.5): named column sets
//! over the fixed vocabulary in `columns`, with the blotter's
//! presentation keys, resolved through `geode_core::view::
//! {ColumnFormat, ColumnPresentation}` so formatting code is shared.
//! Two bundled views ship as [`BUILTIN_VIEWS`]; desk and user layers
//! override by name (`merge::atomic_depth`).

use crate::core::columns::{ColumnDef, column};
use geode_core::config::{Diagnostic, MergedDoc, Severity};
use geode_core::view::{ColumnFormat, ColumnPresentation};
use std::cell::RefCell;

pub const PRICER_VIEWS_DOC: &str = "pricer_views";

/// The two bundled views (spec §6.5). Part 3 pushes this into the
/// builtin config layer with the factory (planning decision 12).
pub const BUILTIN_VIEWS: &str = r#"[vanilla]
columns = ["qty", "underlying", "expiry", "strike", "type", "spot_shift", "vol_shift",
           "price", "delta", "gamma", "vega", "theta", "rho"]

[barrier]
columns = ["qty", "underlying", "expiry", "strike", "type", "barrier", "barrier_type", "spot_shift", "vol_shift",
           "price", "delta", "gamma", "vega", "theta", "rho"]
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct ViewColumn {
    pub def: &'static ColumnDef,
    pub presentation: ColumnPresentation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PricerView {
    pub name: String,
    pub columns: Vec<ViewColumn>,
}

/// The loaded views, in doc order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Views {
    views: Vec<PricerView>,
}

impl Views {
    /// Every diagnostic carries `path` (`pricer_views.<view>…`); `layer`
    /// and `file` are `None`, as every reader's are.
    pub fn from_doc(doc: &MergedDoc) -> (Views, Vec<Diagnostic>) {
        let mut out = Views::default();
        let mut diags = Vec::new();
        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("{PRICER_VIEWS_DOC}.{name}")
                } else {
                    format!("{PRICER_VIEWS_DOC}.{name}.{suffix}")
                }
            };
            let report = |severity: Severity, suffix: &str, m: String| Diagnostic {
                severity,
                layer: None,
                file: None,
                message: format!("pricer view '{name}': {m}"),
                path: Some(at(suffix)),
            };
            let Some(table) = value.as_table() else {
                diags.push(report(Severity::Error, "", "not a table; dropped".into()));
                continue;
            };
            let Some(cols) = table.get("columns").and_then(|v| v.as_array()) else {
                diags.push(report(Severity::Error, "", "missing 'columns' array; view dropped".into()));
                continue;
            };
            let mut columns: Vec<ViewColumn> = Vec::with_capacity(cols.len());
            // `enumerate()` before any filtering, so an index in a path is
            // the element's real position in the file (4c §19.5).
            for (i, c) in cols.iter().enumerate() {
                let (col_name, table) = match (c.as_str(), c.as_table()) {
                    (Some(s), _) => (s, None),
                    (None, Some(t)) => match t.get("name").and_then(|v| v.as_str()) {
                        Some(s) => (s, Some(t)),
                        None => {
                            diags.push(report(Severity::Error, &format!("columns.{i}.name"), "column table has no 'name'; dropped".into()));
                            continue;
                        }
                    },
                    (None, None) => {
                        diags.push(report(Severity::Error, &format!("columns.{i}"), format!("column must be a name or a table (got {c}); dropped")));
                        continue;
                    }
                };
                let Some(def) = column(col_name) else {
                    diags.push(report(
                        Severity::Error,
                        &format!("columns.{i}"),
                        format!("unknown column '{col_name}'; dropped"),
                    ));
                    continue;
                };
                if columns.iter().any(|existing| existing.def.name == def.name) {
                    diags.push(report(Severity::Warning, &format!("columns.{i}"), format!("column '{col_name}' repeated; the repeat is dropped")));
                    continue;
                }
                let mut presentation = ColumnPresentation::default();
                if let Some(t) = table {
                    // A `RefCell` so `warn` stays a `Fn` for the two
                    // `&dyn Fn` readers, the way `ViewSpec::from_doc` does it.
                    let col_diags = RefCell::new(Vec::new());
                    let warn = |key: &str, m: String| {
                        col_diags.borrow_mut().push(report(
                            Severity::Warning,
                            &format!("columns.{i}.{key}"),
                            format!("column '{col_name}': {m}"),
                        ));
                    };
                    presentation.parse_column_keys(t, false, &warn);
                    if let Some(f) = t.get("format") {
                        match f.as_table() {
                            None => warn("format", "'format' is not a table".into()),
                            Some(f) => presentation.parse_format_keys(f, &|key, m| warn(&format!("format.{key}"), m)),
                        }
                    }
                    diags.extend(col_diags.into_inner());
                }
                columns.push(ViewColumn { def, presentation });
            }
            if columns.is_empty() {
                diags.push(report(Severity::Error, "", "no valid column; view dropped".into()));
                continue;
            }
            out.views.push(PricerView {
                name: name.clone(),
                columns,
            });
        }
        (out, diags)
    }

    /// `BUILTIN_VIEWS` parsed. The constant is authored with the binary;
    /// `the_two_bundled_views_load_clean` pins that it parses with no
    /// diagnostic, so the `expect`s cannot fire in a shipped build.
    pub fn builtin() -> Views {
        let doc = geode_core::config::LayerDoc::builtin(PRICER_VIEWS_DOC, BUILTIN_VIEWS)
            .expect("BUILTIN_VIEWS is well-formed TOML");
        let merged = geode_core::config::merge_docs(PRICER_VIEWS_DOC, &[doc]);
        Views::from_doc(&merged).0
    }

    pub fn get(&self, name: &str) -> Option<&PricerView> {
        self.views.iter().find(|v| v.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.views.iter().map(|v| v.name.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

/// One column as the table paints it: label, width and format resolved
/// from the vocabulary's defaults under the view's presentation.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedColumn {
    pub def: &'static ColumnDef,
    pub label: String,
    pub width: f32,
    pub format: ColumnFormat,
}

/// The table's columns in view order, after the tree column (which is the
/// delegate's own, Part 3). Takes no sheet: the column set never depends
/// on the rows (planning decision 7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnPlan {
    pub columns: Vec<PlannedColumn>,
}

impl ColumnPlan {
    pub fn build(view: &PricerView) -> ColumnPlan {
        ColumnPlan {
            columns: view
                .columns
                .iter()
                .map(|c| PlannedColumn {
                    def: c.def,
                    label: c.presentation.label.clone().unwrap_or_else(|| c.def.name.to_string()),
                    width: c.presentation.width.unwrap_or(c.def.default_width),
                    format: c.def.default_format.clone().with(&c.presentation),
                })
                .collect(),
        }
    }
}
```

`ColumnFormat::with(self, ..)` takes `self` by value and `ColumnFormat` is `Clone`, not `Copy` (`Colour` carries a `String`), so the `.clone()` stays.

Add to `mod.rs`: `pub mod views;` and `pub use views::{BUILTIN_VIEWS, ColumnPlan, PRICER_VIEWS_DOC, PlannedColumn, PricerView, ViewColumn, Views};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer && cargo test -p geode-core config && cargo clippy -p geode-pricer -p geode-core --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-pricer crates/geode-core/src/config/merge.rs
git commit -m "pricer: the pricer_views doc over the column vocabulary, two bundled views, ColumnPlan"
```

---

### Task 9: Storage rows — `to_rows`/`from_rows` and the `pricer_sheets` declaration

**Files:**
- Create: `crates/geode-pricer/src/core/storage.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs`
- Test: `crates/geode-pricer/src/core/storage.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Sheet`, `RowRecord`, `RowKind`, `LineId`, `LineState`, `Refresh`, `OwnShifts` (Task 4); `Template::storage_name`/`parse` (Task 1); `geode_core::document::{DocumentRows, Column, Value}`; `geode_core::schema::SchemaSpec`; `geode_core::source_config::parse_duration`.
- Produces (in `geode_pricer::core::storage`):
```rust
pub const PRICER_SHEETS_DATASET: &str = "pricer_sheets";
/// The datasets-doc TOML declaring the dataset (spec §7.2); Part 4 pushes it into the builtin layer.
pub const PRICER_SHEETS_DECLARATION: &str;
pub const SHEET_KEY: &str = "sheet";  pub const LINE_AXIS: &str = "line";
/// `None` for a sheet with no rows: a zero-row document is refused by the store (spec §7.2).
pub fn to_rows(sheet: &Sheet) -> Option<DocumentRows>;
/// `name` is the key the document was requested under (the sheet's name).
pub fn from_rows(name: &str, rows: &DocumentRows) -> Result<Sheet, String>;
pub fn encode_overrides(overrides: &MarketOverrides) -> String;        // "NDX=20000;SPX=5100" (BTreeMap order), "" when empty
pub fn parse_overrides(text: &str) -> Result<MarketOverrides, String>;
pub fn encode_refresh(refresh: Refresh) -> String;                     // "" | "off" | "30s"
pub fn parse_refresh(text: &str) -> Result<Refresh, String>;
```

The row shape (spec §7.2, one row per line or package row): key `[sheet]`; axis `line` (i64, the `LineId`); values `order` i64, `kind` utf8 (`line` | `package`), `template` utf8 (`custom` | `cs` | … | `""` on a line), `parent` i64 (the parent's id, `-1` on a root), `qty` i64, `underlying` utf8 (`""` on a package), `expiry_kind` utf8 (`date` | `tenor` | `""`), `expiry` utf8 (`2026-12-18` | `3m` | `""`), `strike_kind` utf8 (`abs` | `pct` | `""`), `strike` f64 (`0` on a package), `option_kind` utf8 (`call` | `put` | `""`), `barrier_kind` utf8 (`""` | `ui` | `uo` | `di` | `do`), `barrier` f64 (`0` when none), `spot_shift_own` i64 (`1` = own, `0` = inherit), `spot_shift` f64, `vol_shift_own` i64, `vol_shift` f64; attributes `view` utf8, `sheet_spot_shift_own` i64, `sheet_spot_shift` f64, `sheet_vol_shift_own` i64, `sheet_vol_shift` f64, `refresh` utf8, `spot_overrides` utf8 (planning decision 6). Values have no NULL: every optional is a flag plus value or a kind plus value.

- [ ] **Step 1: Write the failing tests**

Create `storage.rs` with only this tests block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::sheet::{LineState, OwnShifts, Refresh, Sheet};
    use crate::core::shorthand::parse;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::pricing::OptionKind;
    use geode_core::schema::SchemaSpec;
    use std::time::Duration;

    fn dataset() -> geode_core::schema::DatasetSpec {
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", PRICER_SHEETS_DECLARATION).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema.dataset(PRICER_SHEETS_DATASET).expect("declared").clone()
    }

    /// Every template, both variants, every shift state, a sheet-wide
    /// shift, an override, a refresh — the round-trip fixture.
    fn full_sheet() -> Sheet {
        let mut s = Sheet::new("book-1");
        s.view = "barrier".into();
        let mut rows = vec![
            line(spx(5000.0, OptionKind::Call), 2),
            parse("-3 SPX 20DEC26 5000 P DO 4200").unwrap(),
            parse("NDX 3m 95% C UI 110").unwrap(),
        ];
        for text in [
            "-5 SPX Z26 95%/105% CS",
            "SPX Z26 4800/5200 PS",
            "2 SPX Z26 5000 STRD",
            "SPX Z26 4800/5200 STRG",
            "SPX Z26 4800/5200 RR",
            "3 SPX Z26 4800/5000/5200 FLY",
            "SPX Z26/H27 5000 CAL",
        ] {
            rows.push(parse(text).unwrap());
        }
        push(&mut s, rows);
        s.apply(Edit::SetShift { row: 0, shift: OwnShifts { spot_pct: Some(1.5), vol_pts: None } }).unwrap();
        s.apply(Edit::SetShift { row: 1, shift: OwnShifts { spot_pct: None, vol_pts: Some(-2.0) } }).unwrap();
        s.apply(Edit::SetShift { row: 2, shift: OwnShifts { spot_pct: Some(0.0), vol_pts: Some(0.0) } }).unwrap();
        s.apply(Edit::SetSheetShift(OwnShifts { spot_pct: None, vol_pts: Some(1.0) })).unwrap();
        s.apply(Edit::SetSpotOverride { underlying: "SPX".into(), level: Some(5100.0) }).unwrap();
        s.apply(Edit::SetSpotOverride { underlying: "NDX".into(), level: Some(20000.5) }).unwrap();
        s.refresh = Refresh::Every(Duration::from_secs(45));
        // An empty custom package too.
        push(&mut s, vec![callspread(1)]);
        let last = s.len() - 1;
        s.apply(Edit::Remove { at: last }).unwrap();
        s.apply(Edit::Remove { at: last - 1 }).unwrap();
        s
    }

    fn definition(s: &Sheet) -> Vec<(u64, crate::core::sheet::RowKind, Option<u64>, Option<geode_core::pricing::Instrument>, i64, OwnShifts)> {
        (0..s.len())
            .map(|r| (s.id(r).0, s.kind(r), s.parent(r).map(|p| s.id(p).0), s.instrument(r).cloned(), s.qty(r), s.shift(r)))
            .collect()
    }

    #[test]
    fn the_declaration_parses_and_a_full_sheet_validates_against_it() {
        let ds = dataset();
        assert!(ds.local, "a local dataset (spec §7.2)");
        assert!(ds.is_document());
        assert_eq!(ds.key, vec![SHEET_KEY.to_string()]);
        assert_eq!(ds.axes, vec![LINE_AXIS.to_string()]);
        let rows = to_rows(&full_sheet()).expect("a sheet with rows");
        rows.validate(&ds).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rows.key, vec!["book-1".to_string()]);
        assert_eq!(rows.rows(), full_sheet().len());
    }

    #[test]
    fn to_rows_then_from_rows_is_the_same_definition() {
        let s = full_sheet();
        let rows = to_rows(&s).unwrap();
        let back = from_rows("book-1", &rows).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.name, "book-1");
        assert_eq!(back.view, "barrier");
        assert_eq!(back.sheet_shift, s.sheet_shift);
        assert_eq!(back.overrides, s.overrides);
        assert_eq!(back.refresh, Refresh::Every(Duration::from_secs(45)));
        assert_eq!(definition(&back), definition(&s));
        // Results are not persisted (spec §1.2): every line is stale, rev 1, unpriced.
        for r in 0..back.len() {
            assert_eq!(back.revision(r), 1);
            assert_eq!(back.result(r), None);
            if back.is_line(r) {
                assert_eq!(back.state(r), &LineState::Stale);
            }
        }
        // The next id continues past the highest stored one.
        let mut back = back;
        let n = back.len();
        push(&mut back, vec![line(spx(1.0, OptionKind::Call), 1)]);
        assert!(back.id(n).0 > s.id(s.len() - 1).0);
        // And a second round trip is stable.
        assert_eq!(to_rows(&back).unwrap().rows(), n + 1);
    }

    #[test]
    fn an_empty_sheet_has_no_document() {
        let s = Sheet::new("empty");
        assert_eq!(to_rows(&s), None);
        // A sheet whose last line was removed publishes nothing either.
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(1.0, OptionKind::Call), 1)]);
        assert!(to_rows(&s).is_some());
        s.apply(Edit::Remove { at: 0 }).unwrap();
        assert_eq!(to_rows(&s), None);
    }

    #[test]
    fn overrides_and_refresh_encode_both_ways() {
        let mut o = geode_core::pricing::MarketOverrides::default();
        assert_eq!(encode_overrides(&o), "");
        assert_eq!(parse_overrides("").unwrap(), o);
        o.spot.insert("SPX".into(), 5100.0);
        o.spot.insert("NDX".into(), 20000.5);
        assert_eq!(encode_overrides(&o), "NDX=20000.5;SPX=5100");
        assert_eq!(parse_overrides("NDX=20000.5;SPX=5100").unwrap(), o);
        assert!(parse_overrides("SPX").is_err());
        assert!(parse_overrides("SPX=abc").is_err());
        assert!(parse_overrides("=5").is_err());
        assert_eq!(encode_refresh(Refresh::Default), "");
        assert_eq!(encode_refresh(Refresh::Off), "off");
        assert_eq!(encode_refresh(Refresh::Every(Duration::from_secs(30))), "30s");
        assert_eq!(encode_refresh(Refresh::Every(Duration::from_secs(90))), "90s");
        assert_eq!(parse_refresh("").unwrap(), Refresh::Default);
        assert_eq!(parse_refresh("off").unwrap(), Refresh::Off);
        assert_eq!(parse_refresh("30s").unwrap(), Refresh::Every(Duration::from_secs(30)));
        assert_eq!(parse_refresh("2m").unwrap(), Refresh::Every(Duration::from_secs(120)));
        assert!(parse_refresh("soon").is_err());
    }

    #[test]
    fn a_hostile_document_is_refused_with_a_reason() {
        let s = full_sheet();
        let good = to_rows(&s).unwrap();
        // A missing value column.
        let mut rows = good.clone();
        rows.values.retain(|(n, _)| n != "qty");
        assert!(from_rows("book-1", &rows).unwrap_err().contains("qty"));
        // A leg whose parent is not in the document.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::I64(parents))) = rows.values.iter_mut().find(|(n, _)| n == "parent") {
            parents[4] = 9999;
        }
        assert!(from_rows("book-1", &rows).unwrap_err().contains("parent"));
        // The reserved kind.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::Utf8(kinds))) = rows.values.iter_mut().find(|(n, _)| n == "kind") {
            kinds[0] = "underlying".into();
        }
        assert!(from_rows("book-1", &rows).unwrap_err().contains("underlying"));
        // A bad date.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::Utf8(exp))) = rows.values.iter_mut().find(|(n, _)| n == "expiry") {
            exp[0] = "2026-13-40".into();
        }
        assert!(from_rows("book-1", &rows).unwrap_err().contains("expiry"));
        // A repeated line id.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::I64(ids))) = rows.axes.iter_mut().find(|(n, _)| n == LINE_AXIS) {
            ids[1] = ids[0];
        }
        assert!(from_rows("book-1", &rows).unwrap_err().contains("line"));
        // The good one still loads.
        assert!(from_rows("book-1", &good).is_ok());
    }
}
```

`push` was defined in `sheet::tests` for `Vec<RowSpec>`, and `parse(..)` returns a `RowSpec`, so the `rows` vector above is well-typed. The empty package at the end of `full_sheet` is built by the two `Remove`s (planning decision 8). Delivered results are not part of the fixture: they are not persisted (spec §1.2).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricer storage 2>&1 | grep -E 'error\[|^test result' | head`
Expected: compile errors — `to_rows` etc. not found.

- [ ] **Step 3: Implement**

```rust
//! The storage row shape (line-pricer spec §7.2): a sheet as one document
//! of the `pricer_sheets` dataset — one row per line or package row, the
//! sheet-wide settings as document attributes. Only definitions are
//! stored (spec §1.2): a reopened sheet reprices.
//!
//! Values have no NULL, so every optional is a flag plus a value or a
//! kind plus a value. Part 4 wires the dataset into the builtin layer and
//! the `SheetStore` around `DataHandle`; this module is the pure pair.

use crate::core::sheet::{LineId, LineState, OwnShifts, Refresh, RowKind, RowRecord, Sheet};
use crate::core::template::Template;
use chrono::NaiveDate;
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::pricing::{Barrier, BarrierKind, Expiry, Instrument, MarketOverrides, OptionKind, Strike, Vanilla};
use geode_core::source_config::parse_duration;

pub const PRICER_SHEETS_DATASET: &str = "pricer_sheets";
pub const SHEET_KEY: &str = "sheet";
pub const LINE_AXIS: &str = "line";

/// The datasets-doc declaration (spec §7.2), one `[pricer_sheets.columns.<name>]`
/// table per column. Part 4 pushes it into `ConfigSources.builtin`.
pub const PRICER_SHEETS_DECLARATION: &str = r#"[pricer_sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]

[pricer_sheets.columns.sheet]
type = "utf8"
role = "dimension"
textual = true
[pricer_sheets.columns.line]
type = "i64"
role = "axis"

[pricer_sheets.columns.order]
type = "i64"
role = "value"
[pricer_sheets.columns.kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.template]
type = "utf8"
role = "value"
[pricer_sheets.columns.parent]
type = "i64"
role = "value"
[pricer_sheets.columns.qty]
type = "i64"
role = "value"
[pricer_sheets.columns.underlying]
type = "utf8"
role = "value"
[pricer_sheets.columns.expiry_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.expiry]
type = "utf8"
role = "value"
[pricer_sheets.columns.strike_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.strike]
type = "f64"
role = "value"
[pricer_sheets.columns.option_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.barrier_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.barrier]
type = "f64"
role = "value"
[pricer_sheets.columns.spot_shift_own]
type = "i64"
role = "value"
[pricer_sheets.columns.spot_shift]
type = "f64"
role = "value"
[pricer_sheets.columns.vol_shift_own]
type = "i64"
role = "value"
[pricer_sheets.columns.vol_shift]
type = "f64"
role = "value"

[pricer_sheets.columns.view]
type = "utf8"
role = "attribute"
[pricer_sheets.columns.sheet_spot_shift_own]
type = "i64"
role = "attribute"
[pricer_sheets.columns.sheet_spot_shift]
type = "f64"
role = "attribute"
[pricer_sheets.columns.sheet_vol_shift_own]
type = "i64"
role = "attribute"
[pricer_sheets.columns.sheet_vol_shift]
type = "f64"
role = "attribute"
[pricer_sheets.columns.refresh]
type = "utf8"
role = "attribute"
[pricer_sheets.columns.spot_overrides]
type = "utf8"
role = "attribute"
"#;

/// `NDX=20000.5;SPX=5100` in `BTreeMap` order; `""` when empty
/// (planning decision 6). Plain data, no arithmetic.
pub fn encode_overrides(overrides: &MarketOverrides) -> String {
    overrides
        .spot
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn parse_overrides(text: &str) -> Result<MarketOverrides, String> {
    let mut out = MarketOverrides::default();
    if text.is_empty() {
        return Ok(out);
    }
    for pair in text.split(';') {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("spot_overrides: '{pair}' is not UNDERLYING=LEVEL"))?;
        if k.is_empty() {
            return Err(format!("spot_overrides: '{pair}' has no underlying"));
        }
        let level: f64 = v
            .parse()
            .map_err(|_| format!("spot_overrides: '{v}' is not a number"))?;
        out.spot.insert(k.to_string(), level);
    }
    Ok(out)
}

/// `""` | `off` | `<secs>s` (spec §7.2).
pub fn encode_refresh(refresh: Refresh) -> String {
    match refresh {
        Refresh::Default => String::new(),
        Refresh::Off => "off".to_string(),
        Refresh::Every(d) => format!("{}s", d.as_secs()),
    }
}

pub fn parse_refresh(text: &str) -> Result<Refresh, String> {
    match text {
        "" => Ok(Refresh::Default),
        "off" => Ok(Refresh::Off),
        other => parse_duration(other)
            .map(Refresh::Every)
            .ok_or_else(|| format!("refresh: '{other}' is not a duration or 'off'")),
    }
}

fn expiry_parts(e: &Expiry) -> (&'static str, String) {
    match e {
        Expiry::Date(d) => ("date", d.format("%Y-%m-%d").to_string()),
        Expiry::Tenor(t) => ("tenor", t.clone()),
    }
}

fn barrier_name(k: BarrierKind) -> &'static str {
    match k {
        BarrierKind::UpIn => "ui",
        BarrierKind::UpOut => "uo",
        BarrierKind::DownIn => "di",
        BarrierKind::DownOut => "do",
    }
}

fn own_pair(v: Option<f64>) -> (i64, f64) {
    match v {
        Some(x) => (1, x),
        None => (0, 0.0),
    }
}

/// The document for a sheet, or `None` when it has no rows: a zero-row
/// document is refused by the store, and the last non-empty generation
/// stays as history (spec §7.2).
pub fn to_rows(sheet: &Sheet) -> Option<DocumentRows> {
    if sheet.is_empty() {
        return None;
    }
    let n = sheet.len();
    let mut line = Vec::with_capacity(n);
    let mut order = Vec::with_capacity(n);
    let mut kind = Vec::with_capacity(n);
    let mut template = Vec::with_capacity(n);
    let mut parent = Vec::with_capacity(n);
    let mut qty = Vec::with_capacity(n);
    let mut underlying = Vec::with_capacity(n);
    let mut expiry_kind = Vec::with_capacity(n);
    let mut expiry = Vec::with_capacity(n);
    let mut strike_kind = Vec::with_capacity(n);
    let mut strike = Vec::with_capacity(n);
    let mut option_kind = Vec::with_capacity(n);
    let mut barrier_kind = Vec::with_capacity(n);
    let mut barrier = Vec::with_capacity(n);
    let mut spot_own = Vec::with_capacity(n);
    let mut spot = Vec::with_capacity(n);
    let mut vol_own = Vec::with_capacity(n);
    let mut vol = Vec::with_capacity(n);
    for row in 0..n {
        line.push(sheet.id(row).0 as i64);
        order.push(row as i64);
        let (k, t) = match sheet.kind(row) {
            RowKind::Line => ("line", ""),
            RowKind::Package { template } => ("package", template.storage_name()),
            RowKind::Underlying => ("underlying", ""),
        };
        kind.push(k.to_string());
        template.push(t.to_string());
        parent.push(sheet.parent(row).map_or(-1, |p| sheet.id(p).0 as i64));
        qty.push(sheet.qty(row));
        match sheet.instrument(row) {
            Some(i) => {
                let v = i.vanilla();
                underlying.push(v.underlying.clone());
                let (ek, e) = expiry_parts(&v.expiry);
                expiry_kind.push(ek.to_string());
                expiry.push(e);
                let (sk, s) = match v.strike {
                    Strike::Absolute(k) => ("abs", k),
                    Strike::Percent(p) => ("pct", p),
                };
                strike_kind.push(sk.to_string());
                strike.push(s);
                option_kind.push(match v.kind {
                    OptionKind::Call => "call",
                    OptionKind::Put => "put",
                }.to_string());
                match i {
                    Instrument::Barrier(b) => {
                        barrier_kind.push(barrier_name(b.barrier).to_string());
                        barrier.push(b.level);
                    }
                    Instrument::Vanilla(_) => {
                        barrier_kind.push(String::new());
                        barrier.push(0.0);
                    }
                }
            }
            None => {
                underlying.push(String::new());
                expiry_kind.push(String::new());
                expiry.push(String::new());
                strike_kind.push(String::new());
                strike.push(0.0);
                option_kind.push(String::new());
                barrier_kind.push(String::new());
                barrier.push(0.0);
            }
        }
        let sh = sheet.shift(row);
        let (so, s) = own_pair(sh.spot_pct);
        let (vo, v) = own_pair(sh.vol_pts);
        spot_own.push(so);
        spot.push(s);
        vol_own.push(vo);
        vol.push(v);
    }
    let (sso, ss) = own_pair(sheet.sheet_shift.spot_pct);
    let (svo, sv) = own_pair(sheet.sheet_shift.vol_pts);
    Some(DocumentRows {
        key: vec![sheet.name.clone()],
        attributes: vec![
            ("view".into(), Value::Utf8(sheet.view.clone())),
            ("sheet_spot_shift_own".into(), Value::I64(sso)),
            ("sheet_spot_shift".into(), Value::F64(ss)),
            ("sheet_vol_shift_own".into(), Value::I64(svo)),
            ("sheet_vol_shift".into(), Value::F64(sv)),
            ("refresh".into(), Value::Utf8(encode_refresh(sheet.refresh))),
            ("spot_overrides".into(), Value::Utf8(encode_overrides(&sheet.overrides))),
        ],
        axes: vec![(LINE_AXIS.into(), Column::I64(line))],
        values: vec![
            ("order".into(), Column::I64(order)),
            ("kind".into(), Column::Utf8(kind)),
            ("template".into(), Column::Utf8(template)),
            ("parent".into(), Column::I64(parent)),
            ("qty".into(), Column::I64(qty)),
            ("underlying".into(), Column::Utf8(underlying)),
            ("expiry_kind".into(), Column::Utf8(expiry_kind)),
            ("expiry".into(), Column::Utf8(expiry)),
            ("strike_kind".into(), Column::Utf8(strike_kind)),
            ("strike".into(), Column::F64(strike)),
            ("option_kind".into(), Column::Utf8(option_kind)),
            ("barrier_kind".into(), Column::Utf8(barrier_kind)),
            ("barrier".into(), Column::F64(barrier)),
            ("spot_shift_own".into(), Column::I64(spot_own)),
            ("spot_shift".into(), Column::F64(spot)),
            ("vol_shift_own".into(), Column::I64(vol_own)),
            ("vol_shift".into(), Column::F64(vol)),
        ],
    })
}

fn utf8<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a [String], String> {
    match rows.values.iter().find(|(n, _)| n == name) {
        Some((_, Column::Utf8(v))) => Ok(v),
        Some(_) => Err(format!("value '{name}' is not utf8")),
        None => Err(format!("value '{name}' is missing")),
    }
}

fn i64s<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a [i64], String> {
    match rows.values.iter().find(|(n, _)| n == name) {
        Some((_, Column::I64(v))) => Ok(v),
        Some(_) => Err(format!("value '{name}' is not i64")),
        None => Err(format!("value '{name}' is missing")),
    }
}

fn f64s<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a [f64], String> {
    match rows.values.iter().find(|(n, _)| n == name) {
        Some((_, Column::F64(v))) => Ok(v),
        Some(_) => Err(format!("value '{name}' is not f64")),
        None => Err(format!("value '{name}' is missing")),
    }
}

fn attr<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a Value, String> {
    rows.attributes
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
        .ok_or_else(|| format!("attribute '{name}' is missing"))
}

fn attr_utf8(rows: &DocumentRows, name: &str) -> Result<String, String> {
    match attr(rows, name)? {
        Value::Utf8(s) => Ok(s.clone()),
        _ => Err(format!("attribute '{name}' is not utf8")),
    }
}

fn attr_own(rows: &DocumentRows, flag: &str, value: &str) -> Result<Option<f64>, String> {
    let f = match attr(rows, flag)? {
        Value::I64(i) => *i,
        _ => return Err(format!("attribute '{flag}' is not i64")),
    };
    let v = match attr(rows, value)? {
        Value::F64(x) => *x,
        _ => return Err(format!("attribute '{value}' is not f64")),
    };
    Ok((f != 0).then_some(v))
}

fn own(flag: i64, value: f64) -> Option<f64> {
    (flag != 0).then_some(value)
}

/// A sheet from its document. Rows are taken in `order`; a leg's parent
/// is resolved by id and must precede it. Every line comes back `Stale`
/// at revision 1 with no result (results are not stored, spec §1.2), and
/// `next_id` continues past the highest stored id.
pub fn from_rows(name: &str, rows: &DocumentRows) -> Result<Sheet, String> {
    let ids = match rows.axes.iter().find(|(n, _)| n == LINE_AXIS) {
        Some((_, Column::I64(v))) => v,
        Some(_) => return Err(format!("axis '{LINE_AXIS}' is not i64")),
        None => return Err(format!("axis '{LINE_AXIS}' is missing")),
    };
    let order = i64s(rows, "order")?;
    let kind = utf8(rows, "kind")?;
    let template = utf8(rows, "template")?;
    let parent = i64s(rows, "parent")?;
    let qty = i64s(rows, "qty")?;
    let underlying = utf8(rows, "underlying")?;
    let expiry_kind = utf8(rows, "expiry_kind")?;
    let expiry = utf8(rows, "expiry")?;
    let strike_kind = utf8(rows, "strike_kind")?;
    let strike = f64s(rows, "strike")?;
    let option_kind = utf8(rows, "option_kind")?;
    let barrier_kind = utf8(rows, "barrier_kind")?;
    let barrier = f64s(rows, "barrier")?;
    let spot_own = i64s(rows, "spot_shift_own")?;
    let spot = f64s(rows, "spot_shift")?;
    let vol_own = i64s(rows, "vol_shift_own")?;
    let vol = f64s(rows, "vol_shift")?;
    let n = rows.rows();
    for (label, len) in [
        ("order", order.len()), ("kind", kind.len()), ("template", template.len()), ("parent", parent.len()),
        ("qty", qty.len()), ("underlying", underlying.len()), ("expiry_kind", expiry_kind.len()), ("expiry", expiry.len()),
        ("strike_kind", strike_kind.len()), ("strike", strike.len()), ("option_kind", option_kind.len()),
        ("barrier_kind", barrier_kind.len()), ("barrier", barrier.len()), ("spot_shift_own", spot_own.len()),
        ("spot_shift", spot.len()), ("vol_shift_own", vol_own.len()), ("vol_shift", vol.len()),
    ] {
        if len != n {
            return Err(format!("value '{label}' has {len} rows, the axis has {n}"));
        }
    }

    let mut sheet = Sheet::new(name);
    sheet.view = attr_utf8(rows, "view")?;
    sheet.sheet_shift = OwnShifts {
        spot_pct: attr_own(rows, "sheet_spot_shift_own", "sheet_spot_shift")?,
        vol_pts: attr_own(rows, "sheet_vol_shift_own", "sheet_vol_shift")?,
    };
    sheet.refresh = parse_refresh(&attr_utf8(rows, "refresh")?)?;
    sheet.overrides = parse_overrides(&attr_utf8(rows, "spot_overrides")?)?;

    let mut by_order: Vec<usize> = (0..n).collect();
    by_order.sort_by_key(|i| order[*i]);
    let mut seen: Vec<LineId> = Vec::with_capacity(n);
    let mut records: Vec<RowRecord> = Vec::with_capacity(n);
    for i in by_order {
        let id = u64::try_from(ids[i]).map_err(|_| format!("line id {} is negative", ids[i]))?;
        let id = LineId(id);
        if seen.contains(&id) {
            return Err(format!("line id {} appears twice", id.0));
        }
        seen.push(id);
        let parent_id = if parent[i] < 0 {
            None
        } else {
            let pid = LineId(parent[i] as u64);
            if !records.iter().any(|r| r.id == pid && matches!(r.kind, RowKind::Package { .. })) {
                return Err(format!("line {} names parent {} which is not a package before it", id.0, pid.0));
            }
            Some(pid)
        };
        let row_kind = match kind[i].as_str() {
            "line" => RowKind::Line,
            "package" => RowKind::Package {
                template: Template::parse(&template[i])
                    .ok_or_else(|| format!("line {}: unknown template '{}'", id.0, template[i]))?,
            },
            "underlying" => return Err(format!("line {}: kind 'underlying' is reserved and not readable by this build", id.0)),
            other => return Err(format!("line {}: unknown kind '{}'", id.0, other)),
        };
        let instrument = match row_kind {
            RowKind::Line => Some(read_instrument(
                &underlying[i], &expiry_kind[i], &expiry[i], &strike_kind[i], strike[i], &option_kind[i], &barrier_kind[i], barrier[i],
            ).map_err(|m| format!("line {}: {m}", id.0))?),
            _ => None,
        };
        if qty[i] == 0 && row_kind == RowKind::Line {
            return Err(format!("line {}: quantity is zero", id.0));
        }
        records.push(RowRecord {
            id,
            kind: row_kind,
            parent: parent_id,
            instrument,
            qty: qty[i],
            shift: OwnShifts {
                spot_pct: own(spot_own[i], spot[i]),
                vol_pts: own(vol_own[i], vol[i]),
            },
            revision: 1,
            result: None,
            state: if row_kind == RowKind::Line { LineState::Stale } else { LineState::Fresh },
            priced_at: None,
        });
    }
    if !records.is_empty() {
        sheet
            .apply(crate::core::edit::Edit::Restore { at: 0, rows: records })
            .map_err(|e| format!("could not rebuild the sheet: {e}"))?;
    }
    Ok(sheet)
}

#[allow(clippy::too_many_arguments)]
fn read_instrument(
    underlying: &str,
    expiry_kind: &str,
    expiry: &str,
    strike_kind: &str,
    strike: f64,
    option_kind: &str,
    barrier_kind: &str,
    barrier: f64,
) -> Result<Instrument, String> {
    if underlying.is_empty() {
        return Err("a line needs an underlying".into());
    }
    let expiry = match expiry_kind {
        "date" => Expiry::Date(
            NaiveDate::parse_from_str(expiry, "%Y-%m-%d").map_err(|_| format!("expiry '{expiry}' is not a date"))?,
        ),
        "tenor" => Expiry::tenor(expiry).map_err(|m| format!("expiry: {m}"))?,
        other => return Err(format!("expiry_kind '{other}' is not date or tenor")),
    };
    let strike = match strike_kind {
        "abs" => Strike::Absolute(strike),
        "pct" => Strike::Percent(strike),
        other => return Err(format!("strike_kind '{other}' is not abs or pct")),
    };
    let kind = match option_kind {
        "call" => OptionKind::Call,
        "put" => OptionKind::Put,
        other => return Err(format!("option_kind '{other}' is not call or put")),
    };
    let vanilla = Vanilla {
        underlying: underlying.to_string(),
        expiry,
        strike,
        kind,
    };
    let barrier_kind = match barrier_kind {
        "" => None,
        "ui" => Some(BarrierKind::UpIn),
        "uo" => Some(BarrierKind::UpOut),
        "di" => Some(BarrierKind::DownIn),
        "do" => Some(BarrierKind::DownOut),
        other => return Err(format!("barrier_kind '{other}' is not one of ui uo di do")),
    };
    Ok(match barrier_kind {
        None => Instrument::Vanilla(vanilla),
        Some(b) => Instrument::Barrier(Barrier {
            vanilla,
            level: barrier,
            barrier: b,
        }),
    })
}
```

`from_rows` rebuilds through `Edit::Restore` — the one door — which also validates the parent ids a second time and runs `fold_packages`. Drop the `LineSpec` and `Duration` imports from the module head (the tests import `Duration` themselves).

Add to `mod.rs`: `pub mod storage;` and `pub use storage::{LINE_AXIS, PRICER_SHEETS_DATASET, PRICER_SHEETS_DECLARATION, SHEET_KEY, from_rows, to_rows};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricer && cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS. `the_declaration_parses_and_a_full_sheet_validates_against_it` is the one that proves `to_rows` matches the declaration column for column; if `validate` names a column, the declaration and `to_rows` disagree — fix whichever is wrong against spec §7.2.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-pricer
git commit -m "pricer: the storage row shape — to_rows/from_rows round-trips every template, variant and shift state"
```

---

### Task 10: Benchmarks and `docs/perf.md`

**Files:**
- Modify: `crates/geode-pricer/benches/core.rs` (replace the Task 1 stub)
- Modify: `docs/perf.md` (a new section after "Timeseries (spec §4.4, Part 1)")

**Interfaces:**
- Consumes: `parse`, `Sheet`, `Edit`, `Place`, `OwnShifts`, `RowSpec`, `to_rows`, `from_rows` (Tasks 2–9) through `geode_pricer::core`.
- Produces: four criterion benchmarks in group `pricer_core`: `parse_1000_lines`, `apply_undo_sheet_shift_1000` (the widest-touching edit and its undo, every line re-requested twice), `apply_undo_set_instrument_1000` (one cell edit and its undo at 1,000 lines — the per-keystroke cost), `to_rows_from_rows_1000` (what a restore costs on the UI thread).

Spec §12 names three benches: parse, `GridModel::build`, `apply` plus undo. `GridModel` is Part 3's (it needs a theme); the storage round trip is added because Part 4's restore runs `from_rows` on the UI thread and the number belongs beside the others.

- [ ] **Step 1: Write the bench**

Replace `crates/geode-pricer/benches/core.rs`:

```rust
//! The sheet core's costs at spec §8.2's shape — a sheet of 1,000 lines —
//! against §7's 8 ms pure-UI budget. `parse` is per line typed; `apply`
//! + undo is per keystroke; `to_rows`/`from_rows` is per autosave and per
//! restore. Medians go to docs/perf.md under "Line pricer core".

use criterion::{Criterion, criterion_group, criterion_main};
use geode_pricer::core::{Edit, OwnShifts, Place, RowSpec, Sheet, from_rows, parse, to_rows};
use std::hint::black_box;

/// `n` distinct vanilla lines: alternating buy/sell, calls/puts, strikes
/// stepping through 400 levels — enough variety that no two requests are
/// equal, and every tenth line a callspread so packages are folded too.
fn texts(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            if i % 10 == 9 {
                format!("-{} SPX Z26 {}/{} CS", 1 + i % 4, 4000 + (i % 400) * 5, 4100 + (i % 400) * 5)
            } else {
                format!(
                    "{} SPX Z26 {} {}",
                    if i % 3 == 0 { -1 } else { 1 + (i % 5) as i64 },
                    4000 + (i % 400) * 5,
                    if i % 2 == 0 { "C" } else { "P" }
                )
            }
        })
        .collect()
}

fn sheet(n: usize) -> Sheet {
    let mut s = Sheet::new("bench");
    let rows: Vec<RowSpec> = texts(n).iter().map(|t| parse(t).expect("bench text parses")).collect();
    s.apply(Edit::Insert {
        place: Place::Root { at: 0 },
        rows,
    })
    .expect("insert");
    s
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("pricer_core");

    let lines = texts(1_000);
    g.bench_function("parse_1000_lines", |b| {
        b.iter(|| {
            for t in &lines {
                black_box(parse(t).expect("parses"));
            }
        })
    });

    // The sheet-wide shift toggles between set and cleared on alternate
    // iterations, so every iteration is one apply that re-requests every
    // inheriting line plus the undo that re-requests them again.
    let mut s = sheet(1_000);
    g.bench_function("apply_undo_sheet_shift_1000", |b| {
        b.iter(|| {
            let undo = s
                .apply(Edit::SetSheetShift(OwnShifts {
                    spot_pct: Some(2.0),
                    vol_pts: None,
                }))
                .expect("apply");
            black_box(s.undo(&undo).expect("undo"));
        })
    });

    let mut s = sheet(1_000);
    let instrument = s.instrument(500).expect("a line").clone();
    let other = parse("SPX Z26 9999 P").expect("parses");
    let other = match other {
        RowSpec::Line(l) => l.instrument,
        RowSpec::Package { .. } => unreachable!(),
    };
    g.bench_function("apply_undo_set_instrument_1000", |b| {
        b.iter(|| {
            let undo = s
                .apply(Edit::SetInstrument {
                    row: 500,
                    instrument: other.clone(),
                })
                .expect("apply");
            black_box(s.undo(&undo).expect("undo"));
            debug_assert_eq!(s.instrument(500), Some(&instrument));
        })
    });

    let s = sheet(1_000);
    g.bench_function("to_rows_from_rows_1000", |b| {
        b.iter(|| {
            let rows = to_rows(&s).expect("rows");
            black_box(from_rows("bench", &rows).expect("loads"))
        })
    });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

- [ ] **Step 2: Compile and run**

Run: `cargo clippy -p geode-pricer --all-targets -- -D warnings && cargo bench -p geode-pricer 2>&1 | grep -E 'pricer_core/|time:' `
Expected: four benchmarks report; note each median.

- [ ] **Step 3: Record in `docs/perf.md`**

Append after the Timeseries section:

```markdown
## Line pricer core (spec §12, Part 2)

`cargo bench -p geode-pricer`, criterion medians, `--release`, an M-series
Mac. The sheet is 1,000 rows (every tenth a two-leg callspread), the
shape spec §8.2 sizes the grid model for. Nothing here paints; the grid
model bench is Part 3's.

| Benchmark | What it is | Result |
|---|---|---|
| `parse_1000_lines` | the shorthand parser over 1,000 typed lines | *(fill in)* |
| `apply_undo_sheet_shift_1000` | one sheet-wide shift and its undo: every line's request compared twice, ~1,000 lines staled each way | *(fill in)* |
| `apply_undo_set_instrument_1000` | one cell edit and its undo at 1,000 lines: the per-keystroke cost | *(fill in)* |
| `to_rows_from_rows_1000` | the autosave's document build plus a restore's rebuild through `Edit::Restore` | *(fill in)* |

Budget: the per-keystroke figure is what §7's 8 ms pure-UI budget
constrains (an edit happens on the UI thread before the frame that shows
it); the sheet-wide edit is the worst single keystroke (`:shift spot 2`).
`parse` runs once per `enter` in entry mode. The round trip runs once per
autosave (`to_rows`, Part 4's write-behind) and once per restore.
```

Replace each *(fill in)* with the measured median (e.g. `412 µs`). If any figure is over 8 ms, say so in the paragraph and open a TODO line naming the bench — do not tune in this task.

- [ ] **Step 4: Commit**

```bash
git add crates/geode-pricer/benches/core.rs docs/perf.md
git commit -m "pricer: core benches at 1,000 lines — parse, apply+undo, storage round trip — and their perf.md numbers"
```

---

### Task 11: Harness entries, CLAUDE.md, phase history, spec as-built

**Files:**
- Modify: `scripts/mutation-check.sh` (eleven entries, before the anchors-only block at the end; follow the existing entries' placement)
- Modify: `CLAUDE.md` (status table: one row after "Line pricer Part 1"; the **Pricer (Part 1)** rules block renamed **Pricer (Parts 1–2)** and extended; the harness count on the `mutation-check.sh` command line, currently `1200 entries`, set to the real `grep -c '^run_mutation "' scripts/mutation-check.sh`)
- Modify: `docs/phase-history.md` (one paragraph at the end), the spec (a "§17 As built (Part 2)" section after §16)

- [ ] **Step 1: Harness entries**

Each anchor is a verbatim substring of the code as this plan writes it; re-read the file at each anchor before adding the entry (an implementer may have reformatted a line — `cargo fmt` reflows long lines — and the anchor must match the file, not the plan). Run `--anchors-only` after.

```sh
run_mutation "pricer core: an old revision's delivery is installed" \
  crates/geode-pricer/src/core/sheet.rs \
  '        if revision < current {
            return Delivered::OldRevision { current };
        }' \
  '        if false {
            return Delivered::OldRevision { current };
        }' \
  geode-pricer a_delivery_for_an_old_revision_is_dropped_and_the_current_one_installed

run_mutation "pricer core: a package sums its legs unsigned" \
  crates/geode-pricer/src/core/sheet.rs \
  '                        let q = self.qty[leg] as f64;' \
  '                        let q = (self.qty[leg] as f64).abs();' \
  geode-pricer a_package_sums_qty_times_value_over_its_legs_with_signed_quantities

run_mutation "pricer core: a failed leg still sums" \
  crates/geode-pricer/src/core/sheet.rs \
  '            self.result[p] = if complete && failed.is_none() { Some(sum) } else { None };' \
  '            self.result[p] = if complete { Some(sum) } else { None };' \
  geode-pricer a_package_sums_qty_times_value_over_its_legs_with_signed_quantities

run_mutation "pricer core: undo of a remove re-requests the row" \
  crates/geode-pricer/src/core/sheet.rs \
  '        self.state.insert(at, rec.state);' \
  '        self.state.insert(at, LineState::Stale);' \
  geode-pricer undo_of_a_remove_reinstates_rows_with_ids_and_results_and_requests_nothing

run_mutation "pricer core: an inherited sheet shift reprices nothing" \
  crates/geode-pricer/src/core/edit.rs \
  '            Edit::SetSheetShift(_) => (0..self.len()).filter(|r| self.is_line(*r)).map(|r| self.id(r)).collect(),' \
  '            Edit::SetSheetShift(_) => Vec::new(),' \
  geode-pricer a_sheet_shift_reprices_only_lines_that_inherit_it

run_mutation "pricer core: qty changes the request" \
  crates/geode-pricer/src/core/edit.rs \
  '                let old = self.qty(row);
                self.set_qty(row, qty);' \
  '                let old = self.qty(row);
                self.set_qty(row, qty);
                self.touch(row);' \
  geode-pricer set_qty_and_move_change_no_request

run_mutation "pricer core: a spot override stales every underlying" \
  crates/geode-pricer/src/core/edit.rs \
  '                        if self.is_line(row) && self.instrument(row).is_some_and(|i| i.underlying() == key) {' \
  '                        if self.is_line(row) {' \
  geode-pricer a_spot_override_stales_every_line_on_that_underlying_and_only_a_changed_level_does

run_mutation "pricer core: group accepts a package in the run" \
  crates/geode-pricer/src/core/edit.rs \
  '        if (first..end).any(|r| self.depth(r) != 0 || !self.is_line(r)) {' \
  '        if (first..end).any(|r| self.depth(r) != 0) {' \
  geode-pricer group_refuses_a_run_that_is_not_contiguous_roots

run_mutation "pricer shorthand: the third Friday is the first" \
  crates/geode-pricer/src/core/shorthand.rs \
  '    first.checked_add_days(chrono::Days::new(u64::from(to_friday) + 14))' \
  '    first.checked_add_days(chrono::Days::new(u64::from(to_friday)))' \
  geode-pricer a_month_code_resolves_to_the_third_friday

run_mutation "pricer views: an unknown column is only a warning" \
  crates/geode-pricer/src/core/views.rs \
  '                        Severity::Error,
                        &format!("columns.{i}"),
                        format!("unknown column '"'"'{col_name}'"'"'; dropped"),' \
  '                        Severity::Warning,
                        &format!("columns.{i}"),
                        format!("unknown column '"'"'{col_name}'"'"'; dropped"),' \
  geode-pricer an_unknown_column_is_an_error_and_dropped

run_mutation "pricer storage: an empty sheet publishes a zero-row document" \
  crates/geode-pricer/src/core/storage.rs \
  '    if sheet.is_empty() {
        return None;
    }
    let n = sheet.len();' \
  '    let n = sheet.len();' \
  geode-pricer an_empty_sheet_has_no_document
```

If `cargo fmt` has reflowed the `SetSheetShift` arm or the `is_some_and` line onto several lines, copy the file's actual text into the anchor and the mutation, keeping the same one-token change.

Run: `zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "pricer "`
Expected: eleven `caught`, no ANCHOR/AMBIG, exit 0.

- [ ] **Step 2: CLAUDE.md**

Status row, after the Part 1 row:

```
| Line pricer Part 2 (2026-09-20) | `geode-pricer` (core only): `Sheet` struct of arrays, `Edit`/`Undo` through the one `apply` door, the shorthand parser and renderer (IMM codes, seven templates), `fold_packages`, `columns` + `cell_text`, the `pricer_views` doc + `ColumnPlan`, `to_rows`/`from_rows`, four benches. No tile, not yet in the app's dependency graph. Parts 3 (tile) and 4 (storage) next. | `2026-09-19-…line-pricer` §6, §17 |
```

Rename the **Pricer (Part 1)** block to **Pricer (Parts 1–2)** and append these bullets:

- `geode-pricer` (Part 2) is core only: it depends on `geode-core` alone and the app does not yet depend on it. `Sheet::apply(Edit)` is the ONE mutation door (plus `deliver` and `fold_packages`); it compares `Sheet::request(row)` before and after for the lines the edit can touch (`touched_by`) and bumps `revision` + `Stale` on a change — `SetQty`, `Move`, `Group`, `Ungroup` touch nothing; `SetSpotOverride` stales its underlying's lines EXPLICITLY because the request is unchanged by design (overrides ride in `PriceParams`). A `deliver` installs only at exactly the answered revision (`OldRevision` and `FutureRevision` are dropped).
- An insert position is a `Place` (`Root { at }` on a root boundary, `Leg { package, leg }`), never a bare index; the inverse of `Remove` is `Restore` carrying full `RowRecord`s (ids, results, states), so undo of a removal re-requests nothing; `Group { id: Option<LineId> }` lets undo of an `Ungroup` restore the same package id; `Sheet::undo` applies the inverses in order and answers the redo reversed. A package may be empty (`Fresh`, no result). Package state precedence is `Failed` > `Stale` > `Fresh`, and its result is `Some` only when every leg has one.
- Shorthand: `IMM_MONTHS` is one constant; a month form resolves to the third Friday at parse time (a date convention, spec §6.3); `render_package` prints the template form only while the legs match the table, else one line per leg. `Sheet::shorthand(row)` round-trips through `parse` for every template and both variants.
- `cell_text` is the pure half of the grid model (`CellText { text, state }`); Part 3 colours it. `pricer_views` is atomic at depth 1 in `merge::atomic_depth`; `Views::builtin()` is the two bundled views and Part 3 pushes `BUILTIN_VIEWS` into the builtin layer. `ColumnPlan::build(view)` takes no sheet.
- Storage: `to_rows` is `None` for an empty sheet (the zero-row refusal); values have no NULL (flag + value pairs); spot overrides persist as the `spot_overrides` attribute (`UND=LEVEL;…`); `Refresh` is three-state (`""`/`off`/`30s`); `from_rows` rebuilds through `Edit::Restore`. `PRICER_SHEETS_DECLARATION` is the dataset's TOML, wired into the builtin layer in Part 4.

Update the harness count in the Commands block to the real number.

- [ ] **Step 3: Phase history and spec as-built**

`docs/phase-history.md`, one paragraph at the end, in the file's voice: Part 2 built the pure core in a new crate (eleven tasks), the fifteen planning decisions from this plan's "Decisions made in planning" (name each briefly), the eleven harness entries, and the four bench figures.

The spec: append after §16:

```markdown
## 17. As built (Part 2, 2026-09-20)

The core landed as `geode-pricer::core` with these resolutions of §6/§7 (plan `2026-09-20-line-pricer-part-2-core.md`, "Decisions made in planning"):

- `RowSpec` is an enum (`Line` | `Package { template, legs }`) and IS the parser's answer; `Insert` takes a `Place` (`Root { at }` | `Leg { package, leg }`), not a bare index.
- `Remove`'s inverse is `Edit::Restore { at, rows: Vec<RowRecord> }`; `Undo { inverse: Vec<Edit> }`; `Sheet::undo` answers the redo; `Group` carries `id: Option<LineId>` for undo's sake.
- `SetSpotOverride` stales its underlying's lines inside `apply` (§9.3's "the tile marks" is the sheet's job).
- `Sheet::refresh` is `Refresh { Default, Off, Every(Duration) }`.
- Spot overrides persist as one `spot_overrides` utf8 attribute (`UND=LEVEL;…`); §7.2's column list gains it (ruling 1 was amended after §7.2 was written).
- `ColumnPlan::build(view)` takes no sheet.
- A package may be empty. Package state precedence is `Failed` > `Stale` > `Fresh`; a package's result is `Some` only when every leg has one.
- `cell_text` (`core::columns`) is the pure half of §8.2's grid model.
- A custom package's shorthand is its legs one per line.
- The two bundled views are `BUILTIN_VIEWS` in the crate; §11's "the demo layer adds `pricer_views`" is unnecessary.
- `Instrument::vanilla()`/`expiry()` were added to `geode_core::pricing`; `"pricer_views"` to `config::merge::atomic_depth`.
- `PRICER_SHEETS_DECLARATION` (the §7.2 dataset as TOML) lives in `core::storage`; Part 4 wires it.
- Benches: parse, `apply` + undo (two shapes) and the storage round trip; `GridModel::build` is Part 3's. Numbers in `docs/perf.md`.
- Nothing in Part 2 paints; the crate is not yet in the app's dependency graph.
```

- [ ] **Step 4: Full verification**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo check -p geode-shell --features test-support --all-targets && zsh scripts/mutation-check.sh --anchors-only
```
Expected: all green; `--anchors-only` exits 0.

- [ ] **Step 5: Commit**

```bash
git add docs CLAUDE.md scripts/mutation-check.sh
git commit -m "docs+harness: line pricer Part 2 — rules, eleven entries, spec as-built"
```

---

## Self-review notes

- **Spec coverage (Part 2 scope, §14 part 2):** `Sheet` §6.1 (Task 4), `Edit`/`Undo` §6.2 (Tasks 4–6), parser and renderer §6.3 (Tasks 2, 3), templates §6.3 (Task 1), `fold_packages` §6.4 (Task 4), `columns` §6.5 (Task 7), the `pricer_views` doc and `ColumnPlan` §6.5 (Task 8), `to_rows`/`from_rows` §7.2 (Task 9), the benches §12 (Task 10), the `geode-pricer` core test list §12 (every named test exists: parse table — Task 2; round trip — Task 3; apply/undo identity — Task 6; package sums — Task 4; revision drop — Task 4; `Group` refusal — Task 6; `Move` within parent — Task 6; storage round trip — Task 9; `ColumnPlan` per bundled view — Task 8; unknown-column diagnostic — Task 8), the harness §12 (Task 11: revision drop, package sign, inherited shift, undo as inverse, `qty` changes no request, zero-row refusal, plus five more), §13 docs (Task 11). Deferred as the spec says: everything in §8 (tile), §9.1/9.2/9.4/9.5 (the tile's repricing loop, though `deliver` and `stale_lines` are its core), §7.1/7.3/7.4 (store, write-behind, names).
- **Placeholder scan:** the `todo!("Task 5")`/`todo!("Task 6")` arms in Task 4's `apply_inner` are deliberate compile-time stand-ins replaced by the named tasks, not plan placeholders; every other step carries its code.
- **Type consistency:** `LineSpec`/`RowSpec`/`OwnShifts` are defined in Task 2 and moved in Task 4 with the re-export paths unchanged; `Place::{Root, Leg}`, `RowRecord`, `Delivered`, `Refresh` (Task 4) are used by Tasks 5, 6, 9 with the same field names; `Edit::Group { first, count, template, id }` in Tasks 4 and 6; `Sheet::{touch, set_leg_marker, reindex_parents, splice_in, take_out, swap_adjacent_blocks, new_record, new_package_record, has_id, fresh_id}` are `pub(crate)` in Task 4 and consumed in Tasks 5, 6, 9; `CellText { text, state }`/`CellState` (Task 7) match the tests; `Views::{from_doc, builtin, get, names, is_empty}` and `ColumnPlan::build(&PricerView)` (Task 8) match Task 10's imports; `to_rows -> Option<DocumentRows>` and `from_rows(name, &DocumentRows)` (Task 9) match Task 10. The tests' helper module `sheet::tests` is `pub(crate)` so `edit`, `columns` and `storage` tests share `spx`/`line`/`callspread`/`push`/`result`/`at`.
- **Verified while planning:** the third Fridays the tests assert (2026-12-18, 2026-05-15, 2026-08-21, 2027-03-19 and all twelve of 2027) and every byte offset in the parser's error tests were checked against a calendar and the literal strings.
