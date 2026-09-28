# Stacked Object Dialogs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From inside one configuration (object) dialog, open an object dialog of a different domain from the palette, and return to the covered one exactly as it was left.

**Architecture:** Park and swap. `shell.object_dialog` keeps meaning "the topmost object dialog". Opening a second one moves the live `ObjectDialogState` (plus scroll offset) into the covered stack entry's new `parked_object` slot; popping restores it. A per-domain rule replaces the one-per-kind rule for `Object`. Async deliveries, reload refreshes, and failed-write reverts reach parked states too.

**Tech Stack:** Rust, GPUI (`gpui-pre =0.3.5`), gpui-component 0.6.2, `geode-shell` crate only.

**Spec:** `docs/superpowers/specs/2026-09-27-stacked-object-dialogs-design.md`

## Global Constraints

- Only `crates/geode-shell` changes; no new dependency, no new crate.
- Dialogs still open only through `shell::dialog::open_shell_dialog*`; pure dialog state stays the truth and `sync_dialog_text` stays the only text/focus writer.
- Test production routes: palette keystrokes or `ShellView::dispatch`, real keys, `deliver_distinct`. Never call `park`/`unpark` directly from a GPUI test.
- Every changed correctness contract gets a targeted `scripts/mutation-check.sh` entry naming its test; run `zsh scripts/mutation-check.sh --anchors-only` before merge. Commit before any mutation run (the harness edits tracked files).
- `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` must pass.
- User-facing text says "color", not "colour" (identifiers in touched code may keep existing names).
- When behavior changes, update `docs/current/input-and-dialogs.md` and `crates/geode-shell/README.md` in the same branch.

## Review Focus

1. **A dirty covered draft plus a stacked write in one batch.** Views has a debounced edit pending when Colors commits; both land in one `PendingConfigWrite`. If that write fails, both drafts must be rebuilt (both domains' in-memory edits were reverted). Test in Task 2 (`a_shared_batch_failure_rebuilds_every_contributing_draft`).
2. **Refreshing choices must not dirty a draft.** Updating a `color` field's options without the baseline would make `is_dirty` true and write an unchanged column. Test in Task 3 (`refreshing_color_options_keeps_the_draft_clean`).
3. **A covered color typeahead.** The trader opened `i` on the color field, then pushed Colors. On return the open typeahead must list the new color and keep its typed query. Test in Task 3.
4. **Three deep.** Views → Colors → Scopes, then pop twice: each reveal restores its own state; requesting Views or Colors from the top is refused. Test in Task 1.
5. **A covered Scopes Values stage.** Its `SCOPES_KEY` reply arrives while Colors is on top; today `deliver_values` reads only the live dialog and drops it, leaving "loading…" forever. Test in Task 2.

---

## File map

- `crates/geode-shell/src/shell/dialog.rs`: `ParkedObject`, `ShellModal::parked_object`, `can_open_object`, `park_object_dialog`, `unpark_object_dialog`, `parked_objects_mut`, backstop change, notice recognition.
- `crates/geode-shell/src/shell/mod.rs`: `clear_dialog_state(Object)` unparks.
- `crates/geode-shell/src/shell/objectdialog/mod.rs`: `Domain::ALL`, `Domain::already_open_notice`, `Draft::refresh_color_options`.
- `crates/geode-shell/src/shell/objectdialog/render.rs`: `open`, `open_save_scope`, `open_object` use the domain rule; `deliver_values` finds the Scopes dialog wherever it is; `refresh_color_choices`; `enter_column_stage` uses `colours::names`.
- `crates/geode-shell/src/shell/objectdialog/colours.rs`: `names(config)`.
- `crates/geode-shell/src/shell/objectdialog/views.rs`: `color_options` extracted from `column_fields`.
- `crates/geode-shell/src/shell/objectdialog/apply.rs`: `PendingConfigWrite::origins`, origin threaded through `queue_batch`/`schedule_flush`, revert rebuilds by origin.
- `crates/geode-shell/src/shell/expr_suggest.rs`, `hot_reload.rs`: reach parked states.
- `crates/geode-shell/src/shell/tests/object_stack.rs` (new) and `tests/mod.rs`: GPUI tests. `tests/dialog_stack.rs`: rework the refusal test. `tests/objectdialog.rs`: widen a few helpers to `pub(super)`.
- `scripts/mutation-check.sh`: new entries.
- Docs: `docs/current/input-and-dialogs.md`, `crates/geode-shell/README.md`.

---

### Task 1: Per-domain stacking with park and swap

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`DialogKind` docs ~52-70, `is_already_open_notice` ~105, `can_open` ~111-123, `ShellModal` ~164-183, push ~340-383)
- Modify: `crates/geode-shell/src/shell/mod.rs:1294-1306` (`clear_dialog_state`)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`impl Domain` near `title()` ~174)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs:74-100` (`open`), `:655-690` (`open_save_scope`), `:697-710` (`open_object`)
- Create: `crates/geode-shell/src/shell/tests/object_stack.rs`
- Modify: `crates/geode-shell/src/shell/tests/mod.rs:602` (add `mod object_stack;`)
- Modify: `crates/geode-shell/src/shell/tests/objectdialog.rs` (make `dialog_state`, `desk_view_services`, `flush_config_write` `pub(super)`)
- Modify: `crates/geode-shell/src/shell/tests/dialog_stack.rs:125-170` (`a_kind_already_in_the_stack_is_refused`)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces (used by Tasks 2 and 3):
  - `pub struct ParkedObject { pub state: ObjectDialogState, pub scroll: gpui::Point<Pixels> }` in `dialog.rs`
  - `ShellModal::parked_object: Option<ParkedObject>`
  - `pub(crate) fn can_open_object(view: &mut ShellView, domain: Domain) -> bool`
  - `pub(crate) fn park_object_dialog(view: &mut ShellView)`
  - `pub(crate) fn unpark_object_dialog(view: &mut ShellView)`
  - `pub(crate) fn parked_objects_mut(modals: &mut [ShellModal]) -> impl Iterator<Item = &mut ObjectDialogState>`
  - `Domain::ALL: [Domain; 7]`, `Domain::already_open_notice(self) -> &'static str`
  - Test helpers in `tests/object_stack.rs`: `kinds`, `domains`, `open_palette_action`

- [ ] **Step 1: Widen the test helpers**

In `crates/geode-shell/src/shell/tests/objectdialog.rs`, change `fn dialog_state`, `fn desk_view_services`, and `fn flush_config_write` to `pub(super) fn`. No other change.

- [ ] **Step 2: Write the failing tests**

Create `crates/geode-shell/src/shell/tests/object_stack.rs`:

```rust
//! Object dialogs of different domains stack over each other. The covered one is
//! parked in its own stack entry and comes back exactly as it was left; the same
//! domain never nests.

use super::objectdialog::{desk_view_services, dialog_state, edit_draft};
use super::*;
use crate::shell::dialog::DialogKind;
use crate::shell::objectdialog::{self, Domain};

pub(super) fn kinds(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Vec<DialogKind> {
    shell.read_with(cx, |s, _| s.modals.iter().map(|m| m.kind).collect())
}

/// The domain of every object dialog in the stack, bottom first: parked ones from
/// their entries, then the live one.
pub(super) fn domains(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Vec<Domain> {
    shell.read_with(cx, |s, _| {
        s.modals
            .iter()
            .filter_map(|m| m.parked_object.as_ref().map(|p| p.state.domain))
            .chain(s.object_dialog.as_ref().map(|d| d.domain))
            .collect()
    })
}

/// Open the palette, type `title`, press Enter: the trader's route.
pub(super) fn open_palette_action(cx: &mut gpui::VisualTestContext, title: &str) {
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input(title);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
}

fn views_with_colors() -> ShellServices {
    desk_view_services(&[("colors", "[delta]\nhue = 240\n")])
}

/// Views in a column stage, Colors pushed from the palette, then Escape back:
/// Views returns on the same column stage and row, with the same mode.
#[gpui::test]
fn colors_pushes_over_a_views_column_stage_and_escape_returns_to_it(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter"); // first column's stage
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    assert!(matches!(stage, objectdialog::Stage::Column { .. }), "{stage:?}");
    cx.simulate_keystrokes("j j");
    let selected = edit_draft(&shell, &cx, |d| d.selected);

    open_palette_action(&mut cx, "Edit colors");
    assert_eq!(kinds(&shell, &cx), vec![DialogKind::Object, DialogKind::Object]);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert!(cx.debug_bounds("objectdialog-swatch-delta").is_some(), "Colors paints");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(kinds(&shell, &cx), vec![DialogKind::Object]);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
    assert_eq!(edit_draft(&shell, &cx, |d| d.selected), selected);
}

/// The same domain never nests: Views over Views is refused with a notice naming
/// the domain, and the live Views state is untouched.
#[gpui::test]
fn the_same_domain_is_refused_with_its_own_notice(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    open_palette_action(&mut cx, "Edit colors");
    // Dispatch directly for the refusals: the notice is read before any later key
    // could replace it.
    dispatch_action(&shell, "config::views", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.notice),
        Some("views is already open underneath")
    );
    // Asking for the domain already on top does nothing and says nothing.
    shell.update(&mut cx, |s, _| s.notice = None);
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(shell.read_with(&cx, |s, _| s.notice), None);
}

/// Three deep: each pop reveals its own dialog, and neither covered domain can be
/// requested again from the top.
#[gpui::test]
fn three_object_dialogs_pop_in_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("enter"); // delta's edit stage
    cx.run_until_parked();
    open_palette_action(&mut cx, "Edit scopes");
    assert_eq!(
        domains(&shell, &cx),
        vec![Domain::Views, Domain::Colors, Domain::Scopes]
    );
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(shell.read_with(&cx, |s, _| s.notice), Some("colors is already open underneath"));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { object: "delta".into() },
        "Colors comes back on delta's edit stage"
    );
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
}

/// A non-object dialog between two object dialogs: the Views state is parked with
/// the Views entry, not the Choice list, and survives both pops.
#[gpui::test]
fn a_choice_list_between_two_object_dialogs_keeps_the_lower_one(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    dispatch_action(&shell, "log::level", &mut cx);
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(
        kinds(&shell, &cx),
        vec![DialogKind::Object, DialogKind::Choice, DialogKind::Object]
    );
    assert!(shell.read_with(&cx, |s, _| s.modals[0].parked_object.is_some()));
    assert!(shell.read_with(&cx, |s, _| s.modals[1].parked_object.is_none()));

    cx.simulate_keystrokes("escape"); // Colors
    cx.run_until_parked();
    assert_eq!(kinds(&shell, &cx), vec![DialogKind::Object, DialogKind::Choice]);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views], "Views is live again, covered");
    cx.simulate_keystrokes("escape"); // the Choice list
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
}

/// `open_tree_edit_stage` over caller-supplied services.
fn open_tree_edit_stage_with(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    dir: &std::path::Path,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir, "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    (shell, cx)
}
```

Add `mod object_stack;` after `mod objectdialog;` in `crates/geode-shell/src/shell/tests/mod.rs`.

Rework `a_kind_already_in_the_stack_is_refused` in `tests/dialog_stack.rs`. The `config::scopes` request over Views now pushes, so the refused request becomes `config::views`:

```rust
    dispatch_action(&shell, "config::views", &mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::Settings]
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice),
        Some(crate::shell::objectdialog::Domain::Views.already_open_notice())
    );
```

Leave the rest of that test as it is, including the final loop showing the notice does not outlive the stack. Also update its doc comment and the module doc line "One instance per kind." to "One instance per kind, and per domain for object dialogs."

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p geode-shell object_stack a_kind_already_in_the_stack_is_refused`
Expected: compile errors (`parked_object`, `already_open_notice` on `Domain` do not exist).

- [ ] **Step 4: Add the domain vocabulary**

In `crates/geode-shell/src/shell/objectdialog/mod.rs`, inside `impl Domain` next to `title()`:

```rust
    /// Every domain, for recognising [`Domain::already_open_notice`] strings.
    pub const ALL: [Domain; 7] = [
        Domain::Views,
        Domain::Groupings,
        Domain::Scopes,
        Domain::Schema,
        Domain::Sources,
        Domain::Colors,
        Domain::Expressions,
    ];

    /// The status notice for a request refused because this domain's dialog is
    /// already open lower in the stack.
    pub fn already_open_notice(self) -> &'static str {
        match self {
            Domain::Views => "views is already open underneath",
            Domain::Groupings => "groupings is already open underneath",
            Domain::Scopes => "scopes is already open underneath",
            Domain::Schema => "schema is already open underneath",
            Domain::Sources => "sources is already open underneath",
            Domain::Colors => "colors is already open underneath",
            Domain::Expressions => "expressions is already open underneath",
        }
    }
```

- [ ] **Step 5: Add parking to `dialog.rs`**

Update the `DialogKind` doc: every kind but `Plain` and `Object` appears at most once. `Object` appears once per domain; the covered states are parked in their entries. Change the `Object` variant's doc to: "Every object-dialog domain. `ShellView::object_dialog` holds the topmost one's state; covered ones are parked in their own entries ([`ShellModal::parked_object`])."

Replace the `Object` arm of `already_open_notice` with `"a configuration dialog is already open underneath"` unchanged (still used by `can_open`'s generic path). Extend `is_already_open_notice`:

```rust
pub(crate) fn is_already_open_notice(notice: &str) -> bool {
    DialogKind::ALL
        .iter()
        .any(|kind| kind.already_open_notice() == notice)
        || super::objectdialog::Domain::ALL
            .iter()
            .any(|domain| domain.already_open_notice() == notice)
}
```

Add after `can_open`:

```rust
/// Whether an object dialog on `domain` may be pushed now. Object dialogs stack
/// across domains; the same domain never nests, because two drafts of one file
/// would race each other's writes. The domain already on top is a silent no-op;
/// one lower in the stack says so in the status bar.
pub(crate) fn can_open_object(view: &mut ShellView, domain: Domain) -> bool {
    let live = view.object_dialog.as_ref().map(|state| state.domain);
    if live == Some(domain) && view.top_kind() == Some(DialogKind::Object) {
        return false;
    }
    let covered = live == Some(domain)
        || view
            .modals
            .iter()
            .filter_map(|m| m.parked_object.as_ref())
            .any(|parked| parked.state.domain == domain);
    if covered {
        view.notice = Some(domain.already_open_notice());
        return false;
    }
    true
}

/// Park the live object dialog in the entry that owns it (the topmost `Object`
/// entry) before another object dialog is installed, and reset the shared scroll
/// handle for the newcomer. A no-op when no object dialog is live.
pub(crate) fn park_object_dialog(view: &mut ShellView) {
    let Some(state) = view.object_dialog.take() else {
        return;
    };
    let scroll = view.object_dialog_scroll.offset();
    view.object_dialog_scroll.set_offset(gpui::Point::default());
    let owner = view
        .modals
        .iter_mut()
        .rev()
        .find(|m| m.kind == DialogKind::Object);
    debug_assert!(owner.is_some(), "a live object dialog has a stack entry");
    if let Some(owner) = owner {
        debug_assert!(owner.parked_object.is_none(), "the live entry parks once");
        owner.parked_object = Some(ParkedObject { state, scroll });
    }
}

/// Restore the topmost remaining `Object` entry's parked state after its cover
/// popped. Runs before `refocus_top`, so `sync_dialog_text` reads the revealed
/// state.
pub(crate) fn unpark_object_dialog(view: &mut ShellView) {
    let Some(parked) = view
        .modals
        .iter_mut()
        .rev()
        .find(|m| m.kind == DialogKind::Object)
        .and_then(|m| m.parked_object.take())
    else {
        return;
    };
    view.object_dialog = Some(parked.state);
    view.object_dialog_scroll.set_offset(parked.scroll);
}

/// Every parked object dialog, top first. Take `&mut view.modals` rather than the
/// view so callers can hold other `ShellView` fields (the config) at the same time.
pub(crate) fn parked_objects_mut(
    modals: &mut [ShellModal],
) -> impl Iterator<Item = &mut ObjectDialogState> {
    modals
        .iter_mut()
        .rev()
        .filter_map(|m| m.parked_object.as_mut().map(|parked| &mut parked.state))
}
```

Imports at the top of `dialog.rs`: `use super::objectdialog::{Domain, ObjectDialogState};`.

Add the struct next to `SavedInput`:

```rust
/// An object dialog covered by an object dialog of another domain: its whole state
/// and the shared scroll handle's offset, restored when the cover pops.
pub struct ParkedObject {
    pub state: ObjectDialogState,
    pub scroll: gpui::Point<Pixels>,
}
```

Add the field to `ShellModal` after `saved_input`:

```rust
    /// Set while an object dialog of another domain covers this `Object` entry;
    /// restored by [`unpark_object_dialog`].
    pub parked_object: Option<ParkedObject>,
```

and `parked_object: None,` in the one `view.modals.push(ShellModal { .. })` literal.

In `open_shell_dialog_with_key`, change the backstop:

```rust
    // Backstop for an opener that skipped its own `can_open` check. By then that
    // opener may already have overwritten the live state, which is why each
    // opener checks first. Object dialogs stack per domain; their opener checks
    // `can_open_object` and parks the covered state before installing its own,
    // so the per-kind check would wrongly refuse them here.
    if kind != DialogKind::Object && !can_open(view, kind) {
        return;
    }
```

- [ ] **Step 6: Unpark on close**

In `crates/geode-shell/src/shell/mod.rs`, `clear_dialog_state`:

```rust
            DialogKind::Object => {
                self.object_dialog = None;
                // The next object dialog down, if any, becomes live again.
                dialog::unpark_object_dialog(self);
            }
```

Update the doc to: "Drop the state field `kind` owns, and for an object dialog, bring back the one it covered. A field left behind would swallow the next same-kind dialog's queries."

- [ ] **Step 7: Route the three openers through the domain rule**

In `objectdialog/render.rs`:

`open`: replace the guard and install:

```rust
    if !dialog::can_open_object(view, domain) {
        return;
    }
    // A covered object dialog of another domain keeps its whole state in its own
    // stack entry until this one closes.
    dialog::park_object_dialog(view);
    // Fresh state every open — nothing survives a close/reopen, the same
    // contract `palette` and both list dialogs hold.
    view.object_dialog = Some(ObjectDialogState::new(domain));
```

Update its doc: "A no-op when this domain is already open (see `dialog::can_open_object`); over an object dialog of another domain it stacks."

`open_save_scope`: replace the guard with `if !dialog::can_open_object(shell, Domain::Scopes) { return; }` and rewrite the comment above it:

```rust
    // Guard before either branch touches `object_dialog`: `open` refuses a second
    // Scopes dialog silently, and without this check the notice or `begin_naming`
    // below would land on whichever object dialog is live. Another domain's dialog
    // (Views, say) is parked by `open` and comes back when Scopes closes.
```

`open_object`: replace its guard with `if !dialog::can_open_object(shell, domain) { return; }`, comment: "`open` refuses this domain silently when it is already open; without this guard the edit below would land on whatever object dialog is live."

- [ ] **Step 8: Run the tests**

Run: `cargo test -p geode-shell object_stack a_kind_already_in_the_stack_is_refused`
Expected: all five pass. Then `cargo test -p geode-shell dialog_stack objectdialog` — all pass.

- [ ] **Step 9: Add mutation entries**

Append to `scripts/mutation-check.sh` beside the existing "dialog stack:" entries:

```zsh
run_mutation "object stack: the same domain lower in the stack is pushed again" \
  crates/geode-shell/src/shell/dialog.rs \
  '            .any(|parked| parked.state.domain == domain);' \
  '            .any(|_| false);' \
  geode-shell \
  three_object_dialogs_pop_in_order

run_mutation "object stack: a popped cover does not bring back the dialog it covered" \
  crates/geode-shell/src/shell/mod.rs \
  '                dialog::unpark_object_dialog(self);' \
  '                {}' \
  geode-shell \
  colors_pushes_over_a_views_column_stage_and_escape_returns_to_it

run_mutation "object stack: the covered state parks with the stack top, not its owner" \
  crates/geode-shell/src/shell/dialog.rs \
  '        .rev()
        .find(|m| m.kind == DialogKind::Object);' \
  '        .rev()
        .next();' \
  geode-shell \
  a_choice_list_between_two_object_dialogs_keeps_the_lower_one
```

Run: `zsh scripts/mutation-check.sh --anchors-only`. Expected: no ANCHOR/AMBIG/FILTER errors. Commit (Step 10), then run `zsh scripts/mutation-check.sh "object stack"` and expect all three `caught`.

- [ ] **Step 10: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): object dialogs of different domains stack"
```

---

### Task 2: Deliveries and failed-write reverts reach a covered dialog

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/apply.rs` (`PendingConfigWrite` ~81-93, `commit_edit` ~284-335, `commit_removal` ~343-358, `commit_create` ~364-392, `queue_object` ~399-411, `queue_batch` ~415-431, `schedule_flush` ~476-495, `revert_failed_write` ~634-685)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs:5018-5060` (`deliver_values`)
- Modify: `crates/geode-shell/src/shell/expr_suggest.rs:160-175` (`deliver`)
- Modify: `crates/geode-shell/src/shell/hot_reload.rs:285-298` (the `expr_vocab` rebuild)
- Test: `crates/geode-shell/src/shell/tests/object_stack.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `dialog::parked_objects_mut`, `ShellModal::parked_object`, `can_open_object`, test helpers `kinds`, `domains`, `open_palette_action`, `views_with_colors`, `open_tree_edit_stage_with` from Task 1.
- Produces: `PendingConfigWrite::origins: Vec<Domain>`; `queue_batch(shell, edits, user_dir, delay, origin: Option<Domain>, cx)`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/object_stack.rs` (add `use super::objectdialog::{flush_config_write, services_with_a_saved_scope, open_expression_field};`, `use crate::shell::{EXPR_KEY, SCOPES_KEY};`, `use geode_core::query::DistinctOutcome;`, `use crate::shell::objectdialog::FieldKind;`):

```rust
/// A failed Colors write while Views is covered rebuilds only Colors' draft. Views
/// comes back on its column stage: a rebuild would have dropped it to the edit stage
/// and discarded the trader's place.
#[gpui::test]
fn a_failed_stacked_write_leaves_the_covered_draft_alone(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter"); // column stage
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    // Unparseable and never loaded: only the write discovers it.
    std::fs::write(dir.path().join("colors.toml"), "[delta\nhue =").unwrap();

    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("enter space"); // delta, hue step
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .is_some_and(|n| n.contains("could not save")),
        "Colors says its write failed"
    );

    cx.simulate_keystrokes("escape escape"); // edit stage → browse → close
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
    assert_eq!(dialog_state(&shell, &cx, |s| s.notice.clone()), None);
}

/// Both dialogs contributed to one failed batch: both in-memory edits were reverted,
/// so both drafts are rebuilt to show that.
#[gpui::test]
fn a_shared_batch_failure_rebuilds_every_contributing_draft(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    // A Views edit, still inside its debounce: tick the first column off.
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    std::fs::write(dir.path().join("colors.toml"), "[delta\nhue =").unwrap();
    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("enter space");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .is_some_and(|n| n.contains("could not save")),
        "Colors, the second contributor, says its write failed"
    );

    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .is_some_and(|n| n.contains("could not save")),
        "Views' edit rode the failed batch, so Views says so too"
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0].included),
        "and paints the reverted value"
    );
}

/// A Scopes Values stage covered by Colors still receives its values reply, and
/// shows it once revealed.
#[gpui::test]
fn a_covered_scopes_values_stage_receives_its_delivery(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter enter"); // mine → book's Values stage
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Scopes, Domain::Colors]);

    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
            },
            cx,
        )
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("values")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(names, ["BK000", "BK001"]);
}

/// A covered Scopes expression field receives its `EXPR_KEY` reply.
#[gpui::test]
fn a_covered_expression_field_receives_its_values(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, app| {
        let seen = seen.clone();
        app.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                seen.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    open_expression_field(&shell, &mut cx);
    cx.simulate_input("book = ");
    cx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("a request");
    dispatch_action(&shell, "config::colors", &mut cx);

    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: EXPR_KEY,
                tag: req.tag,
                column: req.column.clone(),
                values: Ok(vec![("BK001".into(), 7)]),
            },
            cx,
        )
    });
    let landed = shell.read_with(&cx, |s, _| {
        s.modals[0]
            .parked_object
            .as_ref()
            .and_then(|p| p.state.expr.as_ref())
            .is_some_and(|c| c.rows().iter().any(|r| format!("{r:?}").contains("BK001")))
    });
    assert!(landed, "the covered field's completion holds the delivered value");
}
```

If `ObjectDialogState::expr` or `Row` is not visible from the tests module, widen `expr` to `pub(crate)` (it is already read as `state.expr` in `expr_suggest.rs`) and keep the `Debug` probe on `Row`; `Row` derives `Debug`. If it does not, assert through `c.rows().len() > 0` after comparing with a no-delivery baseline read before the `deliver_distinct` call.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell object_stack`
Expected: the four new tests FAIL. The revert test fails on the Views stage (rebuilt to `Edit`) or on the notice; the shared-batch test fails on the Views notice; both delivery tests fail because the reply lands nowhere.

- [ ] **Step 3: Record batch origins**

In `apply.rs`, add to `PendingConfigWrite`:

```rust
    /// The object-dialog domains whose drafts contributed edits to this batch. A
    /// failed write reverts memory for the whole batch, so exactly these drafts are
    /// rebuilt; any other open dialog's draft had nothing in it and is left alone.
    origins: Vec<Domain>,
```

`schedule_flush` gains `origin: Option<Domain>` (before `cx`), initialises `origins: Vec::new()` in the `get_or_insert_with`, and after `pending.edits.extend(edits);`:

```rust
    if let Some(domain) = origin
        && !pending.origins.contains(&domain)
    {
        pending.origins.push(domain);
    }
```

`queue_batch` gains `origin: Option<Domain>` (before `cx`) and passes it through. Callers:
- `commit_edit`: `let origin = shell.object_dialog.as_ref().map(|s| s.domain);` read before `queue_batch`, pass `origin`.
- `commit_removal`: same.
- `commit_create`: pass `Some(domain)` (already bound).
- `queue_object`: pass `None`. Add to its doc: "No object dialog's draft contributed, so a failed write rebuilds none."

- [ ] **Step 4: Rebuild by origin**

Split the rebuild out of `revert_failed_write`:

```rust
/// Rebuild one dialog's draft from the reverted config and say why.
fn rebuild_after_revert(state: &mut ObjectDialogState, config: &Config, message: &str) {
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    // ... the existing body from `let selected = draft.selected;` through
    // `state.notice = Some(format!("could not save — change reverted ({message})"));`,
    // with `&shell.services.config` replaced by `config`.
}
```

and in `revert_failed_write`, replace the `if let Some(state) = shell.object_dialog.as_mut() ...` block with:

```rust
    // Only the drafts that contributed to the batch show it reverted. A covered
    // dialog of another domain keeps its unsaved draft: nothing of it was in the
    // batch, and rebuilding it would throw away the trader's place.
    let config = &shell.services.config;
    for state in shell
        .object_dialog
        .iter_mut()
        .chain(super::super::dialog::parked_objects_mut(&mut shell.modals))
        .filter(|state| pending.origins.contains(&state.domain))
    {
        rebuild_after_revert(state, config, &message);
    }
```

(Use whatever path `apply.rs` already uses to reach `crate::shell::dialog`; `crate::shell::dialog::parked_objects_mut` works from anywhere in the crate.)

- [ ] **Step 5: Deliver values to a covered Scopes dialog**

In `render.rs` `deliver_values`, replace the head:

```rust
    // The Scopes dialog that asked may be covered by another domain's dialog; its
    // reply is still its own.
    let Some(state) = shell
        .object_dialog
        .iter_mut()
        .chain(dialog::parked_objects_mut(&mut shell.modals))
        .find(|state| state.domain == Domain::Scopes)
    else {
        return;
    };
```

Keep everything after it unchanged. If the tail calls `sync_dialog_text` or scrolls `object_dialog_scroll`, gate those on `shell.object_dialog.as_ref().is_some_and(|s| s.domain == Domain::Scopes)`, since a parked dialog owns neither the input nor the scroll handle.

- [ ] **Step 6: Deliver expression values to a covered field**

In `expr_suggest.rs` `deliver`, replace the object branch:

```rust
    if !landed {
        for state in view
            .object_dialog
            .iter_mut()
            .chain(super::dialog::parked_objects_mut(&mut view.modals))
        {
            if super::objectdialog::expression_entry_open(state)
                && let Some(c) = state.expr.as_mut()
                && c.deliver(&outcome.column, outcome.tag, outcome.values.clone(), &vocab)
            {
                landed = true;
                break;
            }
        }
    }
```

- [ ] **Step 7: Rebuild every dialog's vocabulary on reload**

In `hot_reload.rs`, replace the `if let Some(state) = self.object_dialog.as_mut() ...` block with:

```rust
                for state in self
                    .object_dialog
                    .iter_mut()
                    .chain(super::dialog::parked_objects_mut(&mut self.modals))
                {
                    if let Some(expr) = state.expr.as_mut() {
                        expr.rebuild(&vocab);
                    }
                }
```

and update the comment above to mention parked object dialogs.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p geode-shell object_stack a_failed_write dialog_stack deliver_values`
Expected: all pass.

- [ ] **Step 9: Mutation entries**

```zsh
run_mutation "object stack: a failed write rebuilds every open draft" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        .filter(|state| pending.origins.contains(&state.domain))' \
  '        .filter(|_| true)' \
  geode-shell \
  a_failed_stacked_write_leaves_the_covered_draft_alone

run_mutation "object stack: a batch forgets a second contributing domain" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        && !pending.origins.contains(&domain)' \
  '        && pending.origins.is_empty()' \
  geode-shell \
  a_shared_batch_failure_rebuilds_every_contributing_draft

run_mutation "object stack: a covered Scopes dialog misses its values reply" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        .find(|state| state.domain == Domain::Scopes)' \
  '        .take(1).find(|state| state.domain == Domain::Scopes)' \
  geode-shell \
  a_covered_scopes_values_stage_receives_its_delivery
```

Run `--anchors-only`, commit, then `zsh scripts/mutation-check.sh "object stack"`; expect every entry caught.

- [ ] **Step 10: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "fix(shell): deliveries and reverts reach a covered object dialog"
```

---

### Task 3: New colors reach a covered column stage

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs:817-836` (`column_fields`), add `color_options`
- Modify: `crates/geode-shell/src/shell/objectdialog/colours.rs` (add `names`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs:861-875` (`enter_column_stage`), add `refresh_color_choices`
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Draft::refresh_color_options` near `enter_column` ~1425; tests module)
- Modify: `crates/geode-shell/src/shell/hot_reload.rs` (call after the `pickable_changed` block)
- Test: `objectdialog/mod.rs` tests, `tests/object_stack.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `dialog::parked_objects_mut`, Task 1 test helpers.
- Produces: `views::color_options(colors: &[String], current: &str) -> Vec<String>`; `colours::names(config: &Config) -> Vec<String>`; `Draft::refresh_color_options(&mut self, colors: &[String])`; `render::refresh_color_choices(shell: &mut ShellView)`.

- [ ] **Step 1: Write the failing pure tests**

In the `objectdialog/mod.rs` tests module, beside `entering_a_column_swaps_the_fields_and_leaving_restores_them_with_the_fold`:

```rust
    /// A column draft on `npv` with the named colors `colors`, as the column door
    /// builds it.
    fn npv_column_draft(colors: &[String]) -> Draft {
        let config = config_with_view_and_datasets();
        let mut draft = Domain::Views.draft(&config, "tree");
        let npv = draft
            .list_items("columns")
            .unwrap()
            .iter()
            .find(|i| i.name == "npv")
            .unwrap()
            .clone();
        draft.column_ctx = Some(views::column_context(&draft, "npv", npv.clone()));
        assert!(draft.enter_column(
            "npv",
            views::column_fields(&npv, colors, Destination::Presentation)
        ));
        draft
    }

    fn color_field(draft: &Draft) -> (Vec<String>, String) {
        let field = draft.fields.iter().find(|f| f.key == "color").unwrap();
        let FieldKind::Choice { options, selected } = &field.kind else {
            panic!("color is a choice");
        };
        (options.clone(), options[*selected].clone())
    }

    /// A color created elsewhere joins the options; the selection stays by name;
    /// and the draft stays clean, because the baseline moved with the fields.
    #[test]
    fn refreshing_color_options_keeps_the_draft_clean() {
        let mut draft = npv_column_draft(&["delta".to_string()]);
        let i = draft.fields.iter().position(|f| f.key == "color").unwrap();
        draft.selected = i;
        // Step until `delta` is selected, then save so the baseline holds it.
        while color_field(&draft).1 != "delta" {
            assert_eq!(draft.step_selected_forward(), Step::Changed);
        }
        draft.mark_saved();

        draft.refresh_color_options(&["delta".to_string(), "ember".to_string()]);
        let (options, selected) = color_field(&draft);
        assert!(options.contains(&"ember".to_string()), "{options:?}");
        assert_eq!(selected, "delta");
        assert!(!draft.is_dirty(), "a refresh is not an edit");
    }

    /// The selected color was deleted: it stays listed as an extra option, as an
    /// unknown configured color does, so the value remains visible and repairable.
    #[test]
    fn refreshing_keeps_a_removed_selected_color_as_an_extra_option() {
        let mut draft = npv_column_draft(&["delta".to_string()]);
        let i = draft.fields.iter().position(|f| f.key == "color").unwrap();
        draft.selected = i;
        while color_field(&draft).1 != "delta" {
            draft.step_selected_forward();
        }
        draft.refresh_color_options(&[]);
        let (options, selected) = color_field(&draft);
        assert_eq!(selected, "delta");
        assert_eq!(options.last().map(String::as_str), Some("delta"));
    }

    /// An open color typeahead lists the new option and keeps its typed query.
    #[test]
    fn refreshing_rebuilds_an_open_color_typeahead() {
        let mut draft = npv_column_draft(&["delta".to_string()]);
        draft.selected = draft.fields.iter().position(|f| f.key == "color").unwrap();
        assert_eq!(draft.begin_choice_entry(), Step::Changed);
        draft.set_query("em".to_string());
        draft.refresh_color_options(&["delta".to_string(), "ember".to_string()]);
        let list = draft.choice.as_ref().unwrap();
        assert!(list.options().contains(&"ember".to_string()));
        assert_eq!(list.query(), "em");
        assert_eq!(list.highlighted_text(), Some("ember"));
    }
```

Use whichever existing forward-step entry point the tests module already uses for a `Choice` row (search the module for `step_selected(`; if the method takes `StepDirection`, call `draft.step_selected(StepDirection::Forward)`), and whichever draft query setter it uses (`set_query` on `Draft` exists per `ObjectDialogState::set_query`'s docs; if the draft method has a different name, use it). If `choice.set_query` needs the query mirrored explicitly, call `draft.choice.as_mut().unwrap().set_query("em")` in the test setup instead.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell refreshing_`
Expected: compile error, `refresh_color_options` not found.

- [ ] **Step 3: Extract the option list and the names**

In `views.rs`:

```rust
/// The options a column's `color` choice offers: the reserved names, then the named
/// colors, then `current` if neither lists it — an unknown or removed color stays
/// visible and repairable. The one builder of this list, shared by
/// [`column_fields`] and [`Draft::refresh_color_options`] so they cannot drift.
pub fn color_options(colors: &[String], current: &str) -> Vec<String> {
    let mut options: Vec<String> = geode_core::colour::RESERVED_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    options.extend(
        colors
            .iter()
            .filter(|name| !geode_core::colour::RESERVED_NAMES.contains(&name.as_str()))
            .cloned(),
    );
    if !options.iter().any(|option| option == current) {
        options.push(current.to_string());
    }
    options
}
```

and in `column_fields` replace the inline list with:

```rust
    let current_color = color_key(&effective.colour);
    let color_options = color_options(colors, &current_color);
```

In `colours.rs`:

```rust
/// The configured named colors, in declared order.
pub fn names(config: &Config) -> Vec<String> {
    config
        .doc(DOC)
        .map(|doc| {
            geode_core::colour::NamedColours::from_doc(doc)
                .0
                .names()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
```

and in `render.rs` `enter_column_stage` replace the inline `colours` computation with `let colours = colours::names(config);`.

- [ ] **Step 4: `Draft::refresh_color_options`**

In `objectdialog/mod.rs`, `impl Draft`, after `enter_column`:

```rust
    /// Rebuild every `color` choice's options from `colors`, keeping each selection by
    /// name. The baseline moves with the fields, so a refresh never reads as an edit. An
    /// open color typeahead is rebuilt over the new options with its query and
    /// highlighted option kept.
    pub fn refresh_color_options(&mut self, colors: &[String]) {
        for fields in [&mut self.fields, &mut self.baseline] {
            for field in fields.iter_mut().filter(|field| field.key == "color") {
                if let FieldKind::Choice { options, selected } = &mut field.kind {
                    let current = options.get(*selected).cloned().unwrap_or_default();
                    *options = views::color_options(colors, &current);
                    *selected = options
                        .iter()
                        .position(|option| *option == current)
                        .unwrap_or(0);
                }
            }
        }
        let open_color_row = match self.text_entry {
            Some(TextEntry { row: EditRow::Field(i), completions: Completions::Choice }) => {
                self.fields.get(i).filter(|field| field.key == "color")
            }
            _ => None,
        };
        if let (Some(field), Some(list)) = (open_color_row, self.choice.as_mut())
            && let FieldKind::Choice { options, .. } = &field.kind
        {
            let keep = list.highlighted_text().map(str::to_string);
            let query = list.query().to_string();
            let mut rebuilt = crate::choice::ChoiceList::new(options.clone(), crate::choice::DEFAULT_CAP);
            rebuilt.set_query(&query);
            rebuilt.place(keep.as_deref());
            *list = rebuilt;
        }
    }
```

Adjust the `TextEntry` pattern to its real field names (`row`, `completions`) if they differ; `begin_choice_entry` constructs it with exactly those.

- [ ] **Step 5: Run the pure tests**

Run: `cargo test -p geode-shell refreshing_`
Expected: PASS.

- [ ] **Step 6: Write the failing GPUI test**

Append to `tests/object_stack.rs`:

```rust
/// The flow the feature exists for: a column needs a color that does not exist yet.
/// Create it in a stacked Colors dialog, come back, and it is among the column's
/// color choices, with the column stage exactly where it was.
#[gpui::test]
fn a_color_created_in_a_stacked_dialog_is_offered_to_the_covered_column(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    let color_options = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        edit_draft(shell, cx, |d| {
            d.fields
                .iter()
                .find(|f| f.key == "color")
                .and_then(|f| match &f.kind {
                    FieldKind::Choice { options, .. } => Some(options.clone()),
                    _ => None,
                })
                .unwrap()
        })
    };
    assert!(!color_options(&shell, &cx).contains(&"ember".to_string()));

    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("n");
    cx.simulate_input("ember");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    // Escape out of Colors, however many stages it takes, and no further.
    for _ in 0..3 {
        if domains(&shell, &cx) == vec![Domain::Views] {
            break;
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
    assert!(color_options(&shell, &cx).contains(&"ember".to_string()));
    assert!(!edit_draft(&shell, &cx, |d| d.is_dirty()), "and the column is not dirtied");
}
```

Run: `cargo test -p geode-shell a_color_created_in_a_stacked_dialog`
Expected: FAIL on the `ember` assertion.

- [ ] **Step 7: Refresh after every applied reload**

In `render.rs`, next to `deliver_values`:

```rust
/// After a reload, every open object dialog's `color` choices follow the configured
/// named colors, including a dialog covered by the Colors dialog that just created
/// one.
pub(in crate::shell) fn refresh_color_choices(shell: &mut ShellView) {
    let names = colours::names(&shell.services.config);
    for state in shell
        .object_dialog
        .iter_mut()
        .chain(dialog::parked_objects_mut(&mut shell.modals))
    {
        if let Some(draft) = state.draft.as_mut() {
            draft.refresh_color_options(&names);
        }
    }
}
```

In `hot_reload.rs`, right after the `if pickable_changed { ... }` block (inside the accepted-reload branch, after `self.services.config = new_config;`):

```rust
            // Named colors may have changed; open column stages offer the new set.
            super::objectdialog::render::refresh_color_choices(self);
```

- [ ] **Step 8: Run the tests**

Run: `cargo test -p geode-shell object_stack refreshing_ objectdialog`
Expected: all pass.

- [ ] **Step 9: Mutation entries**

```zsh
run_mutation "object stack: a color refresh leaves the baseline behind" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        for fields in [&mut self.fields, &mut self.baseline] {' \
  '        for fields in [&mut self.fields] {' \
  geode-shell \
  refreshing_color_options_keeps_the_draft_clean

run_mutation "object stack: a reload never refreshes covered color choices" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            super::objectdialog::render::refresh_color_choices(self);' \
  '            {}' \
  geode-shell \
  a_color_created_in_a_stacked_dialog_is_offered_to_the_covered_column
```

Run `--anchors-only`, commit, then `zsh scripts/mutation-check.sh "object stack"`; expect all caught.

- [ ] **Step 10: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): a covered column stage offers colors created above it"
```

---

### Task 4: Documentation and full verification

**Files:**
- Modify: `docs/current/input-and-dialogs.md` (stack paragraph ~54-64, "Known limitation" ~82-84, palette section ~163-168)
- Modify: `crates/geode-shell/README.md` (the `open_shell_dialog` rule, ~line 89-94)

- [ ] **Step 1: Update `docs/current/input-and-dialogs.md`**

In the stack paragraph, after "Opening a kind already on top does nothing; …", add:

> Object dialogs are the exception: they stack per domain. Over a Views dialog, Colors, Scopes, or any other domain pushes; the covered dialog's whole state and scroll offset are parked in its own stack entry and come back when the cover pops, so the trader returns to the same stage, row, open field, and caret. The same domain never nests (two drafts of one file would race each other's writes): requesting it from the top is a no-op, and from lower in the stack posts "views is already open underneath". A covered object dialog still receives its values replies and reload refreshes, and its `color` choices follow named colors created above it. A failed write rebuilds only the drafts that contributed edits to the failed batch.

Replace the "Known limitation" paragraph with:

> Known limitation: the three `choicedialog` pickers (grouping, tile kind, log level) share one kind and one state field, so a second one cannot open while one is anywhere in the stack.

- [ ] **Step 2: Update `crates/geode-shell/README.md`**

Change "Openers check `dialog::can_open` before installing state: a duplicate kind would overwrite the covered dialog's draft." to:

> Openers check `dialog::can_open` before installing state: a duplicate kind would overwrite the covered dialog's draft. Object dialogs check `dialog::can_open_object` instead; they stack per domain, and `objectdialog::render::open` parks the covered state in its stack entry (`ShellModal::parked_object`) before installing its own. Code that must reach a covered object dialog (deliveries, reload refreshes, write reverts) iterates `object_dialog` plus `dialog::parked_objects_mut`.

- [ ] **Step 3: Full verification**

Run each; all must succeed:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed
```

`--changed` covers every existing entry anchored in the touched files, including the `a_kind_already_in_the_stack_is_refused` entry, whose anchor in `can_open` is untouched. Expect every entry `caught`; any `SURVIVED`, `BUILD`, or `FILTER` must be fixed before merge. Run it detached and read the log if it will exceed a few minutes.

- [ ] **Step 4: Commit**

```bash
git add docs/current/input-and-dialogs.md crates/geode-shell/README.md
git commit -m "docs: object dialogs stack per domain"
```

- [ ] **Step 5: Display check (for the user)**

Record for the user's screen: `cargo run -p geode-app -- --demo`, open Edit views…, a view, a column stage, `ctrl-k` Edit colors…, create a color, Escape back. Expect: the Views dialog on the same column row, the new color in the Color choice, and no flicker of the Views panel between.
