# Geode — Dialog Interaction Model Design

Amends `docs/superpowers/specs/2026-09-01-dialog-filter-input-design.md`.
That document's platform findings (§2) and its ranking, focus and
`ctrl+f` mechanics remain correct and in force. What this amends is its
central claim — that *every* list surface should be filter-first — and
the keybinding dialog's share of it.

Governs the key vocabulary that
`docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md`
builds on, and must land before 4c's dialogs are written.

## 1. Why this changes

The filter-first rewrite aligned the two list dialogs with the command
palette: one vocabulary, three surfaces. It succeeded on its own terms
and the palette-shaped surfaces are better for it.

Its cost only became visible when a surface needed **verbs**. With an
`Input` focused, bare letters can never be actions, so a dialog's whole
vocabulary is what a focused input leaves free — and
`keybindings_view`'s own module doc says exactly that. The consequences
are on the record in the code:

- The keybinding dialog's entire verb set is `enter` (rebind) and
  `escape`. **There is no way to unbind a key or reset one to its
  default** — you edit `keymap.toml` by hand. This is not a decision
  anyone made; it is what was left.
- Both dialogs claim keys purely to render them inert:
  `keybindings_view` claims `tab`/`shift+tab` only to drop them, and
  `settings_view` documents `enter` as "deliberately inert and
  reserved".
- Phase 4c needs save, delete, revert, reorder and new on one surface.
  There is no room for them, and inventing chords means a `NoAction`
  reclaim per chord in every dialog in the app — for keys a trader
  would not guess.

Two prototypes settled the shape (both benches at the artifact recorded
in the Phase 4c plan). The second one changed the answer: making the
**dimension picker** modal costs a keystroke on the app's hottest
keyboard path and frees nothing, because ticking happens *while* you are
typing a filter, so the tick key must be non-printable either way.

That is the rule this design turns on, and it is narrower than "modal
everywhere".

## 2. The model

Two modes, one vocabulary.

**Filter mode.** `ShellView::dialog_input` is focused. Printable keys
type and re-rank rows. Navigation is exactly today's
`listfilter::nav_command` set (`up`/`down`/`ctrl+p`/`ctrl+n` ∓1,
`ctrl+d`/`ctrl+u` ±5, `ctrl+f`/`ctrl+b`/`pageup`/`pagedown` ±10), plus
`enter` and `escape`. Unchanged from what ships today.

**Normal mode.** No input is focused. Letters are verbs. `j`/`k` join
the arrows for ∓1, count prefixes work, and `/` enters filter mode.

**Not every surface has both.** A surface is either *filter-only* — it
has no normal mode at all, opens in its input, and behaves exactly as it
does today — or *modal*, opening in normal mode with `/` to filter.
Nothing gains a mode it has no use for, so nothing gets slower.

## 3. Which surfaces are modal

A surface is modal **iff it has verbs a user would reach for while not
typing.** Declared per surface, never inferred at runtime.

| Surface | Kind | Normal-mode verbs |
|---|---|---|
| Command palette (`ctrl+k`) | filter-only | — |
| Settings (`ctrl+,`) | filter-only | — |
| Dimension picker, both stages | filter-only | — |
| As-of selector | filter-only | — |
| **Keybindings** | **modal** | `enter` rebind · `d` unbind · `r` reset · `/` filter |
| **Config dialogs (4c)** | **modal** | §4 below |

The palette is not an exception to be excused. It is the app's
*type-now* surface, entered deliberately with `ctrl+k` exactly as `/`
and `:` enter text modes in a tile; under the model above it is
filter-only because it has one verb (`enter`) and that verb is reached
while typing.

The picker is filter-only for the reason the prototype demonstrated: its
only verb is tick, tick happens mid-filter, and so a normal mode would
be a keystroke of pure cost on the sequence a trader repeats most.

Settings is filter-only because stepping a value is `tab`, which a
focused input leaves free, and four rows do not need `j`/`k`. If
settings ever grows a reset-to-default, it becomes modal by this same
rule; that is a later decision, not one to pre-empt.

## 4. The modal vocabulary

Shared by the keybinding dialog and every 4c dialog, so learning one
teaches the other:

| Key | Meaning |
|---|---|
| `j` / `k` / `↑` / `↓` | move the selection ∓1 (counts apply: `12j`) |
| `g` / `shift+g` | first / last row |
| `/` | enter filter mode |
| `enter` | commit the selected row (open, rebind, or run an action row) |
| `space` | toggle the selected row's inclusion, where it has one |
| `shift+j` / `shift+k` | move the selected item down / up in an ordered list |
| `i` | edit the selected row's text value; `escape` leaves the field |
| `escape` | the ladder of §5 |
| letters | per-surface verbs — 4c's `s`/`d`/`r`/`n`, keybindings' `d`/`r` |

`shift+j`/`shift+k` replaces Phase 4c §3.3's pick-up sub-mode
(`enter` to grab, arrows to move, `enter` to drop), which existed only
because no key was free. The sub-mode is deleted, not reimplemented.

`i` replaces 4c §3.3's retargeting of the always-focused filter input at
a field, which was that spec's open question 2 and its least certain
interaction. Editing a text value is now the same gesture as everywhere
else.

## 5. The escape ladder

On a modal surface, `escape` takes the first step that applies:

1. in filter mode → return to normal mode, **keeping the query applied**
   (the rows stay narrowed, as leaving a vim search leaves you on the
   match);
2. in normal mode with a non-empty query → clear the query;
3. in normal mode on a nested stage → return to the previous stage
   (4c's `Edit` → `Browse`), running that stage's own confirm first if
   there is unsaved work;
4. otherwise → close the modal.

Each step changes something visible, so the ladder is never a keystroke
that appears to do nothing. Filter-only surfaces keep today's behaviour:
`escape` closes.

## 6. Keymap engine, contexts and counts

The engine already models this. `KeyContext` carries mode pairs and
count support today — `matcher.rs` exercises
`KeyContext::new("blotter").pair("mode", "normal").counts()` — because
the blotter is already a modal surface with `/` and `:`. A dialog
therefore reports:

```rust
KeyContext::new("dialog").pair("mode", "normal").counts()
KeyContext::new("dialog").pair("mode", "filter")
```

with the surface's own name as a second pair
(`.pair("surface", "views")`) so a binding can be scoped to one dialog.
No engine work; this is the vocabulary the engine was built for.

## 7. What does *not* get cheaper

A correction to an earlier claim made in discussion:
`dialog::init_reclaimed_keybindings` does **not** largely dissolve.

- Its `tab`/`shift+tab` → `NoAction` binding exists to suppress
  gpui-component `Root`'s window-wide focus cycling, which applies
  whether or not an input is focused. It stays exactly as it is.
- Its `ctrl+a`/`ctrl+x`/`ctrl+f` reclaims exist because gpui-component's
  `Input` binds them. Filter mode still focuses that `Input`, so they
  stay too.

What changes is only that **new verbs no longer need new reclaims** —
which is the point, but it is a bound on future cost, not a reduction in
present cost. Nothing in this design deletes existing reclaim
scaffolding.

## 8. The keybinding dialog gains two verbs

`d` unbinds the selected binding; `r` resets it to the layer beneath the
user's.

Both write through machinery that already exists. `keymap_edit` already
writes `keymap::UNBOUND_ACTION` (`"none"`) into the user entry to shadow
a key displaced by a rebind, which is precisely what an unbind is when
the binding comes from builtin or desk; when the binding is the user's
own, `r` removes the key from the user entry rather than shadowing it,
the same two-branch shape `apply_rebind`'s step 2 already implements.
The dialog gains the two callers; the writer gains a way to be asked.

This is the concrete gap §1 names, and closing it is the evidence that
the model earns its churn.

## 9. Discoverability

A surface that ignores your first keystroke until you press `/` is less
self-evident than one that starts typing. Three mitigations, all of
which exist or are cheap:

- **A mode indicator** in the panel's title row (`NORMAL` / `FILTER`),
  the way the prototype showed it.
- **A footer hint row** listing the current mode's vocabulary, in the
  mould `shell::picker` now uses.
- **Verbs are also buttons.** 4c's action bar shows each verb with its
  letter, so the key is discoverable and the verb is mouse-reachable —
  a chord alone has no clickable target, which is why chords were
  rejected as the *only* affordance.

## 10. What is untouched

- The palette, settings, the picker and the as-of selector: no change of
  any kind.
- `[ui] find_style` and `crate::vimfind`: still the blotter's setting,
  still unread by dialogs. This design does not restore the vim *find
  session* model — `/` here enters the same fuzzy filter that ships
  today, not a jump-to-match session.
- `crate::vimnav::apply`: still the clamped index arithmetic every list
  surface shares. `j`/`k` feed it exactly as arrows do.
- Ranking, highlighting, `ShellView::dialog_input` ownership and the
  `ctrl+f` shim: all as specified on 2026-09-01.

## 11. Risks

1. **A third state for the keybinding dialog.** It was vim-modal, then
   filter-first, now modal-with-`/`. Churn is real: module docs, that
   dialog's spec share, and every mutation entry anchored on
   filter-first behaviour. Mitigated by the fact that the third state is
   the first one that can express unbind.
2. **Two kinds of dialog in one app.** A user who learns the config
   dialogs will press `j` in settings and type a `j` into its filter.
   The mode indicator makes the difference visible before the keystroke
   rather than after.
3. **`space` as toggle.** It is free in normal mode and universal for
   checkbox lists, but it is also the key most likely to be pressed by
   someone who thinks they are still typing. The mode indicator is the
   defence; if it proves wrong, `x` is the fallback.

## 12. Tests and the harness

**Pure, no window:** the mode state machine (open mode per surface, `/`
in, `escape` ladder out); the modal vocabulary's dispatch table; count
prefixes reaching `vimnav::apply`; keybindings' unbind and reset
lowering to the right `keymap_edit` branch (shadow vs remove).

**`TestAppContext`, real key dispatch:** a letter in normal mode running
its verb rather than typing; the same letter in filter mode typing
rather than running; `escape` walking the full ladder one visible step
at a time; a filter-only surface still consuming its first keystroke as
text.

**Mutation entries**, with attention to the ones a green suite would
miss:

- a modal surface opening in filter mode (which silently restores the
  old behaviour and passes every filter test);
- `escape` skipping a rung of the ladder;
- the query cleared on leaving filter mode rather than kept;
- unbind lowering to remove where it should shadow, which would delete a
  user's *other* binding rather than silence a desk one;
- `space` toggling in filter mode.

## 13. Sequencing

1. **The mode machinery**: `Mode` on the dialog state, the `/` and
   `escape` ladder, the mode indicator, the footer hint, the `dialog`
   key contexts. No surface changes behaviour yet.
2. **The keybinding dialog migrates**, and gains `d` and `r`. This is
   the proving ground: one existing surface, a real feature gained, and
   the vocabulary exercised before anything is built on it.
3. **Phase 4c** builds its scaffold on the settled vocabulary.

Steps 1 and 2 are one branch. 4c does not start until it merges.

## 14. Open questions

1. **Whether modal verbs become registered actions.** Registering
   `dialog::save`, `dialog::unbind` and the rest in the `dialog` context
   would make them user-rebindable, and would let the keybinding dialog
   show its own keys — pleasingly self-consistent. But modals currently
   intercept keys *before* dispatch (`handle_key_down`'s modal branch,
   `ModalKeyHandler`'s first refusal), so routing them through the
   `Matcher` is a real change to how modals see input. Recommended for a
   follow-up once the vocabulary is settled, with step 1 handling the
   keys directly.
2. **`g`/`shift+g` versus count prefixes.** Both are specified; whether
   traders use either in a dialog of six rows is unknown. Cheap to keep,
   cheap to drop after a week of use.
