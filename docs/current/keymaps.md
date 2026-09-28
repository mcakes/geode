# Keymaps and actions

Geode resolves keys to registered action IDs. The action registry supplies the
palette title and category; configuration supplies bindings. Modules contribute
actions and default keymap fragments without requiring the shell to depend on
module crates.

The implementation is in
[`keymap/`](../../crates/geode-shell/src/keymap/mod.rs),
[`defaults.rs`](../../crates/geode-shell/src/defaults.rs), and
[`keymap_edit.rs`](../../crates/geode-shell/src/keymap_edit.rs).
See [configuration](configuration.md) for file loading, reload acceptance,
and write durability, and [shell input](shell.md#actions-and-keyboard-routing) for focused
inputs and overlays that handle keys before ordinary matching.

## Documents and precedence

Bindings live in `keymap.toml`:

```toml
config_version = 1

[[bindings]]
context = "workspace"
[bindings.keys]
"mod+h" = "workspace::focus_left"
"mod+l" = "none"
```

Keymap compilation consumes the original documents, rather than the generic
merged `bindings` array. The application orders them as shell builtins, module
fragments, desk, then user. The compiler preserves the supplied document order
and each document's `[[bindings]]` array order. Within one keys table, it sorts
by the original key spelling, independently of TOML declaration order.

At each press, the last matching exact binding wins. This allows a later entry
to replace an action or assign `"none"` to disable a lower binding. Context
specificity has no separate priority: a later context-free binding also wins
over an earlier contextual binding whenever the sequence is identical.

Two different spellings can normalize to the same key. With Alt as `mod`,
`"alt+h"` and `"mod+h"` in one table are both legal TOML keys; alphabetical
sorting puts `"mod+h"` last, so its action wins that tie.

Malformed entries, predicates, key sequences, and non-string actions produce
error diagnostics and are skipped. Unknown action IDs produce warnings and are
skipped; `"none"` is accepted without registration. An action's owner may
register a retired ID with `ActionRegistry::register_rename` (`config::colours`
→ `config::colors`, `timeseries::colour` → `timeseries::color`,
`timeseries::pick_colour` → `timeseries::pick_color`, `blotter::visual` →
`blotter::visual_rows`, and each module's retired motion and menu-step ids →
`motion::*`, listed [below](#retired-motion-ids) and in the module's
`RENAMED_ACTIONS`). Several retired IDs may rename to one current ID; each
retired ID is still registered once. A binding naming a retired ID binds the
current action and warns with both IDs, so an existing
user keymap keeps working until the file is edited; the dialogs write only the
current ID. `register` refuses a retired ID, so a later registration
cannot be silently redirected. Startup can use the remaining
compiled bindings. On reload, compilation errors participate in the shell's
last-good acceptance gate; see [reload](configuration.md#hot-reload).

`blotter::visual_rows` selects whole rows; `blotter::visual_block` selects a
cell rectangle. The compatibility name `blotter::visual` resolves to row
selection.

### Retired motion ids

Every module-local motion and menu-step id is retired and registered as a
rename of a `motion::*` id. The suffix decides the target, the same in every
module that had it:

| Retired id | Current id | Modules |
|---|---|---|
| `…::down`, `up`, `top`, `bottom` | the same `motion::*` name | blotter, marketdata, pricer, diagnostics |
| `…::left`, `right` | the same `motion::*` name | blotter, marketdata, pricer |
| `…::page_down`, `page_up` | `motion::half_page_down`, `half_page_up` | blotter, marketdata, pricer, diagnostics |
| `…::page_down_full`, `page_up_full` | `motion::page_down`, `page_up` | blotter, marketdata, pricer, diagnostics |
| `…::first_col`, `last_col` | `motion::line_start`, `line_end` | blotter, marketdata, pricer |
| `…::menu_down`, `menu_up` | `motion::menu_down`, `menu_up` | marketdata, pricer |
| `timeseries::list_down`, `list_up` | `motion::menu_down`, `menu_up` | timeseries |

A user binding on a retired id keeps its own context (for example
`blotter && mode == normal`), so it still reaches only that module's tile,
and loading it warns with both ids.

## Key spelling and primary modifier

A binding is a whitespace-separated sequence of keystrokes, such as `"g g"` or
`"mod+shift+h"`. A keystroke joins modifiers and one key with `+`. Modifiers are
`ctrl`, `alt`, `shift`, `cmd` (also `super` or `win`), and `mod`. Parsing folds
ASCII case but never infers Shift from a capital letter: `G` parses as `g`,
whereas `shift+g` retains Shift. The parser accepts arbitrary non-modifier key
names; successful parsing alone does not prove a platform can deliver that key.
A literal `+` cannot be represented through this separator syntax.

The primary modifier is configured separately in **`app.toml`**:

```toml
[keymap]
mod = "alt"
```

`alt` is the default; `cmd` selects Command. The exact value `ctrl` returns an
error and falls back to Alt because it conflicts with shipped literal Control
bindings. Missing, non-string, unknown, and differently cased values silently
fall back to Alt.

Platform event spelling matters. The pinned macOS and Windows backends report
shifted punctuation as its shifted character with Shift cleared, so dock move
bindings use `ctrl+{`, `ctrl+}`, and `ctrl+?`. Shifted letters and arrows retain
the explicit modifier, such as `mod+shift+p` or `shift+left`.

## Context predicates

The shell supplies a stack from outermost to innermost context. On the tile
surface it is `workspace`, then `tile` and the focused occupant's own
context when one is focused, then `palette` while it is open. While a
[page](shell.md#pages) replaces the workspace it is `page`, then the page's
own context, then `palette`; `workspace` and `tile` are absent, so the
bindings in those tables stay inert until the page closes. The builtin
keymap binds `escape` to `page::close` in the `page` context and binds the
workspace switches `mod+1` to `mod+9` context-free, beside the palette
toggle and the other application-wide chords, because a switch is
navigation that must reach from a page as well as from the tile surface.
Predicates support flags, comparisons, boolean operators, and parentheses:

```text
workspace
blotter && mode == normal
!modal && (blotter || marketdata)
mode != insert
```

A module can push more than one key onto its own frame. The blotter's, the
market-data panel's and the line pricer's `key_context` push `mode == visual`
while a grid selection is live (the same flag `mode == normal` above tests for
its absence) and, only then, a second `select == rows` or `select == block`
pair naming the selection's kind — `blotter && select == rows` reaches only
while a `V` row selection is live, never a `v` block one. The market-data
panel and the pricer push their `select` pair whenever a selection is live,
including while an editor or the action menu is open over it.

The market-data panel reports one of `normal`, `visual`, `menu`, or `insert`.
An open editor, picker, choice field, or upload confirmation is `insert` even
while a selection is live, so the editor's `enter`, `escape`, and arrows keep
their insert-mode meaning over a selection; the action menu is `menu`. Its
motions are the shared grid motions (the panel publishes `grid` in every
mode, and the shared bindings match only `mode == normal` or
`mode == visual`); its `marketdata && mode == visual` block binds the
selection verbs as single keys — `y` (`marketdata::yank`), `d`
(`marketdata::delete_row`), `i` and `enter` (`marketdata::edit`), `v`, `V`,
and `escape` — because a doubled normal-mode form (`y y`, `y c`, `d d`) would
leave the first press waiting for a second. Normal-mode keys the block does
not list, such as `space`, `o`, `u`, `n`, and `.`, have no panel binding while
a selection is live; the palette still reaches their actions.

The line pricer reports `normal`, `visual`, `menu`, or `insert` the same way:
an open entry bar, cell editor (the date field included) or `:rm` question is
`insert` over a live selection, so the editor's `enter`, `escape` and arrows
keep their insert-mode meaning. Over a selection the arrows step every
selected cell only while the editor sits on an untouched qty, strike,
barrier or shift cell; otherwise they nudge the editor's text or the date
field's segment (see [features](features.md#selection-2)). An inherited shift
steps from the value it paints, in the live step and the single-cell nudge
alike. Over a selection, an `enter` that leaves the cursor cell as it opened
(an unmoved choice, an untyped date on its opening day, unedited text) writes
nothing and closes the editor. Its motions, arrows, `^`/`$` and `home`/`end`
included, are the shared grid motions (the pricer publishes `grid`); a motion
dispatched from the palette while the entry bar, the cell editor or the action
menu is open closes it first, then moves. The pricer's
`pricer && mode == visual` block binds `y` (`pricer::yank`), `d`
(`pricer::delete`), `shift+j`/`shift+k`, `g p`, `g u`, `i` and `enter`
(`pricer::edit`), `v`, `V` and `escape` as the selection's verbs.
Normal-mode keys it does not list — the doubled `y y`, `y c` and `d d`, `p`,
`shift+p`, `u`, `ctrl+r`, `o`, `shift+o`, `n`, `shift+n`, `space`, the `z`
folds, `g m` and `.` — are unbound while a selection is live; the palette still reaches
them. A palette verb closes an open editor first, as a cancel, so the
palette's `undo` mid-step takes the steps back and then undoes the entry
before them.

A flag matches when any stack frame carries it. A comparison uses the innermost
frame defining its key. Both `==` and `!=` are false when the key is absent;
`!(mode == insert)` therefore differs from `mode != insert` for a missing mode.
Within a frame, the first stored value for a repeated key wins.

Negation binds more tightly than conjunction, which binds more tightly than
disjunction. Identifiers accept ASCII letters, digits, underscores, and hyphens.
Comparison values may also use single or double quotes; quoted strings do not
process escapes. Empty expressions and malformed or trailing tokens are errors.
Unary negation and parenthesis nesting share a depth limit of 64.

## Sequences and counts

The matcher retains a pending sequence and optional count between presses. Each
press evaluates predicates against the context stack supplied for that press.

- An exact match dispatches immediately, even when a longer sequence has the
  same prefix. Binding `g` makes `g g` unreachable through that prefix.
- A prefix with no exact match remains pending while longer candidates exist.
- A dead end clears the sequence and count. Its final key is not retried as the
  start of a different binding.
- `"none"` clears state and returns `NoMatch`; it suppresses action dispatch
  without providing a separate event-consumption result to callers.
- The matcher has no timeout. Its `cancel` operation clears both sequence and
  count; shell transitions such as opening the palette, a modal, or a command
  prompt call it explicitly.

Counts are enabled only when the **innermost** context carries `counts`. Before
a sequence starts, bare digits accumulate to a maximum of 9999. A leading zero
remains an ordinary key; zero extends an existing count. Modified digits and
digits after a sequence starts are ordinary binding keys. The next matched
action receives the optional count; action handlers decide how to use it.

## Shared motions

The shell registers fourteen `motion::*` actions under the "Motion" category
and ships their keys once in the builtin keymap. It handles none of them:
`ShellView::dispatch` offers each to the focused tile's `dispatch` with its
count, and the tile interprets it through `geode_tile::motion`.

| Action | Keys |
|---|---|
| `motion::down` / `motion::up` | `j`, `down` / `k`, `up` |
| `motion::left` / `motion::right` | `h`, `left` / `l`, `right` |
| `motion::top` / `motion::bottom` | `g g` / `shift+g` |
| `motion::half_page_down` / `motion::half_page_up` | `ctrl+d` / `ctrl+u` |
| `motion::page_down` / `motion::page_up` | `ctrl+f`, `pagedown` / `ctrl+b`, `pageup` |
| `motion::line_start` / `motion::line_end` | `^`, `home` / `$`, `end` |
| `motion::menu_down` / `motion::menu_up` | `j`, `down` / `k`, `up` |

A tile publishes the `grid` flag (`KeyContext::grid`) when its grid cursor
takes motions, and the `tilelist` flag (`KeyContext::tilelist`) while one of
its menus or popup lists is open. Grid motions are bound under the single
context `grid && (mode == normal || mode == visual)`
(`defaults::GRID_MOTION_CONTEXT`), so one user override under that context, or
one rebind of a Motion row from the keybindings dialog, reaches every grid tile
in both modes; see [editing a Motion row](#editing-a-motion-row). A grid under
an open menu reports `mode == menu`, so its motions stay out while the menu
steps take `j`/`k`/arrows under `tilelist`. Within each context the named keys
are bound first and the vim keys last, so hints and the dialog show the vim
key. `g g` is the builtin keymap's one multi-key sequence; module fragments add
their own `g` sequences beside it.

The grid rules in `geode_tile::motion`:

- A bare single `down`/`up` wraps at the ends, except while a selection is
  live, where it clamps: wrapping past the anchor would invert the selection.
- Any counted move clamps, a count of one included.
- A counted `top`/`bottom` jumps to row N (1-based), clamped to the last row.
- `half_page_*` moves 5 rows and `page_*` 10 rows, times the count.
- `left`/`right` clamp; `line_start`/`line_end` reach the first and last
  column.
- On an empty axis every motion leaves the position unchanged.

Three tiles publish `tilelist`: the market-data panel and the line pricer while
their `.` action menu is open (beside `mode == menu`), and the timeseries tile
while its series list or one of its menus (action, range, frequency) is open
(beside `popup == series|menu`). The timeseries tile never publishes `grid`,
so its own `h`/`l` pan and `g`/`shift+g` jump are never shadowed. With no list
open the flag is absent and `j`/`k` fall through to the grid (or to nothing).
The retired menu and list step ids are renames of
`motion::menu_down`/`menu_up` (see [retired motion ids](#retired-motion-ids)).

What a menu step means (skipping disabled rows, wrapping a series list) stays
the list's rule; what a tile does around a grid result (entering a header
strip, closing a field, following a log) stays the tile's; see
[features](features.md#motion).

## Module defaults

A factory's `default_keymap` contributes a builtin-layer keymap with a synthetic
`<module:kind>` diagnostic source. Fragments are placed after shell builtins and
before desk/user bindings, so they participate in ordinary override and reset
behavior.

The fragment filter requires a string context whose first scanned identifier
belongs to the factory's declared contexts. It rejects `!`, `||`, and `(`
anywhere in the text, including inside quoted values; this also rejects `!=`.
Authors should use a bare owning flag followed by optional conjunctions, such
as `marketdata && mode == normal`. The ordinary compiler subsequently validates
predicate syntax, keys, and action IDs.

This filter is textual. It does not prove that the first identifier is used as
a flag rather than a comparison key, or that factories declare distinct context
names. Module authors must retain those ownership constraints. Malformed TOML
or filtered entries produce diagnostics without preventing the rest of the
module roster from loading. Retained fragment-filter diagnostics are shown on
reload but do not themselves reject a user edit; errors from the ordinary
compiler still participate in the reload gate.

A grid module's fragment binds no motions: they come from the shared
`motion::*` bindings under `grid`. The blotter's fragment binds only its
verbs (expansion, selection, yank, find, sort, `g m`, escape).
The market-data panel's fragment binds its verbs and its `mode == menu`
pick and close keys (`enter`, `escape`, `.`); the menu's steps are the shared
ones under `tilelist`. `k` on row 0 still enters the attribute strip around
the shared result.
The line pricer's fragment binds its verbs (`g p`, `g u` and `g m` among
them, beside the shell's `g g`: a first `g` waits for the second key), its
`mode == insert` field keys and its `mode == menu` pick and close keys.
The timeseries fragment binds its popups' `enter`, `escape` and `.`; their
row steps are the shared ones under `tilelist`.
The diagnostics page's fragment binds only its verbs (`[`/`]`, `z o`/`z c`,
`enter`, `/`) under `mode == normal` and `escape` under `mode == insert`; the
page publishes `grid` beside its mode, so the shared motions reach its cursor
in normal mode and stay out of the focused filter.

A fragment may name any action, not only ones its own module registers, so
long as its context is the module's own: both the blotter's and the pricer's
default fragments bind `g m` to the shell's `tile::open_with` inside their own
`mode == normal` context.

A `PageFactory` contributes its `default_keymap` the same way, checked
against the contexts it declares (its kind by default). Its toggle is
different: a page's `toggle_binding` (`mod+d` for diagnostics) must be
context-free to reach from the workspace, and a module fragment cannot carry
a context-free binding, so the `PageRoster` emits it as a separate
shell-generated document named `page:<kind>` that binds
`page::toggle_<kind>`. That document is not put through the fragment filter,
because the shell wrote it; it still compiles with the rest and a user
keymap can rebind or unbind it like any builtin.

A page's bare-key bindings carry `mode == normal`, for the reason a
module's do. While one of the page's inputs holds focus its context carries
`mode == insert`, and the insert route resolves bare keys against every
context carrying that pair, so a table on the bare `diagnostics` context
would fire `j`, `G`, or `enter` inside the filter instead of typing them.
The `mode == insert` table holds only the keys the input surrenders
(`escape`, which blurs it).

See [input and dialogs](input-and-dialogs.md#keyboard-ownership) for surfaces
that bypass sequences and counts while handling text, and for the limits of
palette binding badges and which-key hints.

## Editing, unbinding, and reset

The editor opens in Normal mode. `/` or clicking its frozen filter row enters
Filter mode and records the current query. Escape restores that query; bare
Enter keeps what was typed. Both leave filtering without starting a capture
or changing a binding. A second Enter in Normal mode starts capture on the
selected match. Capture has its own key handling; the filter exit rules apply
only while searching the action list. See [dialog filtering](shell.md#dialog-filtering)
for selection, focus, and Escape behavior shared with other dialogs.

The keybinding editor writes only `<user_dir>/keymap.toml`. It preserves original
context and key spellings because these identify entries in the source file:
rendering `mod+h` as `alt+h`, or normalizing predicate whitespace, would target
a different TOML key or context string.

| Operation | Persisted change |
|---|---|
| Rebind | Write the new key, then remove the old user key or write `"none"` over a lower-layer key |
| Rebind to the same spelling | Write the new value and skip displacement |
| Unbind a user key | Remove it, exposing any lower-layer binding |
| Unbind a builtin/desk key | Write a user `"none"` shadow |
| Reset one action | Remove its user bindings, shadows covering its live lower-layer bindings, and orphan shadows on their keys |
| Rebind or unbind a Motion row | Reset the action, then write the shared context ([below](#editing-a-motion-row)) |
| Reset all | Remove every user `bindings` entry, including hand-written entries; retain other fields |

Rebind and unbind use the **first** matching raw context string, creating an
entry when absent. Reset searches **all** matching entries and preserves empty
entries and their comments. A later duplicate context entry can therefore
continue to shadow a newly written rebind. The writer does not validate action
IDs, key syntax, or predicate meaning, and a successful write does not guarantee
that the new binding will win after reload.

A rebind whose old user key is absent still writes the new key and reports
`OldKeyNotFound`. Unbind reports whether a user key was removed; `false` can
mean a removal miss or a successful lower-layer shadow, depending on the
requested operation. Reset reports the actual removal count, which can exceed
the number of requested keys when context entries are duplicated.

The editor rejects malformed TOML and a present `bindings` value that is not a
`toml_edit` array of tables before writing. The compiler accepts inline arrays
such as `bindings = [{ keys = { ... } }]`, but editing those files requires
conversion to `[[bindings]]`. Both ordinary and inline `keys` tables inside an
entry are editable. Missing or non-table `keys` values are replaced with an
empty table. Existing key quoting and leading comments are retained when its
value changes; decoration attached to the replaced value is not retained.

Writes use the shared serialized configuration transaction and become active
through normal reload. The temporary `.tmp` file is outside the reload scanner's
TOML filter. Parse and shape errors leave the original file untouched; shared
[write semantics](configuration.md#runtime-edits) define I/O failure and
durability limits.

### Editing a Motion row

An edit of a `motion::*` row is global. An old-id override (say
`n = "blotter::down"` under `blotter && mode == normal`) keeps working with a
load warning and is what the row displays, but a rebind or unbind does not
write into that module context. In one transaction it first removes every user
override of the action, the same set `r` removes, then writes the shared
context: `GRID_MOTION_CONTEXT` for the grid motions, `tilelist` for
`motion::menu_down`/`menu_up` (`defaults::shared_motion_context`). A rebind
writes the new key there and a `"none"` over the shipped fallback key; an
unbind writes only the `"none"`, which `r` lifts. Capturing the fallback key
itself only clears. Re-capturing the displayed key is a no-op only when every
override already sits in the shared context.

`r` on a Motion row removes the old-id overrides in every module context and
their `"none"` shadows. A dialog rebind made before the rename wrote the old
module key and a `"none"` over the `j` the module then shipped, both under the
module's context. `j` now ships under the grid context, so the shadow covers no
lower binding by raw context string, yet it still silences `j` in that module.
Reset therefore also collects an orphan shadow: a user `"none"` that shadows no
lower-layer binding at all and sits on the keys of one of the action's live
lower-layer bindings. An orphan on keys several actions share is collected for
each of them; removing it only exposes lower layers. After the reset the Motion
row's shipped binding applies in every tile. `shift+r` removes every user
binding, old ids included.

## Display resolution and action registration limits

The dialog's effective binding and reset calculations do not have a live focus
stack. They approximate context coverage: a later same-sequence entry shadows
an earlier one if its context is absent or the two raw context strings are
identical. Logically equivalent or overlapping predicates with different
spellings can therefore appear independently in the dialog even though runtime
dispatch chooses only one. Reset associates an unbind with the live lower-layer
action under that same approximation, so a desk reassignment is not mistaken
for an override of the original builtin action.

The action registry is fixed at startup. New pickable columns and saved scope
names introduced through reload need restart to gain their derived action IDs;
existing scope actions use the refreshed saved scope contents. A configured
scope whose ID collides with an existing action is skipped with a warning.
Each module kind has four add actions: default split, horizontal split, vertical
split, and stack. The parser reserves the `_horizontal`, `_vertical`, and
`_stacked` suffixes for placement, so kind names ending in those suffixes are
ambiguous as default-direction add IDs. The panel reader refuses such a panel
name; any other kind ending in a suffix, or one whose add IDs another
registration already holds, gets no add actions and an Error in the config
section rather than a startup panic.
