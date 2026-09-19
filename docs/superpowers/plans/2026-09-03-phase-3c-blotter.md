# Phase 3c — Blotter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the blotter — a collapsible, keyboard-driven, honestly
marked view of any view definition — boot it on generated data with
`--demo`, and delete the throwaway probe.

**Architecture:** `geode-blotter` is a pure core (column plan, expansion
by path, flatten, cursor, find, formatting, format cache, yank, the `:`
grammar) with no `gpui`, plus a thin `TableDelegate` adapter over
gpui-component's `DataTable` and one gpui entity per tile that observes
the frame, submits keyed queries through `DataHandle`, and applies
outcomes. `geode-app` gains the data bridge that drains the event channel
into the shell, the roster with the blotter factory, config-to-service
wiring, the database path, and `--demo`. The probe and everything that
exists only for it go.

**Tech Stack:** Rust 2024, gpui, gpui-component `0e2fb7a` (`DataTable`,
`TableState`, `TableDelegate`, `Column`, `ColumnSort`, `TableEvent`,
`Size::XSmall` rows), `async-channel` 2.5 (already in `Cargo.lock`),
`chrono`, `criterion`.

**Spec:** `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md`
§1.2, §2.2, §4.3, §5.4, §6, §7, §9 step 5. Plans 3a and 3b are
prerequisites and are consumed as-is.

## Global Constraints

- **Layering:** `geode-blotter` depends on `geode-shell`, `geode-data`,
  `geode-core`; it opens no file and no socket. `geode-app` is where the
  roster and the bridge meet.
- **CI:** all four checks on macOS and Windows; every task ends green.
- **`bench = false` on the lib; `harness = false` on the bench.**
- **No raw colours.** `chart_bullish` / `chart_bearish` are the sign
  tokens; `warning` is the stale/as-of token.
- **Per-frame heap churn is a defect.** `render_td` reads a cached cell
  or paints nothing; formatting happens in `visible_rows_changed` and on
  snapshot arrival only. Flatten, expansion and find run at keypress or
  delivery time.
- **The read path's opinions are law (spec §6.5):** `NonAttributable`
  paints blank, never `0.00`; only null-honouring accessors are used;
  markers are per row by depth.
- **Modules never bind keys.** The blotter registers actions; bindings
  live in `defaults.rs` under `blotter && mode == …`. `DataTable`'s own
  bindings are reclaimed to `NoAction`.
- **Commit before you mutate**; harness entries per task; an unfiltered
  run at the end.
- Commit trailers:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL
  ```

## What already exists (do not rebuild)

From 3a: `geode_core::query::{AsOf, QueryKey, QueryOutcome}`;
`Snapshot::{column_index, columns, meta_at, f64_at, i64_at, text_at,
display_at, dict_codes_at, grouping, tree, has_depth_column,
depth_of_row}`; `geode_core::tree::TreeIndex::{roots, children, parent,
depth, has_children, unplaced, len}`; `geode_core::view::{ColumnFormat,
ColumnPresentation, Negative, Colour, Scale, ViewSpec::presentation_of}`;
`geode_data::{DataHandle, DataService, DataServiceConfig, DataEvent,
EventSink, QueryParams, Request}`; `DataHandle::for_tests()` under
`test-support`; `SourceSpec::from_doc`.

From 3b: `geode_shell::module::{TileContent, FindEvent, TileOccupant,
ModuleFactory, ModuleRoster}`; `geode_shell::frame::{Frame,
FrameVersions}`; `Frame::{versions, scope, set_scope, clear_scope,
undo_scope, slots, active_slot, active_grouping, save_slot, as_of,
set_as_of, effective_scope, requery}`; `ShellView::{frame, deliver,
config}`; `ShellEvent::{ConfigReloaded, RestartRequired}`;
`KeyContext::counts`; `vimnav::{NavCommand, apply}`;
`vimfind::{FindStyle, find_match, filter_matches, FindDirection}`;
`perf::RequeryStats`; the `tile` context with `/` and `:`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-blotter/Cargo.toml`, `src/lib.rs` | Crate, `init` (reclaimed bindings), re-exports. |
| `src/core/plan.rs` | `ColumnPlan`, `PlannedColumn`, `ColumnKind`. |
| `src/core/expansion.rs` | `Path`, `Expansion`, `path_of`, depth bound. |
| `src/core/flatten.rs` | `SortSpec`, `flatten`. |
| `src/core/cursor.rs` | `Cursor`, `Mode`, motions, path restore. |
| `src/core/find.rs` | `FindState` under both styles, `n`/`N`. |
| `src/core/format.rs` | `format_number`, `Sign`. |
| `src/core/cache.rs` | `FormatCache`, `CachedCell`. |
| `src/core/yank.rs` | TSV. |
| `src/core/commands.rs` | `Command`, `parse`, completion vocabulary. |
| `src/delegate.rs` | `BlotterDelegate: TableDelegate`; the state the table renders. |
| `src/tile.rs` | `BlotterTile` entity: frame observation, requery, deliver, timing, header/footer, `Render`. |
| `src/content.rs` | `BlotterContent: TileContent`, `BlotterFactory: ModuleFactory`. |
| `crates/geode-blotter/benches/blotter.rs` | flatten, cache fill, plan build. |
| `crates/geode-app/src/bridge.rs` (new) | `DataHandle` + event drain into the shell; config → `DataServiceConfig`; reload forwarding. |
| `crates/geode-app/src/demo.rs` (new) | `--demo`: emit, config layer, paths. |
| `crates/geode-app/src/main.rs` | Roster, args, data dir. |
| `examples/demo-config/*.toml` | The demo layer (replaces `examples/probe-config`). |
| `crates/geode-shell/src/defaults.rs` | Blotter bindings (the actions are the blotter's). |
| deleted | `crates/geode-shell/src/dataprobe.rs`, `crates/geode-app/src/probe.rs`, `examples/probe-config/`. |

---

### Task 0: Split `shell/mod.rs` before any blotter work

Added 2026-09-05 from `docs/phase-3c-handoff.md`. `crates/geode-shell/src/shell/mod.rs`
is ~11,700 lines, ~7,880 of them one `mod tests`, and one `render` function of
~1,190 lines. The 3b final review found that its one Critical defect survived
four reviews partly because the command line's accept branch sat 1,400 lines
from the pure core it depended on. Tasks 7 and 8 add to this file (occupant
routing, `deliver`, `set_data_status`), so the split goes first.

**This task is a pure move.** No behaviour change, no renames, no signature
edits beyond `fn` → `pub(super) fn` where a moved method is called from
another file under `shell/`, and `use` lines that the compiler demands. Every
doc comment travels with its item. The second commit (Step 5) is the only
place code changes shape.

**Files: `mod.rs` keeps** the `pub mod` lines, the consts it still uses,
`ShellServices`, `ShellEvent`, the `ShellView` struct with its field docs,
`docs_equal`, `ShellView::new`, `close_modal`, `on_frame_changed`,
`save_slot`, `config`, `frame`, `set_probe`, `data_probe_visible`. Target
size ≈ 1,100 lines.

| New file | Moves out of `mod.rs` (non-test) |
|---|---|
| `shell/input.rs` | `context_stack`, `is_palette_toggle`, `handle_key_down`, `dispatch`, `persist_theme`, `persist_font_size`, `persist_find_style`, `mod_alias_held` if only these use it |
| `shell/palette_ctl.rs` | `toggle_palette`, `close_palette`, `sync_palette_scroll`, `handle_palette_key`, `dispatch_palette_item` |
| `shell/commandline_ctl.rs` | `open_command_line`, `close_command_line`, `cancel_command_line`, `on_command_line_changed`, `handle_command_line_key` (the Accept branch lives here) |
| `shell/occupants.rs` | `occupant_kind`, `deliver`, `fill_all_tiles`, `fill_active_tiles`, `ensure_occupants`, `current_tiles` |
| `shell/session_io.rs` | `take_dirty_session_write`, `save_session` |
| `shell/hot_reload.rs` | `apply_reload`, `RELOAD_POLL_INTERVAL` if `new`'s poll loop can reach it without a `pub` |
| `shell/drag.rs` | `DividerDragTarget`, `DividerDrag`, `StripSpec`, `TILE_DRAG_*` consts, `TileDrag`, and every `*_divider_drag` / `*_tile_drag` / `heal_drags_on_root_release` method |
| `shell/render.rs` | the whole `impl Render for ShellView` block and `DIVIDER_GROUP` |

`shell/keys.rs` already exists (the pure keystroke converter) and is not
touched; that is why the key path is `input.rs`.

**Tests** move to a directory module: `#[cfg(test)] mod tests;` in `mod.rs`,
`shell/tests/mod.rs` holding the shared helpers (`test_services`,
`services_with_recorder`, `open_shell`, `shell_of`) as `pub(super)`, and one
submodule per seam, in the order the tests already sit in the file:

| Test file | Tests (by the first and last name in the run) |
|---|---|
| `tests/commandline.rs` | `colon_opens_the_command_line…` … `clicking_the_filter_input_cancels_an_open_command_line` |
| `tests/occupants.rs` | `a_restored_tile_of_an_unknown_kind…` … `a_click_on_a_docked_tile_leaves_the_shell_focused…` |
| `tests/tiling_keys.rs` | `empty_workspace_paints_the_hint` … `ctrl_w_keystroke_closes_the_focused_tile`, plus `close_tile_focuses_adjacent_sibling` |
| `tests/drag.rs` | `dock_test_shell` helper … `switching_away_and_back_within_one_frame_voids_the_drop` (divider and tile drags, with their local helpers `alt_held`, `main_tile_point`, `dock_point`, `two_tile_drag_shell`) |
| `tests/dock.rs` | `a_pending_key_sequence_gates_the_divider_strips` … `a_visible_left_dock_carves_its_column_out_of_the_tree_area` |
| `tests/palette.rs` | `mod_shift_t_keystroke_toggles_the_theme_mode` … `enter_on_the_palette_toggle_row_closes_the_palette_without_reopening` (sequences, palette, rebinding; helpers `test_services_with_gg_binding`, `test_services_with_ctrl_k_rebound_to_split`) |
| `tests/reload.rs` | `config_with_mod` helper … `a_reloaded_groupings_doc_replaces_the_slots…` (includes `ctrl_digits_switch_the_frame_slot_and_ctrl_0_clears_it`) |
| `tests/session.rs` | `test_services_with_session` helper … `mod_shift_t_keystroke_persists_the_new_mode_to_the_user_config_file` |
| `tests/chrome_and_dialogs.rs` | `chrome_paints_quads_even_with_no_tiles_open` … `open_shell_dialog_closes_an_open_palette` (filter input, settings dialog, font size, find style) |
| `tests/keybindings_dialog.rs` | `keybindings_open_paints_the_modal_with_rows` … `click_selects_a_row_and_clicking_it_again_starts_listening` (helpers `dialog_test_shell`, `filter_is_focused`) |
| `tests/perf.rs` | `perf_overlay_toggles_via_the_bound_action`, `the_data_probe_paints_a_pushed_snapshot`, `render_records_frame_samples_and_reset_clears_them` |

A helper used by more than one test file goes to `tests/mod.rs`. If a test
is on the wrong side of a boundary by its content rather than its name,
move it to the file its subject lives in and say so in the report.

**Steps**

- [ ] Step 1: Record the baseline. `cargo test --workspace 2>&1 | grep 'test result'`
  and `cargo test -p geode-shell --lib 2>&1 | grep 'test result'`. Expected:
  1070 workspace, 715 `geode-shell` lib. Write both numbers in the report.
- [ ] Step 2: Move the non-test code, one file at a time, compiling after each
  (`cargo check -p geode-shell --all-targets`). Each new file opens with a
  `//!` doc saying what it owns and why it is separate. Order: `render.rs`,
  `drag.rs`, `input.rs`, `palette_ctl.rs`, `commandline_ctl.rs`,
  `occupants.rs`, `session_io.rs`, `hot_reload.rs`. Visibility: `pub(super)` for
  anything only `shell` needs; keep `pub` only on what was `pub` before.
- [ ] Step 3: Move the tests into `shell/tests/`. Run
  `cargo test -p geode-shell --lib 2>&1 | grep 'test result'` — the count is
  the Step 1 number exactly. Run `cargo test --workspace` — 1070.
- [ ] Step 4: Retarget the harness. `scripts/mutation-check.sh` has 13
  entries anchored on `crates/geode-shell/src/shell/mod.rs`; each anchor's
  path becomes the file its `grep` line moved to. Run
  `zsh scripts/mutation-check.sh --changed` and paste the result lines in
  the report: every entry `caught`, none `caught*`, `FILTER` or `SURVIVED`.
  (Moving code detaches anchors silently — this happened in Phase 3a Task 9.)
  Run `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
  Commit: `refactor(shell): split shell/mod.rs by seam (pure move)`.
- [ ] Step 5: Second commit, the shape changes the handoff assigns to this
  task. Each is small and self-contained:
  - the slot-rebuild block duplicated between `ShellView::new` and
    `apply_reload` becomes one `fn` (in `hot_reload.rs`, called from both);
    the three `reload:` harness entries anchored on `apply_reload` must still
    be `caught`;
  - M3: drop the unreachable `previous == self.scope` guard in
    `Frame::undo_scope` (`frame.rs`), or replace it with a comment saying
    why it cannot fire — read the code first, the review may be wrong;
  - M4: `commandline::word_at` clamps a non-char-boundary cursor to a
    boundary, with a test using a multi-byte character;
  - M6: `chrono` moves to `[dev-dependencies]` in `geode-shell` if no
    non-test code uses it (`grep -rn chrono crates/geode-shell/src`);
  - M8: `restart_required` — decide whether it should clear when a later
    reload restores the original `sources`/`datasets`; if yes implement
    with a test, if no add a doc comment saying it is sticky by design;
  - M9: `theme::write_atomic`'s temp-file name derives from the target
    file's name instead of the hardcoded `.app.toml.*`;
  - M10: a comment above the which-key count row naming the reading of
    spec §3.3 that was chosen;
  - M15: a doc comment in `session.rs` on the restored-unknown-kind
    rewrite;
  - spec corrections in `docs/superpowers/specs/2026-08-28-geode-foundation-design.md`:
    §3.3 dispatch order (workspace arms run before the shell's own);
    §4.2 records that under `keymap.mod = "ctrl"` the shipped
    `workspace::switch_N` bindings win `ctrl+1..9` and slots are set via
    the palette; §4.5 says `ShellEvent::ConfigReloaded` fires for views
    and dimensions only.
  Run the same four checks plus `zsh scripts/mutation-check.sh --changed`.
  Commit: `refactor(shell): deferred 3b cleanups (M3 M4 M6 M8 M9 M10 M15, slot rebuild DRY)`.
- [ ] Step 6: Report: the two test counts before and after, the harness
  output, the line count of every file under `shell/` after the split, and
  any test that moved to a file other than the one the table names.

---

### Task 1: Crate skeleton and `ColumnPlan`

Spec §6.1 "Column plan". Every column resolved once per snapshot; the
tree column folds the grouping's dimension columns.

**Files:**
- Create: `crates/geode-blotter/Cargo.toml`, `src/lib.rs`, `src/core/mod.rs`,
  `src/core/plan.rs`
- Modify: `Cargo.toml` (workspace members)
- Test: `src/core/plan.rs` inline

**Interfaces:**
- Produces:
  ```rust
  pub enum ColumnKind { Tree, Measure, Dimension }
  pub struct PlannedColumn { pub name: String, pub label: String, pub index: Option<usize>,
      pub kind: ColumnKind, pub format: ColumnFormat, pub width: f32,
      pub attribution: Vec<Attribution>, pub semi_joined: Vec<String> }
  pub struct ColumnPlan { pub columns: Vec<PlannedColumn>, pub grouping: Vec<String>, pub grouping_indices: Vec<Option<usize>> }
  impl ColumnPlan {
      pub fn build(view: &ViewSpec, grouping: &[String], snapshot: &Snapshot) -> ColumnPlan;
      pub fn tree_text<'a>(&self, snapshot: &'a Snapshot, row: usize) -> Option<&'a str>;
      pub fn attribution(&self, col: usize, depth: usize) -> Attribution;
      pub fn move_column(&mut self, from: usize, to: usize);
      pub fn same_columns(&self, snapshot: &Snapshot) -> bool;
  }
  pub const TREE_WIDTH: f32 = 260.0; pub const MEASURE_WIDTH: f32 = 110.0; pub const TEXT_WIDTH: f32 = 140.0;
  ```

- [ ] **Step 1: Crate files**

`crates/geode-blotter/Cargo.toml`:

```toml
[package]
name = "geode-blotter"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

[dependencies]
geode-core.workspace = true
geode-shell.workspace = true
geode-data = { path = "../geode-data" }
gpui.workspace = true
gpui-component.workspace = true
chrono = "0.4.42"
toml = "1.1.4"

[dev-dependencies]
criterion = "0.8.2"
geode-core = { workspace = true, features = ["test-support"] }
geode-shell = { workspace = true, features = ["test-support"] }
geode-data = { path = "../geode-data", features = ["test-support"] }
gpui = { workspace = true, features = ["test-support"] }

[[bench]]
name = "blotter"
harness = false
```

Add `"crates/geode-blotter"` to the workspace `members`. `src/lib.rs`:

```rust
//! The blotter (foundation §9.2, Phase 3 spec §6): any view definition
//! as a collapsible, keyboard-driven hierarchy with honest markers. The
//! pure core in `core` has no `gpui`; `delegate` adapts it to
//! gpui-component's `DataTable`; `tile` is the entity per tile;
//! `content` is what the shell hosts.

pub mod content;
pub mod core;
pub mod delegate;
pub mod tile;

pub use content::BlotterFactory;

/// Reclaim `DataTable`'s own key bindings (Phase 3 §3.3): the blotter
/// never gives the table focus, but a row click moves gpui focus there
/// for one frame, and these must not act during it. Same door and same
/// reasoning as `geode_shell::shell::dialog::init_reclaimed_keybindings`.
pub fn init(cx: &mut gpui::App) {
    const CONTEXT: Option<&str> = Some("DataTable");
    cx.bind_keys(
        [
            "escape", "up", "down", "left", "right", "home", "end", "pageup", "pagedown", "tab",
            "shift-tab",
        ]
        .into_iter()
        .map(|key| gpui::KeyBinding::new(key, gpui::NoAction, CONTEXT)),
    );
}
```

`src/core/mod.rs` lists `plan`, `expansion`, `flatten`, `cursor`,
`find`, `format`, `cache`, `yank`, `commands` — add each as its task
creates it; `content`, `delegate`, `tile` are stubbed as empty modules
until Tasks 6–7. Create `benches/blotter.rs` as an empty criterion main
(`criterion_group!(benches,); criterion_main!(benches);` — fill in Task
9) so `cargo bench --no-run` passes from the first commit.

- [ ] **Step 2: Write the failing tests**

`src/core/plan.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    fn view() -> ViewSpec {
        let text = r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref"]
[[tree.columns]]
name = "lhu"
kind = "dimension"
[[tree.columns]]
name = "model_code"
kind = "dimension"
[[tree.columns]]
name = "delta01"
format = { precision = 0, scale = "k" }
label = "Δ"
width = 90
[[tree.columns]]
name = "daily_trading_pnl"
[[tree.columns]]
name = "missing_in_snapshot"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }

    fn meta(name: &str, by_depth: Vec<Attribution>, semantics: ScopeSemantics) -> ColumnMeta {
        ColumnMeta { name: name.into(), attribution_by_depth: by_depth, scope_semantics: semantics }
    }

    fn snapshot() -> Snapshot {
        Snapshot::for_tests(
            vec![
                (meta("lhu", vec![Attribution::Additive; 3], ScopeSemantics::Direct), TestColumn::Str(vec![None, Some("L1"), Some("L1")])),
                (meta("underlying_ref", vec![Attribution::Additive; 3], ScopeSemantics::Direct), TestColumn::Str(vec![None, None, Some("SPX")])),
                (meta("row_depth", vec![Attribution::Additive; 3], ScopeSemantics::Direct), TestColumn::I32(vec![0, 1, 2])),
                (meta("delta01", vec![Attribution::Additive; 3], ScopeSemantics::Direct), TestColumn::F64(vec![Some(1.0), Some(1.0), Some(1.0)])),
                (
                    meta(
                        "daily_trading_pnl",
                        vec![Attribution::Additive, Attribution::Additive, Attribution::NonAttributable],
                        ScopeSemantics::SemiJoined { dimensions: vec!["underlying_ref".into()] },
                    ),
                    TestColumn::F64(vec![Some(7.0), Some(7.0), None]),
                ),
                (meta("model_code", vec![Attribution::Additive; 3], ScopeSemantics::Direct), TestColumn::Str(vec![None, None, Some("EURP")])),
            ],
            2,
        )
    }

    #[test]
    fn the_tree_column_leads_and_grouping_dimensions_fold_into_it() {
        let plan = ColumnPlan::build(&view(), &["lhu".into(), "underlying_ref".into()], &snapshot());
        let names: Vec<&str> = plan.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["", "model_code", "delta01", "daily_trading_pnl", "missing_in_snapshot"]);
        assert_eq!(plan.columns[0].kind, ColumnKind::Tree);
        assert_eq!(plan.columns[0].label, "lhu / underlying_ref");
        assert_eq!(plan.columns[0].width, TREE_WIDTH);
        assert_eq!(plan.columns[1].kind, ColumnKind::Dimension);
        assert_eq!(plan.columns[1].width, TEXT_WIDTH);
        assert_eq!(plan.grouping_indices, vec![Some(0), Some(1)]);
    }

    #[test]
    fn every_column_is_resolved_once_and_an_absent_one_is_none_not_a_panic() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        assert_eq!(plan.columns[2].index, Some(3));
        assert_eq!(plan.columns[4].index, None);
        assert!(plan.same_columns(&snap));
    }

    #[test]
    fn presentation_and_kind_defaults_are_applied() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let d = &plan.columns[2];
        assert_eq!(d.label, "Δ (k)", "the scale suffix follows the label");
        assert_eq!(d.width, 90.0);
        assert_eq!(d.format.precision, 0);
        assert_eq!(d.format.scale, geode_core::view::Scale::Thousands);
        assert!(d.format.thousands, "the measure default survives a partial override");
        let p = &plan.columns[3];
        assert_eq!(p.label, "daily_trading_pnl");
        assert_eq!(p.width, MEASURE_WIDTH);
        assert_eq!(p.format, geode_core::view::ColumnFormat::MEASURE);
    }

    #[test]
    fn attribution_is_per_column_per_depth_and_semi_joined_dimensions_are_named() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        assert_eq!(plan.attribution(3, 2), Attribution::NonAttributable);
        assert_eq!(plan.attribution(3, 1), Attribution::Additive);
        assert_eq!(plan.attribution(4, 0), Attribution::Additive, "an absent column is treated as additive");
        assert_eq!(plan.attribution(3, 9), Attribution::Additive, "past the declared depths, additive");
        assert_eq!(plan.columns[3].semi_joined, vec!["underlying_ref".to_string()]);
        assert!(plan.columns[2].semi_joined.is_empty());
    }

    #[test]
    fn tree_text_is_the_rows_own_level() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        assert_eq!(plan.tree_text(&snap, 0), None, "the grand total has no level of its own");
        assert_eq!(plan.tree_text(&snap, 1), Some("L1"));
        assert_eq!(plan.tree_text(&snap, 2), Some("SPX"));
    }

    #[test]
    fn move_column_reorders_but_never_moves_the_tree_column() {
        let snap = snapshot();
        let mut plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        plan.move_column(3, 1);
        let names: Vec<&str> = plan.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["", "daily_trading_pnl", "model_code", "delta01", "missing_in_snapshot"]);
        plan.move_column(0, 2);
        assert_eq!(plan.columns[0].kind, ColumnKind::Tree, "the tree column stays first");
        plan.move_column(2, 0);
        assert_eq!(plan.columns[0].kind, ColumnKind::Tree);
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p geode-blotter plan:: 2>&1 | tail -3`

- [ ] **Step 4: Implement**

```rust
//! The column plan (Phase 3 spec §6.1): every view column resolved to a
//! snapshot index once, with its kind, format, width and per-depth
//! attribution. Built when a snapshot arrives whose column set differs
//! from the last; never touched per cell. The probe's five name searches
//! per cell are what this replaces.

use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::groupings::GroupingSlots;
use geode_core::snapshot::Snapshot;
use geode_core::view::{ColumnFormat, ViewColumn, ViewSpec};

pub const TREE_WIDTH: f32 = 260.0;
pub const MEASURE_WIDTH: f32 = 110.0;
pub const TEXT_WIDTH: f32 = 140.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// The grouping value for the row's own depth, indented, with a
    /// disclosure glyph. Always first; never moved.
    Tree,
    Measure,
    Dimension,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedColumn {
    /// The snapshot column name; empty for the tree column.
    pub name: String,
    pub label: String,
    /// The snapshot column index, or `None` when the snapshot lacks it —
    /// such a cell paints blank and never panics.
    pub index: Option<usize>,
    pub kind: ColumnKind,
    pub format: ColumnFormat,
    pub width: f32,
    /// Indexed by row depth (§6.5).
    pub attribution: Vec<Attribution>,
    /// Dimensions applied by membership rather than directly.
    pub semi_joined: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnPlan {
    pub columns: Vec<PlannedColumn>,
    pub grouping: Vec<String>,
    /// Snapshot index of each grouping column, for `tree_text`.
    pub grouping_indices: Vec<Option<usize>>,
}

impl ColumnPlan {
    pub fn build(view: &ViewSpec, grouping: &[String], snapshot: &Snapshot) -> ColumnPlan {
        let grouping_indices: Vec<Option<usize>> =
            grouping.iter().map(|g| snapshot.column_index(g)).collect();
        let mut columns = vec![PlannedColumn {
            name: String::new(),
            label: GroupingSlots::label_of(grouping),
            index: None,
            kind: ColumnKind::Tree,
            format: ColumnFormat::TEXT,
            width: TREE_WIDTH,
            attribution: Vec::new(),
            semi_joined: Vec::new(),
        }];
        for column in &view.columns {
            let name = column.name();
            let kind = match column {
                ViewColumn::Dimension { .. } => {
                    if grouping.iter().any(|g| g == name) {
                        continue; // folded into the tree column
                    }
                    ColumnKind::Dimension
                }
                ViewColumn::Measure { .. } | ViewColumn::Derived { .. } => ColumnKind::Measure,
            };
            let presentation = view.presentation_of(name);
            let format = match kind {
                ColumnKind::Measure => ColumnFormat::MEASURE,
                _ => ColumnFormat::TEXT,
            }
            .with(&presentation);
            let base_label = presentation.label.clone().unwrap_or_else(|| name.to_string());
            let label = if format.scale.suffix().is_empty() {
                base_label
            } else {
                format!("{base_label} ({})", format.scale.suffix())
            };
            let width = presentation.width.unwrap_or(match kind {
                ColumnKind::Measure => MEASURE_WIDTH,
                _ => TEXT_WIDTH,
            });
            let index = snapshot.column_index(name);
            let meta = index.and_then(|i| snapshot.meta_at(i));
            let attribution = meta.map(|m| m.attribution_by_depth.clone()).unwrap_or_default();
            let semi_joined = match meta.map(|m| &m.scope_semantics) {
                Some(ScopeSemantics::SemiJoined { dimensions }) => dimensions.clone(),
                _ => Vec::new(),
            };
            columns.push(PlannedColumn {
                name: name.to_string(),
                label,
                index,
                kind,
                format,
                width,
                attribution,
                semi_joined,
            });
        }
        ColumnPlan {
            columns,
            grouping: grouping.to_vec(),
            grouping_indices,
        }
    }

    /// The row's own level: `grouping[depth - 1]` at that row. `None` for
    /// the grand total.
    pub fn tree_text<'a>(&self, snapshot: &'a Snapshot, row: usize) -> Option<&'a str> {
        let depth = snapshot.tree().depth(row);
        let col = (*self.grouping_indices.get(depth.checked_sub(1)?)?)?;
        snapshot.text_at(col, row)
    }

    /// The marker for a cell (§6.5). Absent columns and depths past what
    /// the compiler declared are `Additive`, which paints the value plain
    /// — the compiler blanks a `NonAttributable` cell itself, so this can
    /// only ever err towards showing a number that is really there.
    pub fn attribution(&self, col: usize, depth: usize) -> Attribution {
        self.columns
            .get(col)
            .and_then(|c| c.attribution.get(depth).copied())
            .unwrap_or(Attribution::Additive)
    }

    /// Reorder a column; the tree column stays first whatever is asked.
    pub fn move_column(&mut self, from: usize, to: usize) {
        if from == 0 || to == 0 || from >= self.columns.len() || to >= self.columns.len() {
            return;
        }
        let column = self.columns.remove(from);
        self.columns.insert(to, column);
    }

    /// Whether this plan still describes `snapshot`'s columns.
    pub fn same_columns(&self, snapshot: &Snapshot) -> bool {
        self.columns.iter().all(|c| match c.kind {
            ColumnKind::Tree => true,
            _ => snapshot.column_index(&c.name) == c.index,
        }) && self
            .grouping
            .iter()
            .zip(&self.grouping_indices)
            .all(|(g, i)| snapshot.column_index(g) == *i)
    }
}
```

- [ ] **Step 5: Run, check, commit**

Run: `cargo test -p geode-blotter && cargo bench -p geode-blotter --no-run`

```bash
git add Cargo.toml Cargo.lock crates/geode-blotter
git commit -m "feat(blotter): the crate, DataTable key reclaim, and ColumnPlan

Every view column resolved to a snapshot index once, with kind, format,
width and per-depth attribution; grouping dimensions fold into a leading
tree column (Phase 3 §6.1).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

- [ ] **Step 6: Harness entry** (package `geode-blotter`)

```sh
# ---- blotter core (Phase 3 §6)

run_mutation "plan: attribution is per depth" \
  crates/geode-blotter/src/core/plan.rs \
  '            .and_then(|c| c.attribution.get(depth).copied())' \
  '            .and_then(|c| c.attribution.first().copied())' \
  geode-blotter
```

Run: `zsh scripts/mutation-check.sh "plan:"` — `caught`. Commit.

---

### Task 2: Expansion by path, flatten, and the depth bound

Spec §6.1 "Expansion", "Flatten", "Depth bound"; §6.3 sibling sort.

**Files:**
- Create: `src/core/expansion.rs`, `src/core/flatten.rs`
- Test: inline

**Interfaces:**
- Produces:
  ```rust
  pub type Path = Vec<Option<String>>;   // grouping values root → node; None is NULL
  pub struct Expansion { .. }             // Default
  impl Expansion {
      pub fn is_open(&self, path: &[Option<String>]) -> bool;
      pub fn open(&mut self, path: Path) -> bool;  pub fn close(&mut self, path: &[Option<String>]) -> bool;
      pub fn toggle(&mut self, path: Path) -> bool;
      pub fn open_all(&mut self);  pub fn close_all(&mut self);
      pub fn deepest_open_depth(&self) -> usize;
      pub fn prune_to(&mut self, grouping_len: usize);
  }
  pub fn path_of(snapshot: &Snapshot, plan: &ColumnPlan, row: usize) -> Path;
  pub fn depth_bound(expansion: &Expansion, grouping_len: usize) -> usize;   // min(len, deepest + 1)
  pub struct SortSpec { pub column: usize, pub descending: bool }
  pub fn flatten(snapshot: &Snapshot, plan: &ColumnPlan, expansion: &Expansion, sort: Option<&SortSpec>, out: &mut Vec<u32>);
  ```

- [ ] **Step 1: Write the failing tests**

`expansion.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn p(parts: &[Option<&str>]) -> Path {
        parts.iter().map(|s| s.map(str::to_string)).collect()
    }

    #[test]
    fn open_close_toggle_and_deepest() {
        let mut e = Expansion::default();
        assert_eq!(e.deepest_open_depth(), 0);
        assert!(e.open(p(&[Some("L1")])));
        assert!(!e.open(p(&[Some("L1")])), "already open");
        assert!(e.is_open(&p(&[Some("L1")])));
        assert!(e.toggle(p(&[Some("L1"), Some("SPX")])));
        assert_eq!(e.deepest_open_depth(), 2);
        assert!(!e.toggle(p(&[Some("L1"), Some("SPX")])));
        assert!(!e.is_open(&p(&[Some("L1"), Some("SPX")])));
        assert!(e.close(&p(&[Some("L1")])));
        assert!(!e.close(&p(&[Some("L1")])));
        assert_eq!(e.deepest_open_depth(), 0);
    }

    #[test]
    fn a_null_and_an_empty_string_are_different_paths() {
        let mut e = Expansion::default();
        e.open(p(&[None]));
        assert!(!e.is_open(&p(&[Some("")])));
    }

    #[test]
    fn open_all_opens_every_node_until_close_all() {
        let mut e = Expansion::default();
        e.open_all();
        assert!(e.is_open(&p(&[Some("anything"), Some("at"), Some("all")])));
        assert_eq!(e.deepest_open_depth(), usize::MAX);
        e.close_all();
        assert!(!e.is_open(&p(&[Some("anything")])));
        assert_eq!(e.deepest_open_depth(), 0);
    }

    #[test]
    fn the_depth_bound_is_one_past_the_deepest_open_node_capped_at_the_grouping() {
        let mut e = Expansion::default();
        assert_eq!(depth_bound(&e, 3), 1, "collapsed: the first level only");
        e.open(p(&[Some("L1")]));
        assert_eq!(depth_bound(&e, 3), 2);
        e.open(p(&[Some("L1"), Some("SPX")]));
        assert_eq!(depth_bound(&e, 3), 3);
        e.open_all();
        assert_eq!(depth_bound(&e, 3), 3, "never past the grouping");
        assert_eq!(depth_bound(&Expansion::default(), 0), 0, "a flat view");
    }

    #[test]
    fn regrouping_prunes_paths_deeper_than_the_new_grouping() {
        let mut e = Expansion::default();
        e.open(p(&[Some("L1")]));
        e.open(p(&[Some("L1"), Some("SPX")]));
        e.prune_to(1);
        assert!(e.is_open(&p(&[Some("L1")])));
        assert!(!e.is_open(&p(&[Some("L1"), Some("SPX")])));
    }
}
```

`flatten.rs` (uses `plan::tests`-style fixtures; write its own):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::expansion::{Expansion, path_of};
    use crate::core::plan::ColumnPlan;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta { name: name.into(), attribution_by_depth: vec![Attribution::Additive; 4], scope_semantics: ScopeSemantics::Direct }
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// Root; L1, L2; L1/SPX, L2/SPX, L1/NDX, L2/NDX; L1/SPX/P1, L1/SPX/P2.
    /// Rows 3 and 5 are L1's children, interleaved with L2's.
    fn snapshot() -> Snapshot {
        Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1"), s("L2"), s("L1"), s("L2"), s("L1"), s("L2"), s("L1"), s("L1")])),
                (dim("underlying_ref"), TestColumn::Dict(vec![None, None, None, s("SPX"), s("SPX"), s("NDX"), s("NDX"), s("SPX"), s("SPX")])),
                (dim("position_ref"), TestColumn::Str(vec![None, None, None, None, None, None, None, Some("P1"), Some("P2")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2, 2, 2, 2, 3, 3])),
                (dim("delta01"), TestColumn::F64(vec![Some(100.0), Some(60.0), Some(40.0), Some(10.0), Some(30.0), Some(50.0), Some(10.0), None, Some(4.0)])),
            ],
            3,
        )
    }

    fn view() -> ViewSpec {
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\", \"position_ref\"]\n[[t.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }

    fn visible(expansion: &Expansion, sort: Option<&SortSpec>) -> Vec<u32> {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut out = Vec::new();
        flatten(&snap, &plan, expansion, sort, &mut out);
        out
    }

    #[test]
    fn collapsed_shows_the_root_and_its_children_only_when_the_root_is_open() {
        // The grand total is always open: a blotter that shows one row is
        // not a blotter. Its children are the first level.
        assert_eq!(visible(&Expansion::default(), None), vec![0, 1, 2]);
    }

    #[test]
    fn opening_a_node_shows_its_children_in_row_order_and_descends_only_into_open_nodes() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut e = Expansion::default();
        e.open(path_of(&snap, &plan, 1));
        assert_eq!(visible(&e, None), vec![0, 1, 3, 5, 2]);
        e.open(path_of(&snap, &plan, 3));
        assert_eq!(visible(&e, None), vec![0, 1, 3, 7, 8, 5, 2]);
        e.open_all();
        assert_eq!(visible(&e, None), vec![0, 1, 3, 7, 8, 5, 2, 4, 6]);
    }

    #[test]
    fn a_sort_orders_siblings_within_their_parent_with_null_last() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut e = Expansion::default();
        e.open_all();
        let delta = SortSpec { column: 1, descending: true };
        assert_eq!(visible(&e, Some(&delta)), vec![0, 1, 5, 3, 8, 7, 2, 4, 6], "L1 (60) before L2 (40); NDX 50 before SPX 10; P2 4 before P1 NULL");
        let asc = SortSpec { column: 1, descending: false };
        assert_eq!(visible(&e, Some(&asc)), vec![0, 2, 6, 4, 1, 3, 8, 7, 5], "ascending, NULL still last");
    }

    #[test]
    fn expansion_survives_a_snapshot_that_reorders_siblings() {
        // The same tree with L2 before L1 at depth 1: the open path still
        // names L1 and L1 still opens.
        let reordered = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L2"), s("L1"), s("L1")])),
                (dim("underlying_ref"), TestColumn::Dict(vec![None, None, None, s("SPX")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2])),
            ],
            2,
        );
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut e = Expansion::default();
        e.open(path_of(&snap, &plan, 1)); // L1
        let plan2 = ColumnPlan::build(&view(), reordered.grouping(), &reordered);
        let mut out = Vec::new();
        flatten(&reordered, &plan2, &e, None, &mut out);
        assert_eq!(out, vec![0, 1, 2, 3]);
    }

    #[test]
    fn the_output_buffer_is_reused() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut out = Vec::with_capacity(64);
        let ptr = out.as_ptr();
        flatten(&snap, &plan, &Expansion::default(), None, &mut out);
        assert_eq!(out.as_ptr(), ptr, "no reallocation for a small tree");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement**

`expansion.rs`:

```rust
//! Which nodes are open (Phase 3 spec §6.1): a set of *paths* — the
//! grouping values from the root to the node — never row indices, so
//! expansion survives a requery, a regroup, and a snapshot that
//! reorders siblings. Built at keypress time; never touched per frame.

use crate::core::plan::ColumnPlan;
use geode_core::snapshot::Snapshot;
use std::collections::HashSet;

/// Grouping values root → node. `None` is NULL, which is its own value
/// (a blanked ENUM, P2 §3.6), distinct from the empty string.
pub type Path = Vec<Option<String>>;

#[derive(Debug, Default, Clone)]
pub struct Expansion {
    open: HashSet<Path>,
    /// `zR`: every materialised node is open until `close_all`.
    all: bool,
}

impl Expansion {
    pub fn is_open(&self, path: &[Option<String>]) -> bool {
        self.all || self.open.contains(path)
    }

    pub fn open(&mut self, path: Path) -> bool {
        !self.all && self.open.insert(path)
    }

    pub fn close(&mut self, path: &[Option<String>]) -> bool {
        if self.all {
            // Closing one node under "all open" means: everything else
            // stays open, this one closes. Materialise the set lazily is
            // not possible without the tree, so `all` becomes a plain
            // set of nothing and the caller re-opens what it needs — in
            // practice `zM` then `zo` is what users do. Keep it simple:
            self.all = false;
            self.open.clear();
            return true;
        }
        self.open.remove(path)
    }

    /// `true` when the node is open afterwards.
    pub fn toggle(&mut self, path: Path) -> bool {
        if self.is_open(&path) {
            self.close(&path);
            false
        } else {
            self.open(path);
            true
        }
    }

    pub fn open_all(&mut self) {
        self.all = true;
    }

    pub fn close_all(&mut self) {
        self.all = false;
        self.open.clear();
    }

    /// The deepest open node's depth; `usize::MAX` under `open_all`.
    pub fn deepest_open_depth(&self) -> usize {
        if self.all {
            return usize::MAX;
        }
        self.open.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// After a regroup, paths deeper than the new grouping cannot exist.
    pub fn prune_to(&mut self, grouping_len: usize) {
        self.open.retain(|p| p.len() <= grouping_len);
    }
}

/// The path of `row`: its ancestors' tree texts, root excluded.
pub fn path_of(snapshot: &Snapshot, plan: &ColumnPlan, row: usize) -> Path {
    let tree = snapshot.tree();
    let depth = tree.depth(row);
    let mut path: Path = vec![None; depth];
    let mut at = Some(row);
    let mut d = depth;
    while let (Some(r), true) = (at, d > 0) {
        path[d - 1] = plan.tree_text(snapshot, r).map(str::to_string);
        at = tree.parent(r);
        d -= 1;
    }
    path
}

/// One more than the deepest open node, so a single expand is already in
/// hand; never past the grouping (§6.1, `docs/perf.md`).
pub fn depth_bound(expansion: &Expansion, grouping_len: usize) -> usize {
    expansion
        .deepest_open_depth()
        .saturating_add(1)
        .min(grouping_len)
}
```

`flatten.rs`:

```rust
//! The visible-row list (Phase 3 spec §6.1, §6.3): a DFS over the tree
//! index that descends only into open nodes, so its cost tracks the
//! output, not the materialised rows. Runs on snapshot arrival, expand,
//! collapse and sort — never per frame. Siblings are sorted here when a
//! sort is set; view-shaping in-app (PHILOSOPHY §1) that leaves the
//! compiler's order untouched.

use crate::core::expansion::{Expansion, Path};
use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::snapshot::Snapshot;
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    /// Index into `ColumnPlan::columns`.
    pub column: usize,
    pub descending: bool,
}

pub fn flatten(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    expansion: &Expansion,
    sort: Option<&SortSpec>,
    out: &mut Vec<u32>,
) {
    out.clear();
    let tree = snapshot.tree();
    let mut path: Path = Vec::new();
    let mut scratch: Vec<u32> = Vec::new();
    // The root is always open: a blotter showing one row is not a
    // blotter. Several roots (a flat result) are all listed.
    for &root in tree.roots() {
        out.push(root);
        if tree.depth(root as usize) == 0 {
            descend(snapshot, plan, expansion, sort, root as usize, &mut path, &mut scratch, out);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn descend(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    expansion: &Expansion,
    sort: Option<&SortSpec>,
    node: usize,
    path: &mut Path,
    scratch: &mut Vec<u32>,
    out: &mut Vec<u32>,
) {
    let tree = snapshot.tree();
    let children = tree.children(node);
    if children.is_empty() {
        return;
    }
    let ordered: &[u32] = match sort {
        None => children,
        Some(spec) => {
            scratch.clear();
            scratch.extend_from_slice(children);
            sort_siblings(snapshot, plan, spec, scratch);
            scratch
        }
    };
    // The ordered slice must outlive recursion, which reuses `scratch`.
    let ordered: Vec<u32> = if sort.is_some() { ordered.to_vec() } else { Vec::new() };
    let iter: Box<dyn Iterator<Item = u32>> = if sort.is_some() {
        Box::new(ordered.into_iter())
    } else {
        Box::new(children.iter().copied())
    };
    for child in iter {
        out.push(child);
        let c = child as usize;
        if !tree.has_children(c) {
            continue;
        }
        path.push(plan.tree_text(snapshot, c).map(str::to_string));
        if expansion.is_open(path) {
            descend(snapshot, plan, expansion, sort, c, path, scratch, out);
        }
        path.pop();
    }
}

fn sort_siblings(snapshot: &Snapshot, plan: &ColumnPlan, spec: &SortSpec, rows: &mut [u32]) {
    let Some(column) = plan.columns.get(spec.column) else {
        return;
    };
    let idx = column.index;
    let numeric = column.kind == ColumnKind::Measure;
    rows.sort_by(|a, b| {
        let (a, b) = (*a as usize, *b as usize);
        let ord = match idx {
            None => Ordering::Equal,
            Some(i) if numeric => match (snapshot.f64_at(i, a), snapshot.f64_at(i, b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            Some(i) => match (snapshot.text_at(i, a), snapshot.text_at(i, b)) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        };
        // NULL last in both directions; ties keep row order (stable sort).
        match (spec.descending, ord) {
            (_, Ordering::Equal) => Ordering::Equal,
            (true, o) if is_null(snapshot, idx, numeric, a) || is_null(snapshot, idx, numeric, b) => o,
            (true, o) => o.reverse(),
            (false, o) => o,
        }
    });
}

fn is_null(snapshot: &Snapshot, idx: Option<usize>, numeric: bool, row: usize) -> bool {
    match idx {
        None => true,
        Some(i) if numeric => snapshot.f64_at(i, row).is_none(),
        Some(i) => snapshot.text_at(i, row).is_none(),
    }
}
```

The `descend` body above allocates a `Vec` per sorted sibling set; the
sorted case runs at keypress time and that is acceptable, but tidy it:
sort into `scratch`, copy into a local `Vec<u32>` only when `sort` is
set, and iterate `children` directly otherwise (as written). Delete the
`Box<dyn Iterator>` once it compiles cleanly under clippy — a plain
`let ordered: Vec<u32>` plus `for child in ordered.iter().copied()` in
one branch and `for child in children.iter().copied()` in the other,
sharing a small `visit` closure, reads better; either is correct.

- [ ] **Step 4: Run, check, commit**

```bash
git add crates/geode-blotter
git commit -m "feat(blotter): expansion by path, flatten, the depth bound

Open nodes are paths, so expansion survives requery and regroup; flatten
descends only into open nodes and sorts siblings in place with NULL
last; the depth bound is one past the deepest open node (Phase 3 §6.1,
§6.3).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

- [ ] **Step 5: Harness entries** (package `geode-blotter`)

```sh
run_mutation "flatten: only open nodes are descended into" \
  crates/geode-blotter/src/core/flatten.rs \
  '        if expansion.is_open(path) {' \
  '        if true {' \
  geode-blotter

run_mutation "flatten: NULL sorts last in both directions" \
  crates/geode-blotter/src/core/flatten.rs \
  '            (true, o) if is_null(snapshot, idx, numeric, a) || is_null(snapshot, idx, numeric, b) => o,' \
  '' \
  geode-blotter

run_mutation "expansion: the depth bound is one past the deepest open node" \
  crates/geode-blotter/src/core/expansion.rs \
  '        .saturating_add(1)' \
  '        .saturating_add(2)' \
  geode-blotter
```

Run: `zsh scripts/mutation-check.sh "flatten:"` and `"expansion:"`. Commit.

---

### Task 3: Cursor, modes, and find

Spec §6.1 "Cursor", "Visual mode", "Find"; §2.2.

**Files:**
- Create: `src/core/cursor.rs`, `src/core/find.rs`
- Test: inline

**Interfaces:**
- Produces:
  ```rust
  pub struct Cursor { pub row: usize, pub col: usize }
  pub enum Mode { Normal, Visual { anchor: usize } }
  impl Cursor {
      pub fn move_rows(&mut self, len: usize, cmd: NavCommand, count: Option<u32>);
      pub fn move_cols(&mut self, cols: usize, delta: i64, count: Option<u32>);
      pub fn to_row(&mut self, row: usize, len: usize);  pub fn clamp(&mut self, len: usize, cols: usize);
  }
  pub fn selection(mode: &Mode, cursor: &Cursor) -> std::ops::Range<usize>;   // inclusive of both ends, as a Range
  pub fn restore_by_path(visible: &[u32], snapshot: &Snapshot, plan: &ColumnPlan, path: &[Option<String>], fallback: usize) -> usize;
  pub struct FindState { pub style: FindStyle, pub origin: usize, pub committed: Option<String>, pub narrowed: Option<Vec<usize>> }
  impl FindState {
      pub fn begin(style: FindStyle, cursor_row: usize) -> FindState;
      pub fn changed(&mut self, texts: &[String], query: &str) -> Option<usize>;     // new cursor row (vim), or index into narrowed (fzf)
      pub fn committed(&mut self, query: &str);
      pub fn cancelled(&mut self) -> usize;                                       // the origin
      pub fn repeat(&self, texts: &[String], from: usize, dir: FindDirection, count: Option<u32>) -> Option<usize>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

`cursor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::vimnav::NavCommand;

    #[test]
    fn row_motion_is_counted_and_clamped() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_rows(10, NavCommand::Move(1), Some(5));
        assert_eq!(c.row, 5);
        c.move_rows(10, NavCommand::Move(1), Some(50));
        assert_eq!(c.row, 9, "clamped, no wrap");
        c.move_rows(10, NavCommand::Top, None);
        assert_eq!(c.row, 0);
        c.move_rows(10, NavCommand::Bottom, Some(3));
        assert_eq!(c.row, 2, "a counted G goes to that row (1-based)");
        c.move_rows(10, NavCommand::Bottom, None);
        assert_eq!(c.row, 9);
        c.move_rows(0, NavCommand::Move(1), None);
        assert_eq!(c.row, 0, "empty list");
    }

    #[test]
    fn column_motion_is_counted_and_clamped() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_cols(5, 1, Some(3));
        assert_eq!(c.col, 3);
        c.move_cols(5, 1, Some(9));
        assert_eq!(c.col, 4);
        c.move_cols(5, -1, None);
        assert_eq!(c.col, 3);
        c.clamp(1, 2);
        assert_eq!((c.row, c.col), (0, 1));
    }

    #[test]
    fn a_visual_selection_spans_anchor_to_cursor_either_way() {
        let c = Cursor { row: 2, col: 0 };
        assert_eq!(selection(&Mode::Visual { anchor: 5 }, &c), 2..6);
        assert_eq!(selection(&Mode::Visual { anchor: 0 }, &c), 0..3);
        assert_eq!(selection(&Mode::Normal, &c), 2..3);
    }

    #[test]
    fn the_cursor_returns_to_the_same_node_after_a_requery_or_the_clamped_index() {
        use crate::core::plan::ColumnPlan;
        use geode_core::attribution::{Attribution, ScopeSemantics};
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
        use geode_core::view::ViewSpec;
        let dim = |n: &str| ColumnMeta { name: n.into(), attribution_by_depth: vec![Attribution::Additive; 3], scope_semantics: ScopeSemantics::Direct };
        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Str(vec![None, Some("L2"), Some("L1")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1])),
            ],
            1,
        );
        let doc = merge_docs("views", &[LayerDoc::builtin("views", "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n").unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        let visible = vec![0u32, 1, 2];
        assert_eq!(restore_by_path(&visible, &snap, &plan, &[Some("L1".into())], 0), 2);
        assert_eq!(restore_by_path(&visible, &snap, &plan, &[Some("GONE".into())], 7), 2, "fallback clamped");
        assert_eq!(restore_by_path(&visible, &snap, &plan, &[], 1), 0, "the root's path is empty");
    }
}
```

`find.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::vimfind::{FindDirection, FindStyle};

    fn texts() -> Vec<String> {
        ["Total", "L1", "SPX", "NDX", "SPX"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn vim_style_jumps_as_typed_commits_and_repeats() {
        let mut f = FindState::begin(FindStyle::Vim, 0);
        assert_eq!(f.changed(&texts(), "sp"), Some(2), "first match at or after the origin");
        assert_eq!(f.changed(&texts(), "spq"), None, "no match: the caller keeps the cursor");
        assert_eq!(f.narrowed, None, "vim never narrows");
        f.committed("sp");
        assert_eq!(f.repeat(&texts(), 2, FindDirection::Forward, None), Some(4));
        assert_eq!(f.repeat(&texts(), 4, FindDirection::Forward, None), Some(2), "wraps");
        assert_eq!(f.repeat(&texts(), 4, FindDirection::Backward, None), Some(2));
        assert_eq!(f.repeat(&texts(), 0, FindDirection::Forward, Some(2)), Some(4), "3n is the third match on: counted");
        assert_eq!(FindState::begin(FindStyle::Vim, 0).repeat(&texts(), 0, FindDirection::Forward, None), None, "nothing committed");
        assert_eq!(f.cancelled(), 0, "escape returns the origin");
    }

    #[test]
    fn fzf_style_narrows_as_typed_and_restores_on_cancel() {
        let mut f = FindState::begin(FindStyle::Fzf, 3);
        assert_eq!(f.changed(&texts(), "x"), Some(0), "the cursor sits on the first narrowed row");
        assert_eq!(f.narrowed, Some(vec![2, 3, 4]));
        f.committed("x");
        assert_eq!(f.narrowed, Some(vec![2, 3, 4]), "Enter keeps the narrowed list");
        assert_eq!(f.cancelled(), 3);
        assert_eq!(f.narrowed, None, "escape restores");
        assert_eq!(f.repeat(&texts(), 0, FindDirection::Forward, None), None, "n is vim-style only");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement**

`cursor.rs`:

```rust
//! The cursor (Phase 3 spec §6.1): a visible-row and column pair driven
//! by the shell's `vimnav` vocabulary, multiplied by the engine's count.

use crate::core::expansion::path_of;
use crate::core::plan::ColumnPlan;
use geode_core::snapshot::Snapshot;
use geode_shell::vimnav::{NavCommand, apply};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Visual {
        anchor: usize,
    },
}

impl Cursor {
    pub fn move_rows(&mut self, len: usize, cmd: NavCommand, count: Option<u32>) {
        let n = count.unwrap_or(1) as i64;
        let cmd = match (cmd, count) {
            (NavCommand::Move(d), _) => NavCommand::Move(d * n),
            // `12G` is "row 12", vim-style, 1-based.
            (NavCommand::Bottom, Some(c)) => {
                self.row = (c.max(1) as usize - 1).min(len.saturating_sub(1));
                return;
            }
            (other, _) => other,
        };
        self.row = apply(self.row, len, cmd);
    }

    pub fn move_cols(&mut self, cols: usize, delta: i64, count: Option<u32>) {
        let n = count.unwrap_or(1) as i64;
        self.col = apply(self.col, cols, NavCommand::Move(delta * n));
    }

    pub fn to_row(&mut self, row: usize, len: usize) {
        self.row = row.min(len.saturating_sub(1));
    }

    pub fn clamp(&mut self, len: usize, cols: usize) {
        self.row = self.row.min(len.saturating_sub(1));
        self.col = self.col.min(cols.saturating_sub(1));
    }
}

/// The rows a yank covers: anchor..=cursor in visual mode, the cursor
/// row alone otherwise. Returned as a half-open range.
pub fn selection(mode: &Mode, cursor: &Cursor) -> Range<usize> {
    match mode {
        Mode::Normal => cursor.row..cursor.row + 1,
        Mode::Visual { anchor } => {
            let (a, b) = (cursor.row.min(*anchor), cursor.row.max(*anchor));
            a..b + 1
        }
    }
}

/// The visible index whose node has `path`, or `fallback` clamped.
pub fn restore_by_path(
    visible: &[u32],
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    path: &[Option<String>],
    fallback: usize,
) -> usize {
    visible
        .iter()
        .position(|&r| path_of(snapshot, plan, r as usize) == path)
        .unwrap_or_else(|| fallback.min(visible.len().saturating_sub(1)))
}
```

`find.rs`:

```rust
//! `/` under both find styles (Phase 3 spec §6.1, §2.2). The shell owns
//! the input; this decides what typing into it does, over the tree
//! column's text of each visible row. Vim jumps, fzf narrows.

use geode_shell::vimfind::{FindDirection, FindStyle, filter_matches, find_match};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindState {
    pub style: FindStyle,
    /// Where the cursor was when `/` opened; `escape` returns here.
    pub origin: usize,
    pub committed: Option<String>,
    /// fzf: the visible indices that match, in row order.
    pub narrowed: Option<Vec<usize>>,
}

impl FindState {
    pub fn begin(style: FindStyle, cursor_row: usize) -> FindState {
        FindState {
            style,
            origin: cursor_row,
            committed: None,
            narrowed: None,
        }
    }

    /// The query changed. Vim: the visible row to move the cursor to, if
    /// any; fzf: the index *into the narrowed list* (0 when anything
    /// matches), and `narrowed` is updated.
    pub fn changed(&mut self, texts: &[String], query: &str) -> Option<usize> {
        match self.style {
            FindStyle::Vim => find_match(texts, self.origin, FindDirection::Forward, query),
            FindStyle::Fzf => {
                let matches = filter_matches(texts, query);
                let any = !matches.is_empty();
                self.narrowed = Some(matches);
                any.then_some(0)
            }
        }
    }

    pub fn committed(&mut self, query: &str) {
        if !query.is_empty() {
            self.committed = Some(query.to_string());
        }
    }

    /// `escape`: the origin row; fzf's narrowing is dropped.
    pub fn cancelled(&mut self) -> usize {
        self.narrowed = None;
        self.origin
    }

    /// `n`/`N`, counted. Vim-style only; fzf has every visible row
    /// matching already.
    pub fn repeat(&self, texts: &[String], from: usize, dir: FindDirection, count: Option<u32>) -> Option<usize> {
        if self.style != FindStyle::Vim || texts.is_empty() {
            return None;
        }
        let query = self.committed.as_deref()?;
        let mut at = from;
        for _ in 0..count.unwrap_or(1).max(1) {
            let start = match dir {
                FindDirection::Forward => (at + 1) % texts.len(),
                FindDirection::Backward => (at + texts.len() - 1) % texts.len(),
            };
            at = find_match(texts, start, dir, query)?;
        }
        Some(at)
    }
}
```

- [ ] **Step 4: Run, check, commit**

```bash
git add crates/geode-blotter
git commit -m "feat(blotter): cursor, visual mode, path restore, and find under both styles

Counted row and column motion through vimnav; a visual range; the
cursor returns to the same node after a requery; / jumps under vim and
narrows under fzf with n/N counted (Phase 3 §6.1, §2.2).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

- [ ] **Step 5: Harness entries** (package `geode-blotter`)

```sh
run_mutation "find: fzf narrows and vim does not" \
  crates/geode-blotter/src/core/find.rs \
  '            FindStyle::Vim => find_match(texts, self.origin, FindDirection::Forward, query),' \
  '            FindStyle::Vim => { self.narrowed = Some(filter_matches(texts, query)); find_match(texts, self.origin, FindDirection::Forward, query) }' \
  geode-blotter

run_mutation "cursor: a counted G is a row number" \
  crates/geode-blotter/src/core/cursor.rs \
  '                self.row = (c.max(1) as usize - 1).min(len.saturating_sub(1));' \
  '                self.row = len.saturating_sub(1); let _ = c;' \
  geode-blotter
```

Run: `zsh scripts/mutation-check.sh "find:"` and `"cursor:"`. Commit.

---

### Task 4: Number formatting, the format cache, and yank

Spec §6.2, §6.4, §6.5 and the §7.2 render discipline: cells are formatted
once per snapshot per visible window and never per frame.

**Files:**
- Create: `src/core/format.rs`, `src/core/cache.rs`, `src/core/yank.rs`
- Test: inline

**Interfaces:**
- Produces:
  ```rust
  pub enum Sign { Negative, Zero, Positive }
  pub struct Formatted { pub text: String, pub sign: Sign }
  pub fn format_number(value: f64, format: &ColumnFormat) -> Formatted;
  pub struct CachedCell { pub text: Arc<str>, pub sign: Option<Sign>, pub attribution: Attribution }
  pub struct FormatCache { .. }   // Default
  impl FormatCache {
      pub fn invalidate(&mut self);
      pub fn set_window(&mut self, window: Range<usize>, cols: usize, fill: impl FnMut(usize, usize) -> Option<CachedCell>);
      pub fn get(&self, row: usize, col: usize) -> Option<&CachedCell>;
      pub fn window(&self) -> Range<usize>;
  }
  pub fn cell(snapshot: &Snapshot, plan: &ColumnPlan, row: usize, col: usize) -> Option<CachedCell>;   // the one formatting function
  pub fn tsv(snapshot: &Snapshot, plan: &ColumnPlan, visible: &[u32], range: Range<usize>) -> String;
  ```

- [ ] **Step 1: Write the failing tests**

`format.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::view::{ColumnFormat, Colour, Negative, Scale};

    fn f(precision: u8, thousands: bool, negative: Negative, scale: Scale) -> ColumnFormat {
        ColumnFormat { precision, thousands, negative, colour: Colour::Sign, scale }
    }

    #[test]
    fn precision_thousands_and_sign() {
        let m = ColumnFormat::MEASURE;
        assert_eq!(format_number(1234567.891, &m).text, "1,234,567.89");
        assert_eq!(format_number(-0.5, &m).text, "-0.50");
        assert_eq!(format_number(0.0, &m).sign, Sign::Zero);
        assert_eq!(format_number(-0.001, &m).text, "0.00", "rounds to zero, no negative zero");
        assert_eq!(format_number(-0.001, &m).sign, Sign::Zero);
        assert_eq!(format_number(42.0, &f(0, false, Negative::Minus, Scale::None)).text, "42");
        assert_eq!(format_number(-42.5, &f(0, false, Negative::Minus, Scale::None)).text, "-43");
    }

    #[test]
    fn parentheses_and_scale() {
        assert_eq!(format_number(-1234.5, &f(1, true, Negative::Parens, Scale::None)).text, "(1,234.5)");
        assert_eq!(format_number(1234567.89, &f(0, true, Negative::Minus, Scale::Thousands)).text, "1,235");
        assert_eq!(format_number(1234567.89, &f(2, true, Negative::Minus, Scale::Millions)).text, "1.23");
        assert_eq!(format_number(-999.0, &f(0, true, Negative::Parens, Scale::Thousands)).text, "(1)");
        assert_eq!(format_number(-400.0, &f(0, true, Negative::Parens, Scale::Thousands)).text, "0", "rounds to zero after scaling");
    }

    #[test]
    fn non_finite_values_are_spelled_not_crashed() {
        let m = ColumnFormat::MEASURE;
        assert_eq!(format_number(f64::NAN, &m).text, "NaN");
        assert_eq!(format_number(f64::INFINITY, &m).text, "∞");
        assert_eq!(format_number(f64::NEG_INFINITY, &m).text, "-∞");
    }
}
```

`cache.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plan::ColumnPlan;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;
    use std::cell::Cell;

    #[test]
    fn a_window_move_refills_only_the_rows_that_entered() {
        let mut c = FormatCache::default();
        let fills = Cell::new(0);
        let fill = |r: usize, _c: usize| {
            fills.set(fills.get() + 1);
            Some(CachedCell { text: format!("r{r}").into(), sign: None, attribution: Attribution::Additive })
        };
        c.set_window(0..10, 2, fill);
        assert_eq!(fills.get(), 20);
        assert_eq!(c.get(3, 1).map(|x| &*x.text), Some("r3"));
        c.set_window(5..15, 2, fill);
        assert_eq!(fills.get(), 30, "five new rows, two columns");
        assert_eq!(c.get(14, 0).map(|x| &*x.text), Some("r14"));
        assert!(c.get(4, 0).is_none(), "left the window");
        c.invalidate();
        assert!(c.get(7, 0).is_none());
        c.set_window(5..15, 2, fill);
        assert_eq!(fills.get(), 50, "everything refilled after invalidation");
    }

    #[test]
    fn cells_honour_the_read_paths_opinions() {
        // NonAttributable is NULL and paints blank, never 0.00; a
        // DeterminedNonAdditive cell carries its value and its marker; a
        // real zero paints.
        let meta = |n: &str, by_depth: Vec<Attribution>| ColumnMeta { name: n.into(), attribution_by_depth: by_depth, scope_semantics: ScopeSemantics::Direct };
        let snap = Snapshot::for_tests(
            vec![
                (meta("lhu", vec![Attribution::Additive; 3]), TestColumn::Str(vec![None, Some("L1"), Some("L1")])),
                (meta("underlying_ref", vec![Attribution::Additive; 3]), TestColumn::Str(vec![None, None, Some("SPX")])),
                (meta("row_depth", vec![Attribution::Additive; 3]), TestColumn::I32(vec![0, 1, 2])),
                (meta("cross_gamma02", vec![Attribution::Additive, Attribution::NonAttributable, Attribution::Additive]), TestColumn::F64(vec![Some(0.0), None, Some(2.5)])),
                (meta("daily_trading_pnl", vec![Attribution::Additive, Attribution::Additive, Attribution::DeterminedNonAdditive]), TestColumn::F64(vec![Some(7.0), Some(7.0), Some(7.0)])),
            ],
            2,
        );
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[t.columns]]\nname = \"cross_gamma02\"\n[[t.columns]]\nname = \"daily_trading_pnl\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);

        assert_eq!(cell(&snap, &plan, 0, 1).map(|c| c.text.to_string()), Some("0.00".into()), "a real zero paints");
        assert_eq!(cell(&snap, &plan, 1, 1), None, "NonAttributable is blank, never 0.00");
        let leaf = cell(&snap, &plan, 2, 2).unwrap();
        assert_eq!(&*leaf.text, "7.00");
        assert_eq!(leaf.attribution, Attribution::DeterminedNonAdditive);
        assert_eq!(cell(&snap, &plan, 0, 0), None, "the grand total has no tree text");
        assert_eq!(cell(&snap, &plan, 2, 0).map(|c| c.text.to_string()), Some("SPX".into()));
        assert_eq!(cell(&snap, &plan, 9, 1), None, "past the end");
        assert_eq!(cell(&snap, &plan, 0, 9), None, "no such column");
    }
}
```

`yank.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plan::ColumnPlan;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    #[test]
    fn tsv_has_a_header_indented_tree_text_raw_numbers_and_blanks() {
        let dim = |n: &str| ColumnMeta { name: n.into(), attribution_by_depth: vec![Attribution::Additive; 3], scope_semantics: ScopeSemantics::Direct };
        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Str(vec![None, Some("L1"), Some("L1")])),
                (dim("underlying_ref"), TestColumn::Str(vec![None, None, Some("SPX")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 2])),
                (dim("delta01"), TestColumn::F64(vec![Some(1234567.891), Some(1.5), None])),
            ],
            2,
        );
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[t.columns]]\nname = \"delta01\"\nformat = { precision = 0, scale = \"k\" }\nlabel = \"Δ\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        let out = tsv(&snap, &plan, &[0, 1, 2], 0..3);
        assert_eq!(
            out,
            "lhu / underlying_ref\tΔ (k)\n\t1234567.891\n  L1\t1.5\n    SPX\t\n",
            "raw, unscaled values; blank for NULL; two spaces per depth"
        );
        assert_eq!(tsv(&snap, &plan, &[0, 1, 2], 1..2), "lhu / underlying_ref\tΔ (k)\n  L1\t1.5\n");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement**

`format.rs`:

```rust
//! Number formatting (Phase 3 spec §6.2). Scale first, then precision,
//! then grouping and the negative style; the sign is taken from the
//! rounded value so `-0.001` at two places is a zero, not a red zero.

use geode_core::view::{ColumnFormat, Negative, Scale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Negative,
    Zero,
    Positive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatted {
    pub text: String,
    pub sign: Sign,
}

pub fn format_number(value: f64, format: &ColumnFormat) -> Formatted {
    if value.is_nan() {
        return Formatted { text: "NaN".into(), sign: Sign::Zero };
    }
    if value.is_infinite() {
        return Formatted {
            text: if value > 0.0 { "∞".into() } else { "-∞".into() },
            sign: if value > 0.0 { Sign::Positive } else { Sign::Negative },
        };
    }
    let scaled = match format.scale {
        Scale::None => value,
        s => value / s.divisor(),
    };
    let precision = format.precision as usize;
    let factor = 10f64.powi(precision as i32);
    let rounded = (scaled * factor).round() / factor;
    let sign = if rounded == 0.0 {
        Sign::Zero
    } else if rounded < 0.0 {
        Sign::Negative
    } else {
        Sign::Positive
    };
    let magnitude = format!("{:.*}", precision, rounded.abs());
    let magnitude = if format.thousands { group_thousands(&magnitude) } else { magnitude };
    let text = match (sign, format.negative) {
        (Sign::Negative, Negative::Minus) => format!("-{magnitude}"),
        (Sign::Negative, Negative::Parens) => format!("({magnitude})"),
        _ => magnitude,
    };
    Formatted { text, sign }
}

/// `1234567.89` → `1,234,567.89`; the fraction is left alone.
fn group_thousands(s: &str) -> String {
    let (int, frac) = match s.find('.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let digits: Vec<char> = int.chars().collect();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out.push_str(frac);
    out
}
```

`cache.rs`:

```rust
//! The format cache (foundation §7.2, Phase 3 spec §6.1): shaped text
//! for the visible window, per snapshot, filled from `DataTable`'s
//! `visible_rows_changed` and on arrival — never in `render_td`. A new
//! snapshot invalidates everything; a window move refills only rows
//! that entered.

use crate::core::format::{Sign, format_number};
use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::attribution::Attribution;
use geode_core::snapshot::Snapshot;
use std::ops::Range;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCell {
    pub text: Arc<str>,
    /// `Some` for a number; drives the sign colour.
    pub sign: Option<Sign>,
    pub attribution: Attribution,
}

#[derive(Debug, Default)]
pub struct FormatCache {
    start: usize,
    cols: usize,
    /// `rows[i]` is visible row `start + i`; each holds `cols` cells.
    rows: Vec<Vec<Option<CachedCell>>>,
}

impl FormatCache {
    pub fn invalidate(&mut self) {
        self.rows.clear();
    }

    pub fn window(&self) -> Range<usize> {
        self.start..self.start + self.rows.len()
    }

    /// Move the window, keeping overlapping rows and filling the rest.
    pub fn set_window(
        &mut self,
        window: Range<usize>,
        cols: usize,
        mut fill: impl FnMut(usize, usize) -> Option<CachedCell>,
    ) {
        if cols != self.cols {
            self.rows.clear();
            self.cols = cols;
        }
        let old = self.window();
        let mut rows: Vec<Vec<Option<CachedCell>>> = Vec::with_capacity(window.len());
        for r in window.clone() {
            if old.contains(&r) {
                rows.push(std::mem::take(&mut self.rows[r - old.start]));
            } else {
                rows.push((0..cols).map(|c| fill(r, c)).collect());
            }
        }
        self.start = window.start;
        self.rows = rows;
    }

    pub fn get(&self, row: usize, col: usize) -> Option<&CachedCell> {
        let i = row.checked_sub(self.start)?;
        self.rows.get(i)?.get(col)?.as_ref()
    }
}

/// The one place a cell becomes text (§6.5). A `NonAttributable` cell is
/// NULL in the snapshot and `f64_at` says so; nothing here can turn it
/// into `0.00`.
pub fn cell(snapshot: &Snapshot, plan: &ColumnPlan, row: usize, col: usize) -> Option<CachedCell> {
    let column = plan.columns.get(col)?;
    if row >= snapshot.rows() {
        return None;
    }
    let depth = snapshot.tree().depth(row);
    let attribution = plan.attribution(col, depth);
    match column.kind {
        ColumnKind::Tree => plan.tree_text(snapshot, row).map(|t| CachedCell {
            text: t.into(),
            sign: None,
            attribution,
        }),
        ColumnKind::Measure => {
            let idx = column.index?;
            let value = snapshot.f64_at(idx, row)?;
            let f = format_number(value, &column.format);
            Some(CachedCell {
                text: f.text.into(),
                sign: Some(f.sign),
                attribution,
            })
        }
        ColumnKind::Dimension => {
            let idx = column.index?;
            let text = snapshot.display_at(idx, row)?;
            Some(CachedCell {
                text: text.into(),
                sign: None,
                attribution,
            })
        }
    }
}
```

`yank.rs`:

```rust
//! Yank as TSV (Phase 3 spec §6.4): a header line of labels, then one
//! line per selected visible row — tree text indented two spaces per
//! depth, numbers raw and unscaled, blanks for NULL.

use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::snapshot::Snapshot;
use std::fmt::Write as _;
use std::ops::Range;

pub fn tsv(snapshot: &Snapshot, plan: &ColumnPlan, visible: &[u32], range: Range<usize>) -> String {
    let mut out = String::new();
    let labels: Vec<&str> = plan.columns.iter().map(|c| c.label.as_str()).collect();
    out.push_str(&labels.join("\t"));
    out.push('\n');
    for &row in visible.iter().skip(range.start).take(range.len()) {
        let row = row as usize;
        let mut fields: Vec<String> = Vec::with_capacity(plan.columns.len());
        for column in &plan.columns {
            let field = match column.kind {
                ColumnKind::Tree => {
                    let depth = snapshot.tree().depth(row);
                    let mut s = "  ".repeat(depth);
                    if let Some(t) = plan.tree_text(snapshot, row) {
                        s.push_str(t);
                    }
                    s
                }
                ColumnKind::Measure => column
                    .index
                    .and_then(|i| snapshot.f64_at(i, row))
                    .map(|v| {
                        let mut s = String::new();
                        let _ = write!(s, "{v}");
                        s
                    })
                    .unwrap_or_default(),
                ColumnKind::Dimension => column
                    .index
                    .and_then(|i| snapshot.display_at(i, row))
                    .unwrap_or_default(),
            };
            fields.push(field);
        }
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}
```

- [ ] **Step 4: Run, check, commit**

```bash
git add crates/geode-blotter
git commit -m "feat(blotter): number formatting, the format cache, yank as TSV

Scale, precision, grouping and negative style with the sign taken from
the rounded value; a window cache that refills only rows that entered;
a NonAttributable cell cannot become 0.00 because the accessor says
NULL (Phase 3 §6.2, §6.4, §6.5).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

- [ ] **Step 5: Harness entries** (package `geode-blotter`)

```sh
run_mutation "cache: a NULL measure is None, never a number" \
  crates/geode-blotter/src/core/cache.rs \
  '            let value = snapshot.f64_at(idx, row)?;' \
  '            let value = snapshot.f64_at(idx, row).unwrap_or(0.0);' \
  geode-blotter

run_mutation "cache: a window move keeps overlapping rows" \
  crates/geode-blotter/src/core/cache.rs \
  '            if old.contains(&r) {' \
  '            if false {' \
  geode-blotter

run_mutation "format: the sign is of the rounded value" \
  crates/geode-blotter/src/core/format.rs \
  '    let sign = if rounded == 0.0 {' \
  '    let sign = if scaled == 0.0 {' \
  geode-blotter

run_mutation "format: scale divides before precision" \
  crates/geode-blotter/src/core/format.rs \
  '        s => value / s.divisor(),' \
  '        s => { let _ = s; value }' \
  geode-blotter

run_mutation "yank: numbers are raw and unscaled" \
  crates/geode-blotter/src/core/yank.rs \
  '                        let _ = write!(s, "{v}");' \
  '                        let _ = write!(s, "{:.2}", v);' \
  geode-blotter
```

Run: `zsh scripts/mutation-check.sh "cache:"`, `"format:"`, `"yank:"`. Commit.

---

### Task 5: The `:` grammar and completion vocabulary

Spec §4.3, §3.4. Pure parsing into a `Command`; the vocabulary the shell
ranks.

**Files:**
- Create: `src/core/commands.rs`
- Test: inline

**Interfaces:**
- Produces:
  ```rust
  pub enum Command { Group(Vec<String>), GroupSlot(u8), GroupSave(u8), Unpin, Unscoped,
                     ScopeExpr(String), ScopeText(String), ScopeClear, ScopeUndo,
                     AsOf(String), Live, View(String), Sort { column: String, descending: bool }, SortClear }
  pub fn parse(line: &str) -> Result<Command, String>;
  pub struct Vocabulary { pub columns: Vec<String>, pub views: Vec<String> }
  pub fn completions(line: &str, cursor: usize, vocab: &Vocabulary) -> Vec<String>;
  pub fn parse_as_of(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocabulary {
        Vocabulary {
            columns: vec!["book".into(), "lhu".into(), "delta01".into()],
            views: vec!["tree".into(), "wide".into()],
        }
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(parse("group lhu,book").unwrap(), Command::Group(vec!["lhu".into(), "book".into()]));
        assert_eq!(parse("group lhu, book").unwrap(), Command::Group(vec!["lhu".into(), "book".into()]));
        assert_eq!(parse("group slot 3").unwrap(), Command::GroupSlot(3));
        assert_eq!(parse("group save 9").unwrap(), Command::GroupSave(9));
        assert_eq!(parse("unpin").unwrap(), Command::Unpin);
        assert_eq!(parse("unscoped").unwrap(), Command::Unscoped);
        assert_eq!(parse("scope book = 'BK001' and delta01 > 5").unwrap(), Command::ScopeExpr("book = 'BK001' and delta01 > 5".into()));
        assert_eq!(parse("scope text spx rut").unwrap(), Command::ScopeText("spx rut".into()));
        assert_eq!(parse("scope clear").unwrap(), Command::ScopeClear);
        assert_eq!(parse("scope undo").unwrap(), Command::ScopeUndo);
        assert_eq!(parse("asof 14:05").unwrap(), Command::AsOf("14:05".into()));
        assert_eq!(parse("live").unwrap(), Command::Live);
        assert_eq!(parse("view wide").unwrap(), Command::View("wide".into()));
        assert_eq!(parse("sort delta01").unwrap(), Command::Sort { column: "delta01".into(), descending: false });
        assert_eq!(parse("sort delta01 desc").unwrap(), Command::Sort { column: "delta01".into(), descending: true });
        assert_eq!(parse("sort clear").unwrap(), Command::SortClear);
        assert_eq!(parse("  sort   delta01  ").unwrap(), Command::Sort { column: "delta01".into(), descending: false });
    }

    #[test]
    fn errors_name_the_problem() {
        assert!(parse("").unwrap_err().contains("empty"));
        assert!(parse("frobnicate").unwrap_err().contains("unknown command 'frobnicate'"));
        assert!(parse("group").unwrap_err().contains("group"));
        assert!(parse("group slot 12").unwrap_err().contains("1–9"));
        assert!(parse("group save x").unwrap_err().contains("1–9"));
        assert!(parse("sort").unwrap_err().contains("column"));
        assert!(parse("sort delta01 up").unwrap_err().contains("desc"));
        assert!(parse("view").unwrap_err().contains("name"));
        assert!(parse("scope").unwrap_err().contains("scope"));
        assert!(parse("asof").unwrap_err().contains("time"));
    }

    #[test]
    fn completions_follow_the_argument_position() {
        let v = vocab();
        let names = |line: &str| completions(line, line.len(), &v);
        assert_eq!(names(""), vec!["asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view"]);
        assert_eq!(names("so"), vec!["asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view"], "the shell ranks; the vocabulary is whole");
        assert_eq!(names("sort "), vec!["book", "clear", "delta01", "lhu"]);
        assert_eq!(names("sort delta01 "), vec!["desc"]);
        assert_eq!(names("group "), vec!["book", "delta01", "lhu", "save", "slot"]);
        assert_eq!(names("group lhu,"), vec!["book", "delta01", "lhu"]);
        assert_eq!(names("group slot "), (1..=9).map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(names("group save "), (1..=9).map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(names("scope "), vec!["book", "clear", "delta01", "lhu", "text", "undo"]);
        assert_eq!(names("scope book = 'x' and "), vec!["book", "delta01", "lhu"]);
        assert_eq!(names("view "), vec!["tree", "wide"]);
        assert!(names("asof ").is_empty());
        assert!(names("sort delta01 desc ").is_empty());
        assert_eq!(completions("sort delta01", 2, &v), vec!["asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view"], "the cursor's word, not the last");
    }

    #[test]
    fn as_of_accepts_a_clock_time_today_or_rfc3339() {
        use chrono::{TimeZone, Utc};
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 16, 0, 0).unwrap();
        assert_eq!(parse_as_of("14:05", now), Ok(Utc.with_ymd_and_hms(2026, 9, 3, 14, 5, 0).unwrap()));
        assert_eq!(parse_as_of("2026-09-01T07:00:00Z", now), Ok(Utc.with_ymd_and_hms(2026, 9, 1, 7, 0, 0).unwrap()));
        assert!(parse_as_of("25:00", now).is_err());
        assert!(parse_as_of("yesterday", now).unwrap_err().contains("HH:MM"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement**

```rust
//! The `:` vocabulary (Phase 3 spec §4.3) as data: a line parses into a
//! `Command` the tile applies, and the vocabulary for the word under the
//! cursor is what the shell ranks (§3.4).

use chrono::{DateTime, NaiveTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Group(Vec<String>),
    GroupSlot(u8),
    GroupSave(u8),
    Unpin,
    Unscoped,
    ScopeExpr(String),
    ScopeText(String),
    ScopeClear,
    ScopeUndo,
    AsOf(String),
    Live,
    View(String),
    Sort { column: String, descending: bool },
    SortClear,
}

const COMMANDS: [&str; 8] = ["asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view"];

fn slot(arg: Option<&str>, what: &str) -> Result<u8, String> {
    arg.and_then(|a| a.parse::<u8>().ok())
        .filter(|n| (1..=9).contains(n))
        .ok_or_else(|| format!("{what} needs a slot number 1–9"))
}

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty command".into());
    }
    let (head, rest) = match line.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (line, ""),
    };
    match head {
        "unpin" => Ok(Command::Unpin),
        "unscoped" => Ok(Command::Unscoped),
        "live" => Ok(Command::Live),
        "group" => {
            let mut words = rest.split_whitespace();
            match words.next() {
                None => Err("group needs columns, `slot N` or `save N`".into()),
                Some("slot") => slot(words.next(), "group slot").map(Command::GroupSlot),
                Some("save") => slot(words.next(), "group save").map(Command::GroupSave),
                Some(_) => {
                    let columns: Vec<String> = rest
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    Ok(Command::Group(columns))
                }
            }
        }
        "scope" => match rest.split_once(char::is_whitespace) {
            None if rest == "clear" => Ok(Command::ScopeClear),
            None if rest == "undo" => Ok(Command::ScopeUndo),
            None if rest.is_empty() => Err("scope needs an expression, `text …`, `clear` or `undo`".into()),
            Some(("text", words)) => Ok(Command::ScopeText(words.trim().to_string())),
            _ => Ok(Command::ScopeExpr(rest.to_string())),
        },
        "asof" => {
            if rest.is_empty() {
                Err("asof needs a time: HH:MM or RFC 3339".into())
            } else {
                Ok(Command::AsOf(rest.to_string()))
            }
        }
        "view" => {
            if rest.is_empty() {
                Err("view needs a name".into())
            } else {
                Ok(Command::View(rest.to_string()))
            }
        }
        "sort" => {
            let mut words = rest.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (None, _, _) => Err("sort needs a column, or `clear`".into()),
                (Some("clear"), None, _) => Ok(Command::SortClear),
                (Some(column), None, _) => Ok(Command::Sort { column: column.into(), descending: false }),
                (Some(column), Some("desc"), None) => Ok(Command::Sort { column: column.into(), descending: true }),
                (Some(column), Some("asc"), None) => Ok(Command::Sort { column: column.into(), descending: false }),
                _ => Err("sort takes a column and optionally `desc`".into()),
            }
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    pub columns: Vec<String>,
    pub views: Vec<String>,
}

/// The candidates for the word at `cursor`. Sorted, so the shell's
/// ranking of an empty word is stable.
pub fn completions(line: &str, cursor: usize, vocab: &Vocabulary) -> Vec<String> {
    let cursor = cursor.min(line.len());
    let before = &line[..cursor];
    // The words completed so far, and whether the cursor is at the start
    // of a fresh word.
    let words: Vec<&str> = before
        .split(|c: char| c.is_whitespace() || c == ',')
        .collect();
    let (done, _current) = words.split_at(words.len().saturating_sub(1));
    let done: Vec<&str> = done.iter().copied().filter(|w| !w.is_empty()).collect();
    let mut out: Vec<String> = match done.as_slice() {
        [] => COMMANDS.iter().map(|s| s.to_string()).collect(),
        ["sort"] => {
            let mut v = vocab.columns.clone();
            v.push("clear".into());
            v
        }
        ["sort", _] => vec!["desc".into()],
        ["group"] => {
            let mut v = vocab.columns.clone();
            v.push("save".into());
            v.push("slot".into());
            v
        }
        ["group", "slot"] | ["group", "save"] => (1..=9).map(|n| n.to_string()).collect(),
        ["group", ..] => vocab.columns.clone(),
        ["scope"] => {
            let mut v = vocab.columns.clone();
            v.extend(["clear", "text", "undo"].map(String::from));
            v
        }
        ["scope", "text", ..] => Vec::new(),
        ["scope", ..] => vocab.columns.clone(),
        ["view"] => vocab.views.clone(),
        _ => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

/// `HH:MM` means today at that time (UTC, the data's clock); anything
/// else must be RFC 3339.
pub fn parse_as_of(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return Ok(now.date_naive().and_time(t).and_utc());
    }
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("'{text}' is not HH:MM, YYYY-MM-DD[ HH:MM[:SS]] or an RFC 3339 time"))
}
```

- [ ] **Step 4: Run, check, commit**

```bash
git add crates/geode-blotter
git commit -m "feat(blotter): the : grammar and completion vocabulary

Every §4.3 command parses to data with an error that names the problem;
the vocabulary follows the argument position so the shell can rank it
(Phase 3 §4.3, §3.4).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

- [ ] **Step 5: Harness entry** (package `geode-blotter`)

```sh
run_mutation "commands: sort desc is parsed" \
  crates/geode-blotter/src/core/commands.rs \
  '                (Some(column), Some("desc"), None) => Ok(Command::Sort { column: column.into(), descending: true }),' \
  '                (Some(column), Some("desc"), None) => Ok(Command::Sort { column: column.into(), descending: false }),' \
  geode-blotter
```

Run: `zsh scripts/mutation-check.sh "commands:"`. Commit.

---

### Task 6: `BlotterDelegate` — the `DataTable` adapter

Spec §6.6. The delegate owns what the table renders; every `render_td`
is a cache lookup.

**Files:**
- Create: `src/delegate.rs`
- Test: inline (pure state methods) — painting is covered in Task 7

**Interfaces:**
- Produces:
  ```rust
  pub struct BlotterDelegate {
      pub snapshot: Option<Arc<Snapshot>>, pub plan: Option<ColumnPlan>, pub expansion: Expansion,
      pub visible: Vec<u32>, pub shown: Vec<u32>, pub cursor: Cursor, pub mode: Mode,
      pub sort: Option<SortSpec>, pub cache: FormatCache, pub narrowed: Option<Vec<usize>>,
      pub unplaced: usize, pub any_determined: bool, pub semi_joined: Vec<String>,
  }
  impl BlotterDelegate {
      pub fn new() -> Self;
      pub fn apply_snapshot(&mut self, snapshot: Arc<Snapshot>, view: &ViewSpec, grouping: &[String]);
      pub fn reflatten(&mut self);                 // after expansion/sort change; keeps the cursor's node
      pub fn set_narrowed(&mut self, rows: Option<Vec<usize>>);
      pub fn cursor_row_index(&self) -> Option<usize>;     // snapshot row under the cursor
      pub fn cursor_path(&self) -> Option<Path>;
      pub fn shown_texts(&self) -> Vec<String>;           // tree text per shown row, for find
      pub fn depth_bound(&self, grouping_len: usize) -> usize;
      pub fn expand_cursor(&mut self, open: Option<bool>) -> bool;   // None toggles; true if now open
      pub fn cursor_needs_more_depth(&self, grouping_len: usize) -> bool;
      pub fn refill_window(&mut self, window: Range<usize>);
  }
  impl TableDelegate for BlotterDelegate { … }
  ```

- [ ] **Step 1: Write the failing pure tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta { name: name.into(), attribution_by_depth: vec![Attribution::Additive; 4], scope_semantics: ScopeSemantics::Direct }
    }
    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }
    /// Root; L1, L2; L1/SPX, L1/NDX. L2's children are not materialised
    /// (depth bound 2 would give them, this fixture stops at L1's).
    fn snapshot() -> Arc<Snapshot> {
        Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1"), s("L2"), s("L1"), s("L1")])),
                (dim("underlying_ref"), TestColumn::Dict(vec![None, None, None, s("SPX"), s("NDX")])),
                (dim("position_ref"), TestColumn::Str(vec![None; 5])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2, 2])),
                (dim("delta01"), TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(2.0), Some(3.0)])),
            ],
            3,
        ))
    }
    fn view() -> ViewSpec {
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\", \"position_ref\"]\n[[t.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }
    fn grouping() -> Vec<String> {
        vec!["lhu".into(), "underlying_ref".into(), "position_ref".into()]
    }

    #[test]
    fn applying_a_snapshot_builds_the_plan_flattens_and_keeps_the_cursor_node() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        assert_eq!(d.shown, vec![0, 1, 2]);
        assert_eq!(d.plan.as_ref().unwrap().columns.len(), 2);
        d.cursor.row = 2; // L2
        assert!(d.expand_cursor(None), "L2 opens (nothing materialised beneath, but the path is open)");
        assert!(d.cursor_needs_more_depth(3), "L2's children are past the bound");
        assert_eq!(d.depth_bound(3), 2);
        // A new snapshot with L2 first: the cursor follows L2.
        let reordered = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L2"), s("L1"), s("L2")])),
                (dim("underlying_ref"), TestColumn::Dict(vec![None, None, None, s("RUT")])),
                (dim("position_ref"), TestColumn::Str(vec![None; 4])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2])),
                (dim("delta01"), TestColumn::F64(vec![Some(9.0), Some(4.0), Some(5.0), Some(4.0)])),
            ],
            3,
        ));
        d.apply_snapshot(reordered, &view(), &grouping());
        assert_eq!(d.shown, vec![0, 1, 3, 2], "L2 is open and now has a child");
        assert_eq!(d.cursor.row, 1, "still on L2");
        assert!(!d.cursor_needs_more_depth(3), "its child is here");
    }

    #[test]
    fn a_regroup_prunes_expansion_and_rebuilds_the_plan() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.cursor.row = 1;
        d.expand_cursor(Some(true));
        d.cursor.row = 2;
        assert_eq!(d.shown, vec![0, 1, 3, 4, 2]);
        let by_lhu = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1"), s("L2")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (dim("delta01"), TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)])),
            ],
            1,
        ));
        d.apply_snapshot(by_lhu, &view(), &["lhu".to_string()]);
        assert_eq!(d.plan.as_ref().unwrap().grouping, vec!["lhu".to_string()]);
        assert_eq!(d.shown, vec![0, 1, 2]);
        assert_eq!(d.depth_bound(1), 1, "an L1 path deeper than the grouping was pruned");
    }

    #[test]
    fn narrowing_changes_what_is_shown_and_the_cache_window_follows_shown_rows() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.cursor.row = 1;
        d.expand_cursor(Some(true));
        d.set_narrowed(Some(vec![3, 4]));
        assert_eq!(d.shown, vec![3, 4]);
        assert_eq!(d.shown_texts(), vec!["SPX".to_string(), "NDX".to_string()]);
        d.refill_window(0..2);
        assert_eq!(d.cache.get(1, 0).map(|c| c.text.to_string()), Some("NDX".into()));
        assert_eq!(d.cache.get(1, 1).map(|c| c.text.to_string()), Some("3.00".into()));
        d.set_narrowed(None);
        assert_eq!(d.shown, vec![0, 1, 3, 4, 2]);
        assert!(d.cache.get(1, 0).is_none(), "invalidated with the narrowing");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement**

```rust
//! The `TableDelegate` adapter (Phase 3 spec §6.6). Owns everything the
//! table renders — snapshot, plan, expansion, the flattened and shown
//! row lists, cursor, mode, sort, the format cache — so every
//! `render_td` is a lookup. The pure core does the work; this file only
//! sequences it and paints.

use crate::core::cache::{FormatCache, cell};
use crate::core::cursor::{Cursor, Mode, restore_by_path, selection};
use crate::core::expansion::{Expansion, Path, depth_bound, path_of};
use crate::core::flatten::{SortSpec, flatten};
use crate::core::format::Sign;
use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::attribution::Attribution;
use geode_core::snapshot::Snapshot;
use geode_core::view::{Colour, ViewSpec};
use geode_shell::fonts;
use gpui::prelude::*;
use gpui::{App, Context, Div, IntoElement, SharedString, Stateful, TextAlign, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::table::{Column, ColumnSort, TableDelegate, TableState};
use std::ops::Range;
use std::sync::Arc;

const INDENT: f32 = 14.0;
const DETERMINED_MARK: &str = "†";

pub struct BlotterDelegate {
    pub snapshot: Option<Arc<Snapshot>>,
    pub plan: Option<ColumnPlan>,
    pub expansion: Expansion,
    /// The full flatten.
    pub visible: Vec<u32>,
    /// What the table shows: `visible`, or its fzf-narrowed subset.
    pub shown: Vec<u32>,
    pub cursor: Cursor,
    pub mode: Mode,
    pub sort: Option<SortSpec>,
    pub cache: FormatCache,
    pub narrowed: Option<Vec<usize>>,
    pub unplaced: usize,
    /// Whether any painted cell carried the dagger, for the footer.
    pub any_determined: bool,
    pub semi_joined: Vec<String>,
}

impl Default for BlotterDelegate {
    fn default() -> Self {
        Self::new()
    }
}

impl BlotterDelegate {
    pub fn new() -> Self {
        BlotterDelegate {
            snapshot: None,
            plan: None,
            expansion: Expansion::default(),
            visible: Vec::new(),
            shown: Vec::new(),
            cursor: Cursor::default(),
            mode: Mode::Normal,
            sort: None,
            cache: FormatCache::default(),
            narrowed: None,
            unplaced: 0,
            any_determined: false,
            semi_joined: Vec::new(),
        }
    }

    pub fn cursor_path(&self) -> Option<Path> {
        let snapshot = self.snapshot.as_ref()?;
        let plan = self.plan.as_ref()?;
        let row = *self.shown.get(self.cursor.row)? as usize;
        Some(path_of(snapshot, plan, row))
    }

    pub fn cursor_row_index(&self) -> Option<usize> {
        self.shown.get(self.cursor.row).map(|r| *r as usize)
    }

    pub fn apply_snapshot(&mut self, snapshot: Arc<Snapshot>, view: &ViewSpec, grouping: &[String]) {
        let keep = self.cursor_path();
        let rebuild = match &self.plan {
            None => true,
            Some(p) => p.grouping != grouping || !p.same_columns(&snapshot),
        };
        if rebuild {
            self.plan = Some(ColumnPlan::build(view, grouping, &snapshot));
            self.sort = self.sort.filter(|s| s.column < self.plan.as_ref().unwrap().columns.len());
        }
        self.expansion.prune_to(grouping.len());
        self.unplaced = snapshot.tree().unplaced();
        self.semi_joined = self
            .plan
            .as_ref()
            .map(|p| {
                let mut v: Vec<String> = p.columns.iter().flat_map(|c| c.semi_joined.iter().cloned()).collect();
                v.sort();
                v.dedup();
                v
            })
            .unwrap_or_default();
        self.snapshot = Some(snapshot);
        self.narrowed = None;
        self.reflatten_keeping(keep);
    }

    pub fn reflatten(&mut self) {
        let keep = self.cursor_path();
        self.reflatten_keeping(keep);
    }

    fn reflatten_keeping(&mut self, keep: Option<Path>) {
        let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) else {
            self.visible.clear();
            self.shown.clear();
            return;
        };
        flatten(snapshot, plan, &self.expansion, self.sort.as_ref(), &mut self.visible);
        self.rebuild_shown();
        if let Some(path) = keep {
            self.cursor.row = restore_by_path(&self.shown, snapshot, plan, &path, self.cursor.row);
        }
        self.cursor.clamp(self.shown.len(), plan.columns.len());
        self.cache.invalidate();
    }

    fn rebuild_shown(&mut self) {
        self.shown.clear();
        match &self.narrowed {
            None => self.shown.extend_from_slice(&self.visible),
            Some(idx) => self.shown.extend(idx.iter().filter_map(|i| self.visible.get(*i).copied())),
        }
    }

    pub fn set_narrowed(&mut self, rows: Option<Vec<usize>>) {
        self.narrowed = rows;
        self.rebuild_shown();
        let cols = self.plan.as_ref().map_or(0, |p| p.columns.len());
        self.cursor.clamp(self.shown.len(), cols);
        self.cache.invalidate();
    }

    pub fn shown_texts(&self) -> Vec<String> {
        let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) else {
            return Vec::new();
        };
        self.shown
            .iter()
            .map(|&r| plan.tree_text(snapshot, r as usize).unwrap_or("").to_string())
            .collect()
    }

    pub fn depth_bound(&self, grouping_len: usize) -> usize {
        depth_bound(&self.expansion, grouping_len)
    }

    /// `zo`/`zc`/`za` on the cursor row. `Some(true)` opens, `Some(false)`
    /// closes (or closes the parent when the row is a leaf or closed,
    /// vim-style), `None` toggles. Returns whether the row is open after.
    pub fn expand_cursor(&mut self, open: Option<bool>) -> bool {
        let Some(path) = self.cursor_path() else {
            return false;
        };
        if path.is_empty() {
            return true; // the root is always open
        }
        let now_open = match open {
            Some(true) => {
                self.expansion.open(path.clone());
                true
            }
            Some(false) => {
                if self.expansion.is_open(&path) {
                    self.expansion.close(&path);
                } else if path.len() > 1 {
                    let parent = &path[..path.len() - 1];
                    self.expansion.close(parent);
                    // Move the cursor to the parent it just closed.
                    if let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) {
                        self.cursor.row = restore_by_path(&self.shown, snapshot, plan, parent, self.cursor.row);
                    }
                }
                false
            }
            None => self.expansion.toggle(path.clone()),
        };
        self.reflatten();
        now_open
    }

    /// The cursor row is open but has no materialised children: the
    /// snapshot stopped at the depth bound and a requery is needed.
    pub fn cursor_needs_more_depth(&self, grouping_len: usize) -> bool {
        let (Some(snapshot), Some(path)) = (&self.snapshot, self.cursor_path()) else {
            return false;
        };
        let Some(row) = self.cursor_row_index() else {
            return false;
        };
        !path.is_empty()
            && path.len() < grouping_len
            && self.expansion.is_open(&path)
            && !snapshot.tree().has_children(row)
    }

    /// Fill the cache for a window of *shown* rows.
    pub fn refill_window(&mut self, window: Range<usize>) {
        let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) else {
            return;
        };
        let cols = plan.columns.len();
        let shown = &self.shown;
        let mut any_determined = false;
        self.cache.set_window(window, cols, |shown_row, col| {
            let row = *shown.get(shown_row)? as usize;
            let c = cell(snapshot, plan, row, col)?;
            if c.attribution == Attribution::DeterminedNonAdditive {
                any_determined = true;
            }
            Some(c)
        });
        self.any_determined = any_determined;
    }
}

impl TableDelegate for BlotterDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.plan.as_ref().map_or(0, |p| p.columns.len())
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.shown.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(c) = self.plan.as_ref().and_then(|p| p.columns.get(col_ix)) else {
            return Column::default();
        };
        let sort = match self.sort {
            Some(s) if s.column == col_ix && s.descending => Some(ColumnSort::Descending),
            Some(s) if s.column == col_ix => Some(ColumnSort::Ascending),
            _ => Some(ColumnSort::Default),
        };
        let mut label = c.label.clone();
        if !c.semi_joined.is_empty() {
            label.push_str(" ⋈");
        }
        Column {
            key: SharedString::from(c.name.clone()),
            name: SharedString::from(label),
            align: if c.kind == ColumnKind::Measure { TextAlign::Right } else { TextAlign::Left },
            sort: if c.kind == ColumnKind::Tree { None } else { sort },
            width: px(c.width),
            movable: c.kind != ColumnKind::Tree,
            ..Column::default()
        }
    }

    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _window: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.sort = match sort {
            ColumnSort::Default => None,
            ColumnSort::Ascending => Some(SortSpec { column: col_ix, descending: false }),
            ColumnSort::Descending => Some(SortSpec { column: col_ix, descending: true }),
        };
        self.reflatten();
        cx.notify();
    }

    fn move_column(&mut self, col_ix: usize, to_ix: usize, _window: &mut Window, cx: &mut Context<TableState<Self>>) {
        if let Some(p) = self.plan.as_mut() {
            p.move_column(col_ix, to_ix);
        }
        self.cache.invalidate();
        cx.notify();
    }

    fn visible_rows_changed(&mut self, visible_range: Range<usize>, _window: &mut Window, _cx: &mut Context<TableState<Self>>) {
        self.refill_window(visible_range);
    }

    fn render_tr(&mut self, row_ix: usize, _window: &mut Window, cx: &mut Context<TableState<Self>>) -> Stateful<Div> {
        let range = selection(&self.mode, &self.cursor);
        let in_visual = matches!(self.mode, Mode::Visual { .. }) && range.contains(&row_ix);
        div()
            .id(("row", row_ix))
            .when(in_visual, |el| el.bg(cx.theme().selection.opacity(0.35)))
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _window: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let theme = cx.theme();
        let is_cursor = self.cursor.row == row_ix && self.cursor.col == col_ix;
        let kind = self.plan.as_ref().and_then(|p| p.columns.get(col_ix)).map(|c| c.kind);
        let colour = self.plan.as_ref().and_then(|p| p.columns.get(col_ix)).map(|c| c.format.colour);
        let mut el = div()
            .size_full()
            .flex()
            .items_center()
            .px_1()
            .font_family(fonts::MONO)
            .when(kind == Some(ColumnKind::Measure), |el| el.justify_end())
            .when(is_cursor, |el| el.border_1().border_color(theme.table_active_border));

        // The tree column: indent and a disclosure glyph, then the text.
        if kind == Some(ColumnKind::Tree)
            && let (Some(snapshot), Some(&row)) = (&self.snapshot, self.shown.get(row_ix))
        {
            let tree = snapshot.tree();
            let row = row as usize;
            let depth = tree.depth(row);
            let could = depth < snapshot.grouping_len();
            let glyph = if !could {
                "·"
            } else if tree.has_children(row) {
                let open = self.plan.as_ref().is_some_and(|p| self.expansion.is_open(&path_of(snapshot, p, row)));
                if open { "▾" } else { "▸" }
            } else if self.plan.as_ref().is_some_and(|p| self.expansion.is_open(&path_of(snapshot, p, row))) {
                "…" // open, not yet materialised: a requery is in flight
            } else {
                "▸"
            };
            el = el
                .pl(px(depth as f32 * INDENT))
                .child(div().w(px(14.)).text_color(theme.muted_foreground).child(glyph));
        }

        let Some(cell) = self.cache.get(row_ix, col_ix) else {
            return el; // blank: NULL, NonAttributable, or not yet cached
        };
        let text: SharedString = SharedString::from(cell.text.to_string());
        match cell.attribution {
            Attribution::NonAttributable => el, // never a number here
            Attribution::DeterminedNonAdditive => el
                .text_color(theme.muted_foreground)
                .child(text)
                .child(div().pl_1().child(DETERMINED_MARK)),
            Attribution::Additive => {
                let el = match (colour, cell.sign) {
                    (Some(Colour::Sign), Some(Sign::Negative)) => el.text_color(theme.chart_bearish),
                    (Some(Colour::Sign), Some(Sign::Positive)) => el.text_color(theme.chart_bullish),
                    _ => el,
                };
                el.child(text)
            }
        }
    }
}
```

`SharedString::from(cell.text.to_string())` allocates per painted cell
per frame; replace with `SharedString::from(Arc::clone(&cell.text))` if
the pinned gpui's `SharedString` has a `From<Arc<str>>` (it does at
e3adf43 — check `shared_string.rs`), so the paint path stays
allocation-free.

- [ ] **Step 4: Run, check, commit**

```bash
git add crates/geode-blotter
git commit -m "feat(blotter): the DataTable delegate

Owns snapshot, plan, expansion, flattened and shown rows, cursor, mode,
sort and the format cache; render_td is a cache lookup with the tree
column's indent and glyph, sign colours, the dagger, and a blank for a
NonAttributable cell (Phase 3 §6.6).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

- [ ] **Step 5: Harness entries** (package `geode-blotter`)

```sh
run_mutation "delegate: a NonAttributable cell paints nothing" \
  crates/geode-blotter/src/delegate.rs \
  '            Attribution::NonAttributable => el, // never a number here' \
  '            Attribution::NonAttributable => el.child(text),' \
  geode-blotter

run_mutation "delegate: expansion is pruned to the new grouping" \
  crates/geode-blotter/src/delegate.rs \
  '        self.expansion.prune_to(grouping.len());' \
  '        let _ = grouping.len();' \
  geode-blotter
```

The first entry needs a test that can see it: Task 7's
`a_non_attributable_cell_has_no_text_element` uses a cell
`debug_selector` — add `.debug_selector(move || format!("cell-{row_ix}-{col_ix}"))`
to `el` in `render_td` and, in the `NonAttributable` arm's test, assert
the cell's painted bounds exist but the `painted_quads` count does not
grow when compared to an empty cell — or, simpler and honest: keep the
first entry's test at the cache level (`cells_honour_the_read_paths_opinions`)
and mutate `cache.rs` rather than `delegate.rs`. Use whichever the
harness proves; record the choice in the entry's name. Commit.

---

### Task 7: `BlotterTile`, `BlotterContent`, `BlotterFactory`

Spec §6.5, §6.7, §6.8, §3.1, §4.3. The entity per tile: observes the
frame, requeries, applies outcomes, records timing, paints the header
and footer around the table; the content and factory the shell hosts.

**Files:**
- Create: `src/tile.rs`, `src/content.rs`
- Modify: `crates/geode-shell/src/defaults.rs` (bindings only)
- Test: `src/tile.rs` `#[gpui::test]`s

**Interfaces:**
- Produces:
  ```rust
  pub enum Pin { None, Grouping(Vec<String>), Slot(u8) }
  pub struct BlotterTile { .. }
  impl BlotterTile {
      pub fn new(tile: TileId, frame: Entity<Frame>, data: DataHandle, views: Rc<RefCell<Vec<ViewSpec>>>,
                 find_style: Rc<Cell<FindStyle>>, restored: Option<&toml::Table>, window: &mut Window, cx: &mut Context<Self>) -> Self;
      pub fn dispatch(&mut self, action: &ActionId, count: Option<u32>, cx: &mut Context<Self>) -> bool;
      pub fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String>;
      pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String>;
      pub fn find(&mut self, event: FindEvent, cx: &mut Context<Self>);
      pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>);
      pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>);
      pub fn serialize(&self) -> toml::Table;
      pub fn key_context(&self, cx: &App) -> KeyContext;
      pub fn table(&self) -> &Entity<TableState<BlotterDelegate>>;
      pub fn last_query(&self) -> Option<(u64, Vec<String>)>;    // (tag, grouping) — tests
  }
  pub struct BlotterFactory { .. }
  impl BlotterFactory {
      pub fn new(data: DataHandle, views: Vec<ViewSpec>, find_style: FindStyle) -> BlotterFactory;
      pub fn set_views(&self, views: Vec<ViewSpec>);  pub fn set_find_style(&self, style: FindStyle);
  }
  pub const ACTIONS: &[(&str, &str)];   // (id, title) — blotter::down … blotter::sort_cycle
  ```

- [ ] **Step 1: Write the failing tests**

In `tile.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::groupings::GroupingSlots;
    use geode_core::query::{QueryKey, QueryOutcome};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::frame::Frame;
    use geode_shell::module::FindEvent;
    use geode_shell::tiling::TileId;
    use geode_shell::vimfind::FindStyle;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    fn views() -> Vec<ViewSpec> {
        let text = "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[tree.columns]]\nname = \"delta01\"\n[[tree.columns]]\nname = \"daily_trading_pnl\"\n[wide]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[wide.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0
    }

    fn slots() -> GroupingSlots {
        let mut s = GroupingSlots::default();
        s.set(1, vec!["lhu".into()]);
        s.set(2, vec!["underlying_ref".into(), "lhu".into()]);
        s
    }

    /// Root; L1, L2; L1/SPX. Trading PnL is NonAttributable at depth 2.
    fn snapshot() -> Arc<Snapshot> {
        let meta = |n: &str, by_depth: Vec<Attribution>| ColumnMeta { name: n.into(), attribution_by_depth: by_depth, scope_semantics: ScopeSemantics::Direct };
        Arc::new(Snapshot::for_tests(
            vec![
                (meta("lhu", vec![Attribution::Additive; 3]), TestColumn::Dict(vec![None, Some("L1".into()), Some("L2".into()), Some("L1".into())])),
                (meta("underlying_ref", vec![Attribution::Additive; 3]), TestColumn::Dict(vec![None, None, None, Some("SPX".into())])),
                (meta("row_depth", vec![Attribution::Additive; 3]), TestColumn::I32(vec![0, 1, 1, 2])),
                (meta("delta01", vec![Attribution::Additive; 3]), TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(5.0)])),
                (meta("daily_trading_pnl", vec![Attribution::Additive, Attribution::Additive, Attribution::NonAttributable]), TestColumn::F64(vec![Some(7.0), Some(7.0), None])),
            ],
            2,
        ))
    }

    struct Harness {
        tile: Entity<BlotterTile>,
        frame: Entity<Frame>,
        requests: Receiver<Request>,
    }

    fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);
        let (data, requests) = DataHandle::for_tests();
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame = cx.new(|_| Frame::new(slots(), None));
                    cx.new(|cx| {
                        let tile = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                frame.clone(),
                                data.clone(),
                                Rc::new(RefCell::new(views())),
                                Rc::new(Cell::new(FindStyle::Vim)),
                                None,
                                window,
                                cx,
                            )
                        });
                        Host { tile, frame }
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let (tile, frame) = window.root(&vcx).unwrap().read_with(&vcx, |h, _| (h.tile.clone(), h.frame.clone()));
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (Harness { tile, frame, requests }, vcx)
    }

    /// A root view for the test window that just paints the tile.
    struct Host {
        tile: Entity<BlotterTile>,
        frame: Entity<Frame>,
    }
    impl gpui::Render for Host {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.tile.clone())
        }
    }

    fn next_query(rx: &Receiver<Request>) -> geode_data::QueryParams {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("a request") {
                Request::Query(p) => return p,
                _ => continue,
            }
        }
    }

    fn deliver(h: &Harness, cx: &mut gpui::VisualTestContext, tag: u64, snapshot: Result<Arc<Snapshot>, String>) {
        h.tile.update(cx, |t, cx| {
            t.deliver(
                QueryOutcome { key: QueryKey(7), tag, snapshot, submitted: Instant::now() - Duration::from_millis(12) },
                cx,
            )
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn showing_the_tile_submits_one_query_keyed_by_the_tile_with_the_views_grouping(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        assert_eq!(p.key, QueryKey(7));
        assert_eq!(p.view, "tree");
        assert_eq!(p.grouping.as_deref(), Some(&["lhu".to_string(), "underlying_ref".into()][..]));
        assert_eq!(p.max_depth, 1, "collapsed: one level");
        assert!(h.requests.try_recv().is_err(), "exactly one");
    }

    #[gpui::test]
    fn a_frame_slot_change_requeries_once_and_a_pinned_tile_ignores_it(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.frame.update(&mut cx, |f, cx| {
            f.set_active_slot(Some(2));
            cx.notify();
        });
        let p = next_query(&h.requests);
        assert_eq!(p.grouping.as_deref(), Some(&["underlying_ref".to_string(), "lhu".into()][..]));
        assert!(h.requests.try_recv().is_err());

        h.tile.update(&mut cx, |t, cx| t.command("group lhu", cx).unwrap());
        let p = next_query(&h.requests);
        assert_eq!(p.grouping.as_deref(), Some(&["lhu".to_string()][..]), "pinned");
        h.frame.update(&mut cx, |f, cx| {
            f.set_active_slot(Some(1));
            cx.notify();
        });
        assert!(h.requests.recv_timeout(Duration::from_millis(200)).is_err(), "a pinned tile does not follow the slot");
        h.tile.update(&mut cx, |t, cx| t.command("unpin", cx).unwrap());
        let p = next_query(&h.requests);
        assert_eq!(p.grouping.as_deref(), Some(&["lhu".to_string()][..]), "rejoined slot 1");
    }

    #[gpui::test]
    fn a_stale_outcome_is_dropped_an_error_keeps_the_last_snapshot_and_timing_is_recorded(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let rows = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone());
        assert_eq!(rows, vec![0, 1, 2]);
        assert!(h.frame.read_with(&cx, |f, _| f.requery.last()).is_some(), "submit→snapshot and snapshot→paint recorded");

        deliver(&h, &mut cx, p.tag + 100, Ok(Arc::new(Snapshot::for_tests(vec![], 0))));
        let rows = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone());
        assert_eq!(rows, vec![0, 1, 2], "a stale tag changed nothing");

        h.tile.update(&mut cx, |t, cx| t.command("view wide", cx).unwrap());
        let p2 = next_query(&h.requests);
        deliver(&h, &mut cx, p2.tag, Err("binder error".into()));
        let (rows, error) = h.tile.read_with(&cx, |t, cx| (t.table().read(cx).delegate().shown.clone(), t.error.clone()));
        assert_eq!(rows, vec![0, 1, 2], "the last good snapshot stays");
        assert_eq!(error.as_deref(), Some("binder error"));
    }

    #[gpui::test]
    fn motions_expansion_and_yank(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let act = |cx: &mut gpui::VisualTestContext, id: &str, count: Option<u32>| {
            h.tile.update(cx, |t, cx| t.dispatch(&ActionId(id.into()), count, cx))
        };
        assert!(act(&mut cx, "blotter::down", Some(2)));
        assert_eq!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row), 2);
        act(&mut cx, "blotter::up", None);
        act(&mut cx, "blotter::expand", None);
        let rows = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone());
        assert_eq!(rows, vec![0, 1, 3, 2], "L1 opened; SPX is already materialised");
        assert!(h.requests.try_recv().is_err(), "no requery: the child was in hand");

        act(&mut cx, "blotter::down", None);
        act(&mut cx, "blotter::expand", None);
        let p = next_query(&h.requests);
        assert_eq!(p.max_depth, 2, "opening at the bound requeries one level deeper");

        act(&mut cx, "blotter::top", None);
        act(&mut cx, "blotter::visual", None);
        act(&mut cx, "blotter::down", Some(1));
        act(&mut cx, "blotter::yank", None);
        let clip = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(clip.as_deref(), Some("lhu / underlying_ref\tdelta01\tdaily_trading_pnl\n\t9\t7\n  L1\t5\t7\n"));
        assert!(matches!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().mode), Mode::Normal), "yank leaves visual");
        assert!(!act(&mut cx, "workspace::focus_left", None), "not ours");
    }

    #[gpui::test]
    fn find_jumps_under_vim_and_narrows_under_fzf(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        h.tile.update(&mut cx, |t, cx| t.find(FindEvent::Changed("l2".into()), cx));
        assert_eq!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row), 2);
        h.tile.update(&mut cx, |t, cx| t.find(FindEvent::Cancelled, cx));
        assert_eq!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row), 0, "back to the origin");

        h.tile.update(&mut cx, |t, _| t.find_style.set(FindStyle::Fzf));
        h.tile.update(&mut cx, |t, cx| t.find(FindEvent::Changed("l".into()), cx));
        assert_eq!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()), vec![1, 2]);
        h.tile.update(&mut cx, |t, cx| t.find(FindEvent::Committed("l".into()), cx));
        assert_eq!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()), vec![1, 2], "Enter keeps it");
        h.tile.update(&mut cx, |t, cx| t.dispatch(&ActionId("blotter::escape".into()), None, cx));
        assert_eq!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()), vec![0, 1, 2]);
    }

    #[gpui::test]
    fn scope_asof_and_sort_commands_and_completions(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.tile.update(&mut cx, |t, cx| t.command("scope lhu = 'L1'", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.scope.expression.is_some());
        let err = h.tile.update(&mut cx, |t, cx| t.command("scope lhu = ", cx)).unwrap_err();
        assert!(err.contains("at column"), "{err}");
        h.tile.update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, geode_core::query::AsOf::At(_)));
        h.tile.update(&mut cx, |t, cx| t.command("live", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live());
        h.tile.update(&mut cx, |t, cx| t.command("scope undo", cx).unwrap());
        let _ = next_query(&h.requests);

        let err = h.tile.update(&mut cx, |t, cx| t.command("sort nonesuch", cx)).unwrap_err();
        assert!(err.contains("nonesuch"));
        let words = h.tile.read_with(&cx, |t, cx| t.completions("sort ", 5, cx));
        assert_eq!(words, vec!["clear", "daily_trading_pnl", "delta01"]);
        let words = h.tile.read_with(&cx, |t, cx| t.completions("view ", 5, cx));
        assert_eq!(words, vec!["tree", "wide"]);
        let state = h.tile.read_with(&cx, |t, _| t.serialize());
        assert_eq!(state["view"].as_str(), Some("tree"));
        assert_eq!(state["unscoped"].as_bool(), Some(false));
    }

    #[gpui::test]
    fn the_tile_paints_and_a_row_click_moves_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        assert!(cx.debug_bounds("tile-content-7").is_some());
        assert!(cx.debug_bounds("blotter-header-7").is_some());
        let table = h.tile.read_with(&cx, |t, _| t.table().clone());
        table.update(&mut cx, |t, cx| t.set_selected_row(2, cx));
        assert_eq!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row), 2);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement `tile.rs`**

```rust
//! One blotter tile (Phase 3 spec §6.5, §6.7, §6.8): observes the frame,
//! submits keyed queries through `DataHandle`, applies outcomes, records
//! timing, and paints the header strip, the table, and the footer.

use crate::core::commands::{Command, Vocabulary, completions, parse, parse_as_of};
use crate::core::cursor::{Mode, selection};
use crate::core::find::FindState;
use crate::core::flatten::SortSpec;
use crate::core::plan::ColumnKind;
use crate::core::yank::tsv;
use crate::delegate::BlotterDelegate;
use geode_core::groupings::GroupingSlots;
use geode_core::query::{AsOf, QueryKey, QueryOutcome};
use geode_core::scope::{Scope, parse_expr};
use geode_core::view::ViewSpec;
use geode_data::{DataHandle, QueryParams};
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::FindEvent;
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, FindStyle};
use geode_shell::vimnav::NavCommand;
use gpui::prelude::*;
use gpui::{App, ClipboardItem, Context, Entity, IntoElement, Window, div, px};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, h_flex, v_flex};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// After this long without a result the header shows an in-flight glyph
/// (foundation §7.1's 50–200 ms affordance).
const IN_FLIGHT_AFTER: Duration = Duration::from_millis(50);

pub const ACTIONS: &[(&str, &str)] = &[
    ("blotter::down", "Cursor down"),
    ("blotter::up", "Cursor up"),
    ("blotter::left", "Cursor left"),
    ("blotter::right", "Cursor right"),
    ("blotter::top", "Cursor to top"),
    ("blotter::bottom", "Cursor to bottom"),
    ("blotter::page_down", "Half page down"),
    ("blotter::page_up", "Half page up"),
    ("blotter::first_col", "First column"),
    ("blotter::last_col", "Last column"),
    ("blotter::expand", "Expand node"),
    ("blotter::collapse", "Collapse node"),
    ("blotter::toggle", "Toggle node"),
    ("blotter::expand_all", "Expand all"),
    ("blotter::collapse_all", "Collapse all"),
    ("blotter::visual", "Visual mode"),
    ("blotter::escape", "Leave visual / clear narrowing"),
    ("blotter::yank", "Yank rows as TSV"),
    ("blotter::find_next", "Next match"),
    ("blotter::find_prev", "Previous match"),
    ("blotter::sort_cycle", "Sort by cursor column"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    None,
    Grouping(Vec<String>),
    Slot(u8),
}

pub struct BlotterTile {
    tile: TileId,
    frame: Entity<Frame>,
    data: DataHandle,
    views: Rc<RefCell<Vec<ViewSpec>>>,
    pub find_style: Rc<Cell<FindStyle>>,
    table: Entity<TableState<BlotterDelegate>>,
    view_name: String,
    pin: Pin,
    unscoped: bool,
    tile_scope: Scope,
    /// The frame versions last acted on; `None` until the first query.
    acted: Option<FrameVersions>,
    tag: u64,
    last_grouping: Vec<String>,
    in_flight: Option<Instant>,
    delivered_at: Option<Instant>,
    visible: bool,
    pub error: Option<String>,
    find: Option<FindState>,
}

impl BlotterTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        frame: Entity<Frame>,
        data: DataHandle,
        views: Rc<RefCell<Vec<ViewSpec>>>,
        find_style: Rc<Cell<FindStyle>>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view_name = restored
            .and_then(|t| t.get("view").and_then(|v| v.as_str()).map(str::to_string))
            .filter(|n| views.borrow().iter().any(|v| &v.name == n))
            .or_else(|| views.borrow().first().map(|v| v.name.clone()))
            .unwrap_or_default();
        let pin = match restored {
            Some(t) if t.get("pinned_slot").and_then(|v| v.as_integer()).is_some() => {
                Pin::Slot(t["pinned_slot"].as_integer().unwrap() as u8)
            }
            Some(t) if t.get("pinned").and_then(|v| v.as_array()).is_some() => Pin::Grouping(
                t["pinned"].as_array().unwrap().iter().filter_map(|v| v.as_str()).map(str::to_string).collect(),
            ),
            _ => Pin::None,
        };
        let unscoped = restored.and_then(|t| t.get("unscoped").and_then(|v| v.as_bool())).unwrap_or(false);

        let table = cx.new(|cx| {
            TableState::new(BlotterDelegate::new(), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .loop_selection(false)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
        });
        cx.subscribe(&table, |this, _, event: &TableEvent, cx| {
            if let TableEvent::SelectRow(row) = event {
                this.table.update(cx, |t, _| {
                    let d = t.delegate_mut();
                    d.cursor.to_row(*row, d.shown.len());
                });
                cx.notify();
            }
        })
        .detach();
        cx.observe(&frame, |this, _, cx| this.on_frame_changed(cx)).detach();

        BlotterTile {
            tile,
            frame,
            data,
            views,
            find_style,
            table,
            view_name,
            pin,
            unscoped,
            tile_scope: Scope::default(),
            acted: None,
            tag: 0,
            last_grouping: Vec::new(),
            in_flight: None,
            delivered_at: None,
            visible: false,
            error: None,
            find: None,
        }
    }

    pub fn table(&self) -> &Entity<TableState<BlotterDelegate>> {
        &self.table
    }

    pub fn last_query(&self) -> Option<(u64, Vec<String>)> {
        (self.tag > 0).then(|| (self.tag, self.last_grouping.clone()))
    }

    fn view(&self) -> Option<ViewSpec> {
        self.views.borrow().iter().find(|v| v.name == self.view_name).cloned()
    }

    fn grouping(&self, frame: &Frame, view: &ViewSpec) -> Vec<String> {
        match &self.pin {
            Pin::Grouping(g) => g.clone(),
            Pin::Slot(n) => frame.slots().get(*n).map(<[String]>::to_vec).unwrap_or_else(|| view.grouping.clone()),
            Pin::None => frame.active_grouping().map(<[String]>::to_vec).unwrap_or_else(|| view.grouping.clone()),
        }
    }

    /// Which counters this tile follows (§4.1).
    fn follows_changed(&self, now: FrameVersions) -> bool {
        let Some(acted) = self.acted else {
            return true;
        };
        (!self.unscoped && acted.scope != now.scope)
            || (self.pin == Pin::None && acted.grouping != now.grouping)
            || acted.as_of != now.as_of
            || acted.data != now.data
            || acted.config != now.config
    }

    fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let now = self.frame.read(cx).versions();
        if self.follows_changed(now) {
            self.requery(cx);
        }
        cx.notify();
    }

    fn requery(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.view() else {
            self.error = Some(format!("view '{}' is not configured", self.view_name));
            cx.notify();
            return;
        };
        let (grouping, scope, as_of, versions) = {
            let frame = self.frame.read(cx);
            let grouping = self.grouping(frame, &view);
            let scope = if self.unscoped { self.tile_scope.clone() } else { frame.effective_scope(&self.tile_scope) };
            (grouping, scope, frame.as_of().clone(), frame.versions())
        };
        let max_depth = self.table.update(cx, |t, _| {
            let d = t.delegate_mut();
            d.expansion.prune_to(grouping.len());
            d.depth_bound(grouping.len()).max(1)
        });
        self.tag += 1;
        let submitted = Instant::now();
        self.in_flight = Some(submitted);
        self.acted = Some(versions);
        self.last_grouping = grouping.clone();
        let queued = self.data.query(QueryParams {
            key: QueryKey(self.tile.0),
            tag: self.tag,
            submitted,
            view: self.view_name.clone(),
            grouping: Some(grouping),
            scope,
            as_of,
            max_depth,
        });
        if !queued {
            self.error = Some("query refused: the data service is busy or gone".into());
            self.in_flight = None;
        }
        // Repaint once the in-flight affordance is due, if still waiting.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(IN_FLIGHT_AFTER + Duration::from_millis(10)).await;
            let _ = this.update(cx, |t, cx| {
                if t.in_flight.is_some() {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        if outcome.tag != self.tag {
            return; // stale: a newer request is out
        }
        self.in_flight = None;
        let micros = outcome.submitted.elapsed().as_micros() as u64;
        self.frame.update(cx, |f, _| f.requery.record_submit_to_snapshot(micros));
        match outcome.snapshot {
            Ok(snapshot) => {
                self.error = None;
                if let Some(view) = self.view() {
                    let grouping = self.last_grouping.clone();
                    self.table.update(cx, |t, cx| {
                        t.delegate_mut().apply_snapshot(snapshot, &view, &grouping);
                        t.refresh(cx);
                        let row = t.delegate().cursor.row;
                        t.set_selected_row(row, cx);
                    });
                }
                self.delivered_at = Some(Instant::now());
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible {
            let now = self.frame.read(cx).versions();
            if self.follows_changed(now) {
                self.requery(cx);
            }
        }
    }

    pub fn key_context(&self, cx: &App) -> KeyContext {
        let mode = match self.table.read(cx).delegate().mode {
            Mode::Normal => "normal",
            Mode::Visual { .. } => "visual",
        };
        KeyContext::new("blotter").pair("mode", mode).counts()
    }

    fn with_delegate<R>(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut BlotterDelegate) -> R) -> R {
        self.table.update(cx, |t, _| f(t.delegate_mut()))
    }

    fn sync_cursor(&self, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            let (row, col) = (t.delegate().cursor.row, t.delegate().cursor.col);
            t.set_selected_row(row, cx);
            t.scroll_to_row(row, cx);
            t.scroll_to_col(col, cx);
        });
    }

    pub fn dispatch(&mut self, action: &ActionId, count: Option<u32>, cx: &mut Context<Self>) -> bool {
        let Some(name) = action.0.strip_prefix("blotter::") else {
            return false;
        };
        let grouping_len = self.last_grouping.len();
        match name {
            "down" | "up" | "top" | "bottom" | "page_down" | "page_up" => {
                let cmd = match name {
                    "down" => NavCommand::Move(1),
                    "up" => NavCommand::Move(-1),
                    "top" => NavCommand::Top,
                    "bottom" => NavCommand::Bottom,
                    "page_down" => NavCommand::Move(5),
                    _ => NavCommand::Move(-5),
                };
                self.with_delegate(cx, |d| {
                    let len = d.shown.len();
                    d.cursor.move_rows(len, cmd, count);
                });
                self.sync_cursor(cx);
            }
            "left" | "right" | "first_col" | "last_col" => {
                self.with_delegate(cx, |d| {
                    let cols = d.plan.as_ref().map_or(0, |p| p.columns.len());
                    match name {
                        "left" => d.cursor.move_cols(cols, -1, count),
                        "right" => d.cursor.move_cols(cols, 1, count),
                        "first_col" => d.cursor.col = 0,
                        _ => d.cursor.col = cols.saturating_sub(1),
                    }
                });
                self.sync_cursor(cx);
            }
            "expand" | "collapse" | "toggle" => {
                let open = match name {
                    "expand" => Some(true),
                    "collapse" => Some(false),
                    _ => None,
                };
                let n = count.unwrap_or(1).max(1);
                let needs_depth = self.with_delegate(cx, |d| {
                    for _ in 0..n {
                        d.expand_cursor(open);
                    }
                    d.cursor_needs_more_depth(grouping_len)
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
                self.sync_cursor(cx);
                if needs_depth {
                    self.requery(cx);
                }
            }
            "expand_all" | "collapse_all" => {
                let needs_depth = self.with_delegate(cx, |d| {
                    if name == "expand_all" {
                        d.expansion.open_all();
                    } else {
                        d.expansion.close_all();
                    }
                    d.reflatten();
                    d.depth_bound(grouping_len) > d.snapshot.as_ref().map_or(0, |s| s.grouping_len().min(d.expansion.deepest_open_depth().saturating_add(1)))
                        || name == "expand_all"
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
                self.sync_cursor(cx);
                if needs_depth {
                    self.requery(cx);
                }
            }
            "visual" => {
                self.with_delegate(cx, |d| {
                    d.mode = match d.mode {
                        Mode::Normal => Mode::Visual { anchor: d.cursor.row },
                        Mode::Visual { .. } => Mode::Normal,
                    };
                });
            }
            "escape" => {
                self.with_delegate(cx, |d| {
                    d.mode = Mode::Normal;
                    if d.narrowed.is_some() {
                        d.set_narrowed(None);
                    }
                });
                self.find = None;
                self.table.update(cx, |t, cx| t.refresh(cx));
            }
            "yank" => {
                let text = self.with_delegate(cx, |d| {
                    let (Some(snapshot), Some(plan)) = (&d.snapshot, &d.plan) else {
                        return None;
                    };
                    let range = selection(&d.mode, &d.cursor);
                    let out = tsv(snapshot, plan, &d.shown, range);
                    d.mode = Mode::Normal;
                    Some(out)
                });
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            "find_next" | "find_prev" => {
                let dir = if name == "find_next" { FindDirection::Forward } else { FindDirection::Backward };
                if let Some(find) = &self.find {
                    let (texts, from) = self.with_delegate(cx, |d| (d.shown_texts(), d.cursor.row));
                    if let Some(row) = find.repeat(&texts, from, dir, count) {
                        self.with_delegate(cx, |d| {
                            let len = d.shown.len();
                            d.cursor.to_row(row, len);
                        });
                        self.sync_cursor(cx);
                    }
                }
            }
            "sort_cycle" => {
                self.with_delegate(cx, |d| {
                    let col = d.cursor.col;
                    if col == 0 {
                        return;
                    }
                    d.sort = match d.sort {
                        Some(s) if s.column == col && !s.descending => Some(SortSpec { column: col, descending: true }),
                        Some(s) if s.column == col => None,
                        _ => Some(SortSpec { column: col, descending: false }),
                    };
                    d.reflatten();
                });
                self.table.update(cx, |t, cx| {
                    t.refresh_header_layout(cx);
                    t.refresh(cx);
                });
                self.sync_cursor(cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    pub fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        match parse(line)? {
            Command::Group(g) => {
                self.pin = Pin::Grouping(g);
                self.requery(cx);
            }
            Command::GroupSlot(n) => {
                if self.frame.read(cx).slots().get(n).is_none() {
                    return Err(format!("slot {n} is empty"));
                }
                self.pin = Pin::Slot(n);
                self.requery(cx);
            }
            Command::GroupSave(n) => {
                let grouping = self.last_grouping.clone();
                if grouping.is_empty() {
                    return Err("nothing grouped yet".into());
                }
                let result = self.frame.update(cx, |f, cx| {
                    let r = f.save_slot(n, grouping);
                    cx.notify();
                    r
                });
                result?;
            }
            Command::Unpin => {
                self.pin = Pin::None;
                self.requery(cx);
            }
            Command::Unscoped => {
                self.unscoped = !self.unscoped;
                self.requery(cx);
            }
            Command::ScopeExpr(text) => {
                let expr = parse_expr(&text).map_err(|e| format!("{} at column {}", e.message, e.caret + 1))?;
                self.frame.update(cx, |f, cx| {
                    let mut scope = f.scope().clone();
                    scope.expression = Some(expr);
                    if f.set_scope(scope) {
                        cx.notify();
                    }
                });
            }
            Command::ScopeText(words) => {
                self.frame.update(cx, |f, cx| {
                    let mut scope = f.scope().clone();
                    scope.text = (!words.is_empty()).then_some(words);
                    if f.set_scope(scope) {
                        cx.notify();
                    }
                });
            }
            Command::ScopeClear => {
                self.frame.update(cx, |f, cx| {
                    if f.clear_scope() {
                        cx.notify();
                    }
                });
            }
            Command::ScopeUndo => {
                let undone = self.frame.update(cx, |f, cx| {
                    let r = f.undo_scope();
                    cx.notify();
                    r
                });
                if !undone {
                    return Err("nothing to undo".into());
                }
            }
            Command::AsOf(text) => {
                let at = parse_as_of(&text, chrono::Utc::now())?;
                self.frame.update(cx, |f, cx| {
                    if f.set_as_of(AsOf::At(at)) {
                        cx.notify();
                    }
                });
            }
            Command::Live => {
                self.frame.update(cx, |f, cx| {
                    if f.set_as_of(AsOf::Live) {
                        cx.notify();
                    }
                });
            }
            Command::View(name) => {
                if !self.views.borrow().iter().any(|v| v.name == name) {
                    return Err(format!("no view named '{name}'"));
                }
                self.view_name = name;
                self.with_delegate(cx, |d| d.plan = None);
                self.requery(cx);
            }
            Command::Sort { column, descending } => {
                let found = self.with_delegate(cx, |d| {
                    let col = d.plan.as_ref()?.columns.iter().position(|c| c.name == column)?;
                    d.sort = Some(SortSpec { column: col, descending });
                    d.reflatten();
                    Some(())
                });
                if found.is_none() {
                    return Err(format!("no column named '{column}' in this view"));
                }
                self.table.update(cx, |t, cx| {
                    t.refresh_header_layout(cx);
                    t.refresh(cx);
                });
            }
            Command::SortClear => {
                self.with_delegate(cx, |d| {
                    d.sort = None;
                    d.reflatten();
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
            }
        }
        cx.notify();
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        let columns = self
            .table
            .read(cx)
            .delegate()
            .plan
            .as_ref()
            .map(|p| p.columns.iter().filter(|c| c.kind != ColumnKind::Tree).map(|c| c.name.clone()).collect())
            .unwrap_or_default();
        let views = self.views.borrow().iter().map(|v| v.name.clone()).collect();
        completions(line, cursor, &Vocabulary { columns, views })
    }

    pub fn find(&mut self, event: FindEvent, cx: &mut Context<Self>) {
        match event {
            FindEvent::Changed(query) => {
                if self.find.is_none() {
                    let origin = self.table.read(cx).delegate().cursor.row;
                    self.find = Some(FindState::begin(self.find_style.get(), origin));
                }
                let style = self.find_style.get();
                let texts = self.table.read(cx).delegate().shown_texts();
                let find = self.find.as_mut().unwrap();
                let hit = find.changed(&texts, &query);
                let narrowed = find.narrowed.clone();
                self.with_delegate(cx, |d| {
                    if style == FindStyle::Fzf {
                        d.set_narrowed(narrowed);
                    }
                    if let Some(row) = hit {
                        let len = d.shown.len();
                        d.cursor.to_row(row, len);
                    }
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
                self.sync_cursor(cx);
            }
            FindEvent::Committed(query) => {
                if let Some(find) = self.find.as_mut() {
                    find.committed(&query);
                }
            }
            FindEvent::Cancelled => {
                if let Some(mut find) = self.find.take() {
                    let origin = find.cancelled();
                    self.with_delegate(cx, |d| {
                        d.set_narrowed(None);
                        let len = d.shown.len();
                        d.cursor.to_row(origin, len);
                    });
                    self.table.update(cx, |t, cx| t.refresh(cx));
                    self.sync_cursor(cx);
                }
            }
        }
        cx.notify();
    }

    pub fn serialize(&self) -> toml::Table {
        let mut t = toml::Table::new();
        t.insert("view".into(), toml::Value::String(self.view_name.clone()));
        match &self.pin {
            Pin::None => {}
            Pin::Grouping(g) => {
                t.insert("pinned".into(), toml::Value::Array(g.iter().map(|s| toml::Value::String(s.clone())).collect()));
            }
            Pin::Slot(n) => {
                t.insert("pinned_slot".into(), toml::Value::Integer(*n as i64));
            }
        }
        t.insert("unscoped".into(), toml::Value::Boolean(self.unscoped));
        t
    }
}

impl gpui::Render for BlotterTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The paint half of §7.1 (§6.8): the first render after a delivery.
        if let Some(at) = self.delivered_at.take() {
            let micros = at.elapsed().as_micros() as u64;
            self.frame.update(cx, |f, _| f.requery.record_snapshot_to_paint(micros));
        }
        let theme = cx.theme();
        let frame = self.frame.read(cx);
        let stale_after = Duration::from_secs(15 * 60);
        let delegate = self.table.read(cx).delegate();
        let snapshot = delegate.snapshot.clone();

        // Header strip: view · grouping · markers · freshness · AS OF · … · error
        let mut header = h_flex()
            .w_full()
            .h(px(22.))
            .items_center()
            .gap_3()
            .px_2()
            .text_sm()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .debug_selector(|| format!("blotter-header-{}", self.tile.0))
            .child(div().text_color(theme.foreground).child(self.view_name.clone()))
            .child(div().child(GroupingSlots::label_of(&self.last_grouping)));
        match &self.pin {
            Pin::None => {}
            _ => header = header.child(div().text_color(theme.warning_foreground).bg(theme.warning.opacity(0.25)).px_1().rounded(px(3.)).child("pinned")),
        }
        if self.unscoped {
            header = header.child(div().text_color(theme.warning_foreground).bg(theme.warning.opacity(0.25)).px_1().rounded(px(3.)).child("unscoped"));
        }
        if let Some(snapshot) = &snapshot {
            let p = snapshot.provenance();
            let mut datasets: Vec<_> = p.datasets.iter().collect();
            datasets.sort_by(|a, b| a.as_of.cmp(&b.as_of));
            let now = chrono::Utc::now();
            for f in datasets {
                let text = match &f.as_of {
                    Some(t) => format!("{} {}", f.dataset, &t[11..16.min(t.len())]),
                    None => format!("{} —", f.dataset),
                };
                let stale = f
                    .as_of
                    .as_deref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| now.signed_duration_since(t.with_timezone(&chrono::Utc)).to_std().unwrap_or_default() > stale_after);
                header = header.child(div().when(stale, |el| el.text_color(theme.warning)).child(text));
            }
            if let Some(req) = &p.as_of_request {
                header = header.child(div().text_color(theme.warning_foreground).bg(theme.warning.opacity(0.4)).px_1().rounded(px(3.)).child(format!("AS OF {}", &req[..16.min(req.len())])));
            }
        }
        if self.in_flight.is_some_and(|t| t.elapsed() > IN_FLIGHT_AFTER) {
            header = header.child(div().child("…"));
        }
        if let Some(e) = &self.error {
            header = header.child(div().text_color(theme.danger).child(e.clone()));
        }

        // Footer: counts and legends.
        let mut footer = h_flex()
            .w_full()
            .h(px(20.))
            .items_center()
            .gap_4()
            .px_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .border_t_1()
            .border_color(theme.border)
            .child(div().child(format!("{} rows", delegate.shown.len())));
        if delegate.any_determined {
            footer = footer.child(div().child("† shown for this row, do not total"));
        }
        if !delegate.semi_joined.is_empty() {
            footer = footer.child(div().child(format!(
                "⋈ scoped by membership on {}: whole entities that qualify, not their share",
                delegate.semi_joined.join(", ")
            )));
        }
        if delegate.unplaced > 0 {
            footer = footer.child(div().text_color(theme.warning).child(format!("{} rows unplaced", delegate.unplaced)));
        }
        let _ = frame;

        v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.tile.0))
            .child(header)
            .child(div().flex_1().min_h_0().w_full().child(DataTable::new(&self.table).with_size(Size::XSmall).bordered(false).stripe(false)))
            .child(footer)
    }
}
```

The `expand_all` requery condition above is convoluted; replace it with
the plain rule: `expand_all` always requeries with the full depth
(`depth_bound` returns `grouping_len` under `open_all`), `collapse_all`
never does. Write it that way.

- [ ] **Step 4: Implement `content.rs`**

```rust
//! What the shell hosts (Phase 3 spec §3.1, §3.2): the `TileContent`
//! wrapper over a `BlotterTile` entity, and the factory the app puts in
//! the roster. The factory carries the data handle (§2.1); the shell
//! never sees it.

use crate::tile::{ACTIONS, BlotterTile};
use geode_core::query::QueryOutcome;
use geode_core::view::ViewSpec;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, ModuleFactory, TileContent, TileOccupant};
use geode_shell::tiling::TileId;
use geode_shell::vimfind::FindStyle;
use gpui::{App, Entity, Window};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct BlotterContent {
    tile: Entity<BlotterTile>,
}

impl TileContent for BlotterContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context(cx)
    }
    fn dispatch(&self, action: &ActionId, count: Option<u32>, _window: &mut Window, cx: &mut App) -> bool {
        self.tile.update(cx, |t, cx| t.dispatch(action, count, cx))
    }
    fn command(&self, line: &str, _window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, cx))
    }
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor, cx)
    }
    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }
    fn deliver(&self, outcome: QueryOutcome, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.deliver(outcome, cx))
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize()
    }
}

pub struct BlotterFactory {
    data: DataHandle,
    views: Rc<RefCell<Vec<ViewSpec>>>,
    find_style: Rc<Cell<FindStyle>>,
}

impl BlotterFactory {
    pub fn new(data: DataHandle, views: Vec<ViewSpec>, find_style: FindStyle) -> BlotterFactory {
        BlotterFactory {
            data,
            views: Rc::new(RefCell::new(views)),
            find_style: Rc::new(Cell::new(find_style)),
        }
    }

    /// A safe reload (foundation §8): every tile sees the new set on its
    /// next requery, which the frame's config counter triggers.
    pub fn set_views(&self, views: Vec<ViewSpec>) {
        *self.views.borrow_mut() = views;
    }

    pub fn set_find_style(&self, style: FindStyle) {
        self.find_style.set(style);
    }
}

impl ModuleFactory for BlotterFactory {
    fn kind(&self) -> &'static str {
        "blotter"
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Blotter".to_string(),
            });
        }
    }

    fn create(&self, tile: TileId, restored: Option<&toml::Table>, frame: Entity<Frame>, window: &mut Window, cx: &mut App) -> TileOccupant {
        let entity = cx.new(|cx| {
            BlotterTile::new(tile, frame, self.data.clone(), self.views.clone(), self.find_style.clone(), restored, window, cx)
        });
        TileOccupant {
            kind: "blotter",
            view: entity.clone().into(),
            content: Box::new(BlotterContent { tile: entity }),
        }
    }
}
```

- [ ] **Step 5: Bindings**

In `crates/geode-shell/src/defaults.rs`, append to `BUILTIN_KEYMAP`:

```toml
[[bindings]]
context = "blotter && mode == normal"
[bindings.keys]
"j" = "blotter::down"
"k" = "blotter::up"
"h" = "blotter::left"
"l" = "blotter::right"
"g g" = "blotter::top"
"shift+g" = "blotter::bottom"
"ctrl+d" = "blotter::page_down"
"ctrl+u" = "blotter::page_up"
"home" = "blotter::first_col"
"end" = "blotter::last_col"
"z o" = "blotter::expand"
"z c" = "blotter::collapse"
"z a" = "blotter::toggle"
"z shift+r" = "blotter::expand_all"
"z shift+m" = "blotter::collapse_all"
"enter" = "blotter::toggle"
"v" = "blotter::visual"
"y" = "blotter::yank"
"n" = "blotter::find_next"
"shift+n" = "blotter::find_prev"
"s" = "blotter::sort_cycle"
"escape" = "blotter::escape"

[[bindings]]
context = "blotter && mode == visual"
[bindings.keys]
"j" = "blotter::down"
"k" = "blotter::up"
"g g" = "blotter::top"
"shift+g" = "blotter::bottom"
"ctrl+d" = "blotter::page_down"
"ctrl+u" = "blotter::page_up"
"y" = "blotter::yank"
"v" = "blotter::escape"
"escape" = "blotter::escape"
```

These bindings name actions the shell does not register; `build_keymap`
drops unregistered bindings *with a diagnostic*. The shell's own tests
build the keymap from the builtin doc with the builtin registry and
assert `diags.is_empty()` — so `register_builtin_actions` must also
register the blotter's ids as *reserved* names, or the diagnostic must
be downgraded. Do the first: in `defaults.rs`, register each
`blotter::*` id from a `BLOTTER_ACTIONS` const list mirrored from
`geode_blotter::tile::ACTIONS` (the shell cannot import the blotter
crate) with category "Blotter", and make `BlotterFactory::register_actions`
tolerate `Err` (it already ignores the result). Add a test in
`geode-blotter` that the two lists are identical:

```rust
    #[test]
    fn the_shells_reserved_blotter_actions_match_ours() {
        let ours: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ours, geode_shell::defaults::BLOTTER_ACTIONS.to_vec());
    }
```

- [ ] **Step 6: Run, check, commit**

Run: `cargo test -p geode-blotter && cargo test -p geode-shell`

```bash
git add crates/geode-blotter crates/geode-shell/src/defaults.rs
git commit -m "feat(blotter): the tile entity, hosted content, factory and bindings

Observes the frame's counters it follows, submits keyed queries with the
frame's or pinned grouping and the depth bound, drops stale outcomes,
keeps the last good snapshot on error, records both halves of the §7.1
timing, paints header, table and footer, and implements every :
command (Phase 3 §6.5–§6.8, §4.3, §3).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

- [ ] **Step 7: Harness entries** (package `geode-blotter`)

```sh
run_mutation "tile: a stale tag is dropped" \
  crates/geode-blotter/src/tile.rs \
  '        if outcome.tag != self.tag {' \
  '        if false {' \
  geode-blotter

run_mutation "tile: a pinned tile ignores the slot" \
  crates/geode-blotter/src/tile.rs \
  '            || (self.pin == Pin::None && acted.grouping != now.grouping)' \
  '            || acted.grouping != now.grouping' \
  geode-blotter

run_mutation "tile: the depth bound is requested, not everything" \
  crates/geode-blotter/src/tile.rs \
  '            d.depth_bound(grouping.len()).max(1)' \
  '            usize::MAX' \
  geode-blotter

run_mutation "tile: a query error keeps the last snapshot" \
  crates/geode-blotter/src/tile.rs \
  '            Err(e) => self.error = Some(e),' \
  '            Err(e) => { self.error = Some(e); self.table.update(cx, |t, _| *t.delegate_mut() = BlotterDelegate::new()); }' \
  geode-blotter
```

Run: `zsh scripts/mutation-check.sh "tile:"`. Commit.

---

### Task 8: `geode-app` — the data bridge, the roster, the database path, `--demo`

Spec §5.1 (the bridge), §5.4, §7.1, §9.1's roster. The one crate where
shell and data meet.

**Files:**
- Create: `crates/geode-app/src/bridge.rs`, `crates/geode-app/src/demo.rs`
- Modify: `crates/geode-app/src/main.rs`, `crates/geode-app/Cargo.toml`
- Create: `examples/demo-config/{datasets,views,dimensions,groupings,app}.toml`
- Test: `bridge.rs` and `demo.rs` inline (pure parts: config assembly,
  path resolution, the demo layer's text); the end-to-end run is manual
  (§1.2)

**Interfaces:**
- Produces:
  ```rust
  // bridge.rs
  pub struct DataSetup { pub config: DataServiceConfig, pub views: Vec<ViewSpec>, pub dimensions: DerivedDimensions, pub diagnostics: Vec<Diagnostic> }
  pub fn data_setup(config: &Config, db_path: PathBuf) -> Option<DataSetup>;  // None when no datasets/views
  pub fn db_path(config: &Config, demo: Option<&Path>, local_app_data: Option<String>, home: Option<String>) -> PathBuf;
  pub struct Bridge { pub handle: DataHandle, pub factory: Rc<BlotterFactory> }
  pub fn start(setup: DataSetup, find_style: FindStyle, cx: &mut App) -> Bridge;   // spawns the service and the drain task
  pub fn attach(bridge: &Bridge, window: WindowHandle<Root>, cx: &mut App);        // route events to the shell; subscribe to ShellEvent
  // demo.rs
  pub fn demo_dir(rows: usize) -> PathBuf;                       // <temp>/geode-demo/<rows>-42
  pub fn ensure_emitted(dir: &Path, rows: usize) -> std::io::Result<PathBuf>;   // the source dir
  pub fn layer(source_dir: &Path) -> Vec<LayerDoc>;             // the compiled-in demo docs, sources rewritten
  ```

- [ ] **Step 1: Write the failing tests**

`bridge.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, ConfigSources, LayerDoc};

    #[test]
    fn the_database_path_prefers_config_then_demo_then_the_platform_dir() {
        let empty = Config::load(&ConfigSources::default());
        assert_eq!(
            db_path(&empty, None, Some("C:\\Users\\me\\AppData\\Local".into()), Some("/home/me".into())),
            PathBuf::from("C:\\Users\\me\\AppData\\Local").join("Geode").join("geode.duckdb")
        );
        let unix = db_path(&empty, None, None, Some("/home/me".into()));
        assert!(unix.ends_with("Geode/geode.duckdb") || unix.ends_with("geode/geode.duckdb"), "{unix:?}");
        assert_eq!(
            db_path(&empty, Some(std::path::Path::new("/tmp/geode-demo/100000-42")), None, None),
            PathBuf::from("/tmp/geode-demo/100000-42/geode.duckdb")
        );
        let configured = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[data]\ndb_path = \"/var/geode/x.duckdb\"\n").unwrap()],
            ..ConfigSources::default()
        });
        assert_eq!(db_path(&configured, Some(std::path::Path::new("/tmp/d")), None, None), PathBuf::from("/var/geode/x.duckdb"), "config wins even over demo");
    }

    #[test]
    fn data_setup_needs_datasets_and_views_and_carries_sources() {
        let none = Config::load(&ConfigSources::default());
        assert!(data_setup(&none, "/tmp/x.duckdb".into()).is_none());
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("datasets", "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n").unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
                LayerDoc::builtin("sources", "[s]\ndataset = \"risk\"\npaths = [\"/x/*.csv\"]\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(&config, "/tmp/x.duckdb".into()).unwrap();
        assert_eq!(setup.config.sources.len(), 1);
        assert_eq!(setup.views.len(), 1);
        assert_eq!(setup.config.query_workers, 4);
    }
}
```

`demo.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_layer_is_complete_and_points_sources_at_the_directory() {
        let docs = layer(std::path::Path::new("/tmp/geode-demo/100-42/src"));
        let names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["app", "datasets", "dimensions", "groupings", "sources", "views"]);
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        let paths = sources.table["demo"]["paths"].as_array().unwrap();
        assert_eq!(paths[0].as_str(), Some("/tmp/geode-demo/100-42/src/*.csv"));
        assert_eq!(sources.table["demo"]["poll_interval"].as_str(), Some("2s"));
    }

    #[test]
    fn emitting_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let src = ensure_emitted(dir.path(), 500).unwrap();
        let count = std::fs::read_dir(&src).unwrap().count();
        assert!(count > 2);
        let again = ensure_emitted(dir.path(), 500).unwrap();
        assert_eq!(again, src);
        assert_eq!(std::fs::read_dir(&src).unwrap().count(), count, "not emitted twice");
    }
}
```

(`geode-app` gains `tempfile` as a dev-dependency.)

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: The demo config**

`examples/demo-config/datasets.toml` — the current
`examples/probe-config/datasets.toml` with every measure the generator
emits declared (the `RiskBatch` measure lists in
`crates/geode-demo-data/src/model.rs`: `UNDERLYING_MEASURES` at
`underlying` grain, `PAIR_MEASURES` at `underlying_pair`,
`INSTRUMENT_MEASURES` at `instrument`, `POSITION_MEASURES` at
`position`, each with its `_usd` twin as emitted — read `emit.rs` for
the exact header spellings), plus `strike`/`expiry`/`currency`/
`model_code` as `instrument`-grain attributes. Remove the probe-era
header comment; the new header says what the file is for (the `--demo`
layer, and the reference config for the desk's own `datasets.toml`).

`examples/demo-config/views.toml`:

```toml
# The --demo layer's views. `tree` is the shape the blotter runs and
# the requery benchmarks measure; `wide` is the 100-column view the
# DataTable swap trigger is measured on (spec §6.6).
config_version = 1

[tree]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref", "position_ref"]
[[tree.columns]]
name = "delta01"
label = "Δ01"
[[tree.columns]]
name = "gamma01"
label = "Γ01"
[[tree.columns]]
name = "vega01"
[[tree.columns]]
name = "cross_gamma02"
[[tree.columns]]
name = "npv"
format = { precision = 0, scale = "k" }
[[tree.columns]]
name = "daily_trading_pnl"
format = { precision = 0, negative = "parens" }

[wide]
dataset = "risk_snapshot"
grouping = ["book", "lhu", "position_ref"]
# … every measure and its _usd twin as a column (≈44), then derived
# columns `d01 = delta01 * 1`, …, until 100 columns …
```

Write the `wide` view's columns out in full (the executor generates the
list; a `derived` column is `kind = "derived"`, `sql = "delta01 * 2"`).

`examples/demo-config/dimensions.toml`:

```toml
config_version = 1
[desk]
from = "book"
[desk.values]
IDX_EXO_EU = ["BK000", "BK001", "BK002", "BK003", "BK004", "BK005", "BK006", "BK007", "BK008", "BK009"]
IDX_EXO_US = ["BK010", "BK011", "BK012", "BK013", "BK014", "BK015", "BK016", "BK017", "BK018", "BK019"]
```

`examples/demo-config/groupings.toml`:

```toml
config_version = 1
1 = ["lhu", "underlying_ref", "position_ref"]
2 = ["book", "lhu"]
3 = ["underlying_ref", "book", "position_ref"]
4 = ["desk", "book"]
```

`examples/demo-config/app.toml`:

```toml
config_version = 1
[modules]
default = "blotter"
[blotter]
stale_after = "15m"
```

- [ ] **Step 4: Implement `demo.rs`**

```rust
//! `--demo` (Phase 3 spec §7.1): boot on generated data with no real
//! source. Emits the generator's directory once per row count, layers
//! the compiled-in demo config under any desk/user config, and points
//! the database at the same temp directory so a demo never touches a
//! real one.

use geode_core::config::LayerDoc;
use std::path::{Path, PathBuf};

const SEED: u64 = 42;

pub fn demo_dir(rows: usize) -> PathBuf {
    std::env::temp_dir().join("geode-demo").join(format!("{rows}-{SEED}"))
}

/// The source directory, emitted if absent. Idempotent: a directory
/// with files in it is reused, so a second run is a warm start.
pub fn ensure_emitted(dir: &Path, rows: usize) -> std::io::Result<PathBuf> {
    let src = dir.join("src");
    let populated = std::fs::read_dir(&src).map(|mut d| d.next().is_some()).unwrap_or(false);
    if !populated {
        std::fs::create_dir_all(&src)?;
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows,
            seed: SEED,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(&src);
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts)?;
    }
    Ok(src)
}

/// The demo layer: every doc under `examples/demo-config`, compiled in,
/// plus a `sources` doc over `source_dir` polled every two seconds so a
/// file dropped into it shows up while you watch.
pub fn layer(source_dir: &Path) -> Vec<LayerDoc> {
    let sources = format!(
        "config_version = 1\n[demo]\ndataset = \"risk_snapshot\"\npaths = [{:?}]\n\
         readiness = \"sentinel\"\npriority = \"latest_risk\"\npoll_interval = \"2s\"\n\
         pending_timeout = \"1m\"\nbatch_pattern = '^risk_\\d{{4}}-\\d{{2}}-\\d{{2}}_(?P<batch>.+)$'\n",
        source_dir.join("*.csv").to_string_lossy()
    );
    let docs = [
        ("app", include_str!("../../../examples/demo-config/app.toml").to_string()),
        ("datasets", include_str!("../../../examples/demo-config/datasets.toml").to_string()),
        ("dimensions", include_str!("../../../examples/demo-config/dimensions.toml").to_string()),
        ("groupings", include_str!("../../../examples/demo-config/groupings.toml").to_string()),
        ("sources", sources),
        ("views", include_str!("../../../examples/demo-config/views.toml").to_string()),
    ];
    docs.into_iter()
        .map(|(name, text)| LayerDoc::builtin(name, &text).expect("demo config is well-formed"))
        .collect()
}
```

(`format!` with `{:?}` on the path string produces a quoted, escaped TOML
string on both platforms; the `\\d{{4}}` doubles the braces for
`format!`. The generator's batch stems are `risk_<date>_<batch>`.)

- [ ] **Step 5: Implement `bridge.rs`**

```rust
//! Where shell and data meet (Phase 3 spec §5.1, §9.1): builds the
//! service config from the layered docs, spawns the service behind a
//! `DataHandle`, drains its event channel into the shell on a task that
//! wakes on arrival, and forwards config reloads back to the data thread.

use geode_blotter::BlotterFactory;
use geode_core::config::{Config, Diagnostic};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::QueryOutcome;
use geode_core::schema::SchemaSpec;
use geode_core::view::ViewSpec;
use geode_data::source::SourceSpec;
use geode_data::{DataEvent, DataHandle, DataService, DataServiceConfig, EventSink};
use geode_shell::shell::{ShellEvent, ShellView};
use geode_shell::vimfind::FindStyle;
use gpui::{App, AsyncApp, WindowHandle};
use gpui_component::Root;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// Outbound events queued before the sink refuses (§7.3). Tiles × a
/// small burst; a full channel is counted by `DataHandle`. The sink
/// below must `try_send`, never block, and a refusal is counted into a
/// `dropped_events` counter surfaced through `set_data_status` — never
/// silently lost.
const EVENT_BOUND: usize = 256;

pub struct DataSetup {
    pub config: DataServiceConfig,
    pub views: Vec<ViewSpec>,
    pub dimensions: DerivedDimensions,
    pub diagnostics: Vec<Diagnostic>,
}

/// `None` when there is nothing to serve: no datasets or no views.
pub fn data_setup(config: &Config, db_path: PathBuf) -> Option<DataSetup> {
    let datasets = config.doc("datasets")?;
    let views_doc = config.doc("views")?;
    let mut diagnostics = Vec::new();
    let (schema, d) = SchemaSpec::from_doc(datasets);
    diagnostics.extend(d);
    let (views, d) = ViewSpec::from_doc(views_doc);
    diagnostics.extend(d);
    let (dimensions, d) = config.doc("dimensions").map(DerivedDimensions::from_doc).unwrap_or_default();
    diagnostics.extend(d);
    let (sources, d) = config
        .doc("sources")
        .map(|doc| SourceSpec::from_doc(doc, &schema))
        .unwrap_or_default();
    diagnostics.extend(d);
    Some(DataSetup {
        config: DataServiceConfig {
            db_path,
            schema,
            views: views.clone(),
            dimensions: dimensions.clone(),
            query_workers: 4,
            sources,
        },
        views,
        dimensions,
        diagnostics,
    })
}

/// `[app] data.db_path` wins; then the demo directory; then the platform
/// application-data directory (spec §5.4).
pub fn db_path(config: &Config, demo: Option<&Path>, local_app_data: Option<String>, home: Option<String>) -> PathBuf {
    if let Some(p) = config.get("app", "data.db_path").and_then(|v| v.as_str()) {
        return PathBuf::from(p);
    }
    if let Some(dir) = demo {
        return dir.join("geode.duckdb");
    }
    if let Some(lad) = local_app_data {
        return PathBuf::from(lad).join("Geode").join("geode.duckdb");
    }
    let home = home.unwrap_or_else(|| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/Geode/geode.duckdb")
    } else {
        PathBuf::from(home).join(".local/share/geode/geode.duckdb")
    }
}

pub struct Bridge {
    pub handle: DataHandle,
    pub factory: Rc<BlotterFactory>,
    events: async_channel::Receiver<DataEvent>,
}

pub fn start(setup: DataSetup, find_style: FindStyle, _cx: &mut App) -> Bridge {
    for d in &setup.diagnostics {
        eprintln!("[data] {d}");
    }
    let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
    let sink: EventSink = Arc::new(move |e| tx.try_send(e).is_ok());
    let handle = DataService::spawn(setup.config, sink);
    let factory = Rc::new(BlotterFactory::new(handle.clone(), setup.views, find_style));
    Bridge { handle, factory, events: rx }
}

/// Route events into the shell and forward reloads. Wakes on arrival:
/// `async_channel::Receiver::recv` is a future gpui's executor polls, so
/// delivery latency is a frame, not a poll interval.
pub fn attach(bridge: &Bridge, window: WindowHandle<Root>, cx: &mut App) {
    let rx = bridge.events.clone();
    let handle = bridge.handle.clone();
    let factory = bridge.factory.clone();

    let shell = window
        .read(cx)
        .ok()
        .and_then(|root| root.view().clone().downcast::<ShellView>().ok())
        .expect("the window's root view is the shell");

    // Reloads: new views to the data thread and to the factory.
    cx.subscribe(&shell, {
        let handle = handle.clone();
        let factory = factory.clone();
        move |shell, event: &ShellEvent, cx| {
            if let ShellEvent::ConfigReloaded = event {
                let config = shell.read(cx).config();
                let Some(views_doc) = config.doc("views") else {
                    return;
                };
                let (views, _) = ViewSpec::from_doc(views_doc);
                let (dims, _) = config.doc("dimensions").map(DerivedDimensions::from_doc).unwrap_or_default();
                factory.set_views(views.clone());
                factory.set_find_style(FindStyle::from_config(config));
                handle.replace_views(views, dims);
            }
        }
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(event) = rx.recv().await {
            let outcome = match event {
                DataEvent::Query(outcome) => Some(outcome),
                DataEvent::Published { dataset, batch, gen_id, .. } => {
                    eprintln!("[data] published {dataset}/{batch} gen {gen_id}");
                    let _ = cx.update(|cx| {
                        shell.read(cx).frame().update(cx, |f, cx| {
                            f.note_published();
                            cx.notify();
                        });
                    });
                    None
                }
                DataEvent::Health { source, worst, detail } => {
                    eprintln!("[data] health {source}: {} — {detail}", worst.label());
                    let _ = cx.update(|cx| {
                        shell.update(cx, |s, cx| s.set_data_status(Some(format!("{source}: {}", worst.label())), cx));
                    });
                    None
                }
                DataEvent::Diagnostics(diags) => {
                    for d in diags {
                        eprintln!("[data] {d}");
                    }
                    None
                }
            };
            if let Some(outcome) = outcome {
                let delivered = window.update(cx, |root, window, cx| {
                    if let Ok(shell) = root.view().clone().downcast::<ShellView>() {
                        shell.update(cx, |s, cx| s.deliver(outcome, window, cx));
                    }
                });
                if delivered.is_err() {
                    return; // the window is gone
                }
            }
        }
    })
    .detach();
}
```

`ShellView::set_data_status(&mut self, status: Option<String>, cx)` is a
small shell addition: a field shown in the status bar's left region in
the `warning` token (like `restart_required`), `debug_selector`
`"data-status"`. Add it in this task with a one-line test in the shell.

`route_outcome`-style `deliver` needs the `Window`, which is why the
outcome path goes through `window.update` and the others through
`cx.update`.

The last `DataHandle` must be shut down off the UI thread —
`DataHandle::shutdown` joins the service thread, which waits for
in-flight ingest and discovery — so the quit hook spawns
`handle.shutdown()` on `cx.background_executor()` rather than dropping
it on the main thread.

- [ ] **Step 6: `main.rs`**

- Parse args: `--demo [rows]` (default `100_000`). Anything else prints
  usage and exits 2.
- `Cargo.toml`: add `geode-blotter = { path = "../geode-blotter" }`,
  `geode-demo-data = { path = "../geode-demo-data" }`,
  `async-channel = "2.5"`; dev-dep `tempfile`.
- In `run`: after `gpui_component::init` and the reclaimed keybindings,
  call `geode_blotter::init(cx)`.
- `build_shell_services(demo: Option<&Path>)`: when demo, `ConfigSources.builtin`
  is the keymap plus `demo::layer(&source_dir)`. After the config loads:
  `let db = bridge::db_path(&config, demo_dir, env LOCALAPPDATA, env HOME);`
  `let setup = bridge::data_setup(&config, db);`. Build the roster:
  `ModuleRoster::new(default_kind)`; when `setup` is `Some`, start the
  bridge and `roster.add(Box::new(BlotterFactoryHandle(bridge.factory.clone())))`
  — the roster wants a `Box<dyn ModuleFactory>` and the bridge keeps an
  `Rc<BlotterFactory>` for `set_views`; give `BlotterFactory` an
  `impl ModuleFactory for Rc<BlotterFactory>` forwarding, or store the
  `Rc` in the roster via a thin newtype. Register actions, then
  `build_keymap`.
- After the window opens: `bridge::attach(&bridge, window, cx)`.
- The `probe::prepare`/`probe::start` calls go (Task 9 deletes the
  module; this task leaves them compiling by keeping `mod probe;` until
  then, or deletes them here — either is fine; the plan deletes them
  here to keep one binary path).

- [ ] **Step 7: Run end to end**

```sh
cargo run -p geode-app -- --demo
```

Expected: the shell opens with a blotter in the first tile (a fresh
session has no tiles — `ctrl+v` opens the first; the empty-workspace
hint still says so), `[data] published …` lines as the scheduler loads,
rows paint grouped by slot 1. `ctrl+2` regroups. `zo` on an LHU expands.
`mod+shift+p` shows requery timings. Then:

```sh
cargo run -p geode-demo-data --example emit -- /tmp/extra 20000
cp /tmp/extra/*BK005* "$TMPDIR/geode-demo/100000-42/src/"
```

Expected: within two seconds the blotter repaints with the new data and
no keypress. Then `cargo run -p geode-app -- --demo 1000000` and read the
overlay's requery row after a `ctrl+2`; record it in `docs/perf.md`
(Task 9).

- [ ] **Step 8: Check and commit**

```bash
git add crates/geode-app examples/demo-config Cargo.lock crates/geode-shell
git commit -m "feat(app): the data bridge, the blotter in the roster, and --demo

DataService behind a handle with events drained on a task that wakes on
arrival; outcomes routed to tiles, publishes to the frame, health to the
status bar, reloads back to the data thread. The database lives in the
platform data dir unless configured; --demo emits generated data and
layers a compiled-in config (Phase 3 §5.1, §5.4, §7.1).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

---

### Task 9: Delete the probe; benches; docs; the harness

Spec §9 step 5. Nothing may still read `GEODE_PROBE_DIR`.

**Files:**
- Delete: `crates/geode-shell/src/dataprobe.rs`, `crates/geode-app/src/probe.rs`,
  `examples/probe-config/`
- Modify: `crates/geode-shell/src/lib.rs` (`pub mod dataprobe;` gone),
  `crates/geode-shell/src/shell/mod.rs` (`data_probe`, `probe`,
  `set_probe`, `data_probe_visible`, the render layer, the `data::toggle_probe`
  arm), `crates/geode-shell/src/defaults.rs` (`data::toggle_probe`
  action and `mod+shift+d`), `crates/geode-app/Cargo.toml` (the probe
  comment on the `geode-data` dependency), `crates/geode-blotter/benches/blotter.rs`,
  `docs/perf.md`, `CLAUDE.md`, `scripts/mutation-check.sh`

- [ ] **Step 1: Delete**

```sh
git rm crates/geode-shell/src/dataprobe.rs crates/geode-app/src/probe.rs
git rm -r examples/probe-config
```

Remove every reference (`grep -rn "dataprobe\|probe::\|toggle_probe\|GEODE_PROBE_DIR\|set_probe" crates docs CLAUDE.md`
must come back empty except for history in `docs/superpowers/plans/` and
`docs/phase-*`). The `mod+shift+d` binding leaves the builtin keymap;
it was a throwaway's and the palette entry goes with it.

- [ ] **Step 2: Benches**

`crates/geode-blotter/benches/blotter.rs`:

```rust
//! The blotter's pure-core costs at the three result shapes docs/perf.md
//! records (133 / 136,868 / 729,466 rows): flatten fully expanded,
//! filling a 40×20 cache window, and building a 100-column plan.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_blotter::core::cache::FormatCache;
use geode_blotter::core::cache::cell;
use geode_blotter::core::expansion::Expansion;
use geode_blotter::core::flatten::flatten;
use geode_blotter::core::plan::ColumnPlan;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
use geode_core::view::ViewSpec;
use std::hint::black_box;

fn dim(name: &str) -> ColumnMeta {
    ColumnMeta { name: name.into(), attribution_by_depth: vec![Attribution::Additive; 4], scope_semantics: ScopeSemantics::Direct }
}

/// Same shape builder as geode-core's tree bench, plus `measures`
/// numeric columns.
fn shape(l1: usize, l2: usize, l3: usize, measures: usize) -> (Snapshot, ViewSpec) {
    // … identical to `crates/geode-core/benches/tree.rs::shape`, then
    // `measures` F64 columns `m0..mN` filled with `(row as f64) * 1.5` …
    // and a view over them (kind measure) grouped lhu/underlying_ref/position_ref.
    unimplemented!("copy shape() from geode-core/benches/tree.rs and add measures")
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("blotter_core");
    g.sample_size(10);
    for (name, l1, l2, l3) in [("133_rows", 12, 10, 0), ("137k_rows", 12, 10, 1_130), ("729k_rows", 80, 10, 900)] {
        let (snap, view) = shape(l1, l2, l3, 6);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        let mut open = Expansion::default();
        open.open_all();
        let mut out = Vec::with_capacity(snap.rows());
        g.bench_function(format!("flatten_all_{name}"), |b| {
            b.iter(|| {
                flatten(&snap, &plan, &open, None, &mut out);
                black_box(out.len())
            })
        });
        g.bench_function(format!("flatten_collapsed_{name}"), |b| {
            b.iter(|| {
                flatten(&snap, &plan, &Expansion::default(), None, &mut out);
                black_box(out.len())
            })
        });
        flatten(&snap, &plan, &open, None, &mut out);
        let shown = out.clone();
        g.bench_function(format!("cache_fill_40x{}_{name}", plan.columns.len()), |b| {
            b.iter(|| {
                let mut cache = FormatCache::default();
                cache.set_window(0..40, plan.columns.len(), |r, c| cell(&snap, &plan, shown[r] as usize, c));
                black_box(cache.window().len())
            })
        });
    }
    let (snap, view) = shape(12, 10, 0, 100);
    g.bench_function("plan_build_100_columns", |b| b.iter(|| black_box(ColumnPlan::build(&view, snap.grouping(), &snap))));
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

Replace the `unimplemented!` with the real builder (the geode-core bench
is the template; measures are `TestColumn::F64`). Run
`cargo bench -p geode-blotter` and record the medians in `docs/perf.md`
under **"Phase 3c: blotter core (`cargo bench -p geode-blotter`)"**.

- [ ] **Step 3: `docs/perf.md`**

Add:
- the blotter core table from Step 2;
- a **"Phase 3: the painted frame"** section with the `--demo 1000000`
  requery readout (submit→snapshot, snapshot→paint) for `ctrl+2` and for
  `zo` at the bound, and one sentence on whether the §7.1 50 ms holds —
  measured, not asserted;
- the `DataTable` swap trigger measurement: `j` on the `wide` view with
  40 visible rows, read from the frame-time overlay's p95. State the
  number and whether it is under 8 ms. If it is not, that is the
  finding; open a follow-up rather than swapping in this plan.
- Replace the `GEODE_PROBE_DIR` recipe with the `--demo` one.

- [ ] **Step 4: `CLAUDE.md`**

- The "Phase 3 (blotter) is next" paragraph becomes **"Phase 3 (the
  blotter) is complete"**, naming the crate, `--demo`, the frame
  (`ctrl+1..9`), the command line, and that the probe is gone.
- Delete the sequencing-constraint paragraph and the cold-start
  paragraph's probe reference; keep the cold-start hold itself.
- Commands: add `cargo run -p geode-app -- --demo [rows]` and
  `cargo bench -p geode-blotter`; update the harness entry count.
- Architecture tree: add `geode-blotter` under modules.
- Gotchas: **`DataTable` is never focused; its key context is bound to
  `NoAction` in `geode_blotter::init`, and a tile click re-arms the
  shell's focus restore.** And: **a `NonAttributable` cell is NULL —
  read only through `f64_at`/`f64_value`; the blotter's cache is the one
  place a cell becomes text.**

- [ ] **Step 5: Unfiltered harness run, final check, commit**

Run: `zsh scripts/mutation-check.sh` — every line `caught`.
Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`

```bash
git add -A
git commit -m "feat: delete the probe; Phase 3 lands

The throwaway data probe and its config example are gone: the blotter
is the painted end of §7.1, --demo is its test story, and nothing reads
GEODE_PROBE_DIR. Blotter core benches recorded in docs/perf.md.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL"
```

---

## Self-Review

**Spec coverage:**

| Spec | Task |
|---|---|
| §1.2 done state (demo boots, keys work, blank/dagger, file lands, overlay timing, probe gone) | 8, 7, 4/6, 8, 9, 9 |
| §2.2 find_style semantics | 3, 7 |
| §3.1 `TileContent` impl; §3.2 factory | 7 |
| §3.3 `DataTable` bindings reclaimed | 1 (`init`), 8 (called) |
| §4.3 every `:` row | 5 (grammar), 7 (application) |
| §4.4 pinned/unscoped tile markers | 7 (header) |
| §5.1 the bridge wakes on arrival; outcomes routed by key; publishes bump the frame | 8 |
| §5.4 database path, demo path | 8 |
| §6.1 plan, expansion, flatten, bound, cursor, visual, find, cache | 1, 2, 3, 4 |
| §6.2 format incl. scale; header suffix | 4, 1 |
| §6.3 `s`, `:sort`, header click; NULL last | 7, 5, 6, 2 |
| §6.4 yank | 4, 7 |
| §6.5 blank/dagger/⋈/freshness/AS OF/stale | 4, 6, 7 |
| §6.6 adapter settings, cursor, scroll, click, sort, move, swap trigger | 6, 7, 9 |
| §6.7 requery discipline, tag, 50 ms affordance, error keeps snapshot | 7 |
| §6.8 timing recorded | 7 |
| §7.1 `--demo` | 8 |
| §7.2 module tests listed | 7 (paint, `j`, `zo`, blank cell via cache, frame change submits one, stale tag, row click, timing); `5j` and `find_style` in 7 |
| §7.3 benches | 9 |
| §9 step 5 deletion | 9 |

**Placeholder scan:** Task 8's `wide` view column list and Task 9's
bench `shape()` are marked for the executor to expand mechanically from
a named template; both say exactly what to produce. Nothing else is
deferred.

**Type consistency:**
- `BlotterDelegate` fields used by `BlotterTile` (`shown`, `cursor`,
  `mode`, `plan`, `expansion`, `sort`, `narrowed`, `any_determined`,
  `semi_joined`, `unplaced`) are all `pub` in Task 6.
- `FindState::changed` returns the cursor row under vim and the index
  into `narrowed` under fzf; `BlotterTile::find` applies `set_narrowed`
  before `to_row`, so the index is into `shown`. Consistent.
- `QueryParams.grouping: Option<Vec<String>>` (3a as amended) is what
  `requery` fills.
- `Frame::requery` is `pub` (3b Task 2); `record_submit_to_snapshot` /
  `record_snapshot_to_paint` (3b Task 7) are what Task 7 calls.
- `ShellView::set_data_status` is added in Task 8 and is the only shell
  change in this plan besides bindings and the deletions.
- `geode_shell::defaults::BLOTTER_ACTIONS` (Task 7 Step 5) mirrors
  `geode_blotter::tile::ACTIONS`, pinned by a test.

## Execution Handoff

Plan complete. Execution order across the three plans: 3a → 3b → 3c,
each merged green before the next begins. Task 0 (the `shell/mod.rs`
split) was added on 2026-09-05 and is reviewed as its own gate before
Task 1 starts.
