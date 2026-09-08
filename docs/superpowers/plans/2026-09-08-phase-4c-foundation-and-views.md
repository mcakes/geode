# Phase 4c, Part 1 — Foundation and the Views Dialog

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One config write door, one shared object-dialog scaffold, and the Views dialog on top of them — the hardest adapter first, so it settles the field vocabulary before three more depend on it.

**Architecture:** `config_write` becomes the single door every config write goes through, replacing three `write_atomic` implementations and backing six existing persist paths. `shell::objectdialog` is a two-stage modal (browse named objects → edit one object's fields) in the mould of `keybindings_view`, consuming the two-mode vocabulary that merged as `crates/geode-shell/src/dialogmode.rs`. A `Domain` enum supplies what differs per config doc; this plan implements only `Domain::Views`.

**Tech Stack:** Rust, gpui + gpui-component (pinned), `toml_edit`, criterion, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md`
**Also binding:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` — the key vocabulary, now merged and as-built.

## Why this plan stops at Views

The spec sequences seven task groups. This plan covers the first three, ending with a working Views dialog. Groupings, Scopes, Sources and the schema inspector get their own plan, written against the scaffold **as built** rather than as specified — the spec itself puts Views first precisely so the hardest shape settles the vocabulary before the thin adapters are written, and a plan that guessed at the scaffold's real signatures would be guessing at exactly what Views is meant to determine.

## Global Constraints

- Every new lib/bin target needs `bench = false`; `[[bench]]` targets need `harness = false`.
- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`.
- **TDD**: failing test, run it, watch it fail for the right reason, then implement.
- **A mutation entry for every behaviour changed.** Run `zsh scripts/mutation-check.sh --changed` after every task. **Commit before mutating.** Before adding an anchor, confirm it occurs exactly once **as a substring** — the harness does a single `str.replace(…, 1)`, so a shorter-indented anchor can match a longer-indented line earlier in the file. Also re-check pre-existing entries in any file you touch: a refactor can silently orphan one.
- No crate other than `geode-data` may open a file or socket. `geode-shell` never depends on `geode-data`.
- **Nothing may stall the render thread.** Config writes go on the background executor, following `ShellView::persist_theme` (`crates/geode-shell/src/shell/input.rs:326`).
- **Never a raw colour** — every colour from `cx.theme()` tokens.
- Modals open only through `shell::dialog::open_shell_dialog_with_key`.
- Doc comments explain WHY, densely.

## As-built vocabulary this plan builds on

Merged in `b5f0d7f`. Use these exact names:

```rust
// crates/geode-shell/src/dialogmode.rs
pub enum DialogMode { Normal, Filter }
pub enum EscapeStep { LeaveFilter, ClearQuery, PreviousStage, Close }
pub fn escape_step(mode: DialogMode, query_is_empty: bool, has_previous_stage: bool) -> EscapeStep;
pub enum NormalCommand { Nav(NavCommand), EnterFilter, Commit, Toggle, MoveItem(i32), EditText, Verb(char) }
pub fn normal_command(ks: &Keystroke) -> Option<NormalCommand>;

// crates/geode-shell/src/shell/dialog.rs
pub(crate) fn mode_pill(mode: DialogMode, cx: &App) -> AnyElement;
// `tab`/`shift-tab` are reclaimed in the "GeodeModalOpen" context, which
// `ShellView::render` puts on the root element while `self.modal.is_some()`.
```

`keybindings_view` is the worked example of all of it: `KeybindingsState.mode`, `notice: Option<String>`, `begin_capture`, the mode pill in the content's first row, and the mode-aware footer hint.

**`EscapeStep::PreviousStage` is unreachable in the keybinding dialog but IS reachable here** — the edit stage returns to browse. This plan is the first consumer of that rung.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-shell/src/config_write.rs` | **NEW.** `read`/`write`/`edit`, the one door. One temp-file-and-rename. |
| `crates/geode-shell/src/theme.rs`, `fontsize.rs`, `vimfind.rs`, `frame.rs`, `keymap_edit.rs`, `session.rs` | Migrated onto `config_write`; their three `write_atomic` copies deleted. |
| `crates/geode-core/src/source_config.rs` | **NEW.** `SourceSpec::from_doc`, moved from `geode-data`. |
| `crates/geode-core/src/view.rs` | `ColumnPresentation.hidden`; `ViewPresentationSpec::from_doc`. |
| `crates/geode-core/src/config/load.rs` | The presentation merge, after the named-object merge. |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | **NEW.** Pure core: `Stage`, `ObjectDialogState`, `Draft`, `Field`, `FieldKind`, `Destination`, `ObjectRow`, `Domain`. |
| `crates/geode-shell/src/shell/objectdialog/views.rs` | **NEW.** The `Domain::Views` adapter. |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | **NEW.** The gpui shell: browse list, edit list, action bar, hints. |
| `crates/geode-shell/src/shell/tests/objectdialog.rs` | **NEW.** Real-key-dispatch tests. |
| `crates/geode-shell/src/defaults.rs` | `config::views` action. |
| `scripts/mutation-check.sh`, `CLAUDE.md` | Entries; count. |

---

### Task 1: The write door

**Files:**
- Create: `crates/geode-shell/src/config_write.rs`
- Modify: `crates/geode-shell/src/lib.rs`, `theme.rs`, `fontsize.rs`, `vimfind.rs`, `frame.rs`, `keymap_edit.rs`, `session.rs`

**Interfaces:**
- Produces: `config_write::{read, write, edit}` — see Step 3 for exact signatures.

Three `write_atomic` implementations exist today (`theme.rs:524`, `keymap_edit.rs:387`, `session.rs:972`) and six persist paths use them. This task makes it one. It lands first because everything else writes through it and because it is the only task here that can regress shipped behaviour.

- [ ] **Step 1: Read all three existing implementations before writing anything**

`theme.rs:524`, `keymap_edit.rs:387`, `session.rs:972`. They differ — at least one includes the pid in the temp filename. Note every difference and keep the union of their guarantees; a migration that quietly drops one is a regression. Record the differences in your report.

- [ ] **Step 2: Write the failing tests**

New `mod tests` in `config_write.rs`. Use `tempfile::tempdir()`, the pattern `keymap_edit`'s tests use.

```rust
#[test]
fn write_is_atomic_and_creates_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("nested");
    write(&sub, Layer::User, "app", "config_version = 1\n").expect("write");
    assert_eq!(
        std::fs::read_to_string(sub.join("app.toml")).unwrap(),
        "config_version = 1\n"
    );
    // No temp file survives a successful write.
    let strays: Vec<_> = std::fs::read_dir(&sub).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy() != "app.toml")
        .collect();
    assert!(strays.is_empty(), "temp files must not survive: {strays:?}");
}

/// The whole point of `edit`: a user's comments and unrelated keys are
/// theirs, and a keyed persist must not eat them.
#[test]
fn edit_preserves_comments_and_unrelated_keys() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.toml"),
        "# mine\nconfig_version = 1\n\n[ui]\nfont_size = \"small\"\n\n[theme]\nname = \"Ayu Dark\"\n",
    ).unwrap();
    edit(dir.path(), Layer::User, "app", |doc| {
        doc["theme"]["name"] = toml_edit::value("Bloomberg");
    }).expect("edit");
    let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
    assert!(text.contains("# mine"), "{text}");
    assert!(text.contains(r#"font_size = "small""#), "{text}");
    assert!(text.contains(r#"name = "Bloomberg""#), "{text}");
}

/// A file the user hand-edited into a broken state is theirs. Refuse,
/// leave it byte-for-byte, and say so — the contract
/// `fontsize::persist_to_user_config` already keeps.
#[test]
fn edit_refuses_an_unparseable_file_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let broken = "config_version = = 1\n";
    std::fs::write(dir.path().join("app.toml"), broken).unwrap();
    let err = edit(dir.path(), Layer::User, "app", |doc| {
        doc["theme"]["name"] = toml_edit::value("x");
    }).expect_err("must refuse");
    assert!(err.contains("app.toml"), "the error names the file: {err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("app.toml")).unwrap(),
        broken,
        "the file must be untouched"
    );
}

/// Only the user layer is writable. Desk and builtin are refused rather
/// than attempted, so a bug cannot write a shared desk file.
#[test]
fn only_the_user_layer_is_writable() {
    let dir = tempfile::tempdir().unwrap();
    for layer in [Layer::Builtin, Layer::Desk] {
        assert!(write(dir.path(), layer, "app", "x = 1\n").is_err(), "{layer:?}");
        assert!(edit(dir.path(), layer, "app", |_| {}).is_err(), "{layer:?}");
    }
    assert!(!dir.path().join("app.toml").exists(), "nothing was written");
}

/// `edit` on a doc that does not exist yet creates it — every keyed
/// persist starts from no file on a fresh install.
#[test]
fn edit_creates_a_missing_doc() {
    let dir = tempfile::tempdir().unwrap();
    edit(dir.path(), Layer::User, "app", |doc| {
        doc["ui"]["font_size"] = toml_edit::value("large");
    }).expect("edit");
    let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
    assert!(text.contains("large"), "{text}");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p geode-shell --features test-support config_write`
Expected: FAIL to compile — `write`, `edit` not found.

- [ ] **Step 4: Implement**

```rust
//! The one door every config write in this crate goes through
//! (`docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` §6).
//!
//! Before this module there were three `write_atomic` implementations
//! and six persist paths spread across `theme`, `fontsize`, `vimfind`,
//! `frame` (twice) and `keymap_edit`. Phase 4c's dialogs would have made
//! it ten. One door means one set of guarantees to get right: a write is
//! atomic, only the user layer is writable, and a file this process
//! cannot parse is refused untouched rather than replaced.
//!
//! No `ShellServices` parameter: `user_dir` is what a caller actually
//! has, and threading a whole services struct through the background
//! executor for a path would be worse.

use std::path::Path;
use toml_edit::DocumentMut;
use geode_core::config::Layer;

pub fn read(user_dir: &Path, layer: Layer, doc: &str) -> Result<String, String>;
pub fn write(user_dir: &Path, layer: Layer, doc: &str, text: &str) -> Result<(), String>;
pub fn edit(
    user_dir: &Path,
    layer: Layer,
    doc: &str,
    f: impl FnOnce(&mut DocumentMut),
) -> Result<(), String>;
```

`write` is the union of the three existing implementations from Step 1. `edit` is read-or-create, parse (refusing on failure, file untouched), apply `f`, `write`. Both refuse any layer but `Layer::User` before touching the filesystem.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --features test-support config_write`
Expected: PASS, 5 tests.

- [ ] **Step 6: Migrate the six persist paths**

Rewrite each to call `config_write::edit`, and **delete all three `write_atomic` implementations**. Each persist keeps its existing public signature — they are the seams other modules and tests drive.

Run the full suite after each migration, not once at the end: `cargo test --workspace`. Existing tests for `fontsize`, `vimfind`, `theme` and `keymap_edit` cover comment preservation and unparseable-file refusal; they must stay green without modification. **If a test needs changing to accommodate the migration, stop and report it** — that means behaviour changed, which this task must not do.

- [ ] **Step 7: Full gate, commit, harness**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A && git commit -m "refactor(shell): one config write door, replacing three write_atomic copies"
zsh scripts/mutation-check.sh --changed
```

Add entries for the door's own guarantees — a green suite would not see these:

```bash
run_mutation "config_write: a non-user layer is writable" \
  crates/geode-shell/src/config_write.rs \
  '<the layer guard, verbatim from your implementation>' \
  '<the guard defeated>' \
  geode-shell only_the_user_layer_is_writable

run_mutation "config_write: an unparseable file is overwritten" \
  crates/geode-shell/src/config_write.rs \
  '<the parse-failure early return, verbatim>' \
  '<the return removed or defeated>' \
  geode-shell edit_refuses_an_unparseable_file_without_touching_it
```

Every entry that the deleted `write_atomic` implementations anchored on must be re-anchored. Check for these before you finish — `grep` the script for `write_atomic`.

---

### Task 2: `SourceSpec::from_doc` moves to `geode-core`

**Files:**
- Create: `crates/geode-core/src/source_config.rs`
- Modify: `crates/geode-core/src/lib.rs`, `crates/geode-data/src/source/config.rs`, `crates/geode-data/src/source/discovery.rs`

**Interfaces:**
- Produces: `geode_core::source_config::SourceSpec` and its `from_doc(&MergedDoc, &SchemaSpec) -> (Vec<SourceSpec>, Vec<Diagnostic>)`, re-exported from `geode_data::source` so existing call sites are unchanged.

Spec §2.2. `geode-shell` may never depend on `geode-data`, so a Sources dialog cannot otherwise run the reader that validates what it writes. This lands here, ahead of the dialogs, because it touches the crate graph and should not be entangled with UI work.

- [ ] **Step 1: Read the current reader and its type**

`crates/geode-data/src/source/config.rs:43` (`from_doc`) and `discovery.rs:36` (`SourceSpec`). Confirm the reader's only dependencies are `MergedDoc`, `SchemaSpec` and `Diagnostic` — all in `geode-core`. **If it depends on anything else in `geode-data`, stop and report**: the move is not the mechanical one this task assumes.

- [ ] **Step 2: Move, re-export, and prove nothing changed**

Move `SourceSpec` and `from_doc` verbatim into `crates/geode-core/src/source_config.rs`. Re-export from `geode-data`:

```rust
// crates/geode-data/src/source/discovery.rs (or config.rs, wherever the
// type was defined): the type now lives in geode-core so geode-shell can
// validate a sources doc without depending on this crate (Phase 4c §2.2).
pub use geode_core::source_config::SourceSpec;
```

Move the reader's existing tests with it. **Change no test assertions** — this is a move, and a changed assertion means it was not.

- [ ] **Step 3: Verify**

```bash
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
```
Expected: green, with every moved test passing unchanged.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor(core): SourceSpec::from_doc moves to geode-core

geode-shell may never depend on geode-data, so a Sources dialog could not
otherwise run the reader that validates what it writes. Discovery and
ingest keep the type by re-export; no call site changes."
```

---

### Task 3: `ViewPresentationSpec` and the loader merge

**Files:**
- Modify: `crates/geode-core/src/view.rs`, `crates/geode-core/src/config/load.rs`
- Test: in both files' `mod tests`

**Interfaces:**
- Produces: `ColumnPresentation.hidden: Option<bool>`; `ViewPresentationSpec::from_doc(&MergedDoc) -> (ViewPresentationSpec, Vec<Diagnostic>)`; the merge applied in the loader.

**A correction to the spec, already verified — carry it out as written here, not as §1.2 words it.** Spec §1.2 says presentation adds "a `hidden: bool` on `ViewColumn`". `ViewColumn` is an **enum** (`view.rs:23`, `Dimension { name }` / `Measure { name }`) with no shared struct fields, so there is nowhere to put it. `ColumnPresentation` (`view.rs:141`) already exists per column, already carries `width: Option<f32>`, and is already merged into `ViewSpec.presentation: BTreeMap<String, ColumnPresentation>`. **`hidden` goes there.** Record this in your report; Task 6 puts it in the spec's As-built.

- [ ] **Step 1: Write the failing tests**

```rust
// in crates/geode-core/src/view.rs's mod tests
#[test]
fn presentation_reads_order_hidden_and_width_per_view() {
    let doc = merged_doc(r#"
config_version = 1
[tree]
order = ["book", "npv", "delta01"]
hidden = ["cross_gamma02"]
[tree.width]
npv = 120
"#);
    let (spec, diags) = ViewPresentationSpec::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    let tree = spec.views.get("tree").expect("tree");
    assert_eq!(tree.order, vec!["book", "npv", "delta01"]);
    assert!(tree.hidden.contains("cross_gamma02"));
    assert_eq!(tree.width.get("npv").copied(), Some(120.0));
}

/// A desk that renames a column must not break a personal file. The
/// name is warned about and ignored, never an error.
#[test]
fn a_column_the_view_lacks_is_a_warning_not_an_error() {
    // Build a ViewSpec with columns book, npv; a presentation naming
    // `gone`. Merge. Expect: a Warning diagnostic naming `gone`, no
    // Error, and the view's own columns untouched.
}
```

Then, in `config/load.rs`'s tests:

```rust
/// The merge runs AFTER the named-object merge, so a user-layer view
/// override and a presentation file compose rather than race.
#[test]
fn presentation_is_merged_over_the_view_after_the_named_object_merge() {
    // Fixture: desk `views` with tree = [book, npv, delta01];
    //          user `view_presentation` with order [npv, book] and
    //          hidden [delta01].
    // Expect the loaded ViewSpec's column order to be npv, book, and
    // delta01's presentation.hidden == Some(true) — with the view still
    // attributed to the DESK layer, since presentation does not fork it.
}
```

Fill both bodies with real fixtures following the file's existing test helpers — read them first.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core presentation`
Expected: FAIL — `ViewPresentationSpec` not found.

- [ ] **Step 3: Implement**

Add `hidden: Option<bool>` to `ColumnPresentation`. Add `ViewPresentationSpec`, atomic at depth one per view name (the same rule `config::merge::atomic_depth` applies to `views`). Apply the merge in the loader after the named-object merge, so `ViewSpec.columns` order and each column's `ColumnPresentation` reflect it **before any module sees the view**.

Keep `Config::explain` reporting the view's own layer for the view — the presentation doc is separate, which spec §15 open question 4 already accepts.

- [ ] **Step 4: Verify, commit, harness**

```bash
cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
git add -A && git commit -m "feat(core): ViewPresentationSpec, merged over the view after the named-object merge"
zsh scripts/mutation-check.sh --changed
```

Entries for: the merge running before modules see the view (mutate the merge out; the loader test must fail); an unknown column becoming an error rather than a warning.

---

### Task 4: The scaffold's browse stage

**Files:**
- Create: `crates/geode-shell/src/shell/objectdialog/mod.rs`, `objectdialog/views.rs`, `objectdialog/render.rs`
- Create: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (the `object_dialog` field + its scroll handle), `defaults.rs` (`config::views`)

**Interfaces:**
- Consumes: `dialogmode::{DialogMode, EscapeStep, NormalCommand, escape_step, normal_command}`; `dialog::{open_shell_dialog_with_key, mode_pill}`.
- Produces: `Stage::{Browse, Edit{object}}`; `ObjectDialogState { domain, stage, selected, query, mode, draft, notice }`; `Domain::Views`; `ObjectRow { name, summary, layer, overridden, drifted }`; `Domain::objects(&Config) -> Vec<ObjectRow>`.

Browse only. No editing, no writing. The stage this task ends at is: `config::views` from the palette lists the views with their owning layer and an `overridden` marker, `j`/`k` move, `/` filters, `escape` closes.

- [ ] **Step 1: Write the pure-core failing tests**

In `objectdialog/mod.rs`'s `mod tests`, over a fixture `Config` with a builtin `views` doc and a user override of one view:

```rust
#[test]
fn a_row_carries_the_layer_that_won_and_marks_a_user_override() {
    let config = config_from(&[
        (Layer::Builtin, "views", "[tree]\ndataset = \"risk\"\n[wide]\ndataset = \"risk\"\n"),
        (Layer::User,    "views", "[tree]\ndataset = \"risk\"\n"),
    ]);
    let rows = Domain::Views.objects(&config);
    let tree = rows.iter().find(|r| r.name == "tree").expect("tree");
    let wide = rows.iter().find(|r| r.name == "wide").expect("wide");
    assert_eq!(tree.layer, Layer::User);
    assert!(tree.overridden, "user layer plus an earlier layer means overridden");
    assert_eq!(wide.layer, Layer::Builtin);
    assert!(!wide.overridden, "a view only one layer defines is not overridden");
}

/// A view the user layer alone defines is theirs, not an override —
/// getting this wrong would offer "Revert to desk" on a view no desk
/// has, and reverting would delete it.
#[test]
fn a_user_only_object_is_not_marked_overridden() {
    let config = config_from(&[(Layer::User, "views", "[mine]\ndataset = \"risk\"\n")]);
    let rows = Domain::Views.objects(&config);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].overridden);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support objectdialog`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the pure core and the Views adapter's `objects`**

`ObjectRow`, `Domain`, `Stage`, `ObjectDialogState`. Derive rows from `Config::layered_docs(name)`, which returns Builtin → Desk → User order: `layer` is the last layer containing the name; `overridden` is "the user layer contains it AND an earlier layer does too". `drifted` returns `false` for now — `overrides.toml` is Part 2's work; leave the field and say so in its doc comment.

**Rows derive fresh on every render and keystroke** — no caching, the contract `keybindings_view::derive_rows` and `settings_view::derive_rows` both hold.

- [ ] **Step 4: Wire the modal**

`render.rs` paints: `mode_pill` first, the shared filter row, the browse list, the footer hint. Open through `open_shell_dialog_with_key` with `focus_filter: false` (normal mode). Route keys by mode exactly as `keybindings_view::handle_key` does — read it and follow it; do not invent a second routing shape. Register `config::views` in `defaults.rs` with **no default binding** (spec §10: palette-only, as `keybindings::open` is).

- [ ] **Step 5: End-to-end test through real key dispatch**

```rust
#[gpui::test]
fn config_views_opens_in_normal_mode_and_lists_the_views(cx: &mut gpui::TestAppContext) {
    // dispatch "config::views"; assert mode == Normal, a row painted per
    // view, and that a bare letter does NOT reach the filter.
}

#[gpui::test]
fn slash_filters_and_escape_walks_the_ladder(cx: &mut gpui::TestAppContext) {
    // "/" then text narrows; escape leaves filter keeping the query;
    // escape clears the query; escape closes. Four rungs, one at a time.
}
```

- [ ] **Step 6: Gate, commit, harness**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A && git commit -m "feat(shell): the object dialog's browse stage, over Domain::Views"
zsh scripts/mutation-check.sh --changed
```

Entries for: `overridden` computed from the winning layer alone (ignoring whether an earlier layer defines it) — the mutation that would offer a destructive revert on a user-only view; and the dialog opening in filter mode, which silently restores the old model and passes every filter test.

---

### Task 5: The edit stage

**Files:**
- Modify: `objectdialog/mod.rs`, `objectdialog/views.rs`, `objectdialog/render.rs`, `shell/tests/objectdialog.rs`

**Interfaces:**
- Consumes: Task 1's `config_write`, Task 3's `ViewPresentationSpec`, Task 4's scaffold.
- Produces: `Field { key, label, kind, dest }`; `FieldKind::{Text, Number, Bool, Choice, MultiChoice, OrderedList}`; `ListItem { name, included, width }`; `Destination::{Doc, Presentation}`; `Draft`; `Domain::{fields, to_table, validate}`.

This is where the spec's hardest shape lands, and the reason Views goes first.

- [ ] **Step 1: Write the pure-core failing tests**

The one that matters most — the destination split. Getting it wrong forks a desk view on a column drag, which §4.1 exists to prevent:

```rust
#[test]
fn presentation_fields_and_doc_fields_go_to_different_destinations() {
    let config = demo_config();
    let fields = Domain::Views.fields(&config, Some("tree"));
    let dataset = fields.iter().find(|f| f.key == "dataset").expect("dataset");
    assert_eq!(dataset.dest, Destination::Doc, "the dataset defines the view");
    let columns = fields.iter().find(|f| f.key == "columns").expect("columns");
    assert_eq!(
        columns.dest, Destination::Presentation,
        "order, inclusion and width are presentation — a dragged width must \
         never fork a desk view"
    );
}

/// `space` toggles inclusion; `shift+j` moves the item, not the cursor.
#[test]
fn an_ordered_list_toggles_and_reorders() {
    let mut draft = draft_for("tree");
    let before: Vec<String> = list_names(&draft);
    draft.toggle_selected();   // space on item 0
    assert!(!list_items(&draft)[0].included);
    draft.move_item(1);        // shift+j
    assert_eq!(list_names(&draft)[1], before[0], "the item moved, not the cursor");
}

/// A draft groups its writes by destination, so one save is at most one
/// `config_write::edit` per file — never a write per field.
#[test]
fn a_save_groups_writes_by_destination() {
    let draft = dirty_draft_touching_both();
    let groups = draft.writes_by_destination();
    assert_eq!(groups.len(), 2);
    assert!(groups.contains_key(&Destination::Doc));
    assert!(groups.contains_key(&Destination::Presentation));
}
```

- [ ] **Step 2: Run to verify they fail**, then implement `Field`, `FieldKind`, `ListItem`, `Destination`, `Draft` and the Views adapter's `fields`/`to_table`/`validate`.

`validate` renders the draft with `to_table`, wraps that table alone in a `MergedDoc`, and runs `ViewSpec::from_doc` over it (spec §7.2 — validate the object being edited, not the merged result). Attach each `Diagnostic` to the field whose `key` matches its `path`.

- [ ] **Step 3: The action bar, and staging**

Field edits **stage into the draft and never write** (spec §3.2). The action bar sits below the row list, outside the scroll, so the list never changes length as the draft goes dirty: `Save changes` (only while dirty), `Delete this view`, `Revert to desk` (only when overridden), `Copy to user layer` (instead of those, when the winning layer is builtin or desk). Each is a button showing its letter — `s`, `d`, `r` — and each is clickable. Take every colour from `cx.theme()`.

`escape` from `Edit` uses `escape_step(mode, query_is_empty, has_previous_stage: true)` — **this plan is the first consumer of `EscapeStep::PreviousStage`.** A dirty draft replaces the action block with a single `Discard unsaved changes?` row first.

- [ ] **Step 4: Saving**

On `Save changes`: group by destination, then one `config_write::edit` per group, **on the background executor** following `ShellView::persist_theme` — never on the render thread. Do not apply anything yourself: the 500 ms mtime watcher reloads, and the browse list re-derives (spec §7.1). Set an immediate notice acknowledging the write, in the tense the keybinding dialog now uses ("saving…", not "saved") — it has not been confirmed yet.

- [ ] **Step 5: End-to-end**

```rust
#[gpui::test]
fn hiding_a_column_writes_presentation_and_does_not_fork_the_view(cx: &mut gpui::TestAppContext) {
    // Open config::views on a DESK-layer view, enter it, space a column
    // off, `s`. Assert: view_presentation.toml written with that column
    // hidden, AND the user-layer views.toml does NOT exist — the desk
    // view was not forked.
}
```

That last assertion is the whole point of the destination split; without it the test passes on a broken implementation.

- [ ] **Step 6: Gate, commit, harness**

Entries for: a `Presentation` field written to `Doc` (forks a desk view on a drag); validation run against the merged doc rather than the draft alone; a field edit writing immediately instead of staging; `escape` on a dirty draft skipping the confirm row.

---

### Task 6: Docs and harness

**Files:** `CLAUDE.md`, the 4c spec, `scripts/mutation-check.sh`

- [ ] **Step 1: `CLAUDE.md`** — record that 4c Part 1 landed: the `config_write` door and that no module opens a config file itself; `ViewPresentationSpec` and why a dragged width does not fork a desk view; the object dialog and that `Domain::Views` is the only adapter so far.

- [ ] **Step 2: Spec As-built** — at minimum: `hidden` lives in `ColumnPresentation`, not on `ViewColumn`, because that type is an enum (Task 3); `drifted` is present but always `false` until `overrides.toml` lands in Part 2; anything else that differed.

- [ ] **Step 3: The harness count.**

```bash
echo $(( $(grep -c '^run_mutation' scripts/mutation-check.sh) - $(grep -c '^run_mutation()' scripts/mutation-check.sh) ))
```
Put that in `CLAUDE.md`. **Note:** `worktree-phase-4b-diagnostics` also edits that line; whoever merges second recounts.

- [ ] **Step 4: The full harness.** Run `zsh scripts/mutation-check.sh` detached (~1 hour). Every line must read `caught`.

---

## Self-Review

**Spec coverage.** §6 write door → Task 1. §2.2 `SourceSpec` move → Task 2. §5.6/§1.2 presentation → Task 3 (with the `ColumnPresentation` correction). §3 scaffold + §5.1 markers → Task 4. §3.1/§4/§4.1/§7.2 fields, destinations, validation → Task 5. §10 actions → Task 4 step 4. **§3.3 is only partly covered by Task 5**, not fully as an earlier version of this line claimed: `Bool`, `Choice` and `OrderedList` (toggle-include, reorder) land. Three things do not: `MultiChoice` — no per-option row exists, the same Part 2 status as `Text`; a `Number` field's `shift+space` step-down — `dialogmode::normal_command` has no `shift+space` and `Number` steps forward only; and setting an `OrderedList` item's `width` sub-row, since nothing writes it. Views has no `Number` or `MultiChoice` field and this plan's own end-to-end test never sets a width, so none of the three gaps was forced into view; the `Number` and width gaps are recorded as named Part 2 tasks in the spec's §16 — `MultiChoice` is not, since §1.3's done state never promises it in Part 1; its own Part 2 status is stated directly in `mod.rs`'s `FieldKind` doc comment instead. **Deliberately out of this plan** (Part 2): §5.2 drift and `overrides.toml`, §8.2–8.4 Groupings/Scopes/Sources, §8.3's reload prompt and swappable `DataHandle`, §9 the schema inspector.

**Placeholders.** Task 3 Step 1's second test and Task 4/5's `#[gpui::test]` bodies are specified as intent plus required assertions rather than literal code, because they need fixture helpers from files the implementer must read first; each names exactly what it must assert. Task 1 Step 7's mutation anchors are marked `<verbatim from your implementation>` because an anchor must match real code and be substring-unique — inventing one here would produce a lying entry.

**Type consistency.** `Destination`, `Field`, `FieldKind`, `ListItem`, `Draft`, `Domain`, `ObjectRow`, `Stage` are used in Task 5 exactly as Task 4 defines them. `ColumnPresentation.width` is `Option<f32>` (matching `view.rs:148`), so `ListItem.width` is `Option<f32>` too — not the `Option<u32>` the spec's §3.1 sketch wrote.
