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
| Palette over a dialog stack | Takes precedence over the modal handler below: the palette owns keys while it is open, whether or not a dialog is open beneath it. Every action is listed; an `opens_dialog` action pushes, and any other action runs behind the stack and returns focus to the top dialog. Three actions that would open real transient chrome behind the stack (`tile::command_line`, `tile::find`, `stack::pick`) are refused instead, as are workspace switches (`workspace::switch_*`) and the pin toggle (`frame::pin_workspace`) — see [palette and which-key](#palette-and-which-key). |
| Shell modal or component dialog | Excludes the ordinary matcher. A shell modal's handler gets first refusal. A chord it declines is dispatched only if it is bound to a dialog-opening action or the palette toggle; every other chord stays inert. Unclaimed Escape closes the top dialog. |
| Focused per-tile command line | Handles its own keys. The effective palette toggle remains available and cancels the line. |
| Focused scope text field | Typing bypasses the matcher. Single-key chords resolve against the workspace context only, with or without an open page. Escape restores the entry text; Escape and Enter return focus home: the open page's handle, else the shell root. |
| Open page or occupant holding focus in insert mode | Single-key chords use the whole context stack; bare keys use only contexts carrying `mode == insert`. The open page's `holds_focus` is consulted before any tile's. Because the page's own context carries `mode == insert` then, its bare-key bindings must sit in a `mode == normal` table or they fire inside the input; see [keymaps](keymaps.md#context-predicates). |
| Palette toggle and open palette | The toggle resolves directly against the effective single-key binding; an open palette owns remaining keys. |
| Stack member list | Consumes non-chord keys. Chords fall through to ordinary matching. |
| Active tile or divider drag | Escape stops the drag before ordinary matching. |
| Ordinary keymap | Resolves against workspace, then tile and occupant contexts when present. While a page is open the stack is `page`, then the page's own context, then `palette` when open; neither `workspace` nor `tile` is present. |

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
unhandled action ids are offered to the open page, else the focused
occupant. Counts reach stack cycling and module dispatch; the general
workspace router ignores them.

With a [page](shell.md#pages) open, Escape is taken in this order: an open
modal closes and the page stays; the palette closes and focus returns to
the page; the focused scope field takes it, restoring its entry text and
returning focus to the page; a focused page input takes it
through the page's own `mode == insert` binding, which blurs the input back
to normal mode; then the `page` context's `escape` resolves to
`page::close`, which the page sees first and may consume when it has
something of its own to dismiss, and otherwise the shell closes the page.
`mod+d` is the diagnostics page's context-free toggle. Under a modal both
the toggle and `page::close` are refused with the close-the-dialog notice,
because the dialog was opened over the page. The workspace switches
`mod+1` to `mod+9` are context-free, so a switch closes the page from
wherever it is and lands on that workspace's focused tile.

## Modal lifetime and focus

[`shell/dialog.rs`](../../crates/geode-shell/src/shell/dialog.rs) owns opening,
closing, shared input synchronization, and common list and field rendering.
Open through `open_shell_dialog` or its keyed wrapper. The opening path clears
competing transient state and pending key sequences. A mouse-opened dialog
uses `prevent_default` so the click's default focus behavior cannot undo the
focus assigned by the opening path.

Dialogs form a stack (`ShellView::modals`), and only the top entry paints and
takes keys; every entry beneath it is inert and invisible until revealed.
Opening a kind already on top does nothing; requesting a kind already open
lower in the stack does nothing either, and posts "… is already open
underneath" instead of pushing a duplicate or overwriting that entry's state.
Closing — Enter commit, Escape, the close button, or a backdrop click — pops
exactly one level and clears only the popped kind's own state, never a kind
still lower in the stack. The backdrop occludes what it covers, so a click
outside the panel only closes the dialog; it never also reaches a scope-bar
chip or tile beneath.

Object dialogs are the exception to one-per-kind: they stack per domain.
Over a Views dialog, Colors, Scopes, or any other domain pushes, so a trader
editing a column can open Colors from the palette, add the color the column
needs, and Escape back. The covered dialog's whole state and scroll offset are
parked in its own stack entry (`ShellModal::parked_object`) and restored when
the cover pops: the same stage, row, open field, and caret. The same domain
never nests, because two drafts of one file would race each other's writes:
requesting it from the top does nothing, and from lower in the stack posts
"views is already open underneath" (the notice names the domain). A covered
object dialog still receives its values replies and reload refreshes, and its
column `color` choices follow named colors created above it, with the current
selection kept by name and the draft left clean. A failed configuration write
rebuilds only the drafts that contributed edits to the failed batch; a covered
dialog with nothing in it keeps its unsaved draft.

Revealing the covered entry restores the shared input's text and caret to
what they were when it was covered, and gives back its focus: mode dialogs
(Settings, Keybindings, the Object dialog, As-of) resolve focus from their own
state through `sync_dialog_text`; filter-only dialogs (the picker, `Choice`
lists, the scope-expression dialog) always focus the input. The scope field's
return-to-field flag belongs to the base of the stack; a nested push must not
overwrite it with "the dialog beneath had focus". A covered dialog's async
deliveries (a distinct-values reply, a config reload) still apply to its own
state while it is hidden, and show once it is revealed.

Render closures receive a borrowed shell and must not re-enter its entity.
Keyboard and pointer transitions mutate the dialog model, then reconcile the
shared input through `sync_dialog_text`. Pointer handlers must run that step
themselves because they do not pass through the keyboard handler's tail.
Writing an input value does not emit `InputEvent::Change`; model mutations
cannot depend on such an event to keep text synchronized.

Each entry records the workspace active when it was pushed
(`ShellModal::workspace`). Frame dialogs — the dimension picker, as-of,
grouping, the scope picker, the frame expression dialog, and the Scopes
dialog's frame actions — read and commit the lane of the workspace recorded on the stack's
base entry, through `ShellView::target_frame`, so a dialog opened in a pinned
workspace changes only that workspace's lane (see
[workspace lanes](shell.md#workspace-lanes)).

Known limitation: apart from object dialogs, the stack holds one instance per
`DialogKind`, so a second request for a live kind cannot open beside the first
even from a different call site. The `choicedialog` pickers (grouping, scope,
tile kind, column, log level) share one kind and one state field, so none of them can
open while another is anywhere in the stack.

A multi-screen dialog registers its back step with `dialog::set_back`: a
predicate over its current state and the transition Escape's final back step
runs. While the predicate holds, the shared title row paints a ghost Back
button left of the title, with a tooltip naming Escape. A click leaves exactly
one screen: it discards whatever Escape's earlier steps would discard, then
takes that same transition, and synchronizes the shared input as every
pointer transition does. The object dialog shows Back in Naming, Edit, Column,
and Values; the dimension picker in Values; the log-level choice in its level
step. Browse, Columns, the log-level targets, and one-step dialogs paint none.
In the object dialog one click cancels an open value field with its typed
text, reverts filtering, and clears a kept query before leaving; while a y/n
confirmation is pending the button stays painted and its click does nothing.
The as-of dialog's Custom field and timeseries popups have no Back button.

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

The link chooser is the one exception (`ChoiceList::set_query_placing`). Its
opening row is the tile's current follow row, `follow · workspace` for a tile
that was never linked, and that row's text survives `a` and `c`: kept lit by
text it would stay lit over `follow · A`, and Enter would follow nothing. So
a changed query there lights the row it ranks first. The current follow row
stays lit only when nothing outranks it: under a blank query (empty or all
whitespace, which ranks every row level), and under a query that ranks it
level with the top row, such as `f` or `follow`, which every follow row
matches alike. Lit on the first of those level rows, Enter after a shared
prefix would unfollow. A highlight moved with the arrows after typing is
kept until the query text changes again.

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

The palette opens over an open dialog stack rather than being mutually
exclusive with one: it lists every action regardless of what is open beneath
it, paints above the top dialog, and a click outside the palette closes only
the palette. A dialog-opening row pushes its dialog onto the stack; every
other row dispatches and runs behind the stack, which stays exactly as it
was. Closing the palette — Escape, a commit, or an outside click — restores
focus to the top dialog, whether or not the action it ran moved focus itself.

Picking "Open the tile command line", "Find in tile", or "Stack: Pick…" while
a dialog is open is refused: `ShellView::dispatch` posts a status notice and
runs nothing. Each would open real transient chrome behind the stack that the
palette's own post-dispatch refocus would immediately take keyboard focus
away from, leaving it open but unreachable — a command line or find prompt
that cancels itself on the next render for having never held focus, or a
stack member list that only becomes usable once the stack closes. This
differs from an ordinary palette action, which is allowed to run behind the
stack, as described just above: these three are refused instead of left
stranded.

"Switch to workspace N" (`workspace::switch_*`) and "Toggle the frame pin for
this workspace" (`frame::pin_workspace`) are refused the same way, with the
same notice. Either would change which frame lane the active workspace reads
while the dialog stays bound to the lane it opened in, so the toolbar would
mix two lanes (historical tiles without the historical stripe, for example).
Refusing them keeps the dialog's lane and the active lane the same workspace
for as long as a dialog is open.

While a page is open the three transient-chrome actions above are refused
with `close the page first (esc)`, and so are `tile::add`, every per-kind add action,
`tile::open_with`, `tile::autosize_columns`, `tile::link_group`, and every `workspace::`,
`dock::`, and `stack::` action except the workspace switches: the page
covers the tile surface they would open on or change, and a layout edited
behind a page is a change the trader cannot see (`Close tile` would destroy
an unseen tile with no undo). A switch closes the page and then switches.
The palette reaches every other action over a page.

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
- A module action-menu hint is an action identity that `geode_tile::menu`
  resolves against the live keymap when the menu opens, when its rows
  rebuild, and when the keymap is republished; the keys paint through
  `kbd::menu_binding`. A `:` command-line verb (an unbound action's
  fallback) or a label such as a range preset's `1w` stays text because it
  is not a key.
- A menu's trailing lane paints keys the way gpui-component's `PopupMenu`
  does: the label without the chip's fill or padding, in the lane's color,
  so a highlighted row's keys follow the highlight.
- A key named inside a sentence (a notice, a confirmation, a refusal) keeps
  the keymap's lowercase spelling from `palette::render_binding`, since that
  is what a user types into a keymap file.

Hardcoded hints name the shipped key. A user rebinding does not change the
empty-state hint's `ctrl+k`; the palette, keybinding rows, tooltips, the
timeseries footer, and every module's action menu read the live keymap.

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

### Autosize columns

Two doors reach one route. `:autosize` in a blotter, market-data, or pricer
tile fits every column of that tile's table to its content, and
`:autosize reset` drops the fitted widths and returns to the configured or
default ones; both are in each module's completions. The palette's
"Autosize columns" (`tile::autosize_columns`, category Tile, unbound by
default) calls `TileContent::autosize_columns` on the focused tile's
occupant alone, which in the three table modules is the same method the
command runs. The trait default refuses with "this tile has no table", and
the shell paints a refusal, or the same text when no tile is focused, as a
status notice that clears on the next dispatch. A table tile with no rows to
measure refuses a fit with "nothing loaded to fit" and keeps its widths.
`:autosize reset` never refuses. Measurement and storage are
described in [features](features.md#autosized-columns).

## Scope text and stack selection

The toolbar scope field edits `FrameView::scope().text` live. Focus captures the
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
column when no fallback exists. Escape or the title row's Back button from
Values discards the stage's query, results, and ticks and returns to the
previous column. Escape from Columns closes the dialog. Neither step applies
the draft.

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

## Grouping, scope, tile, log, and column choices

[`shell/choicedialog.rs`](../../crates/geode-shell/src/shell/choicedialog.rs)
uses a filter-only `ChoiceList` with a target-specific commit. Tab completes,
Enter or a row click commits, and Escape closes, except for the nested log
level stage.

| Target | Rows and commit |
|---|---|
| Grouping | View default, then filled slots 1–9; opens on the active choice. Empty-query digits commit directly, with zero choosing the default. Unfilled digits are consumed. Commit rechecks slot existence, reports removal if needed, then closes. |
| Scope (`frame::scope`, `mod+o`, and the toolbar's load glyph) | One row per saved scope, named, in the saved set's name order, read from the target frame's live saved scopes at open, so a scope saved or reloaded since startup is listed (the palette's `scope::<name>` rows are registered once at startup). Opens on the first saved scope equal to the frame's current scope, else the first row, so Enter on an untouched picker changes nothing. Commit loads through `ShellView::load_saved_scope`, the `scope::<name>` actions' own path: one undoable `set_scope` step in the target lane. A name removed by a reload while the picker was open loads nothing, closes, and reports "that saved scope no longer exists". With no saved scope the list is replaced by a hint to narrow the scope and save it with the save glyph (painted once the scope is non-empty) or `scope::save_current` (its chord when bound, else its palette title); Enter there does nothing and the footer offers only Escape. |
| Tile kind | Roster order excluding the placeholder. Closes before adding to the tile focused at commit time: fills a placeholder or splits a real tile using the configured placement. |
| Tile kind with context (`tile::open_with`) | The same rows, pre-filtered to kinds whose factory accepts a column of the focused tile's captured dimension context, titled `Open {subject} in…` (the first context value of an accepted column). Commit always splits, passing the factory's translated `launch_state` as the new tile's restored record. |
| Column (`config::view_column`, `config::schema_column`) | The focused tile's presented columns from `TileContent::tile_columns`, captured at open; a row reads the header label, then ` · name` when they differ. Schema omits columns no dataset of the view declares in current configuration (derived view columns, and derived dimensions a view lists as plain dimension columns). Opens on the cursor's column, else the first row. Before the list opens, a tile with no columns refuses with "this tile has no dataset columns", a Schema list with nothing left with "no schema columns in this tile's view", and a target dialog already in the stack with the stack's own refusal. Commit closes the list, then opens the dialog on that column's Column stage (see [configuration dialogs](configuration-dialogs.md#stages-and-ownership)). Palette-only, no default binding. |
| Log level | Choose a logging target, then its level. Escape or the Back button from levels returns to a rebuilt target list and clears the filter; a level choice submits `Diagnostics::request_level`. |
| Link group (`tile::link_group`, `mod+u`, and the status bar's `following` segment) | The [link groups](shell.md#link-groups) of the focused tile, which is captured at open with its membership, so a pick lands on that tile even if focus has moved. Rows are `follow · workspace`, then `follow · A` to `follow · D`; a tile whose module emits also gets `emit · none` and `emit · A` to `emit · D`. A row stands for its change by position, not by its text. The title is `Link group`, with ` · following A` and ` · emitting B` appended for the groups the tile is in. Opens on the row for what the tile follows, so Enter on an untouched chooser changes nothing; a typed query places the highlight by the chooser's own rule (see [filtering, choice, and movement](#filtering-choice-and-movement)). Digits type into the filter. Commit closes the chooser, then follows or emits through the shell's link doors; one pick changes one of the two. With no focused tile, or a placeholder focused, nothing opens and the status bar reads `no tile to link`. A tile closed under the open chooser (the palette still reaches `Close tile`) is linked to nothing, and the pick reports `that tile is no longer open`. Refused while a page is open. |

Closing and reopening creates fresh dialog state. These pickers apply on
Enter; they do not use Normal/Filter mode's keep-query Enter.

## Frame expression

[`shell/scope_expr_view.rs`](../../crates/geode-shell/src/shell/scope_expr_view.rs)
edits the frame's expression and the frame's named-expression references
(Whole and Add through staged names, Term by naming the term), in one of three modes chosen by the door that
opens it. Bare Enter trims and parses the draft in every mode.

| Mode | Opened by | Seed | Enter | Empty Enter, nothing staged |
|---|---|---|---|---|
| Scope expression (whole) | `frame::scope_expression` | The whole expression; the frame's names staged | Sets the names to the staged list and replaces the expression | Clears the expression and the names |
| Edit scope term | A click on a toolbar term chip | That top-level `and` term | Replaces that term; the other terms keep their order | Removes that term |
| Add scope expression | `frame::add_expression` (the `+` menu's "Expression…" row) | Empty, nothing staged | Appends the staged names the frame lacks and joins the text to the current expression with `and`, or sets it when there is none | Closes without a change |

Whole and Add stage named expressions beside the text. Each staged name
paints as a `≡ name` chip above the field, in staged order, with a `×` that
unstages it; a name the frame cannot resolve (missing or invalid) takes the
danger tone, as its scope-bar chip does. Backspace with the caret at the
field's start and no selection removes the last staged chip; anywhere else
backspace edits the text. A name is staged by accepting its suggestion row
(see [Suggestions](#suggestions)). Enter applies the staged names and the
text in one `set_scope`, so one undo takes back both. An empty field with
names staged applies the names alone: Whole sets them and clears the
expression, Add appends them. Term mode stages nothing.

The term and add modes show a muted note under the field saying what the
commit touches. A successful commit changes only the expression and, in
Whole and Add, the named references in the frame scope, through undoable
`set_scope`, then closes. Parse errors remain inline
in every mode and typing clears the error. The term dialog remembers the term
it was seeded with; if the scope changed while it was open so that its index
no longer holds that term (gone, or a different term in its place), an edit
or an empty (removing) commit refuses inline rather than touch whichever term
now has that index. Escape discards the scope draft; definitions already
saved with `mod+s` remain.
`frame::clear_expression` drops the whole expression layer without a dialog;
with no expression it does nothing. `frame::add_expression` is bound to
`mod+x` by default, as `frame::pick` is to `mod+p`; neither
`frame::scope_expression` nor `frame::clear_expression` has a default chord.
All three are in the palette. Named expressions have no entry of their own: the Add
dialog offers them beside typed text, and any expression is named at
creation or later with `mod+s`.

In every mode, `mod+s` (the configured `mod` key, Alt by default, with
`s`; the footer's chip shows the user's own alias) saves the typed text as a
named expression. On an empty or whitespace-only field it refuses at once
with `nothing to save — the expression is empty` and opens nothing.
Otherwise the field becomes a name entry labelled `Save this expression as a
named expression · name`, with the suggestion list off and the staged chips
kept. Escape leaves the entry and puts the text back. Enter checks the name
first, then the text, and refuses inline with the entry still open for:

- a malformed name (`name: <reason>`, from `check_object_name`);
- a reserved name (`'<name>' is reserved`);
- a name the Expressions domain already holds (`'<name>' already exists`);
- text that the dialog's own Enter would refuse (a syntax or schema error);
- no writable user config directory (`no writable user config directory —
  nothing was changed`).

A save writes `[name] expression = "<text>"` to the user layer of
`expressions.toml` through the object dialog's write path, rebuilds the
frame's named expressions from the pending configuration at once (so the new
name resolves before the write reaches disk), empties the field, and stages
the name. In Whole and Add nothing reaches the frame scope until Enter;
Escape cancels that draft without removing the saved definition.

In Term mode the save names the term (the entry reads `Name this term ·
name`, the footer chip `name this term`): Enter on a name writes the field's
text (edits included) as above, then replaces the term with the name in one
`set_scope` (the term leaves the expression and the name joins the frame's
named list) and closes the dialog. One undo puts the plain term back; the
definition stays. The term is checked before anything is written, so a term
that changed underneath refuses with the term dialog's usual message and
writes nothing. A write that fails later rolls the configuration back but not
the swap: the tile then refuses to query with a missing-name error rather
than drop the filter, and one undo restores the term, as with a staged name
in Add.

The toolbar's `+` opens a two-row menu, "Dimension…" (`frame::pick`) and
"Expression…" (`frame::add_expression`), each row showing its action's live binding
through `kbd::menu_binding`. It owns the keyboard while open: `j`/`k`
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

### Suggestions

While the field is open, a live list under it
([`shell/expr_suggest.rs`](../../crates/geode-shell/src/shell/expr_suggest.rs),
pure state in
[`exprcomplete.rs`](../../crates/geode-shell/src/exprcomplete.rs)) shows what
fits at the caret, reading the partial text through
[`geode_core::scope::complete`](../../crates/geode-core/src/scope/complete.rs).
It refreshes when text or caret changes without requiring a full parse. Rows
are ranked against the token prefix and capped at 50. The vocabulary combines
columns across datasets, then derived dimensions; the first declaration of a
name determines its role and type.

The rows depend on the caret's position in the grammar:

| Caret is after | Rows offered |
|---|---|
| Nothing, `(`, `and`, `or`, `not` | In the frame dialog's Whole and Add modes, the unstaged named expressions; then every column, then `not` and `(` |
| A column | The operators valid for that column's type |
| An operator, or inside an open `in (` list | Values for that column, when there are any to list |
| A complete term | `and`, `or`, plus `)` when a paren is open (or `,` / `)` inside an `in` list) |

Right after typing `in` and before its `(`, the only row offered is `(` itself.

Named rows appear only at that column position, only in the frame dialog's
Whole and Add modes: never in Term mode, never in the name entry, and never
in the Scopes or Expressions object dialogs' `expression` field. They rank
with the other rows by their name. A row paints as `≡ name` under its own
selector (`scope-expr-named-row-{name}`), so a name equal to a column's name
is a separate row. Its detail is an elided preview of the definition's text;
a definition that is invalid shows its reason instead, in danger text.
Accepting a named row (tab or a click) erases the typed token, through the
same range replace as an insertion so cmd+z restores the text, and stages the
name as a chip above the field instead of writing it. A staged name is not
offered again until it is unstaged. Add mode also leaves out the names the
frame already has, since Enter would add nothing for them. A configuration
reload re-offers the current definitions under an open dialog.

Operators are filtered by the column's type: text offers `= != in like`;
number, date, and timestamp offer `= != < <= > >= in`; bool offers `= !=`;
a derived dimension offers `= != in`. `<>` still parses but is never offered;
it is a synonym for `!=`.

Categorical text values come from a distinct query with row counts. Ready
results and failures are cached per column until the field closes; a failure
is not retried within that opening. All requests use one query-pool key, so
requesting another column supersedes an unanswered request and removes its
loading entry. Returning to that column requests it again. A dialog pushed
over an expression field can supersede its request under the same key.
Revealing the field clears cached values and requests them again. The hint reads `loading values…` while the request is in flight and `values
unavailable: <reason>` if it fails. A derived dimension lists its configured
labels with no query, and a bool column lists `true`/`false`. Every other
kind — non-categorical text such as a key, and number, date, or timestamp
columns — gets a hint instead of a list; a date or timestamp hint gives an
example literal to type in quotes. The values request is narrowed by a scope,
never by the in-progress text (a half-typed value or a trailing `or` would
make the typed prefix an unsound filter):

| Mode | Values narrowed by |
|---|---|
| Whole | The frame's dimension selections, text filter and as-of, with the staged names in place of the frame's. Its own expression is excluded, since the dialog replaces it. |
| Add | The frame's full current scope, including its expression, plus the staged names. |
| Term | The frame's scope with the edited term removed. |

Staging or unstaging a name, saving one with `mod+s`, or a reload that
changes the definitions changes that scope, so the frame dialog drops every
cached column and asks again for the one under the caret; a reply to a
request made before the change is dropped.

A reply whose tag is not that column's latest is dropped, so a superseded
request never overwrites a newer one.

Tab inserts the highlighted row over the token under the caret and re-reads
the new position; shift+tab moves the highlight back one row; the arrows and
ctrl+p/ctrl+n move it by exactly one (page keys and ctrl+u/d/b/f stay the
field's own caret keys). A row click inserts without moving focus out of the
field. The click names its row by label, found in the list as it stands at
the press; if its label is absent, nothing is inserted. The second press of
a double-click is ignored. Enter applies the draft according to the mode
table above; it does not insert a suggestion. Every insertion is a range replace on the
field's own text, so cmd+z undoes it like any other edit.

A warning line under the rows names the first schema problem in the text — an
unknown column (with `did you mean <name>?` when one is close) or an
ordering/`like` comparison on a derived dimension — but never one whose span
still touches the caret, so a warning never flags a word still being typed.
Syntax errors stay silent while typing; they surface only on Enter.

Enter refuses a syntax error or a schema error (the same check the warning
line uses) with its message, and the text stays in the field. With no schema
loaded (an empty vocabulary), Enter still parses syntax but skips schema
checks.

Validation checks names against the combined vocabulary rather than an
individual tile's dataset. It checks forbidden derived-dimension operators,
but does not enforce every type-specific operator restriction shown in the
suggestions. Dates are quoted strings, so malformed dates can still fail at
query time. Typed prefixes rank value suggestions without narrowing the
underlying distinct request. The blotter's `:filter` command uses its separate
command-line completion.
