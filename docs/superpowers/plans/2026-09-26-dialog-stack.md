# Dialog Stack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let shell dialogs stack. Opening a dialog while another is open pushes the new one. Enter or Escape pops one level and reveals the dialog beneath, as it was left.

**Architecture:** `ShellView::modal: Option<ShellModal>` becomes `modals: Vec<ShellModal>`, where each entry records its `DialogKind`. The per-kind state fields stay. A kind appears at most once in the stack, and closing clears only the top kind's field. The one shared `dialog_input` is snapshotted into the covered entry on push and restored on pop. Every router that used to pick "whichever state is `Some`" now picks by top kind. With a dialog open, unclaimed chords reach the shell only for dialog-opening actions and the palette toggle. The palette opens above the stack and runs any action.

**Tech Stack:** Rust, GPUI + gpui-component 0.6.2 (`InputState::cursor`, `InputState::set_selected_range`), the `geode-shell` crate.

**Spec:** `docs/superpowers/specs/2026-09-26-dialog-stack-design.md`

## Global Constraints

- Dialogs open only through `shell::dialog::open_shell_dialog` / `open_shell_dialog_with_key`.
- The pure dialog draft is the source of truth. `sync_dialog_text` is the text/focus bridge for mode dialogs.
- One instance per kind. A request for the top kind does nothing. A request for a lower kind sets a status notice and leaves the stack unchanged.
- Chords reach through a dialog only for `dialog::opens_dialog` actions and the palette toggle. The palette lists and runs every action.
- Only the top dialog paints. There is no breadcrumb or depth marker.
- Every pointer action keeps its keyboard route. No I/O or state mutation during render.
- Test production routes: real keystrokes through `simulate_keystrokes`, real dispatch through `ShellView::dispatch`.
- Code comments state the local invariant and the failure it prevents, never a task number.
- Commit before running mutations; the harness edits tracked files in place.
- `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` stay clean.

## Deviation from the spec, recorded here and in the spec

Spec §5 put an `opens_dialog: bool` on `ActionDef`. Only `ShellView::dispatch` opens shell dialogs; feature crates cannot. So the flag becomes one shell function, `dialog::opens_dialog(&ActionId) -> bool`, which sits beside that dispatch table. A registry-wide test pins it to actual dispatch behavior in both directions. This avoids editing 26 `ActionDef` literals across seven crates for a fact only the shell can know. Task 5 updates spec §5 to match.

## Review Focus

1. **Filter-only dialog covered, then revealed.** A picker or expression dialog with typed text is covered by a chord-opened dialog, then that dialog is dismissed. The typed text, caret and focus must come back. The expression dialog is the sharp case: its input text *is* its value (`scope_expr_view::handle_key` reads `dialog_input` on Enter), so losing it silently loses the user's expression. Pinned in Task 2 (`a_covered_expression_dialog_gets_its_typed_text_back`).
2. **Typing into the top dialog while a different kind sits beneath.** The `Change` subscriber used to route to the first `Some` field in a fixed order. With Object beneath As-of, typing would filter the hidden object dialog. Pinned in Task 2 (`a_pushed_dialog_owns_the_shared_input_until_it_pops`).
3. **Expression suggestions refreshing from the wrong dialog's text.** The `dialog_input` observer runs `expr_suggest::refresh` on every notify. A covered expression dialog must not recompute suggestions from a Settings filter query typed above it. Pinned in Task 2 (`a_covered_expression_dialog_gets_its_typed_text_back` asserts the completion is unchanged while covered).
4. **Palette action that moves tile focus while a dialog is open.** `note_keyboard_focus_move` arms `pending_focus_restore`, and render would then focus the shell root, stealing focus from the dialog's input. Pinned in Task 4 (`a_palette_action_behind_the_stack_leaves_focus_on_the_top_dialog`).
5. **Return-to-field after a nested session.** The first dialog was opened from the scope-bar field, then a second dialog was pushed and a palette opened and closed mid-stack. The last pop must still return focus to the field. Pinned in Task 4 (`the_last_pop_returns_to_the_field_after_a_palette_mid_stack`).

---

## File Structure

- Modify `crates/geode-shell/src/shell/dialog.rs`: add `DialogKind`, `SavedInput`, `can_open`, `refocus_top`, `opens_dialog`; opener gains `kind`; `sync_dialog_text` and `enter_filter_by_mouse` route by top kind.
- Modify `crates/geode-shell/src/shell/mod.rs`: `modals` field, accessors, `close_modal` pop, `clear_dialog_state`, `Change` subscriber by top kind, field docs.
- Modify `crates/geode-shell/src/shell/input.rs`: modal branch (palette above stack, chord pass-through).
- Modify `crates/geode-shell/src/shell/palette_ctl.rs`: stack-aware toggle, close and commit.
- Modify `crates/geode-shell/src/shell/render.rs`: guards, modal paints below palette, focus-restore skip.
- Modify `crates/geode-shell/src/shell/expr_suggest.rs`: completion by top kind; delivery to any live completion.
- Modify the openers (replace refusal guard, pass kind): `settings_view.rs`, `keybindings_view.rs`, `picker.rs`, `asof_view.rs`, `scope_expr_view.rs`, `choicedialog.rs`, `objectdialog/render.rs`.
- Modify guards: `drag.rs`, `addfilter.rs`.
- Create `crates/geode-shell/src/shell/tests/dialog_stack.rs`; register it in `crates/geode-shell/src/shell/tests/mod.rs`.
- Modify existing tests mechanically (`.modal.is_some()` → `.modal_open()`).
- Modify `scripts/mutation-check.sh`, `docs/current/input-and-dialogs.md`, `docs/current/shell.md`, `crates/geode-shell/README.md`, the spec §5, and `TODO.md`.

---

### Task 1: Stack representation, behavior unchanged

Swap the representation without changing behavior. `can_open` still refuses whenever any dialog is open, and `close_modal` still clears everything. After this task the whole suite passes unchanged except for the mechanical accessor rename.

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (field at the `modal: Option<dialog::ShellModal>` declaration, its initializer `modal: None,`, `close_modal`)
- Modify: `render.rs`, `input.rs`, `drag.rs`, `addfilter.rs`, and the seven opener files
- Modify: every file under `crates/geode-shell/src/shell/tests/` that reads `.modal`

**Interfaces:**
- Produces:
  - `pub enum DialogKind { Settings, Keybindings, Picker, AsOf, ScopeExpr, Choice, Object, Plain }` (`Copy, Eq, Debug`) in `shell::dialog`
  - `ShellModal::kind: DialogKind`
  - `ShellView::modal_open(&self) -> bool`, `top_modal(&self) -> Option<&dialog::ShellModal>`, `top_kind(&self) -> Option<dialog::DialogKind>`, `modal_depth(&self) -> usize` (all `pub(crate)`)
  - `dialog::can_open(view: &mut ShellView, kind: DialogKind) -> bool` (`pub(crate)`)
  - `open_shell_dialog(view, window, cx, kind: DialogKind, title, build)` and `open_shell_dialog_with_key(view, window, cx, kind: DialogKind, title, build, on_key, focus_filter)`

- [ ] **Step 1: Add `DialogKind` and `kind` to `dialog.rs`**

Insert above `pub struct ShellModal`:

```rust
/// Which dialog a stack entry is. Each kind but `Plain` owns one `ShellView`
/// state field, so a kind appears at most once in the stack (see [`can_open`]);
/// a second instance would overwrite the live one's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Settings,
    Keybindings,
    /// The dimension picker (`picker.rs`).
    Picker,
    AsOf,
    ScopeExpr,
    /// Every `choicedialog` target (tile kinds, grouping, log level): they share
    /// the one `choice_dialog` field.
    Choice,
    /// Every object-dialog domain: they share the one `object_dialog` field.
    Object,
    /// A modal with no state field of its own.
    Plain,
}

impl DialogKind {
    /// The status notice for a request refused because this kind is already
    /// open lower in the stack.
    pub(crate) fn already_open_notice(self) -> &'static str {
        match self {
            DialogKind::Settings => "settings is already open underneath",
            DialogKind::Keybindings => "keybindings is already open underneath",
            DialogKind::Picker => "the picker is already open underneath",
            DialogKind::AsOf => "as-of is already open underneath",
            DialogKind::ScopeExpr => "the expression dialog is already open underneath",
            DialogKind::Choice => "a choice list is already open underneath",
            DialogKind::Object => "a configuration dialog is already open underneath",
            DialogKind::Plain => "a dialog is already open underneath",
        }
    }
}
```

Add `pub kind: DialogKind,` as the first field of `ShellModal`, with the doc line `/// Which state field this entry owns; see [`DialogKind`].`

- [ ] **Step 2: Add `can_open` (single-modal behavior for now)**

Below `DialogKind`'s impl:

```rust
/// Whether a dialog of `kind` may be pushed now. Openers call this before
/// installing their state, because installing state for a kind that is already
/// open would overwrite the live instance.
pub(crate) fn can_open(view: &mut ShellView, _kind: DialogKind) -> bool {
    view.modals.is_empty()
}
```

This keeps today's single-modal refusal; Task 2 replaces the body.

- [ ] **Step 3: Move `ShellView` to a `Vec` and add accessors (`mod.rs`)**

Replace the field declaration `modal: Option<dialog::ShellModal>,` with:

```rust
    /// Open modals, bottom first. Only the last entry paints and receives keys;
    /// the rest keep their state and reappear when everything above them pops.
    /// Installed through `dialog::open_shell_dialog`. Dialog-specific data and
    /// scroll handles live in the per-kind fields below; a kind's field is `Some`
    /// exactly while that kind is in this stack.
    modals: Vec<dialog::ShellModal>,
```

Replace the initializer `modal: None,` with `modals: Vec::new(),`.

Add to `impl ShellView` next to `close_modal`:

```rust
    /// Whether any modal is open.
    pub(crate) fn modal_open(&self) -> bool {
        !self.modals.is_empty()
    }

    /// The live (topmost) modal.
    pub(crate) fn top_modal(&self) -> Option<&dialog::ShellModal> {
        self.modals.last()
    }

    /// The live modal's kind, which decides who owns the shared input and keys.
    pub(crate) fn top_kind(&self) -> Option<dialog::DialogKind> {
        self.modals.last().map(|m| m.kind)
    }

    /// How many modals are stacked.
    pub(crate) fn modal_depth(&self) -> usize {
        self.modals.len()
    }
```

Change `close_modal`'s first line from `self.modal = None;` to `self.modals.clear();`. Leave the rest unchanged for now.

- [ ] **Step 4: Opener takes the kind (`dialog.rs`)**

In `open_shell_dialog`, add a `kind: DialogKind` parameter after `cx` and pass it through:

```rust
pub fn open_shell_dialog<F>(
    view: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
    kind: DialogKind,
    title: impl Into<SharedString>,
    build: F,
) where
    F: Fn(&ShellView, &mut Window, &mut App) -> AnyElement + 'static,
{
    open_shell_dialog_with_key(view, window, cx, kind, title, build, None, false);
}
```

In `open_shell_dialog_with_key`, add `kind: DialogKind,` after `cx`. First line of the body:

```rust
    // Backstop for an opener that skipped its own `can_open` check. By then that
    // opener may already have overwritten the live state, which is why each
    // opener checks first.
    if !can_open(view, kind) {
        return;
    }
```

Replace the `view.modal = Some(ShellModal { ... });` block with:

```rust
    view.modals.push(ShellModal {
        kind,
        title: title.into(),
        title_extra: None,
        build: Rc::new(build),
        on_key,
        back: None,
    });
```

Clippy will flag `too_many_arguments` (8). Add `#[allow(clippy::too_many_arguments)]` above `open_shell_dialog_with_key`, with the comment `// One door for every dialog; bundling these into a struct would only rename them.`

In `set_title_extra` and `set_back`, replace `view.modal.as_mut()` with `view.modals.last_mut()`. In `back_available`, replace `view.modal.as_ref()` with `view.modals.last()`. In `step_back`, replace `view.modal.as_ref()` with `view.modals.last()`. Update the `ShellModal` doc line that mentions `self.modal` to `self.modals`.

- [ ] **Step 5: Openers pass their kind and use `can_open`**

In each opener, replace the guard `if view.modal.is_some() { return; }` (or `shell.modal`) with a `can_open` call, and add the kind argument to the opener call right after `cx,`:

| File | Guard becomes | Kind passed |
|---|---|---|
| `settings_view.rs` (`open`) | `if !dialog::can_open(view, dialog::DialogKind::Settings) { return; }` | `dialog::DialogKind::Settings` |
| `keybindings_view.rs` (`open`) | `... DialogKind::Keybindings ...` | `DialogKind::Keybindings` |
| `picker.rs` (`open`) | `... DialogKind::Picker ...` | `DialogKind::Picker` |
| `asof_view.rs` (`open`) | `... DialogKind::AsOf ...` | `DialogKind::AsOf` |
| `scope_expr_view.rs` (`open`) | `... DialogKind::ScopeExpr ...` | `DialogKind::ScopeExpr` |
| `choicedialog.rs` (the shared open fn at the `if view.modal.is_some()` guard) | `... DialogKind::Choice ...` | `DialogKind::Choice` |
| `objectdialog/render.rs` (`open`) | `... DialogKind::Object ...` | `DialogKind::Object` |
| `objectdialog/render.rs` (`open_save_scope`) | `if !dialog::can_open(shell, dialog::DialogKind::Object) { return; }` | (calls `open`, no direct opener call) |

Use whatever path each file already uses for `dialog` (`dialog::` or `super::dialog::`). Update each opener's doc comment "A no-op if a modal is already open" to "A no-op when this kind is already open (see `dialog::can_open`)".

- [ ] **Step 6: Guards read the accessor**

Replace mechanically in production code:
- `render.rs`: both `|| self.modal.is_some()` → `|| self.modal_open()`; `self.modal.is_none()` → `!self.modal_open()`; `.key_context(if self.modal.is_some() {` → `.key_context(if self.modal_open() {`; the modal extraction `let modal = self.modal.as_ref().map(|modal| {` → `let modal = self.modals.last().map(|modal| {`; update the adjacent comment's `self.modal` to `self.modals`.
- `drag.rs`: three `|| self.modal.is_some()` → `|| self.modal_open()`; `&& self.modal.is_none()` → `&& !self.modal_open()`.
- `addfilter.rs`: both `self.modal.is_some()` → `self.modal_open()`.
- `input.rs` modal branch: `self.modal.is_some()` (three places) → `self.modal_open()`; `self.modal.as_ref().and_then(|m| m.on_key.clone())` → `self.modals.last().and_then(|m| m.on_key.clone())`.

Run: `grep -rn '\.modal\b' crates/geode-shell/src | grep -v '/tests/'`
Expected: no output.

- [ ] **Step 7: Migrate the tests mechanically**

```bash
cd /Users/mch/Repos/geode
perl -pi -e 's/\b([a-z_]+)\.modal\.is_none\(\)/!$1.modal_open()/g; s/\b([a-z_]+)\.modal\.is_some\(\)/$1.modal_open()/g; s/\b([a-z_]+)\.modal\.as_ref\(\)/$1.top_modal()/g' crates/geode-shell/src/shell/tests/*.rs
grep -rn '\.modal\b' crates/geode-shell/src/shell/tests
```

Expected: no output from the grep. In `tests/chrome_and_dialogs.rs`, the direct `dialog::open_shell_dialog(shell, window, cx, "Test modal", ...)` call gains `dialog::DialogKind::Plain,` after `cx,`.

- [ ] **Step 8: Build and run the suite**

Run: `cargo test -p geode-shell 2>&1 | tail -20`
Expected: all tests pass. Behavior is unchanged.

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings 2>&1 | tail -5`
Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add -A crates/geode-shell
git commit -m "refactor(shell): hold modals as a kind-tagged stack, still single-depth

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Stacking semantics — push, pop, reveal, routing by top kind

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs`
- Modify: `crates/geode-shell/src/shell/expr_suggest.rs`
- Create: `crates/geode-shell/src/shell/tests/dialog_stack.rs`
- Modify: `crates/geode-shell/src/shell/tests/mod.rs` (add `mod dialog_stack;` beside the other `mod` lines)
- Modify: `crates/geode-shell/src/shell/tests/picker.rs` (make `services_with_pickable` `pub(super)`)

**Interfaces:**
- Consumes: Task 1's `DialogKind`, `can_open`, accessors.
- Produces:
  - `dialog::SavedInput { text: String, cursor: usize }`
  - `ShellModal::saved_input: Option<SavedInput>`
  - `dialog::refocus_top(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>)` (`pub(crate)`)
  - `ShellView::clear_dialog_state(&mut self, kind: dialog::DialogKind)` (private)

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-shell/src/shell/tests/dialog_stack.rs`:

```rust
//! Stacked dialogs through production routes. A dialog opened over another
//! pushes; Enter or Escape pops one level, and the revealed dialog has its
//! query, caret, mode and focus back. One instance per kind.

use super::*;
use crate::dialogmode::DialogMode;
use crate::shell::dialog::DialogKind;
use geode_core::query::DistinctOutcome;

fn input_text(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> String {
    shell.read_with(cx, |s, cx| s.dialog_input.read(cx).value().to_string())
}

fn input_cursor(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> usize {
    shell.read_with(cx, |s, cx| s.dialog_input.read(cx).cursor())
}

fn kinds(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> Vec<DialogKind> {
    shell.read_with(cx, |s, _| s.modals.iter().map(|m| m.kind).collect())
}

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// Views in filter mode with "ab" typed, then Settings pushed through dispatch.
/// Settings owns the shared input while on top, and typing goes to it, not to the
/// hidden Views. Escape pops Settings only, and Views has its query, caret, mode
/// and focus back.
#[gpui::test]
fn a_pushed_dialog_owns_the_shared_input_until_it_pops(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    assert_eq!(input_text(&shell, &mut vcx), "ab");

    dispatch_action(&shell, "settings::open", &mut vcx);
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object, DialogKind::Settings]);
    assert_eq!(input_text(&shell, &mut vcx), "", "the pushed dialog starts with a clear input");

    vcx.simulate_keystrokes("/ x");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.settings.as_ref().unwrap().effective_query().to_string()),
        "x"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .object_dialog
            .as_ref()
            .unwrap()
            .effective_query()
            .to_string()),
        "ab",
        "typing into the top dialog must not filter the one beneath"
    );

    // Settings' filter: escape leaves filter, a second escape closes Settings.
    vcx.simulate_keystrokes("escape escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(shell.read_with(&vcx, |s, _| s.settings.is_none()));
    assert_eq!(input_text(&shell, &mut vcx), "ab");
    assert_eq!(input_cursor(&shell, &mut vcx), 2);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.object_dialog.as_ref().unwrap().mode),
        DialogMode::Filter
    );
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// Enter commits the top dialog (as-of: the highlighted preset) and pops
/// exactly one level.
#[gpui::test]
fn a_commit_pops_one_level(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    dispatch_action(&shell, "frame::as_of", &mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object, DialogKind::AsOf]);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(shell.read_with(&vcx, |s, _| s.as_of_dialog.is_none()));
    assert!(shell.read_with(&vcx, |s, _| s.object_dialog.is_some()));
    assert!(vcx.debug_bounds("shell-modal-panel").is_some(), "Views paints again");
}

/// A request for the kind already on top does nothing. A request for a kind lower
/// in the stack posts a notice and changes nothing: no push, no state overwrite.
#[gpui::test]
fn a_kind_already_in_the_stack_is_refused(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    dispatch_action(&shell, "settings::open", &mut vcx);

    dispatch_action(&shell, "settings::open", &mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object, DialogKind::Settings]);
    assert_eq!(shell.read_with(&vcx, |s, _| s.notice), None);

    dispatch_action(&shell, "config::scopes", &mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object, DialogKind::Settings]);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice),
        Some(DialogKind::Object.already_open_notice())
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .object_dialog
            .as_ref()
            .unwrap()
            .effective_query()
            .to_string()),
        "ab",
        "the refused request must not reinstall the live object dialog's state"
    );
}

/// The expression dialog's input text is its value. Covered by Settings, its typed
/// expression survives, and its suggestions are not recomputed from Settings' query.
/// Revealed, the text and caret come back and the field has focus.
#[gpui::test]
fn a_covered_expression_dialog_gets_its_typed_text_back(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "frame::scope_expression");
    vcx.simulate_input("book = ");
    let before = shell.read_with(&vcx, |s, _| {
        format!("{:?}", s.scope_expr_dialog.as_ref().unwrap().completion)
    });

    dispatch_action(&shell, "settings::open", &mut vcx);
    vcx.simulate_keystrokes("/ z z");
    assert_eq!(
        shell.read_with(&vcx, |s, _| format!(
            "{:?}",
            s.scope_expr_dialog.as_ref().unwrap().completion
        )),
        before,
        "a covered expression field must not refresh from another dialog's text"
    );

    vcx.simulate_keystrokes("escape escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::ScopeExpr]);
    assert_eq!(input_text(&shell, &mut vcx), "book = ");
    assert_eq!(input_cursor(&shell, &mut vcx), "book = ".len());
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// A covered picker still receives its distinct values, and shows them when revealed.
#[gpui::test]
fn a_covered_picker_receives_its_delivery(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, super::picker::services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);
    dispatch_action(&shell, "frame::as_of", &mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Picker, DialogKind::AsOf]);

    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 3)]),
            },
            cx,
        )
    });
    assert!(shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().values.is_some()));

    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Picker]);
    assert!(vcx.debug_bounds("picker-value-BK000").is_some());
}
```

In `tests/picker.rs`, change `fn services_with_pickable()` to `pub(super) fn services_with_pickable()`. In `tests/mod.rs`, add `mod dialog_stack;` in the list of test modules, keeping the list alphabetical.

If `ExprCompletion` does not derive `Debug`, add `#[derive(Debug)]` to it in `crates/geode-shell/src/exprcomplete.rs`, or derive `PartialEq` and compare values instead. Either way, the assertion must compare the whole completion, not one field.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p geode-shell dialog_stack 2>&1 | tail -30`
Expected: all five fail on the first push assertion, because `kinds` is `[Object]` and the push was refused.

- [ ] **Step 3: `can_open` allows other kinds**

Replace the Task 1 body in `dialog.rs`:

```rust
/// Whether a dialog of `kind` may be pushed now. Openers call this before
/// installing their state, because a second instance of a kind would overwrite
/// the live one's state field. A request for the kind already on top does
/// nothing; one for a kind lower in the stack says so in the status bar.
pub(crate) fn can_open(view: &mut ShellView, kind: DialogKind) -> bool {
    let Some(at) = view.modals.iter().position(|m| m.kind == kind) else {
        return true;
    };
    if at + 1 < view.modals.len() {
        view.notice = Some(kind.already_open_notice());
    }
    false
}
```

- [ ] **Step 4: Snapshot the covered input on push, and record the field flag only for the base**

Add to `dialog.rs` beside `ShellModal`:

```rust
/// The shared input's text and caret as the entry beneath a push left them.
/// The input is one entity reused at every depth, and some dialogs (the
/// expression dialog) keep their value only in it.
pub struct SavedInput {
    pub text: String,
    pub cursor: usize,
}
```

Add a `ShellModal` field:

```rust
    /// Set while another entry covers this one; restored by [`refocus_top`].
    pub saved_input: Option<SavedInput>,
```

In `open_shell_dialog_with_key`, replace

```rust
    view.overlay_return_to_filter = view.filter_field_focused(window, cx);
```

with

```rust
    // Only the stack's base records where to return: a nested push must not
    // overwrite it with "the dialog beneath had focus".
    let first = view.modals.is_empty();
    if first {
        view.overlay_return_to_filter = view.filter_field_focused(window, cx);
    }
    // The covered entry keeps the shared input's text and caret; the push below
    // clears the input for the new dialog.
    if let Some(covered) = view.modals.last_mut() {
        let input = view.dialog_input.read(cx);
        covered.saved_input = Some(SavedInput {
            text: input.value().to_string(),
            cursor: input.cursor(),
        });
    }
```

Keep the existing comment above the flag line, updated to say it records for the base only. Add `saved_input: None,` to the `ShellModal { .. }` literal in the push.

If the borrow checker rejects `view.modals.last_mut()` alongside `view.dialog_input.read(cx)`, read the input into locals first:

```rust
    let (text, cursor) = {
        let input = view.dialog_input.read(cx);
        (input.value().to_string(), input.cursor())
    };
    if let Some(covered) = view.modals.last_mut() {
        covered.saved_input = Some(SavedInput { text, cursor });
    }
```

- [ ] **Step 5: `refocus_top` in `dialog.rs`**

```rust
/// Give the top dialog back the shared input and focus once whatever covered
/// it (a popped dialog, the palette) is gone. Restores the text and caret the
/// entry had when it was covered, then chooses focus: mode dialogs through
/// [`sync_dialog_text`], filter-only dialogs by focusing the input they always
/// type into. `set_value` emits no `Change`, and the restored text is what the
/// entry's state already holds.
pub(crate) fn refocus_top(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(top) = view.modals.last_mut() else {
        return;
    };
    let kind = top.kind;
    if let Some(saved) = top.saved_input.take() {
        let input = view.dialog_input.clone();
        input.update(cx, |i, cx| {
            i.set_value(saved.text, window, cx);
            i.set_selected_range(saved.cursor..saved.cursor, cx);
        });
    }
    match kind {
        DialogKind::Picker | DialogKind::Choice | DialogKind::ScopeExpr => {
            let handle = view.dialog_input.read(cx).focus_handle(cx);
            handle.focus(window, cx);
        }
        DialogKind::Plain => {}
        DialogKind::Settings | DialogKind::Keybindings | DialogKind::Object | DialogKind::AsOf => {
            sync_dialog_text(view, window, cx);
        }
    }
}
```

- [ ] **Step 6: `close_modal` pops one level (`mod.rs`)**

Replace the body of `close_modal`:

```rust
    /// Close the live (topmost) modal: clear only its kind's state, then give the
    /// revealed dialog back its input and focus, or, when none remains, return
    /// focus to where the first dialog was opened from.
    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(top) = self.modals.pop() {
            self.clear_dialog_state(top.kind);
        }
        if self.modals.is_empty() {
            self.return_focus_from_overlay(window, cx);
        } else {
            dialog::refocus_top(self, window, cx);
        }
        cx.notify();
    }

    /// Drop the state field `kind` owns. A field left behind would swallow the
    /// next same-kind dialog's queries.
    fn clear_dialog_state(&mut self, kind: dialog::DialogKind) {
        use dialog::DialogKind;
        match kind {
            DialogKind::Settings => self.settings = None,
            DialogKind::Keybindings => self.keybindings = None,
            DialogKind::Picker => self.picker = None,
            DialogKind::AsOf => self.as_of_dialog = None,
            DialogKind::ScopeExpr => self.scope_expr_dialog = None,
            DialogKind::Choice => self.choice_dialog = None,
            DialogKind::Object => self.object_dialog = None,
            DialogKind::Plain => {}
        }
    }
```

Update the doc comments on the `settings`, `keybindings`, `picker`, `as_of_dialog`, `scope_expr_dialog`, `choice_dialog`, `object_dialog` fields where they say "cleared on close" or "cleared by `close_modal`" to say "cleared when its kind pops".

- [ ] **Step 7: `sync_dialog_text` routes by top kind (`dialog.rs`)**

Replace the body with:

```rust
    // The live dialog owns the shared input. Several kinds' states can be `Some`
    // at once while stacked, so the owner is the top kind, never the first
    // non-empty field.
    let (mode, listening, query) = match shell.top_kind() {
        Some(DialogKind::AsOf) => {
            // No list mode or capture state: its query always owns the focused input.
            let Some(state) = shell.as_of_dialog.as_ref() else {
                return;
            };
            let query = state.query();
            let input = shell.dialog_input.clone();
            if input.read(cx).text() != query {
                input.update(cx, |i, cx| i.set_value(query, window, cx));
            }
            input.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        Some(DialogKind::Keybindings) => {
            let Some(state) = shell.keybindings.as_ref() else {
                return;
            };
            (state.mode, state.listening.is_some(), state.query.as_str())
        }
        Some(DialogKind::Object) => {
            let Some(state) = shell.object_dialog.as_ref() else {
                return;
            };
            (state.mode, false, state.effective_query())
        }
        Some(DialogKind::Settings) => {
            let Some(state) = shell.settings.as_ref() else {
                return;
            };
            (state.mode, false, state.effective_query())
        }
        // Filter-only dialogs keep their own focus path (see `refocus_top`).
        _ => return,
    };
```

Keep the remainder (the input text compare and `match dialogmode::focus_target(..)`) unchanged. Update the function's doc comment: "These dialog states are mutually exclusive: closing clears all of them" becomes "Several states can be `Some` while stacked; the top kind decides."

The as-of arm borrows `shell` for `query` while cloning `dialog_input`, exactly as the old code did. If borrowck complains, clone `query` to a `String` first.

- [ ] **Step 8: `enter_filter_by_mouse` routes by top kind (`dialog.rs`)**

Replace the `if let … else if let …` chain with:

```rust
    match shell.top_kind() {
        Some(DialogKind::Keybindings) => {
            let Some(state) = shell.keybindings.as_mut() else {
                return;
            };
            // Keep the confirmation's exclusive input route until it is answered.
            if state.confirm.is_some() {
                return;
            }
            state.listening = None;
            dialogmode::enter_filter(&mut state.mode, &mut state.filter_entry_query, &state.query);
        }
        Some(DialogKind::Object) => {
            let Some(state) = shell.object_dialog.as_mut() else {
                return;
            };
            // `build_edit` still paints the frozen row during confirmation, so guard
            // the transition here as well as in keyboard routing.
            if state.confirm.is_some() {
                return;
            }
            state.enter_filter();
        }
        Some(DialogKind::Settings) => {
            let Some(state) = shell.settings.as_mut() else {
                return;
            };
            dialogmode::enter_filter(&mut state.mode, &mut state.filter_entry_query, &state.query);
        }
        _ => {}
    }
```

- [ ] **Step 9: The `dialog_input` `Change` subscriber routes by top kind (`mod.rs`)**

In the `cx.subscribe_in(&dialog_input, ..)` closure, replace the `if let Some(state) = view.object_dialog.as_mut() { .. } else if let Some(state) = view.keybindings.as_mut() { .. } … else if let Some(state) = view.scope_expr_dialog.as_mut() { .. }` chain with a `match` on the top kind. Move each arm's body verbatim:

```rust
            // Route to the live dialog. While stacked, several of these fields are
            // `Some`; typing belongs to the top one only.
            match view.top_kind() {
                Some(dialog::DialogKind::Object) => {
                    if let Some(state) = view.object_dialog.as_mut() {
                        // (existing object_dialog arm body, verbatim)
                    }
                }
                Some(dialog::DialogKind::Keybindings) => {
                    if let Some(state) = view.keybindings.as_mut() {
                        // (existing keybindings arm body, verbatim)
                    }
                }
                Some(dialog::DialogKind::Settings) => {
                    if let Some(state) = view.settings.as_mut() {
                        // (existing settings arm body, verbatim)
                    }
                }
                Some(dialog::DialogKind::Picker) => {
                    if let Some(state) = view.picker.as_mut() {
                        // (existing picker arm body, verbatim)
                    }
                }
                Some(dialog::DialogKind::Choice) => {
                    if let Some(state) = view.choice_dialog.as_mut() {
                        // (existing choice_dialog arm body, verbatim)
                    }
                }
                Some(dialog::DialogKind::AsOf) => {
                    if let Some(state) = view.as_of_dialog.as_mut() {
                        // (existing as_of_dialog arm body, verbatim)
                    }
                }
                Some(dialog::DialogKind::ScopeExpr) => {
                    if let Some(state) = view.scope_expr_dialog.as_mut() {
                        scope_expr_view::on_query_changed(state);
                    }
                }
                Some(dialog::DialogKind::Plain) | None => {}
            }
            cx.notify();
```

The `// (… verbatim)` lines stand for the existing arm bodies, moved unchanged. Delete the two old comments claiming the arms are mutually exclusive by `close_modal`'s contract.

- [ ] **Step 10: Expression suggestions follow the top kind (`expr_suggest.rs`)**

Replace `completion_mut`:

```rust
/// The live dialog's expression completion. The `dialog_input` observer calls
/// `refresh` on every notify, so a covered dialog's completion must not be
/// returned here, or another dialog's text would recompute it.
pub(crate) fn completion_mut(view: &mut ShellView) -> Option<&mut ExprCompletion> {
    match view.top_kind()? {
        DialogKind::ScopeExpr => view.scope_expr_dialog.as_mut().map(|s| &mut s.completion),
        DialogKind::Object => {
            let state = view.object_dialog.as_mut()?;
            if !super::objectdialog::expression_entry_open(state) {
                return None;
            }
            Some(state.expr.get_or_insert_with(ExprCompletion::default))
        }
        _ => None,
    }
}
```

In `values_scope`, gate the same way: wrap the existing two `if` blocks in `match view.top_kind()` with a `Some(DialogKind::ScopeExpr)` arm (first block) and a `Some(DialogKind::Object)` arm (second block), returning `None` otherwise.

Replace `deliver`:

```rust
/// An `EXPR_KEY` reply. A covered dialog's field still owns its outstanding
/// request, so the reply is offered to each live completion; the tag decides
/// which one asked. Dropped when none matches.
pub(crate) fn deliver(view: &mut ShellView, outcome: DistinctOutcome, cx: &mut Context<ShellView>) {
    let vocab = view.expr_vocab.clone();
    let mut landed = false;
    if let Some(state) = view.scope_expr_dialog.as_mut() {
        landed = state
            .completion
            .deliver(&outcome.column, outcome.tag, outcome.values.clone(), &vocab);
    }
    if !landed
        && let Some(state) = view.object_dialog.as_mut()
        && super::objectdialog::expression_entry_open(state)
        && let Some(c) = state.expr.as_mut()
    {
        landed = c.deliver(&outcome.column, outcome.tag, outcome.values, &vocab);
    }
    if landed {
        cx.notify();
    }
}
```

Add `use super::dialog::DialogKind;` to the imports. Read `ExprCompletion::deliver`'s signature in `crates/geode-shell/src/exprcomplete.rs` and match its argument types. It must return `false` without mutating when the tag is not its latest; confirm this by reading it before relying on the fallthrough.

- [ ] **Step 11: Run the new tests**

Run: `cargo test -p geode-shell dialog_stack 2>&1 | tail -30`
Expected: all five pass.

- [ ] **Step 12: Run the whole crate**

Run: `cargo test -p geode-shell 2>&1 | tail -30`
Expected: pass. A test that asserted the old "a second open is refused" rule is the only acceptable failure. For each such failure:
1. Read it and confirm it pins the single-modal refusal, not something else.
2. Rewrite it to the stack rule, or delete it if a `dialog_stack` test already covers the new rule.
3. Name it in the commit message.

Do not weaken any other failing assertion; investigate it with superpowers:systematic-debugging.

- [ ] **Step 13: Commit**

```bash
git add -A crates/geode-shell
git commit -m "feat(shell): dialogs stack; a pop reveals the dialog beneath as it was

One instance per kind; the covered entry keeps the shared input's text and
caret; sync, the Change subscriber, filter-by-mouse and expression
suggestions route by the top kind instead of the first Some field.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Dialog-opening chords reach through a dialog

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs` (add `opens_dialog`)
- Modify: `crates/geode-shell/src/shell/input.rs` (modal branch in `handle_key_down`)
- Test: `crates/geode-shell/src/shell/tests/dialog_stack.rs`

**Interfaces:**
- Consumes: Task 2's stack.
- Produces: `dialog::opens_dialog(action: &crate::actions::ActionId) -> bool` (`pub(crate)`)

- [ ] **Step 1: Write the failing tests**

Append to `tests/dialog_stack.rs`:

```rust
/// A dialog-opening chord pushes over an open dialog: `mod+t` (alt under the test
/// mod alias) opens as-of, and `ctrl+,` opens Settings.
#[gpui::test]
fn a_dialog_chord_pushes_over_an_open_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("alt-t");
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object, DialogKind::AsOf]);
    vcx.simulate_keystrokes("ctrl-,");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::AsOf, DialogKind::Settings]
    );
}

/// Any other chord stays inert behind a dialog: `ctrl+=` must not grow the font.
#[gpui::test]
fn a_non_dialog_chord_is_inert_behind_a_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    let before = shell.read_with(&vcx, |s, _| s.font_size);
    vcx.simulate_keystrokes("ctrl-=");
    assert_eq!(shell.read_with(&vcx, |s, _| s.font_size), before);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
}

/// `opens_dialog` matches dispatch in both directions. Over a stateless base modal,
/// every registered action is dispatched: a flagged action must push, and an
/// unflagged one must not.
#[gpui::test]
fn opens_dialog_matches_what_dispatch_pushes(cx: &mut gpui::TestAppContext) {
    // Flagged actions that legitimately refuse in this fixture, each with the reason.
    const REFUSES_IN_FIXTURE: &[&str] = &[];
    let (window, mut vcx) = open_shell(cx, super::picker::services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let ids: Vec<crate::actions::ActionId> =
        shell.read_with(&vcx, |s, _| s.services.registry.iter().map(|d| d.id.clone()).collect());
    let mut wrong = Vec::new();
    for id in ids {
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.close_palette(window, cx);
                s.cancel_command_line(window, cx);
                while s.modal_open() {
                    s.close_modal(window, cx);
                }
                crate::shell::dialog::open_shell_dialog(
                    s,
                    window,
                    cx,
                    DialogKind::Plain,
                    "Base",
                    |_, _, _| gpui::div().into_any_element(),
                );
                s.dispatch(&id, None, window, cx);
            });
        });
        let pushed = shell.read_with(&vcx, |s, _| s.modal_depth() > 1);
        let flagged = crate::shell::dialog::opens_dialog(&id);
        if pushed != flagged && !(flagged && REFUSES_IN_FIXTURE.contains(&id.0.as_str())) {
            wrong.push(format!("{} pushed={pushed} flagged={flagged}", id.0));
        }
    }
    assert!(wrong.is_empty(), "opens_dialog disagrees with dispatch: {wrong:#?}");
}
```

If a flagged action does not push in this fixture, read its opener. Add it to `REFUSES_IN_FIXTURE` only if it refuses for a fixture-only reason (for example, no grouping slots configured), and write that reason as a comment on the entry. Never add an action whose opener would refuse in production.

If `close_palette` or `cancel_command_line` is not visible from the test module, widen that function to `pub(super)`. The test module is a descendant of `shell`, so `pub(super)` on items in `shell::palette_ctl` / `shell::commandline` may need `pub(in crate::shell)`.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-shell dialog_stack 2>&1 | tail -30`
Expected: `a_dialog_chord_pushes_over_an_open_dialog` fails at `[Object]`. `opens_dialog_matches_what_dispatch_pushes` fails to compile, because `opens_dialog` doesn't exist.

- [ ] **Step 3: Add `opens_dialog` (`dialog.rs`)**

```rust
/// Whether dispatching `action` opens a shell dialog. With a dialog open, an
/// unclaimed chord reaches the shell only for these actions and the palette
/// toggle, so a stray chord cannot change tiles hidden behind the modal. Mirrors
/// the dialog-opening arms of `ShellView::dispatch`;
/// `opens_dialog_matches_what_dispatch_pushes` holds the two together.
pub(crate) fn opens_dialog(action: &crate::actions::ActionId) -> bool {
    matches!(
        action.0.as_str(),
        "settings::open"
            | "keybindings::open"
            | "config::views"
            | "config::groupings"
            | "config::scopes"
            | "config::schema"
            | "config::sources"
            | "config::colors"
            | "frame::pick"
            | "scope::save_current"
            | "frame::as_of"
            | "frame::scope_expression"
            | "frame::add_expression"
            | "frame::grouping"
            | "tile::add"
            | "log::level"
    ) || action.0.starts_with("frame::pick_")
}
```

- [ ] **Step 4: Chord pass-through in the modal branch (`input.rs`)**

In `handle_key_down`'s modal branch, the code after `if handled { … return; }` is currently:

```rust
                if event.keystroke.key == "escape" {
                    self.close_modal(window, cx);
                }
```

Insert before it:

```rust
                // An unclaimed chord that opens a dialog pushes it over this one,
                // resolved against the workspace context as the scope field's
                // chords are. Every other chord stays inert: an action behind
                // the modal would change tiles the trader cannot see.
                if let Some(ks) = convert_keystroke(&event.keystroke)
                    && ks.mods.is_chord()
                {
                    let stack = [KeyContext::new("workspace")];
                    let action = self
                        .single_keystroke_binding(&ks, &stack)
                        .map(|binding| binding.action.clone());
                    if let Some(action) = action
                        && dialog::opens_dialog(&action)
                    {
                        self.dispatch(&action, None, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                }
```

Update the branch's leading comment: "A shell modal gets first refusal through its key handler; a declined chord that opens a dialog pushes it; an unclaimed Escape closes the top dialog."

- [ ] **Step 5: Run the tests**

Run: `cargo test -p geode-shell dialog_stack 2>&1 | tail -30`
Expected: all pass. If `a_dialog_chord_pushes_over_an_open_dialog` fails because a dialog's own handler claims `alt-t` or `ctrl-,`, read that handler. A handler that claims every chord is a real conflict: report it rather than editing the handler.

Run: `cargo test -p geode-shell 2>&1 | tail -20`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add -A crates/geode-shell
git commit -m "feat(shell): dialog-opening chords push over an open dialog

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The palette over the stack

**Files:**
- Modify: `crates/geode-shell/src/shell/input.rs`
- Modify: `crates/geode-shell/src/shell/palette_ctl.rs`
- Modify: `crates/geode-shell/src/shell/render.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (the `overlay_return_to_filter` field doc)
- Test: `crates/geode-shell/src/shell/tests/dialog_stack.rs`

**Interfaces:**
- Consumes: `dialog::refocus_top`, `ShellView::modal_open`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/dialog_stack.rs`:

```rust
/// The palette opens above a dialog and paints above it. Escape closes only the
/// palette, and the dialog has its focus back.
#[gpui::test]
fn the_palette_opens_over_a_dialog_and_escape_returns_to_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    vcx.simulate_keystrokes("ctrl-k");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_some()));
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(vcx.debug_bounds("palette-click-catcher").is_some());

    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_none()));
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert_eq!(input_text(&shell, &mut vcx), "ab");
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// A dialog-opening palette entry pushes its dialog over the stack.
#[gpui::test]
fn a_palette_dialog_entry_pushes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("Open settings");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "settings::open"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object, DialogKind::Settings]);
}

/// A non-dialog palette action runs behind the stack. The stack stays, and the
/// top dialog keeps focus even though the action armed a tile focus restore.
#[gpui::test]
fn a_palette_action_behind_the_stack_leaves_focus_on_the_top_dialog(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    let before = shell.read_with(&vcx, |s, _| s.line_numbers);
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("line numbers");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "ui::line_numbers_cycle"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_ne!(shell.read_with(&vcx, |s, _| s.line_numbers), before, "the action ran");
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(dialog_filter_is_focused(&shell, &mut vcx));

    // A workspace action arms `pending_focus_restore`; render must not hand focus
    // to the shell root while a dialog is open.
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("Focus left");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "workspace::focus_left"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    draw(&mut vcx);
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// The first dialog was opened from the scope-bar field. A pushed dialog and a
/// palette opened and closed mid-stack must not overwrite that. The last pop
/// returns focus to the field.
#[gpui::test]
fn the_last_pop_returns_to_the_field_after_a_palette_mid_stack(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::focus_text", &mut vcx);
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_keystrokes("ctrl-,");
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_keystrokes("escape");
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::AsOf, DialogKind::Settings]);
    vcx.simulate_keystrokes("escape");
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::AsOf]);
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(!shell.read_with(&vcx, |s, _| s.modal_open()));
    assert!(filter_is_focused(&shell, &mut vcx));
}
```

If the palette titles differ from "Open settings", "line numbers" and "Focus left", check the `action(reg, …)` titles in `crates/geode-shell/src/defaults.rs` and use a query that ranks the intended action first. The `matches!` assertions guard against selecting the wrong row. If the as-of dialog's Escape clears a non-empty query before closing, the test still works, because its query is empty. The same goes for Settings in Normal mode.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-shell dialog_stack 2>&1 | tail -40`
Expected: `the_palette_opens_over_a_dialog…` fails at `palette.is_some()`, because the modal branch takes ctrl-k. The other three fail too.

- [ ] **Step 3: Route keys to a palette above the stack (`input.rs`)**

At the top of the modal branch, right after `if self.modal_open() || window.has_active_dialog(cx) {`, insert:

```rust
            // A palette opened over the stack owns the keyboard until it closes,
            // exactly as it does with no dialog open.
            if self.palette.is_some() && self.modal_open() {
                if let Some(ks) = convert_keystroke(&event.keystroke)
                    && self.is_palette_toggle(&ks, cx)
                {
                    self.toggle_palette(window, cx);
                } else {
                    self.handle_palette_key(event, window, cx);
                }
                cx.notify();
                return;
            }
```

In the chord pass-through added in Task 3, first check the palette toggle, before resolving the workspace binding:

```rust
                if let Some(ks) = convert_keystroke(&event.keystroke)
                    && ks.mods.is_chord()
                {
                    // The palette opens above the stack; it is how any action,
                    // not only a dialog, is reached while dialogs are open.
                    if self.is_palette_toggle(&ks, cx) {
                        self.toggle_palette(window, cx);
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    let stack = [KeyContext::new("workspace")];
                    // … (Task 3 body unchanged)
                }
```

- [ ] **Step 4: Stack-aware palette open, close and commit (`palette_ctl.rs`)**

In `toggle_palette`, replace

```rust
        self.overlay_return_to_filter = self.filter_field_focused(window, cx);
```

with

```rust
        // The flag belongs to the stack's base when dialogs are open: a palette
        // over them returns to the top dialog, not to the field.
        if !self.modal_open() {
            self.overlay_return_to_filter = self.filter_field_focused(window, cx);
        }
```

Replace `close_palette`'s last line `self.return_focus_from_overlay(window, cx);` with:

```rust
        if self.modal_open() {
            super::dialog::refocus_top(self, window, cx);
        } else {
            self.return_focus_from_overlay(window, cx);
        }
```

In `commit_selected`, after the `dispatch_palette_item` call inside the `if let Some(item)` block, add:

```rust
            // A non-dialog action runs behind the stack and may have moved focus
            // (a tile's own input, the shell root). The top dialog keeps it.
            if self.modal_open() {
                super::dialog::refocus_top(self, window, cx);
            }
```

Update the doc comments of `close_palette` and `commit_selected` to describe the stack case in one sentence each.

- [ ] **Step 5: Paint the palette above the modal, and skip the tile focus restore under a modal (`render.rs`)**

In `ShellView::render`'s element chain, move the whole `.when_some(modal, |el, (title, title_extra, build)| { … })` block so it comes before the palette's `.when_some(…)` block (the one that builds `palette-click-catcher`). Replace the comment "Paint modals above the palette layer and below component overlays. The modal opening path closes the palette, so these are mutually exclusive in normal operation." with:

```rust
            // Paint the modal below the palette: a palette opened over the stack
            // must be visible and take clicks above the dialog it covers.
```

Fix the which-key comment that says "the same reason as the modal-vs-palette ordering above" so it no longer claims the palette and modal are never both open.

In the `pending_focus_restore` block near the top of `render`, change

```rust
        if self.pending_focus_restore {
            self.pending_focus_restore = false;
            if !self.occupant_holds_insert_focus(window, cx) {
                self.focus_handle.focus(window, cx);
            }
        }
```

to

```rust
        if self.pending_focus_restore {
            self.pending_focus_restore = false;
            // An open dialog owns focus; a tile focus move behind it (a palette
            // action) must not pull focus to the shell root.
            if !self.modal_open() && !self.occupant_holds_insert_focus(window, cx) {
                self.focus_handle.focus(window, cx);
            }
        }
```

In `mod.rs`, update the `overlay_return_to_filter` doc. "One flag suffices because the palette and modal are mutually exclusive" becomes "One flag suffices: it belongs to the first overlay opened, and a palette or dialog opened over an open dialog neither records nor consumes it."

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-shell dialog_stack 2>&1 | tail -30`
Expected: all pass.

Run: `cargo test -p geode-shell 2>&1 | tail -20`
Expected: pass. `open_shell_dialog_closes_an_open_palette` in `tests/chrome_and_dialogs.rs` must still pass, since it opens with no dialog beneath.

- [ ] **Step 7: Commit**

```bash
git add -A crates/geode-shell
git commit -m "feat(shell): the palette opens over a dialog stack and runs any action behind it

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Mutation entries, docs, spec note, TODO

**Files:**
- Modify: `scripts/mutation-check.sh`
- Modify: `docs/current/input-and-dialogs.md`, `docs/current/shell.md`, `crates/geode-shell/README.md`
- Modify: `docs/superpowers/specs/2026-09-26-dialog-stack-design.md` (§5)
- Modify: `TODO.md` (untracked; edit in place only, never `git add` it)

- [ ] **Step 1: Re-anchor the entries this work moved**

Run: `zsh scripts/mutation-check.sh --anchors-only 2>&1 | grep -E 'ANCHOR|AMBIG|FILTER' | head -40`

Known stale anchors:
- `overlay focus: the dialog door records whether the field held focus`: the line is now indented eight spaces inside `if first {`. Update both anchor and replacement to the eight-space form.
- `objectdialog: closing the modal leaves the dialog's state behind`: the anchor becomes `            DialogKind::Object => self.object_dialog = None,` and the replacement `            DialogKind::Object => {}`. Keep the test filter.
- The two `sync_dialog_text` / enter-filter entries that anchor on `} else if let Some(state) = shell.settings.as_ref() {` / `.as_mut() {` in `dialog.rs`: re-anchor to the Settings arm's `let Some(state) = shell.settings.as_ref() else {` / `.as_mut() else {` with replacement `.as_ref().filter(|_| false) else {` / `.as_mut().filter(|_| false) else {`.
- Any other entry the script reports. Re-anchor it to the moved line with the same mutation meaning. Delete it only if the behavior it guarded no longer exists, and say which in the commit.

Re-run until the command prints nothing and exits 0.

- [ ] **Step 2: Add the new entries**

Add a section to `scripts/mutation-check.sh` near the existing `# ---- overlays return focus to the field they opened from` block. Copy each anchor exactly from the formatted source; if `cargo fmt` wrapped a line differently from below, use the formatted line.

```zsh
# ---- dialog stack: pop one level, route by the top kind, chords and palette
#
# A pop that clears every kind shows up only when a second dialog was open, and
# routing by "first Some field" shows up only when two states coexist. Neither
# single-dialog test can see these.

run_mutation "dialog stack: close clears the whole stack instead of the top" \
  crates/geode-shell/src/shell/mod.rs \
  '        if let Some(top) = self.modals.pop() {' \
  '        if let Some(top) = self.modals.drain(..).next() {' \
  geode-shell \
  a_pushed_dialog_owns_the_shared_input_until_it_pops

run_mutation "dialog stack: typed queries route to the Object state regardless of the top" \
  crates/geode-shell/src/shell/mod.rs \
  '            match view.top_kind() {' \
  '            match view.top_kind().map(|_| dialog::DialogKind::Object) {' \
  geode-shell \
  a_pushed_dialog_owns_the_shared_input_until_it_pops

run_mutation "dialog stack: sync writes the Object query whatever is on top" \
  crates/geode-shell/src/shell/dialog.rs \
  '    let (mode, listening, query) = match shell.top_kind() {' \
  '    let (mode, listening, query) = match shell.top_kind().map(|_| DialogKind::Object) {' \
  geode-shell \
  a_pushed_dialog_owns_the_shared_input_until_it_pops

run_mutation "dialog stack: a pop does not restore the covered input" \
  crates/geode-shell/src/shell/dialog.rs \
  '    if let Some(saved) = top.saved_input.take() {' \
  '    if let Some(saved) = top.saved_input.take().filter(|_| false) {' \
  geode-shell \
  a_covered_expression_dialog_gets_its_typed_text_back

run_mutation "dialog stack: a covered expression field refreshes from the top dialog's text" \
  crates/geode-shell/src/shell/expr_suggest.rs \
  '        _ => None,' \
  '        _ => view.scope_expr_dialog.as_mut().map(|s| &mut s.completion),' \
  geode-shell \
  a_covered_expression_dialog_gets_its_typed_text_back

run_mutation "dialog stack: a kind already in the stack is pushed again" \
  crates/geode-shell/src/shell/dialog.rs \
  '    let Some(at) = view.modals.iter().position(|m| m.kind == kind) else {' \
  '    let Some(at) = view.modals.iter().position(|_| false) else {' \
  geode-shell \
  a_kind_already_in_the_stack_is_refused

run_mutation "dialog stack: a nested push overwrites the base's return-to-field flag" \
  crates/geode-shell/src/shell/dialog.rs \
  '    let first = view.modals.is_empty();' \
  '    let first = true;' \
  geode-shell \
  the_last_pop_returns_to_the_field_after_a_palette_mid_stack

run_mutation "dialog stack: a palette over the stack records the return-to-field flag" \
  crates/geode-shell/src/shell/palette_ctl.rs \
  '        if !self.modal_open() {' \
  '        if true {' \
  geode-shell \
  the_last_pop_returns_to_the_field_after_a_palette_mid_stack

run_mutation "dialog stack: every chord reaches through a dialog" \
  crates/geode-shell/src/shell/input.rs \
  '                        && dialog::opens_dialog(&action)' \
  '                        && true' \
  geode-shell \
  a_non_dialog_chord_is_inert_behind_a_dialog

run_mutation "dialog stack: render hands focus to the root under a dialog" \
  crates/geode-shell/src/shell/render.rs \
  '            if !self.modal_open() && !self.occupant_holds_insert_focus(window, cx) {' \
  '            if !self.occupant_holds_insert_focus(window, cx) {' \
  geode-shell \
  a_palette_action_behind_the_stack_leaves_focus_on_the_top_dialog
```

Check the `expr_suggest.rs` entry: `        _ => None,` must match exactly once in that file (`--anchors-only` reports AMBIG otherwise). If it is ambiguous, pick the `completion_mut` arm by a distinctive neighbor, for example by giving that arm a comment line and anchoring there.

Run: `zsh scripts/mutation-check.sh --anchors-only 2>&1 | tail -5`
Expected: exit 0, no ANCHOR/AMBIG/FILTER lines.

- [ ] **Step 3: Commit, then run the new entries**

```bash
git add scripts/mutation-check.sh
git commit -m "test(shell): mutation entries for the dialog stack

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
zsh scripts/mutation-check.sh "dialog stack"
```

Expected: every entry reports `caught` by its named test, not `caught*` and not `SURVIVED`. For a survivor, strengthen the named test until it catches the mutation, then commit the test. Then run `zsh scripts/mutation-check.sh --changed` detached (see the Mutation harness cost note: run it in the background and wait for completion). Every entry for the changed files must be caught. Re-anchored entries that now survive mean the move changed meaning: fix them.

- [ ] **Step 4: Update the current guides and README**

`docs/current/input-and-dialogs.md`:
- In the keyboard-ownership table row "Shell modal or component dialog", say that a shell modal's handler gets first refusal. A declined chord bound to a dialog-opening action (or the palette toggle) is dispatched, other chords are inert, and unclaimed Escape closes the top dialog.
- Rewrite "## Modal lifetime and focus" to state:
  - dialogs form a stack and only the top paints and takes keys;
  - opening a kind already on top does nothing, and one lower down posts "… is already open underneath";
  - closing (Enter commit, Escape, close button, backdrop click) pops one level and clears only that kind's state;
  - the covered dialog's shared-input text and caret are restored on reveal, and its focus comes from its state (mode dialogs) or goes to the input (filter-only dialogs);
  - the scope-field return flag belongs to the base dialog;
  - a covered dialog's async deliveries still apply.
- In the palette section, state that the palette opens over a dialog stack, lists every action, pushes dialog actions, runs others behind the stack, and returns focus to the top dialog on close.
- Record the known limitation: one instance per kind, and the three `choicedialog` pickers count as one kind.

`docs/current/shell.md`: wherever it states that one modal is open at a time or that the palette and modal are mutually exclusive, correct it with a pointer to input-and-dialogs.md.

`crates/geode-shell/README.md`: in the `dialog.rs` module note, say that it owns the modal stack (`DialogKind`, `can_open`, `refocus_top`, `opens_dialog`).

Run: `grep -rn -i 'one modal\|single modal\|mutually exclusive' docs/current crates/geode-shell/README.md crates/geode-shell/src/shell/*.rs | grep -i 'modal\|palette\|dialog'`
Expected: each remaining hit is still true under the stack, or has been corrected.

- [ ] **Step 5: Spec §5 note and TODO**

In `docs/superpowers/specs/2026-09-26-dialog-stack-design.md` §5, replace the `ActionDef gains opens_dialog: bool …` paragraph with:

```markdown
`dialog::opens_dialog(&ActionId) -> bool` names the actions whose dispatch
opens a shell dialog. Only `ShellView::dispatch` opens shell dialogs, so the
list lives beside that dispatch table rather than on `ActionDef`.
`opens_dialog_matches_what_dispatch_pushes` dispatches every registered action
over a base modal and requires flagged ⇔ pushed.
```

In `TODO.md`, delete the line `* ALlow dialogs on top of dialogs (a stack)`. `TODO.md` is untracked and Matthew's own file: edit it, but do not stage it.

- [ ] **Step 6: Full verification**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace 2>&1 | tail -20
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```

Expected: all clean, all pass, anchors exit 0.

- [ ] **Step 7: Commit**

```bash
git add docs/current/input-and-dialogs.md docs/current/shell.md crates/geode-shell/README.md docs/superpowers/specs/2026-09-26-dialog-stack-design.md
git commit -m "docs: dialogs stack; palette over the stack; opens_dialog lives beside dispatch

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Display checks (for Matthew, after merge)

1. Open Views and type a filter. Press `mod+t`: as-of paints alone, without Views showing through. Escape brings back Views with the filter and caret.
2. With a dialog open, press `ctrl+k`: the palette paints above the dialog, and a click outside the palette closes only the palette.
3. With two dialogs stacked, the backdrop dims once. It should not be darker than for one dialog, because only the top entry paints a backdrop.
