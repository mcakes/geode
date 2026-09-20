# Geode — Command-Line Locality Design

Amends `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md`
§4.3 (the `:` vocabulary and its "`:` commands that change the frame do
so globally" sentence) and
`docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md`
§3.6 (the `:asof` forms) and §3.8 (the `:scope` forms). It also amends
Phase 4b's diagnostics tile vocabulary (`:level`, `:overlay`). Everything
else in those documents stands.

## 1. Why this changes

Geode has two typed command surfaces and they were meant to read
differently: the palette (`ctrl+k`) is where a trader reaches anything,
frame-wide or tile-local, and the per-tile `:` line is the fast path onto
the tile under the cursor. Phase 3 §4.3 blurred that on purpose — "`:`
commands that change the frame do so globally, as a `:scope` from any
tile should" — and the result is a vocabulary a trader cannot predict.
On a blotter, `:group a,b`, `:filter …`, `:unscoped`, `:view` and `:sort`
change that tile; `:scope …`, `:asof 16:00`, `:live` and `:group save N`
change every tile in the window. On the diagnostics tile `:section` is
local and `:level`/`:overlay` are app-wide. The user ruling of
2026-09-20: **a `:` line changes only the tile it was typed on.** The
palette, chords and the scope bar are the doors onto the frame and the
app.

Applying the ruling literally would delete `:asof` and leave a hole: the
frame has three axes and two of them already have tile overrides —
grouping (`:group` pins, `:unpin` follows) and scope (`:filter` narrows,
`:unscoped` ignores) — while as-of has none. So `:asof` stays and
becomes the third override: a tile pinned to a time while the frame
stays live, or pinned to live while the frame is historical. The same
ruling picked, for each command that loses its `:` form, where it goes:
the frame scope expression gets a palette dialog, the log level gets a
palette choice, and `:group save N`, `:asof undo`, `:live` and
`:overlay` are dropped in favour of the doors that already exist.

## 2. The rule

A `:` line is dispatched to the focused occupant's `TileContent::
command` and **may change only that tile's own state**: what it queries
for, how it paints, its cursor, its draft. It may not write the frame
(scope, grouping, as-of, slots), the shell, the config or the log
levels, and it may not change what any other tile shows. An occupant
that needs a frame-wide or app-wide effect exposes a palette action
instead — the palette dispatches `frame::*` to the frame and everything
else to the focused tile, so it is legitimately global or local per
action, which is the reading traders already have of it.

Two things pin the rule:

- **The contract doc.** `TileContent::command`'s doc comment states the
  rule and names the three doors for anything wider. A module author
  reads it before writing a parser.
- **One sweep test per module.** Each module with a `:` vocabulary gains
  a test that runs *every* word its parser accepts (drawn from the
  module's own command list constant, so a new word is swept the day it
  is added) through a tile in a `TestAppContext` and asserts afterwards
  that the frame's `scope`, `grouping` and `as_of` counters are
  unchanged, the frame's slot set is unchanged, `Frame::take_pending_
  persist` is `None`, the `Diagnostics` entity has no pending level or
  overlay toggle, and the fixture's temporary user config directory has
  the same files with the same contents as before the word (there is no
  recording sink in `config_write`; a directory diff is the honest
  check). A word that needs an argument is fed a valid one. The test is the harness's reference for
  every refusal in §5: a refusal mutated back into the old frame write
  must turn this test red.

Nothing in this rule limits what a `:` command may *read*: `:asof` still
reads the clock, `:group slot N` still reads the frame's slots, `:view`
still reads the config.

## 3. The tile as-of override (blotter)

### 3.1 State

`BlotterTile` gains

```rust
enum TileAsOf {
    /// Query at the frame's as-of; requery when it changes.
    Follow,
    /// Query at this instant regardless of the frame.
    Pinned(AsOf),
}
```

beside `Pin` (grouping) and `tile_scope`/`unscoped` (scope). It is
`Follow` on a fresh tile.

### 3.2 Vocabulary

| Line | Effect |
|---|---|
| `:asof <time>` | Pin this tile to `<time>`: `HH:MM` means today in the trader's local clock, RFC 3339 for anything else — the existing `parse_as_of` (4a §3.6), unchanged. |
| `:asof live` | Pin this tile to live. Under a historical frame the tile shows live data. |
| `:asof clear` | Follow the frame again (the `:unpin` of as-of). |
| `:asof` | Error naming the three forms. |

Pinning to the value the frame already has is still a pin: the tile
stops following, and a later frame change leaves it where it is. This
matches `:group slot N` on the active slot.

`:live` and `:asof undo` are removed (§5). `:live` is removed rather
than re-pointed at `:asof live` because a trader who types it out of
habit expects the frame to go live; a flipped meaning is a trap, a
refusal that names `Return to live` in the palette is not.

### 3.3 Query and barrier

`submit` reads the tile's as-of as

```rust
let as_of = match &self.as_of {
    TileAsOf::Follow => frame.as_of().clone(),
    TileAsOf::Pinned(a) => a.clone(),
};
```

and `differs_on_followed` compares the `as_of` counter **only while
`Follow`**, the same shape as `grouping` under `Pin::None`:

```rust
(!self.unscoped && versions.scope != now.scope)
    || (self.pin == Pin::None && versions.grouping != now.grouping)
    || (matches!(self.as_of, TileAsOf::Follow) && versions.as_of != now.as_of)
    || versions.data != now.data
    || versions.config != now.config
```

Because that one predicate feeds both `follows_changed` and `promote`'s
gate (I-1), a pinned tile neither requeries on a frame as-of change nor
has a staged snapshot invalidated by one. The flip barrier needs no new
code: the shell already opens it over every visible tile, and a tile
whose `follows_changed` answers `false` self-arrives from its own
`on_frame_changed` through `Frame::arrived`, exactly as a
pinned-grouping tile does today.

Every `:asof` form that changes the tile's as-of requeries the tile at
once through `requery`, as `:group` does. Setting the same value again
is a no-op with no requery.

### 3.4 Header chip

The blotter header's as-of chip today paints `AS OF HH:MM` in
`Tone::Warning` from the snapshot's `as_of_request`. That stays the
paint for a **following** tile under a historical frame — inherited
danger, the one warning-toned thing beside `unscoped` (the 2026-09-19
emphasis-budget ruling). A **pinned** tile paints instead:

| Pin | Chip | Tone |
|---|---|---|
| `Pinned(At(t))` | `AS OF HH:MM` | `Tone::Neutral` |
| `Pinned(Live)` | `LIVE` | `Tone::Neutral` |

`Neutral` is the tone `pinned` and `filtered` wear: a state the trader
chose. The tooltip reads `Pinned to <full time>` / `Pinned to live`
with the hint `:asof clear follows the frame`. The chip is painted from
the tile's own `TileAsOf`, not from provenance, so it is right from the
keystroke rather than from the next delivery; the provenance-driven
warning chip is suppressed while pinned (a pinned tile's request always
carries its pin, so the two would otherwise both paint).

The `LIVE` chip is the first time a tile's chip and the window stripe
disagree on direction — the stripe says historical, the tile says live.
That is the correct reading and is accepted; it is listed under display
checks.

### 3.5 Session

The tile record gains `as_of`, absent while following:

```toml
[[tiles]]
kind = "blotter"
as_of = "2026-09-20T16:00:00Z"   # Pinned(At)
# as_of = "live"                  # Pinned(Live)
```

Restore parses RFC 3339 into `Pinned(At)`, the literal `live` into
`Pinned(Live)`, and anything else into `Follow` with a `geode::session`
warning naming the tile — the same tolerance `pinned_slot` has for a bad
integer. The value is written in UTC like every other stored instant;
the chip displays the trader's local clock.

### 3.6 Not in this piece

The market-data panel keeps following the frame's as-of. Its request
path has the same two seams (`as_of` read in `submit`, the
`as_of`/`data` comparison in `differs_on_followed`), so the override
transfers directly; it is a follow-up, not a reason to widen this spec.

## 4. Two palette doors

### 4.1 `frame::scope_expression` — "Set scope expression…"

Category `Frame`, no default binding. Opens a one-line modal through
`open_shell_dialog` (so it cancels pending sequences, closes the
palette, returns focus to the scope bar's field on close when opened
from it, and calls `prevent_default` for the mouse door) over the
shell's `dialog_input`, seeded with the frame's current expression
source (`Expr::Display`, un-elided). `enter` commits, `escape` discards.

Commit rules:

- Empty text clears the frame's expression (`scope.expression = None`),
  through `Frame::set_scope` so undo/redo and the flip barrier see it.
- Non-empty text is parsed with `parse_expr`; a parse error is painted
  inline under the field at the caret column, the dialog stays open,
  the frame is untouched. This is the same refusal the Scopes dialog's
  expression field gives (`scopes::parse_text`) and the same message
  shape the old `:scope <expr>` gave.
- There is no dataset validation at this door. `:scope <expr>` validated
  against the *typing tile's* dataset, which was arbitrary for a
  frame-wide value; the compiler already drops a conjunct naming a
  column a dataset has no storage for. A column that exists nowhere
  will show as an empty result, the same as it does when loaded from a
  saved scope.

The scope bar's expression chip becomes a control: a mouse-down on it
opens this dialog. Its tooltip hint, which today reads `:filter <expr>
sets it` (wrong — that is the tile layer), becomes `click to edit ·
palette: Set scope expression…`. The chip goes through `control::paint`
for hover and pressed like the scope bar's other clickable chips.

### 4.2 `log::level` — "Set log level…"

Category `Diagnostics`, no default binding. A new `Target::LogLevel` on
`shell::choicedialog`, in two steps over the same `ChoiceList`:

1. **Target.** Options are the `geode::` suffixes of
   `geode_core::log::TARGETS` (`ingest`, `query`, …), each row showing
   its current level from the `Diagnostics` entity's levels, spelled
   `"{target} · {level}"`.
2. **Level.** `error`, `warn`, `info`, `debug`, `trace`, with the current
   one placed into view (`ChoiceList::place`).

A pick on step 2 calls `Diagnostics::request_level(target, level)` on the
shell-owned entity and notifies — the exact path the tile's `:level`
took, so the pending persist and the runtime reload filter are
unchanged. `escape` on step 2 returns to step 1; on step 1 it closes.
`Target::LogLevel` carries the chosen target between the steps.

`:overlay` needs no replacement: `perf::toggle_overlay` (`mod+shift+p`)
already exists.

## 5. Removals and refusals

Every removed word stays in its module's parser as a **refusal**: a
`Command::Refused(&'static str)` variant whose message names the door.
The shell paints it inline on the line like any other error, and the
word is **not** in the module's completion vocabulary (a refusal is not
a suggestion). The sweep of §2 runs refusals too.

Blotter:

| Line | Message |
|---|---|
| `:scope …` (every form) | `:scope is frame-wide — the scope bar (mod+/), Set scope expression…, or the palette's Scope: entries` |
| `:asof undo` | `frame as-of undo is in the palette (Undo as-of)` |
| `:live` | `:asof live pins this tile; Return to live (palette) sets the frame` |
| `:group save N` | `saving a slot is in the Groupings dialog or the grouping picker (mod+g)` |

Diagnostics:

| Line | Message |
|---|---|
| `:level …` | `log levels are app-wide — Set log level… in the palette` |
| `:overlay` | `Toggle performance overlay (mod+shift+p)` |

`Command::GroupSave`, `ScopeExpr`, `ScopeText`, `ScopeClear`,
`ScopeUndo`, `ScopeRedo`, `ScopeDrop`, `ScopeSave`, `ScopeLoad`,
`AsOfUndo` and `Live` are deleted from the blotter's enum; `Level` and
`Overlay` from the diagnostics enum. `parse_as_of` and the
`Frame` scope API they called are untouched — the palette and the scope
bar still use them.

The market-data panel's vocabulary is already local (`:key`, `:revert`,
`:bump`, `:rebase`, `:upload`, `:menu`, `:set`, `:auto`) and does not
change. `:upload` is an egress request, an outward action rather than
shared state, and is inside the rule.

## 6. Documentation

- Phase 3 §4.3 and Phase 4a §3.6/§3.8 each get a one-line supersession
  note pointing here; their tables are not rewritten.
- `docs/phase-history.md` gets this piece's paragraph; `CLAUDE.md` gets
  a status row and one load-bearing bullet under "Shell: frame, tiles,
  diagnostics": *a `:` line changes only its own tile; every module's
  vocabulary is swept by a test that asserts the frame counters and the
  config write sink are untouched; `TileAsOf` is the third override
  beside `Pin` and `tile_scope`.*
- The `TileContent::command` doc states the rule.
- The timeseries viewer and line pricer specs are already within the
  rule (every `:` verb in both is sheet- or tile-local) and need no
  amendment; their implementers inherit the sweep test as a requirement.

## 7. Testing and harness

Tests (TDD, in the order the work lands):

- **Sweep** (§2): `every_colon_command_leaves_the_frame_and_config_alone`
  in the blotter, diagnostics and market-data crates.
- **Blotter tile** (`TestAppContext`): `:asof 16:00` submits a request
  carrying `At` with the frame's counter unchanged and the neutral chip
  painted; `:asof live` under a historical frame submits `Live` and
  paints `LIVE`; `:asof clear` follows again and requeries at the
  frame's value; a frame as-of change does not requery a pinned tile and
  the barrier still releases (the pinned tile self-arrives); pinning the
  frame's own value still stops following; session round-trip for
  `At`, `Live` and a malformed value; each refusal's message and its
  absence from completions.
- **Expression dialog**: opens from the palette and from a click on the
  chip, **and typing after the click lands in the field** (the
  mouse-opened-dialog rule); parse error paints and leaves the frame
  alone; empty commit clears; commit goes through `set_scope` (undo
  restores it); focus returns to the scope bar field when opened from
  it.
- **Log level**: the two-step pick calls `request_level` on the
  entity; `escape` steps back; the current level is placed into view.
- **Chip tones**: the neutral `AS OF`/`LIVE` pair rides the existing
  `every_chip_tone_is_readable_on_every_bundled_theme` sweep (no new
  tone).

Mutation-harness entries (each with its 6th-argument test):

| Entry | Mutation | Caught by |
|---|---|---|
| `asof-pin:request` | `Pinned(a)` arm reads `frame.as_of()` | pinned request carries `At` |
| `asof-pin:follows` | drop the `matches!(Follow)` guard | frame change does not requery a pinned tile |
| `asof-pin:session` | write `as_of` while following | session round-trip |
| `asof-pin:chip` | pinned chip paints `Warning` | chip tone test |
| `locality:blotter-scope` | `Refused` → old `set_scope` call | blotter sweep |
| `locality:blotter-live` | `Refused` → old `set_as_of(Live)` | blotter sweep |
| `locality:diag-level` | `Refused` → old `request_level` | diagnostics sweep |
| `locality:completions` | refusal word back in `COMMANDS` | absence-from-completions test |
| `expr-dialog:empty-clears` | empty commit keeps the expression | empty-commit test |

Run `--anchors-only` before merge as always.

## 8. Display checks (pending a real window)

- The neutral `AS OF 16:00` chip beside a live toolbar, and the neutral
  `LIVE` chip under the historical stripe.
- The expression chip's hover/pressed states and the dialog's inline
  parse error at the caret.
- The two-step log-level picker's `"{target} · {level}"` rows.

## 9. Out of scope

- The market-data panel's as-of override (§3.6).
- A tile-level as-of *undo*. The override is one value; `:asof clear` is
  its reverse.
- Any change to what `ctrl+k` dispatches; the palette's global-or-local
  reading is already right.

## 10. As built (2026-09-20)

- §2's sweep checks `Frame::take_pending_persist`,
  `Frame::take_pending_scope_persist` and the `Diagnostics` entity's
  pending level/overlay rather than diffing a user directory: those
  three are the only channels a module has onto a config write (the
  shell performs the write), so they are the honest check at module
  level. The blotter sweep checks all three it can reach; a module with
  a narrower vocabulary checks only the ones its own words could touch.
- §7's `asof-pin:chip` entry became `asof-pin: the pinned chip paints`
  and `asof-pin: the frame chip hides while pinned`: a `TestAppContext`
  can see whether a chip paints, not its colour; the tone rides the
  existing `every_chip_tone_is_readable_on_every_bundled_theme` sweep.
- §5's refusal for `:asof undo` names the palette title as it is,
  `Swap to the previous as of`; `:group save N` names `Edit groupings…`.
- The status bar's as-of segment read `:live to return`; it now reads
  `Return to live in the palette`.
- The expression chip's tooltip had said `:filter <expr> sets it` (the
  tile layer); it names `frame::scope_expression` now.
- The pinned chip's cached `AS OF HH:MM` text is rebuilt lazily in
  `render` when the local date changes (`asof_chip_date`,
  `refresh_asof_chip`), so the elided form never outlives its day
  (review finding on Task 3).
- The harness entry for the expression commit is titled `expr-dialog:
  enter commits the parsed expression to the frame` (a
  `set_scope_in_session` mutation considered during Task 6 would have
  survived: that method pushes undo identically; the entry as built
  defends the commit, and the undo half is the test's own last
  assertion).
- The parse-error text in the expression dialog AND the as-of dialog now
  paints through `chip_paint(theme, Tone::DangerText)` (a raw
  `theme.danger` bypassed the 3:1 floor); four other raw `theme.danger`
  text sites (`status.rs`'s reload-message and config-write-error
  segments, `commandline_view.rs`'s error strip, `objectdialog/
  render.rs`'s diagnostic `!` glyph) are pre-existing and left for a
  follow-up — `toolbar.rs`'s contradiction chip is NOT one of them: its
  fill falls back to `theme.danger` but its text already comes from
  `chip_paint`.
- §7's harness table reconciles against what was built as follows:
  `locality:blotter-scope` and `locality:blotter-live` merged into the
  one entry `locality: a refused word never writes the frame`; every
  other row was respelled to match its as-built name; and three entries
  are new beyond the table — `expr-dialog: enter commits the parsed
  expression to the frame` and the two `loglevel:` entries (the step-1
  and step-2 mutations of `shell::choicedialog`'s `Target::LogLevel`).
- Task 5 kept one renamed completion test
  (`completions_offer_sections_after_the_section_word`) that pins
  `["section"]` → the five section names.
- The harness lost the stale `commands: scope drop needs a dimension`
  entry (its anchored line was deleted with `:scope`).
- Display checks pending: §8's list.
