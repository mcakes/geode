# Verb consistency across every surface — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the six rulings of interaction-model spec §20 true in code: one confirm shape for `d`/`r`, the picker on the escape ladder, a clickable value chip as the mouse form of `space`, a `/` line that commits on click-away, one wrap/clamp rule and one nav key set on every list and tile cursor, and the palette's `tab` reclaimed.

**Architecture:** Every rule lives in exactly one place and every surface routes through it — `vimnav::apply` for motion, `dialog::confirm_row` + `dialog::ConfirmAnswer` for the destructive question, `dialog::value_chip` for the mouse step, `ShellView::leave_command_line` for click-away. Pure cores first (unit-tested without a window), gpui wiring second (window-tested through `shell/tests/`), a mutation-harness entry per behaviour changed.

**Tech Stack:** Rust, gpui + gpui-component (pinned rev), `gpui::TestAppContext`/`VisualTestContext` window tests, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` §20 (read §5, §16, §17, §18 and §19 too — §20 amends them). Blotter/market-data tile vocabulary: `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md` §4.3, `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md` §8.3.

## Global Constraints

- **Diagnostics tile untouched** (spec §20.6) — no file under `crates/geode-diagnostics/` changes.
- **Layering** (CLAUDE.md): `geode-shell` never depends on a module; `geode-blotter` and `geode-marketdata` depend on `geode-shell` and never on each other.
- **Every dialog mouse handler that mutates mode/query/stage/draft ends in `dialog::sync_dialog_text`** (spec §16.1 / §17.1 rule 3) and never calls `focus`/`set_value` itself.
- **A step is the surface's ONE step path**: settings → `apply_setting`, object dialog → `step_selected_row`. No second implementation.
- **Selectors are `&'static str` at `debug_bounds` call sites** — tests spell them out.
- **CI gates** on every task: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p <crate>`; Windows must keep building (no platform-specific code here).
- **Harness**: add a `run_mutation` entry (6th argument = the covering test) for every behaviour changed; `zsh scripts/mutation-check.sh --anchors-only` must exit 0 before merge; the entry count in `CLAUDE.md` (`mutation harness (904 entries)`) is updated once, in Task 10.
- **Commit after every task**; the harness restores files with `git checkout`, so never mutate with uncommitted work.
- Commit messages end with the attribution trailer from the session's system reminder.

---

## File map

| File | Change |
|---|---|
| `crates/geode-shell/src/vimnav.rs` | `apply` wraps on ±1; new `apply_clamped` (the old body) |
| `crates/geode-blotter/src/core/cursor.rs` | `move_rows` gains `wrap: bool`; `move_cols` → `apply_clamped` |
| `crates/geode-blotter/src/tile.rs` | passes `wrap = mode is Normal` |
| `crates/geode-marketdata/src/tile.rs` | `move_cursor` row axis through `apply`; `page_down_full`/`page_up_full` verbs |
| `crates/geode-marketdata/src/content.rs` | two new `ACTIONS` + four fragment bindings |
| `crates/geode-shell/src/palette.rs` | `move_selection` delegates to `apply`; panel gains `GeodePalette` key context |
| `crates/geode-shell/src/shell/palette_ctl.rs` | all nav keys through `nav_command` + `apply` |
| `crates/geode-shell/src/shell/picker.rs` | `nav_command` + `apply`; Values `escape` → `back_to_columns`; row click selects, tick click toggles; hints |
| `crates/geode-shell/src/shell/asof_view.rs` | `nav_command` + `apply`; local `move_selection` deleted |
| `crates/geode-shell/src/shell/dialog.rs` | `ConfirmAnswer`, `confirm_row`, `value_chip`; palette `tab` reclaim; `enter_filter_by_mouse` armed guard |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | consume shared `confirm_row`/`ConfirmAnswer`; value chip on field rows; `i`/`n` buttons; edit-row click armed guard |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | `Draft::vocabulary_of(row)`; `selected_vocabulary` delegates |
| `crates/geode-shell/src/shell/keybindings_view.rs` | `KeybindingConfirm`, `confirm` on state, armed routing, action bar, confirm hints |
| `crates/geode-shell/src/shell/settings_view.rs` | value chip; `click_selects_or_steps` deleted; `on_value_chip_clicked` |
| `crates/geode-shell/src/shell/commandline_ctl.rs` | `leave_command_line` |
| `crates/geode-shell/src/shell/render.rs` | three call sites → `leave_command_line` |
| `crates/geode-shell/src/shell/tests/{palette,picker,asof,keybindings_dialog,objectdialog,chrome_and_dialogs,commandline}.rs` | window tests |
| `scripts/mutation-check.sh` | entries |
| `CLAUDE.md`, spec §20 "As built" | docs |

---

### Task 1: `vimnav::apply` wraps on ±1 — the blotter cursor follows

**Files:**
- Modify: `crates/geode-shell/src/vimnav.rs:252-269` (`apply`), module doc line 17, tests at 557-596
- Modify: `crates/geode-blotter/src/core/cursor.rs:26-43`, tests 134-162
- Modify: `crates/geode-blotter/src/tile.rs:726-731`
- Modify: `scripts/mutation-check.sh` (append two entries)

**Interfaces:**
- Produces: `pub fn vimnav::apply(selected: usize, len: usize, cmd: NavCommand) -> usize` — **wraps** when `cmd == Move(±1)`, clamps otherwise. `pub fn vimnav::apply_clamped(selected, len, cmd) -> usize` — the previous behaviour, for column axes and visual mode. `Cursor::move_rows(&mut self, len: usize, cmd: NavCommand, count: Option<u32>, wrap: bool)`.

- [ ] **Step 1: Write the failing pure tests in `vimnav.rs`**

Replace the `apply_does_not_wrap` test (line 588) and add two more, inside `mod tests`:

```rust
    #[test]
    fn apply_wraps_a_single_step_at_both_ends() {
        // Spec §20.5: a bare ±1 wraps — the palette's rule, now
        // everyone's.
        assert_eq!(apply(4, 5, NavCommand::Move(1)), 0);
        assert_eq!(apply(0, 5, NavCommand::Move(-1)), 4);
        assert_eq!(apply(0, 1, NavCommand::Move(1)), 0, "one row wraps to itself");
    }

    #[test]
    fn apply_clamps_every_larger_step() {
        // ±5 / ±10 (and a counted ±1, which arrives here already
        // multiplied — `Cursor::move_rows`) clamp, never wrap.
        assert_eq!(apply(4, 5, NavCommand::Move(2)), 4);
        assert_eq!(apply(0, 5, NavCommand::Move(-2)), 0);
        assert_eq!(apply(3, 5, NavCommand::Move(10)), 4);
        assert_eq!(apply(1, 5, NavCommand::Move(-10)), 0);
    }

    #[test]
    fn apply_clamped_never_wraps() {
        // The column axis and visual mode use this one.
        assert_eq!(apply_clamped(4, 5, NavCommand::Move(1)), 4);
        assert_eq!(apply_clamped(0, 5, NavCommand::Move(-1)), 0);
        assert_eq!(apply_clamped(0, 0, NavCommand::Move(1)), 0);
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-shell vimnav::tests::apply_`
Expected: `apply_wraps_a_single_step_at_both_ends` FAILS (`left: 4, right: 0`); `apply_clamped_never_wraps` fails to compile (`apply_clamped` not found). Comment the third test out to see the first fail, or accept the compile error as the failure — either is fine.

- [ ] **Step 3: Implement**

Replace `apply` (vimnav.rs:252-269) with:

```rust
/// Apply a resolved [`NavCommand`] to a `selected` index against a list of
/// `len` items. **A bare ±1 wraps at both ends; every larger step clamps**
/// (interaction-model spec §20.5, user ruling 2026-09-14): `j` at the
/// last row lands on the first, `ctrl+d` at the last row stays, and a
/// counted `2j` — which `Cursor::move_rows` multiplies into the delta
/// before calling here — clamps like any other multi-row step, with no
/// special case for a count of one. `Top`/`Bottom` are absolute. An
/// empty list (`len == 0`) always yields `0`, regardless of the command.
///
/// Two axes deliberately do NOT take this rule and call
/// [`apply_clamped`] instead: a column axis (`h`/`l` — the ruling was
/// about rows) and a blotter in visual mode (a wrapping `j` at the bottom
/// would put the cursor above the anchor and invert the selection).
pub fn apply(selected: usize, len: usize, cmd: NavCommand) -> usize {
    match cmd {
        NavCommand::Move(delta) if delta.abs() == 1 && len > 0 => {
            let len = len as i64;
            ((selected as i64 + delta).rem_euclid(len)) as usize
        }
        _ => apply_clamped(selected, len, cmd),
    }
}

/// [`apply`] without the single-step wrap: clamped to `0..len` at both
/// ends whatever the delta. The column axis and visual mode use this.
pub fn apply_clamped(selected: usize, len: usize, cmd: NavCommand) -> usize {
    if len == 0 {
        return 0;
    }
    let max = (len - 1) as i64;
    match cmd {
        NavCommand::Move(delta) => (selected as i64 + delta).clamp(0, max) as usize,
        NavCommand::Top => 0,
        NavCommand::Bottom => len - 1,
    }
}
```

Update the module doc at line 17-18: replace `No wrap: see [`apply`].` with `` A bare ±1 wraps, a counted one clamps: see [`apply`]. ``

- [ ] **Step 4: Run vimnav tests**

Run: `cargo test -p geode-shell vimnav::`
Expected: all PASS.

- [ ] **Step 5: Write the failing blotter cursor tests**

In `crates/geode-blotter/src/core/cursor.rs` `mod tests`, replace `row_motion_is_counted_and_clamped` with:

```rust
    #[test]
    fn row_motion_is_counted_and_clamped_but_a_bare_step_wraps() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_rows(10, NavCommand::Move(1), Some(5), true);
        assert_eq!(c.row, 5);
        c.move_rows(10, NavCommand::Move(1), Some(50), true);
        assert_eq!(c.row, 9, "a counted step clamps, no wrap");
        c.move_rows(10, NavCommand::Move(1), None, true);
        assert_eq!(c.row, 0, "a bare j at the bottom wraps to the top (spec §20.5)");
        c.move_rows(10, NavCommand::Move(-1), None, true);
        assert_eq!(c.row, 9, "and a bare k at the top wraps to the bottom");
        c.move_rows(10, NavCommand::Move(1), Some(1), true);
        assert_eq!(c.row, 0, "1j is j: the count is multiplied in, not special-cased");
        c.move_rows(10, NavCommand::Top, None, true);
        assert_eq!(c.row, 0);
        c.move_rows(10, NavCommand::Bottom, Some(3), true);
        assert_eq!(c.row, 2, "a counted G goes to that row (1-based)");
        c.move_rows(10, NavCommand::Bottom, None, true);
        assert_eq!(c.row, 9);
        c.move_rows(0, NavCommand::Move(1), None, true);
        assert_eq!(c.row, 0, "empty list");
    }

    #[test]
    fn visual_mode_clamps_a_bare_step() {
        // `wrap = false` is what the tile passes while `Mode::Visual`: a
        // wrap would put the cursor above the anchor and invert the
        // selection.
        let mut c = Cursor { row: 9, col: 0 };
        c.move_rows(10, NavCommand::Move(1), None, false);
        assert_eq!(c.row, 9);
        c.row = 0;
        c.move_rows(10, NavCommand::Move(-1), None, false);
        assert_eq!(c.row, 0);
    }

    #[test]
    fn column_motion_is_counted_and_clamped() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_cols(5, 1, Some(3));
        assert_eq!(c.col, 3);
        c.move_cols(5, 1, Some(9));
        assert_eq!(c.col, 4);
        c.move_cols(5, 1, None);
        assert_eq!(c.col, 4, "l at the last column stays: columns never wrap");
        c.move_cols(5, -1, None);
        assert_eq!(c.col, 3);
        c.clamp(1, 2);
        assert_eq!((c.row, c.col), (0, 1));
    }
```

- [ ] **Step 6: Run to see them fail**

Run: `cargo test -p geode-blotter core::cursor::`
Expected: compile error (`move_rows` takes 3 arguments).

- [ ] **Step 7: Implement `Cursor`**

Replace `move_rows`/`move_cols` (cursor.rs:26-43):

```rust
    /// Move the row by `cmd`, `count` times. `wrap` is whether a BARE
    /// ±1 wraps at the ends (spec §20.5, `vimnav::apply`'s rule) — the
    /// tile passes `true` in normal mode and `false` in visual mode,
    /// where a wrap would carry the cursor past the anchor and invert
    /// the selection. A counted step is multiplied in before the rule
    /// is applied, so `2j` clamps and `1j` wraps exactly as `j` does.
    pub fn move_rows(&mut self, len: usize, cmd: NavCommand, count: Option<u32>, wrap: bool) {
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
        self.row = if wrap {
            apply(self.row, len, cmd)
        } else {
            apply_clamped(self.row, len, cmd)
        };
    }

    /// Columns clamp whatever the count: the ruling behind `apply`'s
    /// wrap was about rows, and a horizontal wrap is a separate question
    /// left as it was.
    pub fn move_cols(&mut self, cols: usize, delta: i64, count: Option<u32>) {
        let n = count.unwrap_or(1) as i64;
        self.col = apply_clamped(self.col, cols, NavCommand::Move(delta * n));
    }
```

Fix the import at the top of cursor.rs to `use geode_shell::vimnav::{NavCommand, apply, apply_clamped};` (check the existing `use` line and extend it).

In `crates/geode-blotter/src/tile.rs:726-731` change the call:

```rust
                self.with_delegate(cx, |d| {
                    let len = d.shown.len();
                    // Spec §20.5: a bare j/k wraps in normal mode only.
                    let wrap = matches!(d.mode, Mode::Normal);
                    d.cursor.move_rows(len, cmd, count, wrap);
                });
```

(`Mode` is already in scope in tile.rs — it is matched a few lines below.)

- [ ] **Step 8: Run the blotter suite**

Run: `cargo test -p geode-blotter`
Expected: all PASS. If a tile-level test asserted "j at the last row stays", update it to assert the wrap and name spec §20.5 in the message.

- [ ] **Step 9: Harness entries**

Append to `scripts/mutation-check.sh`, next to the other `vimnav`/blotter entries (search for `run_mutation "blotter:` and add after the last one):

```sh
run_mutation "vimnav: a bare ±1 wraps (spec §20.5)" \
  crates/geode-shell/src/vimnav.rs \
  '        NavCommand::Move(delta) if delta.abs() == 1 && len > 0 => {' \
  '        NavCommand::Move(delta) if delta.abs() == 0 && len > 0 => {' \
  geode-shell \
  apply_wraps_a_single_step_at_both_ends

run_mutation "blotter cursor: visual mode clamps a bare step (spec §20.5)" \
  crates/geode-blotter/src/core/cursor.rs \
  '        self.row = if wrap {' \
  '        self.row = if true {' \
  geode-blotter \
  visual_mode_clamps_a_bare_step
```

Run: `zsh scripts/mutation-check.sh --anchors-only`
Expected: `0 stale, 0 ambiguous`, exit 0.

- [ ] **Step 10: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy -p geode-shell -p geode-blotter --all-targets -- -D warnings
git add crates/geode-shell/src/vimnav.rs crates/geode-blotter/src/core/cursor.rs crates/geode-blotter/src/tile.rs scripts/mutation-check.sh
git commit -m "vimnav: a bare ±1 wraps, everything larger clamps — the blotter cursor follows, visual mode and columns clamp (spec §20.5)"
```

---

### Task 2: Market-data tile — same wrap rule, full-page keys

**Files:**
- Modify: `crates/geode-marketdata/src/content.rs:27-47` (`ACTIONS`), fragment at 80-81
- Modify: `crates/geode-marketdata/src/tile.rs:1016-1030` (dispatch), `1376-1388` (`move_cursor`)
- Modify: `crates/geode-marketdata/src/tile.rs` tests (near `cursor_moves_with_counts_and_scrolls`, line 3000)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `geode_shell::vimnav::{apply, apply_clamped, NavCommand}` from Task 1.
- Produces: verbs `marketdata::page_down_full` / `marketdata::page_up_full` (±10 × count).

- [ ] **Step 1: Write the failing window test**

Add after `cursor_moves_with_counts_and_scrolls` in `tile.rs`'s `mod tests`:

```rust
    /// Spec §20.5 on the panel: a bare `j` wraps, a counted one clamps,
    /// the full-page pair moves ten, and `h`/`l` never wrap.
    #[gpui::test]
    fn a_bare_row_step_wraps_and_the_full_page_keys_move_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        let terms: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
        let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
        h.deliver(&mut vcx, tag, Arc::new(document_of(&terms, &NODES, BASE)));

        h.dispatch(&mut vcx, "up", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()).0,
            11,
            "a bare k at the top wraps to the last row"
        );
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()).0, 0, "and j wraps back");
        h.dispatch(&mut vcx, "down", Some(20));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()).0, 11, "a counted step clamps");
        h.dispatch(&mut vcx, "page_up_full", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()).0, 1, "ctrl+b moves ten");
        h.dispatch(&mut vcx, "page_down_full", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()).0, 11);
        h.dispatch(&mut vcx, "page_down_full", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()).0, 11, "and clamps at the end");
        h.dispatch(&mut vcx, "last_col", None);
        let last = h.tile.read_with(&vcx, |t, _| t.cursor()).1;
        h.dispatch(&mut vcx, "right", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()).1,
            last,
            "columns clamp: l at the last column stays"
        );
    }
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p geode-marketdata a_bare_row_step_wraps`
Expected: FAIL at the first assertion (`left: 0, right: 11`).

- [ ] **Step 3: Implement**

`content.rs` — add two `ACTIONS` after `page_up`:

```rust
    ("marketdata::page_down_full", "Full page down"),
    ("marketdata::page_up_full", "Full page up"),
```

and four bindings in the `mode == normal` fragment after `"ctrl+u" = "marketdata::page_up"`:

```toml
"ctrl+f" = "marketdata::page_down_full"
"ctrl+b" = "marketdata::page_up_full"
"pagedown" = "marketdata::page_down_full"
"pageup" = "marketdata::page_up_full"
```

`tile.rs` dispatch (line 1016): add the two verbs to the motion arm and their deltas —

```rust
            "down" | "up" | "left" | "right" | "page_down" | "page_up" | "page_down_full"
            | "page_up_full" | "top" | "bottom" | "first_col" | "last_col" => {
                let (rows, cols) = match verb {
                    "down" => (n, 0),
                    "up" => (-n, 0),
                    "left" => (0, -n),
                    "right" => (0, n),
                    "page_down" => (HALF_PAGE * n, 0),
                    "page_up" => (-HALF_PAGE * n, 0),
                    // `vimnav`'s ±10, the blotter's `page_down_full`.
                    "page_down_full" => (FULL_PAGE * n, 0),
                    "page_up_full" => (-FULL_PAGE * n, 0),
```

Add beside `HALF_PAGE`:

```rust
/// How many rows `ctrl+f`/`ctrl+b`/`pagedown`/`pageup` step — `vimnav`'s
/// own ±10, the blotter's `page_down_full`.
const FULL_PAGE: isize = 10;
```

Replace `move_cursor` (1376-1388):

```rust
    /// Spec §20.5: the row axis wraps on a bare ±1 and clamps on anything
    /// larger (`vimnav::apply`, the one rule every list and tile shares);
    /// the column axis clamps whatever the delta (`apply_clamped`). The
    /// `top`/`bottom`/`first_col`/`last_col` verbs pass half-`isize`
    /// deltas, which the clamp arm absorbs.
    fn move_cursor(&mut self, rows: isize, cols: isize) {
        use geode_shell::vimnav::{NavCommand, apply, apply_clamped};
        let (nrows, ncols) = (self.model.rows.len(), self.model.columns.len());
        if nrows == 0 || ncols == 0 {
            self.cursor = (0, 0);
            return;
        }
        self.cursor.0 = apply(self.cursor.0, nrows, NavCommand::Move(rows as i64));
        self.cursor.1 = apply_clamped(self.cursor.1, ncols, NavCommand::Move(cols as i64));
    }
```

- [ ] **Step 4: Run the market-data suite**

Run: `cargo test -p geode-marketdata`
Expected: all PASS. `cursor_moves_with_counts_and_scrolls` still passes (`down` ×3 from 0 → 3; `down` ×9 clamps at 4; the counted deltas are > 1). The `ACTIONS`/fragment mirror tests (content.rs ~204-291) pass because both lists changed together.

- [ ] **Step 5: Harness entry**

```sh
run_mutation "marketdata: the row axis wraps a bare step, the column axis never does (spec §20.5)" \
  crates/geode-marketdata/src/tile.rs \
  '        self.cursor.0 = apply(self.cursor.0, nrows, NavCommand::Move(rows as i64));' \
  '        self.cursor.0 = apply_clamped(self.cursor.0, nrows, NavCommand::Move(rows as i64));' \
  geode-marketdata \
  a_bare_row_step_wraps_and_the_full_page_keys_move_ten
```

- [ ] **Step 6: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-marketdata --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: bare j/k wrap, ctrl+f/ctrl+b/pageup/pagedown at ±10, columns clamp (spec §20.5)"
```

---

### Task 3: Palette, picker and as-of take the full nav set through one rule; palette `tab` reclaimed

**Files:**
- Modify: `crates/geode-shell/src/palette.rs:600-611` (`move_selection`), panel at ~1034
- Modify: `crates/geode-shell/src/shell/palette_ctl.rs:250-312` (`handle_palette_key`)
- Modify: `crates/geode-shell/src/shell/picker.rs:262-269`, `483-497` (`nav_delta`), `509-521`, `566-575`
- Modify: `crates/geode-shell/src/shell/asof_view.rs:168-179`, `292-302`, tests 574-582
- Modify: `crates/geode-shell/src/shell/dialog.rs:355-364` (reclaims)
- Test: `crates/geode-shell/src/shell/tests/{palette,picker,asof}.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `vimnav::apply` (Task 1), `listfilter::nav_command`.
- Produces: nothing new; `PaletteState::move_selection(delta: i32)` keeps its signature (14 pure tests call it) and becomes a delegate to `apply`.

- [ ] **Step 1: Write the failing window tests**

`shell/tests/asof.rs` — add (use whatever open helper the file already has for the as-of dialog; it dispatches `frame::as_of` and delivers presets — read the top of the file and reuse its fixture):

```rust
/// Spec §20.5: every list takes the whole nav set through one rule.
/// `ctrl+n`/`ctrl+p` used to be dead here and `up` at row 0 wrapped;
/// now `ctrl+n` moves, `up` at 0 still wraps (a bare ±1), and `ctrl+u`
/// at 0 clamps.
#[gpui::test]
fn the_as_of_list_takes_ctrl_n_and_clamps_a_page_step(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_as_of_with_presets(cx, 3); // ≥3 presets
    let selected = |cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| s.as_of_dialog.as_ref().unwrap().selected)
    };
    cx.simulate_keystrokes("ctrl-n");
    assert_eq!(selected(&cx), 1, "ctrl+n is down");
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(selected(&cx), 0);
    cx.simulate_keystrokes("up");
    assert_eq!(selected(&cx), 2, "a bare step wraps");
    cx.simulate_keystrokes("ctrl-u");
    assert_eq!(selected(&cx), 0, "a page step clamps");
}
```

If the file has no helper that opens the dialog with N presets, write `open_as_of_with_presets` by copying the preset-publishing preamble of the nearest existing test (search the file for `Publish` — presets come from `frame` generations published through `crate::frame::Publish`).

`shell/tests/picker.rs` — add after `escape_cancels_without_touching_the_scope`:

```rust
/// Spec §20.5 on the picker: `ctrl+d` moves five and clamps, `up` at
/// row 0 wraps — the same `nav_command` + `apply` pair every other list
/// routes through, replacing the picker's own four-key `nav_delta`.
#[gpui::test]
fn the_values_list_takes_the_full_nav_set(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let values: Vec<(String, u64)> = (0..8).map(|i| (format!("BK00{i}"), 1)).collect();
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 1,
                column: "book".into(),
                values: Ok(values),
            },
            cx,
        )
    });
    vcx.run_until_parked();
    let selected = |vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| s.picker.as_ref().unwrap().selected)
    };
    vcx.simulate_keystrokes("ctrl-d");
    assert_eq!(selected(&vcx), 5, "ctrl+d moves five");
    vcx.simulate_keystrokes("ctrl-d");
    assert_eq!(selected(&vcx), 7, "and clamps at the end");
    vcx.simulate_keystrokes("down");
    assert_eq!(selected(&vcx), 0, "a bare down at the end wraps");
    vcx.simulate_keystrokes("up");
    assert_eq!(selected(&vcx), 7, "and a bare up at the top wraps");
}
```

`shell/tests/palette.rs` — add:

```rust
/// Spec §20.5: `tab` inside the palette is reclaimed so gpui-component's
/// `Root` cannot cycle focus off the query field while the palette is
/// open — `dialog::init_reclaimed_keybindings` binds it to `NoAction` in
/// the `GeodePalette` context, the modal's own treatment.
#[gpui::test]
fn tab_in_the_palette_leaves_the_query_field_focused(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    let focused = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| {
            shell.read(cx).palette_input.read(cx).focus_handle(cx).is_focused(window)
        })
    };
    assert!(focused(&mut cx), "the palette opens with its field focused");
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(focused(&mut cx), "tab must not move focus off the palette's field");
    assert!(shell.read_with(&cx, |s, _| s.palette.is_some()), "and the palette is still open");
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-shell the_as_of_list_takes_ctrl_n the_values_list_takes_the_full_nav_set tab_in_the_palette`
Expected: as-of FAILS at `ctrl+n is down` (selected stays 0); picker FAILS at `ctrl+d moves five`; the palette test may pass or fail depending on `Root`'s cycling in the test window — record which, and keep the test either way (it pins the reclaim).

- [ ] **Step 3: Implement — palette**

`palette.rs:600-611`:

```rust
    /// Move the selection by `delta` — `vimnav::apply`'s rule (spec
    /// §20.5): a bare ±1 wraps, anything larger clamps. Kept as a method
    /// because the pure tests below and `handle_palette_key` call it by
    /// this name; the arithmetic itself lives in one place now.
    pub fn move_selection(&mut self, delta: i32) {
        let len = self.filtered.len();
        self.selected = crate::vimnav::apply(
            self.selected,
            len,
            crate::vimnav::NavCommand::Move(delta as i64),
        );
    }
```

`palette_ctl.rs` `handle_palette_key` (250-312): delete the four `"up"`/`"down"`/`"p" if mods.control`/`"n" if mods.control` arms and their `mods` binding; the fallback arm already routes `listfilter::nav_command` through `vimnav::apply` and now covers them (`nav_command` maps `up`/`down`/`ctrl+p`/`ctrl+n` to `Move(±1)`, which `apply` wraps). Replace the arm's comment block with:

```rust
            // Everything but escape/enter: the whole `listfilter::nav_command`
            // set through `vimnav::apply` — a bare ±1 wraps, a page step
            // clamps (spec §20.5), the same rule every list and tile has.
            // A bare typed character lands here too and must stay a true
            // no-op — deliberately not `cx.stop_propagation()`, so the
            // window's text-input phase still delivers it to the focused
            // `palette_input` (see this method's doc comment).
```

Update `handle_palette_key`'s doc comment (230-247) to drop the "named arms wrap, fallback clamps" paragraph. Add the palette key context: in `palette::render` (~line 1034, the panel `div` carrying `debug_selector(|| "palette-panel")`) add `.key_context("GeodePalette")`. In `dialog::init_reclaimed_keybindings` add:

```rust
        // Spec §20.5: the palette overlay is not a `GeodeModal`, so it
        // never had the reclaim — `Root` could cycle focus off the query
        // field on `tab`.
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodePalette")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodePalette")),
```

- [ ] **Step 4: Implement — picker**

Delete `PickerState::move_selection` (262-269) and `nav_delta` (483-497). In `handle_columns_key` replace the `nav_delta` block with:

```rust
    if let Some(cmd) = listfilter::nav_command(ks) {
        let query = shell
            .picker
            .as_ref()
            .map(|p| p.query.clone())
            .unwrap_or_default();
        let len = PickerState::columns(&shell.pickable, &query).len();
        if let Some(p) = shell.picker.as_mut() {
            p.selected = vimnav::apply(p.selected, len, cmd);
        }
        cx.notify();
        return true;
    }
```

and in `handle_values_key`:

```rust
    if let Some(cmd) = listfilter::nav_command(ks) {
        let len = shell.picker.as_ref().map(|p| p.shown().len()).unwrap_or(0);
        if let Some(p) = shell.picker.as_mut() {
            p.selected = vimnav::apply(p.selected, len, cmd);
        }
        sync_picker_scroll(shell);
        cx.notify();
        return true;
    }
```

Add `use crate::{listfilter, vimnav};` to picker.rs's imports. Any pure test in picker.rs's `mod tests` that called `move_selection` is rewritten to call `vimnav::apply` on `selected` directly (or deleted if it only tested wrapping — `apply_wraps_a_single_step_at_both_ends` now owns that claim).

- [ ] **Step 5: Implement — as-of**

Delete the free `move_selection` (asof_view.rs:168-179) and its three pure tests (574-582). Replace the `up`/`down` block in `handle_key` (292-302):

```rust
    if let Some(cmd) = listfilter::nav_command(ks) {
        let len = shell
            .as_of_dialog
            .as_ref()
            .map(|state| cached_presets(state, shell.frame.read(cx)).len())
            .unwrap_or(0);
        if let Some(state) = shell.as_of_dialog.as_mut() {
            state.selected = vimnav::apply(state.selected, len, cmd);
        }
        cx.notify();
        return true;
    }
```

Add `use crate::{listfilter, vimnav};`. Update the `handle_key` doc comment's "`up`/`down` move `selected`" sentence to "every `listfilter::nav_command` key moves `selected` through `vimnav::apply` (spec §20.5)".

- [ ] **Step 6: Run the three test files and the pure suites**

Run: `cargo test -p geode-shell palette picker asof`
Expected: all PASS, including the 14 pure `move_selection_*` tests in palette.rs (their ±1 expectations are unchanged by the delegate).

- [ ] **Step 7: Harness entries**

```sh
run_mutation "picker: the values list moves through vimnav::apply, not a private ±1 (spec §20.5)" \
  crates/geode-shell/src/shell/picker.rs \
  '            p.selected = vimnav::apply(p.selected, len, cmd);' \
  '            p.selected = vimnav::apply_clamped(p.selected, len, cmd);' \
  geode-shell \
  the_values_list_takes_the_full_nav_set

run_mutation "asof: the preset list takes the full nav set (spec §20.5)" \
  crates/geode-shell/src/shell/asof_view.rs \
  '    if let Some(cmd) = listfilter::nav_command(ks) {' \
  '    if let Some(cmd) = listfilter::nav_command(ks).filter(|c| matches!(c, vimnav::NavCommand::Move(1 | -1))) {' \
  geode-shell \
  the_as_of_list_takes_ctrl_n_and_clamps_a_page_step

run_mutation "palette: tab is reclaimed inside the palette (spec §20.5)" \
  crates/geode-shell/src/shell/dialog.rs \
  '        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodePalette")),' \
  '        gpui::KeyBinding::new("f24", gpui::NoAction, Some("GeodePalette")),' \
  geode-shell \
  tab_in_the_palette_leaves_the_query_field_focused
```

**Anchor check:** the picker entry's `from` line appears TWICE in picker.rs (columns and values arms) — `--anchors-only` will report `AMBIG`. Make the values arm's line unique by writing it as `p.selected = vimnav::apply(p.selected, len, cmd); // values` and anchor on that. If the palette reclaim test passed *before* the reclaim existed (Step 2), that mutation will SURVIVE — then drop the entry and record in the commit message that the reclaim is a display-check item, as §20.5 already says.

- [ ] **Step 8: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "lists: palette, picker and as-of route every nav key through vimnav::apply; palette reclaims tab (spec §20.5)"
```

---

### Task 4: The picker walks the escape ladder; a Values row click selects, its tick toggles

**Files:**
- Modify: `crates/geode-shell/src/shell/picker.rs` — `handle_values_key`, new `back_to_columns`, `hints`, `build_values` row (~820-845), module doc
- Test: `crates/geode-shell/src/shell/tests/picker.rs` (`escape_cancels_without_touching_the_scope` updated; two new tests)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `fn picker::back_to_columns(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>)`.

- [ ] **Step 1: Update the existing escape test and add two**

In `escape_cancels_without_touching_the_scope`, after `vcx.simulate_keystrokes("escape");` replace the two `assert!`s on `modal`/`picker` with:

```rust
    assert!(
        shell.read_with(&vcx, |s, _| matches!(
            s.picker.as_ref().map(|p| &p.stage),
            Some(picker::Stage::Columns)
        )),
        "spec §20.2: escape from Values steps back to Columns first"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().ticked.len()),
        0,
        "the ticks are dropped on the way back"
    );
    let book_ix = shell.read_with(&vcx, |s, _| {
        s.pickable.iter().position(|p| p.column == "book").unwrap()
    });
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().selected),
        book_ix,
        "with the cursor on the column just left"
    );
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_none()),
        "and a second escape closes, clearing the picker like every other dialog"
    );
```

(`picker` needs importing in the test file: `use crate::shell::picker;` — check the file's existing imports.) Then add:

```rust
/// §20.3's split applied here: a click on a Values row SELECTS it; the
/// tick glyph is the click target that toggles, as it is on the object
/// dialog's list rows. Until now the whole row toggled on one click.
#[gpui::test]
fn a_values_row_click_selects_and_only_the_tick_toggles(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 1,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 1), ("BK001".into(), 2)]),
            },
            cx,
        )
    });
    vcx.run_until_parked();
    let ticked = |vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| s.picker.as_ref().unwrap().ticked.len())
    };
    assert_eq!(ticked(&vcx), 0, "nothing pre-ticked: the scope is empty");

    // The row's label text, well right of the tick.
    let row = vcx.debug_bounds("picker-value-BK001").expect("row paints");
    vcx.simulate_click(
        gpui::point(row.origin.x + row.size.width / 2.0, row.center().y),
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().selected),
        1,
        "the row click moved the cursor"
    );
    assert_eq!(ticked(&vcx), 0, "and toggled nothing");

    let tick = vcx.debug_bounds("picker-tick-BK001").expect("the tick paints");
    vcx.simulate_click(tick.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(ticked(&vcx), 1, "the tick click is `tab`");
}

/// The Values footer says `back`, the Columns footer says `close`.
#[test]
fn the_values_hint_says_escape_goes_back() {
    use crate::shell::picker::{Hint, Stage, hints};
    let values = hints(&Stage::Values { column: "book".into() });
    let after_escape = values
        .windows(2)
        .find(|w| w[0] == Hint::Key("escape"))
        .map(|w| w[1]);
    assert_eq!(after_escape, Some(Hint::Text("back")));
    let columns = hints(&Stage::Columns);
    let after_escape = columns
        .windows(2)
        .find(|w| w[0] == Hint::Key("escape"))
        .map(|w| w[1]);
    assert_eq!(after_escape, Some(Hint::Text("close")));
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-shell escape_cancels_without_touching_the_scope a_values_row_click_selects the_values_hint_says`
Expected: all three FAIL (escape closes outright; the row click ticks; the hint reads `close`).

- [ ] **Step 3: Implement `back_to_columns` and the escape arm**

In picker.rs, after `commit_column`:

```rust
/// `escape` on the Values stage (spec §20.2, `EscapeStep::PreviousStage`):
/// back to `Columns` with the ticks and the Values query dropped and the
/// cursor on the column just left — `commit_column` walked backwards. A
/// no-op when the stage is already `Columns`, whose own `escape` falls
/// through to the shell's modal branch and closes. Nothing is applied on
/// the way back: `PickerState::apply` is still reached only from `enter`.
fn back_to_columns(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(Stage::Values { column }) = shell.picker.as_ref().map(|p| p.stage.clone()) else {
        return;
    };
    let position = shell
        .pickable
        .iter()
        .position(|p| p.column == column)
        .unwrap_or(0);
    if let Some(p) = shell.picker.as_mut() {
        p.stage = Stage::Columns;
        p.selected = position;
        p.query.clear();
        p.values = None;
        p.ticked.clear();
        p.ticks_touched = false;
    }
    shell.dialog_input.update(cx, |input, cx| {
        input.set_value("", window, cx);
    });
    cx.notify();
}
```

(Check `PickerState`'s field names for the values slot — the struct at picker.rs:126-160 names them `values`, `ticked`, `ticks_touched`; use exactly those.) In `handle_values_key`, as the FIRST check:

```rust
    // Modifier-agnostic, like the shell's own modal close: a
    // `shift+escape` must not be a key this stage claims and drops.
    if ks.key == "escape" {
        back_to_columns(shell, window, cx);
        return true;
    }
```

Update `handle_key`'s doc comment (580-584) — `escape` is now claimed by the Values arm and falls through only from Columns. In `hints`, change the Values stage's final `Hint::Text("close")` to `Hint::Text("back")`. Update the module doc's §-style notes to record the ladder.

- [ ] **Step 4: Implement the row/tick click split**

In `build_values` (the `uniform_list` closure, ~806-845), give the tick its own selector and handler, and make the row's handler select only:

```rust
                        let value_for_tick = value.clone();
                        let tick_entity = entity.clone();
                        let tick = if is_ticked {
                            div().text_color(primary).child("✓")
                        } else {
                            div().text_color(muted).child("·")
                        }
                        .debug_selector(move || format!("picker-tick-{value_for_tick}"))
                        // §20.3's rule on this list: the tick is `tab`'s
                        // mouse form; the row is the cursor's.
                        .on_mouse_down(gpui::MouseButton::Left, move |_event, _window, cx| {
                            cx.stop_propagation();
                            tick_entity.update(cx, |shell, cx| {
                                if let Some(p) = shell.picker.as_mut() {
                                    p.selected = i;
                                    p.toggle_selected();
                                }
                                sync_picker_scroll(shell);
                                cx.notify();
                            });
                        });
```

and the row's `on_mouse_down` body becomes:

```rust
                            .on_mouse_down(gpui::MouseButton::Left, move |_event, _window, cx| {
                                entity.update(cx, |shell, cx| {
                                    if let Some(p) = shell.picker.as_mut() {
                                        p.selected = i;
                                    }
                                    sync_picker_scroll(shell);
                                    cx.notify();
                                });
                            })
```

`div()` needs `.id(...)`? No — `on_mouse_down` on a plain `div` works without an id (the row already does it). Update the comment at picker.rs:462 that describes the click.

- [ ] **Step 5: Run picker tests**

Run: `cargo test -p geode-shell picker`
Expected: all PASS.

- [ ] **Step 6: Harness entries**

```sh
run_mutation "picker: escape from Values steps back to Columns instead of closing (spec §20.2)" \
  crates/geode-shell/src/shell/picker.rs \
  '        back_to_columns(shell, window, cx);' \
  '        let _ = (window, &cx); return false;' \
  geode-shell \
  escape_cancels_without_touching_the_scope

run_mutation "picker: a values row click selects, only the tick toggles (spec §20.3)" \
  crates/geode-shell/src/shell/picker.rs \
  '                                        p.selected = i;
                                    }
                                    sync_picker_scroll(shell);' \
  '                                        p.selected = i;
                                        p.toggle_selected();
                                    }
                                    sync_picker_scroll(shell);' \
  geode-shell \
  a_values_row_click_selects_and_only_the_tick_toggles
```

If the multi-line anchor matches both handlers, anchor on the row handler's `// row` comment line instead — add `// row: select only` above `p.selected = i;` in the row handler and use that as the anchor.

- [ ] **Step 7: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "picker: escape walks the ladder (Values → Columns → close); a row click selects and the tick toggles (spec §20.2, §20.3)"
```

---

### Task 5: Shared `dialog::ConfirmAnswer` and `dialog::confirm_row` — refactor the object dialog onto them

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs` (new items after `hint_rows`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs:1103-1116` (armed block), `3829-3910` (`confirm_row`)
- Test: `dialog.rs` `mod tests` (pure); existing `objectdialog.rs` window tests must stay green unchanged

**Interfaces:**
- Produces:
  ```rust
  pub enum ConfirmAnswer { Yes, No }
  impl ConfirmAnswer { pub fn from_key(ks: &Keystroke) -> Option<ConfirmAnswer> }
  pub type ConfirmHandler = Rc<dyn Fn(&mut ShellView, &mut Window, &mut Context<ShellView>)>;
  pub(crate) fn confirm_row(prompt: String, yes_label: &'static str, selector_prefix: &'static str, entity: &Entity<ShellView>, on_yes: ConfirmHandler, on_no: ConfirmHandler, cx: &mut App) -> AnyElement
  ```
  Selectors: `"{prefix}-confirm"`, `"{prefix}-confirm-yes"`, `"{prefix}-confirm-no"` (object dialog passes `"objectdialog"`, so its existing tests keep matching).

- [ ] **Step 1: Write the failing pure test in `dialog.rs`**

```rust
#[cfg(test)]
mod confirm_tests {
    use super::ConfirmAnswer;
    use crate::keymap::{Keystroke, Modifiers};

    fn ks(key: &str, mods: Modifiers) -> Keystroke {
        Keystroke { mods, key: key.to_string() }
    }
    const SHIFT: Modifiers = Modifiers { ctrl: false, alt: false, shift: true, cmd: false };

    /// Spec §20.1: one router for every destructive question. `y`/`enter`
    /// bare say yes, `n` bare and `escape` with ANY modifiers say no, and
    /// everything else is `None` — claimed and dropped by the caller.
    #[test]
    fn the_confirm_router_answers_four_keys_and_drops_the_rest() {
        assert_eq!(ConfirmAnswer::from_key(&ks("y", Modifiers::NONE)), Some(ConfirmAnswer::Yes));
        assert_eq!(ConfirmAnswer::from_key(&ks("enter", Modifiers::NONE)), Some(ConfirmAnswer::Yes));
        assert_eq!(ConfirmAnswer::from_key(&ks("n", Modifiers::NONE)), Some(ConfirmAnswer::No));
        assert_eq!(ConfirmAnswer::from_key(&ks("escape", Modifiers::NONE)), Some(ConfirmAnswer::No));
        assert_eq!(ConfirmAnswer::from_key(&ks("escape", SHIFT)), Some(ConfirmAnswer::No));
        assert_eq!(ConfirmAnswer::from_key(&ks("y", SHIFT)), None, "Y is not y");
        assert_eq!(ConfirmAnswer::from_key(&ks("enter", Modifiers::CTRL)), None);
        assert_eq!(ConfirmAnswer::from_key(&ks("d", Modifiers::NONE)), None);
    }
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p geode-shell confirm_tests`
Expected: compile error, `ConfirmAnswer` not found.

- [ ] **Step 3: Implement the shared pieces in `dialog.rs`**

```rust
/// The answer to a destructive question, on every surface that asks one
/// (spec §20.1): the object dialog's `d`/`r`/`o` and the keybindings
/// dialog's `d`/`r`. `None` means the key is neither answer — the caller
/// claims and drops it, because a stray letter must not act on the
/// object behind an unanswered question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAnswer {
    Yes,
    No,
}

impl ConfirmAnswer {
    /// `y`/`enter` bare are yes; `n` bare and `escape` with any modifiers
    /// are no (modifier-agnostic on `escape` for the same reason every
    /// dialog's close is: `shift+escape` must not be a key that visibly
    /// does nothing).
    pub fn from_key(ks: &Keystroke) -> Option<ConfirmAnswer> {
        let bare = ks.mods == Modifiers::NONE;
        match ks.key.as_str() {
            "y" | "enter" if bare => Some(ConfirmAnswer::Yes),
            "n" if bare => Some(ConfirmAnswer::No),
            "escape" => Some(ConfirmAnswer::No),
            _ => None,
        }
    }
}

/// What a confirm button runs. `Rc` so the two closures can be cloned
/// into gpui's `'static` click handlers.
pub type ConfirmHandler = Rc<dyn Fn(&mut ShellView, &mut Window, &mut Context<ShellView>)>;

/// The confirm block every dialog paints in place of its action bar
/// while a destructive question stands (spec §20.1): the question in
/// `theme.warning`, a `danger` button labelled with the verb, and a ghost
/// `Cancel`. Both handlers are mouse-side answers and so end in
/// [`sync_dialog_text`] here, once, rather than in each caller (spec
/// §16.1: a click never passes through the key path). Selectors:
/// `"{selector_prefix}-confirm"`, `-yes`, `-no`.
pub(crate) fn confirm_row(
    prompt: String,
    yes_label: &'static str,
    selector_prefix: &'static str,
    entity: &Entity<ShellView>,
    on_yes: ConfirmHandler,
    on_no: ConfirmHandler,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let go_ahead = entity.clone();
    let leave_it = entity.clone();
    let block = format!("{selector_prefix}-confirm");
    let yes_sel = format!("{selector_prefix}-confirm-yes");
    let no_sel = format!("{selector_prefix}-confirm-no");
    let yes_id = SharedString::from(yes_sel.clone());
    let no_id = SharedString::from(no_sel.clone());
    h_flex()
        .w_full()
        .gap_3()
        .items_center()
        .debug_selector(move || block.clone())
        .child(div().text_sm().text_color(theme.warning).child(prompt))
        .child(
            div().debug_selector(move || yes_sel.clone()).child(
                Button::new(yes_id)
                    .small()
                    .danger()
                    .label(yes_label)
                    .on_click(move |_event, window, cx| {
                        let on_yes = on_yes.clone();
                        go_ahead.update(cx, |shell, cx| {
                            on_yes(shell, window, cx);
                            sync_dialog_text(shell, window, cx);
                        });
                    }),
            ),
        )
        .child(
            div().debug_selector(move || no_sel.clone()).child(
                Button::new(no_id)
                    .small()
                    .ghost()
                    .label("Cancel")
                    .on_click(move |_event, window, cx| {
                        let on_no = on_no.clone();
                        leave_it.update(cx, |shell, cx| {
                            on_no(shell, window, cx);
                            sync_dialog_text(shell, window, cx);
                            cx.notify();
                        });
                    }),
            ),
        )
        .into_any_element()
}
```

- [ ] **Step 4: Run the pure test**

Run: `cargo test -p geode-shell confirm_tests`
Expected: PASS.

- [ ] **Step 5: Refactor the object dialog onto them (no behaviour change)**

`objectdialog/render.rs:1103-1116` armed block becomes:

```rust
    if let Some(confirm) = armed {
        match dialog::ConfirmAnswer::from_key(ks) {
            Some(dialog::ConfirmAnswer::Yes) => {
                disarm_confirm(shell);
                run_confirmed(shell, confirm, cx);
            }
            Some(dialog::ConfirmAnswer::No) => disarm_confirm(shell),
            // Claimed and dropped: while a destructive question is on
            // screen, a stray letter must not act on the object behind it.
            None => {}
        }
        cx.notify();
        return true;
    }
```

Replace the local `confirm_row` (3829-3910) with a thin adapter that keeps its call site's signature:

```rust
/// The confirm block, which **replaces** the action bar rather than
/// joining it — `dialog::confirm_row` (spec §20.1) with this dialog's
/// question, verb and handlers. The yes handler's `run_confirmed`
/// delete/revert arm walks all the way back to browse through
/// `leave_edit`, which is why the shared row syncs after it.
fn confirm_row(
    confirm: Confirm,
    name: &str,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    let yes_label = match confirm {
        Confirm::Delete => "Delete",
        Confirm::Revert => "Revert",
        Confirm::Overwrite => "Overwrite",
    };
    let on_yes: dialog::ConfirmHandler = Rc::new(|shell, _window, cx| {
        let armed = shell
            .object_dialog
            .as_ref()
            .and_then(|state| state.draft.as_ref())
            .and_then(|draft| draft.confirm);
        if let Some(confirm) = armed {
            disarm_confirm(shell);
            run_confirmed(shell, confirm, cx);
        }
    });
    let on_no: dialog::ConfirmHandler = Rc::new(|shell, _window, _cx| disarm_confirm(shell));
    dialog::confirm_row(confirm.prompt(name), yes_label, "objectdialog", entity, on_yes, on_no, cx)
}
```

Note the original was `.w(px(WIDTH))`; the shared row is `w_full()`, which inside the object dialog's fixed-width column paints the same. If `Rc` is not imported in render.rs, add `use std::rc::Rc;`.

- [ ] **Step 6: Run the object dialog tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: all PASS — `confirming_with_the_mouse_while_filtering_empties_the_field`, the `d`/`r` confirm tests and the `objectdialog-confirm*` selectors are unchanged.

- [ ] **Step 7: Harness — re-anchor the existing entries that pointed at the old lines**

Run: `zsh scripts/mutation-check.sh --anchors-only`. Every `ANCHOR`/stale line naming `objectdialog/render.rs` and the old `"enter" | "y"` / `"escape" || (bare && ks.key == "n")` text is re-anchored to the new `Some(dialog::ConfirmAnswer::Yes) =>` / `Some(dialog::ConfirmAnswer::No) =>` lines with the same mutation intent (e.g. mutate `Yes` arm to `disarm_confirm(shell);` alone). Add one for the router:

```sh
run_mutation "dialog: the confirm router treats a modified escape as no (spec §20.1)" \
  crates/geode-shell/src/shell/dialog.rs \
  '            "escape" => Some(ConfirmAnswer::No),' \
  '            "escape" if bare => Some(ConfirmAnswer::No),' \
  geode-shell \
  the_confirm_router_answers_four_keys_and_drops_the_rest
```

- [ ] **Step 8: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "dialog: one ConfirmAnswer router and one confirm_row, the object dialog refactored onto them (spec §20.1)"
```

---

### Task 6: Keybindings `d`/`r` ask first and have buttons

**Files:**
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs` — `KeybindingsState` (327+), `handle_key` (673+, normal-mode `Verb('d'|'r')` arms at 788-796), `on_row_clicked` (913), `build` (1269+: hints 1417-1454, assembly ~1518-1522)
- Modify: `crates/geode-shell/src/shell/dialog.rs:613-622` (`enter_filter_by_mouse` armed guard)
- Test: `crates/geode-shell/src/shell/tests/keybindings_dialog.rs` — every `d`/`r` test gains a `y`; new tests
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `dialog::{ConfirmAnswer, ConfirmHandler, confirm_row}` (Task 5).
- Produces: `pub enum KeybindingConfirm { Unbind, Reset }`, `KeybindingsState.confirm: Option<KeybindingConfirm>`, selectors `keybindings-confirm[-yes|-no]`, `keybindings-action-d`, `keybindings-action-r`.

- [ ] **Step 1: Update the existing write tests and add the new ones**

In `tests/keybindings_dialog.rs`, every test that presses `d` or `r` and then asserts a file write or a "silencing …"/"removing …" notice (`d_unbinds_the_selected_binding`, `d_on_a_user_layer_binding_removes_it_rather_than_shadowing_it`, `r_resets_a_user_override_by_removing_it`, `d_acknowledges_the_write_immediately_and_names_the_way_back`, `d_on_a_contexted_binding_does_not_promise_the_retype_recovery`, `r_acknowledges_the_write_it_spawned`, `d_does_not_claim_a_write_it_has_not_confirmed`) changes `simulate_keystrokes("d")` → `simulate_keystrokes("d y")` and `("r")` → `("r y")`. The tests that expect a NOTICE and no write (`r_on_a_row_with_no_user_override_says_so_and_writes_nothing`, `d_on_an_unbound_row_says_so_and_writes_nothing`, `r_on_a_silenced_row_names_the_recovery_instead_of_denying_the_override`) stay on a bare `d`/`r` — those rows arm nothing. Then add:

```rust
/// Spec §20.1: `d` arms a question and writes nothing until `y`; `n`
/// withdraws it; the confirm row paints and the action bar is gone.
#[gpui::test]
fn d_asks_before_writing_and_n_withdraws(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);

    vcx.simulate_keystrokes("d");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm.is_some()),
        "d arms the question"
    );
    assert!(vcx.debug_bounds("keybindings-confirm").is_some(), "and it paints");
    assert!(
        vcx.debug_bounds("keybindings-action-d").is_none(),
        "the action bar is replaced by the question"
    );
    assert!(
        !dir.path().join("keymap.toml").exists(),
        "nothing is written while the question stands"
    );

    // A stray verb is claimed and dropped while armed.
    vcx.simulate_keystrokes("r");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm
            == Some(keybindings_view::KeybindingConfirm::Unbind)),
        "r under an armed d neither re-arms nor acts"
    );

    vcx.simulate_keystrokes("n");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm.is_none()),
        "n withdraws it"
    );
    assert!(vcx.debug_bounds("keybindings-confirm").is_none());
    assert!(!dir.path().join("keymap.toml").exists());
}

/// A row click while a question stands is claimed and dropped — the
/// object dialog's tick-click rule (§18.9.2) on this surface — and so is
/// the frozen filter row's.
#[gpui::test]
fn a_row_click_while_a_confirm_is_armed_is_dropped(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("d");
    vcx.run_until_parked();

    let row = vcx.debug_bounds("keybindings-row-0").expect("a row paints");
    vcx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| {
            let state = s.keybindings.as_ref().unwrap();
            state.confirm.is_some() && state.listening.is_none()
        }),
        "the click neither retargeted nor started a capture"
    );

    let frozen = vcx.debug_bounds("dialog-filter-frozen").expect("frozen row paints");
    vcx.simulate_mouse_down(
        gpui::point(frozen.origin.x + gpui::px(20.0), frozen.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| {
            let state = s.keybindings.as_ref().unwrap();
            state.confirm.is_some() && state.mode == DialogMode::Normal
        }),
        "the frozen-row click did not enter filter mode over an open question"
    );
}

/// The two verbs are buttons too, and the button arms exactly as the
/// key does; the yes button writes.
#[gpui::test]
fn the_unbind_button_arms_and_the_yes_button_writes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);
    let (_, bound) = selected_row(&shell, &vcx);
    let (key, _) = bound.expect("bound");

    let button = vcx.debug_bounds("keybindings-action-d").expect("the unbind button paints");
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("keybindings-confirm").is_some(), "the button arms");

    let yes = vcx.debug_bounds("keybindings-confirm-yes").expect("yes paints");
    vcx.simulate_click(yes.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("written");
    assert!(text.contains(&format!("\"{key}\" = \"none\"")), "{text}");
    assert!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm.is_none()),
        "and the question is gone"
    );
}
```

(`DialogMode` and `keybindings_view` are imported at the top of the test file; check and add if not.)

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-shell keybindings_dialog`
Expected: the three new tests fail to compile (`confirm` field / `KeybindingConfirm` missing); the updated `d y` tests would pass trivially against the old code (the `y` is dropped), which is why the new tests carry the claim.

- [ ] **Step 3: Implement the state and key routing**

`keybindings_view.rs` — beside `KeybindingsState`:

```rust
/// The destructive question `d`/`r` arm (spec §20.1): the same
/// ask-then-act shape the object dialog's `Confirm` has, on this dialog's
/// two verbs. `Unbind` writes the `"none"` shadow (or removes the user's
/// own entry); `Reset` removes the user override. Armed only where the
/// write would actually happen — `d` on an unbound row and `r` on a row
/// with no user override keep giving their notices unarmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingConfirm {
    Unbind,
    Reset,
}

impl KeybindingConfirm {
    pub fn prompt(self, title: &str, key: &str) -> String {
        match self {
            KeybindingConfirm::Unbind => format!("Silence {key} for '{title}'?"),
            KeybindingConfirm::Reset => format!("Remove your {key} override on '{title}'?"),
        }
    }
    pub fn yes_label(self) -> &'static str {
        match self {
            KeybindingConfirm::Unbind => "Unbind",
            KeybindingConfirm::Reset => "Reset",
        }
    }
}
```

Add to `KeybindingsState`:

```rust
    /// `Some` while `d`/`r`'s question stands (spec §20.1). Every other
    /// key is claimed and dropped until it is answered; a row click, the
    /// frozen-row click and the action buttons are dropped too.
    pub confirm: Option<KeybindingConfirm>,
```

and `confirm: None` in `KeybindingsState::new`. In `handle_key`, immediately after the `listening` block (before `if state.mode == DialogMode::Normal`):

```rust
    if let Some(confirm) = state.confirm {
        match dialog::ConfirmAnswer::from_key(ks) {
            Some(dialog::ConfirmAnswer::Yes) => {
                state.confirm = None;
                let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
                state.notice = match confirm {
                    KeybindingConfirm::Unbind => unbind_selected(row, &user_dir, cx),
                    KeybindingConfirm::Reset => reset_selected(row, &user_dir, cx),
                };
            }
            Some(dialog::ConfirmAnswer::No) => state.confirm = None,
            // Claimed and dropped while the question stands.
            None => {}
        }
        cx.notify();
        return true;
    }
```

Replace the two `Verb` arms (788-796):

```rust
            // The two write verbs (spec §8, §20.1): each ARMS a question
            // where a write would happen, and gives its notice unarmed
            // where none would — `d` on an unbound row, `r` on a row with
            // no user override — so the answer `y` runs is always a real
            // write. `unbind_selected`/`reset_selected` are the writers,
            // reached from the armed block above.
            NormalCommand::Verb('d') => {
                let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
                if row.is_some_and(|r| r.current.is_some()) {
                    state.confirm = Some(KeybindingConfirm::Unbind);
                } else {
                    state.notice = unbind_selected(row, &user_dir, cx);
                }
            }
            NormalCommand::Verb('r') => {
                let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
                if row.is_some_and(|r| r.current.as_ref().is_some_and(|b| b.layer == Layer::User)) {
                    state.confirm = Some(KeybindingConfirm::Reset);
                } else {
                    state.notice = reset_selected(row, &user_dir, cx);
                }
            }
```

`on_row_clicked`: after the notice clear, add

```rust
    // Spec §20.1: a click is claimed and dropped while a question stands —
    // the object dialog's tick-click rule (§18.9.2) on this surface.
    if state.confirm.is_some() {
        return;
    }
```

`dialog::enter_filter_by_mouse`'s keybindings arm:

```rust
    if let Some(state) = shell.keybindings.as_mut() {
        // Spec §20.1: not over an open question.
        if state.confirm.is_some() {
            return;
        }
        state.listening = None;
        state.mode = DialogMode::Filter;
    }
```

- [ ] **Step 4: Implement the footer, the action bar and the confirm row**

In `build`, the `hints` expression gains a first branch:

```rust
    let hints: Vec<Hint> = if state.confirm.is_some() {
        vec![
            Hint::prose(HintRow::Go, "this needs an answer first"),
            Hint::new(HintRow::Go, &["enter"], "go ahead"),
            Hint::new(HintRow::Go, &["escape"], "leave it alone"),
        ]
    } else if state.listening.is_some() {
```

Add, before `build`:

```rust
/// A verb pressed with the mouse (spec §20.1) — one door with the key, so
/// a button and its letter can never differ: arms, never writes.
fn press_verb(shell: &mut ShellView, key: &str, window: &mut Window, cx: &mut Context<ShellView>) {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let user_dir = shell.user_dir.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return;
    };
    if state.notice.take().is_some() {
        cx.notify();
    }
    if state.confirm.is_some() || state.listening.is_some() {
        return;
    }
    let visible = visible_rows(state, &rows);
    let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
    match key {
        "d" if row.is_some_and(|r| r.current.is_some()) => {
            state.confirm = Some(KeybindingConfirm::Unbind);
        }
        "d" => state.notice = unbind_selected(row, &user_dir, cx),
        "r" if row.is_some_and(|r| r.current.as_ref().is_some_and(|b| b.layer == Layer::User)) => {
            state.confirm = Some(KeybindingConfirm::Reset);
        }
        "r" => state.notice = reset_selected(row, &user_dir, cx),
        _ => {}
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// The action bar under the list: `d` where the row has a binding to
/// silence, `r` where it has a user override to remove — `danger`
/// outline buttons showing their key chip, `keybindings-action-{key}`.
/// Replaced by the confirm row while a question stands.
fn action_block(
    shell: &ShellView,
    state: &KeybindingsState,
    row: Option<&KeybindingRow>,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    if let Some(confirm) = state.confirm {
        let (title, key) = row
            .and_then(|r| r.current.as_ref().map(|b| (r.title.to_string(), palette::render_binding(&b.keystrokes))))
            .unwrap_or_default();
        let on_yes: dialog::ConfirmHandler = Rc::new(|shell, window, cx| {
            let ks = Keystroke { mods: Modifiers::NONE, key: "y".to_string() };
            handle_key(shell, &ks, window, cx);
        });
        let on_no: dialog::ConfirmHandler = Rc::new(|shell, _window, _cx| {
            if let Some(state) = shell.keybindings.as_mut() {
                state.confirm = None;
            }
        });
        return dialog::confirm_row(
            confirm.prompt(&title, &key),
            confirm.yes_label(),
            "keybindings",
            entity,
            on_yes,
            on_no,
            cx,
        );
    }
    let theme = cx.theme();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let mut verbs: Vec<(&'static str, &'static str)> = Vec::new();
    if state.listening.is_none() {
        if row.is_some_and(|r| r.current.is_some()) {
            verbs.push(("d", "Unbind"));
        }
        if row.is_some_and(|r| r.current.as_ref().is_some_and(|b| b.layer == Layer::User)) {
            verbs.push(("r", "Reset to lower layer"));
        }
    }
    let mut bar = h_flex()
        .w(px(WIDTH))
        .gap_2()
        .items_center()
        .debug_selector(|| "keybindings-actions".to_string());
    for (key, label) in verbs {
        let ks = crate::keymap::parse_keystroke(key, Modifiers::NONE).expect("valid");
        let entity_for_action = entity.clone();
        let selector = format!("keybindings-action-{key}");
        let button = Button::new(SharedString::from(format!("keybindings-{key}")))
            .small()
            .outline()
            .danger()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(key_chip(&ks, chip_fg, chip_bg))
                    .child(label),
            )
            .on_click(move |_event, window, cx| {
                entity_for_action.update(cx, |shell, cx| press_verb(shell, key, window, cx));
            });
        bar = bar.child(div().debug_selector(move || selector.clone()).child(button));
    }
    let _ = shell;
    bar.into_any_element()
}
```

The yes handler re-enters `handle_key` with a synthetic `y` so the mouse and the key share one writer — `handle_key` takes `_window: &mut Window`, so pass `window` through. In `build`, compute `let row = visible.get(state.selected).and_then(|m| rows.get(m.row));` where `visible`/`rows` are already in scope, and assemble:

```rust
        .child(dialog::filter_row(&shell.dialog_input, frozen_query, cx))
        .child(list)
        .child(action_block(shell, state, row, entity, cx))
        .child(footer)
```

Imports to add at the top of keybindings_view.rs if missing: `use std::rc::Rc;`, `use gpui::SharedString;`, `use gpui_component::button::{Button, ButtonVariants as _};`, `use gpui_component::Sizable as _;`, `use super::dialog;`. `key_chip` is this file's own.

- [ ] **Step 5: Run the keybindings tests**

Run: `cargo test -p geode-shell keybindings`
Expected: all PASS.

- [ ] **Step 6: Harness entries**

```sh
run_mutation "keybindings: d arms a confirm instead of writing (spec §20.1)" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '                    state.confirm = Some(KeybindingConfirm::Unbind);
                } else {
                    state.notice = unbind_selected(row, &user_dir, cx);' \
  '                    state.notice = unbind_selected(row, &user_dir, cx);
                } else {
                    state.notice = unbind_selected(row, &user_dir, cx);' \
  geode-shell \
  d_asks_before_writing_and_n_withdraws

run_mutation "keybindings: a row click is dropped while a confirm is armed (spec §20.1)" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '    if state.confirm.is_some() {
        return;
    }
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {' \
  '    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {' \
  geode-shell \
  a_row_click_while_a_confirm_is_armed_is_dropped
```

- [ ] **Step 7: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "keybindings: d/r arm the shared confirm, y writes, n withdraws; unbind/reset buttons (spec §20.1)"
```

---

### Task 7: `dialog::value_chip` — the settings dialog steps on a chip click, not a second row click

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs` (new `value_chip` after `confirm_row`)
- Modify: `crates/geode-shell/src/shell/settings_view.rs:372-379` (delete `click_selects_or_steps`), `768-806` (`on_row_clicked`), `878-897` (`value_el`), pure tests 1244-1260
- Test: `crates/geode-shell/src/shell/tests/chrome_and_dialogs.rs`
- Modify: `scripts/mutation-check.sh` (retire any entry anchored on `click_selects_or_steps`)

**Interfaces:**
- Produces:
  ```rust
  pub type StepHandler = Rc<dyn Fn(bool /* forward */, &mut Window, &mut App)>;
  pub(crate) fn value_chip(text: String, selector: String, fg: Hsla, bg: Hsla, on_step: Option<StepHandler>) -> AnyElement
  ```
  Settings selector: `settings-value-{row_ix}`.

- [ ] **Step 1: Write the failing window test**

In `tests/chrome_and_dialogs.rs`, after `a_settings_row_click_keeps_focus_where_the_mode_says`:

```rust
/// Spec §20.3: the value chip is the mouse form of `space`/`shift+space`
/// — click steps forward, shift+click steps back — and a click on the
/// row's label only selects. The old second-click-steps rule is gone.
#[gpui::test]
fn the_settings_value_chip_steps_and_a_row_click_only_selects(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let font = |cx: &gpui::VisualTestContext| shell.read_with(cx, |s, _| s.font_size);
    assert_eq!(font(&cx), crate::fontsize::FontSize::Medium);

    // Row 1 is Font size. Its chip:
    let chip = cx.debug_bounds("settings-value-1").expect("the value chip paints");
    cx.simulate_click(chip.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(font(&cx), crate::fontsize::FontSize::Large, "click steps forward");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        1,
        "and selects the row"
    );
    cx.simulate_click(
        chip.center(),
        gpui::Modifiers { shift: true, ..Default::default() },
    );
    cx.run_until_parked();
    assert_eq!(font(&cx), crate::fontsize::FontSize::Medium, "shift+click steps back");

    // A click on the row's label, twice: select only, never a step.
    let row = cx.debug_bounds("settings-row-1").expect("row paints");
    let label = gpui::point(row.origin.x + gpui::px(20.0), row.center().y);
    cx.simulate_click(label, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.simulate_click(label, gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(font(&cx), crate::fontsize::FontSize::Medium, "a second row click no longer steps");
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "the chip click ended in the sync: normal mode keeps the field blurred"
    );
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p geode-shell the_settings_value_chip_steps`
Expected: FAIL — `settings-value-1` does not paint.

- [ ] **Step 3: Implement `value_chip` in `dialog.rs`**

```rust
/// What a value chip's click runs: `forward` is `!shift`.
pub type StepHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// A steppable row's value, painted as a chip that is the mouse form of
/// `space`/`shift+space` (spec §20.3): click steps forward, shift+click
/// steps back. `on_step: None` paints the plain value with no fill and
/// no handler — the four cases where the keys are inert too (a read-only
/// domain, a one-option `Choice`, an armed confirm, an open text field).
/// `stop_propagation` so the row's own select does not also run; the
/// handler itself ends in [`sync_dialog_text`] at the caller, since it
/// mutates the dialog off the key path (§17.1 rule 3).
pub(crate) fn value_chip(
    text: String,
    selector: String,
    fg: Hsla,
    bg: Hsla,
    on_step: Option<StepHandler>,
) -> AnyElement {
    let base = div()
        .font_family(crate::fonts::MONO)
        .text_sm()
        .flex_shrink_0()
        .debug_selector(move || selector.clone());
    match on_step {
        None => base.text_color(fg).child(text).into_any_element(),
        Some(on_step) => base
            .px_1p5()
            .py_0p5()
            .rounded(px(4.))
            .bg(bg)
            .text_color(fg)
            .cursor_pointer()
            .child(text)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                cx.stop_propagation();
                on_step(!event.modifiers.shift, window, cx);
            })
            .into_any_element(),
    }
}
```

(`px` is already imported in dialog.rs; `MouseDownEvent` carries `modifiers`.)

- [ ] **Step 4: Rewire the settings dialog**

Delete `click_selects_or_steps` (372-379) and its two pure tests (1244-1260). Replace `on_row_clicked` (783-806):

```rust
/// A real mouse click on the row for `clicked`: select it, nothing more
/// (spec §20.3 — the second-click step is gone; the value chip is the
/// mouse form of `space`). Ends in [`dialog::sync_dialog_text`], the
/// row-click seam (spec §16.1/§17.1 rule 3).
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: SettingId,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = rows_for(shell);
    let Some(state) = shell.settings.as_mut() else {
        return;
    };
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    state.selected = ix;
    shell.settings_scroll.scroll_to_item(ix);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// The value chip's click (spec §20.3): select the row and step it
/// through the ONE step path a key takes, [`apply_setting`] via
/// [`step`]. `forward` is `!shift`.
fn on_value_chip_clicked(
    shell: &mut ShellView,
    clicked: SettingId,
    forward: bool,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = rows_for(shell);
    let Some(state) = shell.settings.as_mut() else {
        return;
    };
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    state.selected = ix;
    shell.settings_scroll.scroll_to_item(ix);
    if let Some(row) = visible.get(ix).and_then(|m| rows.get(m.row)) {
        let dir = if forward { StepDirection::Right } else { StepDirection::Left };
        let new_ix = step(row.values.len(), row.current, dir);
        apply_setting(shell, row.id, new_ix, cx);
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

Replace `value_el` (878-883) in `build`:

```rust
        let entity_for_chip = entity.clone();
        let chip_id = row.id;
        let on_step: dialog::StepHandler = Rc::new(move |forward, window, cx| {
            entity_for_chip.update(cx, |shell, cx| {
                on_value_chip_clicked(shell, chip_id, forward, window, cx);
            });
        });
        let value_el = dialog::value_chip(
            row.values[row.current].clone(),
            format!("settings-value-{row_ix}"),
            theme.muted_foreground,
            theme.muted,
            Some(on_step),
        );
```

Add `use std::rc::Rc;` if missing. Update the module doc's click paragraph and spec §18.2's "row click keeps the dialog's existing two-step rule" sentence gets a one-line note pointing at §20.3 (docs task collects the rest).

- [ ] **Step 5: Run settings tests**

Run: `cargo test -p geode-shell settings chrome_and_dialogs`
Expected: all PASS. `a_settings_row_click_keeps_focus_where_the_mode_says` still passes (it only ever clicked to select).

- [ ] **Step 6: Harness**

Search `scripts/mutation-check.sh` for `click_selects_or_steps`; retire any entry anchored on it with a comment in the file's own retirement style (see the retired entry near line 2216 for the wording). Add:

```sh
run_mutation "settings: shift+click on the value chip steps back (spec §20.3)" \
  crates/geode-shell/src/shell/dialog.rs \
  '                on_step(!event.modifiers.shift, window, cx);' \
  '                on_step(true, window, cx);' \
  geode-shell \
  the_settings_value_chip_steps_and_a_row_click_only_selects
```

- [ ] **Step 7: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "settings: the value chip is the mouse form of space/shift+space; a row click only selects (spec §20.3)"
```

---

### Task 8: The object dialog's value chip, `i`/`n` buttons, and the armed-confirm click guard

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs:1458-1485` (`selected_vocabulary` → `vocabulary_of`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` — field row value (3070-3090), `actions` (2467-2516), `press_verb` (3944-3970), `on_edit_row_clicked` (4001-4031), browse build assembly (~2876), browse `n` arm (extract `begin_new_object`)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `dialog::{value_chip, StepHandler}` (Task 7).
- Produces: `Draft::vocabulary_of(&self, row: Option<EditRow>, domain: Domain) -> RowVocabulary`; `fn begin_new_object(shell, cx)`; selectors `objectdialog-value-{field key}`, `objectdialog-action-i`, `objectdialog-action-n`.

- [ ] **Step 1: Write the failing window tests**

In `tests/objectdialog.rs` (near the Colours tests, which have a `Number` field):

```rust
/// Spec §20.3 on the object dialog: the hue chip steps on click and
/// shift+click through `step_selected_row`'s own path (so it writes), a
/// click on the row's label only selects, and the chip is plain text —
/// no handler — while a confirm is armed.
#[gpui::test]
fn the_value_chip_steps_a_number_and_is_inert_under_a_confirm(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
    cx.simulate_keystrokes("enter"); // delta
    cx.run_until_parked();
    let hue = |cx: &gpui::VisualTestContext| {
        edit_draft(&shell, cx, |d| match d.fields[0].kind {
            objectdialog::FieldKind::Number { value, .. } => value,
            _ => panic!("hue is a Number"),
        })
    };
    assert_eq!(hue(&cx), 240);

    let chip = cx.debug_bounds("objectdialog-value-hue").expect("the hue chip paints");
    cx.simulate_click(chip.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(hue(&cx), 255, "click steps forward by the field's step");
    cx.simulate_click(chip.center(), gpui::Modifiers { shift: true, ..Default::default() });
    cx.run_until_parked();
    assert_eq!(hue(&cx), 240, "shift+click steps back");
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("colours.toml")).unwrap();
    assert!(written.contains("[delta]\nhue = 240"), "the chip went through the write path: {written}");

    // Label click: select only.
    let row = cx.debug_bounds("objectdialog-field-hue").expect("row paints");
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(20.0), row.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(hue(&cx), 240, "a row click does not step");

    // Armed: the chip has no handler.
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
    let chip = cx.debug_bounds("objectdialog-value-hue").expect("still painted, as text");
    cx.simulate_click(chip.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(hue(&cx), 240, "inert while the question stands");
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "and the question is still there — the click was dropped, not answered"
    );
}

/// The two keyboard-only verbs gain buttons: `i` on the edit stage's bar
/// when the selected row is one `i` opens, `n` on the browse stage.
#[gpui::test]
fn i_and_n_have_buttons_that_do_what_their_keys_do(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
    let n = cx.debug_bounds("objectdialog-action-n").expect("browse offers n as a button");
    cx.simulate_click(n.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        dialog_state(&shell, &cx, |s| matches!(s.stage, objectdialog::Stage::Naming)),
        "the n button opens the naming row"
    );
    assert!(dialog_filter_is_focused(&shell, &mut cx), "and the field took focus through the sync");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    cx.simulate_keystrokes("enter"); // delta — cursor on `hue`, a Number, which `i` opens
    cx.run_until_parked();
    let i = cx.debug_bounds("objectdialog-action-i").expect("the edit bar offers i on a Number row");
    cx.simulate_click(i.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "the i button opens the value field"
    );
    assert!(dialog_filter_is_focused(&shell, &mut cx));
}

/// §20.6's fallout: an edit-row click while a confirm is armed is claimed
/// and dropped, like the tick click — it neither moves the cursor nor
/// opens a column stage that would silently disarm the question.
#[gpui::test]
fn an_edit_row_click_is_dropped_while_a_confirm_is_armed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let before = edit_draft(&shell, &cx, |d| d.selected);
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
    let row = cx.debug_bounds("objectdialog-field-dataset").expect("a row paints");
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(20.0), row.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.selected), before, "the cursor did not move");
    assert!(cx.debug_bounds("objectdialog-confirm").is_some(), "the question still stands");
}
```

(`edit_draft`, `dialog_state`, `open_tree_edit_stage`, `flush_config_write` are existing helpers in this file. `open_tree_edit_stage` uses `services_with_a_desk_view`, whose `tree` view is desk-owned; `d` there refuses with a notice rather than arming — read the fixture; if so, use `services_with_a_user_only_view` (line 1679) and open its edit stage instead so `d` arms.)

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-shell the_value_chip_steps_a_number i_and_n_have_buttons an_edit_row_click_is_dropped`
Expected: all FAIL (chip/buttons absent; the row click moves the cursor).

- [ ] **Step 3: `Draft::vocabulary_of`**

In `objectdialog/mod.rs`, replace `selected_vocabulary` with:

```rust
    pub fn selected_vocabulary(&self, domain: Domain) -> RowVocabulary {
        self.vocabulary_of(self.selected_row(), domain)
    }

    /// [`selected_vocabulary`](Self::selected_vocabulary) for any row —
    /// what the value chip asks per painted row (spec §20.3), so the chip
    /// and the footer can never disagree about whether a row steps.
    pub fn vocabulary_of(&self, row: Option<EditRow>, domain: Domain) -> RowVocabulary {
        match row {
            None => RowVocabulary::Inert,
            Some(EditRow::Item { .. }) => RowVocabulary::Item,
            Some(EditRow::Available { .. }) => RowVocabulary::Available,
            Some(EditRow::Field(i)) => match &self.fields[i].kind {
                FieldKind::Choice { options, .. } if options.len() < 2 => RowVocabulary::Inert,
                FieldKind::Choice { .. } | FieldKind::Bool(_) => RowVocabulary::Steps,
                FieldKind::Number { .. } => RowVocabulary::StepsAndTypes,
                FieldKind::Text(_) if domain.text_editable(&self.fields[i].key) => {
                    RowVocabulary::Types
                }
                FieldKind::Text(_)
                | FieldKind::MultiChoice { .. }
                | FieldKind::OrderedList { .. } => RowVocabulary::Inert,
            },
        }
    }
```

(Keep the existing doc comments on `selected_vocabulary`.)

- [ ] **Step 4: The chip on field rows**

In `render.rs`'s `build_edit` row loop, the `EditRow::Field(index)` arm: replace the inner `div().text_sm().text_color(theme.muted_foreground).child(field_value(field))` with a chip. Above the loop, compute once:

```rust
    // Spec §20.3: whether any chip may carry a handler this frame. The
    // four inert cases mirror the keys' own: read-only domain, armed
    // confirm, open text field (and per row, a one-option `Choice`).
    let chips_live = writable && draft.confirm.is_none() && draft.text_entry.is_none();
```

(`writable` is already computed in `build_edit` for the badges — confirm the name; if it is `dest_badges`, add `let writable = state.domain.writable(&state.stage);` beside it.) In the arm:

```rust
                let steps = matches!(
                    draft.vocabulary_of(Some(edit_row), state.domain),
                    RowVocabulary::Steps | RowVocabulary::StepsAndTypes
                );
                let on_step: Option<dialog::StepHandler> = (chips_live && steps).then(|| {
                    let entity = entity.clone();
                    Rc::new(move |forward: bool, window: &mut Window, cx: &mut App| {
                        entity.update(cx, |shell, cx| {
                            on_value_chip_clicked(shell, position, forward, window, cx);
                        });
                    }) as dialog::StepHandler
                });
                let value_chip = dialog::value_chip(
                    field_value(field),
                    format!("objectdialog-value-{}", field.key),
                    theme.muted_foreground,
                    theme.muted,
                    on_step,
                );
```

and use `.child(value_chip)` where the old `div` was. Import `RowVocabulary` from `super` if not already. Add the handler beside `on_edit_row_clicked`:

```rust
/// The value chip's click (spec §20.3): move the cursor to the row, then
/// exactly the key's path — [`step_selected_row`] with `filtering` from
/// the mode, so the "nothing changes with …" notice names the right key.
/// Claimed and dropped while a confirm is armed or a text field is open
/// (the chip paints without a handler then, but a test can still call
/// this door). The read-only gate is `step_selected_row`'s callers' —
/// applied here too, since this is one.
fn on_value_chip_clicked(
    shell: &mut ShellView,
    position: usize,
    forward: bool,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    if draft.confirm.is_some() || draft.text_entry.is_some() {
        return;
    }
    if position >= draft.visible_rows().len() {
        return;
    }
    let filtering = state.mode == DialogMode::Filter;
    let writable = state.domain.writable(&state.stage);
    if let Some(draft) = draft_mut(shell) {
        draft.selected = position;
    }
    shell.object_dialog_scroll.scroll_to_item(position);
    if writable {
        step_selected_row(shell, forward, filtering, cx);
    } else {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

- [ ] **Step 5: The armed-confirm guard on the edit-row click**

In `on_edit_row_clicked`, after the notice clear:

```rust
    // Spec §20.6 fallout: claimed and dropped while a question stands —
    // `on_tick_clicked`'s own guard. Without it a door row's click would
    // open a column stage whose `Draft::enter_column` silently clears
    // the confirm, answering the question with a shrug.
    if shell
        .object_dialog
        .as_ref()
        .and_then(|s| s.draft.as_ref())
        .is_some_and(|d| d.confirm.is_some())
    {
        return;
    }
```

- [ ] **Step 6: The `i` and `n` buttons**

In `actions` (2467-2516), after the `r` push and before the Scopes `o` push:

```rust
    // Spec §20.3: `i` is a button wherever the selected row is one it
    // opens — the footer's own test (`RowVocabulary`), plus Groupings'
    // whole-chain `i` (§18.8) which is live on every row there.
    let types = matches!(
        draft.selected_vocabulary(state.domain),
        RowVocabulary::Types | RowVocabulary::StepsAndTypes
    ) || state.domain == Domain::Groupings;
    if types && draft.text_entry.is_none() {
        out.push(Action {
            key: "i",
            label: "Edit value".to_string(),
            destructive: false,
        });
    }
```

In `press_verb`'s `match key`, add `"i" => open_text_field(shell),` and, since `open_text_field` moves focus into the field, end `press_verb` with `dialog::sync_dialog_text(shell, window, cx);` (rename `_window` → `window`) before `cx.notify()`. Update its doc comment: it is no longer §16.6's exception — it syncs.

Extract the browse `n` arm's body into a function and call it from both the key and a button:

```rust
/// `n` (§18.2, spec §20.3): the naming stage, unless the domain refuses
/// it — read-only, or a fixed roster. Called from the key and the browse
/// stage's `n` button alike. `seed` and `seed_taken` are §19.3's Sources
/// seeding, computed by the caller before `object_dialog` is borrowed.
fn begin_new_object(shell: &mut ShellView, seed: Option<String>, seed_taken: bool) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    if !state.domain.writable(&state.stage) {
        state.notice = Some(READ_ONLY_NOTICE.to_string());
    } else if state.domain.roster().is_some() {
        state.notice = Some("the slots are fixed — open one to fill it".to_string());
    } else {
        state.begin_naming();
        state.naming_dataset = seed.clone();
        if let Some(dataset) = seed
            && !seed_taken
        {
            state.query = dataset;
        }
    }
}
```

The `Verb('n')` arm becomes `NormalCommand::Verb('n') => begin_new_object(shell, seed.clone(), seed_taken),` — note `state` (a `&mut` borrow) is live in that match; restructure by ending the borrow before the call (`drop`-by-scope: compute what the arm needs, then call after the match, or take the `&mut state` borrow per arm as the `Commit` arm already does with `open_selected(shell, cx)`). The browse stage gains a bar between `list` and `footer` (~2876):

```rust
        .child(browse_action_bar(shell, state, entity, cx))
```

```rust
/// The browse stage's one button (spec §20.3): `n`, where the domain can
/// create. Same button shape as the edit stage's bar.
fn browse_action_bar(
    shell: &ShellView,
    state: &ObjectDialogState,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    let offers_n = !matches!(state.stage, Stage::Naming)
        && state.domain.writable(&state.stage)
        && state.domain.roster().is_none();
    if !offers_n {
        return div().into_any_element();
    }
    let theme = cx.theme();
    let ks = crate::keymap::parse_keystroke("n", Modifiers::NONE).expect("valid");
    let entity = entity.clone();
    let seed = seed_dataset_under_cursor(shell);
    let seed_taken = seed
        .as_deref()
        .is_some_and(|d| Domain::Sources.name_taken(&shell.services.config, d));
    let label = format!("New {}", object_word(state.domain));
    h_flex()
        .w(px(WIDTH))
        .gap_2()
        .items_center()
        .child(
            div().debug_selector(|| "objectdialog-action-n".to_string()).child(
                Button::new("objectdialog-n")
                    .small()
                    .outline()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(key_chip(&ks, theme.muted_foreground, theme.muted))
                            .child(label),
                    )
                    .on_click(move |_event, window, cx| {
                        let seed = seed.clone();
                        entity.update(cx, |shell, cx| {
                            if let Some(state) = shell.object_dialog.as_mut()
                                && state.notice.take().is_some()
                            {
                                cx.notify();
                            }
                            begin_new_object(shell, seed, seed_taken);
                            dialog::sync_dialog_text(shell, window, cx);
                            cx.notify();
                        });
                    }),
            ),
        )
        .into_any_element()
}
```

- [ ] **Step 7: Run the object dialog tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: all PASS. The test `press_verb`-related "audited exception" harness entry (search the script for `press_verb`) may need its expectation updated: `press_verb` now syncs.

- [ ] **Step 8: Harness entries**

```sh
run_mutation "objectdialog: the value chip is inert under an armed confirm (spec §20.3)" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    let chips_live = writable && draft.confirm.is_none() && draft.text_entry.is_none();' \
  '    let chips_live = writable && draft.text_entry.is_none();' \
  geode-shell \
  the_value_chip_steps_a_number_and_is_inert_under_a_confirm

run_mutation "objectdialog: an edit-row click is dropped while a confirm is armed (spec §20.6)" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        .is_some_and(|d| d.confirm.is_some())
    {
        return;
    }' \
  '        .is_some_and(|d| d.confirm.is_some())
    {
        let _ = 0;
    }' \
  geode-shell \
  an_edit_row_click_is_dropped_while_a_confirm_is_armed
```

If the second anchor matches `on_tick_clicked`'s guard too, anchor on the comment line `// Spec §20.6 fallout: claimed and dropped while a question stands —` plus the two following lines.

- [ ] **Step 9: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "objectdialog: value chip steps on click/shift+click, i and n buttons, edit-row click dropped under a confirm (spec §20.3, §20.6)"
```

---

### Task 9: Clicking away from a `/` line commits it

**Files:**
- Modify: `crates/geode-shell/src/shell/commandline_ctl.rs:93-103` (new `leave_command_line` beside `cancel_command_line`)
- Modify: `crates/geode-shell/src/shell/render.rs:229-238` (backstop), `:621`, `:672` (tile listeners)
- Test: `crates/geode-shell/src/shell/tests/commandline.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `pub(super) fn ShellView::leave_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>)`.

- [ ] **Step 1: Write the failing window test**

In `tests/commandline.rs`, after `a_mouse_down_on_another_tile_cancels_an_open_command_line` (copy its tile-locating block verbatim — the `let (target_id, click_point) = …` expression):

```rust
/// Spec §20.4: a tile mouse-down while a `/` line holds text COMMITS
/// the find (the cursor stays on the match, `n`/`N` have a target), an
/// empty `/` line cancels, and a `:` line still cancels.
#[gpui::test]
fn a_mouse_down_on_a_tile_commits_an_open_find_line(cx: &mut gpui::TestAppContext) {
    use crate::module::{FindEvent, recording::Recorded};
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(&window, &mut cx);
    let opened_on = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.simulate_keystrokes("/");
    cx.simulate_input("sp");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let (_target_id, click_point) = /* the tile-locating block from
        a_mouse_down_on_another_tile_cancels_an_open_command_line, verbatim */;

    cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("command-line").is_none(), "the line closed");
    assert!(
        log.borrow()
            .contains(&Recorded::Find(opened_on, FindEvent::Committed("sp".into()))),
        "the click committed the find: {:?}",
        log.borrow()
    );
    assert!(
        !log.borrow().contains(&Recorded::Find(opened_on, FindEvent::Cancelled)),
        "and did not cancel it"
    );

    // An empty `/` line is a cancel (vimfind's own rule).
    cx.simulate_keystrokes("/");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let now_on = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log.borrow().contains(&Recorded::Find(now_on, FindEvent::Cancelled)),
        "an empty find line cancels on click-away: {:?}",
        log.borrow()
    );
}
```

The existing `a_mouse_down_on_another_tile_cancels_an_open_command_line` already asserts a `:` line's click-away records nothing — it stays as the `:` half of this rule.

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p geode-shell a_mouse_down_on_a_tile_commits_an_open_find_line`
Expected: FAIL at "the click committed the find" (the log holds `Cancelled`).

- [ ] **Step 3: Implement**

`commandline_ctl.rs`, after `cancel_command_line`:

```rust
    /// The mouse's way out of the command line (spec §20.4): a click
    /// away from a `/` line whose text has already moved the cursor
    /// COMMITS it — `FindEvent::Committed` with the field's text, so the
    /// cursor stays on the match and `n`/`N` have a target — exactly as
    /// the scope bar keeps its text on blur. An empty `/` line, and any
    /// `:` line (nothing typed there has applied, and a stray click must
    /// not run a command), cancel through [`cancel_command_line`].
    /// `escape` is still `cancel_command_line` for both prompts.
    pub(super) fn leave_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.command_line.as_ref() else {
            return;
        };
        let text = self.command_input.read(cx).value().to_string();
        if line.prompt == Prompt::Find && !text.is_empty() {
            if let Some(o) = self.occupants.get(&line.tile) {
                o.content.find(FindEvent::Committed(text), window, cx);
            }
            self.close_command_line(window, cx);
        } else {
            self.cancel_command_line(window, cx);
        }
    }
```

`render.rs`: the backstop at ~237 (`self.cancel_command_line(window, cx);` inside the `if self.command_line.as_ref().is_some_and(…)` block) → `self.leave_command_line(window, cx);`; the tree-tile listener at 621 and the dock-tile listener at 672 → `view.leave_command_line(window, cx);`. Update the three comments' "cancels" wording to "leaves (commits a find, cancels a command — `leave_command_line`)". `dialog::open_shell_dialog` (dialog.rs:448) and `toggle_palette` (palette_ctl.rs:41) keep `cancel_command_line` — a chord that opens an overlay is not a click away. Update `cancel_command_line`'s doc comment to name its sibling.

- [ ] **Step 4: Run the command-line tests**

Run: `cargo test -p geode-shell commandline`
Expected: all PASS. If `clicking_the_filter_input_cancels_an_open_command_line` (500) opened a `/` line with text, it now expects `Committed` — update its assertion and rename it `…_leaves_an_open_command_line`; if it opened a `:` line it is unchanged.

- [ ] **Step 5: Harness entry**

```sh
run_mutation "commandline: a click away commits a non-empty find line (spec §20.4)" \
  crates/geode-shell/src/shell/commandline_ctl.rs \
  '        if line.prompt == Prompt::Find && !text.is_empty() {' \
  '        if false {' \
  geode-shell \
  a_mouse_down_on_a_tile_commits_an_open_find_line
```

The retired-entry comment at script line ~2216 ("a tile mouse-down cancels an open line … retired") stays; append a line noting the rule is now `leave_command_line`'s and covered by the entry above.

- [ ] **Step 6: fmt, clippy, anchors, commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings && zsh scripts/mutation-check.sh --anchors-only
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "commandline: a click away commits a find line and cancels a command line (spec §20.4)"
```

---

### Task 10: Docs, the full gates, and the harness run

**Files:**
- Modify: `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` (§20 "As built" subsection; §16.6 note on `press_verb`; §18.2's row-click sentence)
- Modify: `CLAUDE.md` (one paragraph; harness count)
- Modify: `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md` §4.3 (one sentence: bare `j`/`k` wrap, counted clamp, visual clamps)

- [ ] **Step 1: Spec "As built"**

Append `### 20.8 As built (2026-09-14)` to §20, recording per subsection: the shared items' names (`dialog::ConfirmAnswer`, `dialog::confirm_row`, `dialog::value_chip`, `vimnav::apply`/`apply_clamped`, `ShellView::leave_command_line`, `picker::back_to_columns`, `KeybindingConfirm`, `Draft::vocabulary_of`, `begin_new_object`); the one deviation from §20.1's wording — `Confirm` itself stays in `objectdialog` (its `Overwrite` variant and `prompt` are that dialog's), and what moved is the row and the router; that `PaletteState::move_selection` survives as a delegate to `apply` rather than being deleted (fourteen pure tests name it); that `press_verb` is no longer §16.6's audited exception (it syncs, because `i` opens a focused field); and the display-check items (the chip's look on both dialogs, the keybindings action bar and confirm row, the picker's tick target, the palette `tab` reclaim). Amend §16.6 and §18.2 in place with a one-line pointer each to §20.

- [ ] **Step 2: CLAUDE.md**

Add a paragraph after "**Footer rows by category (user ruling 2026-09-14…)**":

> **One answer per verb (2026-09-14, interaction-model spec §20):** `escape` discards what was being *typed* and never what was *stepped or ticked* — stated as a rule now. `vimnav::apply` is the one motion rule (a bare ±1 wraps, anything larger or counted clamps; `apply_clamped` for column axes and a blotter in visual mode), and the palette, picker, as-of selector and market-data tile all route through it with the full `listfilter::nav_command` set. `dialog::ConfirmAnswer` + `dialog::confirm_row` are the one destructive question — the keybindings dialog's `d`/`r` arm it too (`KeybindingConfirm`) and have buttons. `dialog::value_chip` is the mouse form of `space`/`shift+space` on every steppable row (settings' second-click step is gone; the object dialog's `i`/`n` gained buttons). The picker's Values `escape` steps back to Columns (`back_to_columns`) and its row click selects while its tick toggles. A tile mouse-down while a `/` line holds text COMMITS it (`ShellView::leave_command_line`); `:` lines and `escape` still cancel. The diagnostics tile is deliberately untouched pending its rework.

Update the harness count on the `zsh scripts/mutation-check.sh` line to the real number: `grep -c '^run_mutation "' scripts/mutation-check.sh`.

- [ ] **Step 3: Blotter spec §4.3**

One sentence under the motion keys: "A bare `j`/`k` wraps at the ends; a counted step and the page keys clamp; visual mode clamps a bare step too (interaction-model spec §20.5)."

- [ ] **Step 4: Full gates**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p geode-shell --features test-support --all-targets
cargo test --workspace
cargo bench --workspace --no-run
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed
```

Expected: every command exits 0; `--changed` prints no `SURVIVED`. Run `--changed` detached (`run_in_background`) — it compiles per entry and takes minutes.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md docs
git commit -m "docs: spec §20 as built, CLAUDE.md verb-consistency paragraph, blotter §4.3 wrap rule"
```

---

## Self-review

**Spec coverage.** §20.1 → Tasks 5, 6. §20.2 → Task 4. §20.3 → Tasks 7, 8 (chip on both dialogs, `i`/`n` buttons, picker tick target in Task 4). §20.4 → Task 9. §20.5 → Tasks 1, 2, 3 (wrap rule, blotter, market-data, palette/picker/as-of, palette `tab`). §20.6 → nothing to build; the edit-row armed guard is in Task 8. §20.7's tests and harness entries are distributed per task; Task 10 runs the whole harness.

**Type consistency.** `vimnav::apply(usize, usize, NavCommand) -> usize` and `apply_clamped` (Task 1) are what Tasks 2 and 3 call. `Cursor::move_rows(len, cmd, count, wrap)` — four args everywhere in Task 1. `dialog::ConfirmAnswer::from_key(&Keystroke) -> Option<ConfirmAnswer>` and `ConfirmHandler = Rc<dyn Fn(&mut ShellView, &mut Window, &mut Context<ShellView>)>` (Task 5) are what Tasks 5 and 6 pass. `dialog::StepHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>` and `value_chip(String, String, Hsla, Hsla, Option<StepHandler>)` (Task 7) are what Task 8 uses. `Draft::vocabulary_of(Option<EditRow>, Domain) -> RowVocabulary` (Task 8) matches its one call. `leave_command_line(&mut self, &mut Window, &mut Context<Self>)` (Task 9) matches the three call sites' existing `cancel_command_line` shape.

**Known judgement calls an implementer may hit.** Task 6's `on_yes` re-enters `handle_key` with a synthetic `y`: if the borrow of `shell.keybindings` inside `handle_key` conflicts with anything, split the armed block's yes arm into a `fn run_confirmed(shell, confirm, cx)` and call it from both. Task 8's `begin_new_object` extraction: the browse `match` holds `state: &mut` — move the `Verb('n')` handling out of the match (set a local `let mut wants_new = false;`, flip it in the arm, call after the match) rather than fighting the borrow. Task 3's palette `tab` test may already pass before the reclaim in the test window; the reclaim is still shipped, the harness entry is dropped if it survives, and §20.5 already calls it a display-check item.
