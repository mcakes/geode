# Market-Data Panel Header Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the CVI panel the approved dense header — "underlying" vocabulary, a dirty dot, an inline attribute strip the cursor can enter and edit, concise state — plus a tile-owned popup carrying the action list and the underlying picker, with per-kind verbs reserved.

**Architecture:** Everything lands in `crates/geode-marketdata`. Pure cores first (`core::cursor`, `core::menu`, `Draft.attrs`, `parse_attr`, `HeaderCell`), each tested without a window; the tile (`tile.rs`) wires them behind the existing `dispatch`/`command`/`render` doors; the popup is a gpui `deferred(anchored(..))` element the tile owns, with keys arriving through a new `mode == menu` context in the module's keymap fragment. No shell change is needed.

**Tech Stack:** Rust, gpui (pinned zed checkout `7960b2a`), gpui-component (pinned `0e2fb7a`), `geode_shell::listfilter::rank`, `geode_core::colour`, proptest, the mutation harness `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-14-geode-market-data-panel-header-design.md` (this plan argues from it; §-references below are to it unless prefixed "documents spec", which is `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md`).

## Global Constraints

- Geode is a lens, not a brain (`docs/PHILOSOPHY.md`): reanchor/recalc forward are greyed rows only; no computation is added anywhere.
- Nothing formats or allocates in `render` beyond `debug_selector` closures: every string the header paints is prepared in `rebuild_chrome` (the tile's existing rule — `changed()` is the one door).
- Every action is keyboard-reachable: every menu row and the menu itself is a registered `marketdata::*` action bound in `DEFAULT_KEYMAP`.
- A fragment predicate is a plain conjunction (`marketdata && mode == menu`); never `!`, `||`, `(`.
- An occupant that opens a focused `InputState` must `window.blur(cx)` then drop it — both halves, that order (`close_editor` is the existing implementation; the picker reuses it).
- The mouse selects; it never opens an editor (user ruling 2026-09-14). A click on an attribute value moves the cursor only.
- Times shown to a trader are the local clock (`chrono::Local`), `HH:MM` for state text, `HH:MM:SS` for the source time.
- The trader-facing word is **underlying**; the data tier's word stays **key** (`PanelSpec`, `DocumentParams`, storage, `MarketDataTile.key`).
- Text colours must clear `geode_core::colour::READABLE_RATIO` (3.0) on every bundled theme; reuse `FlooredTones` for warning/danger text and `cell_paint`'s fills.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p geode-marketdata` green at every commit; `zsh scripts/mutation-check.sh --anchors-only` exits 0 before the final merge.
- Harness entries: every rule a task adds gets a `run_mutation` entry naming its test (6th argument). Bump the count in `CLAUDE.md`'s command table.
- Commit messages end with the session's attribution trailer.

---

## File map

| File | Responsibility after this plan |
|---|---|
| `crates/geode-marketdata/src/core/spec.rs` | `PanelSpec` gains `HeaderAttr { column, label, ty }` and `actions: &[KindAction]` |
| `crates/geode-marketdata/src/core/matrix.rs` | `MatrixModel.header: Vec<HeaderCell>` prepared from snapshot + draft |
| `crates/geode-marketdata/src/core/draft.rs` | `Draft.attrs`, `set_attr`, `parse_attr`, `attr_text`, `DraftBadge`, `badge()` (replaces `summary()`) |
| `crates/geode-marketdata/src/core/cursor.rs` (new) | `Cursor` enum and every motion, pure |
| `crates/geode-marketdata/src/core/menu.rs` (new) | `MenuRow`, `MenuInputs`, `rows()`, pure |
| `crates/geode-marketdata/src/commands.rs` | `underlying`/`key` alias, `set`, `menu` verbs |
| `crates/geode-marketdata/src/content.rs` | `ACTIONS` grows; fragment gains `.`/`u` and the `mode == menu` block; registers kind actions |
| `crates/geode-marketdata/src/header.rs` (new) | The prepared header (`HeaderModel`) and its render function |
| `crates/geode-marketdata/src/popup.rs` (new) | `Popup::{Menu, Picker}` state and render (anchored/deferred) |
| `crates/geode-marketdata/src/tile.rs` | Wiring: cursor, attribute editor, popup keys, `:set`, session |
| `crates/geode-marketdata/src/delegate.rs` | unchanged except `Cursor::Cell` mirroring |
| `scripts/mutation-check.sh` | new entries per task |
| `CLAUDE.md`, the spec's §11 "As built" | Task 8 |

`tile.rs` is 4,457 lines; Tasks 4 and 6 move the header and popup paint into their own files rather than growing it.

---

### Task 1: The vocabulary — `:underlying`, `:key` alias, session key

**Files:**
- Modify: `crates/geode-marketdata/src/commands.rs`
- Modify: `crates/geode-marketdata/src/tile.rs` (`command`, `serialize`, `new`'s restore, `rebuild_chrome`'s two key strings, `set_key`'s notice)
- Test: `crates/geode-marketdata/src/commands.rs` tests module; `crates/geode-marketdata/src/tile.rs` tests module

**Interfaces:**
- Produces: `Command::Key(Vec<String>)` unchanged in shape; `commands::VERBS` lists `underlying` first and `key` is parsed as an alias but NOT offered as a completion. Session table key `"underlying"` written; `"key"` still read.

- [ ] **Step 1: Write the failing parser tests**

Append to the `tests` module at the bottom of `commands.rs`:

```rust
    #[test]
    fn underlying_parses_like_key_and_key_stays_an_alias() {
        assert_eq!(parse("underlying SPX.Z"), Ok(Command::Key(vec!["SPX.Z".into()])));
        assert_eq!(parse("key SPX.Z"), Ok(Command::Key(vec!["SPX.Z".into()])));
        assert_eq!(parse("underlying"), Err("usage: underlying <value>".into()));
    }

    #[test]
    fn completions_offer_underlying_and_never_the_key_alias() {
        let verbs = completions("", 0, &[], false);
        assert_eq!(verbs[0], "underlying");
        assert!(!verbs.iter().any(|v| v == "key"), "{verbs:?}");
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p geode-marketdata commands::tests::underlying commands::tests::completions_offer_underlying`
Expected: FAIL — `parse("underlying SPX.Z")` is `Err("unknown command 'underlying'")`.

- [ ] **Step 3: Implement**

In `commands.rs`:

```rust
/// Every verb, in the order completions offer them. `key` is a silent
/// alias of `underlying` (the trader-facing word, spec §3) and is not
/// listed: it parses, it is not taught.
const VERBS: [&str; 8] = ["underlying", "revert", "bump", "rebase", "discard", "upload", "set", "menu"];
```

(`set` and `menu` are parsed in Tasks 5 and 6; add them to `VERBS` now so the table is written once, and parse them as `Err("set is not built yet")`/`Err("menu is not built yet")` placeholders that those tasks replace.)

In `parse`, replace the `Some("key") => {` arm's head with:

```rust
        Some(verb @ ("underlying" | "key")) => {
            let value = words
                .next()
                .ok_or_else(|| format!("usage: {verb} <value>"))?;
```

and keep the body; update the two error strings inside it from "document key" to "an underlying is one word; parts are separated by …" and "'{value}' has an empty part". Add:

```rust
        Some("set") => Err("set is not built yet".to_string()),
        Some("menu") => Err("menu is not built yet".to_string()),
```

In `tile.rs::command`, the catalog re-request gate becomes:

```rust
        if matches!(line.split_whitespace().next(), Some("underlying" | "key")) {
            self.request_catalog(cx);
        }
```

In `serialize`, write `"underlying"` instead of `"key"`. In `new`, where `restored` is read (`let key = restored.and_then(|t| t.get("key"))…`), read `t.get("underlying").or_else(|| t.get("key"))`. In `rebuild_chrome`, the no-key chip text becomes `format!("{} — no underlying — :underlying <value>", self.spec.title)` (Task 4 restyles it; the string changes now so the test below can pin the word). In `set_key`'s refusal keep the text (`:revert first`).

Update the existing test `serialize_round_trips_key_and_draft` to assert the table holds `"underlying"`, and add:

```rust
    #[gpui::test]
    fn a_session_written_with_key_still_restores(cx: &mut gpui::TestAppContext) {
        let mut t = toml::Table::new();
        t.insert("key".into(), toml::Value::Array(vec![toml::Value::String("SPX.Z".into())]));
        let (h, mut vcx) = open_with(cx, Some(t));
        h.visible(&mut vcx, true);
        let req = h.document_request().expect("a restored underlying requests its document");
        assert_eq!(req.document_key, vec!["SPX.Z".to_string()]);
    }
```

(`document_key` is the field name on `DocumentParams`; check `geode_core::query::DocumentParams` and use its actual field.)

- [ ] **Step 4: Run the crate's tests**

Run: `cargo test -p geode-marketdata`
Expected: PASS. Fix any test that asserted the literal `no key` string.

- [ ] **Step 5: Harness entry and commit**

Append to `scripts/mutation-check.sh` before the `if [[ -n "$changed_ref" ]]` tail:

```bash
# ---- Panel header: vocabulary (spec 2026-09-14 §3) ----------------------
run_mutation "mdheader: a session written with key still restores" \
  crates/geode-marketdata/src/tile.rs \
  'restored.and_then(|t| t.get("underlying").or_else(|| t.get("key")))' \
  'restored.and_then(|t| t.get("underlying"))' \
  geode-marketdata \
  a_session_written_with_key_still_restores
```

(Adjust the anchor to the exact expression as written in `new`; run `zsh scripts/mutation-check.sh "mdheader:"` and confirm `caught`.)

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: the underlying is the trader's word; :key stays an alias"
```

---

### Task 2: `HeaderAttr` on the spec, `HeaderCell` on the model

**Files:**
- Modify: `crates/geode-marketdata/src/core/spec.rs`
- Modify: `crates/geode-marketdata/src/core/matrix.rs` (`MatrixModel.header`, `header_of`)
- Modify: `crates/geode-marketdata/src/tile.rs` (`rebuild_chrome` reads `.label`/`.text`)
- Test: `matrix.rs` tests

**Interfaces:**
- Produces:
  ```rust
  // spec.rs
  pub struct HeaderAttr { pub column: &'static str, pub label: &'static str, pub ty: ColumnType }
  // PanelSpec.header: &'static [HeaderAttr]
  // matrix.rs
  pub struct HeaderCell { pub column: SharedString, pub label: SharedString, pub text: SharedString, pub edited: bool }
  // MatrixModel.header: Vec<HeaderCell>
  ```
- `edited` is always `false` in this task; Task 3 fills it from the draft.

- [ ] **Step 1: Failing test**

In `matrix.rs` tests, find the test asserting `.header` (`"the header reads the document-level attributes off row 0"`) and change its assertion to the new shape:

```rust
        let header: Vec<(String, String, String)> = model
            .header
            .iter()
            .map(|h| (h.column.to_string(), h.label.to_string(), h.text.to_string()))
            .collect();
        assert_eq!(
            header,
            vec![
                ("anchor_date".into(), "anchor".into(), "2026-09-12".into()),
                ("spot_ref".into(), "spot".into(), "5000".into()),
            ]
        );
        assert!(model.header.iter().all(|h| !h.edited));
```

(Use whatever the fixture's actual attribute values are.)

- [ ] **Step 2: Run to see it fail to compile**

Run: `cargo test -p geode-marketdata matrix::tests`
Expected: compile error — `header` is `Vec<(SharedString, SharedString)>`.

- [ ] **Step 3: Implement**

`spec.rs`:

```rust
/// One document-level attribute the header paints (spec 2026-09-14 §4):
/// the column it reads, the short label the dense row shows, and the
/// declared type a typed edit is parsed as — on the SPEC for the same
/// reason `value_type` is (a `Snapshot` carries no declared type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderAttr {
    pub column: &'static str,
    pub label: &'static str,
    pub ty: ColumnType,
}
```

`PanelSpec.header: &'static [HeaderAttr]`; `names()` uses `self.header.iter().any(|h| h.column == column)`; `CVI.header`:

```rust
    header: &[
        HeaderAttr { column: "anchor_date", label: "anchor", ty: ColumnType::Date },
        HeaderAttr { column: "spot_ref", label: "spot", ty: ColumnType::F64 },
    ],
```

`matrix.rs`:

```rust
/// One header attribute as painted: prepared text, and whether the draft
/// has overridden it (Task 3 sets `edited`; here it is always false).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderCell {
    pub column: SharedString,
    pub label: SharedString,
    pub text: SharedString,
    pub edited: bool,
}
```

`header_of`:

```rust
fn header_of(snapshot: &Snapshot, spec: &PanelSpec) -> Vec<HeaderCell> {
    spec.header
        .iter()
        .filter_map(|attr| {
            let idx = snapshot.column_index(attr.column)?;
            let value = label_at(snapshot, idx, 0)?;
            Some(HeaderCell {
                column: attr.column.into(),
                label: attr.label.into(),
                text: value.into(),
                edited: false,
            })
        })
        .collect()
}
```

`tile.rs::rebuild_chrome`: `for h in &self.model.header { … format!("{}: {}", h.label, h.text) … }`. Fix every other `header` reader the compiler names (there is a `header: &["currency"]` fixture in `matrix.rs` tests — give it a `HeaderAttr` with `ty: ColumnType::Utf8`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-marketdata`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-marketdata
git commit -m "marketdata: header attributes carry a label and a type on the spec"
```

---

### Task 3: One draft — `attrs`, `parse_attr`, `DraftBadge`

**Files:**
- Modify: `crates/geode-marketdata/src/core/draft.rs`
- Modify: `crates/geode-marketdata/src/core/matrix.rs` (`header_of` takes the draft)
- Modify: `crates/geode-marketdata/src/tile.rs` (callers of `summary()`, `len()` notices)
- Test: `draft.rs` tests (incl. the existing proptest block), `matrix.rs` tests

**Interfaces:**
- Produces:
  ```rust
  pub struct Draft { pub base: Option<String>, pub edits: BTreeMap<(usize,usize), f64>, pub attrs: BTreeMap<String, Value>, pub state: DraftState, labels: … }
  impl Draft {
      pub fn set_attr(&mut self, column: &str, value: Value, base: &str);
      pub fn cell_count(&self) -> usize;      // edits.len()
      pub fn attr_count(&self) -> usize;      // attrs.len()
      pub fn len(&self) -> usize;             // both
      pub fn badge(&self) -> DraftBadge;      // replaces summary()
      pub fn count_phrase(&self) -> String;   // "3 cells, spot" — for confirms/notices
  }
  pub enum DraftBadge { Clean, Dirty, Behind { newer: String }, Sent }
  pub fn parse_attr(text: &str, ty: ColumnType) -> Result<Value, String>;
  pub fn attr_text(value: &Value) -> String;   // Date YYYY-MM-DD, numbers shortest round-trip, Utf8 verbatim
  ```
- `rebase(model)` keeps an attr whose `column` the model's `header` declares; the dropped list gains attribute names as `(column, "attribute")` pairs so `dropped_notice` needs no second shape.

- [ ] **Step 1: Failing tests**

Add to `draft.rs` tests:

```rust
    use geode_core::document::Value;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    #[test]
    fn an_attribute_edit_is_part_of_the_same_draft() {
        let mut draft = Draft::default();
        assert_eq!(draft.badge(), DraftBadge::Clean);
        draft.set_attr("spot_ref", Value::F64(4520.0), "2026-09-14T14:00:00Z");
        assert_eq!(draft.len(), 1);
        assert_eq!(draft.attr_count(), 1);
        assert_eq!(draft.base.as_deref(), Some("2026-09-14T14:00:00Z"));
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.badge(), DraftBadge::Dirty);
        assert_eq!(draft.revert(), 1);
        assert!(draft.is_empty() && draft.base.is_none());
    }

    #[test]
    fn an_attribute_edit_survives_rebase_when_the_newer_document_declares_it() {
        let mut draft = Draft::default();
        draft.set_attr("spot_ref", Value::F64(1.0), "t0");
        draft.set_attr("gone", Value::I64(2), "t0");
        let newer = model_with_header(&[("spot_ref", "spot")]); // helper: a MatrixModel whose header names these columns
        let (kept, dropped) = draft.rebase(&newer);
        assert_eq!(kept, 1);
        assert_eq!(dropped, vec![("gone".to_string(), "attribute".to_string())]);
        assert_eq!(draft.attrs.get("spot_ref"), Some(&Value::F64(1.0)));
    }

    #[test]
    fn parse_attr_per_type() {
        assert_eq!(parse_attr("2026-09-14", ColumnType::Date), Ok(Value::Date(d(2026, 9, 14))));
        assert_eq!(parse_attr("2026-13-45", ColumnType::Date), Err("'2026-13-45' is not a date (YYYY-MM-DD)".into()));
        assert_eq!(parse_attr(" 4520.5 ", ColumnType::F64), Ok(Value::F64(4520.5)));
        assert_eq!(parse_attr("7", ColumnType::I64), Ok(Value::I64(7)));
        assert_eq!(parse_attr("7.5", ColumnType::I64), Err("'7.5' is not a whole number".into()));
        assert_eq!(parse_attr("  ", ColumnType::Utf8), Err("a value is required".into()));
        assert_eq!(parse_attr(" abc ", ColumnType::Utf8), Ok(Value::Utf8("abc".into())));
        assert!(parse_attr("x", ColumnType::Bool).is_err());
    }

    #[test]
    fn attr_text_is_the_documents_own_spelling() {
        assert_eq!(attr_text(&Value::Date(d(2026, 9, 14))), "2026-09-14");
        assert_eq!(attr_text(&Value::F64(5000.0)), "5000");
        assert_eq!(attr_text(&Value::F64(4520.25)), "4520.25");
        assert_eq!(attr_text(&Value::I64(3)), "3");
    }

    #[test]
    fn toml_round_trips_attribute_edits() {
        let mut draft = Draft::default();
        draft.set_attr("anchor_date", Value::Date(d(2026, 9, 14)), "t0");
        draft.set_attr("spot_ref", Value::F64(4520.0), "t0");
        let back = Draft::from_toml(&draft.to_toml());
        assert_eq!(back.attrs, draft.attrs);
        assert_eq!(back.base, draft.base);
        assert_eq!(back.state, DraftState::Editing);
    }

    #[test]
    fn count_phrase_names_cells_and_attributes() {
        let mut draft = Draft::default();
        draft.set((0, 0), ("1M".into(), "-20".into()), 0.1, "t0");
        draft.set((0, 1), ("1M".into(), "-10".into()), 0.1, "t0");
        draft.set_attr("spot_ref", Value::F64(1.0), "t0");
        assert_eq!(draft.count_phrase(), "2 cells, spot_ref");
        let mut one = Draft::default();
        one.set((0, 0), ("1M".into(), "-20".into()), 0.1, "t0");
        assert_eq!(one.count_phrase(), "1 cell");
    }
```

Write `model_with_header` beside the tests:

```rust
    fn model_with_header(attrs: &[(&str, &str)]) -> MatrixModel {
        MatrixModel {
            header: attrs
                .iter()
                .map(|(c, l)| HeaderCell { column: (*c).into(), label: (*l).into(), text: "".into(), edited: false })
                .collect(),
            ..MatrixModel::default()
        }
    }
```

Extend the existing proptest that round-trips cell edits with a strategy over attrs (`prop::collection::btree_map("[a-z_]{1,8}", any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Value::F64), 0..4)`) and assert `back.attrs == draft.attrs`.

In `matrix.rs` tests add:

```rust
    #[test]
    fn an_edited_attribute_paints_the_drafts_value_marked_edited() {
        let snapshot = cvi_fixture(); // whichever fixture the header test uses
        let mut draft = Draft::default();
        draft.set_attr("spot_ref", Value::F64(4520.0), "t0");
        let model = MatrixModel::build(&snapshot, &CVI, &draft).unwrap();
        let spot = model.header.iter().find(|h| h.column == "spot_ref").unwrap();
        assert_eq!((spot.text.as_ref(), spot.edited), ("4520", true));
        let anchor = model.header.iter().find(|h| h.column == "anchor_date").unwrap();
        assert!(!anchor.edited);
    }
```

- [ ] **Step 2: Run to see failures**

Run: `cargo test -p geode-marketdata draft::tests matrix::tests`
Expected: compile errors (`attrs`, `set_attr`, `badge`, `parse_attr` undefined).

- [ ] **Step 3: Implement**

`draft.rs`:

```rust
use geode_core::document::Value;

/// What the header says about the draft at a glance (spec 2026-09-14 §4):
/// a dot for `Dirty`, `update HH:MM` for `Behind`, `sent HH:MM` for `Sent`
/// (Part 4), nothing for `Clean`. Counts live in `count_phrase`, for the
/// places a number matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftBadge {
    Clean,
    Dirty,
    Behind { newer: String },
    Sent,
}
```

Add `pub attrs: BTreeMap<String, Value>` to `Draft`. Then:

```rust
    pub fn cell_count(&self) -> usize { self.edits.len() }
    pub fn attr_count(&self) -> usize { self.attrs.len() }
    pub fn len(&self) -> usize { self.edits.len() + self.attrs.len() }
    pub fn is_empty(&self) -> bool { self.edits.is_empty() && self.attrs.is_empty() }

    /// Record one attribute edit — the same base rule as `set`.
    pub fn set_attr(&mut self, column: &str, value: Value, base: &str) {
        if self.is_empty() || self.base.is_none() {
            self.base = Some(base.to_string());
        }
        self.attrs.insert(column.to_string(), value);
        if matches!(self.state, DraftState::Clean | DraftState::Sent) {
            self.state = DraftState::Editing;
        }
    }

    pub fn badge(&self) -> DraftBadge {
        match &self.state {
            DraftState::Clean => DraftBadge::Clean,
            DraftState::Editing => DraftBadge::Dirty,
            DraftState::Behind { newer } => DraftBadge::Behind { newer: newer.clone() },
            DraftState::Sent => DraftBadge::Sent,
        }
    }

    /// "3 cells, spot_ref" / "1 cell" / "anchor_date, spot_ref".
    pub fn count_phrase(&self) -> String {
        let mut parts = Vec::new();
        match self.edits.len() {
            0 => {}
            1 => parts.push("1 cell".to_string()),
            n => parts.push(format!("{n} cells")),
        }
        parts.extend(self.attrs.keys().cloned());
        parts.join(", ")
    }
```

`set`'s base guard becomes `if self.is_empty() || self.base.is_none()`. `revert` counts `self.len()` before clearing and clears `attrs` too. In `rebase`, after the cell loop:

```rust
        let declared: std::collections::HashSet<&str> =
            model_of_newer.header.iter().map(|h| h.column.as_ref()).collect();
        let mut attrs = BTreeMap::new();
        for (column, value) in std::mem::take(&mut self.attrs) {
            if declared.contains(column.as_str()) {
                attrs.insert(column, value);
            } else {
                dropped.push((column, "attribute".to_string()));
            }
        }
        self.attrs = attrs;
        self.state = if self.is_empty() { DraftState::Clean } else { DraftState::Editing };
        (self.len(), dropped)
```

Delete `summary()` and the old `count_phrase(count, verb)`; keep `local_hhmm` (Task 4 uses it). `to_toml` adds:

```rust
        if !self.attrs.is_empty() {
            let mut attrs = toml::Table::new();
            for (column, value) in &self.attrs {
                attrs.insert(column.clone(), match value {
                    Value::F64(f) => toml::Value::Float(*f),
                    Value::I64(i) => toml::Value::Integer(*i),
                    Value::Utf8(s) => toml::Value::String(s.clone()),
                    Value::Date(d) => toml::Value::String(d.format("%Y-%m-%d").to_string()),
                });
            }
            table.insert("attrs".into(), toml::Value::Table(attrs));
        }
```

`from_toml` reads it back: a `Float`/`Integer` → `F64`/`I64`; a `String` that parses as `%Y-%m-%d` → `Date`, else `Utf8` (a date string is unambiguous, a free-text attribute never looks like one; say so in a comment). `state` is `Editing` when either map is non-empty.

```rust
/// Parse a typed header attribute to the value a document holds.
pub fn parse_attr(text: &str, ty: ColumnType) -> Result<Value, String> {
    let trimmed = text.trim();
    match ty {
        ColumnType::Date => chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| format!("'{text}' is not a date (YYYY-MM-DD)")),
        ColumnType::F64 => parse_cell(text, ColumnType::F64).map(Value::F64),
        ColumnType::I64 => parse_cell(text, ColumnType::I64).map(|f| Value::I64(f as i64)),
        ColumnType::Utf8 if trimmed.is_empty() => Err("a value is required".to_string()),
        ColumnType::Utf8 => Ok(Value::Utf8(trimmed.to_string())),
        other => Err(format!("a {other:?} attribute is not editable")),
    }
}

/// The document's own spelling of an attribute (what `label_at` yields for
/// the delivered value), so an edited value paints in the same shape.
pub fn attr_text(value: &Value) -> String {
    match value {
        Value::F64(f) => format!("{f}"),
        Value::I64(i) => i.to_string(),
        Value::Utf8(s) => s.clone(),
        Value::Date(d) => d.format("%Y-%m-%d").to_string(),
    }
}
```

Check `parse_cell`'s I64 error text and reuse it; `7.5` for I64 must be refused (it already is — confirm the message, and change the test's expected string to match).

`matrix.rs`: `header_of(snapshot, spec, draft)`:

```rust
            let (text, edited) = match draft.attrs.get(attr.column) {
                Some(value) => (attr_text(value), true),
                None => (label_at(snapshot, idx, 0)?, false),
            };
```

`tile.rs`: replace `self.draft.summary()` in `rebuild_chrome` with a temporary `match self.draft.badge() { DraftBadge::Clean => None, DraftBadge::Dirty => Some("edited"), DraftBadge::Behind { newer } => Some(format!("update {}", local_hhmm(&newer))), DraftBadge::Sent => Some("sent") }` chip (Task 4 replaces the whole header). `set_key`'s refusal: `format!("{} pending — :revert first", self.draft.count_phrase())`. Any test asserting the old summary text (`edit_commit_paints_the_cell_as_edited_and_the_header_counts_it`, `a_restored_draft_lands_in_behind_on_a_newer_delivery`) is updated to the new chip text; Task 4 rewrites them again — keep the change minimal.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-marketdata && cargo clippy -p geode-marketdata --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Harness entries and commit**

```bash
run_mutation "mddraft: an attribute edit counts in the draft" \
  crates/geode-marketdata/src/core/draft.rs \
  '    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.attrs.is_empty()
    }' \
  '    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }' \
  geode-marketdata an_attribute_edit_is_part_of_the_same_draft

run_mutation "mddraft: rebase keeps a declared attribute and names an undeclared one" \
  crates/geode-marketdata/src/core/draft.rs \
  '            if declared.contains(column.as_str()) {' \
  '            if false {' \
  geode-marketdata an_attribute_edit_survives_rebase_when_the_newer_document_declares_it

run_mutation "mddraft: an edited attribute paints the draft's value" \
  crates/geode-marketdata/src/core/matrix.rs \
  '            let (text, edited) = match draft.attrs.get(attr.column) {' \
  '            let (text, edited) = match None::<&Value> {' \
  geode-marketdata an_edited_attribute_paints_the_drafts_value_marked_edited
```

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: attribute edits join the draft; parse_attr; DraftBadge replaces summary"
```

---

### Task 4: The dense header row

**Files:**
- Create: `crates/geode-marketdata/src/header.rs`
- Modify: `crates/geode-marketdata/src/tile.rs` (`rebuild_chrome` → `HeaderModel::prepare`, `render` calls `header::render`, remove `Chip`/`Tone` chip list, keep `FlooredTones`/`tone_colour`)
- Modify: `crates/geode-marketdata/src/lib.rs` (`mod header;`)
- Test: `header.rs` tests (pure), `tile.rs` tests (chip assertions → `header_texts`)

**Interfaces:**
- Consumes: `DraftBadge`, `HeaderCell`, `FlooredTones`, `tone_colour`, `cell_paint` (from `delegate.rs`; make it `pub(crate)` — it already is).
- Produces:
  ```rust
  pub(crate) struct HeaderModel {
      pub title: SharedString,                 // spec.title, the badge
      pub underlying: Option<SharedString>,    // display_key or None
      pub dirty: bool,
      pub attrs: Vec<HeaderCell>,              // a clone of model.header (Rc-cheap SharedStrings)
      pub state: Option<(SharedString, Tone)>, // the one short run
      pub notice: Option<SharedString>,
      pub time: Option<SharedString>,          // HH:MM:SS
      pub stale: bool,
  }
  pub(crate) struct HeaderInputs<'a> { pub spec: &'a PanelSpec, pub key: Option<&'a [String]>, pub model: &'a MatrixModel, pub badge: DraftBadge, pub unresolved_restore: bool, pub notice: Option<&'a SharedString>, pub source_at: Option<DateTime<Utc>> }
  impl HeaderModel { pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel; pub(crate) fn texts(&self) -> Vec<String>; /* test door: every painted string in order */ }
  pub(crate) fn render(h: &HeaderModel, cursor_attr: Option<usize>, editor: Option<(usize, &Entity<InputState>)>, menu_open: bool, theme: &Theme, tones: &FlooredTones, tile: &Entity<MarketDataTile>) -> impl IntoElement;
  ```
  `Tone` moves to `header.rs` (`pub(crate) enum Tone { Plain, Key, Time, Warn, Error }`), as does `tone_colour`. `cursor_attr`/`editor` are `None` until Task 5; `menu_open` is `false` until Task 6.

- [ ] **Step 1: Failing pure tests**

`header.rs` tests:

```rust
    fn inputs<'a>(model: &'a MatrixModel, key: Option<&'a [String]>, badge: DraftBadge) -> HeaderInputs<'a> {
        HeaderInputs { spec: &CVI, key, model, badge, unresolved_restore: false, notice: None, source_at: None }
    }

    #[test]
    fn a_clean_header_says_nothing_but_identity_attributes_and_time() {
        let model = model_with_header(&[("anchor_date", "anchor", "2026-09-14"), ("spot_ref", "spot", "5000")]);
        let key = vec!["SPX.Z".to_string()];
        let h = HeaderModel::prepare(inputs(&model, Some(&key), DraftBadge::Clean));
        assert_eq!(h.texts(), vec!["CVI", "SPX.Z", "anchor 2026-09-14", "spot 5000"]);
        assert!(!h.dirty && h.state.is_none());
    }

    #[test]
    fn dirty_is_a_dot_and_behind_reads_update_hhmm() {
        let model = model_with_rows(); // any model with one row
        let key = vec!["SPX.Z".to_string()];
        let dirty = HeaderModel::prepare(inputs(&model, Some(&key), DraftBadge::Dirty));
        assert!(dirty.dirty && dirty.state.is_none());
        let newer = chrono::Utc::now().to_rfc3339();
        let behind = HeaderModel::prepare(inputs(&model, Some(&key), DraftBadge::Behind { newer: newer.clone() }));
        let expected = format!("update {}", chrono::DateTime::parse_from_rfc3339(&newer).unwrap().with_timezone(&chrono::Local).format("%H:%M"));
        assert_eq!(behind.state.as_ref().map(|(t, tone)| (t.to_string(), *tone)), Some((expected, Tone::Warn)));
    }

    #[test]
    fn the_no_underlying_no_document_and_parked_states() {
        let empty = MatrixModel::default();
        let none = HeaderModel::prepare(inputs(&empty, None, DraftBadge::Clean));
        assert_eq!(none.texts(), vec!["CVI", "no underlying — load…"]);
        let key = vec!["NKY.Z".to_string()];
        let waiting = HeaderModel::prepare(inputs(&empty, Some(&key), DraftBadge::Clean));
        assert_eq!(waiting.state.as_ref().map(|s| s.0.to_string()), Some("no document yet".into()));
        let mut parked = inputs(&empty, Some(&key), DraftBadge::Dirty);
        parked.unresolved_restore = true;
        let parked = HeaderModel::prepare(parked);
        assert_eq!(parked.state.as_ref().map(|s| s.0.to_string()), Some("edits await a document".into()));
        assert!(parked.dirty);
    }

    #[test]
    fn a_notice_is_kept_whole_after_the_state() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let notice: SharedString = "'abc' is not a number".into();
        let mut i = inputs(&model, Some(&key), DraftBadge::Behind { newer: "2026-09-14T14:09:00Z".into() });
        i.notice = Some(&notice);
        let h = HeaderModel::prepare(i);
        let texts = h.texts();
        let state_at = texts.iter().position(|t| t.starts_with("update ")).unwrap();
        let notice_at = texts.iter().position(|t| t == "'abc' is not a number").unwrap();
        assert!(state_at < notice_at);
    }

    #[test]
    fn the_time_is_local_hhmmss_and_stale_is_a_flag() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let at = chrono::Utc::now();
        let mut i = inputs(&model, Some(&key), DraftBadge::Clean);
        i.source_at = Some(at);
        let h = HeaderModel::prepare(i);
        assert_eq!(h.time.as_deref(), Some(at.with_timezone(&chrono::Local).format("%H:%M:%S").to_string().as_str()));
        assert!(!h.stale, "staleness is the tile's clock reading, applied at paint");
    }
```

`texts()` order: `title`, `underlying` (or the no-underlying line), each `"{label} {text}"`, state, notice, time (with `" stale"` appended when `stale`). Write `model_with_header(&[(col, label, text)])` and `model_with_rows()` helpers in the test module.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-marketdata header::tests`
Expected: compile error, `header` module missing.

- [ ] **Step 3: Implement `header.rs`**

```rust
//! The panel's header row (spec 2026-09-14 §4): prepared once per change
//! by [`HeaderModel::prepare`] — the tile's `changed()` door — and painted
//! by [`render`] with no formatting of its own.

use crate::core::draft::DraftBadge;
use crate::core::matrix::{HeaderCell, MatrixModel};
use crate::core::spec::PanelSpec;
use crate::delegate::{CellPaint, cell_paint};
use crate::tile::{FlooredTones, MarketDataTile};
use chrono::{DateTime, Utc};
use geode_shell::fonts;
use gpui::prelude::*;
use gpui::{Entity, SharedString, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Theme, h_flex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone { Plain, Key, Time, Warn, Error }

pub(crate) struct HeaderInputs<'a> { /* as in Interfaces */ }
pub(crate) struct HeaderModel { /* as in Interfaces */ }

impl HeaderModel {
    pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel {
        let underlying = i.key.map(|k| SharedString::from(crate::tile::display_key(k)));
        let (dirty, mut state) = match i.badge {
            DraftBadge::Clean => (false, None),
            DraftBadge::Dirty => (true, None),
            DraftBadge::Behind { newer } => (true, Some((format!("update {}", local_hhmm(&newer)).into(), Tone::Warn))),
            DraftBadge::Sent => (false, Some(("sent".into(), Tone::Time))),
        };
        if i.key.is_some() && i.model.rows.is_empty() {
            state = Some(if i.unresolved_restore && dirty {
                ("edits await a document".into(), Tone::Warn)
            } else {
                ("no document yet".into(), Tone::Warn)
            });
        }
        HeaderModel {
            title: i.spec.title.into(),
            underlying,
            dirty,
            attrs: i.model.header.clone(),
            state,
            notice: i.notice.cloned(),
            time: i.source_at.map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string().into()),
            stale: false,
        }
    }
    pub(crate) fn texts(&self) -> Vec<String> { /* the order above */ }
}
```

(`local_hhmm` — make `draft.rs`'s `pub(crate)` and import it. `display_key` — make it `pub(crate)` in `tile.rs`. `FlooredTones` and `tone_colour` — move `tone_colour` here, keep `FlooredTones` in `tile.rs` as `pub(crate)`.)

`render`:

```rust
pub(crate) fn render(
    h: &HeaderModel, cursor_attr: Option<usize>, editor: Option<(usize, &Entity<InputState>)>,
    menu_open: bool, theme: &Theme, tones: &FlooredTones, tile: &Entity<MarketDataTile>, tile_id: u64,
) -> impl IntoElement {
    let muted = theme.muted_foreground;
    let mut row = h_flex().w_full().h(px(22.)).items_center().gap_3().px_2().text_sm()
        .text_color(muted).border_b_1().border_color(theme.border)
        .debug_selector(move || format!("marketdata-header-{tile_id}"));
    // 1. badge
    row = row.child(div().px_1p5().rounded_sm().bg(theme.secondary).text_color(theme.secondary_foreground).text_xs().child(h.title.clone()));
    // 2. underlying + dot
    match &h.underlying {
        Some(u) => {
            row = row.child(div().font_weight(gpui::FontWeight::BOLD).text_color(theme.foreground).child(u.clone()));
            if h.dirty {
                row = row.child(div().size(px(8.)).rounded_full().bg(tones.warn).debug_selector(move || format!("marketdata-dirty-{tile_id}")));
            }
        }
        None => { row = row.child(div().text_color(muted).child("no underlying — load…")); }
    }
    // 3. attribute strip
    for (i, attr) in h.attrs.iter().enumerate() {
        let CellPaint { fill, text } = cell_paint(theme, false, attr.edited);
        let at_cursor = cursor_attr == Some(i);
        let mut value = div().px_1().rounded_sm().font_family(fonts::MONO).text_color(text)
            .when_some(fill, |d, f| d.bg(f))
            .border_1().border_color(if at_cursor { theme.table_active_border } else { gpui::transparent_black() })
            .debug_selector(move || format!("marketdata-attr-{tile_id}-{i}"));
        value = match editor {
            Some((e, state)) if e == i => value.child(div().min_w(px(80.)).child(Input::new(state))),
            _ => value.child(attr.text.clone()),
        };
        row = row.child(h_flex().gap_1().child(div().text_color(muted).child(attr.label.clone())).child(value));
    }
    row = row.child(div().flex_1());
    // 5. state, notice
    if let Some((text, tone)) = &h.state { row = row.child(div().text_color(tone_colour(*tone, false, theme, tones)).child(text.clone())); }
    if let Some(n) = &h.notice { row = row.child(div().text_color(tones.error).child(n.clone())); }
    // 6. time
    if let Some(t) = &h.time {
        let colour = if h.stale { tones.warn } else { muted };
        row = row.child(h_flex().gap_1().text_color(colour).child(t.clone()).when(h.stale, |d| d.child("stale")));
    }
    // 7. ⋯ — Task 6 wires the click; painted now so the row's shape is final
    row = row.child(div().px_1p5().rounded_sm().border_1().border_color(theme.border)
        .when(menu_open, |d| d.bg(theme.secondary))
        .text_color(muted).child("⋯")
        .debug_selector(move || format!("marketdata-menu-button-{tile_id}")));
    let _ = tile;
    row
}
```

The mouse handlers on the attribute values and on `⋯` are added in Tasks 5 and 6 (they need the tile entity, which is why it is a parameter now).

`tile.rs`: replace `chips: Vec<Chip>` with `header: HeaderModel`; `rebuild_chrome` becomes

```rust
    fn rebuild_chrome(&mut self) {
        self.source_at = /* unchanged */;
        self.header = HeaderModel::prepare(HeaderInputs {
            spec: self.spec, key: self.key.as_deref(), model: &self.model,
            badge: self.draft.badge(), unresolved_restore: self.unresolved_restore,
            notice: self.notice.as_ref(), source_at: self.source_at,
        });
    }
```

and `render` does `self.header.stale = self.is_stale(now)` before calling `header::render(&self.header, None, None, false, theme, &self.tones, &cx.entity(), self.id.0)`. Test door: `header_chips()` → `header_texts()` returning `self.header.texts()` with `stale` applied. Update every tile test that read chips: the strings are now the concise ones (`"CVI"`, `"SPX.Z"`, `"anchor 2026-09-12"`, `"spot 5000"`, `"update HH:MM"`, `"no document yet"`, `"edits await a document"`). `stale` must paint the right glyph: the existing `stale` chip test becomes an assertion that `header_texts()` ends with `"HH:MM:SS stale"`.

Bundled-theme sweep, in `tile.rs` tests beside `every_header_tone_is_readable_on_every_bundled_theme`: add the dot — `contrast_ratio(to_rgb(FlooredTones::derive(theme).warn), ground(theme)) >= 3.0` is already covered by `Tone::Warn`; assert it explicitly once with a comment that the dot is `tones.warn`.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-marketdata && cargo clippy -p geode-marketdata --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 5: Harness entries and commit**

```bash
run_mutation "mdheader: behind reads update HH:MM" \
  crates/geode-marketdata/src/header.rs \
  'format!("update {}", local_hhmm(&newer))' \
  'format!("different document received {}", local_hhmm(&newer))' \
  geode-marketdata dirty_is_a_dot_and_behind_reads_update_hhmm

run_mutation "mdheader: a dirty draft paints the dot" \
  crates/geode-marketdata/src/header.rs \
  '            DraftBadge::Dirty => (true, None),' \
  '            DraftBadge::Dirty => (false, None),' \
  geode-marketdata dirty_is_a_dot_and_behind_reads_update_hhmm
```

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: the dense header row — badge, underlying, dot, attribute strip, concise state"
```

---

### Task 5: The cursor enters the strip; attribute editing; `:set`

**Files:**
- Create: `crates/geode-marketdata/src/core/cursor.rs`
- Modify: `crates/geode-marketdata/src/core/mod.rs` (`pub mod cursor;`)
- Modify: `crates/geode-marketdata/src/tile.rs` (`cursor: Cursor`, `move_cursor` → `cursor::step`, `begin_edit`/`commit_edit` on `Attr`, `sync_cursor`, `yank_text`, `clamp_cursor`, `cursor_to`, `command` for `:set`)
- Modify: `crates/geode-marketdata/src/commands.rs` (`Command::Set { attr, value: Option<String> }`)
- Modify: `crates/geode-marketdata/src/header.rs` (`on_mouse_down` on an attribute value → `tile.cursor_to_attr(i)`)
- Modify: `crates/geode-marketdata/src/delegate.rs` (mirror `Cursor::Cell` only; `Attr` mirrors as no selection)
- Test: `cursor.rs` tests; `commands.rs` tests; `tile.rs` window tests

**Interfaces:**
- Produces:
  ```rust
  // core/cursor.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Cursor { Cell { row: usize, col: usize }, Attr(usize) }
  pub struct Grid { pub rows: usize, pub cols: usize, pub attrs: usize }
  pub enum Motion { Rows(isize), Cols(isize), Top, Bottom, FirstCol, LastCol }
  pub fn step(cursor: Cursor, last_grid_col: &mut usize, motion: Motion, grid: Grid) -> Cursor;
  pub fn clamp(cursor: Cursor, grid: Grid) -> Cursor;
  // commands.rs
  Command::Set { attr: String, value: Option<String> }
  // tile.rs
  pub(crate) fn cursor_to_attr(&mut self, i: usize, cx: &mut Context<Self>);
  ```
- `Editing` gains a target: `enum EditTarget { Cell { cell: (usize, usize), labels: (SharedString, SharedString) }, Attr { index: usize, column: SharedString } }`.

- [ ] **Step 1: Failing pure tests (`cursor.rs`)**

```rust
    const G: Grid = Grid { rows: 5, cols: 4, attrs: 2 };
    fn cell(row: usize, col: usize) -> Cursor { Cursor::Cell { row, col } }

    #[test]
    fn k_on_the_top_row_enters_the_strip_at_the_nearest_attribute() {
        let mut last = 0;
        assert_eq!(step(cell(0, 1), &mut last, Motion::Rows(-1), G), Cursor::Attr(1));
        assert_eq!(last, 1);
        assert_eq!(step(cell(0, 3), &mut last, Motion::Rows(-1), G), Cursor::Attr(1), "clamped to the last attribute");
        assert_eq!(last, 3);
        assert_eq!(step(cell(2, 1), &mut last, Motion::Rows(-1), G), cell(1, 1), "not from a lower row");
    }

    #[test]
    fn k_with_no_attributes_stays_put() {
        let mut last = 0;
        let g = Grid { attrs: 0, ..G };
        assert_eq!(step(cell(0, 2), &mut last, Motion::Rows(-1), g), cell(0, 2));
    }

    #[test]
    fn j_returns_to_the_top_row_at_the_remembered_column() {
        let mut last = 3;
        assert_eq!(step(Cursor::Attr(0), &mut last, Motion::Rows(1), G), cell(0, 3));
        assert_eq!(step(Cursor::Attr(0), &mut last, Motion::Rows(7), G), cell(0, 3), "any downward count lands on row 0");
    }

    #[test]
    fn h_l_caret_dollar_in_the_strip() {
        let mut last = 0;
        assert_eq!(step(Cursor::Attr(0), &mut last, Motion::Cols(1), G), Cursor::Attr(1));
        assert_eq!(step(Cursor::Attr(1), &mut last, Motion::Cols(5), G), Cursor::Attr(1));
        assert_eq!(step(Cursor::Attr(1), &mut last, Motion::Cols(-1), G), Cursor::Attr(0));
        assert_eq!(step(Cursor::Attr(1), &mut last, Motion::FirstCol, G), Cursor::Attr(0));
        assert_eq!(step(Cursor::Attr(0), &mut last, Motion::LastCol, G), Cursor::Attr(1));
    }

    #[test]
    fn grid_verbs_from_the_strip_land_in_the_grid() {
        let mut last = 2;
        assert_eq!(step(Cursor::Attr(1), &mut last, Motion::Top, G), cell(0, 2));
        assert_eq!(step(Cursor::Attr(1), &mut last, Motion::Bottom, G), cell(4, 2));
    }

    #[test]
    fn an_empty_grid_pins_the_cursor_to_the_origin() {
        let mut last = 0;
        let g = Grid { rows: 0, cols: 0, attrs: 2 };
        assert_eq!(step(cell(0, 0), &mut last, Motion::Rows(-1), g), cell(0, 0));
        assert_eq!(clamp(Cursor::Attr(1), g), cell(0, 0));
        assert_eq!(clamp(Cursor::Attr(5), G), Cursor::Attr(1));
        assert_eq!(clamp(cell(9, 9), G), cell(4, 3));
    }
```

`commands.rs`:

```rust
    #[test]
    fn set_parses_an_attribute_with_or_without_a_value() {
        assert_eq!(parse("set spot_ref 4520"), Ok(Command::Set { attr: "spot_ref".into(), value: Some("4520".into()) }));
        assert_eq!(parse("set spot_ref"), Ok(Command::Set { attr: "spot_ref".into(), value: None }));
        assert_eq!(parse("set"), Err("usage: set <attribute> [value]".into()));
        assert_eq!(parse("set anchor_date 2026 09 14"), Err("a value is one word".into()));
    }

    #[test]
    fn set_completes_attribute_names() {
        let c = completions("set ", 4, &[], false, &["anchor_date".into(), "spot_ref".into()]);
        assert_eq!(c, vec!["anchor_date", "spot_ref"]);
    }
```

(`completions` gains a fifth parameter `attrs: &[String]`; update its other callers/tests.)

- [ ] **Step 2: Run to see failures**

Run: `cargo test -p geode-marketdata cursor::tests commands::tests::set`
Expected: compile errors.

- [ ] **Step 3: Implement the cores**

`core/cursor.rs`:

```rust
//! Where the panel's cursor is (spec 2026-09-14 §5.1): a grid cell, or an
//! attribute in the header strip. Pure; the tile applies the result.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor { Cell { row: usize, col: usize }, Attr(usize) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid { pub rows: usize, pub cols: usize, pub attrs: usize }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion { Rows(isize), Cols(isize), Top, Bottom, FirstCol, LastCol }

pub fn clamp(cursor: Cursor, grid: Grid) -> Cursor {
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    match cursor {
        Cursor::Attr(i) if grid.attrs > 0 => Cursor::Attr(i.min(grid.attrs - 1)),
        Cursor::Attr(_) => Cursor::Cell { row: 0, col: 0 },
        Cursor::Cell { row, col } => Cursor::Cell { row: row.min(grid.rows - 1), col: col.min(grid.cols - 1) },
    }
}

/// One motion. `last_grid_col` is the column the cursor left the grid
/// from — written on entering the strip, read on leaving it.
pub fn step(cursor: Cursor, last_grid_col: &mut usize, motion: Motion, grid: Grid) -> Cursor {
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    let max_row = grid.rows - 1;
    let max_col = grid.cols - 1;
    match (cursor, motion) {
        (Cursor::Cell { row: 0, col }, Motion::Rows(n)) if n < 0 && grid.attrs > 0 => {
            *last_grid_col = col;
            Cursor::Attr(col.min(grid.attrs - 1))
        }
        (Cursor::Cell { row, col }, Motion::Rows(n)) => Cursor::Cell { row: add(row, n, max_row), col },
        (Cursor::Cell { row, col }, Motion::Cols(n)) => Cursor::Cell { row, col: add(col, n, max_col) },
        (Cursor::Cell { col, .. }, Motion::Top) => Cursor::Cell { row: 0, col },
        (Cursor::Cell { col, .. }, Motion::Bottom) => Cursor::Cell { row: max_row, col },
        (Cursor::Cell { row, .. }, Motion::FirstCol) => Cursor::Cell { row, col: 0 },
        (Cursor::Cell { row, .. }, Motion::LastCol) => Cursor::Cell { row, col: max_col },
        (Cursor::Attr(_), Motion::Rows(n)) if n > 0 => Cursor::Cell { row: 0, col: (*last_grid_col).min(max_col) },
        (Cursor::Attr(i), Motion::Rows(_)) => Cursor::Attr(i),
        (Cursor::Attr(i), Motion::Cols(n)) => Cursor::Attr(add(i, n, grid.attrs.saturating_sub(1))),
        (Cursor::Attr(_), Motion::Top) => Cursor::Cell { row: 0, col: (*last_grid_col).min(max_col) },
        (Cursor::Attr(_), Motion::Bottom) => Cursor::Cell { row: max_row, col: (*last_grid_col).min(max_col) },
        (Cursor::Attr(_), Motion::FirstCol) => Cursor::Attr(0),
        (Cursor::Attr(_), Motion::LastCol) => Cursor::Attr(grid.attrs.saturating_sub(1)),
    }
}

fn add(at: usize, n: isize, max: usize) -> usize {
    (at as isize).saturating_add(n).clamp(0, max as isize) as usize
}
```

`commands.rs`: `Command::Set { attr: String, value: Option<String> }`, parsed as `set <attr> [value]` with the two errors above; `completions(line, cursor, keys, behind, attrs)` offers `attrs` for the second word of a `set` line.

- [ ] **Step 4: Wire the tile**

- `cursor: Cursor` (was `(usize, usize)`), plus `last_grid_col: usize`. `grid()` helper: `Grid { rows: self.model.rows.len(), cols: self.model.columns.len(), attrs: self.model.header.len() }`.
- `dispatch`'s motion arm builds a `Motion` (`down` → `Rows(n)`, `up` → `Rows(-n)`, `left`/`right` → `Cols`, `page_*` → `Rows(±HALF_PAGE*n)`, `top`/`bottom`/`first_col`/`last_col`) and calls `self.cursor = cursor::step(self.cursor, &mut self.last_grid_col, motion, self.grid())`. Motions into/out of the strip set `chrome = true` (the strip's cursor border is header paint); a motion that stays in the grid keeps `false`.
- `clamp_cursor` → `self.cursor = cursor::clamp(self.cursor, self.grid())`.
- `sync_cursor`: on `Cursor::Cell { row, col }` as today; on `Attr(_)` → `t.clear_selection(cx)` and mirror `d.cursor = None` (make the delegate's `cursor: Option<(usize, usize)>`; `at_cursor` is `Some((row_ix, model_col)) == self.cursor`).
- `cursor_to` (a table click) sets `Cursor::Cell`. New `cursor_to_attr(i)`: `self.cursor = Cursor::Attr(i.min(attrs-1))` if `attrs > 0`, then `sync_cursor`, `rebuild_chrome`, `notify`. `header::render` attaches `.on_mouse_down(MouseButton::Left, { let tile = tile.clone(); move |_, _, cx| tile.update(cx, |t, cx| t.cursor_to_attr(i, cx)) })` on each attribute value (call `cx.stop_propagation()` so the tile's focus-restore mouse-down still runs? — no: the shell's listener is on the tile container above, propagation must continue; do NOT stop it).
- `yank_text`: `Cursor::Attr(i)` → `Yank::Cell` = `attrs[i].text`, `Yank::Row` = `"{label}\t{text}"`, `Yank::Col` → `None` and notice `"nothing to yank in a column here"`.
- `Editing` gets `target: EditTarget`. `begin_edit`: on `Attr(i)`, the same `is_behind`/`edit_base` checks (edit_base requires rows; for an attribute require `!self.model.header.is_empty()` instead — write `attr_edit_base()` mirroring `edit_base`), seed the input with `self.model.header[i].text`, `target: EditTarget::Attr { index: i, column: header[i].column.clone() }`. `commit_edit`: on `Attr`, look up `spec.header.iter().find(|a| a.column == column)` (refuse with `CELL_MOVED`-style notice if the model's header no longer has the column at that index), `parse_attr(&text, attr.ty)` → `Err` stays in insert mode; `Ok(value)` → `self.draft.set_attr(&column, value, &base)`, close, rebuild model.
- `render` passes `cursor_attr: match self.cursor { Cursor::Attr(i) => Some(i), _ => None }` and `editor: self.editor.as_ref().and_then(|e| match &e.target { EditTarget::Attr { index, .. } => Some((*index, &e.state)), _ => None })`; the delegate's editor mirror only receives `EditTarget::Cell`.
- `command`: `Command::Set { attr, value: None }` → `Err(format!("{attr} = {text}"))` where `text` is the header cell's current text (a notice-shaped answer through the command line's own error slot is how the blotter reports too — check `:sort` with no args in `geode-blotter` and mirror; if it uses `Ok` + notice, do that). `Some(value)` → find the `HeaderAttr`, `parse_attr`, `set_attr`, `rebuild_model`, `changed`; unknown attr → `Err(format!("no attribute '{attr}' (anchor_date, spot_ref)"))` listing `spec.header` columns.
- `completions`: pass `self.spec.header.iter().map(|a| a.column.to_string()).collect::<Vec<_>>()`.
- `find` never matches the strip (it reads `row_labels()`/columns — unchanged).
- Tests: `cursor()` test door returns `Cursor`. Update the existing cursor tests (`(0,0)` → `Cursor::Cell { row: 0, col: 0 }`).

- [ ] **Step 5: Window tests**

```rust
    #[gpui::test]
    fn k_from_the_top_row_enters_the_strip_and_i_edits_the_attribute(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "up", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
        assert_eq!(h.selection(&vcx), (None, None), "the grid paints no highlighted row behind the strip");
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("5000"));
        h.set_editor(&mut vcx, "4520");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        let (attrs, dirty) = h.tile.read_with(&vcx, |t, _| (t.model().header.clone(), t.header_dirty()));
        assert_eq!((attrs[1].text.as_ref(), attrs[1].edited), ("4520", true));
        assert!(dirty);
        assert!(h.tile.read_with(&vcx, |t, _| t.header_texts()).contains(&"spot 4520".to_string()));
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Cell { row: 0, col: 1 });
    }

    #[gpui::test]
    fn a_bad_date_stays_in_insert_mode_with_the_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "up", None);           // Attr(0) = anchor_date
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "2026-13-45");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)), Some("'2026-13-45' is not a date (YYYY-MM-DD)".into()));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    }

    #[gpui::test]
    fn set_writes_an_attribute_and_revert_clears_it_and_the_dot(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        assert!(h.tile.read_with(&vcx, |t, _| t.header_dirty()));
        assert!(h.command(&mut vcx, "set nope 1").unwrap_err().starts_with("no attribute 'nope'"));
        h.command(&mut vcx, "revert").unwrap();
        assert!(!h.tile.read_with(&vcx, |t, _| t.header_dirty()));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model().header[1].text.to_string()), "5000");
    }

    #[gpui::test]
    fn a_click_on_an_attribute_moves_the_cursor_and_opens_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let at = centre_of(&mut vcx, "marketdata-attr-7-1"); // TILE is 7 in this harness; use the const
        click_at(&mut vcx, at, 1);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
        assert_eq!(h.mode(&vcx), "normal");
    }

    #[gpui::test]
    fn an_attribute_edit_goes_behind_and_rebase_keeps_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        let tag = h.tile.read_with(&vcx, |t, _| t.tag());
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
        assert!(h.tile.read_with(&vcx, |t, _| t.header_texts()).iter().any(|t| t.starts_with("update ")));
        h.command(&mut vcx, "rebase").unwrap();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().attrs.len()), 1);
        assert!(h.tile.read_with(&vcx, |t, _| t.model().header[1].edited));
    }
```

Add `header_dirty()` (`self.header.dirty`) and `tag()` test doors if absent (check the existing `Behind` tests for how they obtain the tag — mirror that instead of a new door if one exists).

- [ ] **Step 6: Run everything**

Run: `cargo test -p geode-marketdata && cargo clippy -p geode-marketdata --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 7: Harness entries and commit**

```bash
run_mutation "mdcursor: k on the top row enters the strip" \
  crates/geode-marketdata/src/core/cursor.rs \
  '        (Cursor::Cell { row: 0, col }, Motion::Rows(n)) if n < 0 && grid.attrs > 0 => {' \
  '        (Cursor::Cell { row: 0, col }, Motion::Rows(n)) if n < 0 && grid.attrs > usize::MAX - 1 => {' \
  geode-marketdata k_on_the_top_row_enters_the_strip_at_the_nearest_attribute

run_mutation "mdcursor: j from the strip returns to the remembered column" \
  crates/geode-marketdata/src/core/cursor.rs \
  '        (Cursor::Attr(_), Motion::Rows(n)) if n > 0 => Cursor::Cell { row: 0, col: (*last_grid_col).min(max_col) },' \
  '        (Cursor::Attr(_), Motion::Rows(n)) if n > 0 => Cursor::Cell { row: 0, col: 0 },' \
  geode-marketdata j_returns_to_the_top_row_at_the_remembered_column

run_mutation "mdattr: a refused attribute value stays in insert mode" \
  crates/geode-marketdata/src/tile.rs \
  '<the Err arm of parse_attr in commit_edit: exact lines>' \
  '<same arm with close_editor(window, cx); added before the notice>' \
  geode-marketdata a_bad_date_stays_in_insert_mode_with_the_notice

run_mutation "mdattr: the strip clears the table selection" \
  crates/geode-marketdata/src/tile.rs \
  '<the Cursor::Attr arm of sync_cursor: t.clear_selection(cx);>' \
  '<the same arm with the clear removed>' \
  geode-marketdata k_from_the_top_row_enters_the_strip_and_i_edits_the_attribute
```

(Fill the two `<…>` anchors with the exact text as written; `--anchors-only` must report 0 ambiguous.)

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: the cursor enters the attribute strip; attributes edit in place and through :set"
```

---

### Task 6: The action list — `core::menu`, `Popup::Menu`, `.`, `⋯`, kind actions

**Files:**
- Create: `crates/geode-marketdata/src/core/menu.rs`
- Create: `crates/geode-marketdata/src/popup.rs`
- Modify: `crates/geode-marketdata/src/core/spec.rs` (`KindAction`, `PanelSpec.actions`)
- Modify: `crates/geode-marketdata/src/content.rs` (`ACTIONS`, fragment, `register_actions` registers `spec.actions`)
- Modify: `crates/geode-marketdata/src/commands.rs` (`Command::Menu`)
- Modify: `crates/geode-marketdata/src/tile.rs` (`popup: Option<Popup>`, `key_context`, `dispatch`, `render`)
- Modify: `crates/geode-marketdata/src/header.rs` (`⋯` click → `tile.toggle_menu`)
- Modify: `crates/geode-marketdata/src/lib.rs` (`mod popup;`)
- Test: `menu.rs` tests; `content.rs` fragment tests; `tile.rs` window tests

**Interfaces:**
- Produces:
  ```rust
  // core/spec.rs
  pub struct KindAction { pub id: &'static str, pub title: &'static str, pub built: bool }
  // PanelSpec.actions: &'static [KindAction]
  // core/menu.rs
  pub struct MenuInputs<'a> { pub badge: DraftBadge, pub has_key: bool, pub upload_built: bool, pub kind_title: &'a str, pub kind_actions: &'a [KindAction] }
  pub enum MenuRow { Action { id: ActionId, title: SharedString, hint: SharedString, enabled: Result<(), &'static str> }, Separator, Section(SharedString) }
  pub fn rows(i: &MenuInputs) -> Vec<MenuRow>;
  pub fn first_enabled(rows: &[MenuRow]) -> usize;
  pub fn step(rows: &[MenuRow], from: usize, delta: isize) -> usize;   // skips Separator/Section, clamps
  // popup.rs
  pub(crate) enum Popup { Menu(MenuState) }           // Task 7 adds Picker
  pub(crate) struct MenuState { pub rows: Vec<MenuRow>, pub highlighted: usize }
  pub(crate) fn render_menu(m: &MenuState, theme: &Theme, tile: &Entity<MarketDataTile>, tile_id: u64) -> impl IntoElement;  // deferred(anchored(..))
  // tile.rs
  pub(crate) fn toggle_menu(&mut self, cx: &mut Context<Self>);
  pub(crate) fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>);
  pub(crate) fn close_popup(&mut self, cx: &mut Context<Self>);
  ```
- Actions added to `ACTIONS`: `("marketdata::menu", "Actions menu")`, `("marketdata::menu_down", "Menu: next")`, `("marketdata::menu_up", "Menu: previous")`, `("marketdata::menu_pick", "Menu: pick")`, `("marketdata::menu_close", "Menu: close")`, `("marketdata::load_underlying", "Load underlying…")` (Task 7 implements it; here it dispatches a `not built yet` notice), `("marketdata::upload", "Upload")` (dispatch → `not built yet`), `("marketdata::revert", "Revert edits")`, `("marketdata::rebase", "Rebase")`, `("marketdata::discard", "Discard edits")`. The last four route to the existing `revert`/`rebase`/`discard` methods so a menu row and a `:` line take one path.

- [ ] **Step 1: Failing pure tests (`menu.rs`)**

```rust
    fn inputs(badge: DraftBadge) -> MenuInputs<'static> {
        MenuInputs { badge, has_key: true, upload_built: false, kind_title: "CVI", kind_actions: CVI.actions }
    }
    fn titles(rows: &[MenuRow]) -> Vec<String> {
        rows.iter().map(|r| match r { MenuRow::Action { title, .. } => title.to_string(), MenuRow::Separator => "—".into(), MenuRow::Section(s) => format!("[{s}]") }).collect()
    }
    fn enabled(rows: &[MenuRow], title: &str) -> Result<(), &'static str> {
        rows.iter().find_map(|r| match r { MenuRow::Action { title: t, enabled, .. } if t == title => Some(enabled.clone()), _ => None }).unwrap()
    }

    #[test]
    fn a_clean_draft_offers_load_and_greys_upload_and_revert() {
        let rows = rows(&inputs(DraftBadge::Clean));
        assert_eq!(titles(&rows), vec!["Load underlying…", "Upload", "Revert edits", "—", "[CVI]", "Reanchor", "Recalc forward"]);
        assert_eq!(enabled(&rows, "Load underlying…"), Ok(()));
        assert_eq!(enabled(&rows, "Upload"), Err("not built yet"));
        assert_eq!(enabled(&rows, "Revert edits"), Err("nothing to revert"));
        assert_eq!(enabled(&rows, "Reanchor"), Err("not built yet"));
    }

    #[test]
    fn a_dirty_draft_greys_load_and_a_built_upload_is_live() {
        let mut i = inputs(DraftBadge::Dirty);
        i.upload_built = true;
        let rows = rows(&i);
        assert_eq!(enabled(&rows, "Load underlying…"), Err("revert or upload first"));
        assert_eq!(enabled(&rows, "Upload"), Ok(()));
        assert_eq!(enabled(&rows, "Revert edits"), Ok(()));
        let clean = rows(&MenuInputs { upload_built: true, ..inputs(DraftBadge::Clean) });
        assert_eq!(enabled(&clean, "Upload"), Err("nothing to upload"));
    }

    #[test]
    fn behind_shows_rebase_and_discard_and_greys_upload() {
        let mut i = inputs(DraftBadge::Behind { newer: "2026-09-14T14:09:00Z".into() });
        i.upload_built = true;
        let rows = rows(&i);
        assert!(titles(&rows).iter().any(|t| t.starts_with("Rebase onto ")));
        assert!(titles(&rows).contains(&"Discard edits".to_string()));
        assert_eq!(enabled(&rows, "Upload"), Err("rebase or discard first"));
    }

    #[test]
    fn a_spec_with_no_kind_actions_has_no_section() {
        let mut i = inputs(DraftBadge::Clean);
        i.kind_actions = &[];
        let rows = rows(&i);
        assert!(!titles(&rows).iter().any(|t| t == "—" || t.starts_with('[')));
    }

    #[test]
    fn navigation_skips_separators_and_starts_on_the_first_enabled_row() {
        let rows = rows(&inputs(DraftBadge::Clean));
        assert_eq!(first_enabled(&rows), 0);
        let last_action = rows.len() - 1;
        assert_eq!(step(&rows, 2, 1), 5, "over the separator and the section header");
        assert_eq!(step(&rows, 5, -1), 2);
        assert_eq!(step(&rows, last_action, 3), last_action);
        assert_eq!(step(&rows, 0, -1), 0);
    }
```

`content.rs`:

```rust
    #[test]
    fn dot_and_u_bind_in_normal_mode_and_the_menu_keys_in_menu_mode() {
        let doc = fragment_doc(CVI.kind, DEFAULT_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let normal = [KeyContext::new("workspace"), KeyContext::new("tile"), KeyContext::new("marketdata").pair("mode", "normal").counts()];
        let menu = [KeyContext::new("workspace"), KeyContext::new("tile"), KeyContext::new("marketdata").pair("mode", "menu").counts()];
        for (stack, spec, expected) in [
            (&normal, ".", "marketdata::menu"), (&normal, "u", "marketdata::load_underlying"),
            (&menu, "j", "marketdata::menu_down"), (&menu, "k", "marketdata::menu_up"),
            (&menu, "enter", "marketdata::menu_pick"), (&menu, "escape", "marketdata::menu_close"), (&menu, ".", "marketdata::menu_close"),
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_kind_actions_are_registered() {
        let mut registry = ActionRegistry::default();
        let (data, _rx) = DataHandle::for_tests();
        MarketDataFactory::new(data, &CVI, Duration::from_secs(60)).register_actions(&mut registry);
        for a in CVI.actions { assert!(registry.get(&ActionId(a.id.to_string())).is_some(), "{}", a.id); }
    }
```

(Use the registry's actual lookup method — check `geode_shell::actions::ActionRegistry`.)

- [ ] **Step 2: Run to see failures**

Run: `cargo test -p geode-marketdata menu::tests content::tests`
Expected: compile errors.

- [ ] **Step 3: Implement the cores**

`spec.rs`:

```rust
/// A verb this document kind owns (spec 2026-09-14 §6.3): listed in the
/// panel's action menu under the kind's own section, registered as an
/// action so the palette and a keymap reach it. `built: false` paints
/// greyed "not built yet"; when built it is an egress REQUEST to the
/// upstream system (charter: Geode computes nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindAction { pub id: &'static str, pub title: &'static str, pub built: bool }
// PanelSpec: pub actions: &'static [KindAction],
// CVI: actions: &[
//     KindAction { id: "marketdata::cvi_reanchor", title: "Reanchor", built: false },
//     KindAction { id: "marketdata::cvi_recalc_forward", title: "Recalc forward", built: false },
// ],
```

`core/menu.rs`:

```rust
pub fn rows(i: &MenuInputs) -> Vec<MenuRow> {
    let dirty = !matches!(i.badge, DraftBadge::Clean);
    let behind = matches!(i.badge, DraftBadge::Behind { .. });
    let mut out = vec![
        action("marketdata::load_underlying", "Load underlying…", "u",
            if dirty { Err("revert or upload first") } else { Ok(()) }),
        action("marketdata::upload", "Upload", ":upload",
            if !i.upload_built { Err("not built yet") } else if behind { Err("rebase or discard first") } else if !dirty { Err("nothing to upload") } else { Ok(()) }),
    ];
    if let DraftBadge::Behind { newer } = &i.badge {
        out.push(action("marketdata::rebase", &format!("Rebase onto {}", local_hhmm(newer)), ":rebase", Ok(())));
        out.push(action("marketdata::discard", "Discard edits", ":discard", Ok(())));
    }
    out.push(action("marketdata::revert", "Revert edits", ":revert", if dirty { Ok(()) } else { Err("nothing to revert") }));
    if !i.kind_actions.is_empty() {
        out.push(MenuRow::Separator);
        out.push(MenuRow::Section(i.kind_title.into()));
        for k in i.kind_actions {
            out.push(action(k.id, k.title, "", if k.built { Ok(()) } else { Err("not built yet") }));
        }
    }
    out
}
```

with `action(id, title, hint, enabled)` a constructor, `first_enabled` the first `Action` index with `Ok` (0 if none), and `step` moving `delta` steps over `Action` rows only, clamped.

- [ ] **Step 4: `popup.rs` and the tile**

`popup.rs`:

```rust
pub(crate) enum Popup { Menu(MenuState) }
pub(crate) struct MenuState { pub rows: Vec<MenuRow>, pub highlighted: usize }

pub(crate) fn render_menu(m: &MenuState, theme: &Theme, tile: &Entity<MarketDataTile>, tile_id: u64) -> impl IntoElement {
    let mut list = v_flex().min_w(px(240.)).py_1().rounded_md().border_1().border_color(theme.border)
        .bg(theme.popover).text_color(theme.popover_foreground).text_sm().shadow_md()
        .debug_selector(move || format!("marketdata-menu-{tile_id}"))
        .on_mouse_down_out({ let tile = tile.clone(); move |_, _, cx| tile.update(cx, |t, cx| t.close_popup(cx)) });
    for (i, row) in m.rows.iter().enumerate() {
        list = list.child(match row {
            MenuRow::Separator => div().h(px(1.)).my_1().bg(theme.border).into_any_element(),
            MenuRow::Section(s) => div().px_3().pt_1().text_xs().text_color(theme.muted_foreground).child(s.clone()).into_any_element(),
            MenuRow::Action { title, hint, enabled, .. } => {
                let disabled = enabled.is_err();
                let reason: SharedString = match enabled { Err(r) => (*r).into(), Ok(()) => hint.clone() };
                h_flex().px_3().py_0p5().justify_between().gap_4()
                    .when(i == m.highlighted, |d| d.bg(theme.list_active))
                    .text_color(if disabled { theme.muted_foreground } else { theme.popover_foreground })
                    .debug_selector(move || format!("marketdata-menu-row-{tile_id}-{i}"))
                    .on_mouse_down(MouseButton::Left, { let tile = tile.clone(); move |_, window, cx| { cx.stop_propagation(); tile.update(cx, |t, cx| t.menu_pick(i, window, cx)) } })
                    .child(title.clone())
                    .child(div().text_color(theme.muted_foreground).child(reason))
                    .into_any_element()
            }
        });
    }
    deferred(anchored().anchor(Corner::TopRight).position_mode(AnchoredPositionMode::Local).snap_to_window_with_margin(px(8.)).child(list)).with_priority(1)
}
```

`anchored().position_mode(Local)` with no `.position(..)` positions at the parent's origin; the parent is a zero-size `div().relative()` placed at the header's right edge (`render` puts it as the last child of the header row, after `⋯`, with `.absolute().right_0().top(px(22.))`). Confirm against `anchored.rs` at the pinned rev while implementing (`AnchoredPositionMode::Local` interprets `position` relative to the parent's bounds; with `position` unset it uses the parent's origin) and adjust the anchor corner so the list hangs down-left from `⋯`.

`tile.rs`:

- `popup: Option<Popup>`.
- `key_context()`: `if self.editor.is_some() { "insert" } else if matches!(self.popup, Some(Popup::Menu(_))) { "menu" } else { "normal" }`.
- `toggle_menu(cx)`: if `Some(Menu)` → `close_popup`; else build `MenuState { rows: menu::rows(&MenuInputs { badge: self.draft.badge(), has_key: self.key.is_some(), upload_built: false, kind_title: self.spec.title, kind_actions: self.spec.actions }), highlighted: first_enabled(&rows) }`; if an editor is open, `close_editor` first (needs `window` — make `toggle_menu` take `window`). `cx.notify()`.
- `close_popup(cx)`: `self.popup = None; cx.notify()`.
- `menu_pick(index, window, cx)`: read the row's `id` and `enabled`; `Err(reason)` → `self.notice = Some(reason.into()); self.rebuild_chrome(); cx.notify()` and keep the popup; `Ok` → `close_popup`, then `self.dispatch(&id, None, window, cx)`.
- `dispatch`: at the top, after the prefix strip: `if !matches!(verb, "menu" | "menu_down" | "menu_up" | "menu_pick" | "menu_close") && self.popup.is_some() { self.close_popup(cx); }` — the spec's "any other action closes it first". Arms: `"menu"` → `toggle_menu`; `"menu_close"` → `close_popup` (return `true` only if one was open); `"menu_down"`/`"menu_up"` → `step(rows, highlighted, ±n)`; `"menu_pick"` → `menu_pick(highlighted)`; `"revert"`/`"rebase"`/`"discard"` → the existing methods, an `Err` becoming the notice; `"upload"`, `"load_underlying"` (until Task 7) and any `spec.actions` id → notice `"not built yet"`. For the kind ids, match `verb` against `self.spec.actions.iter().find(|a| a.id.strip_prefix("marketdata::") == Some(verb))`.
- `render`: after the header, `.when_some(self.popup.as_ref(), |el, p| match p { Popup::Menu(m) => el.child(render_menu(m, theme, &cx.entity(), self.id.0)) })`; pass `menu_open` to `header::render`; `⋯`'s `on_mouse_down` → `cx.stop_propagation(); tile.update(cx, |t, cx| t.toggle_menu(window, cx))`. NOTE: stopping propagation on `⋯` means the shell's tile mouse-down (focus re-arm) does not run for that click — acceptable, the click changes nothing about focus; but verify `pending_focus_restore` is not needed by running the shell's existing window tests unchanged.
- `command`: `Command::Menu` → `toggle_menu` (needs `window`: `TileContent::command` has one — thread it through `MarketDataTile::command(line, window, cx)`; update `Harness::command`).
- `content.rs`: `ACTIONS` gains the ten ids; `register_actions` also loops `self.spec.actions`; fragment gains

```toml
"." = "marketdata::menu"
"u" = "marketdata::load_underlying"

[[bindings]]
context = "marketdata && mode == menu"
[bindings.keys]
"j" = "marketdata::menu_down"
"k" = "marketdata::menu_up"
"enter" = "marketdata::menu_pick"
"escape" = "marketdata::menu_close"
"." = "marketdata::menu_close"
```

- [ ] **Step 5: Window tests**

```rust
    #[gpui::test]
    fn dot_opens_the_menu_and_escape_closes_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        draw(&mut vcx);
        assert!(vcx.debug_bounds(&format!("marketdata-menu-{TILE}")).is_some(), "the popup is painted");
        h.dispatch(&mut vcx, "menu_close", None);
        assert_eq!(h.mode(&vcx), "normal");
    }

    #[gpui::test]
    fn enter_on_a_greyed_row_notices_and_keeps_the_menu(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", None);   // Upload (greyed: not built yet)
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(h.mode(&vcx), "menu");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)), Some("not built yet".into()));
    }

    #[gpui::test]
    fn an_unrelated_action_closes_the_menu_first(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Cell { row: 1, col: 0 });
    }

    #[gpui::test]
    fn a_menu_row_click_dispatches_and_a_click_outside_closes(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 1").unwrap();
        h.dispatch(&mut vcx, "menu", None);
        let revert = centre_of(&mut vcx, &format!("marketdata-menu-row-{TILE}-2")); // Revert edits on a dirty, not-behind draft
        click_at(&mut vcx, revert, 1);
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        assert_eq!(h.mode(&vcx), "normal");
        h.dispatch(&mut vcx, "menu", None);
        let cell = centre_of(&mut vcx, &format!("marketdata-cell-1-1"));
        click_at(&mut vcx, cell, 1);
        assert_eq!(h.mode(&vcx), "normal");
    }

    #[gpui::test]
    fn the_menu_button_toggles_the_menu(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let button = centre_of(&mut vcx, &format!("marketdata-menu-button-{TILE}"));
        click_at(&mut vcx, button, 1);
        assert_eq!(h.mode(&vcx), "menu");
        click_at(&mut vcx, button, 1);
        assert_eq!(h.mode(&vcx), "normal");
    }

    #[gpui::test]
    fn a_kind_action_answers_not_built_yet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "cvi_reanchor", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)), Some("not built yet".into()));
    }
```

`centre_of` takes `&'static str` today; widen it to `&str` (or `impl Into<String>`).

- [ ] **Step 6: Run**

Run: `cargo test -p geode-marketdata && cargo test -p geode-shell && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (the shell suite too, since `ACTIONS`/fragment changes are visible to the roster tests).

- [ ] **Step 7: Harness entries and commit**

```bash
run_mutation "mdmenu: upload is greyed while behind" \
  crates/geode-marketdata/src/core/menu.rs \
  'else if behind { Err("rebase or discard first") }' \
  'else if false { Err("rebase or discard first") }' \
  geode-marketdata behind_shows_rebase_and_discard_and_greys_upload

run_mutation "mdmenu: an unrelated action closes the popup first" \
  crates/geode-marketdata/src/tile.rs \
  '<the exact close-first guard line in dispatch>' \
  '<the guard with && false appended to its condition>' \
  geode-marketdata an_unrelated_action_closes_the_menu_first

run_mutation "mdmenu: a greyed row is a notice, not a dispatch" \
  crates/geode-marketdata/src/tile.rs \
  '<the Err(reason) arm of menu_pick>' \
  '<the same arm falling through to close_popup + dispatch>' \
  geode-marketdata enter_on_a_greyed_row_notices_and_keeps_the_menu
```

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: the action list — a tile-owned popup on . and ⋯, kind actions reserved"
```

---

### Task 7: Load underlying — `Popup::Picker`, `u`

**Files:**
- Modify: `crates/geode-marketdata/src/popup.rs` (`Popup::Picker(PickerState)`, `render_picker`)
- Modify: `crates/geode-marketdata/src/tile.rs` (`open_picker`, `picker_pick`, `key_context`, `dispatch`, `close_editor`-style teardown)
- Modify: `crates/geode-marketdata/src/content.rs` (fragment: insert-mode `up`/`down` for the picker? — no: reuse `menu_down`/`menu_up` by binding `down`/`up`/`ctrl+j`/`ctrl+k` in `marketdata && mode == insert` to `marketdata::menu_down`/`menu_up`; `enter`/`escape` there already route to `commit`/`cancel`, which the tile forwards to the picker while one is open)
- Test: `tile.rs` window tests; `content.rs` fragment test

**Interfaces:**
- Produces:
  ```rust
  pub(crate) struct PickerState { pub input: Entity<InputState>, pub all: Vec<String>, pub ranked: Vec<usize>, pub highlighted: usize }
  // tile.rs
  pub(crate) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>);
  pub(crate) fn picker_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>);
  ```
- `key_context()` reports `insert` while a picker is open (its `Input` holds focus). `dispatch("commit")` while a picker is open loads the highlighted row; `dispatch("cancel")` closes it (blur then drop, `close_editor`'s order); `menu_down`/`menu_up` move its highlight.

- [ ] **Step 1: Failing tests**

`content.rs` fragment test: add to the insert-mode assertions `("down", "marketdata::menu_down")`, `("up", "marketdata::menu_up")`, `("ctrl+j", "marketdata::menu_down")`, `("ctrl+k", "marketdata::menu_up")` in `marketdata && mode == insert`.

`tile.rs`:

```rust
    #[gpui::test]
    fn u_opens_the_picker_typing_filters_and_enter_loads(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| { d.note_catalog(catalog(&["SPX.Z", "NKY.Z", "SX5E.Z"])); cx.notify(); });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        h.set_picker_text(&mut vcx, "nky");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        let req = h.document_request().expect("the pick requests its document");
        assert_eq!(req.document_key, vec!["NKY.Z".to_string()]);
        assert!(h.tile.read_with(&vcx, |t, _| t.header_texts()).contains(&"NKY.Z".to_string()));
    }

    #[gpui::test]
    fn the_picker_is_refused_while_the_draft_has_edits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 1").unwrap();
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(h.tile.read_with(&vcx, |t, _| t.notice().unwrap_or("").contains(":revert")));
    }

    #[gpui::test]
    fn escape_closes_the_picker_and_gives_focus_up(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(vcx.update(|window, cx| window.focused(cx).is_none()), "blurred before dropped");
    }

    #[gpui::test]
    fn opening_the_picker_asks_for_a_fresh_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        let before = h.diagnostics.read_with(&vcx, |d, _| d.catalog_requests());
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.diagnostics.read_with(&vcx, |d, _| d.catalog_requests()), before + 1);
    }
```

(`catalog(&[..])` exists in the tests; find how `key_completions_come_from_the_catalog` installs one and mirror it — `note_catalog` and `catalog_requests` are placeholders for those actual doors. `set_picker_text` is a new harness helper writing the picker's `InputState` the way `set_editor` writes the cell editor's.)

- [ ] **Step 2: Run to see failures**

Run: `cargo test -p geode-marketdata u_opens the_picker escape_closes_the_picker opening_the_picker`
Expected: FAIL/compile errors.

- [ ] **Step 3: Implement**

`popup.rs`:

```rust
pub(crate) enum Popup { Menu(MenuState), Picker(PickerState) }
pub(crate) struct PickerState { pub input: Entity<InputState>, pub all: Vec<String>, pub ranked: Vec<usize>, pub highlighted: usize }

impl PickerState {
    /// Re-rank against the field's text; the highlight resets to the top.
    pub(crate) fn refilter(&mut self, query: &str) {
        self.ranked = geode_shell::listfilter::rank(&self.all, query).into_iter().map(|r| r.row).collect();
        self.highlighted = 0;
    }
    pub(crate) fn highlighted_key(&self) -> Option<&str> { self.ranked.get(self.highlighted).map(|&i| self.all[i].as_str()) }
}

pub(crate) fn render_picker(p: &PickerState, theme: &Theme, tile: &Entity<MarketDataTile>, tile_id: u64) -> impl IntoElement {
    // same anchored/deferred shell as render_menu; first child an Input::new(&p.input) row,
    // then one row per `ranked` index (empty ⇒ one muted row "no underlyings known"),
    // each row on_mouse_down → tile.picker_pick(i, window, cx) with stop_propagation,
    // on_mouse_down_out → tile.close_popup_with_window(window, cx)
}
```

`tile.rs`:

- `open_picker(window, cx)`: refuse with `format!("{} pending — :revert first", self.draft.count_phrase())` while dirty; close an open editor first; `request_catalog(cx)`; build `all = self.catalog_keys(cx)`, `input = cx.new(|cx| InputState::new(window, cx))`, subscribe to its `InputEvent::Change` (`cx.subscribe(&input, |this, _, ev, cx| if let InputEvent::Change = ev { … refilter(&text) … cx.notify() })` — check the exact event variant in gpui-component's `input` module), focus it, `ranked = 0..all.len()`, `self.popup = Some(Popup::Picker(..))`.
- `picker_pick(index, window, cx)`: take the key at `ranked[index]`, `close_popup_with_window` (blur + drop), then `set_key(split_key(&key), cx)` with the `Err` as notice.
- `close_popup_with_window(window, cx)`: if `Picker`, `window.blur(cx)` first; then `self.popup = None`.
- `dispatch`: `"commit"` with a picker open → `picker_pick(highlighted)`; `"cancel"` with a picker open → `close_popup_with_window`; `menu_down`/`menu_up` with a picker open move `highlighted` over `ranked` (clamped); `"load_underlying"` → `open_picker`. The close-first guard from Task 6 must not close a picker on `commit`/`cancel`/`menu_down`/`menu_up` — extend its exclusion list.
- `key_context()`: `insert` if `editor.is_some() || matches!(popup, Some(Popup::Picker(_)))`.
- Catalog delivery while the picker is open: in the diagnostics observer that already exists for the catalog (find where `catalog_keys` consumers react), refresh `all` and `refilter` with the current text.
- `content.rs` fragment, insert block:

```toml
"down" = "marketdata::menu_down"
"up" = "marketdata::menu_up"
"ctrl+j" = "marketdata::menu_down"
"ctrl+k" = "marketdata::menu_up"
```

(`menu_down`/`menu_up` with neither popup open return `false`.)

- [ ] **Step 4: Run**

Run: `cargo test -p geode-marketdata && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 5: Harness entries and commit**

```bash
run_mutation "mdpicker: the picker is refused while the draft has edits" \
  crates/geode-marketdata/src/tile.rs \
  '<the dirty guard at the top of open_picker>' \
  '<the guard with && false>' \
  geode-marketdata the_picker_is_refused_while_the_draft_has_edits

run_mutation "mdpicker: closing the picker blurs before dropping" \
  crates/geode-marketdata/src/tile.rs \
  '<the window.blur(cx) line in close_popup_with_window>' \
  '<that line removed>' \
  geode-marketdata escape_closes_the_picker_and_gives_focus_up
```

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: load underlying — the picker popup on u and the menu row"
```

---

### Task 8: Docs, harness count, spec as-built, final checks

**Files:**
- Modify: `CLAUDE.md` (the Part 3 paragraph gains a "Header, attributes and actions (2026-09-14)" sentence block; the command table's harness count)
- Modify: `docs/superpowers/specs/2026-09-14-geode-market-data-panel-header-design.md` (new §11 "As built")
- Modify: `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md` (§8.2/§8.3 get a one-line pointer to the new spec)

- [ ] **Step 1: Full verification**

Run, in order, and paste the tail of each into the commit message body if anything is notable:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
cargo bench --workspace --no-run
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed
```

Expected: all green; every new entry `caught`.

- [ ] **Step 2: CLAUDE.md**

Add after the Part 3 paragraph's "Editing is keyboard-only…" sentence a block covering: underlying vs key at the tier boundary; `Cursor::{Cell, Attr}` and `last_grid_col`; one draft with `attrs`; `HeaderModel::prepare` as the only place header text is formatted; `Popup::{Menu, Picker}` with the close-first rule and the two `key_context` modes (`menu` for the menu, `insert` for the picker); `KindAction` as reserved egress verbs; the harness entry count (count the `run_mutation` lines: `grep -c '^run_mutation' scripts/mutation-check.sh`).

- [ ] **Step 3: Spec as-built**

Append `## 11. As built (2026-09-14)` to the new spec: `clear_selection` resolved §5.1's open note; the picker's insert-mode routing (`commit`/`cancel`/`menu_down`/`menu_up`); the `⋯` click stops propagation; anything the implementation had to decide that the spec left open. In the documents spec, add to §8.2 and §8.3 one line each: "Superseded in part by `2026-09-14-geode-market-data-panel-header-design.md` (header, attributes, actions)."

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md docs/superpowers/specs
git commit -m "docs: market-data panel header as built; harness count"
```

---

## Self-review

**Spec coverage.** §3 vocabulary → Task 1 (command, session, header string), Task 4 (badge/underlying), Task 7 (picker title). §4 header row → Task 4 (all seven elements; `⋯` click in Task 6). §5.1 cursor → Task 5 (every motion, click, `clear_selection`). §5.2 editing → Task 5 (`parse_attr`, insert-mode refusal, `:set`). §5.3 draft → Task 3 (attrs, rebase, toml, `count_phrase`), header `edited` → Tasks 2–3. §6.1 popup + close-first → Task 6. §6.2 rows → Task 6 (`menu::rows`). §6.3 kind actions → Task 6. §7 picker → Task 7. §8 session/commands/fragment → Tasks 1, 5, 6, 7. §9 tests → each task; the bundled-theme sweep for the dot → Task 4. §10 exclusions respected. Part 4's `upload_built` flag is threaded from the start so Part 4 flips one bool.

**Placeholders.** Four harness anchors in Tasks 5–7 are written as `<…>` because they quote lines whose exact text the implementer writes; each names the function and the line's role, and `--anchors-only` refuses a stale one. Everything else is literal.

**Type consistency.** `HeaderAttr { column, label, ty }` (Task 2) is what `parse_attr(text, attr.ty)` (Task 5) and `header_of` (Tasks 2–3) read. `HeaderCell { column, label, text, edited }` is what `HeaderModel.attrs`, `rebase`'s declared set and `yank_text` read. `DraftBadge` (Task 3) feeds `HeaderModel::prepare` (Task 4) and `MenuInputs` (Task 6). `Cursor`/`Grid`/`Motion`/`step`/`clamp` (Task 5) are the only cursor vocabulary; the delegate mirrors `Option<(usize, usize)>`. `Popup::{Menu, Picker}`, `toggle_menu`, `menu_pick`, `close_popup`, `close_popup_with_window`, `open_picker`, `picker_pick` (Tasks 6–7) are named identically in the fragment test, the dispatch arms and the render handlers.
