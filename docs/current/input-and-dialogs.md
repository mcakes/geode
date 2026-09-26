# Input and dialogs

The shell routes keys according to the surface that owns keyboard focus before
consulting the ordinary keymap matcher. Dialog state owns the query, selection,
and editing stage; GPUI inputs and scroll handles reflect that state. This guide
describes those boundaries and the behavior of the shell's transient surfaces.

See [keymaps and actions](keymaps.md) for binding resolution and
[configuration dialogs](configuration-dialogs.md) for object drafts and writes.

## Keyboard ownership

[`shell/input.rs`](../../crates/geode-shell/src/shell/input.rs) checks owners in
this order:

| Owner | Routing |
|---|---|
| Shell modal or component dialog | Excludes the ordinary matcher. A shell modal's handler gets first refusal; unclaimed Escape closes it. |
| Focused per-tile command line | Handles its own keys. The effective palette toggle remains available and cancels the line. |
| Focused scope text field | Typing bypasses the matcher. Single-key chords resolve against the workspace context only. |
| Occupant holding focus in insert mode | Single-key chords use the whole context stack; bare keys use only contexts carrying `mode == insert`. |
| Palette toggle and open palette | The toggle resolves directly against the effective single-key binding; an open palette owns remaining keys. |
| Stack member list | Consumes non-chord keys. Chords fall through to ordinary matching. |
| Active tile or divider drag | Escape stops the drag before ordinary matching. |
| Ordinary keymap | Resolves against workspace, then tile and occupant contexts when present. |

Here a chord carries Control, Alt, or Command; Shift alone still counts as
typing. Input components can consume their own editing shortcuts before the
shell listener runs. Single-key resolution respects layer order and explicit
unbindings without feeding matcher sequences or numeric counts. An unbound
scope-field chord is consumed; insert-mode handling leaves an unbound key to
the input.

Claimed modal keys stop propagation so they cannot also insert text into the
shared input. Unclaimed printable keys continue to the input. After a modal
handler returns, the shell synchronizes text and focus even if the handler did
not claim the key. A closed modal's closer owns focus instead.

Actions from the palette and ordinary matcher share `ShellView::dispatch`.
Workspace actions mark session state dirty and reconcile structural focus;
unhandled action ids are offered to the focused occupant. Counts reach stack
cycling and module dispatch; the general workspace router ignores them.

## Modal lifetime and focus

[`shell/dialog.rs`](../../crates/geode-shell/src/shell/dialog.rs) owns opening,
closing, shared input synchronization, and common list and field rendering.
Open through `open_shell_dialog` or its keyed wrapper. The opening path clears
competing transient state and pending key sequences. A mouse-opened dialog
uses `prevent_default` so the click's default focus behavior cannot undo the
focus assigned by the opening path.

Render closures receive a borrowed shell and must not re-enter its entity.
Keyboard and pointer transitions mutate the dialog model, then reconcile the
shared input through `sync_dialog_text`. Pointer handlers must run that step
themselves because they do not pass through the keyboard handler's tail.
Writing an input value does not emit `InputEvent::Change`; model mutations
cannot depend on such an event to keep text synchronized.

Normal/Filter dialogs use the [shared filter contract](shell.md#dialog-filtering):
Escape restores the entry query, bare Enter keeps the typed query, and neither
exit activates a row. Value fields, naming, Settings choices, and keybinding
capture have their own key handling. Filter-only pickers below also retain
their own apply and cancel behavior.

Settings updates its shell preference when a value is chosen or stepped, then
submits the user-layer write. Propagation to modules follows each setting's
global or reload path; the row updating is not proof that every open module
has adopted the change. Keybinding edits instead become effective through the config
watcher's reload; the editor derives rows from the current registry and keymap.
Neither surface's background write acknowledgement is proof that the write
completed. See [configuration writes](configuration.md#runtime-edits) and
[keybinding editing](keymaps.md#editing-unbinding-and-reset) for failure and
layering details.

The default-source row derives its options from the same
`SeriesSettings` global read by timeseries tiles: `(none)` followed by the
configured fetch sources in document order. An absent global supplies no
sources; the row still offers `(none)`. These are configuration entries, not a
check that each adapter started successfully.

## Filtering, choice, and movement

[`listfilter`](../../crates/geode-shell/src/listfilter.rs) ranks labels without
changing their source identity. An empty or whitespace-only query returns all
rows in declared order. Consumers keep selection in ranked-row space and
resolve the source row before acting.

Every fuzzy surface (list filters, choice lists, the palette, command-line
completion) shares the palette matcher. A single word is a case-insensitive
subsequence match rewarding prefix, word-start, and consecutive characters. A
query of several whitespace-separated words matches when every word matches,
in any order, on characters of its own, so `scope clear` finds "Clear scope"
and `scope scope` does not. Words in the typed order earn a bonus and rank
above the same words reversed. When two words' best matches collide, the words
are placed again one at a time, longest first, on free characters; that
placement is greedy and can miss a fit another assignment would find.
Command-line completion ranks a single word, so it never takes this path.

[`ChoiceList`](../../crates/geode-shell/src/choice.rs) owns options, query,
ranking, and highlight. Re-ranking preserves the highlighted option by its
text when still present; duplicate labels resolve to the first match. The
option text therefore serves as identity within a choice list. Tab completes
the highlighted option and Enter chooses it through `choice::route`.

The choice model retains every match. Its `painted` slice is a moving window,
with a default capacity of twelve, that follows the highlight. Windowed
consumers use window-relative indices; the shared dialog renderer uses the
full ranked list in a bounded scrolling viewport and full ranked indices.
These two index spaces must not be mixed when handling clicks or scrolling.

[`vimnav::apply`](../../crates/geode-shell/src/vimnav.rs) is the common movement
rule: a move of exactly one row wraps at either end, larger moves clamp, and
an empty list stays at zero. Filter-safe keys are separate from letter-based
normal-mode commands so typing `j` or `k` into a filter cannot move a row too.
The standalone `VimListNav` count parser does not share the keymap matcher's
9999 count cap. `vimfind` supplies pure find-state helpers; current modal
filters do not use it as their controller.

## Palette and which-key

[`palette`](../../crates/geode-shell/src/palette.rs) matches action title and
category, not action id. Category matches receive half weight, rounded up.
Usage contributes a bounded frequency/recency bonus; the usage snapshot and
ranking time are fixed when the palette opens so rows do not move merely as
time passes. Equal scores retain item order. Unlike `listfilter`, palette
queries are not trimmed to turn whitespace-only input into an empty filter.

Palette action rows, including grouping slots, use the same shell dispatch
path as a binding. Additional theme and live saved-scope rows have their own
dispatch targets. A registered saved-scope action and a live saved-scope row are distinct
items with distinct usage keys. Live rows allow a newly reloaded scope to be
selected even if no corresponding action was registered at startup.

Binding badges are indicative: the palette takes the first compiled binding
for an action without resolving current contexts or later shadows. They are
not a guarantee that pressing the displayed key currently dispatches that row.

[`whichkey`](../../crates/geode-shell/src/shell/whichkey.rs) is likewise a hint
view, not a second matcher. For each next key it favors the shortest remaining
sequence and then the last equal-length entry, suppressing a winning `none`.
Hints for longer continuations do not guarantee the eventual dispatch;
the matcher still decides after each key and context change.

### Displaying keys

Every key shown as a key is gpui-component's `Kbd`, reached through
[`shell::kbd`](../../crates/geode-shell/src/shell/kbd.rs). That covers dialog
footer hints, keybinding rows, tooltips, palette binding badges, which-key
continuations, the status bar's pending keys, empty-state and section hints,
and module menus and footers. `Kbd` owns the label and look: platform glyphs
on macOS (`⌃⇧P`, `⎋`) and `Ctrl+Shift+P` elsewhere, with the key capitalised,
so `g` reads `G` and `shift+g` reads `⇧G`.

- A hint line that names keys inside prose writes them between backticks
  (``"double-click or `ctrl+k` → Add a tile"``); `kbd::marked` paints each
  backticked run as chips and the rest as text.
- A module menu hint stored as a keymap spec goes through `kbd::menu_spec`.
  A `:` command-line verb stays text because it is not a key, and so does a
  spec naming `mod`, because the alias is the user's.
- A menu's trailing lane paints keys the way gpui-component's `PopupMenu`
  does: the label without the chip's fill or padding, in the lane's color,
  so a highlighted row's keys follow the highlight.
- A key named inside a sentence (a notice, a confirmation, a refusal) keeps
  the keymap's lowercase spelling from `palette::render_binding`, since that
  is what a user types into a keymap file.

Hardcoded hints name the shipped key. A user rebinding does not change the
empty-state hint's `ctrl+k` or a module menu's hint; the palette, keybinding
rows, tooltips and the timeseries footer and menu read the live keymap.

## Per-tile command and find lines

[`commandline`](../../crates/geode-shell/src/commandline.rs) tracks the prompt
kind, owner tile, cached token range, vocabulary, candidates, and completion
highlight. The GPUI input owns text and caret position. The shell captures the
owner tile at open time and routes commits back to it. `:` commands are
local to that tile; frame and application changes use registered actions.

Enter runs an exact, case-sensitive vocabulary match, or runs when the current
token is empty or has no candidates. A unique candidate is expanded and executed in the same Enter press;
multiple candidates require a choice. Tab replaces the cached token range and
cycles the full candidate list without rebuilding it after each completion.
The popup displays only the first eight candidates, so cycling can move the
highlight beyond the displayed rows. The popup has no pointer selection.
Enter and Escape are recognized by key name regardless of modifiers.

The two prompt types have different completion and dismissal semantics:

| Prompt | Enter | Escape or overlay cancellation | Input blur |
|---|---|---|---|
| Command (`:`) | Execute; a reported error keeps the prompt open | Close without execution | Cancel |
| Find (`/`) | Send `FindEvent::Committed`, including for empty text | Send `FindEvent::Cancelled` | Commit nonempty text; cancel empty text |

Find edits send `FindEvent::Changed` to the owner as typing proceeds. The
module owns how those events affect its selection and search state. Opening
the palette remains available through its effective toggle binding and uses
the command line's cancellation path.

## Scope text and stack selection

The toolbar scope field edits `Frame::scope().text` live. Focus captures the
entry text and opens a scope session; text changes coalesce into one undo
entry. Enter and blur keep the result and end the session. Escape restores
the entry text while still inside the session, then ends it and returns focus
to the shell. This ordering prevents the abandoned text from becoming an undo
target. A shell chord that changes the frame while the field remains focused
reflects the resulting text back into the field.

The stack member list has no text input. It opens on the active member,
wraps with `j`/`k` or arrows, activates a numbered member immediately, and uses
Enter for the highlighted member. Escape dismisses it without activation.
Other non-chord keys are consumed. A dispatched action closes the list; a
chord that produces no dispatch can leave it open.

Escape during a tile drag cancels the pending move. Escape during a divider
drag retains the geometry already applied and stops tracking, marking the
session dirty. These paths run after modal and palette handling, so an overlay
retains priority for its own Escape behavior.

## Dimension picker

[`shell/picker.rs`](../../crates/geode-shell/src/shell/picker.rs) is a
filter-only Columns → Values dialog. Opening for a known column skips Columns;
an unknown column falls back to the column list. Values requests capture the
scope and as-of at request time, excluding the column's own dimension selection
so counts reflect the other filters. The current selection is pre-ticked.

Distinct requests carry tags allocated across modal sessions. Delivery requires
Values to still be open for the same column and tag. Returning to Columns or
closing the modal makes old deliveries inapplicable but does not cancel the
underlying request. Values are displayed in a virtualized list.

Tab toggles the highlighted value. Ctrl+A adds all filtered values without
clearing ticks outside the filter; Ctrl+X clears every tick. A row click moves
the highlight, while a tick click or a row double-click toggles that value, as
Tab does (the double-click's second press toggles; a third press does not
toggle back). Enter applies to the
current frame scope, preserving its other fields:

- Nonempty ticks, including pre-ticks, are authoritative. Moving the highlight
  alone does not replace them.
- An untouched empty tick set uses the highlighted value when one exists.
- An explicitly emptied set, or an empty set with no highlighted fallback,
  removes the column constraint.

Enter is not gated on loading, success, or a nonempty filtered list. While
loading or after a query failure it can still apply pre-ticks or remove the
column when no fallback exists. Escape from Values discards the stage's query,
results, and ticks and returns to the previous column. Escape from Columns
closes the dialog. Neither Escape step applies the draft.

## As-of selector

[`shell/asof_rows.rs`](../../crates/geode-shell/src/shell/asof_rows.rs) owns a
filter-only list in fixed section order: Current and Live while pinned,
business-day presets, Custom, then recent publishes. Ranking matches labels,
not the timestamp column, and preserves section order. With an empty query,
bare digits 1–5 commit presets. Enter commits the highlighted row; no match
leaves the dialog open.

Tab opens Custom, clears the query, and seeds the segmented field from the
highlighted instant, then the current pin, then the captured current time.
It truncates fractional seconds and selects the Day segment. While Custom is
open, non-chord keys belong to the segmented field. Enter completes pending
digits and resolves the local time; incomplete segments and nonexistent local
times remain as inline errors. Escape or Tab closes the field without
restoring the old query. Escape with no field open closes the modal. Clicking
another row closes Custom and commits that row instead.

A global data-version change refreshes the rows while preserving the query
and open custom field. Selection is restored by displayed section, label, and
timestamp text, which are not guaranteed to uniquely identify a full timestamp.
The clock is captured at open and is retained across refreshes; reopening is
needed to adopt a changed clock configuration. Commits update the frame's as-of
and close the dialog. As-of undo swaps with the previous value rather than
walking a history stack.

## Grouping, tile, and log choices

[`shell/choicedialog.rs`](../../crates/geode-shell/src/shell/choicedialog.rs)
uses a filter-only `ChoiceList` with a target-specific commit. Tab completes,
Enter or a row click commits, and Escape closes, except for the nested log
level stage.

| Target | Rows and commit |
|---|---|
| Grouping | View default, then filled slots 1–9; opens on the active choice. Empty-query digits commit directly, with zero choosing the default. Unfilled digits are consumed. Commit rechecks slot existence, reports removal if needed, then closes. |
| Tile kind | Roster order excluding the placeholder. Closes before adding to the tile focused at commit time: fills a placeholder or splits a real tile using the configured placement. |
| Log level | Choose a logging target, then its level. Escape from levels returns to a rebuilt target list and clears the filter; a level choice submits `Diagnostics::request_level`. |

Closing and reopening creates fresh dialog state. These pickers apply on
Enter; they do not use Normal/Filter mode's keep-query Enter.

## Frame expression

[`shell/scope_expr_view.rs`](../../crates/geode-shell/src/shell/scope_expr_view.rs)
edits the frame's expression in one of three modes, chosen by the door that
opens it. Bare Enter trims and parses the draft in every mode.

| Mode | Opened by | Seed | Enter | Empty Enter |
|---|---|---|---|---|
| Scope expression (whole) | `frame::scope_expression` | The whole expression | Replaces the expression | Clears it |
| Edit scope term | A click on a toolbar term chip | That top-level `and` term | Replaces that term; the other terms keep their order | Removes that term |
| Add scope expression | `frame::add_expression`, the `+` menu's "Expression…" row | Empty | Joins it to the current expression with `and`, or sets it when there is none | Closes without a change |

The term and add modes show a muted note under the field saying what the
commit touches. A successful commit changes only the expression in the frame
scope, through undoable `set_scope`, then closes. Parse errors remain inline
in every mode and typing clears the error. The term dialog remembers the term
it was seeded with; if the scope changed while it was open so that its index
no longer holds that term (gone, or a different term in its place), an edit
or an empty (removing) commit refuses inline rather than touch whichever term
now has that index. Escape applies nothing.
`frame::clear_expression` drops the whole expression layer without a dialog;
with no expression it does nothing. Neither new action has a default chord.

The toolbar's `+` opens a two-row menu, "Dimension…" (`frame::pick`) and
"Expression…" (`frame::add_expression`), each row showing its action's live
binding through `kbd::menu_binding`. It owns the keyboard while open: `j`/`k`
or the arrows move with wrap, Enter commits the highlighted row, Escape
closes, and other bare keys are consumed. A chord passes to the matcher, and
any dispatch closes the menu. A row click commits; a press of any button
anywhere else closes the menu and reaches nothing beneath it, and while the
menu is open the wheel does not reach the tiles beneath it either. Escape and
an outside press cancel any chord prefix typed while the menu was open. A
commit is a dispatch of the row's action, so the menu opens exactly what the
palette row would. Opening the menu takes the shell root's focus; if the
scope text field held focus, the menu's own close returns it there, and so
does closing the dialog or picker one of its rows opened.

Validation is syntax-only. The editor has no dataset against which to check
column names, types, or operator compatibility, so an accepted expression may
still fail when a tile queries its dataset.
