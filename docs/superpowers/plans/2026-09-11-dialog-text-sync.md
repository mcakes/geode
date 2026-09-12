# Dialog Text Sync — One Owner for Mode, Focus and Text

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the thirty hand-assembled mode/focus/text transitions in the keybinding and object dialogs with one reconcile function that makes gpui match the pure state after every mutation, so the "one switch" can no longer be half-thrown.

**Architecture:** The pure cores keep `mode` and `query` (`KeybindingsState`, `ObjectDialogState`, `Draft`). A pure `dialogmode::focus_target(mode, listening) -> FocusTarget` decides who holds the keyboard. `dialog::sync_dialog_text(shell, window, cx)` applies it and writes the shared `Input`'s text from the effective query when they differ. It runs at three seams only: the tail of the modal branch in `ShellView::handle_key_down`, the tail of each row-click handler, and `open_shell_dialog_with_key`. Every transition site becomes a pure mutation.

**Tech Stack:** Rust, gpui + gpui-component (pinned rev `0e2fb7a`), `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` — **§16 is this plan's brief** (§16.1 the rule, §16.2 the effective query, §16.3 the pure decision, §16.4 untouched, §16.5 tests). §2, §5 and §15 are the background; 4c spec §18.6 records the one-way mirror ruling §16.2 turns into a getter.

## Global Constraints

- CI on macOS and Windows: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. Run all five before every commit that touches Rust.
- **TDD**: failing test first, watched to fail for the right reason, RED/GREEN pasted in the report.
- **A mutation entry for every behaviour changed**, in `scripts/mutation-check.sh`, anchor unique as a substring of its file (`grep -c -F` prints 1; multi-line anchors are fine), naming its covering test as the 6th argument and confirmed `caught` by running that entry. `zsh scripts/mutation-check.sh --anchors-only` exits 0 before every commit. **This refactor deletes lines that existing entries anchor on** — at least `objectdialog: n empties a leftover browse filter before naming` and `keybindings: leaving filter mode clears the query`; each must be re-anchored to the line that now carries that behaviour (a sync rule), never deleted, and must still be caught by its named test.
- **Behaviour is unchanged.** Every existing window test in `shell/tests/objectdialog.rs` and `shell/tests/keybindings_dialog.rs` passes unmodified. A test that needs changing is a sign the refactor changed behaviour — stop and report.
- A doc comment that contradicts the code is a defect. Many comments explain the hand-written pairing ("The `Input` owns the text; clearing only the mirrored copy would leave…", "The one switch, thrown the other way…"); each one whose code goes away must go with it or be rewritten to point at the sync.
- Never a raw colour. `geode-shell` never depends on `geode-data`. Nothing stalls the render thread: the sync does a string compare and at most one `set_value` per keystroke.
- No `Entity<ShellView>::read` inside render closures (`ShellModal::build`'s doc).
- **Filter-only dialogs are untouched** (§16.4): settings, picker, as-of keep their own open-time focus and are not routed through the sync.
- An in-flight worktree `space-advances-cursor` (another session) edits `objectdialog/render.rs`'s `Toggle` arm. Base on `main`; do not touch that arm's cursor logic beyond removing focus/text calls.

## As-built vocabulary this plan builds on

```rust
// crates/geode-shell/src/dialogmode.rs — pure, no gpui
pub enum DialogMode { Normal, Filter }
pub fn escape_step(mode, query_is_empty, has_previous_stage) -> EscapeStep
// crates/geode-shell/src/shell/dialog.rs
pub struct ShellModal { title, title_extra, build, on_key }
pub fn open_shell_dialog_with_key(view, window, cx, title, build, on_key, focus_filter: bool)
//   … `if focus_filter { dialog_input.focus_handle(cx).focus(window, cx) }` at ~line 464
pub type ModalKeyHandler = Rc<dyn Fn(&mut ShellView, &Keystroke, &mut Window, &mut Context<ShellView>) -> bool>
// crates/geode-shell/src/shell/input.rs ~505-532: the modal branch of handle_key_down —
//   `handler(self, &ks, window, cx)`; claimed → stop_propagation + notify + return;
//   unclaimed escape → close_modal; unclaimed other → propagate to the focused Input.
// crates/geode-shell/src/shell/mod.rs ~933: dialog_input's InputEvent::Change subscription —
//   routes `input.value()` to the open dialog's `set_query` (object dialog, keybindings, settings) etc.
// KeybindingsState { mode, query, selected, listening: Option<Vec<Keystroke>>, notice, .. }
//   set_query(query) — also clears `listening`
// ObjectDialogState { domain, stage: Browse|Naming|Edit{object}, selected, query, mode, notice, draft: Option<Draft> }
//   set_query(query) — writes draft.query in Stage::Edit, self.query otherwise (4c §18.6 one-way mirror)
// Draft { .. query: String, selected .. }
// Transition sites today (hand-assembled): objectdialog/render.rs lines ~313, 341-344, 368-369, 389-390,
//   452-453, 571-575, 650-652, 743-745, 796, 907-909, 1181, 2478-2482;
//   keybindings_view.rs lines ~686-699, 748, 768-771, 808-809, 926-929, and open() ~541-556.
```

---

## File map

| File | Responsibility after this plan |
|---|---|
| `crates/geode-shell/src/dialogmode.rs` | + `FocusTarget`, `focus_target(mode, listening)` (T1) |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | + `ObjectDialogState::effective_query()`; `set_query` doc says it is the write half (T1) |
| `crates/geode-shell/src/shell/dialog.rs` | + `sync_dialog_text`; `open_shell_dialog_with_key` ends in the sync (T2) |
| `crates/geode-shell/src/shell/input.rs` | modal branch calls the sync after the handler, claimed or not (T2) |
| `crates/geode-shell/src/shell/keybindings_view.rs` | every focus/`set_value` call removed; row click ends in the sync (T2) |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | every focus/`set_value` call removed; both row clicks end in the sync (T3) |
| `scripts/mutation-check.sh` | new entries per sync rule; orphaned entries re-anchored (T1–T3) |
| spec §16.6, `CLAUDE.md` | as-built (T4) |

---

### Task 1: The pure decision and the effective query

**Files:**
- Modify: `crates/geode-shell/src/dialogmode.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`impl ObjectDialogState`, near `set_query` ~line 1589)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `pub enum FocusTarget { Input, Shell }`; `pub fn focus_target(mode: DialogMode, listening: bool) -> FocusTarget`; `ObjectDialogState::effective_query(&self) -> &str`.

- [ ] **Step 1: Failing pure tests** in `dialogmode.rs`'s existing test module:

```rust
#[test]
fn focus_follows_the_mode_unless_a_capture_is_listening() {
    assert_eq!(focus_target(DialogMode::Filter, false), FocusTarget::Input);
    assert_eq!(focus_target(DialogMode::Normal, false), FocusTarget::Shell);
    // A capture reads raw keystrokes off the shell root whatever the mode says.
    assert_eq!(focus_target(DialogMode::Filter, true), FocusTarget::Shell);
    assert_eq!(focus_target(DialogMode::Normal, true), FocusTarget::Shell);
}
```

and in `objectdialog/mod.rs`'s tests (it has `config_from`):

```rust
#[test]
fn the_effective_query_is_the_stages_own() {
    let config = config_from(&[(Layer::Desk, "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n")]);
    let mut state = ObjectDialogState::new(Domain::Views);
    state.set_query("br".to_string());
    assert_eq!(state.effective_query(), "br");
    state.enter_edit(&config, "tree");            // pub(in objectdialog) — this test is inside the module tree
    assert_eq!(state.effective_query(), "", "entering the edit stage starts with no filter");
    state.set_query("np".to_string());
    assert_eq!(state.effective_query(), "np");
    assert_eq!(state.query, "", "the browse query is untouched by an edit-stage keystroke");
    state.leave_edit();
    assert_eq!(state.effective_query(), "");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell focus_follows_the_mode && cargo test -p geode-shell the_effective_query_is_the_stages_own`
Expected: compile errors — `FocusTarget`, `focus_target`, `effective_query` missing.

- [ ] **Step 3: Implement** in `dialogmode.rs`, beside `escape_step`:

```rust
/// Who holds the keyboard while a modal dialog is open (spec §16.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    /// The shared filter `Input`: printable keys are text.
    Input,
    /// The shell root: printable keys reach the dialog's `on_key` as verbs
    /// (normal mode) or as raw captured keystrokes (listening).
    Shell,
}

/// The one decision `dialog::sync_dialog_text` applies. `listening` is the
/// keybinding dialog's capture state and wins over the mode: a capture
/// must see every keystroke raw, and a focused `Input` would eat the
/// printable ones as text before the dialog's handler ran.
pub fn focus_target(mode: DialogMode, listening: bool) -> FocusTarget {
    if listening {
        return FocusTarget::Shell;
    }
    match mode {
        DialogMode::Filter => FocusTarget::Input,
        DialogMode::Normal => FocusTarget::Shell,
    }
}
```

In `objectdialog/mod.rs`:

```rust
    /// The query the open stage is filtering by — the draft's in
    /// `Stage::Edit`, the state's own otherwise (spec §16.2). The read
    /// half of [`ObjectDialogState::set_query`]'s one-way mirror: the
    /// sync writes the shared `Input` from this, so a query left in the
    /// other stage's slot can never reach the screen.
    pub fn effective_query(&self) -> &str {
        match (&self.stage, self.draft.as_ref()) {
            (Stage::Edit { .. }, Some(draft)) => draft.query.as_str(),
            _ => self.query.as_str(),
        }
    }
```

Rewrite `set_query`'s doc to say it is the write half and that `effective_query` is the read half.

- [ ] **Step 4: Run tests** — `cargo test -p geode-shell dialogmode && cargo test -p geode-shell objectdialog` → PASS.

- [ ] **Step 5: Mutation entries**

```sh
# §16.3: a capture must read raw keystrokes off the shell root. Dropping the
# `listening` override focuses the Input mid-capture and the captured `a`
# becomes text — the keybinding dialog's original modal defect.
run_mutation "dialogmode: listening overrides the mode for focus" \
  crates/geode-shell/src/dialogmode.rs \
  '    if listening {
        return FocusTarget::Shell;
    }' \
  '    if false {
        return FocusTarget::Shell;
    }' \
  geode-shell \
  focus_follows_the_mode_unless_a_capture_is_listening

# §16.2: the read half of the one-way mirror. Reading self.query in the edit
# stage paints (and, after Task 3, WRITES into the Input) the browse query.
run_mutation "objectdialog: effective_query reads the edit stage's draft" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            (Stage::Edit { .. }, Some(draft)) => draft.query.as_str(),' \
  '            (Stage::Edit { .. }, Some(_draft)) => self.query.as_str(),' \
  geode-shell \
  the_effective_query_is_the_stages_own
```

- [ ] **Step 6: Checks and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support && zsh scripts/mutation-check.sh --anchors-only
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): focus_target and effective_query — the pure half of the dialog text sync (§16)"
```

---

### Task 2: `sync_dialog_text`, its three seams, and the keybinding dialog converted

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs` (+ `sync_dialog_text`; `open_shell_dialog_with_key`)
- Modify: `crates/geode-shell/src/shell/input.rs` (~505-532, the modal branch)
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs` (sites ~686-699, 748, 768-771, 808-809, 926-929; `open`)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `focus_target`, `FocusTarget` (T1); `ObjectDialogState::effective_query` (T1).
- Produces: `pub(crate) fn sync_dialog_text(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>)`.

- [ ] **Step 1: Failing window test** in `shell/tests/keybindings_dialog.rs` (beside the existing capture tests; use their opening helper):

```rust
/// §16.1: the sync, not the transition site, owns focus and text. Enter
/// filter mode, type, leave it, clear the query, start a capture, cancel
/// it — and after every step the focused surface and the Input's text
/// are what the pure state says, with no site in `keybindings_view`
/// touching either directly (Task 2 deletes them all; this test is what
/// proves the sync reproduces them).
#[gpui::test]
fn focus_and_text_follow_the_pure_state_through_every_transition(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    assert!(!dialog_filter_is_focused(&shell, &cx), "opens in normal mode: shell root holds the keys");
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(dialog_filter_is_focused(&shell, &cx));
    cx.simulate_input("pal");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");           // leave filter, keep the query
    cx.run_until_parked();
    assert!(!dialog_filter_is_focused(&shell, &cx));
    let text = shell.read_with(&cx, |shell, cx| shell.dialog_input.read(cx).value().to_string());
    assert_eq!(text, "pal", "leaving filter mode keeps the query in the field");
    cx.simulate_keystrokes("escape");           // clear the query
    cx.run_until_parked();
    let text = shell.read_with(&cx, |shell, cx| shell.dialog_input.read(cx).value().to_string());
    assert_eq!(text, "", "clearing the query empties the field through the sync");
    cx.simulate_keystrokes("/");
    cx.simulate_input("pal");
    cx.simulate_keystrokes("enter");            // begin a capture from filter mode
    cx.run_until_parked();
    assert!(!dialog_filter_is_focused(&shell, &cx), "listening: the shell root reads raw keys");
    cx.simulate_keystrokes("escape");           // cancel the capture
    cx.run_until_parked();
    assert!(dialog_filter_is_focused(&shell, &cx), "back to the mode underneath: filter");
}
```

(Check how the existing capture tests start listening — `enter` on a row in normal mode; from filter mode the same `enter` must start it. Adapt the keystrokes to what `handle_key` does today; do not change behaviour.)

- [ ] **Step 2: Run to verify** — it likely PASSES today (the hand-written sites do this). Record that: this test is a regression net for the deletion below; the RED is produced in Step 4 by deleting the sites before adding the sync call, and the GREEN by adding it.

- [ ] **Step 3: Implement the sync** in `dialog.rs`:

```rust
/// Make gpui agree with the open modal dialog's pure state (spec §16.1):
/// focus goes where [`dialogmode::focus_target`] says, and the shared
/// `Input` holds the dialog's effective query.
///
/// Called at three seams and nowhere else — the tail of the modal branch
/// in `ShellView::handle_key_down`, the tail of each row-click handler,
/// and [`open_shell_dialog_with_key`] — so a transition site is a pure
/// mutation and cannot forget the gpui half. A no-op when no modal
/// dialog with a mode is open; the filter-only dialogs (settings, picker,
/// as-of) keep their own open-time focus (§16.4).
///
/// The text write is guarded by a compare because `InputState::set_value`
/// emits no `InputEvent::Change`: writing unconditionally would be
/// harmless for the mirror but would move the caret on every keystroke.
/// Focusing an already-focused handle is idempotent.
pub(crate) fn sync_dialog_text(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let (mode, listening, query) = if let Some(state) = shell.keybindings.as_ref() {
        (state.mode, state.listening.is_some(), state.query.clone())
    } else if let Some(state) = shell.object_dialog.as_ref() {
        (state.mode, false, state.effective_query().to_string())
    } else {
        return;
    };
    let input = shell.dialog_input.clone();
    if input.read(cx).value().as_ref() != query.as_str() {
        input.update(cx, |i, cx| i.set_value(query, window, cx));
    }
    match dialogmode::focus_target(mode, listening) {
        FocusTarget::Input => input.read(cx).focus_handle(cx).focus(window, cx),
        FocusTarget::Shell => shell.focus_handle.focus(window, cx),
    }
}
```

(`value()` returns a `SharedString`-like; adjust the comparison to whatever it is at the pinned rev. The `query.clone()` is one small allocation per keystroke on a modal dialog — acceptable; if `value()` can be compared without cloning `query`, prefer that.)

In `open_shell_dialog_with_key`: the `if focus_filter { … focus … }` block becomes: the filter-only dialogs still need their focus (they have no mode) — keep `if focus_filter { focus the input }` for them, and ADD an unconditional `sync_dialog_text(view, window, cx)` at the end, which is a no-op for them and sets the initial focus for the modal ones (their state is `Some` by then — both `open` functions set `view.keybindings`/`view.object_dialog` before calling this door). Rewrite `focus_filter`'s doc: "for a dialog without a mode; a modal dialog's initial focus comes from the sync".

In `input.rs`'s modal branch: after `let handled = …;`, and regardless of `handled`, call `dialog::sync_dialog_text(self, window, cx)` — but only while `self.modal.is_some()` (an unclaimed `escape` closes the modal and `close_modal` owns focus after that). Order: sync BEFORE `cx.stop_propagation()`/`return` on the claimed path, and before the `escape` close on the unclaimed path:

```rust
                let handled = …;
                if self.modal.is_some() {
                    dialog::sync_dialog_text(self, window, cx);
                }
                if handled { cx.stop_propagation(); cx.notify(); return; }
                if event.keystroke.key == "escape" { self.close_modal(window, cx); }
```

- [ ] **Step 4: Convert the keybinding dialog.** Delete every `input.read(cx).focus_handle(cx).focus(window, cx)`, `shell.focus_handle.focus(window, cx)` and `input.update(cx, |i, cx| i.set_value("", window, cx))` in `keybindings_view.rs` (`press_while_listening`'s Cancel/Commit arms, the `ClearQuery` rung, `EnterFilter`, the filter-mode `escape`, `on_row_clicked`), leaving the pure mutations (`state.listening = None`, `state.mode = …`, `state.query.clear()`). `on_row_clicked` ends with `dialog::sync_dialog_text(shell, window, cx)` before its `cx.notify()`. Delete or rewrite every comment that described the deleted pairing. Run the Step 1 test after the deletions but BEFORE the `input.rs` sync call is wired to capture the RED; then wire it and capture the GREEN. Run the whole keybindings test file: every existing test passes unmodified.

- [ ] **Step 5: Mutation entries and re-anchoring**

```sh
# §16.1, focus rule: the sync is the only thing that moves focus now.
run_mutation "dialog: the sync focuses the Input in filter mode" \
  crates/geode-shell/src/shell/dialog.rs \
  '        FocusTarget::Input => input.read(cx).focus_handle(cx).focus(window, cx),' \
  '        FocusTarget::Input => shell.focus_handle.focus(window, cx),' \
  geode-shell \
  focus_and_text_follow_the_pure_state_through_every_transition

# §16.1, text rule: the Input is written from the effective query.
run_mutation "dialog: the sync writes the Input from the query" \
  crates/geode-shell/src/shell/dialog.rs \
  '        input.update(cx, |i, cx| i.set_value(query, window, cx));' \
  '        let _ = query;' \
  geode-shell \
  focus_and_text_follow_the_pure_state_through_every_transition
```

Re-anchor `keybindings: leaving filter mode clears the query` (its anchor line is deleted) onto the pure `state.query.clear()` in the `ClearQuery` rung, mutated to a no-op, and confirm its named test still catches it. Run `--anchors-only`; fix every `ANCHOR` line in `keybindings_view.rs`.

- [ ] **Step 6: Checks and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support && zsh scripts/mutation-check.sh --anchors-only
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): sync_dialog_text at the three seams; keybinding dialog converted (§16)"
```

---

### Task 3: The object dialog converted

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (sites ~313, 341-344, 368-369, 389-390, 452-453, 571-575, 650-652, 743-745, 796, 907-909, 1181, 2478-2482; `open`)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (doc comments on `enter_edit`, `begin_naming`, `cancel_naming`, `leave_edit` that describe the deleted pairing)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `dialog::sync_dialog_text` (T2), `effective_query` (T1).

- [ ] **Step 1: The regression net is the existing suite.** These four window tests already cover every one-switch defect the branch found and must pass unmodified at the end: `an_object_opened_from_filter_mode_still_escapes_back_a_stage`, `n_opens_an_empty_name_field_even_after_a_browse_filter`, `slash_filters_the_edit_stage_and_escape_walks_the_full_ladder`, `clicking_an_edit_row_while_filtering_keeps_the_filter_focused`. Run them first and record GREEN as the baseline.

- [ ] **Step 2: Delete the sites, capture RED.** Remove every focus and `set_value("")` call in `render.rs` (twelve sites — grep `focus_handle` and `set_value(` in the file until only the sync's own calls remain, which live in `dialog.rs`, so the count in `render.rs` reaches zero). Leave the pure mutations: `state.mode = …`, `state.query.clear()` + `state.selected = 0` (the `ClearQuery` rungs), `begin_naming()`, `cancel_naming()`, `enter_edit`/`enter_edit_with` (both already clear the draft/query), `leave_edit()`. The two `ClearQuery` rungs in `handle_browse_key` and `handle_edit_key` must clear the PURE query (`state.query.clear()` / `draft.query.clear()`) — check each still does after the `set_value` line is gone. `on_row_clicked` and `on_edit_row_clicked` end with `dialog::sync_dialog_text(shell, window, cx)` before `cx.notify()`. `leave_edit` and `enter_edit_stage` need no call: they are reached only from key handlers or `run_confirmed`, whose tail the modal branch syncs — verify `run_confirmed` is only reached from `handle_edit_key` or the confirm buttons; the confirm buttons' `on_click` closures call `run_confirmed` directly, so add the sync at the end of both button closures (`confirm_row`'s yes and cancel), the same way the row clicks do. Run the four tests: expect RED on the ones whose transition lost its focus/text (the naming and edit-filter ones at least). Then run the whole object-dialog test file: every existing test passes; if one does not, the sync is missing a seam — find it, do not edit the test.

- [ ] **Step 3: Comments.** Rewrite `render.rs`'s module doc "The one switch: normal mode is a blurred filter" paragraph (the mechanism is now the sync), `enter_edit_stage`'s "one door" doc (its second half — emptying the Input and moving focus — is now the sync's), `mod.rs`'s `enter_edit`/`begin_naming`/`cancel_naming` docs ("the blur needs a `Window`, which this file may not name…" is still true, but the pairing they describe lives in one place now), and every deleted-site comment. `grep -n 'Input owns the text\|one switch\|thrown the other way' crates/geode-shell/src/shell/objectdialog/` must return only sentences that are true.

- [ ] **Step 4: Re-anchor.** `objectdialog: n empties a leftover browse filter before naming` anchored on the deleted `set_value` + focus pair: re-anchor onto `begin_naming`'s `self.query.clear();` in `mod.rs` (mutate to a no-op); its test `n_opens_an_empty_name_field_even_after_a_browse_filter` must still catch it (the sync would then write the stale query back into the field). Run `--anchors-only`; fix every `ANCHOR` in `render.rs`/`mod.rs`. Add one entry for the confirm-button sync if a test can see it (a click on `objectdialog-confirm-yes` from filter mode, asserting focus after) — write that window test; otherwise say so in the report.

- [ ] **Step 5: Checks and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support && zsh scripts/mutation-check.sh --anchors-only
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "refactor(config): object dialog transitions are pure; the sync owns focus and text (§16)"
```

---

### Task 4: As-built, CLAUDE.md, harness

- [ ] **Step 1:** `zsh scripts/mutation-check.sh --changed=main` detached; every entry `caught`; tree clean after.
- [ ] **Step 2:** Spec §16.6 "As built": what shipped, the seams the sync actually runs at (including the confirm buttons if Task 3 added them), which entries were re-anchored and onto what, anything that deviated. One paragraph in 4c spec §18.6 noting the one-way mirror is now `effective_query`.
- [ ] **Step 3:** CLAUDE.md: in the dialogs paragraph ("Dialogs now have two interaction modes…"), add two sentences: pure state is the truth; `dialog::sync_dialog_text` at three seams is the only thing that moves focus or writes the shared Input; a new transition site is a pure mutation and must not call `focus` or `set_value` itself.
- [ ] **Step 4:** The five CI checks; commit `docs: dialog text sync as built (§16.6)`.

---

## Self-review

- **Spec coverage:** §16.1 → T2 (sync + seams), T3 (object dialog sites). §16.2 → T1 (`effective_query`) and T2 (the sync reads it). §16.3 → T1. §16.4 → T2 (filter-only dialogs keep `focus_filter`). §16.5 → T1–T3 entries, T3's regression net, T4's run.
- **Placeholders:** the T2 window test's capture keystrokes are marked "adapt to what `handle_key` does today" because the exact key that begins a capture from filter mode must be read off the code; everything else is concrete.
- **Type consistency:** `sync_dialog_text(shell, window, cx)` is the one signature used in T2 and T3; `focus_target(mode, listening)` in T1 and T2; `effective_query() -> &str` in T1, T2.
- **Behaviour-preservation gate:** T3 step 2 forbids editing existing tests; T2 step 4 likewise.
