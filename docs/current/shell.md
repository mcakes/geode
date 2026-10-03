# The shell

The shell is Geode's persistent workspace. It owns the window, tile layout,
keyboard routing, shared frame state, dialogs, configuration surfaces, and the
contract used to host modules. Feature crates own their domain behavior; the
shell gives them a place to live and common interaction rules.

The shell never depends on `geode-data` or on a module crate. `geode-app`
composes those layers. This dependency boundary lets a module carry its own
data handle while receiving only shell concepts such as `TileId`, `Frame`, and
`Delivery` from the host.

## State ownership

`ShellView` is the retained GPUI entity for one window. It owns transient
window state: the active workspace, tile occupants, focus restoration,
palette, modal, command line, drag state, and session dirt. Its `render`
method describes the current frame; it must not become a second domain model
or perform blocking work.

State with a narrower owner stays outside `ShellView`:

- `Workspaces` and the tiling tree own layout and structural focus.
- `Frame` owns the scope, grouping, as-of value (shared, or per pinned
  workspace), the link groups (each group's scope and board, and which tile
  follows and emits into which), recent publications, and version counters
  observed by tiles.
- Each module entity owns its cursor, subscriptions, draft, and prepared
  presentation.
- GPUI component state, such as `InputState` and `TableState`, owns reusable
  control behavior.
- Four GPUI globals carry settings that are genuinely app wide and visible to
  modules: `UiSettings`, `Chords`, `AppClock`, and `SeriesSettings`.

New state belongs in the narrowest owner that can keep it correct. A global is
appropriate only when independently hosted modules must observe the same
application setting.

## Tiles, workspaces, and stacks

The pure tiling model has three node kinds: split, leaf, and stack. Layout is
computed once by `Tree::layout`; painting, directional focus, divider
geometry, and drop targets use that answer rather than maintaining parallel
geometry. Each workspace also owns left, right, and bottom dock trees.

See [tiling and workspaces](tiling.md) for region crossing, empty-dock focus,
insertion/removal rules, drag geometry, and restoration limits.

A stack occupies one layout slot and keeps two or more leaf tiles alive. Only
its active member is visible. Focusing a member activates it, so structural
focus cannot point at a hidden member. The shell sends each occupant a
`StackHandle`; modules paint its marker through the shared builder and use it
to open the shell-owned member list. `stack::pull_*` moves the visible
neighbor into the focused tile's slot and `stack::split` turns a stack back
into tiles. The shell dispatches both rather than the pure workspace router:
the split's orientation needs the slot's geometry, and a refused pull leaves a
notice.

A fullscreen tile paints with the same chrome as a workspace's only tile,
so the status bar marks it instead. The bar's right region is its
view-state section: how the window is being shown, then the active
theme's name. While a main-tree tile is fullscreen, a muted segment there
reads `fullscreen · N hidden` (just
`fullscreen` when it hides nothing). N comes from
`Workspace::fullscreen_hidden`. The segment's tooltip names the
`workspace::fullscreen_tile` key, resolved on hover. Clicking the segment
dispatches that action and restores the layout. Dock trees cannot be
fullscreen, so a dock never shows the segment.

The same section opens with a `following A` segment while the focused tile
follows a link group, with the group's underlying after it (`following A ·
SPX.Z`) when the group's scope names exactly one. A tile that only emits
shows none, and a page hides it. The label is cached on the focused tile,
its group and the group's scope generation, and refreshed while a render is
prepared, so a repaint formats nothing. Its tooltip names the
`tile::link_group` key and a click dispatches that action. Each tile's own
header carries the matching chip (`geode_tile::header::link_chips`): the
group's letter with a down arrow for follow and an up arrow for emit. See
[link groups](#link-groups) for what following and emitting do.

Tile occupants are created through the app-supplied `ModuleRoster`. A new
occupant begins hidden and receives an explicit visibility value during the
next reconciliation. Hidden occupants may release live subscriptions and
must requery when shown if their followed versions changed. Removing an
occupant (closing its tile, or filling a placeholder in place) calls
`set_visible(false)` and then `closed`, once, before the occupant is dropped.
Hiding never calls `closed`: a workspace switch, a dock toggle or a stack
cycle only hides.

### Dimension context

A tile reports `TileContent::dimension_context` on demand: every dimension or
key with one value at its cursor row (`geode_core::context::DimensionContext`,
column name to value; the blotter reads its grouping path, shown columns and
hidden context columns, the pricer the cursor line's sole underlying as
`underlying_ref`). A column is absent whenever the row names no single value
(NULL, mixed, or above the column's grouping level), never a guessed key,
because a panel opened on a made-up value is a plausible wrong answer. A
tile with no cursor row answers `None`, the default.

`tile::open_with` pulls the focused tile's context and lists, in the shared
choice dialog, every roster kind whose `ModuleFactory::accepts` (column
names, such as `underlying_ref`) names at least one column present in it; the
other columns a row carries do not block a kind. The dialog is titled
`Open {subject} in…`, the subject being the first context value, in context
order, of a column a listed kind accepts. `ModuleRoster::context_columns` is
every factory's `accepts`, then every row menu action's column (see
[Row menu](#row-menu)); `geode-app` hands it to the data service so each row
carries those values. A pick always splits: the factory's
`launch_state` translates the context into that kind's own restored-state
table, so a source and a target agree without depending on each other. A
context holding no registered context column (empty, or only columns such as
`lhu` that no factory accepts) falls back to the plain tile picker; one
holding a registered column that no listed kind accepts produces a notice
instead of opening anything. The context is captured when
the dialog opens, so moving the source tile's cursor afterward does not
change what a pick creates.

`TileContent::launched` runs once, deferred with `cx.defer_in` past the
current render, for an occupant `ShellView::add_tile` created (an add or a
duplicate, never a session restore) that is the focused tile on its first
render. A module that is useless without further state, such as a panel
added with no state — from the palette, the tile picker, or
`tile::open_with` falling back to the picker — asks for it here.

`TileContent::set_focused` tells an occupant whether it is the focused tile
of the active workspace's focused region. The shell diffs the focused tile
during its render, before the tiles render, and tells the tile that lost
focus and the one that gained it; a fresh occupant under the focused id is
told again. Focus is reported after visibility in the same render, so a tile
hidden beneath an opening page hears `set_visible(false)` and then
`set_focused(false)`. The call runs inside the draw, so a module keeps the
flag and reads it when it paints, and must not notify the frame from it. A module
uses it to paint a focus-only state or to let a press that merely focused
the tile do nothing else (the shell's own tile handler runs after the
module's, so the flag still says whether the tile was focused before the
press).

### Row menu

The row menu lists what a row's single-valued columns let the user do. The
shell owns it (`shell/row_menu.rs`), so every grid gets the same menu from
its one `dimension_context`. Two routes open it:

- `tile::context_menu` ("Row actions…", category Tiles) takes the focused
  tile's `dimension_context` and hangs the menu at the context's `anchor`
  (window-space logical pixels, the cursor row's lower-left as the tile
  last painted it), or at the tile's top-left when it records none. The
  blotter and pricer bind it to `g .` in normal mode, beside `g m`.
- A right press on a tile first focuses it, as a left press does, then
  asks the occupant's `TileContent::press_context` for the pressed row. The
  tile cell hears the press in the capture phase, so an occupant that stops
  its propagation (gpui-component's table does, on a cell) cannot hide it.
  The shell reads `press_context` one deferred effect later
  (`open_row_menu_after_press`): every mouse-down listener, capture and
  bubble, has run by then, so an occupant records its pressed row
  synchronously in a mouse-down listener (any phase). Handling it routes
  through an event may land after the menu opens, and must not move window
  focus.
  The default answers `None` and nothing opens; an answer opens the menu at
  the pointer. The blotter and the pricer answer it.

`dimension::menu_rows` builds the rows. Each context column, the clicked
column (`DimensionContext::first`) first and then the rest in context
order, gets a section headed `{column} · {value}` holding "Open {Kind}" for
every roster kind whose `accepts` names that column, then every registered
action on that column. A kind accepting several present columns sits in the
first such section only. Sections with no rows are left out, and
separators part the rest. A context with no rows at all opens nothing: the
status notice reads "no actions for this row" (`NO_ROW_ACTIONS`). That is
also the answer with no tile focused or a tile that answers no context.

While open, the menu owns the keys: `j`/`down` and `k`/`up` step over
enabled rows, `enter` picks the highlighted row, and `escape` closes it.
Any other bare key is consumed. A chord passes to the matcher, and its
dispatch closes the menu. The menu also closes on a pick, a press outside
it, any dispatched action, a dialog opening, or the palette opening. It
does not open while a modal is open, and it is refused over a page. A
disabled row shows its reason and is inert, to a click or to `enter`.
Picking "Open {Kind}" splits a new tile of that kind beside the focused
one, restored from its factory's `launch_state` of the context, as a
`tile::open_with` pick does. When a dismissal (`escape`, a press outside)
closes it, focus returns to the scope bar's text field only if that field
held focus and a key opened the menu; otherwise to the shell root. Opening
the menu closes the palette, the command line, the stack list and the
add-a-filter menu, and takes the shell root's focus.

A crate adds a row by implementing `dimension::DimensionAction`: `id`,
`title`, the `column` whose section it sits in, `available` (enabled, or
disabled with the reason its row shows; enabled by default), `run`, and
`chosen` (a value picked from `choose_value`; does nothing by default).
`geode-app` registers it with `ModuleRoster::add_action` in
`add_bridge_modules` (through `add_dimension_actions`), before the roster's
`context_columns` are published, so each row carries the action's column.
`run` gets the menu's context and an `ActionCx`, after the menu has closed:
`open_tile(kind, state)` splits a tile beside the focused one, as `g m`
does; `open_url(url)` hands a URL to the OS (`App::open_url`), or to the
`dimension::UrlOpener` global's function when one is set, as tests set it;
and `notice(text)` sets the status notice. The notice takes any text, so an
action can report what it did, not only a fixed refusal. Nothing reports
whether a handler took a URL, so a notice says only what Geode did.

`choose_value(context, column, title, exclude, empty)` opens the
shared choice dialog, titled `title`, over `column`'s distinct live values,
unscoped, minus `exclude`. It asks for them with a `Request::Distinct`
under the reserved `ACTION_KEY` and a fresh tag, and reads `loading…` until
they arrive, its footer offering only `escape`; `enter` does nothing while
loading, and a query typed meanwhile filters the rows when they do.
`ShellView::deliver_distinct` routes `ACTION_KEY` replies to the open
dialog, which drops one whose tag or column is not its own. No values left
after the exclusion closes the dialog with the notice `empty`; a failed
fetch closes it with `could not load {column} values: {reason}`. Nothing is
asked when a choice dialog is already open. A pick closes the dialog first,
then calls the action that asked (the running one) with
`chosen(context, value)`, so a dialog that
`chosen` opens lands on the stack the list was opened over.

`confirm(question, on_yes)` opens a plain y/n dialog asking `question`. `y`,
`enter`, or the Yes button closes it and then runs `on_yes` with a fresh
`ActionCx`; `n`, `escape` (with any modifiers: shift or ctrl with escape
is consumed as No too), or Cancel closes it. Any other bare key is
consumed; a chord or a shift-modified key passes to the matcher. A confirm
asked while a plain dialog is already on top is refused with the notice
`another dialog is already open`.

`ShellView::note_command` shows a position-system command's answer as the
status notice, worded by `geode_core::positions::outcome_notice`, which
shares its wording with the action's `sent_notice`. The app's bridge calls
it for every `DataEvent::Command`. It replaces the current status notice,
as any notice does, and checks no tag: the latest answer wins.

Three actions are registered, in this order. Two are from `geode-nemo` and
both titled "Open in Nemo": `nemo::open_position` on `position_ref`, opening
`nemo://position/<id>`, and `nemo::open_instrument` on `instrument_ref`,
opening `nemo://instrument/<id>`. The id is percent-encoded as one path
segment (RFC 3986 unreserved characters pass). Each opens the target row's
value only, ignoring any selection riding in the context, and sets the
notice `opened <url>`. The third, from `geode-positions`, is "Move LHU…"
(`positions::move_lhu`) on `position_ref`. It acts on the selection riding
in the context when there is one, chooses the target LHU with
`choose_value`, asks with `confirm`, and sends one command through the data
handle; see [Move LHU](features.md#move-lhu). `add_dimension_actions` takes
whether startup resolved a position service (`positions.toml`, which is
restart-required) and passes it to the action, which is disabled without
one.

## Actions and keyboard routing

The action registry is the vocabulary shared by the keymap and command
palette. The keymap resolves layered bindings against an ordered stack of
contexts and supports chords, sequences, and numeric counts. The focused
surface contributes its context; module fragments extend the vocabulary
without giving a feature access to `ShellView`.

See [input and dialogs](input-and-dialogs.md) for routing before the matcher,
focused text ownership, palette and completion behavior, and frame pickers.

The [keymap contract](keymaps.md) defines precedence, immediate exact matches,
sequence cancellation, count handling, and the limits of fragment filtering
and binding-editor resolution.

One logical command has one action or owner method. Keyboard bindings, palette
rows, toolbar controls, and pointer handlers route to that command instead of
copying its mutation. Every pointer command exposed to the user must have a
keyboard route.

The per-tile `:` command line is local to that tile. It may change the tile's
query, presentation, cursor, or draft. Frame-wide and application-wide
commands belong in the action registry and palette. The existing `[ui]
find_style` setting chooses each new `/` prompt's behavior. Vim routes
incremental `FindEvent`s to the occupant. Fzf opens a tile-local ranked results
table through `TileContent::start_fuzzy_find`: the occupant supplies labels,
ancestor paths, and reveal callbacks; the shell owns query input and selection.
Exact, prefix, and contiguous label matches rank ahead of looser fuzzy matches.
The tile keeps its header and uses its normal table columns, widths, numeric
formatting, and row density. Tree results retain matches and their ancestors,
each parent once, with sibling branches ranked by their strongest match.
Context-only ancestors are muted; their numeric totals remain unchanged.
Rows retain their original indentation and chevrons. Blotter search uses the
normal cell's disclosure alignment, flex shrinking, and direct label layout,
including blank grand-total cells and leaf markers. Both normal and search
gutters size to the painted numbers, with a two-digit minimum. Expansion does
not preallocate digit space for offscreen descendants. The tree column reserves
only that compact minimum; larger visible numbers use some label space without
moving the numeric columns. Line numbers follow the
On / Relative / Off setting and address the displayed rows, including parents.
Column headers
use the tile's original renderer, preserving typography, alignment, colors,
and separators. The selected
row's ancestor path and direct-match count appear above the prompt. Numeric values
provide context; names and ancestor paths are the search keys. The results
table tells the occupant which source rows it shows (`set_table`'s `on_rows`)
whenever its visible range changes and at the next layout after any result-set
change, clamped to the rows that remain (the tail of the same height when none
do, so a lone match is reported); before its first range it reports the first
64 rows. Market data and the pricer format only those rows, outside paint, and
a cell the occupant has not prepared paints blank. `refresh_rows` has the table
re-report the same rows (the pricer's price refresh), and `set_notice` puts the
occupant's word on the results in the status line in place of the match count
(the pricer's "Results out of date — reopen /").
Search text is prepared once per dataset. Ranking runs off the UI thread;
query revisions cancel obsolete work and prevent stale results being accepted.
Single-word extensions rescore the last completed query's candidates; edits,
backspace, multi-word queries, and dataset replacement search the full index.
Results retain only row indices. Match positions are calculated for visible
rows and cached in the table, so a broad match does not allocate highlights
for every row or free millions of highlight buffers on the UI thread.
Up/Down or Ctrl-N/P select any displayed row. Clicking a chevron or pressing Tab
folds the selected branch in the search view only; editing the query reopens
matching branches. Enter or a row click reveals the selected row in the tree, and Escape
or leaving the tile cancels without moving the tree. No matches keeps the
prompt open. The table virtualizes results and scrolls to keyboard selection.

Blotter, pricer, and market-data implement Fzf search. Tree searches include
collapsed descendants within the current scope. The blotter requests the full
grouping depth when necessary. Every loaded parent and leaf is displayed together
in full tree order, using labels prepared only for visible rows. Search text is
indexed on a worker; completing that index does not change the empty-query
display order. Queries entered during preparation wait for the complete index.
The index is reused while the snapshot, column plan, and
sort are unchanged; closing the prompt cancels pending preparation. Searching does not expand the tree; choosing a result
opens its ancestors and restores the normal tree with that row active. Pricer
results also include legs inside closed packages. Unsupported tiles retain
their existing find behavior.

## Focus

GPUI window focus and the tiling model's focused tile are separate state and
must be reconciled deliberately. Left and right tile presses focus the tile
and arm `pending_focus_restore`; a right press does not start a drag or a
double-click gesture, and is heard in the capture phase, so an occupant that
stops its propagation still has its tile focused. This gives a module's context menu the same tile's key
context. Keyboard commands that move structural focus also arm restoration.
The shell returns window focus to the appropriate tile surface, except while
that occupant intentionally holds an insert-mode input.

An occupant that closes a focused input must blur it before dropping its
handle. Switching workspaces also restores focus immediately when the old
occupant remains alive but is no longer mounted. Likewise, the render that
takes a tile off screen returns focus to the shell root unless a shell input
or a still-painted occupant (through `holds_focus`) owns it, so a pull into a
tile that is typing leaves its editor focused. Without these steps GPUI can
retain or fall back from a handle that no longer receives the shell's key
listeners, leaving subsequent chords ineffective.

Dialogs open through `open_shell_dialog`. That door closes competing
transient surfaces, establishes the modal state, and calls `prevent_default`
so a dialog opened from mouse-down keeps the focus it assigns. Dialog content
is built from a borrowed `&ShellView` during render; it must not re-enter the
entity with `Entity<ShellView>::read` while that entity is already rendering.

## Dialog filtering

Object dialogs (Browse, Edit, Column, and Values), Settings, and Keybindings
share `dialogmode`'s filter transitions. `/` and a click on the frozen filter
row both record the current query before focusing the input. Escape restores
that entry query; bare Enter keeps the edited query. Both leave Filter mode
without opening a row, committing an edit, starting key capture, or closing
the dialog. A second Enter in Normal mode performs the selected row's normal
action where supported: opening an object or nested editor, opening a Settings
choice, or starting a Keybindings capture.

`enter_filter`, `filter_exit`, and `exit_filter` own the snapshot and key rules;
`sync_dialog_text` reconciles the resulting query and focus. Escape accepts
modifiers, while the keep-query Enter must be unmodified. Restoring a different
query resets the list toward the first match; object edit stages then settle
on an eligible row under their [cursor rules](configuration-dialogs.md#stages-and-ownership).
Leaving unchanged text keeps selection. In Normal mode, subsequent Escape
presses clear a query, return from a nested stage, then pop this dialog off
the stack: an entry beneath it, if any, is revealed with its own query, caret,
mode, and focus restored, rather than the whole stack closing (see
[modal lifetime](input-and-dialogs.md#modal-lifetime-and-focus)). The modal
title row's Back button is the pointer route for the return step: one click
discards what the earlier Escape presses would and leaves the stage.

Naming, open object value fields, Settings typeahead, and keybinding capture
have their own commit/cancel handling before filter routing. They can focus
the same input or use the same mode value without adopting filter-exit
semantics. Palette, dimension picker, and as-of surfaces likewise keep their
own interaction rules. See [configuration dialogs](configuration-dialogs.md)
and [keybinding editing](keymaps.md#editing-unbinding-and-reset).

## The shared frame

`Frame` is a pure value held in a GPUI entity. It combines the global scope,
active grouping, as-of value, recent publications, saved scopes, named
expressions, and version counters. Every mutation bumps only the counters
affected by that change, so a tile can cheaply ignore dimensions it does not
follow.

### Workspace lanes

The selection lives in a lane: scope with its undo/redo stacks and open text
session, the active grouping slot, and as-of with its one remembered previous
value. The frame holds one shared lane and one lane per pinned workspace. An
unpinned workspace reads and writes the shared lane; a pinned one reads and
writes only its own. Definitions stay shared across lanes — grouping slot
contents, saved scopes, named expressions — as do recent publications and the
data and config versions.

Every lane draws its scope, grouping, and as-of generations from one
frame-wide counter, so a generation number names exactly one value in any
lane. A tile compares numbers, not content; with per-lane counters a tile
whose workspace changed lanes could see an equal number over different
content and skip a requery it needed. Pinning copies the shared lane's values
and generations into the new lane with empty history — equal content under
equal numbers, so nothing requeries. Unpinning discards the lane, its history
included, without a confirm; nothing is promoted to the shared lane, and the
workspace reads the shared lane again. A grouping reload (`replace_slots`)
bumps grouping in every lane, hidden pinned ones included, and clears an
active slot that no longer exists in each lane separately; saving a slot
(`save_slot`) bumps grouping only in the lanes where that slot is active.

Tiles receive a `FrameRef` bound to their own tile and its workspace (see
[architecture](architecture.md)), so a pin or unpin changes the lane a tile
reads, and following a [link group](#link-groups) changes whose scope it
reads, without it re-subscribing. Any lane's or group's change notifies every
observer; each tile's version compare filters out the ones it does not read.

Only the active workspace's lane opens a flip barrier: its visible tiles are
the ones a barrier coordinates, so the shell compares and opens against the
active lane. A workspace switch is not a frame change: it re-seeds the flip
baseline from the new active lane instead of opening a barrier. When the
switch leaves or enters a pinned workspace, the old lane's text session ends
and the scope field re-reads the new lane; a focused field keeps focus and
opens a fresh session there, so Escape restores the new lane's text. A switch
between two unpinned workspaces leaves the field's session whole, because
ending it would split one edit into two undo entries. Pinning and unpinning
rebind the field the same way.

The toolbar's first readout control is a pin glyph, before the as-of chip:
a bare, muted verb while the active workspace is unpinned, a solid chip in
the theme's primary color while it is pinned (`Tone::Active`: the fill is
moved toward `foreground` where a theme's primary sits too close to its title
bar, so the on state reads at a glance on every bundled theme). The glyph
keeps a chip's height in both states, so toggling it does not shift the
readout. Its tooltip is "Pin the frame to workspace N" or "Frame
pinned to workspace N". A click toggles the pin, as does the palette action
`frame::pin_workspace` ("Toggle the frame pin for this workspace", category
Frame), which has no default binding. The pin covers scope, grouping, and
as-of together; there is no per-part pin.

Shell surfaces resolve their lane through `ShellView::target_frame`: the
workspace recorded on the base entry of the modal stack when a dialog is
open, else the active workspace. A dialog therefore reads and commits the
lane it was opened from. The flip barrier and the toolbar's scope text field
use the active workspace's lane directly. Under a modal the two name the same
workspace: the palette refuses `workspace::switch_*` and
`frame::pin_workspace` while a dialog is open (see
[input and dialogs](input-and-dialogs.md#palette-and-which-key)), so the
active lane cannot move, and the toolbar cannot mix two lanes, beneath an
open dialog. The session's `[frame]` record is
written from and restored into the shared lane explicitly, whatever is
active.

A scope (`geode_core::scope::Scope`) has four parts: dimension selections, a
list of named-expression references (`named`), a text filter, and an
expression. A query combines all four with AND. Composing scope layers
(`Scope::and_then`, the frame scope with a tile's own) treats each part by its
own rule: dimension selections on the same column intersect, named references
append in outer-then-inner order without repeating one already present,
expressions combine with AND, and an inner text filter replaces an outer one.
A named reference is not itself expression syntax — the grammar has no token
for it — so it cannot combine with `or` or `not`, and a named expression
cannot itself reference another one.

`FrameView::effective_scope` composes the frame and tile layers and then resolves
every named reference through `Scope::resolve` against the frame's own
`NamedExpressions` (read from `expressions.toml`; see
[configuration](configuration.md#documents)), folding each into `expression`
in list order, ANDed together and then with whatever expression the scope
already carried. Resolution runs before every query a scope reaches — a tile's
own requery and the shell's distinct-value requests (the dimension picker, the
Scopes dialog's Values stage, and the frame's expression-suggestion lists) all
resolve the scope they are about to ask for values or rows under. A missing or
invalid name is that request's error instead: a blotter tile paints it in
place of a result, keeping whatever snapshot it had already painted rather
than clearing it, and the other surfaces show it as their own
values-unavailable text. Nothing is queried in any of these cases — dropping
the name would widen the scope into a plausible wrong total, which this
guards against by refusing outright. `geode-data`'s scope compiler refuses a
scope that still carries `named`, as a safety net behind these call sites: a
resolution reaching the query layer unresolved is a defect, not a case to
serve.

Editing a named expression's text, or adding or removing one, reaches every
scope that ticks it without editing the scope itself: an `expressions`
(or `datasets`/`dimensions`) reload rebuilds the frame's `NamedExpressions`
and, when the content actually changed, bumps the frame's config version, so
every tile whose effective scope depends on it requeries (see
[hot reload](configuration.md#hot-reload)). A definition that stops parsing,
or a name a scope ticks that is removed, becomes that scope's resolution
error the same way a name that was always missing does.

Dataset and document watches narrow publication invalidation to consumers
that read the affected data. The frame keeps weak watches, allowing closed
tiles to disappear without explicit deregistration.

A document watch names an encoded batch key, and it also hears every document
under that key when the key is a shorter prefix of a multi-part key: a watch
on `SPX` in `option_chain` fires for `SPX␟2026-10-16` and every other SPX
expiry. Matching happens at a key-part boundary (`is_key_prefix`), so the same
watch never fires for `SPXW␟…`. On each publish the frame looks up the batch
and each of its key-part prefixes, so the cost grows with key arity, not with
the number of watches. `PublicationWatch::matches` answers whether a publish
concerns a watch and follows the same prefix rule; `PublicationWatch::is_for`
is the exact identity test a tile uses to decide whether it can keep a watch
or must register a new one for a different key.

A scope, grouping, or as-of change opens a flip barrier. Following tiles stage
their results until all participants answer or the deadline passes, then
promote together. This prevents one frame from showing tiles evaluated under
different global states. The barrier holds each awaited tile to the version
tuple that tile itself reads, so an old result cannot satisfy it. A later
change to the active lane replaces the barrier with one over every visible
tile: whatever was awaited before answered an older frame. A link group's
scope change concerns only that group's followers, so it joins the open
barrier instead of replacing it (see [link groups](#link-groups)).

Only visible occupants are barrier participants. Hiding a following tile (a
stack, dock or workspace switch) cancels nothing: its in-flight query
finishes, the reply applies when it lands (unless a counter the tile follows
moved since it asked, in which case it is dropped, as a superseded stage is)
and still answers any barrier the tile was enrolled in, and on return the tile
requeries only if a counter it follows moved while it was hidden. Closing is
different: removal calls `TileContent::closed`, and a following tile cancels
its query by key and answers any open barrier still waiting on it. The shell
calls `closed` and `set_visible` inside its render, where gpui drops a notify
to the frame (the render reads it), so a release made there would leave the
other tiles holding their stages until the deadline. Every following tile
therefore makes an arrival from either door, a close or a show whose requery
is refused or has nothing to ask, through
`geode_tile::following::DeferredDoor`: the arrival and its notify land after
the draw, and the other tiles promote at once. Frame observers, deliveries and
key handlers keep `FrameDoor`, which notifies in the same pass. `closed` fires
only for removal while the window lives; quitting the application calls it
for no occupant. A tile with its own error and no query
to send (a blotter whose view is no longer configured, or whose scope names an
undefined expression) answers the barrier at once, as a failed query does.
Tiles that submit no frame query (pricer, diagnostics) answer every barrier at
once. The rules live once, in `geode_tile::following`, which reads the
frame only through the tile's `FrameRef`: a tile in a pinned workspace
answers the barrier with its own lane's versions, not the shared lane's, and
a tile following a link group with that group's scope generation.

Scope text editing is one undoable session. The first real change records the
base scope, subsequent keystrokes coalesce, and returning exactly to the base
removes the no-op undo entry. Changes made through another surface during the
session remain distinct.

The toolbar's scope segment paints the dimension chips, then one chip per
named-expression reference (`≡ name`, in `Scope.named` order), then one chip
per top-level `and` term of the expression (`Expr::conjuncts`: nested `and`s
flatten on both sides; an `or`, a `not`, or a single comparison is one term),
then the contradiction chip. The frame still holds one `Expr`; the terms are
a view of it, and an edit rebuilds a left-folded `and` chain from the
remaining terms (`Expr::from_conjuncts`). A term chip's body opens the
expression dialog on that term; the `×` inside it drops that term alone
(`FrameViewMut::drop_expression_term`). As on a dimension chip, the `×` occludes the
body's hitbox, which is what keeps its press from also opening the dialog.
Term chips are addressed by index, which is stable within one scope version;
the term dialog also carries the term it was seeded with and refuses inline
unless that term is still at its index at commit time. Every term
edit, append, and clear goes through undoable `set_scope`.

A named chip's tooltip is the expression text. A name the frame's
`NamedExpressions` cannot resolve paints as a danger-toned chip,
`≡ name · missing` or `≡ name · invalid`, whose tooltip is the reason
`Scope::resolve` gives; every tile that scope reaches refuses to query until
the name is defined again or removed. The `×` inside a named chip removes that
name (`FrameViewMut::drop_named`, undoable through `set_scope`) and does nothing
else. Named chips are keyed by name, so their element ids survive a
neighbour's removal. The chip body has the chips' hover and pressed fills,
and a click on it opens the Expressions dialog on that name
(`objectdialog::render::open_object`): a defined name opens in its edit
stage, an invalid one included, since editing it is how it gets fixed; a
missing name opens the Browse list with the notice `'<name>' is not
defined`. The keyboard route to one name's removal is the scope expression
dialog: `frame::scope_expression` opens it in Whole mode with the frame's
names staged as chips, backspace at the field's start removes the last one,
and Enter applies the rest (see
[input and dialogs](input-and-dialogs.md#frame-expression)). A scope whose
only content is a name is not empty: the chips row and the save glyph paint
for it.

The load glyph (a folder-open icon, `scope-load-chip`) follows the `+` and
paints whatever the scope holds, empty included; a click opens the scope
picker (`frame::scope`, `mod+o`; see
[input and dialogs](input-and-dialogs.md#grouping-scope-tile-log-and-column-choices)),
and the glyph holds its pressed fill while the picker is open. The save
glyph, when the scope is savable, comes after it, so its appearance never
moves the load glyph.

The `+` verb opens the "Add a filter" menu under itself: "Dimension…"
dispatches `frame::pick`, "Expression…" dispatches `frame::add_expression` (whose dialog offers the
named expressions beside typed text), and each row shows its action's live
binding, if any. The `+` holds its
pressed fill while the menu is open. The menu is shell-owned transient state
(`shell/addfilter.rs`), not gpui-component's `PopupMenu`, because its rows
dispatch the shell's string actions and label them from the shell keymap.

### Link groups

A link group ties tiles together more narrowly than a workspace does. There
are four, fixed: A, B, C and D (`geode_core::link::Group`). A tile may follow
one group and emit into one, the same group or two different ones. A group
carries a scope and a board of draft documents. It carries no grouping and no
as-of: a follower keeps its workspace lane's.

**Following.** `Frame::view_for(ws, tile)`, which a tile's `FrameRef`
resolves to, composes the tile's reading: the workspace lane, with the scope
and the scope generation of the group the tile follows in place of the
lane's. `effective_scope` composes the group's scope with the tile's own
layer; the lane's scope is not composed in, the group replaces it. A follow
or an unfollow changes the tile's scope generation, so the tile requeries by
itself and nothing else waits for it. (The one exception is a lane and a
group that were both never written: both hold the empty scope at generation
zero, equal content under an equal number, so nothing requeries and nothing
needs to.)

Only a tile whose reading takes the frame's scope can follow: its module
answers `TileContent::follows()` true, which the blotter and the pricer do
(they query under it) and the vol slice viewer does (it reads its underlying
from it). A timeseries tile and a market-data panel never read the scope and
answer false, so the chooser offers them no follow row and `set_follow` refuses
them: a group's chip over content the group does not select would misreport
what the tile shows.

Group scope generations come from the same frame-wide counter as the lanes',
so a number names one scope in any lane or group.

**Writing a group's scope.** A group's scope is written by its emitters (see
below). Through a following tile's own handle (`FrameRef::update`),
`set_scope` and `clear_scope` also write the followed group's scope instead
of the lane's; a group keeps no undo history, and no module calls them. Every
other scope verb (the text session, undo and redo, dimension and term edits,
loading and saving a scope) reads and writes the workspace lane even through
a follower's view. The scope bar edits the workspace: its handle is bound to
no tile, and its model is built from the lane.

**Membership** lives on the frame, keyed by tile id. A module stores no
group: its header reads its chips from the frame at paint
(`geode_tile::header::link_chips`), so a change made through the shell cannot
leave a stale copy in a tile. The shell's doors are `ShellView::set_follow`
and `set_emit`. They refuse, with a debug log line naming the tile and the
reason, a tile id with no occupant and a placeholder: a membership lives
until its occupant closes or is replaced, and one written for either would
have nothing to end it. `set_follow` never sets a tile following whose
module answers `TileContent::follows()` false, and `set_emit` never sets one
emitting whose module answers `emits()` false. Closing a tile, or filling a
placeholder in place, drops the tile's membership after `TileContent::closed`
(a closing follower answers the flip barrier under its group's identity,
which it reads only while still a member). Duplicating a tile copies its
serialized module state, which holds no membership: the copy starts in no
group.

**Emission is a pull.** A module has no door to a group. The frame's
membership and emission writes (`follow`, `emit`, `post_emission`,
`forget_tile`), the tile-bound writable view and the barrier's per-tile
keying are private to the shell crate, so a module, which reaches the frame
through its handle, cannot link a tile or post for one. Its only write for a
group is `set_scope` / `clear_scope` through its own handle while it follows
one. (A module does write the frame for other things through that handle:
it reports its arrival at a flip barrier and registers its watches.) What a
module does is answer `TileContent::emission()`: an optional scope and a
list of board entries. The blotter and the pricer post the cursor row's one
underlying as a one-value `underlying_ref` scope
(`geode_core::link::underlying_scope`); a market-data panel posts its
underlying and, while its draft is not clean (`Editing`, `Behind` and `Sent`
alike), its draft document (see [features](features.md)). Each entry carries
a `DraftMark` (`Editing`, `Behind` or `Sent`, with `label()` the word a
follower shows, none for a live edit): the rows alone cannot tell a live
edit from the old base held behind a newer document or an upload awaiting
its echo. A follower shows the mark's word beside the draft (the vol slice
viewer's `cvi draft · behind`). A tile's content is type-erased
(`Box<dyn TileContent>` beside an `AnyView`), so the shell cannot observe
the tile's entity. `TileContent::watch_emission(changed)`
has the module subscribe to its own entity and call `changed`, which carries
nothing. The shell holds that subscription exactly while the tile emits. On
each call it defers a pull until the update that called it has finished, then
reads `emission()` and posts it with `Frame::post_emission`; deferring lets a
module call `changed` from inside its own update. Joining a group pulls once
at once, so the group hears the tile's current emission without waiting for
its next change.

A post writes only what changed:

- An emission equal to the tile's last (the same scope, and board entries
  comparing equal by allocation, `Arc::ptr_eq`, and by mark) writes nothing
  and notifies nobody, so a pull on every notification of an emitting tile
  is cheap. It also does not retake a board key another emitter posted
  since, or restore a scope another writer moved.
- An emission with no scope leaves the group's scope as it is. The cursor
  resting on a row that names no single underlying does not clear the
  group.
- A scope equal to the group's draws no generation. A tile that follows and
  emits into one group therefore does not loop: its own emission moves the
  group once, the frame change reaches the tile, the shell pulls again, and
  the equal emission ends it there.

**The board** is derived, not stored: each group's board is rebuilt from the
last emission of every tile emitting into it, the latest post winning a key
(dataset plus document key). An entry leaves the moment its emitter stops
listing it, leaves or switches group, or closes, and a key another emitter
still posts is uncovered, not lost. The group's scope stays as last written
when an emitter leaves. A tile switching group takes its entries off the old
board at once and puts nothing on the new one until its next post, which the
shell pulls as it joins.

A reader holds a `BoardWatch` (`Frame::watch_board`, for a dataset or one
document key, or a key prefix at a part boundary) and compares its
`revision()`; `Frame::board_entry` returns the posted `BoardEntry` (its rows
and mark) and `board_gen` counts a board's changes. A key changes when its
entry arrives, leaves, or holds another allocation or another mark, so a
draft falling behind or being sent moves the watch with the same rows.
Board changes move only these revisions. They never move the frame's `data`
version, so a draft edited at typing speed does not requery
tiles that watch published data, and they are never staged behind a flip
barrier: a draft is not a publish. Watches are held weakly and reaped at the
next registration.

**Flips.** A flip barrier keys each awaited tile to its own identity
(`Frame::open_flip_each`), because a follower's scope generation is its
group's: enrolled under the lane's, its arrival would never match and every
flip beside a follower would wait out the 250 ms deadline. The shell keeps
two baselines, the active lane's versions and the four group scope
generations. When the lane moved, it replaces the barrier over every visible
tile, each under its own reading. When only a group's scope moved, it calls
`Frame::extend_flip` with that group's visible followers: an open barrier
keeps its other keys and its deadline and takes the followers under their new
identity, and with no barrier open one opens over those followers alone. A
group with no visible follower adds nothing and opens nothing. A follow
change opens no barrier; if a barrier already awaits the tile, its key is
re-identified (`Frame::reidentify`) so the tile's next answer still matches.
The group baseline is not re-seeded on a workspace switch: the numbers are
frame-wide, and re-seeding would swallow a group change whose notification is
still pending.

**The chooser.** `tile::link_group` (`mod+u`, the palette, and a click on the
status bar's `following` segment) opens the link chooser on the focused tile
(see [input and dialogs](input-and-dialogs.md#grouping-scope-tile-log-and-column-choices)).
It lists the follow rows for a tile whose module follows and the emit rows
for one whose module emits. A tile that does neither (a timeseries tile) has
nothing to choose: no dialog opens and the status bar reads `this tile has
no link group to join`. A press on a tile's header link chip opens the
chooser on that tile: the chip queues `Frame::request_link_chooser`, since a
module never reaches the shell, and the shell's frame observer drains it,
focuses the chip's tile (it may not have held focus) and dispatches
`tile::link_group`. The press stops at the chip. The request moves no frame
version and so opens no flip.

**The chip.** A tile in a group shows a solid chip per group in its header:
the group's color as the fill, the letter and the role arrows as text floored
against that fill. `geode_shell::link::group_color` resolves each group's hue
(215, 25, 285 and 130 degrees) between the theme's anchors and floors it
toward 3:1 against the theme's background; the header paints it unchanged. A
sweep over every bundled theme holds each painted fill to that ratio, its
text to the 4.5:1 text ratio on that fill, and the four fills a minimum
distance apart. A custom theme is not swept and can fall short. The letter
is the identity; the color is a second cue.

**Session.** A tile's record carries `follow` and `emit`, and each group's
scope is saved under `[links.<letter>]` (see
[session format](#session-format)). A group keeps its last scope across a
restart: it is set on the group before any tile exists, so a restored
follower's first query reads it with no emitter present. Restored empty, a
follower that showed one underlying would show the whole book under the same
chip, and an emitter restored on a total row posts no scope to correct it. A
restored emitter is subscribed and pulled once after the first render, and
moves the group only when its cursor names an underlying. A group's board is
not session state: it is rebuilt from what its emitters post.

Moving a group's scope advances the frame generation the session writer
treats as dirt, but the writer compares its snapshot without the groups'
scopes, so a group's scope alone writes nothing: an emitting tile's cursor
does not rewrite `session.toml`. Every snapshot the writer does write, and
the quit-time save, carry each group's scope as it is then. After a crash a
group's scope is therefore the one in the last session write, which may be
older than the last one the group held.

**Known limitations.**

- The 3:1 guarantee for a chip's fill is measured against the theme's
  background, which is the tile's. A chip painted on any other surface is not
  covered by the sweep.
- A flip barrier released during render-time reconciliation notifies
  nobody: GPUI drops a notification sent, while a window draws, to an entity
  that window read in its last draw, and the shell's render reads the frame.
  Two routes release one there: a tile that closes while it is the last key
  an open flip awaits, and a tile made visible (`set_visible(true)`) whose
  query submission is refused, which answers the barrier at once. Tiles
  holding staged results then stay on their pre-flip data until the next
  frame notification.
- A `:unscoped` blotter or pricer that follows a group ignores the group's
  scope, as it ignores the workspace's, while its header still shows the
  chip and the status bar still names the group.
- No tile reads a board. Emitting panels post their drafts and the watch
  interface is exercised only by tests; nothing displays a posted draft.
- A market-data panel cannot follow a group: it does not take its underlying
  from one.

## Module hosting and delivery

`TileContent` is the module boundary. An occupant supplies its key context,
handles actions and local commands, receives find events and deliveries,
reports focus ownership, and accepts visibility and stack state. Required
methods make lifecycle obligations explicit for every feature.

`Delivery` is an exhaustive enum. Adding a new outcome type forces every
occupant to decide how it handles that variant at compile time. Query, pricing,
and series outcomes route by tile key. A series fetch completion has no tile
key and is broadcast to visible occupants because several tiles may watch the
same `(identity, source)` pair.

The app bridge supplies these deliveries from a coalescing mailbox. See
[requests and UI delivery](request-delivery.md) for admission/refusal,
stale-result handling, publication routing, and window lifetime.

### Pages

`PageContent` and `PageFactory`, beside the tile seam in
[`module.rs`](../../crates/geode-shell/src/module.rs), host a surface that
replaces the tile surface instead of living in a tile. While a page is open
the tile surface, divider strips, drag catchers, and the command line are
neither built nor painted, and the page takes the tile surface's rect: from
the sidebar's right edge to the window's, and from below the toolbar (and
the historical as-of stripe) to the status bar. The toolbar, which is also
the window's title bar, the stripe, the sidebar, and the status bar stay,
and every toolbar control (scope field, chips, `+` menu, as-of chip,
grouping readout, pin) stays live over the page, so the frame's scope and
as-of stay visible and editable beside it. Modals, the palette, which-key,
the performance overlay, and notifications paint above the page as they do
above the workspace. A page owns its own inputs, has no `:` line, and
receives no deliveries. `geode-app` registers factories in a `PageRoster`; the shell
stays ignorant of any page's content, and a page changes application state
only through the `Diagnostics` request channels or the `ShellActions`
handle it was created with, never by holding `ShellView`.

The shell holds one page at a time, created by its factory on the first
open and retained for the window's lifetime so a round trip keeps its
state; only the open flag changes between toggles. Toggling a different
kind replaces the retained page after stashing its `serialize()` table, so
the replaced kind's state still reaches the next save and its next open.
A page opens through its registered `page::toggle_<kind>` action, from the
keymap, the palette, the sidebar button, or the status bar's diagnostics
summary, and closes through `page::close`, the same toggle, or any
`workspace::switch_N`, which closes the page and then switches. A toggle
naming a kind no factory registered logs a warning and opens nothing.

A tile opens the diagnostics page by queueing `request_diagnostics_page` on
the shared `Diagnostics` entity (its health chip does this); the shell's
diagnostics observer drains it through `open_page`, never the toggle, so the
request only ever opens. It begins as a dispatched action does: the crash
report's action tail records `page::toggle_diagnostics`, and the shell's
notice, stack list and add-filter menu expire. Under a modal it is then
refused with the toggle's `close the dialog first` notice.

While a page is open the key context stack is `page`, then the page's own
context, then `palette` when it is open. `workspace` and `tile` are absent,
so tile movement, dock, stack, `:`, and `/` bindings cannot fire into a
page; workspace switches are bound context-free for this reason. Action ids
the shell does not recognise go to the open page's `dispatch` instead of
the focused occupant. The palette still reaches every action id over a
page, so `ShellView::dispatch` refuses with a notice the ones that would
open chrome or change a layout the trader cannot see: the tile command
line, find, `tile::add` and every per-kind add action, `tile::open_with`,
`tile::context_menu`, `tile::autosize_columns`, and every `workspace::`, `dock::`, and `stack::`
action other than the workspace switches, which close the page first. The
refusal is one predicate in `input.rs`; `Close tile` over a page would
otherwise destroy an unseen tile with no undo.

`page::toggle_*` and `page::close` are refused with the close-the-dialog
notice while a modal is open: the dialog was opened over the page and
would otherwise be left over a workspace it did not come from. The modal
and palette routes take Escape first, so Escape reaches `page::close` only
with nothing above the page; see
[keyboard ownership](input-and-dialogs.md#keyboard-ownership). The page
sees `page::close` before the shell acts and may consume it by returning
`true` when it has something of its own to dismiss; otherwise the shell
closes it.

Opening focuses the page's handle; closing returns focus to the shell
root. A page reports `holds_focus` while one of its inputs owns the
keyboard, and its context then carries `mode == insert`: the insert route
consults the open page before any tile, so bare keys type into the input
and only chords resolve against the stack. A page's bare-key bindings
therefore live in a `mode == normal` table
([keymaps](keymaps.md#context-predicates)). Focus restoration after an
overlay closes, and after Escape or Enter in the scope field, goes to the
open page's handle (`focus_home`) rather than the shell root. A press on
bare chrome (the toolbar background, the sidebar, the status bar) can still
leave focus on the shell root; the page's bindings keep working there,
because the context stack follows the open page, not the focused element.
Chords typed in the scope field resolve
against `workspace` over a page as they do without one. A mouse open calls
`prevent_default` so the press cannot bubble to the shell root and take
the focus back.

Tiles beneath a page are hidden: the occupant reconciliation announces
`set_visible(false)` to every occupant, which releases their watched
demand, and `visible_tile_keys` is empty, so no flip barrier waits on a
tile nobody can see. Closing shows them again. A divider or tile drag in
flight is cancelled when a page opens.

The sidebar paints one button per registered page between the workspace
discs and the settings avatar, in roster order, with the factory's icon
and a tooltip naming the page and its toggle binding. The open page's
button takes the active workspace disc's treatment and no pointer states,
because selected must stay distinct from hovered.

A page persists as `[pages.<kind>]` in the session file, written from
`serialize()` on the coalesced flush; a page-state change with no shell
action behind it still flushes, because the flush compares the snapshot.
Whether a page was open at quit is not saved; Geode starts in the
workspace. A table for a kind that never opens, or that no factory knows,
is kept as read and written back unchanged; a `pages` entry that is not a
table warns and is dropped.

## Diagnostics state and demand

[`Diagnostics`](../../crates/geode-shell/src/diagnostics.rs) holds operational
state beside the frame. The app bridge supplies data events; the shell
supplies config-load diagnostics. The model does no I/O or clock reads and
has no GPUI context. Callers supply timestamps and notify after mutation.
The [diagnostics page](features.md#diagnostics) prepares its own tables
from this state.

The combined `version` invalidates the cached status summary. `DiagVersions`
provides narrower counters so the page can ignore updates its selected
section does not read:

| Counter | Changes |
|---|---|
| `sources` | Source description, health/detail, poll times, ingest activity, stopped data threads |
| `data` | Publication events and catalog snapshots |
| `config` | Current config-load batch, batch history, retained data conditions |
| `log_levels` | Target-level settings |
| `perf` | Copied frame histogram, dropped-event count, and the mirrored overlay value |

Equal snapshots leave their counters unchanged; loading and publication events
always advance theirs. Frame as-of/config versions and log ring sequences are
separate inputs observed by the page. Catalog resource metrics and frame
requery statistics have no dedicated perf invalidation, so they appear on the
next perf-section rebuild.

`set_catalog` stores the snapshot together with the arrival time its caller
supplies, `catalog_at`. An equal snapshot changes neither the counters nor
`catalog_at`: the page's header shows when the stored answer arrived, not
when the database was last asked, so a repeat answer that changes nothing
does not move it.

Source health remains absent until the first report, even if description or
poll events created the source entry. Unreported sources do not contribute to
the status summary. A changed health or detail records a transition, including
recovery, with the most recent 16 retained per source.

Each source's description carries the dataset it loads into
(`SourceSummary.dataset`, from `SourceSpec.dataset` at attach). A tile asks
`health_for_datasets` or `health_for_sources` for a `TileHealth`: the worst
unhealthy source it reads by `Health::severity`, the lowest name on a tie, the
reason (the health's own, else the source's detail) and how many other read
sources are also unhealthy. Only described, reported sources count for the
dataset question; Ok and Pending are silent; an unknown dataset or source is
ignored.

Config loads replace the current diagnostic batch; a clean load clears it and
an identical load adds no history. The model retains the latest 16 changed
batches, including the current one. Data-layer conditions append separately,
deduplicate against retained entries, and keep at most 256; above that the
oldest warning or info is evicted first, and an error only when nothing but
errors remains, so a flood of benign warnings cannot hide schema drift or a
worker failure. Config reloads do
not clear them, and they have no per-condition resolution operation. The
summary counts current config errors and retained data errors independently;
history is excluded. Cache hits share an `Rc<str>` without copying text.

Catalog demand has two lifetimes. `watch` registers a visible page and queues
its initial refresh; publications and visible as-of changes queue watched
refreshes. The last `unwatch` clears that pending demand. `request_catalog`
records an explicit consumer's request, which survives diagnostics hiding.
When both are pending, `take_catalog_request` consumes them together as
explicit demand, preserving its retry policy.

Two request channels let the page change application state without
holding `ShellView`. `request_level` queues a target and level; the shell
drains it, applies the new `[log]` levels to the log control, and persists
them to the user layer. `request_overlay_toggle` queues a flip of the
performance overlay. The shell sets the overlay through one door for both
the keyboard action and the drained toggle and mirrors the value back with
`set_overlay_visible`, which advances the `perf` counter so the page's
switch repaints; the switch and the overlay cannot disagree. Request
methods do not notify observers themselves; the caller notifies in the
same entity update.

Demand changes do not advance data versions. Callers must still notify in the
same entity update so the bridge observes them. The bridge allows one catalog
request in flight, coalesces follow-up demand, and retries busy-refused or
failed requests with a delay. A request refused because the data service has
stopped drops its demand instead: nothing can serve a retry. It reads the
current frame as-of when submitting.

Histogram copying is limited to watched diagnostics and changes in sample
count or maximum. This keeps idle polling from causing needless repaints,
but misses changes solely to discarded-idle counts or a reset/refill that
reproduces the same count and maximum.

### Stopped threads and refusals

`Diagnostics::stopped` lists data threads that died despite containment (a
`DataEvent::ThreadStopped`), in report order, each with its spawn name, a
readable label (`thread_label`: `data service`, `ingest`, `discovery`,
`pricing`, `query worker N`, `fetch <source>`, `subscription <source>`,
`egress <target>`), the reason, and the arrival time. `note_thread_stopped`
records each thread once — a redelivered report is a no-op — rebuilds the
prepared `StoppedSegment`, and advances the `sources` counter. Nothing
removes an entry: a stopped thread stays stopped until Geode restarts, so the
segment never clears.

The stopped segment is the first thing on the status bar's left side, before
the count prefix, in the danger tone. Its text is one of:

- `<label> stopped` for one thread, for example `ingest stopped`;
- `N data threads stopped` for two or more;
- `data service stopped — restart Geode` whenever the request loop
  (`geode-data`) is among them, including a service that failed to open. The
  loop outranks the rest, because every other count on the bar then
  describes a service that no longer runs.

The tooltip carries the reason: the one thread's reason; `label: reason`
pairs joined by `; ` for several; or the loop's reason followed by `; also
stopped: …` naming the others. Its detail line reads `click to open
diagnostics`. A click takes the diagnostics summary's route: it dispatches
`page::toggle_diagnostics`, the same action as `mod+d` and the sidebar's
diagnostics button, so the diagnostics page opens over the workspace (or,
already open, closes). The page keeps whatever section it was showing.

**Known limitation:** the page's Sources section does not list stopped
threads; the segment's tooltip is where the reason is read, and the
segment stays on the status bar whatever the page shows.

`Diagnostics::refused` holds the data handle's cumulative count of `Busy`
refusals since launch (submissions turned away by a full request queue);
`Stopped` refusals are not counted: they describe a service that is gone,
not one that is behind, and the stopped segment already says so. The status summary shows it as `N refused` after `N dropped`
(dropped app-bridge events), omitted at zero. `note_refused` advances only the
combined version; no page section shows the count.

**Known limitation:** the bridge reads the refusal total only when it drains
an event, so a refusal made while no events flow appears at the next event.

## Persistence and configuration

### Session format

`session.toml` in the user configuration directory stores layout and transient
state. It is loaded once at startup and excluded from configuration reload
change detection. Theme and other durable preferences live in layered config.
With no configured session path, the shell skips persistence.

The writer emits `config_version = 1` and these records:

| Record | Contents |
|---|---|
| `active` | Workspace index, 1–9 |
| `workspaces.N` | Main tree, focused tile, optional fullscreen tile, and focused region |
| `workspaces.N.docks.<side>` | Left, right, or bottom dock tree, focused tile, visibility, and size |
| `workspaces.N.tiles.<id>` | Module name, its opaque state table, and the link groups the tile is in: `follow` and `emit`, each a group letter `"a"` to `"d"`, written only when set |
| `frame` | Dimension selections, named-expression references, text/expression scope, grouping slot, and as-of |
| `workspaces.N.frame` | Pinned lane for workspace N (same fields as `frame`); present iff workspace N is pinned |
| `links.<letter>` | One link group's scope, under `scope`, in the encoding `frame` uses for its own (dimension selections, text, expression, named-expression references); written only for a group whose scope is not empty, and an absent table reads as an empty scope |
| `palette.usage` | Per-row usage count and last-used timestamp |
| `pages.<kind>` | One opaque table per page kind from `PageContent::serialize`, kept for kinds that never opened this session; the diagnostics page writes its `section` |

Trees use recursive `leaf`, `split`, and `stack` nodes. Splits store orientation,
children, and ratios; stacks store tile IDs and the active member index. All
materialized workspaces are saved, including empty ones. Default docks, empty
module state, empty usage history, and an empty pages table are omitted. Only
tile records whose IDs belong to the workspace's main or dock trees are
written. Whether a page was open is not recorded.

The legacy dock `tile = N` encoding loads silently as a single-leaf tree. If
both `tile` and `node` appear, `node` wins with a warning. Unknown keys are
ignored during loading and lost on the next save; serialization reconstructs
the document and does not preserve comments. See
[`session.rs`](../../crates/geode-shell/src/session.rs) for the node encoding.

### Recovery and restoration

A missing file starts a fresh session without warnings. An unreadable file,
invalid TOML, or rejected session starts fresh with warnings logged by the app.
A missing version warns and assumes version 1; any other present version value
rejects the session. Loading and recovery do not rewrite or back up the file;
a later save replaces it with the current state.

| Problem | Recovery |
|---|---|
| Invalid workspace index, workspace shape, or main-tree structure | Reject the entire session, including tile records, frame state, and usage |
| Positive finite split ratios with valid arity | Normalize the ratios |
| Repeated/already-claimed stack members, invalid active index, or fewer than two surviving members | Prune members, reset the index, and collapse or remove the stack as needed |
| Dangling focus or fullscreen ID | Drop the reference; give a nonempty tree a valid focused tile |
| Invalid dock subtree | Drop that dock's tree with a warning; retain the workspace |
| Duplicate dock tile claims | Main trees take priority, then earlier docks; prune duplicate dock claims with warnings |
| Hidden focused dock | Choose a fallback region; visible empty docks remain valid focus targets |
| Main fullscreen combined with dock focus | Clear fullscreen and keep dock focus, with a warning |
| Malformed or locally dangling tile record | Warn and drop the record |
| A tile's `follow` or `emit` is not a string or names no group | Warn and read that key as unset; keep the tile, its module and its state |
| `links` is not a table, a key under it names no group, a group's entry or its `scope` is not a table | Warn and read that group's scope as empty; the other groups and the rest of the file still read. A scope's own fields recover as `frame`'s do |
| Invalid optional frame or palette data | Retain usable fields/entries and warn for the errors their readers report |
| `workspaces.N.frame` is not a table | Warn; the workspace restores unpinned |

Frame restoration parses scope expressions but does not validate columns
against the current schema. Invalid slots, expression syntax, date strings,
and non-array dimension entries warn; some wrong-type fields and non-string
dimension values are silently ignored. `Scope::impossible` is not persisted.
`named` restores as an ordered, deduplicated list of strings the same way a
saved scope's does — a non-array value warns and is dropped, but names are
kept without checking they are defined; a missing or invalid one becomes the
frame's resolution error on the next query rather than failing restoration.
The shell applies scope, slot, and as-of, then clears scope history. Undo/redo
history and recent publishes start fresh; saved scopes come from config.
A `workspaces.N.frame` table is read by the same reader, so a partial record
keeps its usable fields. Each restored pinned lane is pinned, filled, and has
its scope history cleared like the shared one, before the flip baseline is
seeded, so a restored lane never reads as just changed. Its active slot is
cleared before the recorded one applies, so a recorded slot that is now empty
leaves the lane with no slot rather than the shared lane's. A pinned record
whose workspace the restored layout lacks is skipped with a warning; this is
a defensive check, since one session read supplies both the layout and the
pins and cannot produce such a record.

The shell creates occupants from restored records through the module roster.
Only the factory matching a record's module name receives its state. An
unavailable module displays a placeholder and retains the original record for
subsequent saves. Closing the tile discards it; filling the placeholder replaces
it with the new occupant's state. Restored IDs seed subsequent tile allocation
so ordinary additions do not reuse them.

A record's `follow` and `emit` are applied to the frame before any occupant
exists, so a restored follower's first query is already scoped by its group.
They are applied only to a record whose module has a factory and whose tile
is placed in some workspace, on screen or not: any other tile will never have
a module occupant to end the membership. A record behind a placeholder keeps
its two keys as read and writes them back on every save, so the membership
returns with the module. A restored `follow` on a tile whose module does not
follow, and a restored `emit` on one whose module does not emit, are cleared
once the occupant exists. A restored emitter is subscribed, and its emission
pulled, after the render that created the occupants: pulled between two
occupants of one pass, a follower would start on a different scope according
to whether its tile id sorts before or after its emitter's. Restoring a
membership moves no lane version and no group scope generation, so it opens
no flip.

Each `links.<letter>` scope is set on its group before the memberships are
applied and before the flip baseline is seeded, without a notification. It
draws the group a scope generation from the frame's counter like any group
scope write; because the baseline is seeded after it, the restored scope is
not read as a change and opens no flip.

### Saving and failure behavior

Workspace actions mark layout state dirty and return without session file I/O.
The shared reload watcher checks for a session snapshot on its 500 ms tick,
before any configuration-scan early return. It also compares serialized module
state, the frame's generation counter, and palette usage versions, so those
changes can trigger a save independently of layout dirt. This is periodic
coalescing, not a timer reset after each action; other work adds to the
interval. A link group's scope moves with an emitter's cursor and advances
the frame's generation each time, so the snapshot is compared, as text
without the `links` tables, with the last one extracted, and an equal one is
not written again. A group's scope alone therefore never causes a write; the
text handed to the file includes the `links` tables whenever one is written.

Snapshot collection and TOML serialization run on the UI thread. The watcher
awaits the file write on the background executor before continuing its loop.
The layout dirty flag clears before serialization, and comparison baselines
advance when serialization succeeds, before disk I/O. A failed write is logged
but does not retry unchanged state on the next tick. A serialization failure
leaves comparison baselines unchanged, but a layout-only change can lose its
dirty flag. These baselines track extracted snapshots, not confirmed saves.

The quit hook saves current state synchronously regardless of dirty flags,
the link groups' scopes included.
Session writes use the shared atomic replacement primitive: a unique sibling
temporary file, file sync, then rename. Readers see a complete old or new file;
the directory is not synced, so durability across system failure is best-effort.
Errors can leave temporary files. Session writes bypass the configuration
submission queue and transaction lock. The quit hook does not join an in-flight
periodic save, so the last rename can leave an older snapshot on disk. Separate
processes writing the same session path likewise have no ordering guarantee.

### Configuration writes and reloads

The [configuration-dialog contract](configuration-dialogs.md) covers draft
ownership, stages, validation, inherited objects, and the boundary between
local application and successful persistence.

Runtime configuration writes target only the user layer. `config_write` is
the common door for ordered, atomic edits. Accepted writes to one directory
run in submission order on the background executor. The target file is parsed
before editing, temporary files live beside it and do not end in `.toml`, and
rename exposes either the old or new complete file to the reload poll.

Hot reload keeps the last valid configuration when a changed document is
rejected by the file, modifier, clock, or keymap checks. Later typed readers
do not roll back the whole reload. Every accepted reload advances the frame's
config revision and republishes `Chords`; the other module-visible globals
update only when their values change. See the
[reload contract](configuration.md#hot-reload) for detection, validation,
notification ordering, and restart requirements.

## Presentation rules

Shared builders own repeated shell presentation: semantic chips, list rows,
control interaction states, colors, and the rem-based geometry scale. Use
stable domain-derived element IDs. A list row paints its highlight and
hover through `listrow::paint_row`, which takes only an identified
element: gpui repaints on a hover transition only for an element with an
id, so an id-less row's hover fill lags the pointer until an unrelated
repaint. Stateful inputs, lists, and tables are
created once and retained rather than rebuilt during render.

Every pressable element on the toolbar calls `occlude()`. `TitleBar`
starts a window move from any press that reaches its own hitbox, even one
a child has already handled. On Windows its drag area also answers the
caption hit test. Occluding keeps both from seeing a press on a control,
so dragging in the scope field selects text. Only bare title-bar space
moves the window.

Theme values provide colors and radii. Chrome geometry is authored against
the rem scale so application zoom changes the interface coherently. Prepared
models and caches must include every value that can change their output,
including theme or rem revisions where geometry is involved.

## Verification and limitations

Pure tests cover tiling, key resolution, frame transitions, parsing, and
ranking. GPUI context and window tests cover focus, keyboard and pointer
routes, dialogs, occupants, and persistence. Tests should exercise the public
interaction route for focus-sensitive behavior; calling an internal mutation
does not prove that keys reach it.

The `test-support` feature exposes the recording occupant and test accessors
used by feature crates. CI builds that feature explicitly. Visual facts still
require a real-window display check when headless tests cannot observe color,
exact geometry, or animation.

See the [geode-shell crate README](../../crates/geode-shell/README.md) for the
module map and commands.
