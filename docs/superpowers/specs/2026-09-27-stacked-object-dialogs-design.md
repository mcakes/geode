# Stacked object dialogs

Status: approved in conversation 2026-09-27; spec awaiting review.

## 1. Why

The palette opens over a dialog stack, and a dialog-opening row pushes its
dialog. That fails for the case traders actually hit: editing a Views column,
they realize the color they want does not exist yet, open the palette, pick
"Colors", and get "a configuration dialog is already open underneath". Every
object-dialog domain (Views, Groupings, Scopes, Schema, Sources, Colors,
Expressions) shares one `DialogKind::Object` and one `ShellView::object_dialog`
field, so `dialog::can_open` refuses any second one. Their only route is to
close Views, losing the stage, cursor, and open field, edit the color, and
walk back.

The goal: from inside one configuration dialog, open a configuration dialog of
a different domain, edit, commit or cancel, and Escape back to exactly where
they were — same stage, cursor, draft, open field, and caret — with the
covered dialog showing what the stacked one changed.

Rulings (user, 2026-09-27):

1. **Scope.** Object dialogs of *different* domains stack. The same domain
   never nests (Views over Views would give one file two drafts). The Choice
   pickers (grouping, tile kind, log level) keep their one-instance rule; they
   are not part of this change.
2. **Mechanism.** Park and swap: the covered dialog's state moves into its own
   stack entry, and `shell.object_dialog` keeps meaning "the top object
   dialog". Not a per-domain `DialogKind::Object(Domain)` with a lookup at
   every access site.

## 2. Stacking rule

`dialog::can_open` stops treating `Object` as a single kind. An object-dialog
opener asks with its domain:

- no object dialog in the stack, or only ones of other domains → push;
- the same domain already on top → no-op, as today for any kind on top;
- the same domain lower in the stack → refused with a per-domain notice,
  "views is already open underneath" (one `&'static str` per domain, so the
  notice field keeps its type). `is_already_open_notice` recognises these
  notices too, so the close path clears them as it clears the kind notices.

The domain rule lives in one function (e.g. `can_open_object(view, domain)`),
which reads the domain of the top entry from `shell.object_dialog` and of
covered entries from their parked state (§3). `objectdialog::render::open` and
`open_save_scope` both call it; `open_save_scope`'s guard comment is rewritten
for the domain rule. Saving a scope from a Views dialog therefore pushes a
Scopes dialog rather than being refused, and its `begin_naming` touches only
the state `open` just installed.

Every other kind keeps `can_open(view, kind)` unchanged.

## 3. Park and swap

`ShellModal` gains one optional slot:

```rust
/// Set while an object dialog of another domain covers this entry.
pub parked_object: Option<ParkedObject>,
```

`ParkedObject` holds the covered `ObjectDialogState` and the covered dialog's
scroll position (`object_dialog_scroll` is one shared handle; the scroll
offset is saved and restored rather than giving each instance a handle).

- **Push.** When an object dialog opens and `shell.object_dialog` is `Some`,
  the live state is `take`n into the parked slot of the topmost *Object* entry
  before the new state is installed. The entry that owns a parked state is the
  one whose dialog it is — not necessarily the stack top, since a Choice list
  may sit between them. A non-object dialog opening over an object dialog
  parks nothing; `shell.object_dialog` stays in place, inert as today.
- **Pop.** `clear_dialog_state(Object)` restores the next Object entry's
  parked state (and scroll offset) into `shell.object_dialog` instead of
  setting `None`. Restoration happens before `refocus_top`, so
  `sync_dialog_text` reads the revealed dialog's mode and query, and the
  existing `saved_input` path restores the shared input's text and caret.
- **Invariant.** At most one `ObjectDialogState` per domain exists across
  `shell.object_dialog` and all parked slots; `shell.object_dialog` is the
  state of the topmost Object entry, or `None` when there is none.

The ~200 reads of `shell.object_dialog` (render, keys, pointer handlers,
typing) need no change: they already act only when an Object entry is live.

## 4. Deliveries to a covered dialog

The four paths that write to object-dialog state outside the key route must
reach parked states as well. A helper yields the live state and every parked
one (`object_dialogs_mut`).

- **Reload vocabulary rebuild** (`hot_reload.rs`, the `expr_vocab` block):
  rebuild the open expression completion in every object dialog.
- **Distinct-values reply** (`expr_suggest.rs`, the delivery): offer the
  outcome to each dialog with an open expression field; `deliver` already
  matches on column and tag, so at most one accepts it.
- **Scopes values reply** (`render.rs`, `deliver_values`): a Scopes Values
  stage covered by another domain's dialog still owns its `SCOPES_KEY`
  request; the reply goes to the Scopes dialog wherever it is in the stack,
  not only when it is live.
- **Failed-write revert** (`apply.rs`, `revert_failed_write`): today it
  rebuilds whichever draft is on top. `PendingConfigWrite` records every
  domain whose draft contributed edits to the batch (a debounced Views edit
  and a Colors commit can share one batch), and exactly those dialogs — live
  or parked — are rebuilt and given the "could not save" notice. A batch
  queued from outside the object dialog (`queue_object`) rebuilds none.
  Without this, a Colors write failing after the trader returned to Views
  would replace their unsaved Views draft with one rebuilt from config.

`expr_suggest`'s own request path (`completion_mut`, `values_scope`) keys on
`top_kind()` and the live state, which is correct: only the visible dialog
requests values.

## 5. New colors reach a covered draft

A Views or Schema column's color choices are copied into the draft when the
column stage is built (`views::column_fields`). A color created in the
stacked Colors dialog would therefore be missing from the Views column it was
created for.

After every applied reload, each object dialog's draft (live and parked)
refreshes the options of its `color` choice fields from the new config,
using the same list `column_fields` builds, and keeps the selected value by
name. A selected name the reload removed stays as an extra option, as
`column_fields` already does for an unknown configured color. No other field
of the draft changes. The refresh is one function next to `column_fields` so
the two option lists cannot drift.

This covers the committed-then-Escape case and a hot reload from disk alike.

## 6. Failure semantics

- A refused same-domain request changes nothing and posts the notice.
- Closing a stacked object dialog never discards the covered draft; only the
  popped entry's state is dropped.
- A failed write reverts memory for the whole batch (unchanged) and rebuilds
  only the drafts that contributed to it.
- Palette actions that run behind the stack are unchanged.

## 7. Testing

GPUI context tests in `shell/tests/dialog_stack.rs`, driven through real keys
and the palette:

1. Views, column stage on a column → palette "Colors" → the Colors dialog is
   on top; create a color; Escape to Views → same column stage, same row,
   and the new color is among the color choices.
2. An open value field and its typed text in the covered Views dialog
   survive a Colors push and pop (caret included).
3. Views over Views through the palette is refused with "views is already
   open underneath"; the stack depth is unchanged.
4. Views → Choice list → palette "Colors" pushes, and popping back through
   the Choice list reveals the intact Views state.
5. A failed Colors write while Views is covered leaves the Views draft
   untouched and puts the notice on Colors.
6. A distinct-values reply for the covered dialog's open expression field
   lands there, not in the top dialog.

Pure tests for the color-option refresh (selection kept by name, removed
selection preserved).

Mutation-harness entries, each naming its test: the domain comparison in
`can_open_object`; restoring the parked state on pop (replace with `None`);
the revert's domain match; the color-option refresh keeping the selection.

## 8. Documentation

- `docs/current/input-and-dialogs.md`: the stack paragraph gains the domain
  rule and parking; the "Known limitation" paragraph now names only the Choice
  pickers.
- `DialogKind::Object` and `can_open` doc comments; `open_save_scope`'s guard
  comment.
- The `geode-shell` README's dialog invariants.
