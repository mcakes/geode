# Named Scope Expressions: Part 1 (Core, Storage, Resolution, Dialogs) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A saved scope can reference named expressions stored in
`expressions.toml`. The references are live and are resolved before any query,
and a missing or invalid name is an explicit error, never a wider scope. The
Scopes dialog gets a ticked "Named expressions" field, and a new Expressions
object dialog manages the named expressions themselves.

**Architecture:**

- **Core types.** `geode-core` gains `named::NamedExpressions` (the reader for
  the new layered document) and `Scope.named: Vec<String>`. It also gains
  `Scope::resolve`, which folds each named expression into the scope's
  expression with `and` and clears the list, or fails and names the bad
  reference.
- **Frame.** The frame holds the current `NamedExpressions`, the same way it
  holds saved scopes. `Frame::effective_scope` becomes fallible, and every
  site that hands a scope to the data layer resolves it first.
- **Safety net.** `geode-data`'s scope compiler refuses any scope that still
  carries names. A resolve missed at some call site therefore becomes a
  visible error rather than silently dropping the filter.
- **Dialogs.** Both reuse the object-dialog machinery. The Scopes field is an
  `OrderedList`, like Dimensions. The Expressions domain is modelled on the
  Colors domain.

**Tech Stack:** Rust, GPUI and gpui-component 0.6.2, and DuckDB. The plan adds
no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-27-named-scope-expressions-design.md`.
This plan covers "Delivery 1". Part 2 (the frame surfaces) is a separate plan.

## Global Constraints

- Work in the worktree branch `worktree-named-expressions`. Every commit ends
  with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- `geode-core` stays pure. It does no I/O beyond the existing `Config` entry
  points.
- `geode-shell` and `geode-data` never depend on each other.
- The document is named `expressions`, with the constant
  `geode_core::config::EXPRESSIONS_DOC`. Each object has one key,
  `expression = "<text>"`, and whole objects are replaced by name across the
  layers (`atomic_depth` `Some(1)`).
- Saved scopes and the session `[frame]` store references as
  `named = ["a", "b"]`. The key is omitted when the list is empty.
- `Scope.named` keeps insertion order with no duplicates. In `and_then`, the
  outer list comes first, followed by inner names not already present.
- Resolution ANDs the named expressions in list order, then ANDs the result
  with the existing expression.
  - A missing name fails with
    `named expression '<name>' is missing`.
  - An invalid one fails with
    `named expression '<name>' is invalid: <reason>`.
  - A scope whose `named` is empty resolves to itself unchanged.
- The compiler refuses any scope whose `named` is non-empty, with the message
  `scope carries unresolved named expressions`.
- The saved-scope reader never drops a scope because a name is missing.
  Resolution reports it instead.
- Runtime config writes go only through `geode_shell::config_write`, and only
  to the user layer.
- Never mutate state or do I/O during render. Use theme tokens and
  `scale::design` only.
- Tests go through production routes (real keys, clicks and reloads), not
  internal mutations.
- Code comments state the local invariant and the failure it prevents. They
  never cite task numbers.
- Docs describe current behaviour only. No chronology.

## Review Focus

1. **A name reaching the query unresolved.** Every scope-to-data site must
   resolve first: the tile query, the picker's values, the Scopes Values stage,
   and expression suggestions. The compiler refusal catches any site that was
   missed. Task 3 tests the refusal, and Task 4 tests the tile path.
2. **A missing name on a restored session or a reloaded config.** The tile
   must show the error and never unscoped rows. The flip barrier must still be
   acknowledged, or other tiles stall until the deadline. Covered in Task 4.
3. **Editing a named expression through reload.** The tile must requery with
   the new text even though its scope did not change. Covered in Task 4.
4. **Unticking the last named expression in the Scopes dialog.** It must be
   allowed. The generic "keep at least one entry" guard must not refuse it.
   Covered in Task 5.
5. **Deleting a named expression that scopes still use.** The confirmation
   must list those scopes, and deleting must not rewrite them. Covered in
   Task 6.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-core/src/named.rs` (new) | `NamedExpr` and `NamedExpressions`: the reader, lookup, and diagnostics |
| `crates/geode-core/src/lib.rs` | `pub mod named;` |
| `crates/geode-core/src/config/mod.rs` | `EXPRESSIONS_DOC` |
| `crates/geode-core/src/config/merge.rs` | `"expressions"` in `atomic_depth` |
| `crates/geode-core/src/scope/mod.rs` | `Scope.named`, `is_empty`, `and_then`, `resolve` |
| `crates/geode-core/src/scopes.rs` | read and write `named` on saved scopes |
| `crates/geode-data/src/store/mod.rs` and `query/scope_sql.rs` | `StoreError::Scope` and the compiler refusal |
| `crates/geode-shell/src/frame.rs` | the stored `NamedExpressions`, `replace_named_expressions`, the fallible `effective_scope` |
| `crates/geode-shell/src/session.rs` | `[frame] named` |
| `crates/geode-shell/src/shell/hot_reload.rs` and `shell/mod.rs` | `rebuild_named_expressions`, startup, and the reload arm |
| `crates/geode-shell/src/shell/{picker.rs, expr_suggest.rs, objectdialog/render.rs}` | resolve before each distinct request |
| `crates/geode-blotter/src/tile.rs` | show a resolution error as the tile error, acknowledging the barrier |
| `crates/geode-shell/src/shell/objectdialog/scopes.rs` and `render.rs` and `mod.rs` | the Scopes "Named expressions" field |
| `crates/geode-shell/src/shell/objectdialog/expressions.rs` (new) and `mod.rs` and `render.rs` | the Expressions domain |
| `crates/geode-shell/src/defaults.rs` and `shell/input.rs` | the `config::expressions` action |
| `scripts/mutation-check.sh` | targeted entries |
| docs | `configuration.md`, `configuration-dialogs.md`, `shell.md`, READMEs |

---

### Task 1: `NamedExpressions` reader and the `expressions` document (geode-core)

**Files:**
- Create: `crates/geode-core/src/named.rs`
- Modify: `crates/geode-core/src/lib.rs` (add `pub mod named;` alphabetically)
- Modify: `crates/geode-core/src/config/mod.rs` (add
  `pub const EXPRESSIONS_DOC: &str = "expressions";` next to `COLORS_DOC`. Do
  not add it to `RENAMED_DOCS`.)
- Modify: `crates/geode-core/src/config/merge.rs` (add `"expressions"` to the
  `Some(1)` arm of `atomic_depth`)

**Interfaces:**
- Produces:
  - `pub enum NamedExpr { Valid { text: String, expr: Expr }, Invalid { text: String, reason: String } }`
    with `pub fn text(&self) -> &str`
  - `#[derive(Debug, Clone, Default, PartialEq)] pub struct NamedExpressions`
    with the methods:
    - `from_doc(doc: &MergedDoc, vocab: &ExprVocab) -> (Self, Vec<Diagnostic>)`
    - `get(&self, name: &str) -> Option<&NamedExpr>`
    - `names(&self) -> impl Iterator<Item = &str>` (sorted by name)
    - `is_empty(&self) -> bool`
  - `EXPRESSIONS_DOC`

- [ ] **Step 1: Write the failing tests** in `named.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Layer, LayerDoc, merge_docs};
    use crate::scope::complete::ExprVocab;

    fn doc(layers: &[(&str, Layer)]) -> MergedDoc {
        let docs: Vec<LayerDoc> = layers
            .iter()
            .map(|(text, layer)| {
                let mut d = LayerDoc::builtin(EXPRESSIONS_DOC, text).unwrap();
                d.layer = *layer;
                d
            })
            .collect();
        merge_docs(EXPRESSIONS_DOC, &docs)
    }

    #[test]
    fn valid_and_invalid_entries_are_both_kept() {
        let (n, diags) = NamedExpressions::from_doc(
            &doc(&[("[good]\nexpression = \"npv > 0\"\n[bad]\nexpression = \"npv >\"\n", Layer::Builtin)]),
            &ExprVocab::default(),
        );
        assert!(matches!(n.get("good"), Some(NamedExpr::Valid { .. })));
        match n.get("bad") {
            Some(NamedExpr::Invalid { reason, text }) => {
                assert_eq!(text, "npv >");
                assert!(reason.starts_with("expected a value"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(n.names().collect::<Vec<_>>(), ["bad", "good"]);
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("expressions.bad.expression")));
    }

    #[test]
    fn a_non_table_or_missing_expression_is_invalid_not_dropped() {
        let (n, _) = NamedExpressions::from_doc(
            &doc(&[("x = 3\n[empty]\n", Layer::Builtin)]),
            &ExprVocab::default(),
        );
        assert!(matches!(n.get("x"), Some(NamedExpr::Invalid { .. })));
        assert!(matches!(n.get("empty"), Some(NamedExpr::Invalid { .. })));
    }

    #[test]
    fn the_user_layer_replaces_a_desk_object_whole() {
        let (n, _) = NamedExpressions::from_doc(
            &doc(&[
                ("[liq]\nexpression = \"npv > 0\"\n[keep]\nexpression = \"npv < 0\"\n", Layer::Desk),
                ("[liq]\nexpression = \"npv > 5\"\n", Layer::User),
            ]),
            &ExprVocab::default(),
        );
        assert_eq!(n.get("liq").unwrap().text(), "npv > 5");
        assert_eq!(n.get("keep").unwrap().text(), "npv < 0");
    }

    #[test]
    fn unknown_columns_warn_but_stay_valid() {
        use crate::dimensions::DerivedDimensions;
        use crate::schema::SchemaSpec;
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let (schema, _) = SchemaSpec::from_doc(&merge_docs("datasets", &[datasets]));
        let vocab = ExprVocab::new(&schema, &DerivedDimensions::default());
        let (n, diags) = NamedExpressions::from_doc(
            &doc(&[("[liq]\nexpression = \"bokk = 'A'\"\n", Layer::Builtin)]),
            &vocab,
        );
        assert!(matches!(n.get("liq"), Some(NamedExpr::Valid { .. })));
        let d = diags.iter().find(|d| d.path.as_deref() == Some("expressions.liq.expression")).unwrap();
        assert_eq!(d.severity, crate::config::Severity::Warning);
        assert!(d.message.contains("unknown column 'bokk'"), "{}", d.message);
    }
}
```

Adapt `LayerDoc` construction to the real API (check `LayerDoc`'s fields and
constructors in `config/`). Keep the assertions.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core --lib named`
Expected: a compile error.

- [ ] **Step 3: Implement** `named.rs` above the tests:

```rust
//! Named scope expressions, `expressions.toml`: an expression text saved
//! under a name so saved scopes and the frame can refer to it rather than
//! copy it. A reference is resolved by `Scope::resolve` before any query. An
//! entry that does not parse is kept as `Invalid` with its reason, so a
//! reference to it reports "invalid" rather than "missing". Dropping it would
//! make a broken definition indistinguishable from a deleted one.

use std::collections::BTreeMap;

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::scope::complete::{ExprVocab, check};
use crate::scope::{Expr, parse_expr};

#[derive(Debug, Clone, PartialEq)]
pub enum NamedExpr {
    Valid { text: String, expr: Expr },
    Invalid { text: String, reason: String },
}

impl NamedExpr {
    pub fn text(&self) -> &str {
        match self {
            NamedExpr::Valid { text, .. } | NamedExpr::Invalid { text, .. } => text,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamedExpressions {
    map: BTreeMap<String, NamedExpr>,
}

impl NamedExpressions {
    /// Read the merged document. Unknown columns in a valid expression are a
    /// warning, and the entry stays valid; the query reports the column error
    /// exactly as it does for any expression.
    pub fn from_doc(doc: &MergedDoc, vocab: &ExprVocab) -> (Self, Vec<Diagnostic>) {
        let mut map = BTreeMap::new();
        let mut diags = Vec::new();
        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let path = format!("expressions.{name}.expression");
            let warn = |message: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("expression '{name}': {message}"),
                path: Some(path.clone()),
            };
            let text = value
                .as_table()
                .and_then(|t| t.get("expression"))
                .and_then(|v| v.as_str());
            let entry = match text {
                None => {
                    let reason = "missing 'expression' text".to_string();
                    diags.push(warn(reason.clone()));
                    NamedExpr::Invalid { text: String::new(), reason }
                }
                Some(text) => match parse_expr(text.trim()) {
                    Ok(expr) => {
                        for w in check(text, vocab, None) {
                            diags.push(warn(w.message));
                        }
                        NamedExpr::Valid { text: text.to_string(), expr }
                    }
                    Err(e) => {
                        let reason = format!("{} at column {}", e.message, e.caret + 1);
                        diags.push(warn(reason.clone()));
                        NamedExpr::Invalid { text: text.to_string(), reason }
                    }
                },
            };
            map.insert(name.clone(), entry);
        }
        (Self { map }, diags)
    }

    pub fn get(&self, name: &str) -> Option<&NamedExpr> {
        self.map.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
```

`doc.value` is the merged table. Match the field name `MergedDoc` actually
uses; `DerivedDimensions::from_doc` iterates it the same way.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core --lib`
Expected: all tests pass, and the existing merge tests are unaffected.

- [ ] **Step 5: Add a mutation entry and commit**

Append before `if [[ -n "$changed_ref" ]]; then` in
`scripts/mutation-check.sh`:

```zsh
# ---- Named scope expressions.
# The expressions document replaces whole objects by name across layers.
run_mutation "named expr: the user layer replaces an object whole" \
  crates/geode-core/src/config/merge.rs \
  '        | "scopes"' \
  '        | "scopes" | "expressions_off"' \
  geode-core \
  the_user_layer_replaces_a_desk_object_whole
```

This anchor only works if `"expressions"` sits on its own `| "expressions"`
line. Anchor the mutation on that line instead, mutating it to
`| "expressions_off"`, and make sure the anchor is unique.

Run `zsh scripts/mutation-check.sh --anchors-only`, then
`zsh scripts/mutation-check.sh "named expr"`. Expected: `caught`.

Commit with the message `feat(core): named scope expressions document`.

---

### Task 2: `Scope.named`: composition, resolution, saved-scope storage (geode-core)

**Files:**
- Modify: `crates/geode-core/src/scope/mod.rs`
- Modify: `crates/geode-core/src/scopes.rs`

**Interfaces:**
- Consumes: `NamedExpressions` and `NamedExpr` (Task 1).
- Produces:
  - `pub named: Vec<String>` on `Scope`
  - `Scope::resolve(&self, named: &NamedExpressions) -> Result<Scope, String>`
  - saved scopes read and write `named`

- [ ] **Step 1: Write the failing tests**

In `scope/mod.rs` tests:

```rust
    fn named_fixture() -> crate::named::NamedExpressions {
        use crate::config::{LayerDoc, merge_docs};
        let d = LayerDoc::builtin(
            "expressions",
            "[a]\nexpression = \"x = 1\"\n[b]\nexpression = \"y = 2\"\n[bad]\nexpression = \"z >\"\n",
        )
        .unwrap();
        crate::named::NamedExpressions::from_doc(
            &merge_docs("expressions", &[d]),
            &crate::scope::complete::ExprVocab::default(),
        )
        .0
    }

    #[test]
    fn resolve_ands_names_in_order_then_the_expression_and_clears_the_list() {
        let s = Scope {
            named: vec!["a".into(), "b".into()],
            expression: Some(parse_expr("w = 3").unwrap()),
            ..Scope::default()
        };
        let r = s.resolve(&named_fixture()).unwrap();
        assert!(r.named.is_empty());
        assert_eq!(r.expression.unwrap().to_string(), "((x = 1) and (y = 2)) and (w = 3)");
    }

    #[test]
    fn resolve_without_names_is_identity() {
        let s = Scope { expression: Some(parse_expr("w = 3").unwrap()), ..Scope::default() };
        assert_eq!(s.resolve(&named_fixture()).unwrap(), s);
    }

    #[test]
    fn resolve_names_the_first_bad_reference() {
        let missing = Scope { named: vec!["a".into(), "gone".into()], ..Scope::default() };
        assert_eq!(
            missing.resolve(&named_fixture()),
            Err("named expression 'gone' is missing".to_string())
        );
        let invalid = Scope { named: vec!["bad".into()], ..Scope::default() };
        let e = invalid.resolve(&named_fixture()).unwrap_err();
        assert!(e.starts_with("named expression 'bad' is invalid: expected a value"), "{e}");
    }

    #[test]
    fn and_then_keeps_outer_names_first_without_duplicates() {
        let outer = Scope { named: vec!["a".into(), "b".into()], ..Scope::default() };
        let inner = Scope { named: vec!["b".into(), "c".into()], ..Scope::default() };
        assert_eq!(outer.and_then(&inner).named, ["a", "b", "c"]);
    }

    #[test]
    fn a_scope_with_only_names_is_not_empty() {
        assert!(!Scope { named: vec!["a".into()], ..Scope::default() }.is_empty());
    }
```

The expected `to_string` shape follows `Display`, which fully parenthesises
every `And` operand. Adjust the literal to what `Display` prints for
`And(And(x,y), w)`. The order it asserts must not change.

In `scopes.rs` tests:

```rust
    #[test]
    fn named_round_trips_and_a_missing_name_never_drops_the_scope() {
        let doc = merged_scopes("[s]\nnamed = [\"liq\", \"hedges\"]\n[s.dimensions]\nbook = [\"BK001\"]\n");
        let (saved, _) = saved_scopes_from_doc(&doc, /* existing args for datasets/dims */);
        let s = saved.get("s").expect("kept although 'liq' is not defined anywhere");
        assert_eq!(s.named, ["liq", "hedges"]);
        let table = scope_to_table(s);
        assert_eq!(
            table.get("named").and_then(|v| v.as_array()).map(|a| a.len()),
            Some(2)
        );
        assert!(scope_to_table(&Scope::default()).get("named").is_none(), "empty list omitted");
    }

    #[test]
    fn a_non_array_named_warns_and_is_ignored() {
        let doc = merged_scopes("[s]\nnamed = \"liq\"\n[s.dimensions]\nbook = [\"BK001\"]\n");
        let (saved, diags) = saved_scopes_from_doc(&doc, /* … */);
        assert!(saved.get("s").unwrap().named.is_empty());
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("scopes.s.named")));
    }
```

Build `merged_scopes` from the existing helpers in `scopes.rs`'s tests (look
at how the round-trip test at around :187 builds its doc and calls the
reader). The fixture must include a datasets doc with `book`, so the scope
validates.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core --lib scope`
Expected: a compile error.

- [ ] **Step 3: Implement**

1. `Scope` gets `pub named: Vec<String>`, with a doc comment:

   > Named expressions this scope refers to, by name, ANDed with its other
   > parts. Resolved by [`Scope::resolve`] before any query. The data
   > compiler refuses a scope that still carries names.

   Fix the non-`..Default` literals: `and_then`'s result at
   `scope/mod.rs:96`, `session.rs:214` (compile-fixed here, then given real
   content in Task 4), and the test literals at `scope/mod.rs:487, 518, 565`
   and `session.rs:2258`. Let the compiler find any others.
2. `is_empty` gains `&& self.named.is_empty()`.
3. In `and_then`:
   `named: self.named.iter().chain(inner.named.iter().filter(|n| !self.named.contains(n))).cloned().collect()`.
4. Add `resolve`:

```rust
    /// This scope with every named expression folded into `expression`:
    /// the names in list order, ANDed together, then ANDed with the existing
    /// expression; `named` comes back empty. The first missing or invalid
    /// name is an error, never skipped. Skipping it would widen the scope
    /// and produce plausible wrong totals.
    pub fn resolve(&self, named: &NamedExpressions) -> Result<Scope, String> {
        if self.named.is_empty() {
            return Ok(self.clone());
        }
        let mut folded: Option<Expr> = None;
        for name in &self.named {
            let expr = match named.get(name) {
                None => return Err(format!("named expression '{name}' is missing")),
                Some(NamedExpr::Invalid { reason, .. }) => {
                    return Err(format!("named expression '{name}' is invalid: {reason}"));
                }
                Some(NamedExpr::Valid { expr, .. }) => expr.clone(),
            };
            folded = Some(match folded {
                None => expr,
                Some(acc) => Expr::And(Box::new(acc), Box::new(expr)),
            });
        }
        let mut out = self.clone();
        out.named.clear();
        out.expression = match (folded, self.expression.clone()) {
            (Some(n), Some(e)) => Some(Expr::And(Box::new(n), Box::new(e))),
            (n, e) => n.or(e),
        };
        Ok(out)
    }
```

5. `scopes.rs`:
   - `saved_scopes_from_doc` reads `named`. An array of strings is kept in
     order with duplicates removed.
   - Any other value produces a Warning at `scopes.<name>.named` and is
     ignored; the scope is kept.
   - Do not validate that the names exist.
   - The path heuristic that sends non-dimension diagnostics to
     `.expression` must not rewrite this path.
   - `scope_to_table` writes `named` as a string array when it is non-empty.
6. `Scope::validate` and `applicable_to` are unchanged. `named` passes
   through `applicable_to`, and callers resolve before querying.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core --lib`, `cargo check --workspace --all-targets`,
and `cargo test -p geode-shell --lib session` (the literal fix).
Expected: all pass.

- [ ] **Step 5: Add mutation entries and commit**

```zsh
# A missing name must fail resolution; skipping it would widen the scope.
run_mutation "named expr: a missing name fails resolution" \
  crates/geode-core/src/scope/mod.rs \
  "                None => return Err(format!(\"named expression '{name}' is missing\")),"  \
  "                None => continue," \
  geode-core \
  resolve_names_the_first_bad_reference

# Composition never repeats an outer name.
run_mutation "named expr: and_then skips a duplicate inner name" \
  crates/geode-core/src/scope/mod.rs \
  '.filter(|n| !self.named.contains(n))' \
  '.filter(|_n| true)' \
  geode-core \
  and_then_keeps_outer_names_first_without_duplicates
```

Anchors must match the committed source exactly. If rustfmt reflowed a line,
copy the reflowed text. Run the anchor check and the `named expr` filter.
Expected: both caught.

Commit with the message `feat(core): scopes reference named expressions`.

---

### Task 3: The compiler refuses unresolved names (geode-data)

**Files:**
- Modify: `crates/geode-data/src/store/mod.rs` (add a `StoreError::Scope(String)`
  variant whose `Display` is `scope: {0}`, beside `Document` and `Series`)
- Modify: `crates/geode-data/src/query/scope_sql.rs` (the top of
  `compile_scope_cached`, beside the `impossible` early return)

**Interfaces:**
- Produces: `compile_scope_cached` returns
  `Err(StoreError::Scope("scope carries unresolved named expressions".into()))`
  for a non-empty `named`. That covers both view queries and distinct queries.

- [ ] **Step 1: Write the failing tests** in the `scope_sql.rs` tests. Add
  `a_scope_with_unresolved_names_is_refused`, which calls the same entry the existing compile tests use, with
  `Scope { named: vec!["liq".into()], ..Scope::default() }`, and asserts the
  error's `to_string()` is
  `"scope: scope carries unresolved named expressions"`. Add a second in
  `distinct.rs`'s tests that asserts `compile_distinct_with_cache` (or its
  test entry) returns the same error. Copy an existing distinct test's setup.
- [ ] **Step 2: Run** `cargo test -p geode-data --lib named`. Expected: FAIL.
- [ ] **Step 3: Implement.** Add the check and a one-line comment:

  > A name must be resolved in the shell before the scope reaches here;
  > compiling without it would drop that filter.

- [ ] **Step 4: Run** `cargo test -p geode-data --lib`. Expected: all pass.
- [ ] **Step 5: Add a mutation entry and commit.**

```zsh
# A scope carrying names is refused, never compiled without them.
run_mutation "named expr: the compiler refuses unresolved names" \
  crates/geode-data/src/query/scope_sql.rs \
  '    if !scope.named.is_empty() {' \
  '    if false {' \
  geode-data \
  a_scope_with_unresolved_names_is_refused
```

Commit with the message
`feat(data): refuse a scope with unresolved named expressions`.

---

### Task 4: Frame, session, reload, and resolution at every query site (geode-shell, geode-blotter)

**Files:**
- Modify: `crates/geode-shell/src/frame.rs`
- Modify: `crates/geode-shell/src/session.rs`
- Modify: `crates/geode-shell/src/shell/hot_reload.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (startup)
- Modify: `crates/geode-shell/src/shell/picker.rs`
- Modify: `crates/geode-shell/src/shell/expr_suggest.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs`
  (`enter_values_stage`)
- Modify: `crates/geode-blotter/src/tile.rs` (`requery`)
- Tests:
  - `frame.rs` unit tests;
  - `session.rs` tests;
  - `shell/tests/reload.rs`;
  - `shell/tests/picker.rs`;
  - blotter tile tests. Find the existing blotter tests that build a
    `Frame` and observe `self.error`.

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces:
  - `Frame::named_expressions(&self) -> &NamedExpressions`
  - `Frame::replace_named_expressions(&mut self, n: NamedExpressions) -> bool`.
    It is equality-guarded, and when it changes it bumps `versions.config`
    like `replace_saved_scopes`.
  - `Frame::effective_scope(&self, tile: &Scope) -> Result<Scope, String>`,
    which returns `self.scope.and_then(tile).resolve(&self.named)`.
  - `pub fn rebuild_named_expressions(config: &Config) -> NamedExpressions`
    in `hot_reload.rs`, beside `rebuild_saved_scopes`. It builds the vocab
    with `super::expr_vocab(config)` and logs diagnostics the way saved-scope
    diagnostics are logged.

- [ ] **Step 1: Write the failing tests**
  - **`frame.rs`:**
    - `effective_scope` returns `Err("named expression 'gone' is missing")`
      for a frame scope naming an undefined expression;
    - after `replace_named_expressions` defines it, `effective_scope` returns
      the folded expression;
    - `replace_named_expressions` bumps the config version exactly when the
      content changes.
  - **`session.rs`:** a `FrameRecord` with `named = ["liq"]` round-trips
    through `to_toml` and `from_toml`. A missing name is kept, since restore
    does not validate. A non-array `named` warns and is ignored.
  - **`shell/tests/reload.rs`:** a reload that adds `[liq] expression = "…"`
    to `expressions` updates `frame.named_expressions()`, and one that edits
    the text changes it again. Drive this through `apply_reload`, as the
    neighbouring tests do.
  - **Blotter:**
    - a tile under a frame scope naming a missing expression shows the error
      `named expression 'gone' is missing` (danger tone) and submits no query;
    - the frame's flip barrier for that tile is acknowledged, using the
      `arrived` path the refused-submit branch uses (`tile.rs` around
      810–833);
    - after `replace_named_expressions` defines the name, the next requery
      clears the error and submits.
  - **Picker (`shell/tests/picker.rs`):** with a frame scope naming a missing
    expression, `frame::pick_book` emits **no** `DistinctRequested`. The
    picker's values area shows the resolution error, delivered through
    `deliver_distinct` with `Err(message)` on the picker's own key and tag.
- [ ] **Step 2: Run the tests to verify they fail.**
- [ ] **Step 3: Implement**
  1. **`Frame`.**
     - Add a `named: NamedExpressions` field, defaulting to empty in `new`,
       so no `Frame::new` signature change.
     - Add the three methods above.
     - `effective_scope` has one production caller, the blotter.
  2. **Startup.** In `shell/mod.rs`, where the frame is built with saved
     scopes (around :1079–1080), also call
     `replace_named_expressions(rebuild_named_expressions(&config))`.
  3. **Reload.** In `hot_reload.rs`, next to the saved-scope rebuild (around
     :302–310):

     ```rust
     let named_changed = changed(EXPRESSIONS_DOC) || changed("datasets") || changed("dimensions");
     ```

     When it is true, rebuild and `replace_named_expressions`, then call
     `cx.notify()` if it changed. Do this before the unconditional
     `note_config_reloaded`.
  4. **Session.**
     - `FrameRecord::to_toml` writes `named` when it is non-empty.
     - `from_toml` reads it: an array of strings, in order and deduplicated.
       Any other value is warned about and ignored.
     - Replace the placeholder added in Task 2 with the real read.
  5. **Picker.**
     - In `request_values`, after building `minus_own`, resolve it against
       `view.frame.read(cx).named_expressions()`.
     - On `Err(message)`, call
       `view.deliver_distinct(DistinctOutcome { key: PICKER_KEY, tag, column, values: Err(message) }, cx)`
       instead of emitting.
     - The tag must already be recorded on the picker state, so the delivery
       is accepted.
  6. **Scopes Values stage.** In `enter_values_stage` (`objectdialog/render.rs`),
     resolve `scopes::draft_scope(...)` the same way. On `Err`, deliver
     `Err(message)` under `SCOPES_KEY`. That path paints
     `scopes::failed_field(message)`.
  7. **`expr_suggest`.**
     - In `request_values`, resolve `values_scope(...)` the same way.
     - On `Err(message)`, call
       `c.deliver(&column, tag, Err(message), &vocab)` directly, after
       `mark_loading`, instead of emitting.
     - Leave the Expressions domain branch (Task 6) out for now.
  8. **Blotter `requery`.**
     - Match `frame.effective_scope(&self.tile_scope)`.
     - On `Err(e)`: set `self.error = Some((e, Tone::DangerText))`, set
       `self.acted = Some(versions)`, acknowledge the barrier the way the
       refused-submit path does, call `cx.notify()`, and return without
       submitting.
     - The unscoped branch (`self.tile_scope.clone()`) never carries names
       because tile scopes come from `:filter`. Leave it, and let the
       compiler refusal cover it.
- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --lib` and `cargo test -p geode-blotter --lib`,
`cargo fmt --check`, and
`cargo clippy -p geode-shell -p geode-blotter --all-targets -- -D warnings`.

- [ ] **Step 5: Add mutation entries and commit**

Add one entry each for:
- **the blotter `Err` arm:** make it fall through and submit anyway. Filter to
  the blotter missing-name test.
- **the reload arm:** mutate `changed(EXPRESSIONS_DOC) ||` to `false ||`.
  Filter to the reload test.
- **the picker's resolve:** emit the unresolved scope. Filter to the picker
  test.

Each entry's anchor is copied from the committed line. Run `--anchors-only`
and `"named expr"`.

Commit with the message
`feat(shell): resolve named expressions before every query`.

---

### Task 5: The Scopes dialog "Named expressions" field

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/scopes.rs`
  (`fields_from_table`, `fold`, `summary`/`selects_summary`, `help`,
  `validate`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (Scopes
  behaviour that currently applies domain-wide)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs`
  (`Draft::step_selected`'s keep-one guard)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`, plus
  `scopes.rs` unit tests

**Behaviour:**
- **Field.** The field is `key: "named"`, `label: "Named expressions"`,
  `FieldKind::OrderedList { items, available: Some(..) }`, placed directly
  after `dimensions`. `fields[0]` stays `dimensions`.
  - `items` are the scope's `named` entries in order, with `included: true`.
    An item whose name is not in the merged `expressions` doc gets the note
    `missing`. One that is `Invalid` gets the note `invalid`.
  - `available` is every name in the merged `expressions` doc that is not
    already an item, with `included: false`.
- **Ticking.**
  - `space` on an available row moves it to the items, ticked. This uses the
    generic `OrderedList` behaviour.
  - `space` on an item flips its tick. `x` moves an item back to available.
  - Unticking the **last** named item is allowed: `step_selected`'s
    "keep at least one entry" guard exempts `key == "named"`.
- **Fold.** `fold` writes `source["named"]` as the **ticked** item names in
  order, and removes the key when none are ticked.
- **Narrowing Scopes-wide behaviour to the `dimensions` field.** The
  following Scopes behaviours currently apply to every list and must apply
  only to `dimensions`:
  - `space` opening the Values stage;
  - the "enter opens this dimension's values" notice;
  - `enter` opening Values;
  - the `x` notices, whose wording names dimensions;
  - the section header.

  The `named` section header reads ``NAMED EXPRESSIONS — `space` ticks``.
  Pass the field key into `section_header_text`. Reordering stays refused
  (`NO_ORDER_NOTICE`), because the order has no meaning under `and`.
- **Validation.** `scopes::validate` adds a **Warning** (not an Error, so
  commit is not blocked) for each ticked name that is missing or invalid, at
  path `scopes.<object>.named.<name>`:
  - `named expression '<name>' is missing`
  - `named expression '<name>' is invalid: <reason>`

  Confirm that `row_for_path` maps that path to the item row so the warning
  glyph paints. If it cannot, map it to the field row and state that in the
  report.
- **Summary and help.**
  - The browse `summary` appends `≡ a, b` when `named` is non-empty.
  - `help("named")`: "Named expressions this scope includes. Each is ANDed
    with the scope; edit one in the Expressions dialog and every scope that
    ticks it changes."

- [ ] **Step 1: Write the failing tests**
  - **Unit tests** (`scopes.rs`):
    - `fields_from_table` yields the named items with `missing` and `invalid`
      notes and the correct available set;
    - `fold` writes only ticked names and drops the key when empty.
  - **GPUI tests** (`tests/objectdialog.rs`, with a fixture of
    `services_with_a_saved_scope` plus an `expressions` doc defining `liq`
    and `hedges`, and `mine` carrying `named = ["liq"]`):
    1. Opening `mine` shows the Named expressions section, with `liq` ticked
       and `hedges` available.
    2. `space` on `hedges` ticks it. After flushing the config write,
       `scopes.toml` holds `named = ["liq", "hedges"]`.
    3. Unticking both writes a scope with no `named` key (the last-untick
       guard).
    4. `space` on a named row never opens the Values stage.
    5. A saved scope naming `gone` shows the warning glyph on that row, and
       the draft still commits.
- [ ] **Step 2: Run to verify the tests fail.**
- [ ] **Step 3: Implement** the behaviour above. Grep `is_scopes` and
  `Domain::Scopes` in `render.rs` and narrow each Scopes-wide list behaviour
  by the selected row's field key.
- [ ] **Step 4: Run** `cargo test -p geode-shell --lib`, fmt and clippy.
- [ ] **Step 5: Add mutation entries and commit**

Add entries for:
- **fold writing only ticked names.** Mutate the filter on `included` so it
  keeps every item. Filter to test 2.
- **the last-untick exemption.** Remove the `"named"` exemption. Filter to
  test 3.
- **the named row not opening Values.** Filter to test 4.

Commit with the message
`feat(shell): Scopes dialog field for named expressions`.

---

### Task 6: The Expressions object dialog

**Files:**
- Create: `crates/geode-shell/src/shell/objectdialog/expressions.rs`
  (model it on `colours.rs`)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs`
- Modify: `crates/geode-shell/src/shell/expr_suggest.rs` (`values_scope`)
- Modify: `crates/geode-shell/src/defaults.rs` and `crates/geode-shell/src/shell/input.rs`
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Behaviour and sites** (the exhaustive `match` arms are in
`objectdialog/mod.rs`; the compiler lists every missing arm):
- **Domain arms.** Add `Domain::Expressions` to the enum and give each
  method this arm:

  | Method | Arm |
  |---|---|
  | `doc()` | `expressions::DOC` |
  | `title()` | `"Expressions"` |
  | `crumb_noun()` | `"expressions"` |
  | `summary_fn()` | `expressions::summary` (a one-line text preview) |
  | `presentation_doc()` | `None` |
  | `roster()` | `None` |
  | `prefix_fn()` | `None` |
  | `reserved_names()` | `&geode_core::scopes::RESERVED_NAMES` |
  | `help()` | `expressions::help` |
  | `text_editable()` | `key == "expression"` |
  | `parse_text()` | `scopes::parse_text` (parses `expression`) |
  | `fields()` | `expressions::fields` |
  | `fields_from_source()` | `expressions::fields_from_table(Some(table))` |
  | `to_table()` | `expressions::to_table` |
  | `validate()` | `expressions::validate` |

  Also:
  - `Destination::doc`: add a `(Presentation, Expressions) => unreachable!`
    arm.
  - `duplicable()`: add `Domain::Expressions`, so `c` copies.
- **`expressions.rs`:**
  - One field: `key: "expression"`, `label: "Expression"`, `Text`, `Doc`.
  - `to_table` starts from `draft.source` and sets `expression`, using
    `super::toml_table_to_edit` as `scopes::to_table` does.
  - `validate` renders the object and reads it back through
    `NamedExpressions::from_doc`. An `Invalid` entry is an **Error**
    diagnostic that blocks commit. An unknown column is a Warning.
  - `help("expression")`: "The expression this name stands for. Scopes that
    tick it AND it in; tab completes columns, operators and values; enter
    refuses an unknown column."
- **Suggestions and Enter.**
  - `expression_entry_open` also accepts `Domain::Expressions`, which turns
    on the suggestions.
  - The Enter schema refusal applies to `Domain::Expressions` too, with the
    same `expression: …` notice.
  - `expr_suggest::values_scope` returns `Some(Scope::default())` for the
    Expressions domain. A named expression has no enclosing scope, so its
    values are dataset-wide.
- **Delete confirmation.** For a Delete of a user-layer object in this
  domain, compute who uses it and show it in the confirmation:
  - Saved scopes: scan the raw `scopes` doc from
    `apply::config_with_pending` for objects whose `named` array contains the
    name. Use the raw doc so a malformed scope still counts.
  - The frame: include it if `frame.scope().named` contains the name.
  - Store the result on `ObjectDialogState` as
    `confirm_detail: Option<String>`, cleared wherever `confirm_target` is
    cleared, and paint it in the confirm row after the question:
    `Used by EQ liquid, RATES liquid and the current scope.`
    - Scope names are in sorted order.
    - With a single user, it reads `Used by EQ liquid.`
    - When the frame is the only user, it reads
      `Used by the current scope.`
    - With no users, there is no sentence.
  - Thread `cx` into `arm_delete` if it does not have it.
  - Deleting never edits the scopes that use the name.
- **Palette.** In `defaults.rs`, add
  `action(reg, "config::expressions", "Edit expressions…", "Configuration")`
  beside `config::colors`. In `input.rs`, dispatch it to
  `objectdialog::render::open(self, Domain::Expressions, window, cx)`.
- **Help sweep.** `every_field_on_every_domain_has_help`: grow the cases
  array by one, adding `("config::expressions", services_with_expressions, "expression")`.

- [ ] **Step 1: Write the failing tests** (GPUI), with the fixture
  `services_with_expressions()`: the keymap, a datasets doc with `book` and
  `npv`, `[liq] expression = "npv > 0"`, and a `scopes` doc where `mine`
  carries `named = ["liq"]`. Use a writable user dir
  (`dialog_test_shell_in_dir`).
  1. `config::expressions` opens browse, listing `liq` with its preview.
  2. `n` → name `hedges` → `i` on Expression → type `npv < 0` → enter. The
     user `expressions.toml` holds `[hedges] expression = "npv < 0"`.
  3. Editing `liq`'s expression to `bokk = 'A'` refuses with the
     `expression: unknown column 'bokk'; did you mean 'book'?` notice.
  4. Typing in the expression field paints `scope-expr-row-npv` (the
     suggestions work here).
  5. `d` on user-layer `liq` shows a confirm row containing
     `Used by mine and the current scope.` when the frame scope also names
     `liq`. Confirming removes `liq` from the user doc and leaves `mine`'s
     `named` untouched.
  6. `c` copies `liq`, and the copy keeps its expression.
- [ ] **Step 2: Run to verify the tests fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p geode-shell --lib`, fmt and clippy.
- [ ] **Step 5: Add mutation entries and commit**

Add entries for:
- **the used-by scan:** it finds nothing. Filter to test 5.
- **`fields_from_source` for Expressions:** it returns empty fields. Filter
  to test 6.
- **the Expressions arm in `expression_entry_open`:** filter to test 4.

Commit with the message `feat(shell): Expressions dialog for named scope expressions`.

---

### Task 7: Documentation and full verification

**Files:**
- `docs/current/configuration.md`:
  - the document table (`expressions`);
  - the atomic list;
  - the validation boundary: named expressions are parsed at load, an
    invalid one is kept as invalid, and unknown columns warn;
  - the hot-reload table (`expressions`, `datasets` and `dimensions` rebuild
    them; tiles requery).
- `docs/current/configuration-dialogs.md`:
  - the domain list (Expressions) and the destination table;
  - the Scopes "Named expressions" field: tick semantics, missing and invalid
    warnings, unticking the last one is allowed;
  - delete confirmation text;
  - Expressions suggestions and Enter refusal.
- `docs/current/shell.md`: a scope has four parts (dimension selections, named
  expressions, text filter, expression); resolution happens before every
  query; a missing or invalid name is the tile's error; session `[frame]`
  stores `named`.
- `docs/current/data-path.md`, if it describes scope compilation: the refusal.
- READMEs: `geode-core` (`named`, `Scope::resolve`), `geode-shell`
  (`objectdialog/expressions`), and `geode-data` (the refusal, if its README
  lists scope errors).

- [ ] **Step 1:** Write each section. Verify every sentence against the code
  as merged, not against this plan.
- [ ] **Step 2: Full verification.** Run each command in the foreground:
  `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`,
  `cargo check -p geode-shell --features test-support --all-targets`,
  `zsh scripts/mutation-check.sh --anchors-only`,
  `zsh scripts/mutation-check.sh "named expr"`.
  Expected: all green, with every `named expr` entry caught.
- [ ] **Step 3:** Commit with the message `docs: named scope expressions`.
- [ ] **Step 4:** List the display checks:
  - the Named expressions section and its warning glyph;
  - the Expressions browse preview width;
  - the confirm row with a long used-by list.
