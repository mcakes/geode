# Market-Data Egress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A market-data panel uploads its draft to a configured egress target, and the draft clears when the upstream echo matches what was sent.

**Architecture:** The dividend kind drops `<id>` from the wire and mints `dividend_id` from content at parse. A typed `egress.toml` names targets per document. `geode-data` serves `Request::Upload` on one worker thread per target and answers `DataEvent::Upload`. The shell routes `Delivery::Upload` by tile key. The panel assembles `DocumentRows` from the base snapshot plus the draft, confirms on the command line, and compares the next generation with what it sent.

**Tech Stack:** Rust, GPUI + gpui-component, DuckDB, quick-xml (documents), the in-process `ChannelAdapter` as the demo transport.

**Spec:** `docs/superpowers/specs/2026-09-23-geode-market-data-egress-design.md` (read it first; Task 1 amends it).

## Global Constraints

- Work in the worktree `.claude/worktrees/market-data-egress`, branch `worktree-market-data-egress`. Use a private `CARGO_TARGET_DIR` per worktree; do not share `target/` with the main checkout.
- `geode-shell` and `geode-data` never depend on each other. The shell carries upload outcomes as plain types.
- Feature modules do not depend on sibling features. `geode-marketdata` never names `geode-documents`.
- Only `geode-data` does source/egress I/O. Only `geode-app` wires registries, config and factories.
- The UI thread never waits: `DataHandle::upload` is a bounded `try_send` that answers `false` on refusal; the caller reports it.
- Every `Delivery` and `DataEvent` match is exhaustive. No wildcard arms added.
- Displayed times go through `geode_core::clock::Clock` (`local_hhmm`); never `chrono::Local`.
- TOML order is significant; keep `preserve_order`.
- The explicit error beats the plausible wrong value: refuse and name rather than coerce or guess.
- Every behaviour contract added gets a `run_mutation` entry in `scripts/mutation-check.sh` naming its test (6th argument). Run targeted entries with `zsh scripts/mutation-check.sh "<name substring>"`. Commit before running mutations. Never run `--changed` or the full harness from a subagent.
- Test the production route: keys through the keymap/keystroke path, deliveries through `TileContent::deliver`, not internal mutators.
- After the demo schema or wire change, `rm -rf $TMPDIR/geode-demo/*` before running `--demo`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Per-task gate: `cargo test -p <crate>` for touched crates, `cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo fmt --check`.

## File Map

| File | Responsibility |
|---|---|
| `crates/geode-documents/src/dividend.rs` | wire without `<id>`, `mint_ids`, parse mints, write ignores the axis |
| `crates/geode-marketdata/src/core/spec.rs` | `DIVIDEND` announced/pay `required: true` |
| `crates/geode-marketdata/src/core/draft.rs` | `Sent { at }`, `set_row_cell` leaves `Sent`, typed `bump`, group guard, `:rebase` from `Sent` |
| `crates/geode-marketdata/src/core/upload.rs` (new) | `assemble` (snapshot + model + draft → `DocumentRows`), `echo_differs` |
| `crates/geode-core/src/egress_config.rs` (new) | typed `egress.toml` reader |
| `crates/geode-data/src/egress.rs` (new) | `resolve`, `UploadParams`/`UploadOutcome`, per-target workers |
| `crates/geode-data/src/handle.rs`, `service.rs` | `Request::Upload`, `DataHandle::upload`, `DataEvent::Upload`, config field |
| `crates/geode-shell/src/module.rs` + every `Delivery` match | `Delivery::Upload` |
| `crates/geode-app/src/{main.rs,bridge.rs,events.rs,demo.rs}` | wiring, demo layer, restart rule |
| `crates/geode-marketdata/src/{tile.rs,content.rs,commands.rs,header.rs,core/menu.rs}` | `:upload`, confirm, outcome, echo |

---

### Task 1: Spec amendments

**Files:**
- Modify: `docs/superpowers/specs/2026-09-23-geode-market-data-egress-design.md`

The code survey found five places where the spec cannot be built as written. Amend it before any code, so implementers and reviewers argue from one text.

- [ ] **Step 1: Add a new section `## 10. Amendments (2026-09-24, from the code survey)` with these five items**

```markdown
1. **Egress addresses are per document, with `{key}`.** `ChannelEgress::upload`
   publishes on the address it is given, and the echo reaches the panel only
   if that address is one the document's source subscribes to. §4's shape
   becomes:

       [egress.sophis]
       adapter = "demo_bus"
       [egress.sophis.documents]
       cvi_params = "marketdata/cvi/{key}"
       dividend_schedule = "marketdata/dividend/{key}"

   `{key}` is replaced by the document key's parts joined with `/`. An address
   without `{key}` is allowed (one fixed address for every key).
2. **`egress.toml` is restart-required, like `sources.toml`.** Nothing reloads
   sources into `geode-data` live; egress joins the same restart list rather
   than inventing a live path. §4's "hot reload keeps the last valid set" is
   withdrawn.
3. **The kind ignores the id axis on write; assembly keeps it.** `DocumentRows`
   for a dividend must still carry the `dividend_id` axis (the kind's
   vocabulary check and `validate` both require it), so §6's "the label column
   is omitted" becomes: assembly writes the painted labels (`new-<n>` included)
   into the axis and `DividendKind::write` never emits them.
4. **Group sizes are captured by the tile, not at first edit.** `Draft::set`
   has no model. The tile calls `Draft::capture_groups(&base_model)` whenever
   the painted model is the draft's base (before `:rebase`, before a policy
   rebase, and before writing the session). A draft restored from an older
   session with no captured groups applies no guard.
5. **`Sent` is not persisted.** The session already persists no draft state;
   a `Sent` draft restores as `Editing` with its edits (the trader may upload
   again). §7's "sent, unconfirmed" is withdrawn.
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/2026-09-23-geode-market-data-egress-design.md
git commit -m "docs: egress spec amendments from the code survey

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Dividend id off the wire, minted at parse

**Files:**
- Modify: `crates/geode-documents/src/dividend.rs` (TAGS L74–81, `MINTED_PREFIX` L67, parse `End` arm L358–372 and `</dividend>` L412–441, `write` L668–671, fixtures `DOC` L703–726 / `expected()` L728–759, tests L903/L910/L925/L870)
- Modify: `scripts/mutation-check.sh` (entry `"dividend: a new- id is refused"`, ~L9941)
- Modify: `crates/geode-documents/README.md`

**Interfaces:**
- Produces: `pub fn mint_ids(ex_dates: &[chrono::NaiveDate]) -> Vec<String>` in `geode_documents::dividend`.
- Parse output is unchanged in shape: `axes: vec![("dividend_id", Column::Utf8(minted))]`.

- [ ] **Step 1: Write the failing tests** (in the `tests` module of `dividend.rs`)

```rust
#[test]
fn mint_ids_numbers_same_day_rows_in_feed_order() {
    let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    assert_eq!(
        mint_ids(&[d("2026-09-18"), d("2026-12-18"), d("2026-09-18"), d("2026-09-18")]),
        vec!["2026-09-18", "2026-12-18", "2026-09-18#2", "2026-09-18#3"],
    );
}

#[test]
fn parse_mints_ids_from_ex_dates() {
    let doc = DividendKind.parse(DOC.as_bytes()).unwrap();
    let Column::Utf8(ids) = &doc.rows.axes[0].1 else { panic!() };
    // DOC's two rows: their exDates, in feed order.
    assert_eq!(ids, &mint_ids(&ex_dates_of(&doc.rows)));
    assert!(ids.iter().all(|id| !id.starts_with("new-")));
}

#[test]
fn an_inbound_id_is_an_unknown_element() {
    let doc = DOC.replacen("<exDate>", "<id>X</id><exDate>", 1);
    let parsed = DividendKind.parse(doc.as_bytes()).unwrap();
    assert!(
        parsed.unknown_paths.iter().any(|p| p.ends_with("dividend/id")),
        "{:?}",
        parsed.unknown_paths
    );
}

#[test]
fn write_emits_no_id_even_for_a_minted_label() {
    let mut rows = expected().rows;
    rows.axes[0].1 = Column::Utf8(vec!["new-1".into(), "new-2".into()]);
    let xml = String::from_utf8(DividendKind.write(&rows).unwrap()).unwrap();
    assert!(!xml.contains("<id>"), "{xml}");
}

#[test]
fn parse_write_parse_is_stable_with_ids_reminted() {
    let first = DividendKind.parse(DOC.as_bytes()).unwrap().rows;
    let bytes = DividendKind.write(&first).unwrap();
    let second = DividendKind.parse(&bytes).unwrap().rows;
    assert_eq!(first, second);
}
```

Add the helper `fn ex_dates_of(rows: &DocumentRows) -> Vec<NaiveDate>` (reads `values[0]` as `Column::Date`) in the tests module.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-documents dividend`
Expected: FAIL (`mint_ids` not found).

- [ ] **Step 3: Implement**
  - Remove `("id", "dividend_id")` from `TAGS` (the array becomes `[(&str, &str); 5]`). An inbound `<id>` now classifies `Shape::Unknown` and is reported by the existing path.
  - Delete `MINTED_PREFIX` and the `"id"` match arm; delete the `id` field from the `Dividend` accumulator and the `"missing id"` check. Keep the other five missing-element refusals.
  - Collect `ex_date_col` as today; after the loop, `let id_col = mint_ids(&ex_date_col);`.
  - `mint_ids`:

```rust
/// Geode's own row identity for a dividend (the wire carries none): the
/// ex date, and `#n` for the `n`th row sharing it in feed order. Stable
/// while a row's ex date and its place among same-day rows are
/// unchanged; `Draft::rebase` refuses edits in a group whose size changed.
/// Never begins `new-`, so it cannot collide with a draft's minted labels.
pub fn mint_ids(ex_dates: &[NaiveDate]) -> Vec<String> {
    let mut seen: HashMap<NaiveDate, usize> = HashMap::new();
    ex_dates
        .iter()
        .map(|d| {
            let n = seen.entry(*d).or_insert(0);
            *n += 1;
            if *n == 1 {
                d.format("%Y-%m-%d").to_string()
            } else {
                format!("{}#{}", d.format("%Y-%m-%d"), n)
            }
        })
        .collect()
}
```

  - In `write`, stop reading `ids` and remove `leaf(&mut w, "id", &ids[i])?;`. Keep the axis-name vocabulary check (the axis must still be present).
  - Remove `<id>…</id>` from the `DOC` fixture and from `write_emits_the_documented_shape`'s expected XML; `expected()` takes minted ids for DOC's ex dates.
  - Delete tests `a_missing_id_is_refused_naming_it`, `a_repeated_id_is_refused_naming_it` and `a_new_prefixed_id_is_refused`. Update `an_unknown_elements_subtree_is_skipped_whole_and_reported_once` if its fixture relied on `<id>`.
  - Update the module doc (L1–15) to say the wire carries no id and Geode mints it.

- [ ] **Step 4: Run all tests that parse dividend XML**

Run: `cargo test -p geode-documents && cargo test -p geode-app demo_bus && cargo test -p geode-data dividend`
Expected: PASS. If a `geode-data` or `geode-app` fixture embeds `<id>`, remove it there too.

- [ ] **Step 5: Harness**

Replace the `"dividend: a new- id is refused"` entry with:

```sh
# The ordinal is what keeps two same-day dividends distinct. Mutated to
# the bare date, the second row takes the first's id and the draft's
# edits for one land on the other.
run_mutation "dividend: mint_ids numbers a repeated ex date" \
  crates/geode-documents/src/dividend.rs \
  '            if *n == 1 {' \
  '            if true {' \
  geode-documents \
  mint_ids_numbers_same_day_rows_in_feed_order

# The wire carries no id. Mutated to emit one, an upload leaks Geode's
# internal row identity to Sophis.
run_mutation "dividend: write emits no id" \
  crates/geode-documents/src/dividend.rs \
  '    w.write_event(Event::Start(BytesStart::new("dividend")))' \
  '    leaf(&mut w, "id", "X")?; w.write_event(Event::Start(BytesStart::new("dividend")))' \
  geode-documents \
  write_emits_no_id_even_for_a_minted_label
```

(Adjust each `from` anchor to the exact line in the file after Step 3; confirm with `zsh scripts/mutation-check.sh --anchors-only`.)

Run: `zsh scripts/mutation-check.sh "dividend:"` (after committing)
Expected: every `dividend:` entry reports the mutation caught.

- [ ] **Step 6: README and commit**

Update `crates/geode-documents/README.md`'s dividend paragraph (no wire id; `mint_ids`). Then:

```bash
git add -A crates/geode-documents crates/geode-app crates/geode-data scripts/mutation-check.sh
git commit -m "documents: dividend id leaves the wire, minted from ex dates at parse

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Draft fixes — required dates, `Sent { at }`, `set_row_cell`, typed `bump`

**Files:**
- Modify: `crates/geode-marketdata/src/core/spec.rs` (DIVIDEND L329–397, doc comment L318–327)
- Modify: `crates/geode-marketdata/src/core/draft.rs` (`DraftState` L22–40, `DraftBadge` L46–52, `set_row_cell` L347, `bump` L515, `badge` L742)
- Modify: `crates/geode-marketdata/src/header.rs` (L237–248)
- Modify: `crates/geode-marketdata/src/tile.rs` (bump call L3495–3560)
- Modify: `crates/geode-marketdata/src/core/matrix.rs` (tests that set `DraftState::Sent`, L1610)

**Interfaces:**
- Produces: `DraftState::Sent { at: String }` (RFC 3339 UTC), `DraftBadge::Sent { at: String }`.
- Produces: `Draft::bump(&mut self, cells: impl Iterator<Item = ((usize, usize), (String, String), f64, ColumnType)>, delta: f64, base: &str) -> Result<usize, String>`.
- Produces: `pub fn bumped(current: f64, delta: f64, ty: ColumnType, column: &str) -> Result<Value, String>` in `draft.rs` (the one typing rule; the tile uses it for inserted rows too).

- [ ] **Step 1: Write the failing tests** (`draft.rs` tests module)

```rust
#[test]
fn set_row_cell_moves_a_sent_draft_back_to_editing() {
    let mut draft = Draft::default();
    draft.insert_row("new-1".into(), None, "t0");
    draft.state = DraftState::Sent { at: "2026-09-24T09:00:00Z".into() };
    assert!(draft.set_row_cell("new-1", "amount", Value::F64(1.0)));
    assert_eq!(draft.state, DraftState::Editing);
}

#[test]
fn bump_lands_the_declared_type() {
    let mut draft = Draft::default();
    let n = draft
        .bump(
            [
                ((0, 0), ("a".into(), "x".into()), 1.5, ColumnType::F64),
                ((0, 1), ("a".into(), "y".into()), 3.0, ColumnType::I64),
            ]
            .into_iter(),
            2.0,
            "t0",
        )
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(draft.edits[&(0, 0)], Value::F64(3.5));
    assert_eq!(draft.edits[&(0, 1)], Value::I64(5));
}

#[test]
fn bump_refuses_a_fractional_delta_on_an_integer_column_before_writing() {
    let mut draft = Draft::default();
    let err = draft
        .bump(
            [
                ((0, 0), ("a".into(), "x".into()), 1.5, ColumnType::F64),
                ((0, 1), ("a".into(), "y".into()), 3.0, ColumnType::I64),
            ]
            .into_iter(),
            0.5,
            "t0",
        )
        .unwrap_err();
    assert!(err.contains("whole numbers") && err.contains("y"), "{err}");
    assert!(draft.is_empty(), "no cell written on a refusal");
}
```

In `spec.rs` tests:

```rust
#[test]
fn dividend_dates_are_required() {
    let Columns::Values(cols) = DIVIDEND.columns else { panic!() };
    for c in ["announced_date", "pay_date", "ex_date", "amount", "status"] {
        assert!(cols.iter().find(|v| v.column == c).unwrap().required, "{c}");
    }
}
```

In `header.rs` tests: a `DraftBadge::Sent { at }` badge renders `sent HH:MM` through `local_hhmm` (copy the `Behind` test's clock setup).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-marketdata draft spec header`
Expected: compile FAIL (`Sent` has no field `at`).

- [ ] **Step 3: Implement**
  - `DraftState::Sent { at: String }` and `DraftBadge::Sent { at: String }`; `badge()` passes `at` through. Fix every `DraftState::Sent` pattern (`matches!(…, DraftState::Clean | DraftState::Sent)` becomes `DraftState::Sent { .. }`).
  - `header.rs`: `DraftBadge::Sent { at } => (false, Some((format!("sent {}", local_hhmm(at, i.clock)).into(), Tone::Time)))`.
  - `set_row_cell`: on a successful insert, `if matches!(self.state, DraftState::Clean | DraftState::Sent { .. }) { self.state = DraftState::Editing; }`.
  - `bumped`:

```rust
/// `:bump`'s one typing rule: the result lands the column's declared type.
/// An `I64` column takes whole-number deltas only; anything else is refused
/// rather than rounded, since a rounded bump is a value the trader did not ask for.
pub fn bumped(current: f64, delta: f64, ty: ColumnType, column: &str) -> Result<Value, String> {
    match ty {
        ColumnType::F64 => Ok(Value::F64(current + delta)),
        ColumnType::I64 if delta.fract() == 0.0 => Ok(Value::I64((current + delta) as i64)),
        ColumnType::I64 => Err(format!("bump: {column} takes whole numbers")),
        other => Err(format!("bump: {column} is not numeric ({other:?})")),
    }
}
```

   (Signature gains `column: &str`; use it in the interface above.)
  - `bump` collects the cells, computes every `bumped` first (return the first `Err` with nothing written), then `set`s each. Return `Ok(n)`.
  - `DIVIDEND`: `announced_date` and `pay_date` `required: true`; rewrite the doc comment above it: every column is required; undeclared dividends carry estimated dates on the wire (ruling 2026-09-23).
  - `tile.rs` bump: pass each cell's declared type: `Columns::Values(cols)` → `spec.value_column(col_label).ty`; `Columns::Axis(_)` → `spec.value_type` (slice values are `F64`). Inserted-row cells use `bumped(value, delta, ty, &col_label)?` instead of `Value::F64(value + delta)`. The tile's `bump` returns the `Err` as the command result.

- [ ] **Step 4: Run tests**

Run: `cargo test -p geode-marketdata`
Expected: PASS (fix any test that constructs `DraftState::Sent` bare).

- [ ] **Step 5: Harness entries** (append beside the other `draft:` entries, ~L14680)

```sh
# A cell written on an inserted row after an upload is unsent work.
# Mutated to leave `Sent`, a matching echo clears the draft and loses it.
run_mutation "draft: set_row_cell leaves Sent" \
  crates/geode-marketdata/src/core/draft.rs \
  '<the new if-matches line inside set_row_cell>' \
  '                if false {' \
  geode-marketdata \
  set_row_cell_moves_a_sent_draft_back_to_editing

# An I64 column's bump lands I64. Mutated to F64, egress refuses the
# type mismatch the first time an integer column is bumped.
run_mutation "draft: bump lands the declared integer type" \
  crates/geode-marketdata/src/core/draft.rs \
  '        ColumnType::I64 if delta.fract() == 0.0 => Ok(Value::I64((current + delta) as i64)),' \
  '        ColumnType::I64 if delta.fract() == 0.0 => Ok(Value::F64(current + delta)),' \
  geode-marketdata \
  bump_lands_the_declared_type
```

Commit, then run: `zsh scripts/mutation-check.sh "draft: set_row_cell leaves Sent"` and `… "draft: bump lands"`. Expected: caught.

- [ ] **Step 6: Commit**

```bash
git commit -am "marketdata: Sent carries its time, set_row_cell leaves Sent, bump lands the declared type, dividend dates required

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Same-day group guard on rebase

**Files:**
- Modify: `crates/geode-marketdata/src/core/draft.rs` (`Draft` L156–174, `rebase` L598, `to_toml` L782, `from_toml` L856)
- Modify: `crates/geode-marketdata/src/tile.rs` (`fn rebase` L3577, the policy branch in `apply` L1168–1219, the session write site; find it with `grep -n to_toml crates/geode-marketdata/src/tile.rs`)

**Interfaces:**
- Produces: `pub fn group_sizes(model: &MatrixModel) -> BTreeMap<String, usize>`: counts `RowState::Document | RowState::Deleted` rows by `group_of(label)`.
- Produces: `pub fn group_of(label: &str) -> &str`: the label before the first `#`, or the whole label.
- Produces: `Draft::capture_groups(&mut self, base: &MatrixModel)`: stores `group_sizes(base)` restricted to groups that hold a label with a cell edit or a `Deleted` mark, in a new field `groups: BTreeMap<String, usize>`.
- `rebase` drop reason for a refused label: `(label, format!("row (same-day rows changed: {was} → {now})"))`.

- [ ] **Step 1: Write the failing tests**

Build two flat `DIVIDEND` models with the existing `test_fixtures` helpers (use the helper the other `rebase` tests use to build a `MatrixModel` from rows). The base has labels `["2026-09-18", "2026-09-18#2", "2026-12-18"]`; the newer one has `["2026-09-18", "2026-09-18#2", "2026-09-18#3", "2026-12-18"]`.

```rust
#[test]
fn rebase_refuses_edits_in_a_same_day_group_that_changed_size() {
    let base = flat_model(&["2026-09-18", "2026-09-18#2", "2026-12-18"]);
    let newer = flat_model(&["2026-09-18", "2026-09-18#2", "2026-09-18#3", "2026-12-18"]);
    let mut draft = Draft::default();
    draft.set((1, 3), ("2026-09-18#2".into(), "amount".into()), Value::F64(1.0), "t0");
    draft.set((2, 3), ("2026-12-18".into(), "amount".into()), Value::F64(2.0), "t0");
    draft.capture_groups(&base);
    let (_, dropped) = draft.rebase(&newer);
    assert!(dropped.iter().any(|(l, why)| l == "2026-09-18#2" && why.contains("2 → 3")), "{dropped:?}");
    assert_eq!(draft.edits.len(), 1, "the 2026-12-18 edit survives");
}

#[test]
fn rebase_without_captured_groups_applies_no_guard() {
    let newer = flat_model(&["2026-09-18", "2026-09-18#2", "2026-09-18#3"]);
    let mut draft = Draft::default();
    draft.set((1, 3), ("2026-09-18#2".into(), "amount".into()), Value::F64(1.0), "t0");
    let (_, dropped) = draft.rebase(&newer);
    assert!(dropped.is_empty(), "{dropped:?}");
}

#[test]
fn captured_groups_round_trip_through_the_session() {
    let base = flat_model(&["2026-09-18", "2026-09-18#2"]);
    let mut draft = Draft::default();
    draft.set((1, 3), ("2026-09-18#2".into(), "amount".into()), Value::F64(1.0), "t0");
    draft.capture_groups(&base);
    let back = Draft::from_toml(&draft.to_toml());
    assert_eq!(back.groups, draft.groups);
}
```

(If no `flat_model` helper exists in `test_fixtures.rs`, add one there that builds a `DIVIDEND` snapshot with the given `dividend_id` labels and calls `MatrixModel::build(&snapshot, &DIVIDEND, &Draft::default())`.)

Tile window test: with a dividend panel on a base, edit a cell of a `#2` row, deliver a newer generation that adds a `#3` to that date, run `:rebase` through the command line; the notice names the row and the edit is gone.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-marketdata rebase captured_groups`
Expected: FAIL (`capture_groups` not found).

- [ ] **Step 3: Implement**
  - Field `groups: BTreeMap<String, usize>` (pub, like `rows`); `revert` clears it.
  - `capture_groups`: compute the set of labels touched (`self.labels` values' row labels plus `Deleted` keys in `self.rows`), map to `group_of`, keep `group_sizes(base)` entries for those groups only.
  - In `rebase`, before resolving cell edits and `Deleted` marks: `let now = group_sizes(model_of_newer);` and for every touched label whose `group_of(label)` is in `self.groups` with `was != now.get(g).copied().unwrap_or(0)`, drop it with the reason above. After the rebase, `self.groups` is `capture_groups(model_of_newer)`'s result (the newer document is the new base).
  - `to_toml` writes `groups` as a table `{ "<group>" = <size> }` when non-empty; `from_toml` reads it (absent → empty).
  - Tile: call `self.draft.capture_groups(&base_model)` (a) at the top of `fn rebase` and of the policy-rebase path in `apply`, where the base model is the clean model of the painted base (`MatrixModel::build(base_snapshot, spec, &Draft::default())`, the same clean-model rule the existing rebase uses); (b) before `to_toml` in the session write, when the draft is `Editing` or `Behind` (painted = base).

- [ ] **Step 4: Run tests**

Run: `cargo test -p geode-marketdata`
Expected: PASS.

- [ ] **Step 5: Harness entry**

```sh
# A same-day group whose size changed has shifted ordinals. Mutated to
# skip the check, an edit keyed `<date>#2` lands on a different dividend.
run_mutation "draft: rebase refuses a changed same-day group" \
  crates/geode-marketdata/src/core/draft.rs \
  '<the `was != now` comparison line>' \
  '<same line with the condition replaced by false>' \
  geode-marketdata \
  rebase_refuses_edits_in_a_same_day_group_that_changed_size
```

Commit, then run `zsh scripts/mutation-check.sh "same-day group"`. Expected: caught.

- [ ] **Step 6: Commit**

```bash
git commit -am "marketdata: rebase refuses edits in a same-day dividend group that changed size

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Assembly and echo comparison (pure)

**Files:**
- Create: `crates/geode-marketdata/src/core/upload.rs` (register in `core/mod.rs`)
- Modify: `crates/geode-marketdata/src/core/matrix.rs` only if the pivot does not retain typed column-axis values (see Step 3)
- Test: in `upload.rs`, using `core/test_fixtures.rs`

**Interfaces:**
- Consumes: `MatrixModel` (rows in painted order, `Cell.value: Option<Value>`, `RowState`), `PanelSpec`, `Draft`, `geode_core::snapshot::Snapshot`.
- Produces:

```rust
/// The document an upload sends: the base generation with the draft
/// applied, in painted order. `model` is the PAINTED model (base + draft);
/// `snapshot` is the base generation it was built from (attributes are
/// read typed from it, then overridden by `draft.attrs`).
pub fn assemble(
    snapshot: &Snapshot,
    spec: &PanelSpec,
    model: &MatrixModel,
    draft: &Draft,
) -> Result<DocumentRows, String>;

/// How many rows differ between what was sent and a delivered document,
/// comparing every column except a `Minted` row axis's label; `0` means
/// the echo confirms the upload. `f64` within one ULP, everything else exact.
/// Attributes that differ count as one extra row.
pub fn echo_differs(spec: &PanelSpec, sent: &DocumentRows, delivered: &DocumentRows) -> usize;
```

**Assembly rules** (from the spec §6 and amendment 3):
- `key` = `model.key`.
- `attributes`: one per `spec.header` attr, in the dataset's attribute order (the order the kind expects: CVI `anchor_date, spot_ref`; DIVIDEND `currency, schedule_date`), typed by `HeaderAttr.ty`, value = `draft.attrs[column]` if present, else the snapshot's value for that column in its first row.
- Rows = `model.rows` in order, skipping `RowState::Deleted`.
- Row axis (`spec.rows.column`): `RowIdentity::Minted` → `Column::Utf8(labels)`; `RowIdentity::Typed(ColumnType::Date)` → parse each label `%Y-%m-%d` (a label that does not parse is `Err("row '<label>': not a date")`).
- `Columns::Values(cols)`: one value column per `ValueColumn` in spec order; each cell's `value` must be `Some` (else `Err("row '<label>': <column> is empty")`) and its tag must equal `ty` (else `Err("row '<label>': <column> is <tag>, declared <ty>")`).
- `Columns::Axis(col)`: long form, one output row per (painted row × value column); axes `[row axis, (col, typed column value)]`; the value column is the pivot's value plus every slice value repeated across the row's nodes, in the dataset's value order (CVI `param, forward, atm, skew`). Type-check each as above.
- The output must satisfy `DocumentRows::validate` for the dataset.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_empty_draft_assembles_the_base_document_exactly() {
    for (spec, doc) in [(&CVI, fixture_cvi_rows()), (&DIVIDEND, fixture_dividend_rows())] {
        let snapshot = snapshot_of(spec, &doc);
        let model = MatrixModel::build(&snapshot, spec, &Draft::default()).unwrap();
        assert_eq!(assemble(&snapshot, spec, &model, &Draft::default()).unwrap(), doc, "{}", spec.kind);
    }
}

#[test]
fn a_dividend_draft_assembles_edits_deletes_and_inserts_in_painted_order() {
    // base rows A, B, C; edit B.amount, delete C, insert new-1 after A with every cell set.
    // expected labels: [A, new-1, B]; B.amount edited; attributes from the snapshot.
}

#[test]
fn assembly_refuses_an_empty_cell_and_a_wrong_tag_naming_the_row() { /* … */ }

#[test]
fn echo_ignores_the_minted_label_and_counts_differing_rows() {
    let sent = fixture_dividend_rows();
    let mut echoed = sent.clone();
    echoed.axes[0].1 = Column::Utf8(vec!["x".into(); sent.rows()]);
    assert_eq!(echo_differs(&DIVIDEND, &sent, &echoed), 0);
    if let Column::F64(v) = &mut echoed.values[3].1 { v[0] += 1.0; }
    assert_eq!(echo_differs(&DIVIDEND, &sent, &echoed), 1);
}

#[test]
fn echo_accepts_one_ulp_and_refuses_two() { /* f64::from_bits(bits + 1) passes, +2 fails */ }

#[test]
fn echo_compares_a_typed_label_on_cvi() { /* a changed term counts */ }
```

Write the bodies in full. `snapshot_of` and the fixture row builders: reuse whatever `test_fixtures.rs` already provides for CVI and DIVIDEND snapshots; if the fixtures build snapshots without a `DocumentRows`, add `fixture_*_rows()` returning the `DocumentRows` and derive the snapshot from it in one helper, so the round trip compares like with like.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-marketdata upload`
Expected: FAIL (module missing).

- [ ] **Step 3: Implement**

Read `pivot` and `flatten` in `matrix.rs` first. If the pivot keeps column labels only as text, add `pub column_values: Vec<Value>` to `MatrixModel` (typed node values, same order as `columns` after `slice_columns`; empty for flat), filled by the pivot from the snapshot. Do not parse painted text back into numbers. Then implement `assemble` and `echo_differs` per the rules above. `echo_differs` compares row by row in order; a length difference counts each unmatched row.

- [ ] **Step 4: Run tests**

Run: `cargo test -p geode-marketdata`
Expected: PASS.

- [ ] **Step 5: Harness entries**
  - `"upload: assembly drops deleted rows"`: mutate the `RowState::Deleted` skip. Test: `a_dividend_draft_assembles_edits_deletes_and_inserts_in_painted_order`.
  - `"upload: assembly checks the declared type"`: mutate the tag check to accept. Test: `assembly_refuses_an_empty_cell_and_a_wrong_tag_naming_the_row`.
  - `"upload: echo ignores the minted label"`: mutate the Minted skip. Test: `echo_ignores_the_minted_label_and_counts_differing_rows`.
  - `"upload: echo tolerates one ulp only"`: mutate the ULP bound to 2. Test: `echo_accepts_one_ulp_and_refuses_two`.

Commit, run `zsh scripts/mutation-check.sh "upload:"`. Expected: all caught.

- [ ] **Step 6: Commit**

```bash
git commit -am "marketdata: assemble an upload from base + draft; compare an echo with what was sent

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: `egress.toml` typed reader (`geode-core`)

**Files:**
- Create: `crates/geode-core/src/egress_config.rs` (register in `lib.rs`)
- Reference: `crates/geode-core/src/source_config.rs` (`from_doc` L321, `diag` L245–261)
- Modify: `crates/geode-core/src/config/merge.rs` `atomic_depth` (L15–36): `"egress"` merges at depth 1, like `"sources"`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressSpec {
    pub name: String,
    pub adapter: String,
    /// document dataset name → address template (may contain `{key}`), TOML order.
    pub documents: Vec<(String, String)>,
}

impl EgressSpec {
    /// The address for one document key: `{key}` replaced by the parts joined with `/`.
    pub fn address(&self, document: &str, key: &[String]) -> Option<String>;
}

pub fn from_doc(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<EgressSpec>, Vec<Diagnostic>);
```

Diagnostics (path `egress.<name>` / `egress.<name>.<key>`, message `"egress '<name>': …"`, severity error, the target is dropped):
- `adapter` missing or not a string;
- `documents` missing, not a table, or empty;
- a `documents` key naming no dataset whose family is document (look up in `schema`);
- an address that is not a string.

- [ ] **Step 1: Failing tests:** one per diagnostic (assert path and that the target is absent), a valid doc with two targets preserving order, and `address` with and without `{key}` (a two-part key joins with `/`).
- [ ] **Step 2:** `cargo test -p geode-core egress`. Expected: FAIL.
- [ ] **Step 3:** Implement, mirroring `source_config::from_doc`'s shape and `diag` helper.
- [ ] **Step 4:** `cargo test -p geode-core`. Expected: PASS.
- [ ] **Step 5:** Add harness entries `"egress config: an unknown document is dropped"` and `"egress config: {key} is substituted"`. Commit, then run them.
- [ ] **Step 6: Commit** `core: typed egress.toml reader`.

---

### Task 7: Upload request, per-target workers, `DataEvent::Upload` (`geode-data`)

**Files:**
- Create: `crates/geode-data/src/egress.rs` (register in `lib.rs`; re-export `UploadParams`, `UploadOutcome`)
- Modify: `crates/geode-data/src/handle.rs` (`Request` L31–62, `fn serve` match L324–385, add `upload`)
- Modify: `crates/geode-data/src/service.rs` (`DataServiceConfig` L43–61 gains `pub egress: Vec<EgressSpec>`; `DataEvent` gains `Upload(UploadOutcome)`; `DataService` holds the workers)
- Modify: every `DataServiceConfig { .. }` construction (grep), adding `egress: Vec::new()` where there is none

**Interfaces:**

```rust
pub struct UploadParams {
    pub key: QueryKey,
    pub tag: u64,
    pub target: String,
    pub document: String,
    pub rows: DocumentRows,
}
#[derive(Debug, Clone, PartialEq)]
pub struct UploadOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub target: String,
    pub result: Result<(), String>,
}

/// Drop targets whose adapter is unknown or has no egress side, with a
/// diagnostic each (path `egress.<name>.adapter`). Called by geode-app at
/// startup; the survivors go into `DataServiceConfig::egress`.
pub fn resolve(specs: Vec<EgressSpec>, adapters: &AdapterRegistry) -> (Vec<EgressSpec>, Vec<Diagnostic>);

impl DataHandle {
    pub fn upload(&self, params: UploadParams) -> bool; // self.send(Request::Upload(params))
}
```

**Behaviour:**
- `DataService::open` spawns one worker thread per `EgressSpec` (named `geode-egress-<name>`), owning a `Box<dyn Egress>` from `adapters.get(&spec.adapter).and_then(|a| a.egress())` and a `sync_channel::<Job>(8)`.
- `serve`'s `Request::Upload(p)` arm calls `service.upload(p)`:
  1. unknown target → sink `Err("egress '<t>': unknown target")`;
  2. no address for `p.document` → `Err("egress '<t>': does not accept <document>")`;
  3. `documents.get(&p.document)` missing → `Err("egress '<t>': no document kind <document>")`;
  4. `kind.write(&p.rows)` error → `Err("egress '<t>': <message>")`;
  5. `try_send(Job { bytes, address, key, tag })`, where `Full` → `Err("egress '<t>': queue full")`.
- The worker calls `egress.upload(&address, bytes)` and sinks `DataEvent::Upload` with `Ok(())` or `Err("egress '<t>': <AdapterError>")`. Uploads to one target run one at a time, in order.
- Every `Err` path above also emits exactly one `DataEvent::Upload` (never silence).
- Shutdown: dropping the job senders ends the workers; join them in the existing shutdown path.
- Log one `info!(target: "geode::ingest", …)` per upload result, naming target, document, key and outcome.

- [ ] **Step 1: Failing tests** (`egress.rs` tests, using the `ChannelAdapter` fixture from `adapter/channel.rs` tests and a stub `DocumentKind` whose `write` returns the rows' key as bytes, or errors on demand):
  - `an_upload_reaches_the_target_address_and_answers_ok`: subscribe a `ChannelAdapter` to `marketdata/dividend/>`, upload with the address template `marketdata/dividend/{key}`, assert the subscriber sees `marketdata/dividend/XYZ` with the bytes and the sink gets `Upload { result: Ok(()) }`.
  - `a_write_error_an_unknown_target_and_a_closed_bus_each_answer_err_naming_the_target`.
  - `uploads_to_one_target_run_in_submission_order`.
  - `resolve_drops_an_unknown_adapter_and_one_without_egress`.
  - `handle.rs`: `upload_is_refused_when_the_request_channel_is_full` (fill a `for_tests` handle's channel to `REQUEST_BOUND`, assert `upload` answers `false`).
- [ ] **Step 2:** `cargo test -p geode-data egress upload`. Expected: FAIL.
- [ ] **Step 3:** Implement. Fix every exhaustive `DataEvent` match the compiler names outside `geode-app` (`geode-app` is Task 8).
- [ ] **Step 4:** `cargo test -p geode-data`. Expected: PASS. `cargo check --workspace` may fail only in `geode-app` (Task 8).
- [ ] **Step 5:** Add harness entries `"egress: a write error answers Err"`, `"egress: queue full answers Err"` and `"egress: resolve drops an adapter without egress"`. Commit, run `zsh scripts/mutation-check.sh "egress:"`.
- [ ] **Step 6: Commit** `data: upload requests on a worker per egress target`.

---

### Task 8: Shell `Delivery::Upload` and app wiring

**Files:**
- Modify: `crates/geode-shell/src/module.rs` (`Delivery` L39–57, `key()` L65, test occupants L474/L762)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (router L67, keyed arm L94), `crates/geode-shell/src/shell/tests/occupants.rs:66`
- Modify: `crates/geode-blotter/src/content.rs:125`, `crates/geode-marketdata/src/content.rs:216`, `crates/geode-timeseries/src/content.rs:168`, `crates/geode-diagnostics/src/lib.rs:100`
- Modify: `crates/geode-app/src/bridge.rs` (drain L529; config read near L74–77), `crates/geode-app/src/events.rs` (`key` L31–51, `tag` L53–62), `crates/geode-app/src/main.rs` (registry L143–145, service config), `crates/geode-app/src/demo.rs` (`layer` L50–82)
- Modify: `crates/geode-shell/src/shell/hot_reload.rs:187–196` (add `"egress"` to the restart-required docs)

**Interfaces:**

```rust
// geode-shell/src/module.rs: plain types, the shell never names geode-data.
pub struct UploadDelivery {
    pub key: QueryKey,
    pub tag: u64,
    pub target: String,
    pub result: Result<(), String>,
}
pub enum Delivery { /* … */ Upload(UploadDelivery) }
// Delivery::key(): Delivery::Upload(u) => Some(u.key)
```

- The marketdata factory gains the eligible targets: `MarketDataFactory` (constructed in `bridge.rs`/`main.rs`) takes `egress: Arc<Vec<(String, Vec<String>)>>` (target name → accepted documents, TOML order) and passes `targets_for(spec.document)` (a `Vec<SharedString>`) into `MarketDataTile::new`. Find the factory and tile constructors at `crates/geode-marketdata/src/content.rs:260` and `:356` and `tile.rs:567`.

- [ ] **Step 1: Failing tests:**
  - shell: `an_upload_delivery_reaches_its_tile_and_no_other` (copy the `Query` routing test in `shell/tests/occupants.rs`).
  - app: `events.rs` `key`/`tag` for `DataEvent::Upload`; `demo.rs`: the demo layer's `egress` doc reads into two documents for `sophis` via `egress_config::from_doc` and `resolve` keeps it against a registry with `demo_bus`.
  - hot_reload: an `egress` change lands in the restart list (copy the `sources` test).
- [ ] **Step 2:** `cargo test -p geode-shell occupants hot_reload && cargo test -p geode-app`. Expected: FAIL.
- [ ] **Step 3:** Implement:
  - `Delivery::Upload` routed on its key (add it to the keyed arm). Every other occupant ignores it with an explicit arm; marketdata's arm calls `t.deliver_upload(u, cx)` (a stub that does nothing until Task 9, with a `// Task 9` note removed there).
  - `bridge.rs`: `DataEvent::Upload(o)` becomes `Delivery::Upload(UploadDelivery { key: o.key, tag: o.tag, target: o.target, result: o.result })`.
  - `main.rs`/`bridge.rs`: read `config.doc("egress")` (absent → empty), `egress_config::from_doc`, then `geode_data::egress::resolve` against the adapter registry. Route the diagnostics the way source diagnostics are routed. Put the survivors into `DataServiceConfig.egress` and their `(name, documents)` into the marketdata factory.
  - `demo.rs` layer: add an `egress` doc:

```toml
[sophis]
adapter = "demo_bus"
[sophis.documents]
cvi_params = "marketdata/cvi/{key}"
dividend_schedule = "marketdata/dividend/{key}"
```

   Register it as `("egress", egress)`. Check the doc's exact top-level shape against how `sources` is written in the same function (whether the table is prefixed `sources.` or the doc name is implicit) and match it.
  - `hot_reload.rs`: `"egress"` joins `"sources"` and `"datasets"`.
- [ ] **Step 4:** `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`. Expected: PASS.
- [ ] **Step 5:** Harness: `"shell: an upload delivery routes by key"` (mutate its `key()` arm to `None`). Commit, then run it.
- [ ] **Step 6: Commit** `shell+app: Delivery::Upload, egress.toml wiring, demo sophis target`.

---

### Task 9: The panel's `:upload`, confirm and outcome

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs` (`command` L3793, match L3828–3855, menu verb L1978–1981, `upload_built` L2998, `notice` L499, `new` L567)
- Modify: `crates/geode-marketdata/src/commands.rs` (`Command::Upload` → `Upload(Option<String>)`, parse L154, `completions` L202)
- Modify: `crates/geode-marketdata/src/core/menu.rs:94–107` (Upload enabled when the draft is uploadable)
- Modify: `crates/geode-marketdata/src/content.rs` (`deliver_upload`)

**Interfaces:**
- Consumes: `assemble` (Task 5), `DataHandle::upload`/`UploadParams` (Task 7), `UploadDelivery` (Task 8), `DraftState::Sent { at }` (Task 3), `targets: Vec<SharedString>` (Task 8).
- Produces on the tile: `pending_upload: Option<PendingUpload { target: String, rows: DocumentRows, prompt: SharedString }>`, `sent: Option<DocumentRows>`, `upload_tag: u64`, `upload_error: Option<SharedString>`.

**Behaviour** (spec §6, amended):
- `:upload [target]`: resolve the target (none eligible → `Err("no egress target accepts <document>")`; several and no argument → `Err("upload to which target? <a>, <b>")`; an argument not eligible → `Err("<t> does not accept <document>")`).
- Refusals, in order: clean draft → `"nothing to upload"`; `Behind` → `"rebase or revert first: an upload must be of a document you have seen whole"`; `Sent` with no edits since → `"already sent"`; `incomplete_rows > 0` → `"<n> rows incomplete"`; `assemble` error → its message.
- Otherwise arm `pending_upload`. The command line (or the header's notice line, whichever the tile uses for a transient prompt; read how `Err` text is shown) shows `upload <n> cells, <a> rows added, <d> removed of <key> to <target>? (y/n)`.
- While armed, the tile's key handling takes the next keystroke before anything else: bare `y` submits; any other key cancels with notice `"upload cancelled"` and is consumed. Focus leaving the tile cancels. Use the production keystroke path: find where the tile intercepts keys for its insert mode or the choice popup, and add the armed-confirm check first.
- Submit: `upload_tag += 1`; `info!(target: "geode::ingest", key, target, cells, rows_added, rows_removed, "upload submitted")`; `self.sent = Some(rows.clone())`; `self.data.upload(UploadParams { key: QueryKey(self.id.0), tag, target, document: spec.document.into(), rows })`. On `false`: notice `"upload refused: the data service is busy or gone"`, `sent = None`.
- `deliver_upload(u)`: ignore unless `u.tag == self.upload_tag`. `Ok` → `draft.state = Sent { at: Utc::now().to_rfc3339() }`, repaint. `Err(e)` → `upload_error = Some(format!("upload failed: {e}"))` shown in the header until the next edit or upload; `sent = None`; state unchanged (`Editing`).
- Menu: `upload_built: true`; the Upload row dispatches `:upload` with no argument.
- Completions: `upload` is offered as a verb; after `upload `, the eligible target names.

- [ ] **Step 1: Failing window tests** (tile tests module, `DataHandle::for_tests()` to capture `Request::Upload`):
  - `upload_is_refused_on_a_clean_draft`, `…_behind_draft`, `…_with_an_incomplete_row`, `…_with_no_eligible_target`.
  - `upload_arms_a_confirm_and_y_submits_the_assembled_document`: edit a cell, type `:upload⏎`, assert the prompt text, simulate the keystroke `y`, assert the captured `Request::Upload` carries the target, the document name and `assemble`'s rows.
  - `any_other_key_cancels_the_confirm_and_is_consumed`: `n` cancels, and `j` cancels without moving the cursor.
  - `focus_leaving_the_tile_cancels_the_confirm`.
  - `an_ok_outcome_enters_sent_and_the_header_reads_sent_hhmm`.
  - `an_err_outcome_keeps_editing_and_shows_the_error`.
  - `a_stale_upload_tag_is_ignored`.
  - `commands.rs`: `upload` parses with and without a target; completions after `upload ` list targets.
- [ ] **Step 2:** `cargo test -p geode-marketdata upload`. Expected: FAIL.
- [ ] **Step 3:** Implement per the behaviour above.
- [ ] **Step 4:** `cargo test -p geode-marketdata`, then `cargo check -p geode-shell --features test-support --all-targets`. Expected: PASS.
- [ ] **Step 5:** Harness: `"panel: upload refused while Behind"`, `"panel: the confirm consumes a non-y key"`, `"panel: a stale upload tag is ignored"`. Commit, run `zsh scripts/mutation-check.sh "panel: upload"` and the other two.
- [ ] **Step 6: Commit** `marketdata: :upload with a y/n confirm; Sent on Ok, inline error on Err`.

---

### Task 10: The echo

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs` (`fn apply` L1119, the `on_delivered` call L1130, the policy branch L1168–1219, `fn rebase` L3577)
- Modify: `crates/geode-marketdata/src/core/draft.rs` (`on_delivered`: `Sent` stays `Sent`; `rebase` accepts `Sent`)
- Modify: `crates/geode-marketdata/src/header.rs` (echo notices)

**Behaviour** (spec §7, amended):
- In `apply`, when `draft.state` is `Sent { at }` and the delivered source time differs from `draft.base`: build `clean = MatrixModel::build(snapshot, spec, &Draft::default())`, `delivered = assemble(snapshot, spec, &clean, &Draft::default())`, `n = echo_differs(spec, self.sent.as_ref()?, &delivered)`.
  - `n == 0`: `draft.revert()`, `sent = None`, the panel follows the new generation, notice `format!("sent {}, confirmed {}", local_hhmm(at), local_hhmm(now))`.
  - `n > 0`: the draft stays `Sent`, the panel keeps painting the base (the same staging `Behind` uses), notice `format!("echo differs ({n} rows)")`. It does NOT enter `Behind`; the update policy is not applied to a `Sent` draft.
  - `self.sent` is `None` (not expected, but possible after a refused submit): treat as `Behind` via the existing path.
- `:rebase` from `Sent` runs the normal rebase and yields `Editing`; `:revert` from `Sent` clears as usual. `sent = None` on both.
- The session writes a `Sent` draft's edits as usual (state is not persisted; it restores as `Editing`).

- [ ] **Step 1: Failing window tests:**
  - `a_matching_echo_clears_the_draft_and_says_confirmed`: upload, deliver `Ok`, then deliver a newer generation whose rows equal the sent rows (build it from `sent`); assert the draft is clean, the model paints the new generation, and the notice contains `confirmed`.
  - `a_differing_echo_keeps_sent_and_counts_rows`: the delivered generation changes one amount; assert `Sent`, `echo differs (1 rows)`, the base still painted.
  - `rebase_from_sent_yields_editing`.
  - `a_dividend_echo_matches_despite_reminted_labels`: an inserted `new-1` row is sent; the echo carries it under a minted date id; assert confirmed.
  - `a_sent_draft_restores_as_editing`.
- [ ] **Step 2:** `cargo test -p geode-marketdata echo`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5:** Harness: `"panel: a matching echo clears the draft"` (mutate `n == 0` to `false`), `"panel: a differing echo keeps Sent"` (mutate so it reverts regardless). Commit, then run them.
- [ ] **Step 6: Commit** `marketdata: the echo clears a matching upload and holds a differing one`.

---

### Task 11: End-to-end demo check, docs, final gates

**Files:**
- Modify: `docs/current/data-path.md` (L158/174: egress workers, request, failure semantics), `docs/current/features.md` (L64–65: replace "not built" with upload, confirm, echo, and the same-day reorder limitation), `docs/current/configuration.md` (`egress.toml`, restart-required), `docs/current/performance.md` only if a hot path changed
- Modify: `crates/geode-data/README.md`, `crates/geode-marketdata/README.md`, `crates/geode-documents/README.md` (if not done in Task 2), `crates/geode-core/README.md`

- [ ] **Step 1: Headless end-to-end test** in `geode-app` (beside `demo_bus.rs` tests): open a `DataService` with the demo `ChannelAdapter`, a `dividend` source subscribed to `marketdata/dividend/>`, and the `sophis` target. Submit `Request::Upload` with a dividend document for `XYZ`. Assert `DataEvent::Upload(Ok)` and then a `DataEvent::Published` for `dividend_schedule` batch `XYZ`. Then run a document request and assert its rows equal the uploaded ones except the minted ids.
- [ ] **Step 2:** `rm -rf $TMPDIR/geode-demo/*` and `cargo build -p geode-app`. (A real-window run is a display check for the user; do not claim it.)
- [ ] **Step 3:** Docs per the file list. Describe behaviour, failure semantics and limitations; no task chronology.
- [ ] **Step 4: Gates**

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```

Expected: all clean.

- [ ] **Step 5: Commit** `docs: market-data egress in the current guides and crate READMEs`.

---

## Display checks (for the user, after merge)

1. The `:upload` confirm prompt's placement and legibility.
2. The header's `sent 14:09` then `sent 14:09, confirmed 14:10` on `--demo` (the demo bus echoes within the coalesce window).
3. `echo differs (N rows)` when the demo generator republishes over an upload (it will: the generator does not know about the trader's edits).
