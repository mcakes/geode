# Dialog Interaction Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Geode's dialogs a normal mode so letters can be verbs, and prove it by giving the keybinding dialog the unbind and reset it has never had.

**Architecture:** A pure `dialogmode` core (no gpui) supplies the mode enum, the escape ladder and the normal-mode key vocabulary, in the mould of `listfilter` and `vimnav`. The keybinding dialog adopts it by generalising a focus switch it already performs — while capturing a rebind it blurs `dialog_input` to `shell.focus_handle` so raw keystrokes reach its handler, which is exactly what normal mode is. `keymap_edit` gains an unbind writer alongside `apply_rebind`, reusing the same shadow-versus-remove branch.

**Tech Stack:** Rust, gpui + gpui-component (pinned), `toml_edit`, criterion, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`

## Global Constraints

- Every new lib/bin target needs `bench = false`; `[[bench]]` targets need `harness = false`. No new targets are expected here.
- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. All five must pass before any task is considered done.
- **TDD**: write the failing test, run it, watch it fail for the right reason, then implement.
- **Add a mutation entry for every behaviour changed** (`scripts/mutation-check.sh`). Run `zsh scripts/mutation-check.sh --changed` after every task. **Commit before mutating** — the harness restores files with `git checkout`, which discards uncommitted work.
- No crate other than `geode-data` may open a file or socket. `geode-shell` never depends on `geode-data`.
- Nothing may stall the render thread; file writes go on the background executor.
- Modals open only through `shell::dialog::open_shell_dialog` / `open_shell_dialog_with_key`.
- Pure cores carry the test weight: no gpui types in `dialogmode.rs`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-shell/src/dialogmode.rs` | **NEW.** Pure core: `DialogMode`, `EscapeStep`, `escape_step`, `NormalCommand`, `normal_command`. No gpui. |
| `crates/geode-shell/src/lib.rs` | `pub mod dialogmode;` |
| `crates/geode-shell/src/keymap_edit.rs` | **+** `Unbind`, `UnbindOutcome`, `apply_unbind` beside the existing `Rebind`/`apply_rebind`. |
| `crates/geode-shell/src/shell/keybindings_view.rs` | `KeybindingsState.mode`; `handle_key` routes by mode; mode pill + footer hint in `build`; `d` and `r` verbs. |
| `crates/geode-shell/src/shell/dialog.rs` | `render_modal` gains an optional mode pill in the title row. |
| `crates/geode-shell/src/shell/tests/keybindings_dialog.rs` | Real-key-dispatch tests for mode switching, the ladder, and the two new verbs. |
| `scripts/mutation-check.sh` | New entries; re-anchor any the migration breaks. |
| `CLAUDE.md` | Record the model and which surfaces are modal. |

---

### Task 1: The pure mode core

**Files:**
- Create: `crates/geode-shell/src/dialogmode.rs`
- Modify: `crates/geode-shell/src/lib.rs`

**Interfaces:**
- Consumes: `crate::keymap::{Keystroke, Modifiers}`, `crate::listfilter::nav_command`, `crate::vimnav::NavCommand`.
- Produces: `DialogMode::{Normal, Filter}`; `EscapeStep::{LeaveFilter, ClearQuery, PreviousStage, Close}`; `escape_step(DialogMode, bool, bool) -> EscapeStep`; `NormalCommand::{Nav(NavCommand), EnterFilter, Commit, Toggle, MoveItem(i32), EditText, Verb(char)}`; `normal_command(&Keystroke) -> Option<NormalCommand>`.

- [ ] **Step 1: Check `NavCommand` derives what this needs**

`crates/geode-shell/src/vimnav.rs:67` defines `NavCommand { Move(i64), Top, Bottom }`. `NormalCommand` wraps it and derives `Debug, Clone, Copy, PartialEq, Eq`, so `NavCommand` must derive all five. Read the derive line above `pub enum NavCommand` and add any that are missing. Do not change its variants.

- [ ] **Step 2: Write the failing tests**

Create `crates/geode-shell/src/dialogmode.rs` with only the test module, so the file fails to compile against absent items — that is the failure we want:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keystroke, Modifiers};
    use crate::vimnav::NavCommand;

    fn ks(key: &str, mods: Modifiers) -> Keystroke {
        Keystroke { mods, key: key.to_string() }
    }
    fn bare(key: &str) -> Keystroke { ks(key, Modifiers::NONE) }

    /// The ladder of spec §5: every rung changes something visible, so a
    /// dialog never eats an `escape` that appears to do nothing.
    #[test]
    fn the_escape_ladder_takes_the_first_step_that_applies() {
        use EscapeStep::*;
        // Filter mode always leaves filter first, whatever else is true.
        assert_eq!(escape_step(DialogMode::Filter, true, true), LeaveFilter);
        assert_eq!(escape_step(DialogMode::Filter, false, false), LeaveFilter);
        // Then a non-empty query clears, before any stage is left.
        assert_eq!(escape_step(DialogMode::Normal, false, true), ClearQuery);
        // Then a nested stage.
        assert_eq!(escape_step(DialogMode::Normal, true, true), PreviousStage);
        // Then, and only then, the modal closes.
        assert_eq!(escape_step(DialogMode::Normal, true, false), Close);
    }

    #[test]
    fn normal_mode_maps_the_shared_vocabulary() {
        use NormalCommand::*;
        assert_eq!(normal_command(&bare("j")), Some(Nav(NavCommand::Move(1))));
        assert_eq!(normal_command(&bare("k")), Some(Nav(NavCommand::Move(-1))));
        assert_eq!(normal_command(&bare("g")), Some(Nav(NavCommand::Top)));
        assert_eq!(normal_command(&ks("g", Modifiers::SHIFT)), Some(Nav(NavCommand::Bottom)));
        assert_eq!(normal_command(&bare("/")), Some(EnterFilter));
        assert_eq!(normal_command(&bare("enter")), Some(Commit));
        assert_eq!(normal_command(&bare("space")), Some(Toggle));
        assert_eq!(normal_command(&bare("i")), Some(EditText));
        assert_eq!(normal_command(&ks("j", Modifiers::SHIFT)), Some(MoveItem(1)));
        assert_eq!(normal_command(&ks("k", Modifiers::SHIFT)), Some(MoveItem(-1)));
    }

    /// Arrows and the ctrl-steps keep working in normal mode: the two
    /// modes share one navigation vocabulary, so a hand that learned
    /// `ctrl+d` in the palette is not retrained at the dialog.
    #[test]
    fn normal_mode_still_honours_the_filter_modes_navigation() {
        use NormalCommand::*;
        assert_eq!(normal_command(&bare("down")), Some(Nav(NavCommand::Move(1))));
        assert_eq!(normal_command(&ks("d", Modifiers::CTRL)), Some(Nav(NavCommand::Move(5))));
        assert_eq!(normal_command(&ks("u", Modifiers::CTRL)), Some(Nav(NavCommand::Move(-5))));
    }

    /// A bare letter with no fixed meaning is the surface's own verb —
    /// the whole point of normal mode. `escape` is NOT a verb: it is the
    /// ladder's, and returning `Verb('escape')` would swallow it.
    #[test]
    fn unclaimed_letters_become_surface_verbs_but_escape_does_not() {
        assert_eq!(normal_command(&bare("s")), Some(NormalCommand::Verb('s')));
        assert_eq!(normal_command(&bare("d")), Some(NormalCommand::Verb('d')));
        assert_eq!(normal_command(&bare("r")), Some(NormalCommand::Verb('r')));
        assert_eq!(normal_command(&bare("escape")), None);
        assert_eq!(normal_command(&ks("s", Modifiers::CTRL)), None);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p geode-shell --features test-support dialogmode`
Expected: FAIL to compile — `cannot find type DialogMode`, `cannot find function escape_step`, `normal_command`.

- [ ] **Step 4: Write the implementation**

Put this above the test module in the same file:

```rust
//! The two-mode vocabulary Geode's *modal* dialogs share
//! (`docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`).
//!
//! A modal surface opens in [`DialogMode::Normal`], where no `Input` is
//! focused and bare letters are verbs; `/` enters [`DialogMode::Filter`],
//! which is exactly the always-focused filter that ships today. A surface
//! that has no verbs to reach outside its filter is *filter-only* and
//! never uses this module at all — the palette, settings, the dimension
//! picker and the as-of selector are unchanged (spec §3).
//!
//! No `gpui` here, in the mould of [`crate::vimnav`] and
//! [`crate::listfilter`]: feed it shell-native [`Keystroke`]s and
//! unit-test every transition without a window.

use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::vimnav::NavCommand;

/// Which mode a modal dialog is in. A filter-only surface has no value of
/// this type at all, rather than being permanently `Filter` — the
/// distinction matters because such a surface's `escape` closes the modal
/// instead of walking the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    Normal,
    Filter,
}

/// One rung of the `escape` ladder (spec §5). Each rung changes something
/// the user can see, so `escape` is never a keystroke that appears inert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeStep {
    /// Filter → normal, **keeping the query applied**: leaving a search
    /// leaves you on the match, it does not undo the search.
    LeaveFilter,
    ClearQuery,
    PreviousStage,
    Close,
}

/// The first rung that applies. `has_previous_stage` is the surface's own
/// question (4c's `Edit` has one, `Browse` does not); the keybinding
/// dialog always passes `false`.
pub fn escape_step(
    mode: DialogMode,
    query_is_empty: bool,
    has_previous_stage: bool,
) -> EscapeStep {
    match mode {
        DialogMode::Filter => EscapeStep::LeaveFilter,
        DialogMode::Normal if !query_is_empty => EscapeStep::ClearQuery,
        DialogMode::Normal if has_previous_stage => EscapeStep::PreviousStage,
        DialogMode::Normal => EscapeStep::Close,
    }
}

/// What a keystroke means in normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalCommand {
    /// Movement, shared with filter mode so one hand learns one set.
    Nav(NavCommand),
    EnterFilter,
    Commit,
    Toggle,
    /// Move the selected *item* rather than the selection: `shift+j` /
    /// `shift+k`. This is what replaces the pick-up sub-mode an earlier
    /// draft of Phase 4c needed when no key was free.
    MoveItem(i32),
    EditText,
    /// A bare letter the vocabulary does not claim — the surface's own
    /// verb (`s`, `d`, `r`, `n`).
    Verb(char),
}

/// Map one keystroke to its normal-mode meaning, or `None` when the
/// surface should handle it itself (`escape`, which belongs to the
/// ladder) or ignore it.
pub fn normal_command(ks: &Keystroke) -> Option<NormalCommand> {
    // Shared navigation first, so `ctrl+d` and the arrows mean the same
    // thing here as they do with the filter focused.
    if let Some(cmd) = listfilter::nav_command(ks) {
        return Some(NormalCommand::Nav(cmd));
    }
    if ks.mods == Modifiers::SHIFT {
        return match ks.key.as_str() {
            "j" => Some(NormalCommand::MoveItem(1)),
            "k" => Some(NormalCommand::MoveItem(-1)),
            "g" => Some(NormalCommand::Nav(NavCommand::Bottom)),
            _ => None,
        };
    }
    if ks.mods != Modifiers::NONE {
        return None;
    }
    match ks.key.as_str() {
        "j" => Some(NormalCommand::Nav(NavCommand::Move(1))),
        "k" => Some(NormalCommand::Nav(NavCommand::Move(-1))),
        "g" => Some(NormalCommand::Nav(NavCommand::Top)),
        "/" => Some(NormalCommand::EnterFilter),
        "enter" => Some(NormalCommand::Commit),
        "space" => Some(NormalCommand::Toggle),
        "i" => Some(NormalCommand::EditText),
        // `escape` is the ladder's, never a verb — returning it here
        // would swallow the one key every dialog needs.
        "escape" => None,
        key => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => Some(NormalCommand::Verb(c)),
                _ => None,
            }
        }
    }
}
```

Then add `pub mod dialogmode;` to `crates/geode-shell/src/lib.rs`, in the existing alphabetical position (between `defaults` and `fonts`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --features test-support dialogmode`
Expected: PASS, 4 tests.

If `normal_mode_maps_the_shared_vocabulary` fails on `bare("space")`, check what `keymap::parse_keystroke` names that key in this codebase and use that spelling in both the test and the `match` — do not add a second spelling.

- [ ] **Step 6: Run the full local gate**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all pass. Nothing consumes the new module yet, so no behaviour has changed.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-shell/src/dialogmode.rs crates/geode-shell/src/lib.rs crates/geode-shell/src/vimnav.rs
git commit -m "feat(shell): the pure two-mode dialog vocabulary

DialogMode, the escape ladder and the normal-mode key mapping, with no
gpui, in the mould of listfilter and vimnav. Nothing consumes it yet."
```

---

### Task 2: `keymap_edit` learns to unbind

**Files:**
- Modify: `crates/geode-shell/src/keymap_edit.rs`

**Interfaces:**
- Consumes: the existing `apply_rebind` machinery in the same file — entry lookup/creation, `keymap::UNBOUND_ACTION` (`"none"`, `keymap/build.rs:8`), and the temp-file-and-rename write.
- Produces: `Unbind { context: Option<String>, key: String, is_user_layer: bool }`; `UnbindOutcome { removed: bool }`; `apply_unbind(&Path, &Unbind) -> Result<UnbindOutcome, String>`.

An unbind is the displacement half of a rebind performed on its own. When the binding lives in the user's own layer it is **removed** from that entry; when it comes from builtin or desk it is **shadowed** by writing `"none"`. Getting that branch backwards would delete a user's unrelated binding instead of silencing a desk one, which is why it gets its own mutation entry.

- [ ] **Step 1: Write the failing tests**

Add to `keymap_edit.rs`'s existing `mod tests`. Reuse whatever tempdir helper that module already uses for `apply_rebind`'s tests — read the top of the test module first and follow it exactly rather than introducing a second helper.

```rust
/// A binding that comes from builtin or desk cannot be removed — this
/// module only ever writes the user layer — so it is silenced with the
/// documented `"none"` shadow instead.
#[test]
fn unbinding_a_lower_layer_binding_writes_a_none_shadow() {
    let dir = tempdir_with("");
    let out = apply_unbind(dir.path(), &Unbind {
        context: None,
        key: "ctrl+k".into(),
        is_user_layer: false,
    })
    .expect("write");
    assert!(!out.removed, "a shadow is not a removal");
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).unwrap();
    assert!(text.contains(r#""ctrl+k" = "none""#), "{text}");
}

/// The user's own binding is removed outright, leaving no redundant
/// `"none"` in a table this module owns.
#[test]
fn unbinding_a_user_layer_binding_removes_the_key() {
    let dir = tempdir_with(
        "config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n\
         \"ctrl+k\" = \"palette::toggle\"\n\"ctrl+j\" = \"tile::focus_down\"\n",
    );
    let out = apply_unbind(dir.path(), &Unbind {
        context: None,
        key: "ctrl+k".into(),
        is_user_layer: true,
    })
    .expect("write");
    assert!(out.removed);
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).unwrap();
    assert!(!text.contains("ctrl+k"), "the key is gone: {text}");
    assert!(text.contains("ctrl+j"), "siblings survive: {text}");
    assert!(!text.contains("none"), "no redundant shadow: {text}");
}

/// The caller's belief about where a binding lives can be stale. Removal
/// that finds nothing reports it rather than failing — the same
/// `Displacement::OldKeyNotFound` contract `apply_rebind` already keeps.
#[test]
fn a_removal_that_finds_nothing_reports_it_without_failing() {
    let dir = tempdir_with("config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n");
    let out = apply_unbind(dir.path(), &Unbind {
        context: None,
        key: "ctrl+k".into(),
        is_user_layer: true,
    })
    .expect("a stale belief is not a write failure");
    assert!(!out.removed);
}

/// Comments and unrelated tables survive, as they do for every other
/// keyed persist in this crate.
#[test]
fn unbinding_preserves_comments_and_unrelated_entries() {
    let dir = tempdir_with(
        "# my keymap\nconfig_version = 1\n\n[[bindings]]\ncontext = \"tile\"\n\n\
         [bindings.keys]\n\"ctrl+k\" = \"tile::close\"\n",
    );
    apply_unbind(dir.path(), &Unbind {
        context: None,
        key: "ctrl+k".into(),
        is_user_layer: false,
    })
    .expect("write");
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).unwrap();
    assert!(text.contains("# my keymap"), "{text}");
    assert!(text.contains(r#"context = "tile""#), "the tile entry is untouched: {text}");
    assert!(text.contains(r#""ctrl+k" = "tile::close""#), "{text}");
    assert!(text.contains(r#""ctrl+k" = "none""#), "the no-context entry got the shadow: {text}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell --features test-support keymap_edit`
Expected: FAIL to compile — `cannot find type Unbind`, `cannot find function apply_unbind`.

- [ ] **Step 3: Write the implementation**

Add beside `Rebind`/`apply_rebind`. Reuse this file's existing entry-lookup and write helpers — read `apply_rebind`'s body and call the same private functions rather than duplicating the lookup or the temp-file-and-rename.

```rust
/// One binding to silence, the displacement half of a [`Rebind`]
/// performed on its own (`keybindings_view`'s `d`).
pub struct Unbind {
    /// The `[[bindings]]` entry's `context`, matched exactly as
    /// [`Rebind::context`] is. `None` means the no-`context` entry.
    pub context: Option<String>,
    /// The rendered keystroke to silence, e.g. `"ctrl+k"`.
    pub key: String,
    /// Whether the binding being silenced was itself set by a user-layer
    /// entry. `true` removes the key outright; `false` shadows a
    /// builtin/desk binding by writing `keymap::UNBOUND_ACTION`.
    ///
    /// Getting this backwards is the dangerous case, not a cosmetic one:
    /// a wrong `true` deletes whatever the user *did* have on that key,
    /// and a wrong `false` leaves a redundant `"none"` shadowing the
    /// user's own entry so the key stays dead.
    pub is_user_layer: bool,
}

/// What [`apply_unbind`] did. `removed` is false for a shadow write, and
/// also for a removal that found nothing to remove — the caller's belief
/// about where the binding lives can be stale, which is a warning rather
/// than a failure (the same contract [`Displacement::OldKeyNotFound`]
/// keeps for a rebind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnbindOutcome {
    pub removed: bool,
}

/// Silence one binding in `<user_dir>/keymap.toml`. `Err` only ever means
/// the file read/parse/write itself failed; the file is left untouched on
/// a parse error, exactly as `apply_rebind` leaves it.
pub fn apply_unbind(user_dir: &Path, unbind: &Unbind) -> Result<UnbindOutcome, String> {
    // Follow apply_rebind's own body: read-or-create the document, find
    // or create the [[bindings]] entry matching `unbind.context`, then
    // either remove `unbind.key` from its `keys` table or set it to
    // `crate::keymap::UNBOUND_ACTION`, then write through the same
    // temp-file-and-rename helper.
    todo!("implement against apply_rebind's existing helpers — see Step 3's note")
}
```

**Do not leave the `todo!`.** It is here to mark exactly which helpers to reach for; the implementation is the two branches described in the doc comment, using this file's existing private functions. If `apply_rebind` performs its lookup inline rather than through a helper, extract that lookup into a private function first, in its own commit, and have both callers use it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --features test-support keymap_edit`
Expected: PASS — the four new tests plus every existing `apply_rebind` test still green.

- [ ] **Step 5: Add the mutation entries**

Append to `scripts/mutation-check.sh`, in the keymap section:

```bash
# The dangerous branch: a wrong `true` deletes the user's own binding on
# that key instead of shadowing a desk one.
run_mutation "keymap_edit: unbind always removes instead of shadowing" \
  crates/geode-shell/src/keymap_edit.rs \
  '    if unbind.is_user_layer {' \
  '    if true {' \
  geode-shell unbinding_a_lower_layer_binding_writes_a_none_shadow

run_mutation "keymap_edit: unbind always shadows instead of removing" \
  crates/geode-shell/src/keymap_edit.rs \
  '    if unbind.is_user_layer {' \
  '    if false {' \
  geode-shell unbinding_a_user_layer_binding_removes_the_key
```

Adjust both anchors to the exact line the implementation ended up with — the anchor must match the file verbatim and appear exactly once.

- [ ] **Step 6: Commit, then run the harness**

```bash
git add crates/geode-shell/src/keymap_edit.rs scripts/mutation-check.sh
git commit -m "feat(shell): keymap_edit can unbind a key, shadowing or removing

The displacement half of a rebind, performed on its own: a builtin or
desk binding is shadowed with \"none\"; the user's own is removed. Two
harness entries, because getting the branch backwards deletes a binding
rather than silencing one."
zsh scripts/mutation-check.sh "keymap_edit:"
```
Expected: every entry reports `caught`.

---

### Task 3: The keybinding dialog goes modal

**Files:**
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs`
- Modify: `crates/geode-shell/src/shell/dialog.rs` (mode pill in `render_modal`)
- Test: `crates/geode-shell/src/shell/tests/keybindings_dialog.rs`

**Interfaces:**
- Consumes: Task 1's `dialogmode::{DialogMode, EscapeStep, NormalCommand, escape_step, normal_command}`.
- Produces: `KeybindingsState.mode: DialogMode`, defaulting to `Normal`.

The focus mechanic already exists in this file and is proven in production: entering rebind capture blurs the filter with `shell.focus_handle.focus(window, cx)` so raw keystrokes reach `handle_key` (`keybindings_view.rs:537`), and cancelling refocuses it with `input.read(cx).focus_handle(cx).focus(window, cx)`. Normal mode is that same switch, held open rather than momentary. **Reuse those two calls; do not invent a third focus path.**

- [ ] **Step 1: Write the failing tests**

Add to `crates/geode-shell/src/shell/tests/keybindings_dialog.rs`, following that file's existing helpers for opening the dialog and dispatching real keystrokes:

```rust
/// The dialog now opens in normal mode, so a bare letter is a verb
/// rather than filter text. This is the behaviour change the whole
/// interaction model turns on.
#[gpui::test]
fn the_dialog_opens_in_normal_mode_and_letters_do_not_type(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Normal,
    );
    vcx.simulate_keystrokes("s");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "a bare letter in normal mode must not reach the filter"
    );
}

/// `/` enters filter mode and typing narrows, exactly as it does today.
#[gpui::test]
fn slash_enters_filter_mode_and_typing_narrows(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("/");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Filter,
    );
    vcx.simulate_keystrokes("t h e m e");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "theme",
    );
}

/// The ladder, one visible step at a time: filter → normal keeping the
/// query, → clear the query, → close. A dialog that skipped a rung would
/// close on the first escape and lose the user's filter with it.
#[gpui::test]
fn escape_walks_the_ladder_one_rung_at_a_time(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("/ t h e m e");
    vcx.run_until_parked();

    vcx.simulate_keystrokes("escape");
    let (mode, q) = shell.read_with(&vcx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.mode, k.query.clone())
    });
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(q, "theme", "leaving filter must keep the query applied");

    vcx.simulate_keystrokes("escape");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "the second escape clears the query"
    );
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()), "and does not close");

    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()), "the third closes");
}

/// `j`/`k` move in normal mode; the arrows and ctrl-steps still work in
/// both modes, so one navigation vocabulary serves both.
#[gpui::test]
fn j_and_k_move_in_normal_mode(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("j j");
    assert_eq!(shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().selected), 2);
    vcx.simulate_keystrokes("k");
    assert_eq!(shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().selected), 1);
}
```

If `open_keybindings`, `open_shell`, `shell_of` or `test_services` are named differently in that file, use its names — read it first.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell --features test-support keybindings_dialog`
Expected: FAIL — no field `mode` on `KeybindingsState`.

- [ ] **Step 3: Implement**

1. Add to `KeybindingsState` (`keybindings_view.rs:246`):

```rust
    /// Which mode this dialog is in
    /// (`docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`).
    /// `Normal` on open: bare letters are verbs, and `dialog_input` is
    /// blurred to `shell.focus_handle` so they reach [`handle_key`] —
    /// the same switch rebind capture has always performed, held open
    /// rather than momentary.
    pub mode: DialogMode,
```

Initialise it to `DialogMode::Normal` in `KeybindingsState::new`.

2. In the call that opens this dialog, pass `focus_filter: false` to `open_shell_dialog_with_key`.

3. At the top of `handle_key`, after the existing `listening` branch (capture keeps first refusal — it is a third, momentary mode and must not be routed through `normal_command`), route by mode:

```rust
    if state.mode == DialogMode::Normal {
        if ks.mods == Modifiers::NONE && ks.key == "escape" {
            match dialogmode::escape_step(state.mode, state.query.is_empty(), false) {
                EscapeStep::ClearQuery => {
                    state.query.clear();
                    state.selected = 0;
                    input.update(cx, |i, cx| i.set_value("", window, cx));
                    cx.notify();
                    return true;
                }
                // A dialog with no nested stage never sees PreviousStage;
                // LeaveFilter is unreachable in Normal.
                _ => return false, // let the shell's modal branch close it
            }
        }
        let Some(cmd) = dialogmode::normal_command(ks) else {
            return true; // claimed and dropped: normal mode eats stray keys
        };
        match cmd {
            NormalCommand::Nav(nav) => {
                state.selected = vimnav::apply(state.selected, visible.len(), nav);
                let selected = state.selected;
                shell.keybindings_scroll.scroll_to_item(selected);
            }
            NormalCommand::EnterFilter => {
                state.mode = DialogMode::Filter;
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            NormalCommand::Commit => { /* existing enter-to-listen body */ }
            NormalCommand::Verb('d') => { /* Task 4 */ }
            NormalCommand::Verb('r') => { /* Task 4 */ }
            _ => {}
        }
        cx.notify();
        return true;
    }
```

Keep the existing filter-mode body below, unchanged, with one addition: `escape` in `Filter` sets `state.mode = DialogMode::Normal` and blurs with `shell.focus_handle.focus(window, cx)`, returning `true` — it must not fall through to the shell's close.

4. In `build`, add the mode pill to the title row and a footer hint row listing the current mode's vocabulary. Take the pill's colours from `cx.theme()` tokens — never a literal colour (house rule). Reuse `keybindings_view::key_chip` for the hint's keys, as `settings_view::build` and `shell::picker::hint_row` both do.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --features test-support keybindings_dialog`
Expected: PASS. Existing tests in that file that assumed a focused filter will fail — **fix them by prefixing their keystrokes with `/`**, not by reverting the open mode. Each such fix is a real behaviour change and belongs in this commit.

- [ ] **Step 5: Full gate, commit, harness**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A && git commit -m "feat(shell): the keybinding dialog opens in normal mode

Letters are free; / enters the filter that used to be always focused.
The focus switch is the one rebind capture already performed, held open
rather than momentary."
zsh scripts/mutation-check.sh --changed
```

Then add entries for the two behaviours a green suite would not see:

```bash
run_mutation "keybindings: the dialog opens in filter mode" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '            mode: DialogMode::Normal,' \
  '            mode: DialogMode::Filter,' \
  geode-shell the_dialog_opens_in_normal_mode_and_letters_do_not_type

run_mutation "keybindings: leaving filter mode clears the query" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '                state.mode = DialogMode::Normal;' \
  '                state.mode = DialogMode::Normal;
                state.query.clear();' \
  geode-shell escape_walks_the_ladder_one_rung_at_a_time
```

Adjust both anchors to the lines the implementation ended up with.

---

### Task 4: `d` unbinds, `r` resets

**Files:**
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs`
- Test: `crates/geode-shell/src/shell/tests/keybindings_dialog.rs`

**Interfaces:**
- Consumes: Task 2's `keymap_edit::{Unbind, UnbindOutcome, apply_unbind}`; Task 3's normal-mode dispatch.

This is the capability the whole model exists to prove: today you cannot clear a binding without hand-editing `keymap.toml`.

`d` silences the selected row's effective binding. `r` resets it — which is the same write with `is_user_layer: true`, because resetting means *removing the user's override* so the layer beneath shows through. The row already knows which layer its binding came from; read how `derive_rows` builds a row and pass that through rather than guessing.

- [ ] **Step 1: Write the failing test**

```rust
/// The gap this model exists to close: before it, clearing a binding
/// meant hand-editing keymap.toml.
#[gpui::test]
fn d_unbinds_the_selected_binding(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    // Land on a row with a known binding rather than trusting row 0.
    vcx.simulate_keystrokes("/ p a l e t t e");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("escape");

    vcx.simulate_keystrokes("d");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml"))
        .expect("d must write the user keymap");
    assert!(text.contains("none"), "a builtin binding is silenced, not removed: {text}");
}
```

If the test harness has no `open_shell_with_user_dir`, add one beside the existing `open_shell` that sets `ShellView::user_dir` to the given path — the field already exists (`shell/input.rs:327` reads it).

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p geode-shell --features test-support d_unbinds_the_selected_binding`
Expected: FAIL — no `keymap.toml` written, because `d` does nothing yet.

- [ ] **Step 3: Implement**

Fill the two `Verb` arms left in Task 3. Build an `Unbind` from the selected row and spawn the write on the background executor, following `ShellView::persist_theme` (`shell/input.rs:326`) exactly: clone `user_dir`, `cx.background_executor().spawn(...)`, `.detach()`, warn on error. **No file I/O on the render thread.**

Show the outcome in the row (the binding column reads `—` once unbound) by letting `derive_rows` re-derive from the reloaded keymap, which the config watcher applies. Do not cache the result in `KeybindingsState`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p geode-shell --features test-support keybindings_dialog`
Expected: PASS.

- [ ] **Step 5: Full gate, harness entry, commit**

```bash
run_mutation "keybindings: d never writes an unbind" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '            NormalCommand::Verb('"'"'d'"'"') => {' \
  '            NormalCommand::Verb('"'"'\0'"'"') => {' \
  geode-shell d_unbinds_the_selected_binding
```

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A && git commit -m "feat(shell): d unbinds and r resets in the keybinding dialog

The capability filter-first could not express: with letters free, the
dialog can finally clear a binding instead of sending you to
keymap.toml."
zsh scripts/mutation-check.sh --changed
```

---

### Task 5: Docs and harness reconciliation

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` (as-built notes only)
- Modify: `scripts/mutation-check.sh` (entry count comment)

- [ ] **Step 1: Record the model in `CLAUDE.md`**

Add a paragraph after the Phase 4c one: the two modes, which surfaces are modal (keybindings and 4c's dialogs) and which are filter-only (palette, settings, picker, as-of) with the rule that decides it, that `dialogmode` is the pure core, and the maintainer trap — **`dialog::init_reclaimed_keybindings` does not shrink**; its `tab` binding suppresses `Root`'s focus cycling regardless of focus, and filter mode still focuses the `Input`.

- [ ] **Step 2: Reconcile the spec with what was built**

Append an "as-built" note to the spec for anything that differed. If nothing differed, say so explicitly in one line — an unreconciled spec is worse than one that records "built as specified".

- [ ] **Step 3: Fix the harness entry count**

```bash
echo $(( $(grep -c '^run_mutation' scripts/mutation-check.sh) - 1 ))
```
(The `- 1` excludes the `run_mutation()` function definition, which the naive grep counts.) Put that number in `CLAUDE.md`'s `mutation harness (N entries)` line.

- [ ] **Step 4: Full harness, then commit**

```bash
zsh scripts/mutation-check.sh 2>&1 | tail -40
```
Run it detached; the unfiltered run takes about an hour. Every line must read `caught`; a `SURVIVED` is a missing test, not a curiosity.

```bash
git add -A && git commit -m "docs: the dialog interaction model, as built"
```

---

## Self-Review

**Spec coverage.** §2 modes → Task 1. §3 which surfaces are modal → Task 3 (keybindings) and Task 5 (recorded); the four filter-only surfaces are untouched by construction, since no task edits them. §4 vocabulary → Task 1, applied in Task 3. §5 ladder → Task 1 + Task 3. §6 key contexts → **not implemented**; the spec's §14 open question 1 defers registered actions to a follow-up, and Task 3 handles keys directly as that question recommends, so the `dialog` contexts land with 4c or later. §7 (no reclaim reduction) → Task 5's maintainer note. §8 unbind/reset → Tasks 2 and 4. §9 discoverability → Task 3 step 3.4. §11 risks → risk 3 (`space`) is untested here because the keybinding dialog has nothing to toggle; it first bites in 4c.

**Placeholders.** One deliberate `todo!` in Task 2 Step 3, flagged in bold with instructions not to leave it — it marks which existing helpers to reuse, which is a judgement the implementer must make against code this plan should not transcribe wholesale. Every other step carries real code.

**Type consistency.** `DialogMode`, `EscapeStep`, `NormalCommand`, `normal_command`, `escape_step` are used in Tasks 3 and 4 exactly as Task 1 defines them. `Unbind`/`apply_unbind` in Task 4 match Task 2. `NavCommand::Move(i64)` is `i64` (matching `vimnav.rs:68`) while `NormalCommand::MoveItem(i32)` is `i32` — deliberate, since `MoveItem` is a ±1 list reorder rather than a count-multiplied navigation, but call it out to any reviewer who spots the asymmetry.
