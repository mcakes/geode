# Dialog Mouse Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every modal dialog does with a mouse what it already does with a keyboard: the frozen filter row is clickable, a row click opens or captures, and the object dialog's edit stage toggles by clicking the tick, reorders/adds/removes by drag and drop, and completes a chain segment by clicking a completion row.

**Architecture:** Pure state stays the truth (interaction-model spec §16): each new mouse handler is a pure mutation of `mode`/`query`/`listening`/`Draft` followed by `dialog::sync_dialog_text`. Drop semantics live in one pure method, `Draft::drop_row`, resolved by name; reordering rides gpui's own `on_drag`/`on_drop` with a tiny ghost entity rather than a second hand-rolled drag machine.

**Tech Stack:** Rust, gpui (pinned git checkout `e3adf43`), gpui-component, `TestAppContext`/`VisualTestContext` window tests with `simulate_mouse_down`/`simulate_mouse_move`/`simulate_mouse_up`, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` §17 and `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` §18.9 (both committed at 99a1284 on this branch).

## Global Constraints

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo check -p geode-shell --features test-support --all-targets` must stay green; CI runs on macOS and Windows.
- No raw colours: every colour is a `cx.theme()` token.
- No mouse handler may call `focus(..)` or `InputState::set_value` itself — the only focus/text writer is `dialog::sync_dialog_text`, called on the handler's return (§16.1, §17.1 rule 3). `press_verb` stays the audited exception.
- `enter_edit_stage` (`objectdialog/render.rs`) stays the one door into the edit stage; a click goes through it.
- Filter-only dialogs (palette, settings, dimension picker, as-of selector) are untouched.
- Commit after every task with the attribution trailer below. Commit before running the mutation harness (it restores files with `git checkout`).
- Every harness entry names its covering test (6th argument). Run `zsh scripts/mutation-check.sh --anchors-only` before the final commit.
- `debug_selector` strings a test reads with `debug_bounds` must be `&'static str` literals in the test.

Commit trailer for every commit:

```
Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0188sPcpargWkA1ENqWAd21i
```

---

## File map

| File | Responsibility in this plan |
|---|---|
| `crates/geode-shell/src/shell/dialog.rs` | `FrozenFilter` gains the shell entity; the frozen row gets a mouse-down; `enter_filter_by_mouse` is the one pure transition for "click the field" on either modal dialog |
| `crates/geode-shell/src/shell/keybindings_view.rs` | `click_selects_or_listens` → `click_listens`; the `FrozenFilter` construction passes `entity` |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | `RowDrag` payload, `Draft::locate`, `Draft::row_drag`, `Draft::drop_row` (pure, tested without a window) |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | browse click opens; tick click; drag source/target wiring and `DragGhost`; `on_row_dropped`; completion click; both `FrozenFilter` constructions pass `entity` |
| `crates/geode-shell/src/shell/tests/keybindings_dialog.rs` | window tests for the frozen-row click and single-click capture |
| `crates/geode-shell/src/shell/tests/objectdialog.rs` | window tests for the frozen-row click, browse click, tick click, drop, completion click |
| `scripts/mutation-check.sh` | one entry per new pure branch |
| `CLAUDE.md`, both specs' "As built" sections, memory | the handoff |

---

### Task 1: A click on the frozen filter row enters filter mode

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`FrozenFilter` at ~575, `filter_row` at ~616, new `enter_filter_by_mouse` beside `sync_dialog_text` ~506)
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs:1565-1570` (the `FrozenFilter { .. }` construction)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs:2057-2060` and `:2509-2512` (the two `FrozenFilter { .. }` constructions)
- Test: `crates/geode-shell/src/shell/tests/keybindings_dialog.rs`, `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `pub struct FrozenFilter<'a> { pub query: &'a str, pub slash_filters: bool, pub entity: Entity<ShellView> }`; `pub(crate) fn enter_filter_by_mouse(shell: &mut ShellView)`; the frozen row's `debug_selector` is `"dialog-filter-frozen"`.

- [ ] **Step 1: Write the failing window tests**

Append to `crates/geode-shell/src/shell/tests/keybindings_dialog.rs` (use the same open-the-dialog boilerplate as `click_selects_a_row_and_clicking_it_again_starts_listening` in that file, or the file's `open_keybindings` helper if one exists):

```rust
// --- Mouse parity (interaction-model spec §17) --------------------------

/// §17.1 rule 1: the frozen filter row is the mouse form of `/`. The
/// dialog opens in normal mode with the row frozen; a mouse-down on it
/// must leave the pill reading `filter` with the shared `Input` focused.
#[gpui::test]
fn clicking_the_frozen_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, test_services(), "keybindings::open");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().mode),
        DialogMode::Normal
    );
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the frozen filter row paints in normal mode");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().mode),
        DialogMode::Filter,
        "a click on the field is the mouse form of /"
    );
    let input_focused = cx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(input_focused, "the sync handed the keyboard to the Input");
    // And typing now filters rather than acting as a verb.
    cx.simulate_input("j");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "j"
    );
}

/// The one frozen state where `/` is NOT the filter's key: a capture in
/// progress. A click on the field there cancels the capture and enters
/// filter mode — a click on a text field is never a keystroke to bind.
#[gpui::test]
fn clicking_the_frozen_filter_row_while_listening_cancels_the_capture(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx) = dialog_test_shell_with(cx, test_services(), "keybindings::open");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().listening.is_some()));
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the row is frozen while listening");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    let (listening, mode) = shell.read_with(&cx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.listening.is_some(), k.mode)
    });
    assert!(!listening, "the capture was cancelled");
    assert_eq!(mode, DialogMode::Filter);
}
```

Append to `crates/geode-shell/src/shell/tests/objectdialog.rs`:

```rust
// --- Mouse parity (interaction-model spec §17, 4c §18.9) ----------------

/// §17.1 rule 1 on the object dialog's browse stage.
#[gpui::test]
fn clicking_the_browse_frozen_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("browse opens in normal mode with the row frozen");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    cx.simulate_input("w");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "w");
}

/// And on the edit stage, whose frozen row is the draft's own (§18.3).
#[gpui::test]
fn clicking_the_edit_frozen_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the edit stage opens in normal mode with the row frozen");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    cx.simulate_input("n");
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "n");
}
```

If `keybindings_dialog.rs` lacks `dialog_test_shell_with`/`test_services` in scope, add `use super::*;` at its top the way `objectdialog.rs` does.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p geode-shell frozen_filter_row -- --nocapture`
Expected: FAIL — `debug_bounds("dialog-filter-frozen")` returns `None` (no such selector yet).

- [ ] **Step 3: Implement**

In `crates/geode-shell/src/shell/dialog.rs`:

1. Add the field and the transition:

```rust
pub struct FrozenFilter<'a> {
    pub query: &'a str,
    pub slash_filters: bool,
    /// The shell, so the frozen row's mouse-down can reach
    /// [`enter_filter_by_mouse`] (§17.1 rule 1). Only the frozen branch
    /// needs it — the live `Input` branches are already focused.
    pub entity: Entity<ShellView>,
}

/// §17.1 rule 1: a mouse-down on the frozen filter row is the mouse form
/// of `/`. A pure mutation — [`sync_dialog_text`] on the handler's return
/// is what focuses the `Input`. On the keybinding dialog a capture in
/// progress is cancelled first: a click on a text field is never a
/// keystroke to bind, and `listening` wins over the mode in
/// `dialogmode::focus_target`, so leaving it set would keep the keys on
/// the shell root under a pill reading `filter`.
pub(crate) fn enter_filter_by_mouse(shell: &mut ShellView) {
    if let Some(state) = shell.keybindings.as_mut() {
        state.listening = None;
        state.mode = DialogMode::Filter;
    } else if let Some(state) = shell.object_dialog.as_mut() {
        state.mode = DialogMode::Filter;
    }
}
```

2. In `filter_row`, both `Some(frozen)` arms build `row.py_1().child(..)`; give the row the selector and the handler. Factor the shared prefix so both arms use it:

```rust
        Some(frozen) => {
            let entity = frozen.entity.clone();
            let row = row
                .py_1()
                .cursor_text()
                .debug_selector(|| "dialog-filter-frozen".to_string())
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    entity.update(cx, |shell, cx| {
                        enter_filter_by_mouse(shell);
                        sync_dialog_text(shell, window, cx);
                        cx.notify();
                    });
                });
            let body = if frozen.query.is_empty() && frozen.slash_filters {
                div()
                    .debug_selector(|| "dialog-filter-placeholder".to_string())
                    .child("press / to filter")
                    .into_any_element()
            } else {
                div().child(frozen.query.to_string()).into_any_element()
            };
            row.child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(theme.muted_foreground)
                    .child(search_icon())
                    .child(body),
            )
            .into_any_element()
        }
```

Keep the existing doc comments on the placeholder condition (move them onto the `if`). `cursor_text` is gpui's `Styled::cursor_text()`; if the pinned rev lacks it use `.cursor(gpui::CursorStyle::IBeam)`.

3. Update the three constructions to pass `entity: entity.clone()` (keybindings_view.rs ~1566; objectdialog/render.rs ~2057 and ~2509 — both functions already have an `entity: &Entity<ShellView>` parameter).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell frozen_filter_row`
Expected: all five PASS. Then `cargo test -p geode-shell` to confirm nothing else moved (the placeholder selector is still painted under the same condition).

- [ ] **Step 5: Harness entry**

Append to `scripts/mutation-check.sh` next to the other `objectdialog:` entries (~line 3640):

```sh
# §17.1 rule 1: the frozen filter row is the mouse form of `/`. Mutated
# to a click that syncs without the transition, the row is inert and the
# dialog stays in normal mode under the click.
run_mutation "dialog: a click on the frozen filter row enters filter mode" \
  crates/geode-shell/src/shell/dialog.rs \
  '                        enter_filter_by_mouse(shell);
                        sync_dialog_text(shell, window, cx);' \
  '                        sync_dialog_text(shell, window, cx);' \
  geode-shell clicking_the_frozen_filter_row_enters_filter_mode

# The listening half of the same rule: a capture must be cancelled by the
# click, or `focus_target` keeps the keys on the shell root under a pill
# reading `filter`.
run_mutation "dialog: the frozen-row click cancels a capture in progress" \
  crates/geode-shell/src/shell/dialog.rs \
  '        state.listening = None;
        state.mode = DialogMode::Filter;' \
  '        state.mode = DialogMode::Filter;' \
  geode-shell clicking_the_frozen_filter_row_while_listening_cancels_the_capture
```

Run: `zsh scripts/mutation-check.sh --anchors-only` (expect exit 0), then `zsh scripts/mutation-check.sh "frozen"` (expect both `CAUGHT`).

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add -A && git commit -m "feat(dialog): a click on the frozen filter row enters filter mode (§17.1)"
```

---

### Task 2: A single click on a keybinding row starts capture

**Files:**
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs:496-503` (`click_selects_or_listens`), `:917-936` (`on_row_clicked`), and its pure tests (grep `click_selects_or_listens`)
- Test: `crates/geode-shell/src/shell/tests/keybindings_dialog.rs:459-548` (rewrite `click_selects_a_row_and_clicking_it_again_starts_listening`)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `pub fn click_listens(state: &mut KeybindingsState, clicked_ix: usize)` replacing `click_selects_or_listens`.

- [ ] **Step 1: Rewrite the window test**

Replace `click_selects_a_row_and_clicking_it_again_starts_listening` (keep its boilerplate up to the first `simulate_mouse_down`) with:

```rust
/// §17.1 rule 2: a row click does what `enter` would. One click on a row
/// that is not the selected one both selects it and starts listening —
/// the second click the old rule required was one step short of
/// everything a mouse user came for.
#[gpui::test]
fn a_single_click_on_a_row_starts_listening(cx: &mut gpui::TestAppContext) {
    // …same boilerplate as before up to `inside_row_1`…
    cx.simulate_mouse_down(inside_row_1, MouseButton::Left, gpui::Modifiers::none());
    let (selected, listening) = shell.read_with(&cx, |shell, _| {
        let k = shell.keybindings.as_ref().unwrap();
        (k.selected, k.listening.clone())
    });
    assert_eq!(selected, 1, "the click selected row 1");
    assert_eq!(listening, Some(Vec::new()), "and started listening on it at once");

    // A click on a different row mid-capture retargets the capture.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row_2 = cx.debug_bounds("keybindings-row-2").expect("row 2 painted");
    cx.simulate_mouse_down(
        gpui::point(row_2.origin.x + gpui::px(10.0), row_2.origin.y + gpui::px(10.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let (selected, listening) = shell.read_with(&cx, |shell, _| {
        let k = shell.keybindings.as_ref().unwrap();
        (k.selected, k.listening.clone())
    });
    assert_eq!(selected, 2);
    assert_eq!(listening, Some(Vec::new()));
}
```

Update the pure test(s) that call `click_selects_or_listens` (grep it in `keybindings_view.rs`'s `mod tests`) to:

```rust
    /// §17.1 rule 2: one click, capture at once — on any row, selected
    /// or not, listening or not (a repeat click restarts the capture
    /// with the partial sequence dropped).
    #[test]
    fn a_click_selects_and_listens_in_one_step() {
        let mut state = fresh_state(); // whatever the existing test's constructor is
        state.selected = 0;
        click_listens(&mut state, 3);
        assert_eq!(state.selected, 3);
        assert_eq!(state.listening, Some(Vec::new()));
        state.listening = Some(vec![key("a")]);
        click_listens(&mut state, 3);
        assert_eq!(state.listening, Some(Vec::new()), "a repeat click restarts the capture");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell a_single_click_on_a_row_starts_listening a_click_selects_and_listens`
Expected: compile error (`click_listens` undefined) / FAIL on the `listening` assertion.

- [ ] **Step 3: Implement**

Replace `click_selects_or_listens` with:

```rust
/// §17.1 rule 2: a click on a row is the mouse form of moving the cursor
/// there and pressing `enter` — it selects and starts listening in one
/// step. A click on a different row mid-capture retargets the capture;
/// a click on the same row restarts it with the partial sequence
/// dropped. (Until 2026-09-12 the first click only selected and a
/// second on the same row listened — one step short for a mouse user.)
pub fn click_listens(state: &mut KeybindingsState, clicked_ix: usize) {
    state.selected = clicked_ix;
    state.listening = Some(Vec::new());
}
```

In `on_row_clicked`, replace `click_selects_or_listens(state, ix);` with `click_listens(state, ix);` and update its doc comment ("A click that starts listening therefore blurs the filter …" — every click now does). Fix any other reference (grep the crate).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell keybindings`
Expected: PASS.

- [ ] **Step 5: Harness entry**

```sh
# §17.1 rule 2: one click captures. Mutated back to the two-click rule,
# the first click on an unselected row only selects.
run_mutation "keybindings: a single row click starts listening" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '    state.selected = clicked_ix;
    state.listening = Some(Vec::new());' \
  '    if state.selected == clicked_ix && state.listening.is_none() {
        state.listening = Some(Vec::new());
    } else {
        state.selected = clicked_ix;
        state.listening = None;
    }' \
  geode-shell a_single_click_on_a_row_starts_listening
```

Run `--anchors-only`, then `zsh scripts/mutation-check.sh "single row click"`.

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add -A && git commit -m "feat(keybindings): a single row click starts capture (§17.1 rule 2)"
```

---

### Task 3: A browse row click opens the edit stage

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs:583-605` (`on_row_clicked`)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `enter_edit_stage(shell, &name, None, cx)` (render.rs ~667), `Stage::Naming`, `ObjectDialogState::stage`.

- [ ] **Step 1: Write the failing window tests**

Append to `tests/objectdialog.rs`:

```rust
/// §17.1 rule 2 on browse: one click opens the row's edit stage through
/// the one door (`enter_edit_stage`), exactly as `enter` does.
#[gpui::test]
fn clicking_a_browse_row_opens_its_edit_stage(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    let row = cx
        .debug_bounds("objectdialog-row-wide")
        .expect("the wide view paints a browse row");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit("wide".to_string()),
        "the click opened wide"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
}

/// On Groupings the door lands in the chain field (§18.8), because the
/// door decides, not the click.
#[gpui::test]
fn clicking_a_groupings_row_lands_in_the_chain_field(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book", "lhu"]),
        dir.path(),
        "config::groupings",
    );
    let row = cx.debug_bounds("objectdialog-row-3").expect("slot 3 paints");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry));
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "book / lhu");
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
}

/// While the naming row is open a click only selects: a typed name must
/// not be discarded by a stray click, and `enter` there creates.
#[gpui::test]
fn clicking_a_browse_row_while_naming_only_selects(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    cx.simulate_input("mine");
    cx.run_until_parked();
    let row = cx.debug_bounds("objectdialog-row-wide").expect("rows still paint while naming");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Naming);
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "mine", "the typed name survived");
}
```

Check `Stage`'s exact `Edit` payload shape at `objectdialog/mod.rs:100-115` and match it. If `Stage` is not `Clone`, compare with `matches!`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell clicking_a_browse_row clicking_a_groupings_row`
Expected: FAIL — stage is still `Browse` after the click.

- [ ] **Step 3: Implement**

In `on_row_clicked` (render.rs ~583), after `state.selected = ix;`:

```rust
    // §17.1 rule 2: a click does what `enter` would — open the row —
    // except while naming, where `enter` creates and a click must not
    // discard the typed name. Same door as `open_selected`, and by
    // name rather than index for the same reason the selector is.
    let opens = state.stage != Stage::Naming;
    let name = clicked.to_string();
    shell.object_dialog_scroll.scroll_to_item(ix);
    if opens {
        enter_edit_stage(shell, &name, None, cx);
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
```

(Restructure the existing tail so `state`'s borrow ends before `enter_edit_stage` takes `shell`.) Update the function's doc comment. Then run the whole object-dialog suite: any existing test that clicks an `objectdialog-row-*` and asserts the stage stayed `Browse` is asserting the old rule — rewrite it to assert the new one (§17 records the ruling) rather than weaken the implementation.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: PASS.

- [ ] **Step 5: Harness entry**

```sh
# §17.1 rule 2 on browse: a click opens. Mutated to select-only, the
# stage stays Browse under the click.
run_mutation "objectdialog: a browse row click opens the edit stage" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if opens {
        enter_edit_stage(shell, &name, None, cx);
    }' \
  '    if opens && false {
        enter_edit_stage(shell, &name, None, cx);
    }' \
  geode-shell clicking_a_browse_row_opens_its_edit_stage

# The naming exception: a stray click must not discard the typed name.
run_mutation "objectdialog: a click while naming only selects" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    let opens = state.stage != Stage::Naming;' \
  '    let opens = true;' \
  geode-shell clicking_a_browse_row_while_naming_only_selects
```

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add -A && git commit -m "feat(objectdialog): a browse row click opens the edit stage (§17.1 rule 2)"
```

---

### Task 4: `Draft::drop_row` — the pure drop core

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (beside `move_item` ~1174 and `remove_selected` ~1235; tests in its `mod tests` ~1864)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct RowDrag { pub field: String, pub own: bool, pub name: String }`
  - `impl Draft { pub fn row_drag(&self, row: EditRow) -> Option<RowDrag>; pub fn locate(&self, drag: &RowDrag) -> Option<EditRow>; pub fn drop_row(&mut self, src: &RowDrag, dst: &RowDrag) -> Step }`
- Consumes: `EditRow`, `FieldKind::OrderedList { items, available }`, `Step`, `Draft::follow` (private, same impl).

- [ ] **Step 1: Write the failing pure tests**

In `objectdialog/mod.rs`'s `mod tests`, next to the `move_item` tests (find `reordering_under_a_filter_moves_past_the_hidden_rows_and_says_how_many` ~3144 and reuse whatever draft constructor it uses — a Views draft over `book`, `npv` with `delta01` available is the fixture `views::tests` already builds; if none is reachable from here, build one with `Draft { fields: vec![Field { key: "columns".into(), label: "Columns".into(), dest: Destination::Presentation, kind: FieldKind::OrderedList { items: vec![item("book"), item("npv")], available: Some(vec![item("delta01")]) } }], .. }` and a small `item(name)` helper):

```rust
    fn names(draft: &Draft) -> Vec<String> {
        draft.list_items("columns").unwrap().iter().map(|i| i.name.clone()).collect()
    }
    fn drag(draft: &Draft, own: bool, name: &str) -> RowDrag {
        RowDrag { field: "columns".to_string(), own, name: name.to_string() }
    }

    /// §18.9.3: "drop on a row" means take that row's index. Downward
    /// lands after the target's old position, upward before it.
    #[test]
    fn a_drop_takes_the_target_rows_index() {
        let mut draft = three_column_draft(); // book, npv, delta01 as items; catalogue empty-but-Some
        assert_eq!(draft.drop_row(&drag(&draft, true, "book"), &drag(&draft, true, "delta01")), Step::Changed);
        assert_eq!(names(&draft), ["npv", "delta01", "book"]);
        assert_eq!(draft.drop_row(&drag(&draft, true, "book"), &drag(&draft, true, "npv")), Step::Changed);
        assert_eq!(names(&draft), ["book", "npv", "delta01"]);
    }

    /// The cursor follows the dropped item, so the next keystroke acts on
    /// the thing the trader just placed.
    #[test]
    fn the_cursor_follows_the_dropped_item() {
        let mut draft = three_column_draft();
        draft.drop_row(&drag(&draft, true, "book"), &drag(&draft, true, "delta01"));
        assert_eq!(draft.row_label(draft.selected_row().unwrap()), "book");
    }

    /// Available → Item adds at the target's index rather than appending.
    #[test]
    fn dropping_an_available_row_onto_the_list_adds_it_at_that_index() {
        let mut draft = two_column_draft_with_one_available(); // items book, npv; available delta01
        assert_eq!(draft.drop_row(&drag(&draft, false, "delta01"), &drag(&draft, true, "book")), Step::Changed);
        assert_eq!(names(&draft), ["delta01", "book", "npv"]);
        assert!(draft.list_items("columns").unwrap()[0].included);
        let FieldKind::OrderedList { available, .. } = &draft.fields[0].kind else { panic!() };
        assert!(available.as_ref().unwrap().is_empty());
    }

    /// Item → Available removes, exactly as `x` does, catalogue index
    /// ignored (it has no order).
    #[test]
    fn dropping_an_item_onto_the_catalogue_removes_it() {
        let mut draft = two_column_draft_with_one_available();
        assert_eq!(draft.drop_row(&drag(&draft, true, "npv"), &drag(&draft, false, "delta01")), Step::Changed);
        assert_eq!(names(&draft), ["book"]);
        let FieldKind::OrderedList { available, .. } = &draft.fields[0].kind else { panic!() };
        let avail: Vec<&str> = available.as_ref().unwrap().iter().map(|i| i.name.as_str()).collect();
        assert_eq!(avail, ["delta01", "npv"]);
        assert!(!available.as_ref().unwrap()[1].included);
    }

    /// Nothing to do: same row, catalogue to catalogue, a name that no
    /// longer resolves (a keystroke removed it mid-drag), or a field that
    /// is not a list. None of these writes.
    #[test]
    fn inert_drops_change_nothing() {
        let mut draft = two_column_draft_with_one_available();
        let before = draft.fields.clone();
        assert_eq!(draft.drop_row(&drag(&draft, true, "book"), &drag(&draft, true, "book")), Step::Inert);
        assert_eq!(draft.drop_row(&drag(&draft, true, "gone"), &drag(&draft, true, "book")), Step::Inert);
        assert_eq!(draft.drop_row(&drag(&draft, true, "book"), &drag(&draft, true, "gone")), Step::Inert);
        assert_eq!(draft.fields, before);
        // Two catalogue rows (add a second available item first).
        let FieldKind::OrderedList { available, .. } = &mut draft.fields[0].kind else { panic!() };
        available.as_mut().unwrap().push(item("gamma"));
        assert_eq!(draft.drop_row(&drag(&draft, false, "delta01"), &drag(&draft, false, "gamma")), Step::Inert);
    }

    /// A list with no catalogue (Groupings) refuses a demotion the same
    /// way `x` does — the wording is `remove_selected`'s.
    #[test]
    fn a_drop_into_a_missing_catalogue_is_refused_like_x() {
        let mut draft = groupings_draft(); // items book, lhu; available None
        // There is no available row to target, so the refusal is reached
        // through a payload claiming one.
        let step = draft.drop_row(&drag(&draft, true, "book"), &RowDrag { field: "dimensions".into(), own: false, name: "lhu".into() });
        assert!(matches!(step, Step::Inert), "a target that does not resolve is inert, never a phantom removal");
    }

    /// `row_drag` and `locate` are inverses over every list row, and a
    /// field row has no payload at all.
    #[test]
    fn row_drag_round_trips_through_locate() {
        let draft = two_column_draft_with_one_available();
        for row in draft.rows() {
            match row {
                EditRow::Field(_) => assert_eq!(draft.row_drag(row), None),
                EditRow::Item { .. } | EditRow::Available { .. } => {
                    let payload = draft.row_drag(row).expect("list rows drag");
                    assert_eq!(draft.locate(&payload), Some(row));
                }
            }
        }
    }
```

Use the real fixture names in that test module; the shapes above are what matters. Adjust field keys (`columns` for Views, `dimensions` for Groupings) and `dest` to the fixture's.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell objectdialog::tests::.*drop`
Expected: compile errors (`RowDrag`, `drop_row`, `row_drag`, `locate` undefined).

- [ ] **Step 3: Implement**

In `objectdialog/mod.rs`, after `EditRow`:

```rust
/// What a dragged list row carries (4c §18.9.1): the field's key, which
/// block it came from, and the item's NAME — never an index. The keyboard
/// stays live during a drag, so a keystroke can reorder or remove between
/// the grab and the drop; a payload resolved by name at drop time lands on
/// the row the trader picked up, or on nothing, never on whichever column
/// now holds the grabbed index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDrag {
    pub field: String,
    /// `true` for an [`EditRow::Item`], `false` for an
    /// [`EditRow::Available`] — §18.7.1's variant distinction carried onto
    /// the wire.
    pub own: bool,
    pub name: String,
}
```

In `impl Draft`, beside `move_item`:

```rust
    /// The payload a list row drags (§18.9.1); `None` for a field row,
    /// which is neither a drag source nor a drop target.
    pub fn row_drag(&self, row: EditRow) -> Option<RowDrag> {
        let (field, own, item) = match row {
            EditRow::Item { field, item } => (field, true, item),
            EditRow::Available { field, item } => (field, false, item),
            EditRow::Field(_) => return None,
        };
        let FieldKind::OrderedList { items, available } = &self.fields[field].kind else {
            return None;
        };
        let list = if own { items.as_slice() } else { available.as_deref()? };
        Some(RowDrag {
            field: self.fields[field].key.clone(),
            own,
            name: list.get(item)?.name.clone(),
        })
    }

    /// Resolve a payload back to a row by name, as the draft stands NOW.
    /// `None` when the field is not a list, the block does not exist
    /// (`own == false` on a catalogue-less list) or the name has left it.
    pub fn locate(&self, drag: &RowDrag) -> Option<EditRow> {
        let field = self.fields.iter().position(|f| f.key == drag.field)?;
        let FieldKind::OrderedList { items, available } = &self.fields[field].kind else {
            return None;
        };
        let list = if drag.own { items.as_slice() } else { available.as_deref()? };
        let item = list.iter().position(|i| i.name == drag.name)?;
        Some(if drag.own {
            EditRow::Item { field, item }
        } else {
            EditRow::Available { field, item }
        })
    }

    /// A drop (§18.9.3): `src` takes `dst`'s index. One method decides
    /// every case, and it is the only place they are enumerated:
    ///
    /// - Item → Item: reorder (`remove(src)`, `insert(dst_index)`), so
    ///   downward lands after the target's old position, upward before.
    /// - Available → Item: add at index — `space`'s add, placed rather
    ///   than appended; `included = true`.
    /// - Item → Available: remove, the same act as `x`; the catalogue is
    ///   unordered so the target index is ignored.
    /// - Available → Available: `Inert` — the catalogue has no order.
    /// - Same row, different fields, or a name that no longer resolves:
    ///   `Inert`, nothing written.
    ///
    /// After a change the cursor follows the dropped item, so the next
    /// keystroke acts on the thing the trader just placed (unlike `space`
    /// and `x`, whose cursor rulings are about a *run* of adds/removes).
    pub fn drop_row(&mut self, src: &RowDrag, dst: &RowDrag) -> Step {
        let (Some(src_row), Some(dst_row)) = (self.locate(src), self.locate(dst)) else {
            return Step::Inert;
        };
        if src_row == dst_row {
            return Step::Inert;
        }
        let (field, src_own, src_ix) = match src_row {
            EditRow::Item { field, item } => (field, true, item),
            EditRow::Available { field, item } => (field, false, item),
            EditRow::Field(_) => return Step::Inert,
        };
        let (dst_field, dst_own, dst_ix) = match dst_row {
            EditRow::Item { field, item } => (field, true, item),
            EditRow::Available { field, item } => (field, false, item),
            EditRow::Field(_) => return Step::Inert,
        };
        if field != dst_field {
            return Step::Inert;
        }
        let FieldKind::OrderedList { items, available } = &mut self.fields[field].kind else {
            return Step::Inert;
        };
        let landed = match (src_own, dst_own) {
            (true, true) => {
                let entry = items.remove(src_ix);
                items.insert(dst_ix, entry);
                EditRow::Item { field, item: dst_ix }
            }
            (false, true) => {
                let Some(available) = available.as_mut() else {
                    return Step::Inert;
                };
                let mut entry = available.remove(src_ix);
                entry.included = true;
                items.insert(dst_ix, entry);
                EditRow::Item { field, item: dst_ix }
            }
            (true, false) => {
                let Some(available) = available.as_mut() else {
                    return Step::Inert;
                };
                let mut entry = items.remove(src_ix);
                entry.included = false;
                available.push(entry);
                EditRow::Available { field, item: available.len() - 1 }
            }
            (false, false) => return Step::Inert,
        };
        self.follow(landed);
        Step::Changed
    }
```

Note: `(true, false)` with `available == None` is unreachable through `locate` (a `dst` claiming a missing catalogue resolves to `None` first), so the `Inert` there is a guard, not a refusal — the `x`-style `Refused("space unticks here")` text is unreachable by drop because no catalogue row exists to drop onto; say so in the doc.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell objectdialog::tests`
Expected: PASS.

- [ ] **Step 5: Harness entries**

```sh
# §18.9.3: a drop takes the target's index. Mutated to append, every
# reorder lands at the end.
run_mutation "objectdialog: a drop takes the target row's index" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                let entry = items.remove(src_ix);
                items.insert(dst_ix, entry);' \
  '                let entry = items.remove(src_ix);
                items.push(entry);' \
  geode-shell a_drop_takes_the_target_rows_index

# Available → Item places rather than appends.
run_mutation "objectdialog: an available row dropped onto the list is placed, not appended" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                entry.included = true;
                items.insert(dst_ix, entry);' \
  '                entry.included = true;
                items.push(entry);' \
  geode-shell dropping_an_available_row_onto_the_list_adds_it_at_that_index

# Item → Available unticks on the way out, as `x` does.
run_mutation "objectdialog: a row dropped into the catalogue is unticked" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                let mut entry = items.remove(src_ix);
                entry.included = false;' \
  '                let mut entry = items.remove(src_ix);' \
  geode-shell dropping_an_item_onto_the_catalogue_removes_it

# Same row is inert: without the guard a self-drop is a remove+insert
# that dirties nothing but still reports Changed and queues a write.
run_mutation "objectdialog: a drop on its own row is inert" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if src_row == dst_row {
            return Step::Inert;
        }' \
  '        if src_row == dst_row && false {
            return Step::Inert;
        }' \
  geode-shell inert_drops_change_nothing

# The cursor follows the dropped item.
run_mutation "objectdialog: the cursor follows a dropped item" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        self.follow(landed);
        Step::Changed' \
  '        let _ = landed;
        Step::Changed' \
  geode-shell the_cursor_follows_the_dropped_item
```

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add -A && git commit -m "feat(objectdialog): Draft::drop_row, the pure drop core by name (§18.9.3)"
```

---

### Task 5: The tick is the toggle

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (the `tick` element in `build_edit` ~2286-2296; new `on_tick_clicked` beside `on_edit_row_clicked` ~2750)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `fn on_tick_clicked(shell, position: usize, window, cx)`; tick `debug_selector` `objectdialog-tick-{name}`.
- Consumes: `draft_mut`, `Draft::toggle_selected`, `maybe_refresh_available`, `revalidate`, `scroll_to_cursor`, `commit_or_confirm`, `refuse_step`, `set_notice` (all in render.rs).

- [ ] **Step 1: Write the failing window tests**

Append to `tests/objectdialog.rs` (the `tree` view from `open_tree_edit_stage` has `book`, `npv` as items and `delta01` available; that helper's fixture is a *desk* view, so a `Doc` write asks before forking — the Presentation write for hiding does not):

```rust
/// §18.9.2: the tick is the toggle. Clicking a shown column's tick hides
/// it — a `Presentation` write, no fork question — and leaves the cursor
/// on that row, as `space` would.
#[gpui::test]
fn clicking_a_tick_hides_the_column_and_parks_the_cursor_there(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let tick = cx.debug_bounds("objectdialog-tick-npv").expect("npv paints a tick");
    cx.simulate_mouse_down(
        gpui::point(tick.origin.x + gpui::px(4.0), tick.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    let included = edit_draft(&shell, &cx, |d| {
        d.list_items("columns").unwrap().iter().find(|i| i.name == "npv").unwrap().included
    });
    assert!(!included, "npv is hidden");
    assert_eq!(edit_draft(&shell, &cx, |d| d.row_label(d.selected_row().unwrap())), "npv");
    // The write is a Presentation one: after the debounce the user
    // layer's view_presentation.toml names npv hidden.
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap_or_default();
    assert!(text.contains("hidden = [\"npv\"]"), "{text}");
}

/// On an available row the tick adds — `space`'s add — which is a `Doc`
/// write, so on a desk view it asks before forking rather than writing.
#[gpui::test]
fn clicking_an_available_rows_tick_adds_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let tick = cx.debug_bounds("objectdialog-tick-delta01").expect("delta01 is available");
    cx.simulate_mouse_down(
        gpui::point(tick.origin.x + gpui::px(4.0), tick.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("columns").unwrap().iter().map(|i| i.name.clone()).collect()
    });
    assert_eq!(names, ["book", "npv", "delta01"]);
    assert!(edit_draft(&shell, &cx, |d| d.confirm.is_some()), "a desk view asks before forking");
}
```

Check how the existing `space` tests in that file (search `fn ticking_` / `simulate_keystrokes("space")`) assert on the write and the confirm; mirror their exact file path and clock advance.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell clicking_a_tick clicking_an_available_rows_tick`
Expected: FAIL — `debug_bounds("objectdialog-tick-npv")` is `None`.

- [ ] **Step 3: Implement**

In `build_edit`, replace the `tick` construction with:

```rust
                // §18.9.2: the tick is the toggle. Its mouse-down stops
                // propagation so the row's own select does not double-
                // fire; the handler moves the cursor here itself and then
                // walks `space`'s path, so the mouse and the key cannot
                // disagree.
                let entity_for_tick = entity.clone();
                let tick_position = position;
                let tick_name = entry.name.clone();
                let mut tick = div()
                    .id(gpui::SharedString::from(format!("objectdialog-tick-{}", entry.name)))
                    .font_family(crate::fonts::MONO)
                    .w(px(13.))
                    .text_color(if entry.included {
                        theme.success
                    } else {
                        theme.muted_foreground
                    })
                    .debug_selector(move || format!("objectdialog-tick-{tick_name}"));
                // In the chain field (§18.8) the rows are completions and
                // the tick is a painted state, not a control (§18.9.2).
                if !draft.chain_entry {
                    tick = tick.cursor_pointer().on_mouse_down(
                        MouseButton::Left,
                        move |_event, window, cx| {
                            cx.stop_propagation();
                            entity_for_tick.update(cx, |shell, cx| {
                                on_tick_clicked(shell, tick_position, window, cx);
                            });
                        },
                    );
                }
                let tick = tick
                    .child(if entry.included { "✓" } else { "·" })
                    .into_any_element();
```

`.id()` needs the `StatefulInteractiveElement` import (`gpui::prelude::*` covers it). Beside `on_edit_row_clicked` add:

```rust
/// §18.9.2: a click on a row's tick. The cursor moves to the row first,
/// then exactly `space`'s path runs — `Draft::toggle_selected`, the
/// available-block refresh, revalidation, the scroll and
/// `commit_or_confirm` — so every write and every refusal the key gives,
/// the tick gives. Ends in [`dialog::sync_dialog_text`] like every mouse
/// handler that mutates the draft (§17.1 rule 3).
fn on_tick_clicked(
    shell: &mut ShellView,
    position: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    let Some(draft) = draft_mut(shell) else {
        return;
    };
    if position >= draft.visible_rows().len() {
        return;
    }
    draft.selected = position;
    match draft.toggle_selected() {
        Step::Changed => {
            maybe_refresh_available(shell);
            revalidate(shell);
            scroll_to_cursor(shell);
            commit_or_confirm(shell, cx);
        }
        Step::Refused(reason) => refuse_step(shell, reason),
        Step::Inert => set_notice(shell, "nothing on this row changes with a tick".to_string()),
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: PASS.

- [ ] **Step 5: Harness entry**

```sh
# §18.9.2: the tick toggles. Mutated to a bare select, a tick click moves
# the cursor and changes nothing.
run_mutation "objectdialog: a tick click toggles through space's path" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    draft.selected = position;
    match draft.toggle_selected() {' \
  '    draft.selected = position;
    match Step::Inert {' \
  geode-shell clicking_a_tick_hides_the_column_and_parks_the_cursor_there
```

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add -A && git commit -m "feat(objectdialog): the tick is the toggle (§18.9.2)"
```

---

### Task 6: Drag a row to reorder, add or remove

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (row element in `build_edit` ~2325-2335; new `DragGhost`, `on_row_dropped`)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `pub(in crate::shell) fn on_row_dropped(shell, src: &RowDrag, dst: &RowDrag, window, cx)`; `struct DragGhost { name: SharedString }` implementing `Render`.
- Consumes: `Draft::row_drag`, `Draft::drop_row`, `RowDrag` (Task 4).

- [ ] **Step 1: Write the failing tests**

Append to `tests/objectdialog.rs`:

```rust
/// §18.9.3 at the shell level: the drop handler reorders, parks the
/// cursor on the dropped item and queues the presentation write. The
/// gesture itself is exercised in the next test; this one is what the
/// harness anchors on.
#[gpui::test]
fn the_drop_handler_reorders_and_writes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let (src, dst) = edit_draft(&shell, &cx, |d| {
        let rows = d.rows();
        let book = rows.iter().copied().find(|r| d.row_label(*r) == "book").unwrap();
        let npv = rows.iter().copied().find(|r| d.row_label(*r) == "npv").unwrap();
        (d.row_drag(book).unwrap(), d.row_drag(npv).unwrap())
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_row_dropped(shell, &src, &dst, window, cx);
        });
    });
    cx.run_until_parked();
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("columns").unwrap().iter().map(|i| i.name.clone()).collect()
    });
    assert_eq!(names, ["npv", "book"]);
    assert_eq!(edit_draft(&shell, &cx, |d| d.row_label(d.selected_row().unwrap())), "book");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap_or_default();
    assert!(text.contains("order = [\"npv\", \"book\"]"), "{text}");
}

/// The gesture through gpui: mouse-down on `book`, move past the 2 px
/// threshold (the drag starts and the ghost renders), release over
/// `npv`. If `TestAppContext` turns out not to drive gpui's drag
/// machinery, record that in the spec's as-built and keep the handler
/// test above as the guard.
#[gpui::test]
fn dragging_a_row_onto_another_reorders_the_list(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let book = cx.debug_bounds("objectdialog-item-book").expect("book paints");
    let npv = cx.debug_bounds("objectdialog-item-npv").expect("npv paints");
    let grab = gpui::point(book.origin.x + gpui::px(40.0), book.origin.y + gpui::px(4.0));
    let over_npv = gpui::point(npv.origin.x + gpui::px(40.0), npv.origin.y + gpui::px(4.0));
    cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(
        gpui::point(grab.x + gpui::px(6.0), grab.y + gpui::px(6.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    cx.simulate_mouse_move(over_npv, MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_up(over_npv, MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("columns").unwrap().iter().map(|i| i.name.clone()).collect()
    });
    assert_eq!(names, ["npv", "book"]);
}
```

`objectdialog::render` is `pub mod render` already; `on_row_dropped` must be `pub(in crate::shell)`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell drop_handler dragging_a_row`
Expected: compile error (`on_row_dropped` undefined).

- [ ] **Step 3: Implement**

In render.rs, add near `on_edit_row_clicked`:

```rust
/// The ghost gpui paints under the cursor during a row drag (§18.9.1):
/// the dragged name in the row's own type, on the popover surface.
struct DragGhost {
    name: gpui::SharedString,
}

impl gpui::Render for DragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(px(4.))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(self.name.clone())
    }
}

/// §18.9.3: a row was dropped on another. Resolves both by name through
/// [`Draft::drop_row`], then — on a change — revalidates, scrolls to the
/// item's new row and commits through the same path a keystroke takes,
/// so a desk-owned view still asks before a `Doc` write forks it. Inert
/// drops say nothing except the two a trader could mistake for a
/// failure: a catalogue-to-catalogue drop and a name that left the list
/// mid-drag. `pub(in crate::shell)` so the window tests can drive it
/// without gpui's drag machinery.
pub(in crate::shell) fn on_row_dropped(
    shell: &mut ShellView,
    src: &RowDrag,
    dst: &RowDrag,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    let Some(draft) = draft_mut(shell) else {
        return;
    };
    let resolves = draft.locate(src).is_some() && draft.locate(dst).is_some();
    match draft.drop_row(src, dst) {
        Step::Changed => {
            revalidate(shell);
            scroll_to_cursor(shell);
            commit_or_confirm(shell, cx);
        }
        Step::Refused(reason) => set_notice(shell, reason),
        Step::Inert if !src.own && !dst.own => {
            set_notice(shell, "the catalogue has no order".to_string());
        }
        Step::Inert if !resolves => set_notice(shell, "that row is gone".to_string()),
        Step::Inert => {}
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

Import `RowDrag` from `super::` alongside `Draft`/`Step`. Check `theme.popover`/`popover_foreground` exist on the pinned gpui-component theme (grep `popover` in the checkout's `crates/ui/src/theme`); fall back to `theme.background`/`theme.foreground` if not.

In `build_edit`'s list-row branch (the `EditRow::Item { .. } | EditRow::Available { .. }` arm), compute the payload and make the row stateful. The row element is currently built as `element` (an `h_flex()` div) and given `.on_mouse_down` after the match; change the post-match wiring to:

```rust
        let entity_for_row = entity.clone();
        let clicked = position;
        let mut row_el = element
            .child(label)
            .child(value)
            .debug_selector(move || selector.clone())
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    on_edit_row_clicked(shell, clicked, window, cx);
                });
            });
        // §18.9: list rows drag and accept drops; field rows do neither,
        // and in the chain field (§18.8) rows are completions.
        if let Some(payload) = (!draft.chain_entry)
            .then(|| draft.row_drag(edit_row))
            .flatten()
        {
            let entity_for_drop = entity.clone();
            let target = payload.clone();
            let ghost_name = gpui::SharedString::from(payload.name.clone());
            row_el = row_el
                .id(gpui::SharedString::from(format!("objectdialog-drag-{}-{}", payload.own, payload.name)))
                .cursor_grab()
                .on_drag(payload, move |_drag, _offset, _window, cx| {
                    cx.new(|_| DragGhost { name: ghost_name.clone() })
                })
                .can_drop(|value, _window, _cx| value.downcast_ref::<RowDrag>().is_some())
                .drag_over::<RowDrag>(move |style, _drag, _window, cx| {
                    style.border_t_2().border_color(cx.theme().primary)
                })
                .on_drop(move |dropped: &RowDrag, window, cx| {
                    let dropped = dropped.clone();
                    entity_for_drop.update(cx, |shell, cx| {
                        on_row_dropped(shell, &dropped, &target, window, cx);
                    });
                });
        }
```

`.id()` turns the `Div` into a `Stateful<Div>`; `on_drag`, `can_drop`, `drag_over`, `on_drop` are all on it. If `cursor_grab` does not exist at the pinned rev use `.cursor(gpui::CursorStyle::OpenHand)`. Because `row_el` changes type after `.id()`, declare it as `AnyElement` at the end: build the stateful branch and the plain branch each into `into_any_element()`; the surrounding `list.child(match section_header { .. })` already consumes an `AnyElement`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: `the_drop_handler_reorders_and_writes` PASS. If `dragging_a_row_onto_another_reorders_the_list` fails because the test context never starts the drag, first insert a `cx.update(|window, cx| { let _ = window.draw(cx); })` after the first move (gpui registers drop hitboxes on the next paint) and retry; if it still fails, delete that test and record "gesture untestable in `TestAppContext`" in the as-built (Task 8), keeping the handler test.

- [ ] **Step 5: Harness entry**

```sh
# §18.9.1: the payload is by NAME. Mutated to ignore the source and act
# on the cursor's row, a drop moves whichever row is selected.
run_mutation "objectdialog: a drop resolves its source by name, not the cursor" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    match draft.drop_row(src, dst) {' \
  '    match draft.drop_row(&draft.row_drag(draft.selected_row().unwrap_or(EditRow::Field(0))).unwrap_or_else(|| src.clone()), dst) {' \
  geode-shell the_drop_handler_reorders_and_writes
```

(If that mutant does not compile because of the borrow, use instead: mutate `revalidate(shell); scroll_to_cursor(shell); commit_or_confirm(shell, cx);` in `on_row_dropped`'s `Changed` arm to drop the `commit_or_confirm` call — name the entry "objectdialog: a drop commits its write" — and anchor with the preceding `Step::Changed => {` line so it is unique.)

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add -A && git commit -m "feat(objectdialog): drag a row to reorder, add or remove (§18.9.1, §18.9.3)"
```

---

### Task 7: A click on a completion row completes the chain

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (the row `on_mouse_down` in `build_edit`; new `on_completion_clicked`)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `Draft::chain_entry`, `Draft::complete_chain` (`groupings.rs:254`).

- [ ] **Step 1: Write the failing window test**

```rust
/// §18.9.4: clicking a completion row is the mouse form of `tab` — the
/// trailing segment is replaced by that row and the next opened with
/// ` / `; the field stays focused and the pill still reads `chain`.
#[gpui::test]
fn clicking_a_completion_row_completes_the_chain(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry));
    // Open a fresh segment so `lhu` is offered.
    cx.simulate_input(" / ");
    cx.run_until_parked();
    let row = cx.debug_bounds("objectdialog-item-lhu").expect("lhu is a completion");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "book / lhu / ");
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry), "the field is still open");
    let input_text = shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(input_text, "book / lhu / ", "the sync wrote the completion into the field");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell clicking_a_completion_row`
Expected: FAIL — query unchanged (the click only moved the cursor).

- [ ] **Step 3: Implement**

In `build_edit`'s post-match wiring, choose the handler by `draft.chain_entry`:

```rust
        let chain = draft.chain_entry;
        let mut row_el = element
            .child(label)
            .child(value)
            .debug_selector(move || selector.clone())
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    if chain {
                        on_completion_clicked(shell, clicked, window, cx);
                    } else {
                        on_edit_row_clicked(shell, clicked, window, cx);
                    }
                });
            });
```

And the handler:

```rust
/// §18.9.4: a click on a completion row while the chain field is open —
/// the mouse form of `tab`. The cursor moves to the row and
/// `Draft::complete_chain` runs unchanged; the mode stays `Filter`, so
/// the sync writes the new text into the field and keeps it focused.
fn on_completion_clicked(
    shell: &mut ShellView,
    position: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    let completed = draft_mut(shell).is_some_and(|draft| {
        if position >= draft.visible_rows().len() {
            return false;
        }
        draft.selected = position;
        draft.complete_chain()
    });
    if completed {
        shell.object_dialog_scroll.scroll_to_item(0);
    } else {
        set_notice(shell, "nothing to complete here".to_string());
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: PASS.

- [ ] **Step 5: Harness entry**

```sh
# §18.9.4: a completion click completes. Mutated to a bare select, the
# click moves the highlight and the field's text is unchanged.
run_mutation "objectdialog: a completion row click completes the chain" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        draft.selected = position;
        draft.complete_chain()' \
  '        draft.selected = position;
        true' \
  geode-shell clicking_a_completion_row_completes_the_chain
```

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add -A && git commit -m "feat(objectdialog): a completion row click completes the chain (§18.9.4)"
```

---

### Task 8: Verification, docs and handoff

**Files:**
- Modify: `CLAUDE.md` (a new paragraph after the "Groupings dialog shortcuts" one)
- Modify: both specs (add `### 17.4 As built` and `### 18.9.6 As built`)
- Modify: `crates/geode-shell/src/shell/dialog.rs` `sync_dialog_text`'s doc comment (the seam-class list gains the frozen-row click, the tick, the drop and the completion click)
- Modify: `/Users/mch/.claude/projects/-Users-mch-Repos-geode/memory/` (a handoff memory + `MEMORY.md` line)

- [ ] **Step 1: Full verification**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p geode-shell --features test-support --all-targets
cargo test --workspace
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed
```

Expected: all green; every new entry `CAUGHT`; no `SURVIVED`. Fix anything red before moving on.

- [ ] **Step 2: Docs**

Add to `CLAUDE.md` after the Groupings shortcuts paragraph:

> **Dialog mouse parity (2026-09-12, interaction-model spec §17, 4c §18.9):** the frozen filter row is the mouse form of `/` (`dialog::enter_filter_by_mouse`, cancelling a keybinding capture first); a row click does what `enter` would — the keybinding dialog captures on one click (`click_listens`), a browse row opens its edit stage through `enter_edit_stage` (select-only while naming); in the edit stage the tick is the toggle (`on_tick_clicked`, `space`'s exact path), list rows drag through gpui's own `on_drag`/`on_drop` with `Draft::drop_row` deciding every case by NAME (`RowDrag`, so a keystroke mid-drag cannot redirect the drop) — drop on a row takes its index, an available row dropped into the members adds there, a member dropped into the catalogue removes — and a completion-row click in the chain field is `tab`. Every one of those handlers ends in `dialog::sync_dialog_text` (the fifth seam class); `press_verb` is still the one audited exception. The gpui gesture itself is a display-check item.

Write `### 17.4 As built` and `### 18.9.6 As built` in the specs: commits, what the window tests proved, whether the simulated drag test survived Task 6, and the display-check list (ghost, drag-over highlight, cursor styles, the tick's hit area).

Update `sync_dialog_text`'s doc comment seam list.

- [ ] **Step 3: Memory**

Write `dialog-mouse-parity-handoff.md` (type `project`) recording: branch, commits, the three rulings (tick box + drag body; cross-block drag; completion click), the display-check items, and that `click_selects_or_listens` no longer exists. Add its line to `MEMORY.md`.

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add -A && git commit -m "docs: dialog mouse parity as built (§17.4, §18.9.6), CLAUDE.md, sync seam list"
```

Then invoke `superpowers:requesting-code-review` for the whole branch before `superpowers:finishing-a-development-branch`.
