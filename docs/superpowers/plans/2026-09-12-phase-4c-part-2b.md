# Phase 4c, Part 2b — Text Entry, Schema, Sources, Per-Field Diagnostics, Rejected Reloads, Drift

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish Phase 4c's object dialog: a committed text-entry vocabulary generalised from the Groupings chain field, the read-only Schema inspector, the Sources adapter over a flat dataset·source list, diagnostics that land on the field row they name, a signal when the merge rejects a write the dialog made, and drift detection through `overrides.toml`.

**Architecture:** `shell::objectdialog` stays one pure core (`mod.rs`) plus one gpui shell (`render.rs`) over `Domain` adapters. This plan widens the pure core in five places — `Draft::text_entry` replacing `chain_entry`, `Domain::{writable, text_editable, parse_text, prefix_fn}`, `Field.layer`, `ObjectRow.prefix`, and `Draft::row_for_path` — adds two adapters (`schema.rs`, `sources.rs`), one config doc (`overrides`), and fills `Diagnostic.path` in every `geode-core` reader. Nothing new touches the data service: a Sources write reaches it through the existing restart-required stripe.

**Tech Stack:** Rust, gpui + gpui-component (pinned rev `0e2fb7a`), `toml_edit`, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` — **§19 is this plan's whole brief** (§19.1 text entry, §19.3 Sources, §19.4 Schema, §19.5 per-field diagnostics, §19.6 drift and `ReloadRejected`, §19.7 sequencing). §3, §4, §5.2–5.3, §7.1, §8.3, §8.5, §9, §16–§18 are background. §19.2 records that width is NOT built here (it moved to Part 2c, §20).
**Also binding:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` (the two-mode vocabulary, and §16's rule that `dialog::sync_dialog_text` is the only thing that moves focus or writes the shared `Input`).

## Global Constraints

- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. Run all five before every commit that touches Rust.
- **TDD**: write the failing test, run it, watch it fail for the right reason, then implement.
- **A mutation entry for every behaviour changed**, appended to `scripts/mutation-check.sh` immediately after the last `run_mutation` entry (currently the `dialog: enter_filter_by_mouse ignores the settings dialog` entry at line ~7420, before the `if [[ -n "$changed_ref" ]]` block), each naming the test expected to catch it as its 6th argument. **Commit before mutating.** Before adding an anchor, confirm it occurs exactly once as a substring of its file (`grep -c -F '<anchor>' <file>` must print `1`). `zsh scripts/mutation-check.sh --anchors-only` must exit 0 before every commit — a refactor silently orphans existing anchors in any file you touch.
- **Run the harness DETACHED** (`nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut.log 2>&1 &`), never in a timed foreground call. Verify `git status --porcelain` is clean afterwards.
- Nothing may stall the render thread; config writes go on the background executor (they already do — every write here goes through `apply::queue_batch`).
- **Never a raw colour** — every colour from `cx.theme()` tokens.
- `geode-shell` never depends on `geode-data`. No crate but `geode-data` opens a file or socket, except `geode-shell` writing its own config under `user_dir` through `config_write`.
- Doc comments explain WHY, densely. **A comment that contradicts the code is a defect.** Several existing comments say `Diagnostic::path` is filled by no reader, that `drifted` is always `false`, that `chain_entry` is Groupings-only, that `i` has no row to act on, that `Domain::writable()` is unbuilt — each sentence this plan makes false must be rewritten in the task that makes it false.
- **Renaming an existing object stays unbuilt** (spec §8.2 ruling). **Width is not built here** (§19.2).
- The dialogs are palette-only; no new default key binding to *open* one.
- **A new transition site is a pure mutation of `mode`/`query`/`text_entry` and never calls `focus` or `set_value` itself** — `dialog::sync_dialog_text` does that on the handler's return (interaction-model §16).
- **Every `Field { .. }` and `ObjectRow { .. }` literal in the crate compiles or the task is not done** — Tasks 2 and 3 add fields to those structs; let the compiler list the sites and fill every one.

## As-built vocabulary this plan builds on

```rust
// crates/geode-shell/src/shell/objectdialog/mod.rs (as it stands after Part 2 refinement + §18.8/§18.9)
pub enum Domain { Views, Groupings, Scopes }
pub enum Stage { Browse, Naming, Edit { object: String } }
pub struct ObjectRow { pub name: String, pub summary: String, pub layer: Option<Layer>,
                       pub overridden: bool, pub drifted: bool }
pub enum Destination { Doc, Presentation }
pub struct ListItem { pub name: String, pub included: bool, pub width: Option<f32>, pub kind: Option<String> }
pub enum FieldKind { Text(String), Number { value: i64, min: i64, max: i64 }, Bool(bool),
                     Choice { options: Vec<String>, selected: usize },
                     MultiChoice { options, ticked }, OrderedList { items: Vec<ListItem>, available: Option<Vec<ListItem>> } }
pub struct Field { pub key: String, pub label: String, pub kind: FieldKind, pub dest: Destination }
pub enum EditRow { Field(usize), Item { field: usize, item: usize }, Available { field: usize, item: usize } }
pub enum Confirm { Delete, Revert, Fork, Overwrite { forks: bool } }
pub enum Step { Changed, Inert, Refused(String) }
pub struct Draft { pub name, pub is_new, pub fields, pub source: toml::Table, baseline, baseline_source,
                   pub selected: usize, pub query: String, pub diagnostics: Vec<Diagnostic>,
                   pub confirm: Option<Confirm>, pub chain_entry: bool }
// Draft: rows() row_label(row) visible_rows() selected_row() is_dirty() mark_saved() revert_to_baseline()
//        list_items(key) available_items(key) choice(key) toggle_selected() toggle_selected_back()
//        writes_by_destination() new_object(name, fields, source)
// groupings.rs (Draft impl): begin_chain_entry() cancel_chain_entry() complete_chain() -> bool apply_chain() -> Step
//        chain_candidates(draft) -> Vec<Ranked>; parse_chain(text); trailing_segment(text)
pub struct ObjectDialogState { pub domain, pub stage, pub selected, pub query, pub mode: DialogMode,
                               pub notice: Option<String>, pub draft: Option<Draft> }
// ObjectDialogState: new(domain) enter_edit(config, object) enter_edit_with(draft) leave_edit()
//        set_query(q) effective_query() begin_naming() cancel_naming() has_previous_stage()
// Domain: doc() title() crumb_noun() summary_fn() presentation_doc() roster() objects(config)
//         name_taken(config, name) fields(config, object) draft(config, object) new_draft(config, name)
//         to_table(draft, dest) -> toml_edit::Item  validate(draft, config) -> Vec<Diagnostic>
// free fns: derive_rows(config, doc, presentation_doc, roster, summary) -> Vec<ObjectRow>
//           personalised_names(config, presentation_doc); searchable_text(row) = "{name} {summary}"
//           visible_rows(state, rows); filtered_position(visible, rows, name); object_text(name, item)
//           set_object(document, name, item); toml_table_to_edit(&toml::Table); toml_value_to_item(&toml::Value)
// apply.rs: pub type ObjectEdit = Option<toml::Value>; enum ObjectWrite { Set(v), Remove, Nothing }
//           object_value(object, item, dest); docs_with_object(docs, user_dir, doc, object, value)
//           edits_for(shell); would_fork(shell, domain); blocking_diagnostic(shell)
//           commit_edit(shell, cx) -> Option<String>; commit_removal(shell, keys, cx); commit_create(shell, cx)
//           queue_batch(shell, edits, user_dir, delay, cx); schedule_flush(..); promote(shell, seq, cx)
//           apply_in_memory(shell, user_dir, edits, cx) -> calls shell.apply_reload(Config::from_docs(docs), cx)
//           config_with_pending(shell); finish_flush(shell, seq, outcome, cx); run_writes(user_dir, edits)
//           revert_failed_write(shell, message, cx); WRITE_DEBOUNCE = 250 ms
// render.rs: open(view, domain, window, cx); crumb_text(shell); handle_key → handle_browse_key/handle_edit_key;
//            handle_chain_key(shell, ks, cx) (dispatched when draft.chain_entry, ahead of the filter branch);
//            enter_edit_stage(shell, name, new: Option<Draft>, cx); jump_to_slot; edit_commit_notice(shell);
//            refuse_step; draft_mut(shell); maybe_refresh_available; selected_field_is_steppable(draft);
//            set_notice(shell, s); commit_or_confirm(shell, cx); scroll_to_cursor; revalidate(shell);
//            leave_edit; editing_row(shell) -> Option<ObjectRow>; arm_delete; arm_revert; arm_overwrite;
//            run_confirmed(shell, confirm, cx); removal_edits(shell, docs); actions(shell) -> Vec<Action>;
//            object_word(domain); build(); build_edit(); section_header_text(domain, own); field_value(field);
//            action_bar; confirm_row; create_from_name(shell, cx); derive_rows(shell) -> Vec<ObjectRow>;
//            on_row_clicked; on_edit_row_clicked; on_tick_clicked; on_row_dropped; on_completion_clicked; press_verb
// dialog.rs: set_title_extra(view, build); filter_row(input, frozen, cx); name_row(input, label, cx);
//            mode_pill(mode, cx); chain_pill(cx); state_pill(label, typing, cx) [private]; badge(label, fg, border, selector, cx)
//            sync_dialog_text(shell, window, cx); open_shell_dialog_with_key(..)
// dialogmode.rs: NormalCommand { Nav, EnterFilter, Commit, Toggle, ToggleBack, MoveItem(i32), EditText, Digit(u8), Verb(char) }
// keybindings_view.rs: highlighted_text(text, indices, primary) -> AnyElement   palette.rs: split_label_indices(indices, title_len)
// defaults.rs: action(reg, "config::views", "Edit views", "Configuration")  input.rs: dispatch `config::*` → objectdialog::render::open
// shell/mod.rs: ShellEvent { ConfigReloaded, RestartRequired(String), DistinctRequested(..) }
//               ShellView { services, object_dialog, object_dialog_scroll, dialog_input, frame, diagnostics,
//                           config_write_error: Option<String>, last_reload: reload::ReloadOutcome, user_dir, .. }
// hot_reload.rs: apply_reload(&mut self, new_config, cx) — reload::decide → ReloadOutcome::{Applied{warnings}, KeptLastGood{errors: Vec<String>}}
// geode-core: Diagnostic { severity, layer, file, message, path: Option<String> } + with_path(); Display appends nothing for path yet
//             Layer { Builtin, Desk, User } + name(); Config::{doc, layered_docs, explain(doc, path), all_docs, from_docs}
//             config::merge::atomic_depth(doc) [private]; check_object_name(name); load_views(config)
//             readers: ViewSpec::from_doc, ViewPresentationSpec::from_doc + ViewPresentation::apply, GroupingSlots::from_doc,
//                      saved_scopes_from_doc, SourceSpec::from_doc(doc, schema), SchemaSpec::from_doc, DerivedDimensions::from_doc
//             source_config: parse_duration(s) -> Option<Duration>; DEFAULT_POLL = 30s; DEFAULT_PENDING_TIMEOUT = 600s (both private today)
//             schema: ColumnType { Utf8, F64, I64, Date, Timestamp, Bool }; ColumnRole { Key, Dimension{grain}, Measure{grain, aggregate}, Attribute{grain} }
//                     ColumnSpec { name, source_name, ty, required, textual, categorical, role }; Grain::short(); DatasetSpec::grains()
// tests: crates/geode-shell/src/shell/tests/objectdialog.rs — dialog_test_shell_with(cx, services, action),
//        dialog_test_shell_in_dir(cx, services, dir, action), dialog_state(shell, cx, f), edit_draft(shell, cx, f),
//        dialog_input_text(shell, cx), dialog_filter_is_focused(shell, cx), flush_config_write(cx),
//        services_with_views(), services_with_slot_3(dims); test_services() in tests/mod.rs; BUILTIN_KEYMAP
```

---

## File map

| File | Responsibility after this plan |
|---|---|
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | `TextEntry`, `Draft::text_entry` + `begin/cancel/apply_text_entry` (T1); `Domain::{Schema, writable, text_editable, parse_text}` (T1–T3); `Field.layer` (T2); `ObjectRow.prefix`, `display_name`, `Domain::prefix_fn`, `derive_rows` sort (T3); `Draft::row_for_path`, `flagged_rows` (T4); `OVERRIDES_DOC`, `override_key`, `override_entry`, `shadow_of`, `stale_override_keys`, `drifted` in `derive_rows` (T6) |
| `crates/geode-shell/src/shell/objectdialog/groupings.rs` | chain field re-expressed over `text_entry` (T1) |
| `crates/geode-shell/src/shell/objectdialog/schema.rs` | **new** — the read-only adapter (T2) |
| `crates/geode-shell/src/shell/objectdialog/sources.rs` | **new** — the Sources adapter (T3) |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | `handle_text_key`, `i` on Text/Number, `edit` pill and label (T1); read-only gates, per-row layer badge (T2); prefixed browse rows, `n` seeding (T3); diagnostic glyphs and label prefixes (T4); drift note in the edit header, overrides key on removal (T6) |
| `crates/geode-shell/src/shell/objectdialog/apply.rs` | `promote` reports a rejected in-memory apply, `finish_flush` paints it (T5); `commit_edit` adds the overrides entry on a fork (T6) |
| `crates/geode-shell/src/shell/dialog.rs` | `edit_pill` (T1) |
| `crates/geode-shell/src/defaults.rs`, `crates/geode-shell/src/shell/input.rs` | `config::schema` (T2), `config::sources` (T3) |
| `crates/geode-shell/src/shell/mod.rs` | `ShellEvent::ReloadRejected` (T5) |
| `crates/geode-shell/src/shell/hot_reload.rs` | emits `ReloadRejected`, logs the errors (T5) |
| `crates/geode-core/src/source_config.rs` | empty paths is a warning; `check_batch_pattern`; `pub` defaults (T3); `path` (T4) |
| `crates/geode-core/src/{view,groupings,scopes,dimensions}.rs`, `schema/mod.rs` | `path` on every diagnostic (T4) |
| `crates/geode-core/src/config/mod.rs` | `Display` appends ` (at path)` (T4) |
| `crates/geode-core/src/config/merge.rs` | `overrides` is atomic at depth 1 (T6) |
| `crates/geode-app/src/bridge.rs` | `ReloadRejected` arm; reload path reports presentation diagnostics (T5) |
| `crates/geode-shell/src/shell/tests/objectdialog.rs` | window tests per task |
| `scripts/mutation-check.sh`, `CLAUDE.md`, spec §19.8 | T7 |

---

### Task 1: Committed text entry — `Draft::text_entry`, `i` on `Text` and `Number`

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Draft` struct ~line 731–800; `visible_rows` ~909; `draft()`/`new_object()` literals ~1560, ~1500; `enter_edit` ~1800)
- Modify: `crates/geode-shell/src/shell/objectdialog/groupings.rs:220-340`
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (open's pill ~217; EditText arm ~1017; `handle_chain_key` ~1093; `build_edit` reads of `chain_entry` at ~2119, ~2305, ~2373, ~2399, ~2513, ~2631; footer ~2510)
- Modify: `crates/geode-shell/src/shell/dialog.rs:807` (`chain_pill` gains a sibling)
- Test: `crates/geode-shell/src/shell/objectdialog/mod.rs` (unit tests), `crates/geode-shell/src/shell/tests/objectdialog.rs` (existing chain tests must stay green)

**Interfaces:**
- Consumes: `Draft`, `EditRow`, `FieldKind`, `Step`, `NormalCommand::EditText`, `groupings::{begin_chain_entry, cancel_chain_entry, complete_chain, apply_chain, chain_candidates}`.
- Produces:
  ```rust
  pub struct TextEntry { pub row: EditRow, pub completions: bool }
  // Draft
  pub text_entry: Option<TextEntry>;                       // replaces `chain_entry: bool`
  pub fn chain_entry(&self) -> bool;                       // text_entry.is_some_and(|t| t.completions)
  pub fn begin_text_entry(&mut self) -> Step;              // seeds `query` from the selected Text/Number row
  pub fn cancel_text_entry(&mut self);
  pub fn apply_text_entry(&mut self, parse_text: &dyn Fn(&str, &str) -> Result<String, String>) -> Step;
  // Domain
  pub fn text_editable(self, key: &str) -> bool;           // false on every domain until Sources (T3)
  pub fn parse_text(self, key: &str, text: &str) -> Result<String, String>;  // Ok(text.trim()) until Sources
  // dialog.rs
  pub(crate) fn edit_pill(cx: &App) -> AnyElement;         // state_pill("edit", true, cx), selector dialog-mode-pill-edit
  // render.rs
  fn handle_text_key(shell, ks, cx) -> bool;               // renamed from handle_chain_key; dispatched when draft.text_entry.is_some()
  ```
  Window tests for a real editable `Text` arrive with Sources (Task 3), which is the first domain to have one; this task's tests are the pure core plus the existing chain-field window tests, which must pass unchanged.

- [ ] **Step 1: Write the failing pure-core tests** (append inside `mod tests` at the bottom of `mod.rs`; the module already imports `super::*`)

```rust
    fn draft_with_number_and_text() -> Draft {
        Draft::new_object(
            "x",
            vec![
                Field {
                    key: "polls".to_string(),
                    label: "Stable polls".to_string(),
                    kind: FieldKind::Number { value: 3, min: 1, max: 100 },
                    dest: Destination::Doc,
                },
                Field {
                    key: "interval".to_string(),
                    label: "Poll interval".to_string(),
                    kind: FieldKind::Text("30s".to_string()),
                    dest: Destination::Doc,
                },
            ],
            toml::Table::new(),
        )
    }

    #[test]
    fn begin_text_entry_seeds_the_query_from_a_number_row() {
        let mut draft = draft_with_number_and_text();
        draft.selected = 0;
        assert_eq!(draft.begin_text_entry(), Step::Changed);
        assert_eq!(draft.query, "3");
        assert_eq!(
            draft.text_entry,
            Some(TextEntry { row: EditRow::Field(0), completions: false })
        );
        assert!(!draft.chain_entry(), "a plain field has no completion list");
    }

    #[test]
    fn begin_text_entry_seeds_the_query_from_a_text_row() {
        let mut draft = draft_with_number_and_text();
        draft.selected = 1;
        assert_eq!(draft.begin_text_entry(), Step::Changed);
        assert_eq!(draft.query, "30s");
    }

    #[test]
    fn begin_text_entry_is_inert_off_a_text_or_number_row() {
        let mut draft = Draft::new_object(
            "x",
            vec![Field {
                key: "on".to_string(),
                label: "On".to_string(),
                kind: FieldKind::Bool(true),
                dest: Destination::Doc,
            }],
            toml::Table::new(),
        );
        assert_eq!(draft.begin_text_entry(), Step::Inert);
        assert_eq!(draft.text_entry, None);
    }

    #[test]
    fn applying_a_number_parses_and_refuses_out_of_range_without_clamping() {
        let ok = |_: &str, t: &str| Ok(t.to_string());
        let mut draft = draft_with_number_and_text();
        draft.selected = 0;
        draft.begin_text_entry();
        draft.query = "abc".to_string();
        assert!(matches!(draft.apply_text_entry(&ok), Step::Refused(r) if r.contains("whole number")));
        assert!(draft.text_entry.is_some(), "a refusal keeps the field open");
        draft.query = "500".to_string();
        assert!(matches!(draft.apply_text_entry(&ok), Step::Refused(r) if r.contains("1 and 100")));
        draft.query = "42".to_string();
        assert_eq!(draft.apply_text_entry(&ok), Step::Changed);
        assert_eq!(draft.text_entry, None);
        assert!(matches!(draft.fields[0].kind, FieldKind::Number { value: 42, .. }));
        assert!(draft.query.is_empty());
    }

    #[test]
    fn applying_text_goes_through_the_domains_parser_and_the_same_value_is_inert() {
        let parse = |key: &str, t: &str| -> Result<String, String> {
            if key == "interval" && t.ends_with('s') {
                Ok(t.trim().to_string())
            } else {
                Err("unit needed".to_string())
            }
        };
        let mut draft = draft_with_number_and_text();
        draft.selected = 1;
        draft.begin_text_entry();
        draft.query = "2 minutes".to_string();
        assert_eq!(draft.apply_text_entry(&parse), Step::Refused("unit needed".to_string()));
        draft.query = " 30s ".to_string();
        assert_eq!(draft.apply_text_entry(&parse), Step::Inert, "the trimmed value is the one already there");
        assert_eq!(draft.text_entry, None, "an inert apply still closes the field");
        draft.begin_text_entry();
        draft.query = "45s".to_string();
        assert_eq!(draft.apply_text_entry(&parse), Step::Changed);
        assert_eq!(draft.fields[1].kind, FieldKind::Text("45s".to_string()));
    }

    #[test]
    fn cancelling_text_entry_drops_the_text_and_leaves_the_value() {
        let mut draft = draft_with_number_and_text();
        draft.selected = 1;
        draft.begin_text_entry();
        draft.query = "garbage".to_string();
        draft.cancel_text_entry();
        assert_eq!(draft.text_entry, None);
        assert!(draft.query.is_empty());
        assert_eq!(draft.fields[1].kind, FieldKind::Text("30s".to_string()));
    }
```

- [ ] **Step 2: Run them to confirm they fail to compile**

Run: `cargo test -p geode-shell objectdialog::tests::begin_text_entry 2>&1 | head -20`
Expected: errors — `TextEntry` not found, `text_entry`/`begin_text_entry` not found.

- [ ] **Step 3: Replace `chain_entry` with `text_entry` in the pure core**

In `mod.rs`, next to `Confirm`:

```rust
/// A value field open in the filter row's place (§19.1): the shared
/// `Input` seeded with a row's value, `enter` applying it down the tick's
/// own path and `escape` cancelling. The chain field (§18.8) is the case
/// with `completions: true` — the rows below are then
/// [`groupings::chain_candidates`] rather than the edit rows.
///
/// `row` is an [`EditRow`], not a field index, so a future item-level
/// text (a column's width, Part 2c) is one more arm and not a second
/// mechanism; nothing in this plan opens it on anything but
/// `EditRow::Field`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEntry {
    pub row: EditRow,
    pub completions: bool,
}
```

In `Draft`, replace the `chain_entry: bool` field (and its doc) with:

```rust
    /// A text field is open (§19.1). While it is, `query` holds the text
    /// being typed rather than a filter — the shared `Input` mirrors into
    /// it exactly as a filter does, so there is no second text buffer —
    /// and, for the chain field's `completions: true`,
    /// [`Draft::visible_rows`] is the completion list. A field on the
    /// draft beside `confirm` rather than a `Stage`, because the stage is
    /// what the escape ladder and the browse cursor restore key on, and
    /// both must still read `Edit` here.
    pub text_entry: Option<TextEntry>,
```

Update the two `Draft` literals (`draft()` ~line 1570 and `new_object()` ~line 1500): `chain_entry: false,` → `text_entry: None,`. In `visible_rows`, `if self.chain_entry {` → `if self.chain_entry() {`. In `enter_edit`, keep `draft.begin_chain_entry()` as is (it is redefined below).

Add to `impl Draft` (next to `toggle_selected`):

```rust
    /// Is the open text field the chain field — the one with a completion
    /// list below it? `false` when no field is open at all.
    pub fn chain_entry(&self) -> bool {
        self.text_entry.is_some_and(|entry| entry.completions)
    }

    /// `i` on a `Text` or `Number` row (§19.1): open the field seeded with
    /// the row's value, so appending is one keystroke away. Pure — the
    /// mode switch that hands the shared `Input` the keys is the
    /// handler's, and the sync writes the seed into the field on its
    /// return. `Step::Inert` off any other row: the caller says which
    /// verb (if any) that row has.
    ///
    /// Whether a `Text` row is *editable* is the domain's call
    /// (`Domain::text_editable`), checked by the caller before this —
    /// Groupings' `slot` and Scopes' two summaries are `Text` rows that
    /// must stay read-only, and the draft has no domain to ask.
    pub fn begin_text_entry(&mut self) -> Step {
        let Some(row @ EditRow::Field(index)) = self.selected_row() else {
            return Step::Inert;
        };
        let seed = match &self.fields[index].kind {
            FieldKind::Text(text) => text.clone(),
            FieldKind::Number { value, .. } => value.to_string(),
            _ => return Step::Inert,
        };
        self.query = seed;
        self.text_entry = Some(TextEntry {
            row,
            completions: false,
        });
        Step::Changed
    }

    /// `escape` in an open text field: drop the text and close it. The
    /// value is exactly as it was — nothing here was applied. The chain
    /// field's cancel is this same function (`groupings.rs` re-exports
    /// it under its old name).
    pub fn cancel_text_entry(&mut self) {
        self.text_entry = None;
        self.query.clear();
        self.selected = 0;
    }

    /// `enter` in a plain text field: the typed text becomes the row's
    /// value and the field closes. A `Number` parses in here — its rule
    /// is the kind's own (a whole number inside `min..=max`, refused
    /// rather than clamped, because a clamp would apply a number the
    /// trader did not type). A `Text` goes through `parse_text` — the
    /// adapter's door, `(key, text) -> Result<normalised, reason>` — so
    /// a duration or a regex is refused with the field still open, the
    /// chain field's own rule for a bad chain. The same value typed
    /// back is [`Step::Inert`] and still closes the field: closing is
    /// the visible answer.
    ///
    /// `selected` is kept, not reset to 0: the rows below never changed
    /// (they are the edit rows, not a completion list), so the cursor
    /// stays on the row just edited.
    pub fn apply_text_entry(
        &mut self,
        parse_text: &dyn Fn(&str, &str) -> Result<String, String>,
    ) -> Step {
        let Some(TextEntry { row: EditRow::Field(index), completions: false }) = self.text_entry
        else {
            return Step::Inert;
        };
        let typed = self.query.trim().to_string();
        let field = &mut self.fields[index];
        let label = field.label.clone();
        let outcome = match &mut field.kind {
            FieldKind::Number { value, min, max } => match typed.parse::<i64>() {
                Err(_) => return Step::Refused(format!("{label} must be a whole number")),
                Ok(n) if n < *min || n > *max => {
                    return Step::Refused(format!("{label} must be between {min} and {max}"));
                }
                Ok(n) if n == *value => Step::Inert,
                Ok(n) => {
                    *value = n;
                    Step::Changed
                }
            },
            FieldKind::Text(text) => match parse_text(&field.key, &typed) {
                Err(reason) => return Step::Refused(reason),
                Ok(parsed) if parsed == *text => Step::Inert,
                Ok(parsed) => {
                    *text = parsed;
                    Step::Changed
                }
            },
            _ => Step::Inert,
        };
        self.text_entry = None;
        self.query.clear();
        outcome
    }
```

Add to `impl Domain` (the block holding `fields`/`draft`):

```rust
    /// May `i` edit the `Text` row keyed `key` on this domain? `false`
    /// everywhere until an adapter has an editable text — Groupings'
    /// `slot` and Scopes' two summaries are display-only `Text`s and
    /// must refuse. Sources (§19.3) is the first `true`.
    pub fn text_editable(self, key: &str) -> bool {
        match self {
            Domain::Views | Domain::Groupings | Domain::Scopes => {
                let _ = key;
                false
            }
        }
    }

    /// The adapter's door for a committed `Text` (§19.1): normalise the
    /// typed text, or refuse it with the reason the notice shows. Trims
    /// by default; an adapter with a real grammar (a duration, a regex, a
    /// path list) overrides its own keys.
    pub fn parse_text(self, key: &str, text: &str) -> Result<String, String> {
        match self {
            Domain::Views | Domain::Groupings | Domain::Scopes => {
                let _ = key;
                Ok(text.trim().to_string())
            }
        }
    }
```

- [ ] **Step 4: Re-express the chain field over `text_entry`** in `groupings.rs`

```rust
impl Draft {
    pub fn begin_chain_entry(&mut self) {
        let Some(field) = self.fields.iter().position(|f| f.key == DIMENSIONS) else {
            return;
        };
        let names: Vec<String> = self
            .list_items(DIMENSIONS)
            .unwrap_or_default()
            .iter()
            .filter(|i| i.included)
            .map(|i| i.name.clone())
            .collect();
        self.query = if names.is_empty() {
            String::new()
        } else {
            GroupingSlots::label_of(&names)
        };
        self.text_entry = Some(super::TextEntry {
            row: super::EditRow::Field(field),
            completions: true,
        });
        self.selected = 0;
    }

    /// `escape` in the chain field — [`Draft::cancel_text_entry`] under
    /// the name the chain tests use.
    pub fn cancel_chain_entry(&mut self) {
        self.cancel_text_entry();
    }
    // complete_chain: unchanged.
    // apply_chain: every `self.chain_entry = false;` becomes `self.text_entry = None;` (two sites).
}
```

Update `groupings.rs`'s own unit tests: `assert!(draft.chain_entry)` → `assert!(draft.chain_entry())`, `assert!(!draft.chain_entry)` → `assert!(!draft.chain_entry())`.

- [ ] **Step 5: Run the pure tests**

Run: `cargo test -p geode-shell objectdialog::tests 2>&1 | tail -20` and `cargo test -p geode-shell groupings::tests 2>&1 | tail -5`
Expected: the six new tests PASS; every groupings test PASSES.

- [ ] **Step 6: The gpui side — `handle_text_key`, `i`, the pill, the label, the hint**

`dialog.rs`, after `chain_pill`:

```rust
/// The pill while a plain value field is open (§19.1): `edit`, the same
/// `primary` "you are typing" pair `chain_pill` uses — `filter` would
/// misdescribe what `enter` does. Selector `dialog-mode-pill-edit`.
pub(crate) fn edit_pill(cx: &App) -> AnyElement {
    state_pill("edit", true, cx)
}
```

`render.rs`:

1. `open`'s title-extra closure: replace the `chain_entry` branch with

```rust
            .children(state.map(|s| match s.draft.as_ref().and_then(|d| d.text_entry) {
                Some(entry) if entry.completions => dialog::chain_pill(cx),
                Some(_) => dialog::edit_pill(cx),
                None => dialog::mode_pill(s.mode, cx),
            }))
```

2. `handle_edit_key`'s dispatch (the `let chain_entry = ...; if chain_entry { return handle_chain_key(..) }` block, ~line 803): key on `draft.text_entry.is_some()` and call `handle_text_key`.

3. The `NormalCommand::EditText` arms (~line 1017–1033) become one arm:

```rust
        NormalCommand::EditText => {
            let domain = shell.object_dialog.as_ref().map(|s| s.domain);
            if domain == Some(Domain::Groupings) {
                // §18.8: on Groupings `i` opens the chain field over the
                // slot's whole object — the typed line is the primary way
                // to set a chain.
                if let Some(state) = shell.object_dialog.as_mut()
                    && let Some(draft) = state.draft.as_mut()
                {
                    draft.begin_chain_entry();
                    state.mode = DialogMode::Filter;
                }
                shell.object_dialog_scroll.scroll_to_item(0);
            } else {
                open_text_field(shell);
            }
        }
        NormalCommand::Commit => edit_commit_notice(shell),
```

and add, next to `edit_commit_notice`:

```rust
/// `i` off Groupings (§19.1): open the value field on the selected row,
/// or say why not. A `Number` is always typeable; a `Text` only where
/// the domain says so (`Domain::text_editable`) — a display-only `Text`
/// gets the read-only notice `enter` gives, so the two verbs agree about
/// the same row. Any other row gets `edit_commit_notice`'s answer.
fn open_text_field(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let domain = state.domain;
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    let editable = match draft.selected_row() {
        Some(EditRow::Field(i)) => match &draft.fields[i].kind {
            FieldKind::Number { .. } => true,
            FieldKind::Text(_) => domain.text_editable(&draft.fields[i].key),
            _ => false,
        },
        _ => false,
    };
    if !editable {
        edit_commit_notice(shell);
        return;
    }
    if let Some(state) = shell.object_dialog.as_mut()
        && let Some(draft) = state.draft.as_mut()
        && draft.begin_text_entry() == Step::Changed
    {
        state.mode = DialogMode::Filter;
    }
}
```

4. Rename `handle_chain_key` → `handle_text_key` and rewrite its body so the completion-list keys are gated on `completions`:

```rust
fn handle_text_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    let completions = draft_mut(shell).is_some_and(|d| d.chain_entry());
    if ks.key == "escape" {
        if let Some(state) = shell.object_dialog.as_mut()
            && let Some(draft) = state.draft.as_mut()
        {
            draft.cancel_text_entry();
            state.mode = DialogMode::Normal;
        }
        shell.object_dialog_scroll.scroll_to_item(0);
        cx.notify();
        return true;
    }
    let bare = ks.mods == Modifiers::NONE;
    if bare && ks.key == "enter" {
        let domain = shell.object_dialog.as_ref().map(|s| s.domain);
        let step = draft_mut(shell).map(|draft| {
            if completions {
                draft.apply_chain()
            } else {
                let domain = domain.expect("a draft implies an open dialog");
                draft.apply_text_entry(&|key, text| domain.parse_text(key, text))
            }
        });
        match step {
            Some(Step::Changed) => {
                if let Some(state) = shell.object_dialog.as_mut() {
                    state.mode = DialogMode::Normal;
                }
                if completions {
                    shell.object_dialog_scroll.scroll_to_item(0);
                } else {
                    scroll_to_cursor(shell);
                }
                revalidate(shell);
                commit_or_confirm(shell, cx);
            }
            Some(Step::Inert) => {
                if let Some(state) = shell.object_dialog.as_mut() {
                    state.mode = DialogMode::Normal;
                }
                if completions {
                    shell.object_dialog_scroll.scroll_to_item(0);
                }
            }
            Some(Step::Refused(reason)) => set_notice(shell, reason),
            None => {}
        }
        cx.notify();
        return true;
    }
    if ks.key == "tab" {
        if completions && bare && draft_mut(shell).is_some_and(Draft::complete_chain) {
            shell.object_dialog_scroll.scroll_to_item(0);
        } else {
            set_notice(shell, "nothing to complete here".to_string());
        }
        cx.notify();
        return true;
    }
    if completions && let Some(cmd) = listfilter::nav_command(ks) {
        // the existing highlight-move block, unchanged
        ...
        return true;
    }
    // A plain field: the arrows and ctrl-steps are the caret's, so they
    // reach the focused `Input` (`false`), like every other key.
    false
}
```

Rewrite the doc comment above it: it is now the value field's key table, with the chain field as the `completions` case.

5. Every remaining `draft.chain_entry` read in `render.rs` (`build_edit`'s `action_block` match, the grip/tick withdrawal, `let chain = draft.chain_entry;` on the row click, the drag `.then(..)`, the footer hint's branch, the `filter` swap): `chain_entry` as a *field* no longer exists. Replace:
   - "is any field open" reads (action block, grip/tick withdrawal, drag withdrawal, `filter` swap) with `draft.text_entry.is_some()`; the row click handler captures `let open = draft.text_entry.map(|t| t.completions);` instead of `let chain` and dispatches `Some(true)` → `on_completion_clicked`, `Some(false)` → nothing at all (a plain value field owns the mouse as it owns the keys — moving the cursor under an open field would leave `TextEntry.row` pointing at a row the trader is no longer on), `None` → `on_edit_row_clicked`.
   - the footer hint: `} else if let Some(entry) = draft.text_entry {` with the chain vocabulary when `entry.completions` and, otherwise, `(vec![sep("type a value")], vec![chip("enter"), sep("apply ·"), chip("escape"), sep("cancel")])`.
   - the `filter` swap's label: chain → `format!("slot {} · chain", draft.name)` as today; plain → `format!("{} · {}", draft.name, draft.fields[index].label)` where `index` is the entry's `EditRow::Field(index)`.
   Update every comment that says "chain field" where the sentence is now about any open field.

6. `mod.rs`'s `enter_edit` comment and `ObjectDialogState` doc: `opens_in_chain_field` stays; nothing else changes.

- [ ] **Step 7: Build, run the whole shell suite**

Run: `cargo build -p geode-shell --features test-support --all-targets 2>&1 | grep -E "^(error|warning)" | head`; then `cargo test -p geode-shell 2>&1 | tail -5`
Expected: clean build; every test PASSES, the §18.8/§18.9 chain tests included (`i_opens_the_chain_field_tab_completes_and_enter_writes_the_chain`, `a_refused_chain_keeps_the_field_open_and_escape_cancels_it`, `clicking_a_groupings_row_lands_in_the_chain_field`).

- [ ] **Step 8: Harness entries** — append after the last `run_mutation` entry:

```bash
# §19.1: a Number typed outside its range is REFUSED, never clamped — a
# clamp would apply a number the trader did not type.
run_mutation "objectdialog: a typed number outside min..max is refused not clamped" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                Ok(n) if n < *min || n > *max => {' \
  '                Ok(n) if false && (n < *min || n > *max) => {' \
  geode-shell \
  applying_a_number_parses_and_refuses_out_of_range_without_clamping

# §19.1: the same text typed back closes the field with nothing queued.
run_mutation "objectdialog: retyping the same text is inert and closes the field" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                Ok(parsed) if parsed == *text => Step::Inert,' \
  '                Ok(parsed) if parsed == *text => Step::Changed,' \
  geode-shell \
  applying_text_goes_through_the_domains_parser_and_the_same_value_is_inert

# §19.1: `i` seeds the field with the row's value, not an empty field.
run_mutation "objectdialog: begin_text_entry seeds the query from the row" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        self.query = seed;' \
  '        self.query = String::new();' \
  geode-shell \
  begin_text_entry_seeds_the_query_from_a_number_row
```

Confirm each anchor: `grep -c -F '<anchor>' crates/geode-shell/src/shell/objectdialog/mod.rs` prints `1`. Run `zsh scripts/mutation-check.sh --anchors-only` (exit 0), then the five CI checks.

- [ ] **Step 9: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(objectdialog): committed text entry — i on a Text or Number row (4c §19.1)

Draft::text_entry replaces chain_entry; the chain field is its
completions:true case. Domain::text_editable/parse_text are the
adapter's doors; a Number parses in the scaffold and is refused, never
clamped, outside its range."
```

---

### Task 2: `Domain::writable()`, `Field.layer`, and the Schema inspector

**Files:**
- Create: `crates/geode-shell/src/shell/objectdialog/schema.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Domain` enum and every `match self` on it; `Field`; `mod schema;` declaration next to `mod views;`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (browse `n` gate ~385; edit-stage verb gate at the top of the normal-mode match in `handle_edit_key`; `actions()`; both footer hints; the `EditRow::Field` arm's badges ~2224; `press_verb`)
- Modify: `crates/geode-shell/src/shell/objectdialog/{views,groupings,scopes}.rs` (`layer: None` on every `Field` literal)
- Modify: `crates/geode-shell/src/defaults.rs:478` (after `config::scopes`), `crates/geode-shell/src/shell/input.rs:174` (after the `config::scopes` arm)
- Test: `crates/geode-shell/src/shell/objectdialog/schema.rs` (unit), `crates/geode-shell/src/shell/tests/objectdialog.rs` (window)

**Interfaces:**
- Consumes: `derive_rows`, `Config::explain`, `SchemaSpec::from_doc`, `DerivedDimensions::from_doc`, `ColumnSpec`, `Grain::short`, `dialog::badge`, `merge_docs`, `LayerDoc`.
- Produces:
  ```rust
  pub enum Domain { Views, Groupings, Scopes, Schema }
  impl Domain { pub fn writable(self) -> bool }                  // false for Schema only
  pub struct Field { .., pub layer: Option<Layer> }               // painted as a badge on the row when Some
  pub const READ_ONLY_NOTICE: &str = "the schema is read-only";   // mod.rs
  // schema.rs
  pub const DOC: &str = "datasets";
  pub fn summary(value: &toml::Value) -> String;                  // "<n> columns · <m> measures · <k> dimensions"
  pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field>;   // one Text per column (key "columns.<name>"), then derived rows (key "derived.<name>")
  pub fn describe_column(column: &ColumnSpec) -> String;          // "<type> · <role>[ · grain <g>][ · required][ · textual][ · categorical]"
  pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item;   // the source, unchanged
  pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic>;     // SchemaSpec::from_doc over this dataset alone
  // palette: "config::schema" → "Schema (read-only)", category "Configuration"
  ```

- [ ] **Step 1: Write the failing unit tests** (`schema.rs` bottom, `mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, ConfigSources, Layer, LayerDoc};

    // `required` defaults to true and `categorical` to true for a
    // dimension (`schema::parse_column`), so the expected strings below
    // carry both flags for `book` and neither for `note`.
    const DATASETS: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                            [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                            [risk.columns.note]\ntype = \"utf8\"\nrole = \"attribute\"\ngrain = \"position\"\nrequired = false\n";

    fn config() -> Config {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("datasets", DATASETS).unwrap(),
                // `other` derives from a column no dataset has: it must
                // NOT appear under `risk` (the filter in `fields`).
                LayerDoc::builtin(
                    "dimensions",
                    "[region]\nfrom = \"book\"\n[region.values]\nEU = [\"BK1\"]\n\
                     [other]\nfrom = \"nothing\"\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        })
    }

    #[test]
    fn summary_counts_columns_by_role() {
        let value = config().doc("datasets").unwrap().value.get("risk").cloned().unwrap();
        assert_eq!(summary(&value), "3 columns · 0 measures · 1 dimension");
    }

    #[test]
    fn fields_are_one_read_only_text_per_column_then_the_derived_dimensions() {
        let fields = fields(&config(), Some("risk"));
        let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["columns.book", "columns.note", "columns.position_ref", "derived.region"]
        );
        assert!(matches!(&fields[0].kind, FieldKind::Text(t) if t == "utf8 · dimension · required · categorical"));
        assert!(matches!(&fields[1].kind, FieldKind::Text(t) if t == "utf8 · attribute · grain position"));
        assert!(!fields.iter().any(|f| f.key == "derived.other"), "not this dataset's");
        assert!(matches!(&fields[3].kind, FieldKind::Text(t) if t == "from book · 1 value"));
        assert_eq!(fields[3].label, "region (derived)");
    }

    #[test]
    fn every_row_carries_the_layer_that_defined_it() {
        let fields = fields(&config(), Some("risk"));
        assert!(fields.iter().all(|f| f.layer == Some(Layer::Builtin)), "{fields:?}");
    }

    #[test]
    fn schema_is_the_one_domain_that_is_not_writable() {
        assert!(!Domain::Schema.writable());
        assert!(Domain::Views.writable());
        assert!(Domain::Groupings.writable());
        assert!(Domain::Scopes.writable());
    }

    #[test]
    fn validate_reports_the_datasets_own_reader_diagnostics() {
        let mut config = config();
        // A grain in use whose key column is undeclared is the reader's
        // own complaint, and this dataset's alone.
        let _ = &mut config;
        let draft = Domain::Schema.draft(&config, "risk");
        let diags = validate(&draft, &config);
        assert!(
            diags.iter().any(|d| d.message.contains("requires key column")),
            "{diags:?}"
        );
    }
}
```

(The last test relies on `role = "attribute"` at grain `position` needing the `position` grain's key columns `book`, `lhu`, `position_ref`, `counterparty`; the fixture declares only two, so `validate_dataset` warns.)

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p geode-shell objectdialog::schema 2>&1 | head`
Expected: compile errors (`schema` module missing, `Domain::Schema` missing).

- [ ] **Step 3: Widen the core**

`mod.rs`: add `Schema` to `Domain` with the doc `/// Read-only (§9, §19.4): the datasets the other adapters build their choices from.`; add `pub const READ_ONLY_NOTICE: &str = "the schema is read-only";`; add

```rust
    /// `false` for [`Domain::Schema`] alone (§19.4): the create gate, the
    /// footer hints and every mutating verb — `space`, `shift+space`,
    /// `i`, `d`, `r`, `x`, `n`, `o`, `shift+j`/`shift+k`, a tick click,
    /// a drop — read this, so a read-only surface refuses in one place
    /// rather than by each verb forgetting. The Groupings roster gate
    /// (`roster().is_some()`) is a separate question ("can anything be
    /// created that is not already listed") and stays beside it.
    pub fn writable(self) -> bool {
        !matches!(self, Domain::Schema)
    }
```

Fill every `match self` on `Domain` with a `Schema` arm: `doc` → `schema::DOC`; `title` → `"Schema"`; `crumb_noun` → `"datasets"`; `summary_fn` → `schema::summary`; `presentation_doc` → `None` (extend the `Groupings | Scopes` arm); `roster` → `None`; `text_editable`/`parse_text` → the same as the others; `fields` → `schema::fields`; `to_table` → `schema::to_table`; `validate` → `schema::validate`. `render::section_header_text`'s `(Domain::Scopes, _)` arm becomes `(Domain::Scopes | Domain::Schema, _)`.

`Field` gains, after `dest`:

```rust
    /// The layer this row's value came from, painted as a badge on the
    /// row when `Some` (§19.4). Filled by the Schema adapter from
    /// `Config::explain`; every writable domain leaves it `None`, since
    /// the object-level badge in the header already says whose copy is
    /// on screen and a second badge per row would only repeat it.
    pub layer: Option<Layer>,
```

Add `layer: None,` to every `Field { .. }` literal the compiler reports (`views.rs`, `groupings.rs`, `scopes.rs`, `mod.rs` tests, `tests/objectdialog.rs` if any).

- [ ] **Step 4: Write `schema.rs`**

```rust
//! The read-only schema inspector (spec §9, §19.4) — `Domain::Schema`.
//!
//! Not an editor: a schema is the desk's contract with the data, a
//! `datasets` change is restart-required, and the edit and its effect
//! would be far apart. The other adapters read this doc to build their
//! `Choice`s and catalogues; this dialog makes that vocabulary
//! inspectable. Every field is a display-only `Text`, every row carries
//! the layer it came from, and `Domain::writable()` answers `false`, so
//! the scaffold refuses every verb with `READ_ONLY_NOTICE` in one place.

use super::{Destination, Draft, Field, FieldKind};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{ColumnRole, ColumnSpec, ColumnType, SchemaSpec};

pub const DOC: &str = "datasets";

/// `<n> columns · <m> measures · <k> dimensions`, counted off the raw
/// table rather than the parsed spec, so a dataset the reader drops a
/// column from still counts what the file says.
pub fn summary(value: &toml::Value) -> String {
    let Some(columns) = value.get("columns").and_then(|v| v.as_table()) else {
        return "no [columns] table".to_string();
    };
    let role = |name: &str| {
        columns
            .values()
            .filter(|c| c.get("role").and_then(|r| r.as_str()) == Some(name))
            .count()
    };
    let (n, m, k) = (columns.len(), role("measure"), role("dimension"));
    format!(
        "{n} column{} · {m} measure{} · {k} dimension{}",
        plural(n),
        plural(m),
        plural(k)
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn type_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
}

/// One column, one line: type, role, the grain where the role has one,
/// then whichever of `required`/`textual`/`categorical` hold.
pub fn describe_column(column: &ColumnSpec) -> String {
    let mut out = format!("{} · ", type_name(column.ty));
    match &column.role {
        ColumnRole::Key => out.push_str("key"),
        ColumnRole::Dimension { grain: None } => out.push_str("dimension"),
        ColumnRole::Dimension { grain: Some(g) } => {
            out.push_str(&format!("dimension · carried by {}", g.short()))
        }
        ColumnRole::Measure { grain, .. } => {
            out.push_str(&format!("measure · grain {}", grain.short()))
        }
        ColumnRole::Attribute { grain } => {
            out.push_str(&format!("attribute · grain {}", grain.short()))
        }
    }
    if column.required {
        out.push_str(" · required");
    }
    if column.textual {
        out.push_str(" · textual");
    }
    if column.categorical {
        out.push_str(" · categorical");
    }
    out
}

/// One display-only `Text` per column of `object`, in schema (file)
/// order, then one per derived dimension whose `from` is a column of
/// this dataset. `layer` is `Config::explain` on the dataset (atomic at
/// depth 1, so every column of one dataset carries that dataset's
/// layer) and on the `dimensions` doc for a derived row, which can
/// differ. Keys are `columns.<name>` / `derived.<name>` so §19.5's
/// path matching lands a `datasets.<ds>.columns.<name>.type` diagnostic
/// on its column's row.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let Some(name) = object else {
        return Vec::new();
    };
    let schema = config
        .doc(DOC)
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default();
    let Some(dataset) = schema.dataset(name) else {
        return Vec::new();
    };
    let dataset_layer = config.explain(DOC, name);
    let mut out: Vec<Field> = dataset
        .columns
        .iter()
        .map(|column| Field {
            key: format!("columns.{}", column.name),
            label: column.name.clone(),
            kind: FieldKind::Text(describe_column(column)),
            dest: Destination::Doc,
            layer: dataset_layer,
        })
        .collect();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    for dim in dims.all().filter(|d| dataset.column(&d.from).is_some()) {
        out.push(Field {
            key: format!("derived.{}", dim.name),
            label: format!("{} (derived)", dim.name),
            kind: FieldKind::Text(format!(
                "from {} · {} value{}",
                dim.from,
                dim.values.len(),
                plural(dim.values.len())
            )),
            dest: Destination::Doc,
            layer: config.explain("dimensions", &dim.name),
        });
    }
    out
}

/// Unreachable behind `Domain::writable()`; the source, unchanged, so
/// the exhaustive `Domain::to_table` has an honest arm.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    toml_edit::Item::Table(super::toml_table_to_edit(&draft.source))
}

/// The dataset's own reader diagnostics, over this dataset alone (the
/// same one-object document `views::validate` builds, for the same
/// reason: the whole doc would report every other dataset's problems
/// against this one).
pub fn validate(draft: &Draft, _config: &Config) -> Vec<Diagnostic> {
    let mut table = toml::Table::new();
    table.insert(draft.name.clone(), toml::Value::Table(draft.source.clone()));
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table,
        }],
    );
    SchemaSpec::from_doc(&doc).1
}
```

Check `Grain::short` exists with that name (`crates/geode-core/src/schema/grain.rs:98`); check `ColumnType`, `ColumnRole`, `ColumnSpec` are exported from `geode_core::schema` (`grep -n "pub use" crates/geode-core/src/schema/mod.rs`) and adjust the `use` line to whatever the re-export path is. Verify `summary`'s expected string in the test matches the plural rule (`3 columns · 0 measures · 1 dimension`).

- [ ] **Step 5: Run the unit tests**

Run: `cargo test -p geode-shell objectdialog::schema 2>&1 | tail -12`
Expected: all five PASS.

- [ ] **Step 6: Gate the scaffold on `writable()` and paint the row badge**

`render.rs`:

1. Browse `n` arm: `if !state.domain.writable() { state.notice = Some(READ_ONLY_NOTICE.to_string()); } else if state.domain.roster().is_some() { ... } else { state.begin_naming(); }`.
2. `handle_edit_key`, normal mode, before the `match cmd`: 

```rust
    let writable = shell.object_dialog.as_ref().is_some_and(|s| s.domain.writable());
    if !writable
        && matches!(
            cmd,
            NormalCommand::Toggle
                | NormalCommand::ToggleBack
                | NormalCommand::EditText
                | NormalCommand::MoveItem(_)
                | NormalCommand::Verb('d' | 'r' | 'x' | 'n' | 'o')
        )
    {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        cx.notify();
        return true;
    }
```
   (Find the exact variable name the match is on — `cmd` or the `normal_command(ks)` result — and place this after it is bound and after the armed-confirm and filter-mode branches.)
3. `actions()`: `if !state.domain.writable() { return Vec::new(); }` right after the `draft.is_none()` check.
4. Footer hints: browse omits `n` when `!writable() || roster().is_some()` (today it keys on the roster alone — find the `n` chip and widen the condition); the edit-stage normal-mode hint for a read-only domain is `[/ filter · j k move · escape back]` only.
5. `press_verb`: return early with the notice when `!writable()` (the bar is empty, so this is belt-and-braces; keep it because a test can call it).
6. The `EditRow::Field` arm: before the dest badge, `if let Some(layer) = field.layer { .child(dialog::badge(layer.name(), theme.muted_foreground, theme.border, Some(format!("objectdialog-field-layer-{}", field.key)), cx)) }`; paint the dest badge only when `domain.writable()` (a `doc` badge on a read-only row would promise a write).
7. `edit_commit_notice` on a read-only domain: `READ_ONLY_NOTICE` (so `enter` and `i` agree).

`defaults.rs` after `config::scopes`:

```rust
    // Part 2b Task 2: the read-only schema inspector (spec §9, §19.4)
    // over `datasets` — the vocabulary the other three dialogs build
    // their choices from, made inspectable. Palette-only like its
    // siblings; its title says read-only because the palette row is the
    // only place a trader learns that before opening it.
    action(reg, "config::schema", "Schema (read-only)", "Configuration");
```

`input.rs` after the `config::scopes` arm:

```rust
        } else if action.0 == "config::schema" {
            // Part 2b Task 2: the object dialog's browse stage over
            // `Domain::Schema` (`shell::objectdialog::schema`), read-only.
            objectdialog::render::open(self, objectdialog::Domain::Schema, window, cx);
```

- [ ] **Step 7: Window tests** (`tests/objectdialog.rs`)

```rust
fn services_with_schema() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [vol.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), datasets],
        desk: None,
        user: None,
    });
    services
}

/// §19.4: the inspector lists datasets, opens one to its column rows —
/// each with the layer it came from — and refuses every verb with one
/// notice; `n` is refused in browse and the footer never offers it.
#[gpui::test]
fn the_schema_inspector_lists_datasets_and_refuses_every_verb(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_schema(), "config::schema");
    assert!(cx.debug_bounds("objectdialog-row-risk").is_some());
    assert!(cx.debug_bounds("objectdialog-row-vol").is_some());

    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some(objectdialog::READ_ONLY_NOTICE)
    );
    assert!(matches!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Browse));

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-field-columns.book").is_some());
    assert!(cx.debug_bounds("objectdialog-field-layer-columns.book").is_some(), "the row's own layer badge");
    assert!(cx.debug_bounds("objectdialog-dest-columns.book").is_none(), "no doc badge on a read-only row");

    for key in ["space", "i", "d", "shift-j"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
            Some(objectdialog::READ_ONLY_NOTICE),
            "{key}"
        );
        assert!(shell.read_with(&cx, |s, _| s.pending_config_write.is_none()), "{key} queued a write");
    }
    // `/` still filters.
    cx.simulate_keystrokes("/");
    cx.simulate_input("pos");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-field-columns.position_ref").is_some());
    assert!(cx.debug_bounds("objectdialog-field-columns.book").is_none());
}
```

Check the action bar's selector name in `action_bar` (`objectdialog-actions` is what the chain test asserts) and use that.

Run: `cargo test -p geode-shell the_schema_inspector 2>&1 | tail -8` — PASS.

- [ ] **Step 8: Harness entries**

```bash
# §19.4: `writable()` is the one gate every mutating verb on the schema
# inspector reads. Flipping it to `true` must be caught by the window
# test, which presses four verbs and asserts nothing queued.
run_mutation "objectdialog: Schema is not writable" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        !matches!(self, Domain::Schema)' \
  '        true' \
  geode-shell \
  the_schema_inspector_lists_datasets_and_refuses_every_verb

# §19.4: a schema row carries the layer `Config::explain` names.
run_mutation "objectdialog: schema rows carry the dataset's layer" \
  crates/geode-shell/src/shell/objectdialog/schema.rs \
  '    let dataset_layer = config.explain(DOC, name);' \
  '    let dataset_layer: Option<Layer> = None;' \
  geode-shell \
  every_row_carries_the_layer_that_defined_it

# §19.4: derived rows are those whose `from` is one of THIS dataset's columns.
run_mutation "objectdialog: schema lists only this dataset's derived dimensions" \
  crates/geode-shell/src/shell/objectdialog/schema.rs \
  '    for dim in dims.all().filter(|d| dataset.column(&d.from).is_some()) {' \
  '    for dim in dims.all() {' \
  geode-shell \
  fields_are_one_read_only_text_per_column_then_the_derived_dimensions
```

Anchors unique; `--anchors-only` exit 0; five CI checks.

- [ ] **Step 9: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(objectdialog): the read-only schema inspector — Domain::Schema, writable(), Field.layer (4c §19.4)"
```

---

### Task 3: Sources — idle empty paths, the flat dataset·source list, the adapter

**Files:**
- Modify: `crates/geode-core/src/source_config.rs` (empty paths → warning ~line 148; `check_batch_pattern`; `pub` on `DEFAULT_POLL`/`DEFAULT_PENDING_TIMEOUT`; the existing empty-paths test)
- Create: `crates/geode-shell/src/shell/objectdialog/sources.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Domain::Sources` + arms; `ObjectRow.prefix`; `ObjectRow::display_name`; `Domain::prefix_fn`; `derive_rows`; `searchable_text`; `ObjectDialogState.naming_dataset`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (browse label painting ~1830–1860; browse `n` arm; `create_from_name`)
- Modify: `crates/geode-shell/src/defaults.rs`, `crates/geode-shell/src/shell/input.rs`
- Test: `sources.rs` unit tests, `tests/objectdialog.rs` window tests, `source_config.rs` reader tests

**Interfaces:**
- Consumes: Task 1's `text_editable`/`parse_text`/`begin_text_entry`; Task 2's `Field.layer`; `SourceSpec::from_doc(doc, schema)`, `parse_duration`, `SchemaSpec`.
- Produces:
  ```rust
  // geode-core source_config.rs
  pub const DEFAULT_POLL: Duration; pub const DEFAULT_PENDING_TIMEOUT: Duration;
  pub fn check_batch_pattern(pattern: &str) -> Result<(), String>;   // compiles AND has a `batch` capture
  // mod.rs
  pub enum Domain { Views, Groupings, Scopes, Schema, Sources }
  pub struct ObjectRow { .., pub prefix: Option<String> }
  impl ObjectRow { pub fn display_name(&self) -> String }          // "<prefix> · <name>" or "<name>"
  impl Domain { fn prefix_fn(self) -> Option<fn(&toml::Value) -> Option<String>> }
  fn derive_rows(config, doc, presentation_doc, roster, summary, prefix) -> Vec<ObjectRow>   // sorted (prefix, name) when prefix is Some
  pub struct ObjectDialogState { .., pub naming_dataset: Option<String> }
  // sources.rs
  pub const DOC: &str = "sources"; pub const PATH_SEPARATOR: char = ';';
  pub fn summary(value: &toml::Value) -> String;                   // "<n> path(s) · <priority>"
  pub fn prefix(value: &toml::Value) -> Option<String>;            // the dataset
  pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field>;
  pub fn seed_dataset(draft: &mut Draft, dataset: &str);           // sets the Choice, adding the option if absent
  pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item;
  pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic>;
  pub fn text_editable(key: &str) -> bool;                         // paths, poll_interval, pending_timeout, batch_pattern
  pub fn parse_text(key: &str, text: &str) -> Result<String, String>;
  pub fn spell_duration(d: Duration) -> String;                    // "2h" / "5m" / "45s"
  ```

- [ ] **Step 1: Reader — empty `paths` is a warning and the source is skipped**

In `source_config.rs` tests, find the existing test covering empty paths (`grep -n "empty 'paths'\|paths" crates/geode-core/src/source_config.rs | sed -n 1,20p`). Change or add:

```rust
    #[test]
    fn empty_paths_is_a_warning_and_the_source_is_skipped() {
        let doc = merge_docs(
            "sources",
            &[LayerDoc::builtin("sources", "[idle]\ndataset = \"risk_snapshot\"\npaths = []\n").unwrap()],
        );
        let (specs, diags) = SourceSpec::from_doc(&doc, &schema());
        assert!(specs.is_empty(), "an idle source never reaches the scheduler");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning, "{diags:?}");
        assert!(diags[0].message.contains("idle"), "{}", diags[0].message);
    }

    #[test]
    fn check_batch_pattern_needs_a_compiling_regex_with_a_batch_capture() {
        assert!(check_batch_pattern("(?P<batch>.+)").is_ok());
        assert!(check_batch_pattern("(.+").unwrap_err().contains("does not compile"));
        assert!(check_batch_pattern("(.+)").unwrap_err().contains("batch"));
    }
```

(`schema()` — use whatever helper the existing tests build a `SchemaSpec` with; see the test at line ~267.)

Run: `cargo test -p geode-core source_config 2>&1 | tail -8` — the two new tests FAIL (severity is `Error`; `check_batch_pattern` missing).

Implement:

```rust
            if paths.is_empty() {
                // §19.3 (ruling 2026-09-12): a source with nothing to poll
                // is idle, not broken — a warning, and skipped, so the
                // dialog's `n` can create one and let the trader type the
                // globs in afterwards. One line, so the harness can flip
                // its severity by anchoring on it.
                diags.push(diag(Severity::Warning, name, IDLE_PATHS));
                continue;
            }
```

with, beside the defaults, `pub const IDLE_PATHS: &str = "no 'paths' — the source is idle until one is set";` (the dialog's tests read it too).

```rust
```

```rust
/// Is `pattern` a `batch_pattern` the reader would accept — a regex that
/// compiles and names a `batch` capture? The one spelling of that rule,
/// shared by the reader below and the Sources dialog's inline refusal.
pub fn check_batch_pattern(pattern: &str) -> Result<(), String> {
    match regex::Regex::new(pattern) {
        Err(e) => Err(format!("batch_pattern does not compile: {e}")),
        Ok(re) if re.capture_names().any(|c| c == Some("batch")) => Ok(()),
        Ok(_) => Err("batch_pattern needs a named `batch` capture, like (?P<batch>.+)".to_string()),
    }
}
```

and rewrite the reader's `batch_pattern` match to call it: `Some(p) => match check_batch_pattern(p) { Ok(()) => Some(p.to_string()), Err(e) => { diags.push(diag(Severity::Warning, name, format!("'{e}'; ignoring it"))); None } }`. Make the two defaults `pub`. Run the core tests — PASS. Commit: `git commit -am "feat(core): empty source paths is an idle-source warning; check_batch_pattern (4c §19.3)"`.

- [ ] **Step 2: Failing unit tests for the adapter** (`sources.rs` bottom)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, ConfigSources, LayerDoc};

    fn config() -> Config {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "sources",
                    "[live]\ndataset = \"risk\"\npaths = [\"/a/*.csv\", \"/b/*.csv\"]\n\
                     readiness = { stable_mtime = 4 }\npriority = \"latest_other\"\n\
                     poll_interval = \"45s\"\nbatch_pattern = '^r_(?P<batch>.+)$'\n\
                     [vols]\ndataset = \"vol\"\npaths = [\"/v/*.csv\"]\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        })
    }

    #[test]
    fn summary_and_prefix_read_the_raw_table() {
        let value = config().doc("sources").unwrap().value.get("live").cloned().unwrap();
        assert_eq!(summary(&value), "2 paths · latest_other");
        assert_eq!(prefix(&value).as_deref(), Some("risk"));
    }

    #[test]
    fn fields_spell_every_key_and_default_the_absent_ones() {
        let fields = fields(&config(), Some("live"));
        let by_key = |k: &str| fields.iter().find(|f| f.key == k).unwrap();
        assert!(matches!(&by_key("dataset").kind, FieldKind::Choice { options, selected } if options[*selected] == "risk" && options == &["risk", "vol"]));
        assert!(matches!(&by_key("paths").kind, FieldKind::Text(t) if t == "/a/*.csv; /b/*.csv"));
        assert!(matches!(&by_key("readiness").kind, FieldKind::Choice { options, selected } if options[*selected] == "stable_mtime"));
        assert!(matches!(&by_key("stable_polls").kind, FieldKind::Number { value: 4, min: 1, max: 100 }));
        assert!(matches!(&by_key("priority").kind, FieldKind::Choice { options, selected } if options[*selected] == "latest_other"));
        assert!(matches!(&by_key("poll_interval").kind, FieldKind::Text(t) if t == "45s"));
        assert!(matches!(&by_key("pending_timeout").kind, FieldKind::Text(t) if t == "10m"), "the reader's default, spelled");
        assert!(matches!(&by_key("batch_pattern").kind, FieldKind::Text(t) if t == "^r_(?P<batch>.+)$"));
        assert!(fields.iter().all(|f| f.dest == Destination::Doc && f.layer.is_none()));
    }

    #[test]
    fn to_table_writes_readiness_polls_only_under_stable_mtime() {
        let config = config();
        let mut draft = Domain::Sources.draft(&config, "live");
        let item = to_table(&draft, Destination::Doc);
        let text = super::object_text("live", item);
        assert!(text.contains("readiness = { stable_mtime = 4 }"), "{text}");
        // Step readiness back to sentinel: polls must vanish from the file.
        let i = draft.fields.iter().position(|f| f.key == "readiness").unwrap();
        if let FieldKind::Choice { selected, .. } = &mut draft.fields[i].kind {
            *selected = 0;
        }
        let text = super::object_text("live", to_table(&draft, Destination::Doc));
        assert!(text.contains("readiness = \"sentinel\""), "{text}");
        assert!(!text.contains("stable_mtime"), "{text}");
        assert!(text.contains("paths = [\"/a/*.csv\", \"/b/*.csv\"]"), "{text}");
    }

    #[test]
    fn parse_text_refuses_bad_durations_and_patterns_and_splits_paths() {
        assert_eq!(parse_text("poll_interval", " 30s ").unwrap(), "30s");
        assert!(parse_text("poll_interval", "2 minutes").unwrap_err().contains("45s"));
        assert_eq!(parse_text("paths", "/a/*.csv ;/b/*.csv;").unwrap(), "/a/*.csv; /b/*.csv");
        assert_eq!(parse_text("paths", "  ").unwrap(), "", "no paths is an idle source, not a refusal");
        assert_eq!(parse_text("batch_pattern", "").unwrap(), "");
        assert!(parse_text("batch_pattern", "(.+)").unwrap_err().contains("batch"));
        assert!(text_editable("paths") && text_editable("batch_pattern"));
        assert!(!text_editable("dataset"));
    }

    #[test]
    fn spell_duration_uses_the_largest_exact_unit() {
        use std::time::Duration;
        assert_eq!(spell_duration(Duration::from_secs(7200)), "2h");
        assert_eq!(spell_duration(Duration::from_secs(600)), "10m");
        assert_eq!(spell_duration(Duration::from_secs(45)), "45s");
        assert_eq!(spell_duration(Duration::from_secs(90)), "90s");
    }

    #[test]
    fn a_new_source_is_idle_with_the_seeded_dataset() {
        let config = config();
        let mut draft = Domain::Sources.new_draft(&config, "fresh");
        seed_dataset(&mut draft, "vol");
        assert_eq!(draft.choice("dataset"), Some("vol"));
        let text = super::object_text("fresh", to_table(&draft, Destination::Doc));
        assert!(text.contains("dataset = \"vol\""), "{text}");
        assert!(text.contains("paths = []"), "{text}");
        let diags = validate(&draft, &config);
        assert!(diags.iter().any(|d| d.message.contains("idle")), "{diags:?}");
        assert!(diags.iter().all(|d| d.severity != geode_core::config::Severity::Error));
    }

    #[test]
    fn rows_are_sorted_by_dataset_then_name_and_carry_the_prefix() {
        let rows = Domain::Sources.objects(&config());
        let names: Vec<(Option<&str>, &str)> = rows.iter().map(|r| (r.prefix.as_deref(), r.name.as_str())).collect();
        assert_eq!(names, vec![(Some("risk"), "live"), (Some("vol"), "vols")]);
        assert_eq!(rows[0].display_name(), "risk · live");
    }
}
```

Run: `cargo test -p geode-shell objectdialog::sources 2>&1 | head` — compile errors.

- [ ] **Step 3: Core widening — `Sources`, `prefix`, `naming_dataset`**

`mod.rs`:
- `Domain::Sources` with doc `/// The ingest feeds, one object per source (§8.3, §19.3).`; arms: `doc` → `sources::DOC`; `title` → `"Sources"`; `crumb_noun` → `"sources"`; `summary_fn` → `sources::summary`; `presentation_doc` → `None`; `roster` → `None`; `fields`/`to_table`/`validate` → `sources::*`; `text_editable` → `sources::text_editable(key)`; `parse_text` → `sources::parse_text(key, text)`; `section_header_text` in render → fold into the Scopes/Schema arm.
- `ObjectRow` gains:

```rust
    /// A grouping key painted before the name, dimmed (§19.3): the
    /// dataset a source feeds. `Some` only on Sources; the primary sort
    /// key when present, part of `searchable_text`, never the identity —
    /// the doc key is still `name`, so a dataset with two sources is two
    /// rows and every click handler and selector stays keyed by `name`.
    pub prefix: Option<String>,
```

with `impl ObjectRow { pub fn display_name(&self) -> String { match &self.prefix { Some(p) => format!("{p} · {}", self.name), None => self.name.clone() } } }`. Add `prefix: None,` to both literals in `derive_rows` and to every test literal the compiler reports.

- `Domain::prefix_fn`:

```rust
    /// The text painted before an object's name, if this domain groups
    /// its objects (§19.3). `None` on every domain but Sources.
    fn prefix_fn(self) -> Option<fn(&toml::Value) -> Option<String>> {
        match self {
            Domain::Sources => Some(sources::prefix),
            Domain::Views | Domain::Groupings | Domain::Scopes | Domain::Schema => None,
        }
    }
```

- `derive_rows` gains `prefix: Option<fn(&toml::Value) -> Option<String>>`; in the walk, after `entry.1.summary = summary(value);` add `entry.1.prefix = prefix.and_then(|f| f(value));`; at the end, collect into `let mut out: Vec<ObjectRow> = rows.into_values().map(..).collect();` and, `if prefix.is_some()`, `out.sort_by(|a, b| (&a.prefix, &a.name).cmp(&(&b.prefix, &b.name)));` (the `BTreeMap` already gave name order; the stable sort on `(prefix, name)` is the by-dataset order), then return `out`. Update `objects()` to pass `self.prefix_fn()` and the doc comment listing "the only things a domain decides" to include the prefix.
- `searchable_text`: `format!("{} {}", row.display_name(), row.summary)`.
- `ObjectDialogState` gains `pub naming_dataset: Option<String>` (`None` in `new`), doc: `/// The dataset the row under the cursor fed when \`n\` was pressed (§19.3, Sources only): the new source's \`dataset\` seed. Cleared by \`cancel_naming\` and consumed by \`render::create_from_name\`.` Clear it in `cancel_naming`.

- [ ] **Step 4: Write `sources.rs`**

```rust
//! The Sources adapter (spec §8.3, §19.3) — `Domain::Sources`.
//!
//! Every field is `Destination::Doc`; there is no presentation doc. The
//! browse list is every dataset·source pair, flat — `prefix` is the
//! dataset, painted first and the primary sort key — and `enter` opens
//! the arguments directly. A write reaches the running data service
//! through nothing new: the dialog's in-memory apply runs
//! `apply_reload`, whose `sources` baseline comparison raises the
//! existing restart-required stripe.

use super::{Destination, Draft, Field, FieldKind};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::schema::SchemaSpec;
use geode_core::source_config::{
    DEFAULT_PENDING_TIMEOUT, DEFAULT_POLL, SourceSpec, check_batch_pattern, parse_duration,
};
use std::time::Duration;

pub const DOC: &str = "sources";
/// Between globs in the `paths` text: illegal in a Windows path and
/// unused in globs, where a space is legal in both (§19.3).
pub const PATH_SEPARATOR: char = ';';

const READINESS: [&str; 2] = ["sentinel", "stable_mtime"];
const PRIORITY: [&str; 3] = ["latest_risk", "latest_other", "backfill"];

pub fn summary(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "not a table".to_string();
    };
    let n = table.get("paths").and_then(|v| v.as_array()).map_or(0, |a| a.len());
    let priority = table
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or(PRIORITY[0]);
    format!("{n} path{} · {priority}", if n == 1 { "" } else { "s" })
}

pub fn prefix(value: &toml::Value) -> Option<String> {
    value
        .as_table()
        .and_then(|t| t.get("dataset"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `2h` / `10m` / `45s`: the largest unit that divides exactly, the
/// reader's own grammar (`parse_duration`).
pub fn spell_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s > 0 && s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s > 0 && s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

fn choice(options: &[&str], current: &str) -> FieldKind {
    let mut options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
    if !options.iter().any(|o| o == current) {
        options.insert(0, current.to_string());
    }
    let selected = options.iter().position(|o| o == current).unwrap_or(0);
    FieldKind::Choice { options, selected }
}

pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    let get_str = |key: &str| table.and_then(|t| t.get(key)).and_then(|v| v.as_str());

    let schema = config
        .doc("datasets")
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default();
    let mut datasets: Vec<&str> = schema.datasets.iter().map(|d| d.name.as_str()).collect();
    datasets.sort_unstable();
    // The object's own dataset is always an option (Views' rule): a
    // `Choice` that cannot show its value would step silently.
    let current_dataset = get_str("dataset")
        .map(str::to_string)
        .or_else(|| datasets.first().map(|d| d.to_string()))
        .unwrap_or_default();

    let paths: Vec<String> = table
        .and_then(|t| t.get("paths"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default();

    let (readiness, polls) = match table.and_then(|t| t.get("readiness")) {
        Some(v) if v.as_str() == Some("sentinel") || v.is_str() => ("sentinel", 3),
        Some(v) => match v.as_table().and_then(|t| t.get("stable_mtime")).and_then(|p| p.as_integer()) {
            Some(p) if p > 0 => ("stable_mtime", p),
            _ => ("sentinel", 3),
        },
        None => ("sentinel", 3),
    };

    let duration = |key: &str, default: Duration| -> String {
        get_str(key).map(str::to_string).unwrap_or_else(|| spell_duration(default))
    };

    let text = |key: &str, label: &str, value: String| Field {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Text(value),
        dest: Destination::Doc,
        layer: None,
    };
    let field = |key: &str, label: &str, kind: FieldKind| Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        dest: Destination::Doc,
        layer: None,
    };

    vec![
        field("dataset", "Dataset", choice(&datasets, &current_dataset)),
        text("paths", "Paths", paths.join(&format!("{PATH_SEPARATOR} "))),
        field("readiness", "Readiness", choice(&READINESS, readiness)),
        field("stable_polls", "Stable polls", FieldKind::Number { value: polls, min: 1, max: 100 }),
        field("priority", "Priority", choice(&PRIORITY, get_str("priority").unwrap_or(PRIORITY[0]))),
        text("poll_interval", "Poll interval", duration("poll_interval", DEFAULT_POLL)),
        text("pending_timeout", "Pending timeout", duration("pending_timeout", DEFAULT_PENDING_TIMEOUT)),
        text("batch_pattern", "Batch pattern", get_str("batch_pattern").unwrap_or("").to_string()),
    ]
}

/// `n` seeds the new source's dataset from the browse row under the
/// cursor (§19.3), not the schema's first — the option is added when the
/// schema lacks it, for the same reason `fields` keeps an object's own.
pub fn seed_dataset(draft: &mut Draft, dataset: &str) {
    if let Some(field) = draft.fields.iter_mut().find(|f| f.key == "dataset")
        && let FieldKind::Choice { options, selected } = &mut field.kind
    {
        if !options.iter().any(|o| o == dataset) {
            options.insert(0, dataset.to_string());
        }
        *selected = options.iter().position(|o| o == dataset).unwrap_or(0);
    }
}

pub fn text_editable(key: &str) -> bool {
    matches!(key, "paths" | "poll_interval" | "pending_timeout" | "batch_pattern")
}

/// The inline refusals (§19.3): a duration the reader could not read, a
/// regex that does not compile or has no `batch` capture. `paths` is
/// normalised to the canonical `a; b` spelling; empty is legal — an idle
/// source, the reader's own ruling.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    match key {
        "poll_interval" | "pending_timeout" => match parse_duration(text) {
            Some(_) => Ok(text.to_string()),
            None => Err(format!("{key}: a number and a unit, like 45s, 5m or 2h")),
        },
        "batch_pattern" if text.is_empty() => Ok(String::new()),
        "batch_pattern" => check_batch_pattern(text).map(|()| text.to_string()),
        "paths" => Ok(split_paths(text).join(&format!("{PATH_SEPARATOR} "))),
        _ => Ok(text.to_string()),
    }
}

fn split_paths(text: &str) -> Vec<String> {
    text.split(PATH_SEPARATOR)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The draft as `sources.toml` holds it: the source table with every
/// field applied over it (a key the vocabulary does not model survives).
/// `stable_polls` is written only under `stable_mtime`; an empty
/// `batch_pattern` removes the key.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    let mut table = super::toml_table_to_edit(&draft.source);
    let text = |key: &str| {
        draft.fields.iter().find(|f| f.key == key).and_then(|f| match &f.kind {
            FieldKind::Text(t) => Some(t.clone()),
            _ => None,
        })
    };
    if let Some(dataset) = draft.choice("dataset") {
        table["dataset"] = toml_edit::value(dataset);
    }
    if let Some(paths) = text("paths") {
        let mut array = toml_edit::Array::new();
        for p in split_paths(&paths) {
            array.push(p);
        }
        table["paths"] = toml_edit::value(array);
    }
    let polls = draft.fields.iter().find(|f| f.key == "stable_polls").and_then(|f| match f.kind {
        FieldKind::Number { value, .. } => Some(value),
        _ => None,
    });
    match (draft.choice("readiness"), polls) {
        (Some("stable_mtime"), Some(polls)) => {
            let mut inline = toml_edit::InlineTable::new();
            inline.insert("stable_mtime", toml_edit::Value::from(polls));
            table["readiness"] = toml_edit::value(inline);
        }
        (Some(_), _) => table["readiness"] = toml_edit::value("sentinel"),
        (None, _) => {}
    }
    if let Some(priority) = draft.choice("priority") {
        table["priority"] = toml_edit::value(priority);
    }
    for key in ["poll_interval", "pending_timeout"] {
        if let Some(value) = text(key) {
            table[key] = toml_edit::value(value);
        }
    }
    match text("batch_pattern") {
        Some(p) if !p.is_empty() => table["batch_pattern"] = toml_edit::value(p),
        Some(_) => {
            table.remove("batch_pattern");
        }
        None => {}
    }
    toml_edit::Item::Table(table)
}

/// The reader over this source alone, with the schema it checks
/// `dataset` against — the reader errors on an undeclared dataset, so
/// no cross-check of ours is needed.
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    let rendered: toml::Table = super::object_text(&draft.name, to_table(draft, Destination::Doc))
        .parse()
        .unwrap_or_default();
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table: rendered,
        }],
    );
    let schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    SourceSpec::from_doc(&doc, &schema).1
}
```

Check: how `views::rendered_doc_table` turns an `Item` back into a `toml::Table` (`views.rs:590`) and mirror it exactly rather than the `object_text(..).parse()` sketch above if it differs. Check `toml_edit::Value::from(i64)` and `toml_edit::value(InlineTable)` compile against the pinned `toml_edit`; `toml_edit::Array::push` takes `impl Into<Value>`.

- [ ] **Step 5: Run the unit tests**

Run: `cargo test -p geode-shell objectdialog::sources 2>&1 | tail -12` — all seven PASS. (Fix `to_table`'s expected `readiness = { stable_mtime = 4 }` spelling against what `toml_edit` actually renders, by reading the failure, before touching the implementation.)

- [ ] **Step 6: The gpui side — prefixed rows, `n` seeding, the action**

`render.rs`, browse painter (~line 1830): replace `let name_len = row.name.chars().count();` with the display name, and the label's first child with the two-run label:

```rust
        let display = row.display_name();
        let name_len = display.chars().count();
        let (name_ix, summary_ix) = split_label_indices(&m.indices, name_len);
        // §19.3: a prefixed row paints `<prefix> · ` dimmed and the name
        // after it, as two runs of one highlighted label — the indices
        // are split at the prefix's end so a hit inside the dataset still
        // highlights there.
        let head: AnyElement = match &row.prefix {
            Some(prefix) => {
                let cut = prefix.chars().count() + 3;
                let (in_prefix, in_name): (Vec<usize>, Vec<usize>) =
                    name_ix.iter().copied().partition(|i| *i < cut);
                let in_name: Vec<usize> = in_name.into_iter().map(|i| i - cut).collect();
                h_flex()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(highlighted_text(&format!("{prefix} · "), &in_prefix, theme.primary)),
                    )
                    .child(highlighted_text(&row.name, &in_name, theme.primary))
                    .into_any_element()
            }
            None => highlighted_text(&row.name, &name_ix, theme.primary),
        };
        let label = v_flex().gap_0p5().child(head).child(/* summary as today */);
```

Browse `n` arm, in the `else { state.begin_naming(); }` branch, seed for Sources:

```rust
                    let seed = (state.domain == Domain::Sources)
                        .then(|| {
                            let rows = derive_rows_for(state, shell_config);
                            ...
                        });
```

— `state` is a `&mut` borrow of `shell.object_dialog` here, so compute the seed BEFORE taking that borrow: at the top of the `Verb('n')` arm, `let seed = seed_dataset_under_cursor(shell);` where

```rust
/// §19.3: the dataset of the browse row under the cursor, for `n` on
/// Sources — `None` on every other domain, or with no row.
fn seed_dataset_under_cursor(shell: &ShellView) -> Option<String> {
    let state = shell.object_dialog.as_ref()?;
    if state.domain != Domain::Sources {
        return None;
    }
    let rows = derive_rows(shell);
    let visible = super::visible_rows(state, &rows);
    let row = visible.get(state.selected).and_then(|m| rows.get(m.row))?;
    row.prefix.clone()
}
```

then after `state.begin_naming();`: `state.naming_dataset = seed.clone(); if let Some(dataset) = seed && !state.domain.name_taken(config, &dataset) { state.query = dataset; }` — `config` is `&shell.services.config`, which conflicts with the `&mut` on `shell.object_dialog`; compute `let taken = seed.as_deref().is_some_and(|d| domain.name_taken(&shell.services.config, d));` before the borrow too. The sync writes `query` into the field on return, so the name field opens pre-filled.

`create_from_name`: after `let mut draft = domain.new_draft(..)`, add `if domain == Domain::Sources && let Some(dataset) = shell.object_dialog.as_ref().and_then(|s| s.naming_dataset.clone()) { sources::seed_dataset(&mut draft, &dataset); draft.diagnostics = domain.validate(&draft, &shell.services.config); }`.

`defaults.rs`: `action(reg, "config::sources", "Edit sources", "Configuration");` with a comment in the sibling style; `input.rs`: the dispatch arm for `config::sources` → `Domain::Sources`.

- [ ] **Step 7: Window tests**

```rust
fn services_with_sources() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    let sources = LayerDoc::builtin(
        "sources",
        "[vols]\ndataset = \"vol\"\npaths = [\"/v/*.csv\"]\n\
         [live]\ndataset = \"risk\"\npaths = [\"/a/*.csv\"]\npoll_interval = \"2s\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), datasets, sources],
        desk: None,
        user: None,
    });
    services
}

/// §19.3: rows read dataset first and sort by it; `i` on a text row
/// opens the field seeded with the value; a bad duration is refused with
/// the field open; a good one applies and, on a builtin source, asks
/// before forking; the flush writes the spelling the reader reads.
#[gpui::test]
fn sources_rows_are_dataset_first_and_i_types_a_duration(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    let live = cx.debug_bounds("objectdialog-row-live").unwrap();
    let vols = cx.debug_bounds("objectdialog-row-vols").unwrap();
    assert!(live.origin.y < vols.origin.y, "risk · live sorts before vol · vols");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Cursor to `poll_interval` (row 5: dataset, paths, readiness, polls, priority, poll_interval).
    cx.simulate_keystrokes("j j j j j");
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some() && !d.chain_entry()));
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(dialog_input_text(&shell, &cx), "2s", "seeded with the value");
    assert!(cx.debug_bounds("dialog-mode-pill-edit").is_some());
    assert!(cx.debug_bounds("objectdialog-actions").is_none(), "no verbs while a field is open");

    cx.simulate_input(" minutes");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some()), "refused: still open");
    assert!(dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap().contains("45s"));
    assert_eq!(dialog_input_text(&shell, &cx), "2s minutes", "the text is kept");

    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("backspace backspace");
    cx.simulate_input("30s");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_none()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(cx.debug_bounds("objectdialog-confirm").is_some(), "a builtin source asks before forking");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("sources.toml")).unwrap();
    assert!(written.contains("poll_interval = \"30s\""), "{written}");
    assert!(written.contains("paths = [\"/a/*.csv\"]"), "{written}");
    // §19.3's delivery path: the in-memory apply ran `apply_reload`, whose
    // sources-baseline comparison raised the existing stripe.
    assert!(
        shell.read_with(&cx, |s, _| s.restart_required.clone()).is_some_and(|m| m.contains("sources")),
        "a sources write raises the restart-required stripe"
    );
}

/// §19.3: `n` seeds the dataset from the cursor row and the name from
/// it when free; the created source is idle (empty paths, a warning
/// on the row, never an error).
#[gpui::test]
fn n_on_sources_seeds_the_dataset_and_creates_an_idle_source(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("j"); // vol · vols
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.naming_dataset.clone()).as_deref(), Some("vol"));
    assert_eq!(dialog_input_text(&shell, &cx), "vol", "the dataset's name, since no source holds it");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)).as_deref(), Some("vol"));
    assert!(edit_draft(&shell, &cx, |d| d.diagnostics.iter().all(|x| x.severity != geode_core::config::Severity::Error)));
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("sources.toml")).unwrap();
    assert!(written.contains("[vol]\ndataset = \"vol\"\npaths = []"), "{written}");
}
```

(If `[vol]` with `dataset = "vol"` renders with different key order, assert on the two lines separately.)

Run: `cargo test -p geode-shell sources_rows_are_dataset_first n_on_sources 2>&1 | tail -8` — PASS.

- [ ] **Step 8: Harness entries**

```bash
# §19.3: the path separator is `;`, not whitespace — a space is legal in a path.
run_mutation "sources: paths split on the semicolon" \
  crates/geode-shell/src/shell/objectdialog/sources.rs \
  '    text.split(PATH_SEPARATOR)' \
  '    text.split(char::is_whitespace)' \
  geode-shell \
  parse_text_refuses_bad_durations_and_patterns_and_splits_paths

# §19.3: `stable_polls` is written only under `stable_mtime`.
run_mutation "sources: polls are written only under stable_mtime" \
  crates/geode-shell/src/shell/objectdialog/sources.rs \
  '        (Some("stable_mtime"), Some(polls)) => {' \
  '        (Some(_), Some(polls)) => {' \
  geode-shell \
  to_table_writes_readiness_polls_only_under_stable_mtime

# §19.3: an idle source is a WARNING — an error would block `n`.
run_mutation "sources: empty paths is a warning not an error" \
  crates/geode-core/src/source_config.rs \
  '                diags.push(diag(Severity::Warning, name, IDLE_PATHS));' \
  '                diags.push(diag(Severity::Error, name, IDLE_PATHS));' \
  geode-core \
  empty_paths_is_a_warning_and_the_source_is_skipped

# §19.3: rows sort by dataset first.
run_mutation "objectdialog: prefixed rows sort by prefix then name" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        out.sort_by(|a, b| (&a.prefix, &a.name).cmp(&(&b.prefix, &b.name)));' \
  '        out.sort_by(|a, b| a.name.cmp(&b.name));' \
  geode-shell \
  rows_are_sorted_by_dataset_then_name_and_carry_the_prefix

# §19.3: `n` seeds the dataset from the cursor row, not the schema's first.
run_mutation "sources: n seeds the dataset from the cursor row" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    row.prefix.clone()' \
  '    None' \
  geode-shell \
  n_on_sources_seeds_the_dataset_and_creates_an_idle_source
```

Run the third entry alone (`zsh scripts/mutation-check.sh "empty paths is a warning"`) and confirm `caught`. Task 4 changes `diag`'s signature and MUST re-anchor this entry (its own Step 2 says so).

`--anchors-only` exit 0; five CI checks.

- [ ] **Step 9: Commit**

```bash
git add crates/geode-core crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(objectdialog): the Sources adapter over a flat dataset·source list (4c §19.3)"
```

---

### Task 4: Readers fill `Diagnostic.path`; the draft matches rows

**Files:**
- Modify: `crates/geode-core/src/config/mod.rs` (`Display` ~line 62; the `Diagnostic.path` doc)
- Modify: `crates/geode-core/src/view.rs` (`bad`/`warn` closures ~343, ~427; the columns loop ~400; `ViewPresentationSpec::from_doc` ~577; `ViewPresentation::apply` ~661)
- Modify: `crates/geode-core/src/groupings.rs` (every `Diagnostic {..}` literal in `from_doc`)
- Modify: `crates/geode-core/src/scopes.rs` (`warn` closure ~line 20)
- Modify: `crates/geode-core/src/source_config.rs` (`diag` helper ~98)
- Modify: `crates/geode-core/src/schema/mod.rs` (`note` ~289, `parse_column`'s `bad` ~308, `validate_dataset`)
- Modify: `crates/geode-core/src/dimensions.rs` (`bad` closure ~55)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`EditRow` derives; `Draft::row_for_path`, `flagged_rows`; `Draft::diagnostics` doc)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (row glyph ~2200; header block ~2480)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs:575-582` (the `path: None` comment)
- Test: each reader's own tests; `mod.rs` unit tests; one window test

**Interfaces:**
- Consumes: `Diagnostic::with_path`.
- Produces:
  - Path grammar: `<doc>.<object>[.<field>[.<index>[.<subkey>]]]`. Views: `views.<name>` (object-level), `views.<name>.dataset`, `views.<name>.columns.<i>` and `views.<name>.columns.<i>.format.<key>` / `.label` / `.width`; presentation: `view_presentation.<name>.order`, `.hidden`, `.width.<col>`; groupings: `groupings.<slot>`; scopes: `scopes.<name>`, `.expression`, `.dimensions.<col>`; sources: `sources.<name>` or `sources.<name>.<key>`; datasets: `datasets.<ds>`, `datasets.<ds>.columns.<col>`, `.columns.<col>.<key>`; dimensions: `dimensions.<name>`, `.from`, `.values.<v>`.
  - `Display` appends ` (at <path>)` when `path` is `Some`.
  - `impl Draft { pub fn row_for_path(&self, doc: &str, path: &str) -> Option<EditRow>; pub fn flagged_rows(&self, doc: &str) -> Vec<(EditRow, Severity)> }`
  - `EditRow` derives `PartialOrd, Ord, Hash` in addition to today's.

- [ ] **Step 1: Failing reader tests** — one per reader, in each file's `mod tests`:

```rust
    // view.rs
    #[test]
    fn a_column_format_diagnostic_carries_its_indexed_path() {
        let doc = merge_docs("views", &[LayerDoc::builtin("views",
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[[tree.columns]]\nname = \"delta\"\nformat = { precision = 99 }\n").unwrap()]);
        let (_, diags) = ViewSpec::from_doc(&doc);
        assert_eq!(diags[0].path.as_deref(), Some("views.tree.columns.1.format.precision"), "{diags:?}");
        assert!(diags[0].to_string().ends_with(" (at views.tree.columns.1.format.precision)"));
    }
    // (add the same shape for: a missing dataset → "views.tree.dataset"; presentation `order` naming a
    //  missing column → "view_presentation.tree.order"; groupings → "groupings.3"; scopes bad expression →
    //  "scopes.eod.expression"; sources missing dataset → "sources.live.dataset", bad poll_interval →
    //  "sources.live.poll_interval"; datasets bad type → "datasets.risk.columns.npv.type";
    //  dimensions missing from → "dimensions.region.from")
```

Run `cargo test -p geode-core 2>&1 | grep -E "FAILED|panicked" | head` — the new tests FAIL (`path` is `None`).

- [ ] **Step 2: Fill the paths**

Pattern per reader — the local helper gains the path and every call site passes the deepest key it honestly knows:

- `view.rs`: `let bad = |m: String| Diagnostic { .. }.with_path(format!("views.{name}"))` becomes `let at = |suffix: &str| if suffix.is_empty() { format!("views.{name}") } else { format!("views.{name}.{suffix}") };` and `let bad = |suffix: &str, m: String| Diagnostic { severity: Warning, layer: None, file: None, message: format!("view '{name}': {m}"), path: Some(at(suffix)) };`. `"missing 'dataset'"` → `bad("dataset", ..)`; `"not a table"` → `bad("", ..)`. The columns loop becomes `for (i, c) in cols.iter().enumerate() { let Some(c) = c.as_table() else { continue }; ...` and the column-level `warn` becomes `let warn = |key: &str, m: String| bad(&format!("columns.{i}.{key}"), format!("column '{col_name}': {m}"));` with `warn("format.precision", ..)`, `warn("format.thousands", ..)`, `warn("format.negative", ..)`, `warn("format.colour", ..)`, `warn("format.scale", ..)`, `warn("label", ..)`, `warn("width", ..)`, `warn("format", "'format' is not a table")`, `warn("kind", ..)` for the unknown kind, `warn("sql", ..)` for a derived column without sql, and `bad(&format!("columns.{i}.name"), ..)` for a missing name. Sort entries: `bad("sort", ..)`.
- `ViewPresentationSpec::from_doc`: `bad` gains a suffix the same way (`view_presentation.<view>[.<suffix>]`): `order`, `hidden`, `width.<col>`, `width`. `ViewPresentation::apply`'s `warn` likewise: `order` for the duplicate/unknown-column warnings, `hidden` and `width.<col>` for theirs, `""` for "no view of that name".
- `groupings.rs`: replace the four inline literals with `let at = |slot: &str, m: String| Diagnostic { .., path: Some(format!("groupings.{slot}")) };` (the "not a slot number" case uses `key`).
- `scopes.rs`: `warn(m)` → `warn(path: String, m)`: `format!("scopes.{name}")`, `format!("scopes.{name}.expression")`, `format!("scopes.{name}.dimensions.{column}")` where the reader names a column; read the rest of the function to place each.
- `source_config.rs`: `diag(severity, name, m)` → `diag(severity, name, key: Option<&str>, m)` — the idle-paths line becomes `diags.push(diag(Severity::Warning, name, Some("paths"), IDLE_PATHS));`, so update Task 3's harness anchor (`sources: empty paths is a warning not an error`) to the new line, both `from` and `to` with `path: Some(match key { Some(k) => format!("sources.{name}.{k}"), None => format!("sources.{name}") })`; `"missing 'dataset'"`/`"names undeclared dataset"` → `Some("dataset")`, paths → `Some("paths")`, readiness → `Some("readiness")`, priority → `Some("priority")`, durations → `Some(key)`, batch_pattern → `Some("batch_pattern")`, "not a table" → `None`.
- `schema/mod.rs`: `note(message)` → `note(path: String, message)`; `from_doc`'s "no [columns] table" → `format!("datasets.{ds_name}")`; `parse_column`'s `bad` → `format!("datasets.{ds}.columns.{name}")` and, where a key is known (`type`, `role`, `grain`, `aggregate`), append `.{key}` — read `parse_column`'s body and pass the key at each site; `validate_dataset`'s notes → `datasets.<ds>.columns.<col>` where a column is named, else `datasets.<ds>`.
- `dimensions.rs`: `bad(m)` → suffixes `""`, `from`, `values.<derived_value>`.
- `config/mod.rs` `Display`: after `write!(f, "{}", self.message)?;` add `if let Some(path) = &self.path { write!(f, " (at {path})")?; } Ok(())`. Rewrite `Diagnostic.path`'s doc: it is now filled by every reader; the grammar sentence above goes there.

Run `cargo test -p geode-core 2>&1 | tail -5` — PASS. Check the diagnostics tile's tests in `geode-shell` still pass (`cargo test -p geode-shell diagnostics 2>&1 | tail -3`): a test asserting an exact `to_string()` of a reader diagnostic must be updated to include the suffix.

- [ ] **Step 3: Failing draft tests** (`mod.rs` tests)

```rust
    #[test]
    fn row_for_path_matches_a_field_by_key_and_a_list_item_by_index() {
        let draft = Draft::new_object(
            "tree",
            vec![
                Field { key: "dataset".into(), label: "Dataset".into(), kind: FieldKind::Text("risk".into()), dest: Destination::Doc, layer: None },
                Field { key: "columns".into(), label: "Columns".into(), kind: FieldKind::OrderedList {
                    items: vec![
                        ListItem { name: "npv".into(), included: true, width: None, kind: None },
                        ListItem { name: "delta".into(), included: true, width: None, kind: None },
                    ],
                    available: Some(vec![ListItem { name: "vega".into(), included: false, width: None, kind: None }]),
                }, dest: Destination::Doc, layer: None },
            ],
            toml::Table::new(),
        );
        assert_eq!(draft.row_for_path("views", "views.tree.dataset"), Some(EditRow::Field(0)));
        assert_eq!(draft.row_for_path("views", "views.tree.columns.1.format.precision"), Some(EditRow::Item { field: 1, item: 1 }));
        assert_eq!(draft.row_for_path("views", "views.tree.columns"), Some(EditRow::Field(1)));
        assert_eq!(draft.row_for_path("views", "views.tree.columns.7"), Some(EditRow::Field(1)), "an index off the list lands on the field");
        assert_eq!(draft.row_for_path("views", "views.tree"), None, "object-level stays on the header");
        assert_eq!(draft.row_for_path("views", "views.other.dataset"), None);
        assert_eq!(draft.row_for_path("views", "sources.tree.dataset"), None);
    }
```

Run — fails to compile. Implement in `impl Draft`:

```rust
    /// The row a reader's diagnostic path names (§19.5), or `None` for a
    /// path that is not this object's or names no row — those stay on the
    /// header. The grammar is `<doc>.<object>.<field>[.<index>[...]]`: a
    /// field matches by `key`; a list field's next segment, when it is an
    /// index into `items`, matches that item (never an available row — the
    /// object has no diagnostic about a column it does not have).
    pub fn row_for_path(&self, doc: &str, path: &str) -> Option<EditRow> {
        let rest = path.strip_prefix(&format!("{doc}.{}.", self.name))?;
        self.fields.iter().enumerate().find_map(|(i, field)| {
            let after = if rest == field.key {
                ""
            } else {
                rest.strip_prefix(&format!("{}.", field.key))?
            };
            let index = after.split('.').next().and_then(|s| s.parse::<usize>().ok());
            match (&field.kind, index) {
                (FieldKind::OrderedList { items, .. }, Some(item)) if item < items.len() => {
                    Some(EditRow::Item { field: i, item })
                }
                _ => Some(EditRow::Field(i)),
            }
        })
    }

    /// Every row a current diagnostic lands on, with the worst severity
    /// there, for the row glyph; the header list is what carries the
    /// text.
    pub fn flagged_rows(&self, doc: &str) -> Vec<(EditRow, Severity)> {
        let mut out: Vec<(EditRow, Severity)> = Vec::new();
        for d in &self.diagnostics {
            let Some(row) = d.path.as_deref().and_then(|p| self.row_for_path(doc, p)) else {
                continue;
            };
            match out.iter_mut().find(|(r, _)| *r == row) {
                Some((_, s)) if *s == Severity::Warning && d.severity == Severity::Error => {
                    *s = Severity::Error
                }
                Some(_) => {}
                None => out.push((row, d.severity)),
            }
        }
        out
    }
```

(`find_map` with `?` inside a closure needs the closure to return `Option`; the `strip_prefix(..)?` inside returns `None` for that field, which `find_map` skips — correct.) Import `Severity` in `mod.rs` if it is not already. Add `PartialOrd, Ord, Hash` to `EditRow`'s derive only if you use them; the `Vec` scan above does not need them.

Rewrite `Draft::diagnostics`'s doc: shown on the header AND on the row `row_for_path` names. Rewrite the `path: None` comment in `views::validate` (the dataset cross-check) to `.with_path(format!("views.{}.dataset", draft.name))` and remove the comment claiming no reader fills it.

- [ ] **Step 4: Paint the glyph and the label prefix**

`build_edit`: before the row loop, `let flagged = draft.flagged_rows(domain.doc());`. In the loop, after `element` is built and before the `match edit_row`, compute `let flag = flagged.iter().find(|(r, _)| *r == edit_row).map(|(_, s)| *s);` and add as the element's first child (before `label`):

```rust
        let glyph = match flag {
            Some(Severity::Error) => div().w(px(12.)).text_color(theme.danger).child("!").into_any_element(),
            Some(Severity::Warning) => div().w(px(12.)).text_color(theme.warning).child("!").into_any_element(),
            None => div().w(px(12.)).into_any_element(),
        };
```

with a `debug_selector` of `format!("objectdialog-diag-{selector}")` on the flagged variants (the `selector` string is computed in the match — restructure so the glyph is added after the match, using `selector`). The header block: for each diagnostic, `let prefix = d.path.as_deref().and_then(|p| draft.row_for_path(domain.doc(), p)).map(|row| format!("{}: ", draft.row_label(row))).unwrap_or_default();` and paint `format!("{prefix}{}", d.message)`. Rewrite the block's comment (it says the path is unfilled).

- [ ] **Step 5: Window test**

```rust
/// §19.5: a reader diagnostic that names a column lands on that column's
/// row as a glyph, and its header line is prefixed with the row's label;
/// an object-level one stays on the header alone.
#[gpui::test]
fn a_column_diagnostic_flags_its_row(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let views = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[[tree.columns]]\nname = \"delta\"\nformat = { precision = 99 }\n",
    ).unwrap();
    let datasets = LayerDoc::builtin("datasets", "[risk.columns.npv]\ntype = \"f64\"\nrole = \"dimension\"\n[risk.columns.delta]\ntype = \"f64\"\nrole = \"dimension\"\n").unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), views, datasets],
        desk: None, user: None,
    });
    let (shell, mut cx) = dialog_test_shell_with(cx, services, "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-diag-objectdialog-item-delta").is_some(), "the glyph on delta's row");
    assert!(cx.debug_bounds("objectdialog-diag-objectdialog-item-npv").is_none());
    let diags = edit_draft(&shell, &cx, |d| d.diagnostics.clone());
    assert!(diags.iter().any(|d| d.path.as_deref() == Some("views.tree.columns.1.format.precision")), "{diags:?}");
}
```

(Check the item row's selector spelling — the chain test asserts `objectdialog-item-lhu` — and compose the glyph selector from it.) Run — PASS.

- [ ] **Step 6: Harness entries**

```bash
# §19.5: a column diagnostic's path carries the column INDEX — without
# it the row match lands on the `columns` field header, not the column.
run_mutation "views reader: column diagnostics carry the column index" \
  crates/geode-core/src/view.rs \
  '        let warn = |key: &str, m: String| bad(&format!("columns.{i}.{key}"), format!("column '"'"'{col_name}'"'"': {m}"));' \
  '        let warn = |key: &str, m: String| bad(&format!("columns.{key}"), format!("column '"'"'{col_name}'"'"': {m}"));' \
  geode-core \
  a_column_format_diagnostic_carries_its_indexed_path

# §19.5: an index off the end lands on the field, never on a phantom row.
run_mutation "objectdialog: row_for_path bounds the item index" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                (FieldKind::OrderedList { items, .. }, Some(item)) if item < items.len() => {' \
  '                (FieldKind::OrderedList { items, .. }, Some(item)) if item <= items.len() => {' \
  geode-shell \
  row_for_path_matches_a_field_by_key_and_a_list_item_by_index

# §19.5: a path from another object never flags this draft's rows.
run_mutation "objectdialog: row_for_path is scoped to this object" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        let rest = path.strip_prefix(&format!("{doc}.{}.", self.name))?;' \
  '        let rest = path.strip_prefix(&format!("{doc}.")).and_then(|r| r.split_once('"'"'.'"'"')).map(|(_, r)| r)?;' \
  geode-shell \
  row_for_path_matches_a_field_by_key_and_a_list_item_by_index
```

Verify each anchor line matches the code you actually wrote (`grep -c -F`), fix the entry text rather than the code. `--anchors-only` exit 0; five CI checks.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-core crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(config): every reader fills Diagnostic.path; a diagnostic lands on its field row (4c §19.5)"
```

---

### Task 5: `ShellEvent::ReloadRejected`, the dialog's rejected status, the bridge's presentation diagnostics

**Files:**
- Modify: `crates/geode-shell/src/shell/mod.rs:199-216` (`ShellEvent`)
- Modify: `crates/geode-shell/src/shell/hot_reload.rs` (`apply_reload`: emit + log on the rejected outcome)
- Modify: `crates/geode-shell/src/shell/objectdialog/apply.rs` (`promote`, `schedule_flush`'s task, `finish_flush`)
- Modify: `crates/geode-app/src/bridge.rs` (`ShellEvent` match ~line 292–360; the reload path's `let (views, _) = load_views(config);` ~303)
- Test: `crates/geode-shell/src/shell/tests/reload.rs`, `tests/objectdialog.rs`, `crates/geode-app/src/bridge.rs` tests

**Interfaces:**
- Produces:
  ```rust
  pub enum ShellEvent { ConfigReloaded, RestartRequired(String), DistinctRequested(..), ReloadRejected(Vec<Diagnostic>) }
  // apply.rs
  fn promote(shell, seq, cx) -> Option<(PathBuf, BTreeMap<..>, Option<usize>)>;  // third: error count when memory rejected the batch
  pub(crate) fn finish_flush(shell, seq, outcome, rejected: Option<usize>, cx);
  pub(crate) const REJECTED_STATUS: &str = "saved to disk · rejected by the merge";  // painted as "<REJECTED_STATUS>: <n> error(s) — keeping last good"
  ```

- [ ] **Step 1: Failing test — the event** (`tests/reload.rs`, after `apply_reload_with_an_error_diagnostic_keeps_last_good_config`)

```rust
/// §19.6: a rejected reload says so as an event carrying the errors, so
/// a dialog (or the bridge) can tell a trader the file they just wrote
/// is on disk but not live.
#[gpui::test]
fn a_rejected_reload_emits_reload_rejected_with_the_errors(cx: &mut gpui::TestAppContext) {
    // (the same window/shell/bad_config setup as the test above — extract it into
    //  `fn bad_config_and_shell(cx) -> (Entity<ShellView>, VisualTestContext, Config)` and use it in both)
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let sink = events.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| sink.borrow_mut().push(e.clone()))
            .detach();
    });
    shell.update(&mut cx, |shell, cx| shell.apply_reload(bad_config, cx));
    let rejected: Vec<_> = events.borrow().iter().filter_map(|e| match e {
        ShellEvent::ReloadRejected(d) => Some(d.clone()),
        _ => None,
    }).collect();
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].iter().all(|d| d.severity == geode_core::config::Severity::Error));
    assert!(!rejected[0].is_empty());
}
```

(`cx` here is the `VisualTestContext`; this is the idiom `tests/picker.rs:98-105` uses.) Run — compile error (no variant).

- [ ] **Step 2: The event**

`ShellEvent` gains:

```rust
    /// The last reload was refused — `reload::decide` kept the previous
    /// config because the new one carried these error diagnostics (§19.6).
    /// Distinct from `RestartRequired`: nothing here is live, the file is
    /// on disk exactly as written, and a dialog that just wrote it needs
    /// to say so. Carries the diagnostics, not the count, so a consumer
    /// can name the file.
    ReloadRejected(Vec<geode_core::config::Diagnostic>),
```

`apply_reload`: before the `if let ReloadOutcome::Applied` block, `let rejected: Vec<Diagnostic> = if matches!(outcome, ReloadOutcome::KeptLastGood { .. }) { new_config.diagnostics.iter().filter(|d| d.severity == Severity::Error).cloned().collect() } else { Vec::new() };` — note `new_config` is moved inside the Applied block, so this must come first. After `self.last_reload = outcome;`: `if !rejected.is_empty() { for d in &rejected { tracing::error!(target: "geode::config", "{d}"); } cx.emit(ShellEvent::ReloadRejected(rejected)); }`. Add the bridge arm `ShellEvent::ReloadRejected(_) => {}` with a comment: the shell already logged and noted them; the bridge has nothing to forward. Run the test — PASS.

- [ ] **Step 3: Failing test — the dialog's status line** (`tests/objectdialog.rs`)

```rust
/// §19.6: a flush whose in-memory merge is refused still writes the file
/// (disk stays the arbiter), and the status line says both halves.
#[gpui::test]
fn a_flush_the_merge_rejects_says_saved_but_rejected(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // `keymap.mod = "ctrl"` is refused with an ERROR diagnostic at every
    // reload (Phase 4a Task 4b), so any batch applied over this user
    // layer is rejected while its own object is fine.
    std::fs::write(dir.path().join("app.toml"), "config_version = 1\n[keymap]\nmod = \"ctrl\"\n").unwrap();
    // `dialog_test_shell_in_dir` hands `user_dir` to `ShellView::new` as
    // the WRITE directory only; `services.config` is whatever was built,
    // so the user layer has to be loaded into it here.
    let mut services = test_services();
    let builtin = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[wide]\ndataset = \"risk\"\n[[wide.columns]]\nname = \"npv\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), builtin],
        desk: None,
        user: Some(dir.path().to_path_buf()),
    });
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("j enter");           // wide
    cx.run_until_parked();
    cx.simulate_keystrokes("j space");           // hide npv: a presentation write
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let status = shell.read_with(&cx, |s, _| s.config_write_error.clone()).unwrap();
    assert!(status.starts_with(objectdialog::apply::REJECTED_STATUS), "{status}");
    assert!(dir.path().join("view_presentation.toml").exists(), "the file was written regardless");
}
```

Run — FAILS (`config_write_error` is `None`).

- [ ] **Step 4: Implement**

`apply.rs`:

```rust
/// The status line for a flush whose in-memory merge was refused (§19.6).
pub(crate) const REJECTED_STATUS: &str = "saved to disk · rejected by the merge";

fn promote(shell, seq, cx) -> Option<(PathBuf, BTreeMap<(&'static str, String), ObjectEdit>, Option<usize>)> {
    ...
    apply_in_memory(shell, &user_dir, &edits, cx);
    // Read right after the apply: `apply_reload` just set `last_reload`.
    let rejected = match &shell.last_reload {
        crate::reload::ReloadOutcome::KeptLastGood { errors } => Some(errors.len()),
        _ => None,
    };
    Some((user_dir, edits, rejected))
}
```

`schedule_flush`'s task: `let Ok(Some((user_dir, edits, rejected))) = ...; ... finish_flush(shell, seq, outcome, rejected, cx)`. `finish_flush`'s `Ok(())` arm:

```rust
        Ok(()) => {
            if shell.pending_config_write.as_ref().is_some_and(|p| p.seq == seq) {
                shell.pending_config_write = None;
            }
            // §19.6: the file is on disk either way; what differs is
            // whether memory took it. A rejected merge is said in the
            // same status slot a failed write uses, and cleared by the
            // next flush memory accepts.
            match rejected {
                Some(n) => {
                    shell.config_write_error =
                        Some(format!("{REJECTED_STATUS}: {n} error(s) — keeping last good"));
                    cx.notify();
                }
                None => {
                    if shell.config_write_error.take().is_some() {
                        cx.notify();
                    }
                }
            }
        }
```

Update `finish_flush`'s and `promote`'s docs; `finish_flush` is `pub(crate)` — fix any test caller the compiler reports. Run the test — PASS.

- [ ] **Step 5: The bridge reports presentation diagnostics on reload**

In `bridge.rs`'s `ShellEvent::ConfigReloaded` arm, replace `let (views, _) = load_views(config);` with:

```rust
                let (views, presentation_diags) = load_views(config);
                // §19.6: the reload path used to discard these, so a
                // `view_presentation.toml` entry naming a view that no
                // longer exists warned once at startup and was silent
                // through every reload after — the trader renames a view
                // and their column order quietly stops applying. Reported
                // the way `data_setup`'s are at startup, and noted in the
                // entity through the data-batch door (append + dedupe),
                // never `note_config`, whose replace semantics belong to
                // `apply_reload` alone (Phase 4b MAJ-5).
                for d in &presentation_diags {
                    tracing::warn!(target: "geode::query", "{d}");
                }
                if !presentation_diags.is_empty() {
                    diagnostics.update(cx, |dg, cx| {
                        let before = dg.version();
                        dg.note_data_diagnostics(presentation_diags, SystemTime::now());
                        if dg.version() != before {
                            cx.notify();
                        }
                    });
                }
```

capturing `let diagnostics = diagnostics.clone();` in the subscribe block beside `handle`/`factory` (the `diagnostics` binding exists at line ~219). The test, in `bridge.rs`'s `mod tests`, in the exact shape of `a_refused_distinct_request_errors_the_picker`:

```rust
    /// §19.6: a reload no longer drops `load_views`'s presentation
    /// diagnostics — a stale `view_presentation.toml` name reaches the
    /// diagnostics entity on every reload, not only at startup.
    #[gpui::test]
    fn a_reload_reports_a_stale_presentation_name(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n").unwrap(),
                LayerDoc::builtin("view_presentation", "[gone]\nhidden = [\"npv\"]\n").unwrap(),
            ],
            desk: None,
            user: None,
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let reported = diagnostics.read_with(&vcx, |d, _| {
            d.data_diagnostics.iter().any(|(_, d)| d.message.contains("no view of that name"))
        });
        assert!(reported, "the reload path must report the stale presentation name");
    }
```

(`Diagnostics.data_diagnostics` is a `pub` field; `ShellEvent` needs importing in the test module if it is not already.)

- [ ] **Step 6: Harness entries**

```bash
# §19.6: the rejected event carries ERROR diagnostics only.
run_mutation "hot_reload: ReloadRejected carries only the errors" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '.filter(|d| d.severity == Severity::Error)' \
  '.filter(|_| true)' \
  geode-shell \
  a_rejected_reload_emits_reload_rejected_with_the_errors

# §19.6: a rejected merge is painted, not cleared, by the flush that hit it.
run_mutation "objectdialog: a rejected in-memory apply paints the status line" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        crate::reload::ReloadOutcome::KeptLastGood { errors } => Some(errors.len()),' \
  '        crate::reload::ReloadOutcome::KeptLastGood { errors } => { let _ = errors; None }' \
  geode-shell \
  a_flush_the_merge_rejects_says_saved_but_rejected

# §19.6: the reload path reports presentation diagnostics instead of dropping them.
run_mutation "bridge: reload reports presentation diagnostics" \
  crates/geode-app/src/bridge.rs \
  '                if !presentation_diags.is_empty() {' \
  '                if false && !presentation_diags.is_empty() {' \
  geode-app \
  a_reload_reports_a_stale_presentation_name
```

(Check the first anchor is unique in `hot_reload.rs`; if `.filter(|d| d.severity == Severity::Error)` appears elsewhere, anchor on the whole `let rejected` line.) `--anchors-only` exit 0; five CI checks.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-shell crates/geode-app scripts/mutation-check.sh
git commit -m "feat(shell): ShellEvent::ReloadRejected; a rejected in-memory apply says saved-but-rejected; reload reports presentation diagnostics (4c §19.6)"
```

---

### Task 6: Drift — `overrides.toml`

**Files:**
- Modify: `crates/geode-core/src/config/merge.rs:22` (`"overrides"` atomic at depth 1)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`OVERRIDES_DOC`, `override_key`, `override_entry`, `shadow_of`, `stale_override_keys`; `derive_rows` computes `drifted`; `ObjectRow::drifted` doc)
- Modify: `crates/geode-shell/src/shell/objectdialog/apply.rs` (`commit_edit` adds the entry on a fork)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`removal_edits` adds the overrides key; edit header drift note; the `drifted` badge comment)
- Test: `mod.rs` unit tests; `tests/objectdialog.rs` window tests

**Interfaces:**
- Produces:
  ```rust
  pub const OVERRIDES_DOC: &str = "overrides";
  pub fn override_key(doc: &str, object: &str) -> String;                       // "<doc>.<object>"
  pub fn override_entry(shadowed: Layer, object: &str, value: &toml::Value) -> toml::Value;   // { shadowed_layer, shadowed_text }
  pub fn shadow_of(config: &Config, doc: &str, object: &str) -> Option<(Layer, toml::Value)>; // last non-user layer defining it
  pub fn stale_override_keys(config: &Config) -> Vec<String>;                   // entries whose object the user layer no longer holds, or whose shadow is gone
  ```
  `derive_rows`: `drifted = overridden && entry present && object_text(shadow now) != shadowed_text`.

- [ ] **Step 1: Failing unit tests** (`mod.rs` tests; `config_from` exists there)

```rust
    #[test]
    fn drifted_needs_an_override_entry_and_a_changed_shadow() {
        let desk_v1 = "[tree]\ndataset = \"risk\"\ncolumns = []\n";
        let user = "[tree]\ndataset = \"risk\"\n";
        let entry = |text: &str| format!(
            "[\"views.tree\"]\nshadowed_layer = \"desk\"\nshadowed_text = '''\n{text}'''\n"
        );
        // Entry recorded against exactly the desk text on disk: not drifted.
        let shadow_text = object_text("tree", toml_value_to_item(&desk_v1.parse::<toml::Table>().unwrap()["tree"]));
        let recorded = entry(&shadow_text);
        let config = config_from(&[
            (Layer::Desk, "views", desk_v1),
            (Layer::User, "views", user),
            (Layer::User, "overrides", recorded.as_str()),
        ]);
        let rows = Domain::Views.objects(&config);
        assert!(rows[0].overridden);
        assert!(!rows[0].drifted, "the desk has not moved");

        // The desk adds a column: drifted.
        let desk_v2 = "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n";
        let config = config_from(&[
            (Layer::Desk, "views", desk_v2),
            (Layer::User, "views", user),
            (Layer::User, "overrides", recorded.as_str()),
        ]);
        assert!(Domain::Views.objects(&config)[0].drifted);

        // No entry at all (an override that predates 2b): never drifted.
        let config = config_from(&[(Layer::Desk, "views", desk_v2), (Layer::User, "views", user)]);
        assert!(!Domain::Views.objects(&config)[0].drifted);

        // Not overridden (user-only): an entry is stale and ignored.
        let stale = entry("x");
        let config = config_from(&[(Layer::User, "views", user), (Layer::User, "overrides", stale.as_str())]);
        assert!(!Domain::Views.objects(&config)[0].drifted);
        assert_eq!(stale_override_keys(&config), vec!["views.tree".to_string()]);
    }

    #[test]
    fn override_entry_records_the_shadowed_layer_and_its_text() {
        let value: toml::Value = "dataset = \"risk\"\n".parse::<toml::Table>().unwrap().into();
        let entry = override_entry(Layer::Desk, "tree", &value);
        assert_eq!(entry["shadowed_layer"].as_str(), Some("desk"));
        assert_eq!(entry["shadowed_text"].as_str(), Some(object_text("tree", toml_value_to_item(&value)).as_str()));
        assert_eq!(override_key("views", "tree"), "views.tree");
    }
```

(`config_from` builds layered docs from literal text — check its signature at `mod.rs:2027` and whether it parses `[\"views.tree\"]` quoted keys; `toml` does.) Run — compile errors.

- [ ] **Step 2: Implement the pure core**

`merge.rs`: add `| "overrides"` to the depth-1 list with a comment: `// \`overrides\` (4c §19.6): one entry per forked object, keyed "<doc>.<object>"; user layer only.`

`mod.rs`:

```rust
/// The drift sidecar (spec §5.2, §19.6): `overrides.toml`, user layer
/// only, one entry per forked object keyed `"<doc>.<object>"`, holding
/// the shadowed layer and the shadowed object's canonical TOML text at
/// fork time. A sidecar rather than a key inside the object, because an
/// atomic doc's reader treats an unknown key as a diagnostic.
pub const OVERRIDES_DOC: &str = "overrides";

pub fn override_key(doc: &str, object: &str) -> String {
    format!("{doc}.{object}")
}

/// The entry recorded when `object` is forked over `shadowed`'s copy.
/// The canonical text, not a hash: `DefaultHasher` is not stable across
/// Rust versions, a crypto dependency is unjustified, and keeping the
/// text makes a real diff free if it is ever wanted (§5.2).
pub fn override_entry(shadowed: Layer, object: &str, value: &toml::Value) -> toml::Value {
    let mut table = toml::Table::new();
    table.insert("shadowed_layer".into(), toml::Value::String(shadowed.name().to_string()));
    table.insert("shadowed_text".into(), toml::Value::String(object_text(object, toml_value_to_item(value))));
    toml::Value::Table(table)
}

/// The copy a user-layer write of `object` would shadow: the LAST
/// non-user layer defining it, with its value. `None` when no such
/// layer does — a user-only object forks nothing.
pub fn shadow_of(config: &Config, doc: &str, object: &str) -> Option<(Layer, toml::Value)> {
    config
        .layered_docs(doc)
        .iter()
        .filter(|d| d.layer != Layer::User)
        .filter_map(|d| d.table.get(object).map(|v| (d.layer, v.clone())))
        .last()
}

fn override_entries(config: &Config) -> BTreeMap<String, (String, String)> {
    config
        .layered_docs(OVERRIDES_DOC)
        .iter()
        .filter(|d| d.layer == Layer::User)
        .flat_map(|d| d.table.iter())
        .filter(|(k, _)| *k != "config_version")
        .filter_map(|(k, v)| {
            let t = v.as_table()?;
            Some((k.clone(), (
                t.get("shadowed_layer")?.as_str()?.to_string(),
                t.get("shadowed_text")?.as_str()?.to_string(),
            )))
        })
        .collect()
}

/// Entries that describe nothing any more (§19.6): the user layer no
/// longer holds the object, or no layer beneath shadows it. Ignored by
/// `derive_rows` and pruned by the next overrides write.
pub fn stale_override_keys(config: &Config) -> Vec<String> {
    override_entries(config)
        .keys()
        .filter(|key| {
            let Some((doc, object)) = key.split_once('.') else { return true };
            let user_has = config.layered_docs(doc).iter()
                .any(|d| d.layer == Layer::User && d.table.contains_key(object));
            !user_has || shadow_of(config, doc, object).is_none()
        })
        .cloned()
        .collect()
}
```

`derive_rows`: in the walk keep the shadow — change the accumulator to `(Vec<Layer>, ObjectRow, Option<toml::Value>)` where the third is the last non-user layer's value (`if layered.layer != Layer::User { entry.2 = Some(value.clone()); }`). Before the walk, `let entries = override_entries(config);`. In the final map:

```rust
            row.overridden = mine && layers.iter().any(|l| *l < Layer::User);
            // §19.6: drift is "the shadowed copy moved since the fork" —
            // provable only from the sidecar's recorded text, so no
            // entry means not drifted, never a guess.
            row.drifted = row.overridden
                && match (entries.get(&override_key(doc, &row.name)), shadow) {
                    (Some((_, recorded)), Some(value)) => {
                        object_text(&row.name, toml_value_to_item(&value)) != *recorded
                    }
                    _ => false,
                };
```

(`doc` is `derive_rows`'s parameter.) Rewrite `ObjectRow::drifted`'s doc to describe this. Run the unit tests — PASS.

- [ ] **Step 3: Failing window test — a fork records the entry, `r` removes it**

```rust
/// §19.6: the fork's own batch carries the overrides entry, so it lands
/// in the same flush; `r` removes it with the user copy.
#[gpui::test]
fn a_fork_records_an_override_entry_and_revert_removes_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // A builtin view and two datasets, so stepping `dataset` is a Doc edit
    // on an object the user does not own — a fork.
    let mut services = test_services();
    let views = LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"book\"\n").unwrap();
    let datasets = LayerDoc::builtin("datasets", "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n").unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), views, datasets],
        desk: None, user: None,
    });
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("space");            // dataset: risk → vol
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
    cx.simulate_keystrokes("enter");            // fork
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    assert!(overrides.contains("[\"views.tree\"]"), "{overrides}");
    assert!(overrides.contains("shadowed_layer = \"builtin\""), "{overrides}");
    assert!(overrides.contains("dataset = \"risk\""), "the shadowed text is the builtin's, not the fork: {overrides}");

    // The reload lands; the row is overridden, not drifted (nothing moved).
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-overridden-tree").is_some());
    assert!(cx.debug_bounds("objectdialog-drifted-tree").is_none());

    cx.simulate_keystrokes("enter r enter");     // revert to desk
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    assert!(!overrides.contains("views.tree"), "{overrides}");
}
```

(The test shell may need its mtime watcher or an explicit reload for the browse row to derive from the written user layer — the batch's `apply_in_memory` already applied it in memory, so `objectdialog-overridden-tree` should paint without a reload; if it does not, read how `ticking_a_dimension_in_an_empty_slot_writes_it_without_asking` asserts post-flush state and mirror it.) Run — FAILS (no `overrides.toml`).

- [ ] **Step 4: Wire the write and the removal**

`apply.rs` `commit_edit`, after `let edits = edits_for(shell);` and the empty check:

```rust
    let mut edits = edits;
    // §19.6: a Doc write onto an object the user layer does not own is a
    // fork; record what it shadows, in THIS batch, so the entry cannot
    // land without the fork nor before it. Stale entries ride along as
    // removals — the one moment the sidecar is being written anyway.
    let domain = shell.object_dialog.as_ref().map(|s| s.domain);
    if let Some(domain) = domain
        && would_fork(shell, domain)
        && let Some(draft) = shell.object_dialog.as_ref().and_then(|s| s.draft.as_ref())
        && let Some((layer, value)) = super::shadow_of(&shell.services.config, domain.doc(), &draft.name)
    {
        let state_domain = domain;
        let key = super::override_key(state_domain.doc(), &draft.name);
        for stale in super::stale_override_keys(&shell.services.config) {
            edits.insert((super::OVERRIDES_DOC, stale), None);
        }
        edits.insert((super::OVERRIDES_DOC, key), Some(super::override_entry(layer, &draft.name, &value)));
    }
```

(`would_fork` is the gate, not `shadow_of` alone: after the fork lands, `shadow_of` still answers the builtin for every later Doc edit and the entry would be re-written identically each time; `would_fork` is true only until the user layer owns the object. `draft.name.clone()` before the `let` chain if the borrow checker objects to holding `draft` across `shell.services.config`.)

`render.rs` `removal_edits` (find it; it returns the `(doc, object)` keys for `d`/`r`): append `(super::OVERRIDES_DOC, override_key(domain.doc(), &name))` when the loaded config's overrides doc holds that key (`override_entries` is private — add `pub(super) fn has_override_entry(config, doc, object) -> bool`), so a missing file is never created just to remove nothing. The notice's file list in `run_confirmed` filters `docs` by `keys` — add `OVERRIDES_DOC` to `docs` there so `overrides.toml` is named when removed.

Edit header (`build_edit`): when `row.drifted`, add the `drifted` badge beside `overridden` and, under the header, `div().text_xs().text_color(theme.warning).debug_selector(|| "objectdialog-drift-note".into()).child("the desk's copy has changed since you copied it — r restores it")`. Rewrite the browse painter's `drifted` comment ("Always false today…").

Run the window test — PASS.

- [ ] **Step 5: Harness entries**

```bash
# §19.6: no entry means NOT drifted — never a guess from the current desk copy.
run_mutation "objectdialog: drifted is false without an override entry" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                    _ => false,' \
  '                    (None, Some(_)) => true, _ => false,' \
  geode-shell \
  drifted_needs_an_override_entry_and_a_changed_shadow

# §19.6: drifted compares the shadow's CURRENT text against the recorded one.
run_mutation "objectdialog: drifted compares the shadow text" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                        object_text(&row.name, toml_value_to_item(&value)) != *recorded' \
  '                        object_text(&row.name, toml_value_to_item(&value)) == *recorded' \
  geode-shell \
  drifted_needs_an_override_entry_and_a_changed_shadow

# §19.6: the entry rides the fork's own batch.
run_mutation "objectdialog: a fork records its override entry" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        edits.insert((super::OVERRIDES_DOC, key), Some(super::override_entry(layer, &draft.name, &value)));' \
  '        let _ = (key, layer, value);' \
  geode-shell \
  a_fork_records_an_override_entry_and_revert_removes_it

# §19.6: the shadow is the last NON-USER layer.
run_mutation "objectdialog: shadow_of skips the user layer" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        .filter(|d| d.layer != Layer::User)' \
  '        .filter(|_| true)' \
  geode-shell \
  drifted_needs_an_override_entry_and_a_changed_shadow
```

Check `'                    _ => false,'` is unique in `mod.rs` (it will not be — anchor on a longer unique line or restructure the match into a named helper `fn drift_of(entry: Option<&(String, String)>, shadow: Option<&toml::Value>, name: &str) -> bool` and anchor inside it). `--anchors-only` exit 0; five CI checks.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-core crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(objectdialog): drift via overrides.toml — recorded on fork, removed on revert/delete (4c §19.6)"
```

---

### Task 7: Docs, harness as a set, full verification

**Files:**
- Modify: `CLAUDE.md` (a "Phase 4c Part 2b is done" paragraph after the Part 2 refinement / groupings / mouse-parity paragraphs; the harness count on the `zsh scripts/mutation-check.sh` line)
- Modify: `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` (new `### 19.8 As built`, and correct in place any §19 sentence the build contradicted)
- Modify: `scripts/mutation-check.sh` (header count, if it states one)

- [ ] **Step 1: Reconcile the spec.** Read §19 top to bottom against the code. Write §19.8 "As built" in §18.6's shape: what shipped per task, every deviation with its reason (the `naming_dataset` seed, `REJECTED_STATUS` living on `config_write_error`, `would_fork` as the drift gate, any selector or wording that differs), the display-pending note (no sandbox paints a window), and the harness count. Delete or rewrite §19 sentences the build made false rather than adding a correction beside them.

- [ ] **Step 2: `CLAUDE.md`.** One paragraph in the house style: text entry (`Draft::text_entry`, `i` on Text/Number, `parse_text`), Schema (`writable()`, `Field.layer`), Sources (flat prefixed list, `;` paths, idle empty paths, restart stripe), `Diagnostic.path` grammar and `row_for_path`, `ReloadRejected` + the rejected status, drift (`overrides.toml`, `would_fork` gate, stale pruning). Update the harness entry count on the command line (`grep -c '^run_mutation "' scripts/mutation-check.sh`).

- [ ] **Step 3: Full verification**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace 2>&1 | tail -3
cargo bench --workspace --no-run 2>&1 | tail -2
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
git status --porcelain   # must be empty before the harness
nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut.log 2>&1 &
```

Wait for `/tmp/mut.log` to end (poll with `tail -3 /tmp/mut.log` at intervals; do not busy-loop a subagent on it). Expected: every entry in changed files `caught`, `0 SURVIVED`. A `SURVIVED` line is a task's test that cannot see its own behaviour — fix the test, not the entry. `git status --porcelain` empty afterwards.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md docs scripts/mutation-check.sh
git commit -m "docs: Phase 4c Part 2b as built (§19.8), CLAUDE.md, harness count"
```

Then `superpowers:finishing-a-development-branch`.
