# Dialog stack — design

Date: 2026-09-26. Status: approved in conversation, awaiting written-spec review.

## 1. Intent

Today the shell holds one modal. Every dialog opener refuses when one is
already open, so the only way to reach another dialog is to close the
current one first. The desk wants dialogs to stack: opening a dialog while
one is open pushes the new one on top and makes it active. Dismissing the
top dialog, by committing it (Enter) or leaving it (Escape), reveals the
dialog beneath exactly as it was left.

Success means:

- a dialog can be opened from inside another dialog, or by a shell chord or
  palette entry while a dialog is open;
- Enter and Escape on the top dialog pop exactly one level;
- the revealed dialog has its query, cursor, mode, and focus back, driven by
  its pure state;
- closing the last dialog returns focus to wherever the first one was
  opened from, as closing a dialog does today.

## 2. Rulings

1. **One instance per kind.** A dialog kind appears at most once in the
   stack. A request for a kind already on top does nothing. A request for a
   kind lower in the stack is refused with a status notice naming the kind,
   for example "settings is already open underneath" or "a configuration
   dialog is already open underneath". Nothing is discarded. The three
   pickers built on `shell::choicedialog` (tile kinds, grouping, log level)
   share one kind, `Choice`, because they share one state field.
2. **Only the top dialog is live.** It alone paints and receives keys and
   pointer events. Lower dialogs are hidden but keep their state.
3. **A commit affects only its own dialog.** Enter applies the top dialog's
   own effect and pops it. The dialog beneath is not handed a value. It
   re-renders from whatever the effect changed, for example the frame, the
   config, or the keymap.
4. **Chords reach through only to open dialogs.** With a dialog open, a
   ctrl/alt/cmd chord the dialog does not claim is resolved against the
   `workspace` context. The shell dispatches it only when the resolved
   action is marked `opens_dialog` or is the palette toggle. Every other
   chord stays inert, as it is today.
5. **The palette reaches everything.** The palette can open over a dialog
   and lists every action. Choosing an action that opens a dialog pushes
   that dialog. Choosing any other action runs it behind the stack: the
   stack stays open, and the action's effect shows once the stack is
   dismissed. This deliberately differs from ruling 4. The palette is the
   route for running a non-dialog action while dialogs are open. A stray
   chord is not.
6. **No depth indicator.** The top dialog paints its own title row only.
   There is no breadcrumb or stack count.

## 3. Stack model

`ShellView::modal: Option<ShellModal>` becomes `modals: Vec<ShellModal>`.
Each `ShellModal` gains a `kind: DialogKind` field, where `DialogKind` is one
of `Settings`, `Keybindings`, `Picker`, `AsOf`, `ScopeExpr`, `Choice`, or
`Object`. The per-kind state fields (`settings`, `keybindings`, `picker`,
`as_of_dialog`, `scope_expr_dialog`, `choice_dialog`, `object_dialog`)
remain. A kind's field is `Some` exactly while that kind is in the stack.

### Opening

`open_shell_dialog` / `open_shell_dialog_with_key` take the kind and do the
following in order:

1. If the kind is on top, return. If it is lower in the stack, post the
   ruling-1 notice and return. This single check replaces the per-opener
   `if view.modal.is_some() { return }` guards.
2. Cancel the matcher, the command line, and the add-filter menu, as today.
3. Close the palette. When the palette was open over a dialog, this returns
   focus to nothing: the new dialog takes focus in step 6.
4. Record `overlay_return_to_filter` only when the stack is empty before
   this push. Later pushes must not overwrite where the first dialog came
   from.
5. Push the entry. Clear the shared `dialog_input`.
6. Call `prevent_default`, then `sync_dialog_text`, as today.

Openers install their state before calling the opener, as today. Because
the kind check now happens inside the opener, an opener must not install
state for a kind that is already present. Otherwise it would overwrite the
live instance's state. Each opener therefore checks first through a
`dialog::can_open(view, kind)` helper, and the opener's own refusal is the
backstop.

### Closing

`close_modal` becomes a pop:

1. Pop the top entry and clear only its kind's state field. That kind's
   scroll handles and pending-delivery tags reset where they reset today.
2. If an entry remains, call `sync_dialog_text` for the new top. It rewrites
   the shared input's text from that dialog's state and chooses focus.
   Filter-only kinds (`Picker`, `Choice`, `ScopeExpr`) focus the input.
3. If the stack is empty, call `return_focus_from_overlay`, as today.

Every existing `close_modal` caller (commit paths, Escape fallback,
title-row close button) therefore pops one level. No caller needs to clear
the whole stack. If one is found during implementation, it gets its own
explicit `close_all_modals`.

### Top-entry helpers

`set_title_extra`, `set_back`, `back_available`, and `step_back` act on
`modals.last()`. A `top_kind()` accessor exposes the top kind to routing.

## 4. Shared input and focus

There is still one retained `dialog_input`, reused at every depth. The pure
dialog state stays the source of truth, so revealing a dialog only has to
rewrite the input from that state.

Two routers currently infer the owner from a fixed field order. That order
was harmless while the fields were mutually exclusive. With a stack it
picks the wrong owner. Both routers switch to `top_kind()`:

- `sync_dialog_text` (`shell/dialog.rs`) selects the owner by top kind.
- The `dialog_input` `Change` subscriber (`shell/mod.rs`) routes a typed
  query to the top kind's state. Without this, typing in a `Choice` pushed
  over an `Object` dialog would filter the hidden object dialog.

`enter_filter_by_mouse` and the frozen filter row route by top kind in the
same way.

## 5. Key routing

In `handle_key_down`'s modal branch (`shell/input.rs`):

1. Offer the key to the top entry's `on_key`, then run `sync_dialog_text`
   while an entry remains, as today.
2. If the handler declines and the keystroke is a chord (ctrl/alt/cmd;
   shift alone is typing), resolve it against `[workspace]`. Dispatch it
   if it is the effective palette toggle or an action whose `ActionDef` has
   `opens_dialog`. Then stop propagation.
3. Otherwise, an unclaimed Escape pops one level through `close_modal`.

`dialog::opens_dialog(&ActionId) -> bool` names the actions whose dispatch
opens a shell dialog. Only `ShellView::dispatch` opens shell dialogs, so the
list lives beside that dispatch table rather than on `ActionDef`.
`opens_dialog_matches_what_dispatch_pushes` dispatches every registered action
over a base modal and requires flagged ⇔ pushed.

Component dialogs (`window.has_active_dialog`) keep their separate handling
and are out of scope.

## 6. Palette over a dialog

- The palette toggle works while dialogs are open (ruling 4). The palette
  renders above the top dialog and owns keys while open, as it does today.
- The palette lists every action (ruling 5).
- Choosing an `opens_dialog` action closes the palette and pushes the
  dialog. Choosing any other action closes the palette, dispatches the
  action, and returns focus to the top dialog through `sync_dialog_text`.
- Escape closes only the palette, and focus returns to the top dialog.
- `close_palette`'s focus return becomes stack-aware. With a dialog open it
  calls `sync_dialog_text`. With no dialog open it uses
  `return_focus_from_overlay`. Palette-opened-over-a-dialog must not consume
  `overlay_return_to_filter`, which belongs to the stack's base.
- A non-dialog action that closes or replaces dialog state is the risk case.
  Implementation audits palette-reachable actions for any that call
  `close_modal` or write a per-kind state field, and records the outcome in
  the plan.

Audit result: every `close_modal` call and every per-kind state write sits
inside a dialog's own handler, a `can_open`-gated opener, or
`clear_dialog_state`, so no palette-reachable non-dialog action closes or
replaces dialog state. The tile-kind picker resolves its target tile at
commit, not at open, so it does not need to read or hold dialog state either.

## 7. Rendering and guards

- Only `modals.last()` paints its panel, title, title extras, and Back
  button.
- The `GeodeModalOpen` key context, the drag guards, the add-filter guards,
  and the render guards that read `modal.is_some()` all read
  `!modals.is_empty()`.
- Whether the palette paints above a dialog depends only on the palette
  panel's z-order in `render.rs`. It must be deferred or ordered after the
  modal layer.

## 8. Async deliveries

A covered dialog keeps its state, so a delivery for it (picker distinct
values, scope-expression suggestions, as-of rows) applies when it arrives.
The revealed dialog shows the result. Delivery tags become inapplicable
only when their own kind is popped, not whenever any dialog closes. The
plan lists every delivery path that currently keys applicability off
"a modal is open" and moves it to "this kind is present".

## 9. Testing

GPUI window tests, driven through production key and pointer routes:

- push from inside a dialog (a row or verb that opens another dialog);
- push by a flagged chord; an unflagged chord is inert behind a dialog;
- Escape pops one level; a commit pops one level; each restores the
  revealed dialog's query, caret, mode, and focus;
- typing after a pop filters the revealed dialog, not a stale one;
- a same-kind request on top is a no-op; one lower down posts the notice
  and leaves the stack unchanged;
- palette over a dialog: an `opens_dialog` entry pushes; a non-dialog entry
  runs with the stack intact and focus back on the top dialog; Escape
  returns focus to the dialog;
- the last pop returns focus to the scope-bar field when the first dialog
  was opened from it, including when a palette was opened and closed
  mid-stack;
- a delivery for a covered dialog lands and shows when it is revealed.

Mutation harness entries:

- `close_modal` clears every kind instead of the top kind;
- `sync_dialog_text` ignores `top_kind()`;
- the `Change` subscriber ignores `top_kind()`;
- the `opens_dialog` gate in the modal chord branch;
- `overlay_return_to_filter` is rewritten on a nested push;
- the same-kind refusal.

## 10. Documentation

Update `docs/current/input-and-dialogs.md` (modal lifetime and focus, the
keyboard-ownership table, and the palette), `docs/current/shell.md`, and the
`geode-shell` README module notes for `dialog.rs`. Remove the "Allow dialogs
on top of dialogs" line from `TODO.md`.

## 11. Out of scope

- Two instances of one kind (would need per-entry state).
- Returning a value from a top dialog to the one beneath.
- A breadcrumb or depth marker.
- Stacking gpui-component dialogs.
