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
  gpui-component `Root`'s window-wide focus cycling. `Root`'s *binding*
  applies whether or not an input is focused — but **the reclaim did
  not**, and the claim that it "stays exactly as it is" was wrong. The
  reclaim was scoped to `"GeodeModal"`, a key context that rides the
  modal *panel*, so it is on the dispatch stack only while something
  inside the panel holds focus. Normal mode focuses the shell root, so
  `"GeodeModal"` was absent, `Root`'s `Tab` won, and `window.focus_next`
  walked focus off the shell root onto whatever sits behind the modal.
  Making normal mode the resting state (§2) is precisely what exposed
  this: while normal mode was momentary — rebind capture only — nothing
  pressed `tab` there. The reclaim therefore had to *grow*, not stay
  still: a second `"GeodeModalOpen"` scope, the shell root's own key
  context while `modal.is_some()`, carrying the same
  `tab`/`shift+tab` → `NoAction`. See §15.
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

## 15. As built

Tasks 1–4 implemented this spec. Where the built code differs from the
design above:

- **§2's mode indicator lives in the dialog, not the shared chrome.**
  `render_modal` paints its title row from `ShellModal`'s fixed
  `title: SharedString`, shared verbatim by settings, the picker and the
  as-of selector; threading a per-dialog, per-frame mode through that
  struct and every call site was out of proportion to what one surface
  needed. The pill is instead `dialog::mode_pill`, rendered as the
  keybinding dialog's own first content child, right-aligned above the
  filter row — visually the same corner the design intended. The helper
  still lives in `dialog.rs` so Phase 4c's modal surfaces share the one
  badge; the shared chrome (`render_modal` itself) is unchanged.
- **§9's `NORMAL`/`FILTER` labels shipped lowercase** (`normal`/
  `filter`). Deliberate: this crate's key rendering
  (`palette::render_keystroke`'s `ctrl+k`) is already lowercase, and a
  shouted badge next to those chips would read as a different design
  system.
- **§6's `dialog` key contexts were not built.** Modals intercept keys
  before the `Matcher` runs (`handle_key_down`'s modal branch,
  `ModalKeyHandler`'s first refusal), so routing `d`/`r`/`enter`/etc.
  through registered, user-rebindable actions is a real change to how
  modals see input, not a small one. §14's open question 1 already
  recommended deferring this, and Task 3 took that recommendation:
  `dialogmode::normal_command` is matched directly in
  `keybindings_view::handle_key`, with no `dialog` context and nothing
  registered in the action registry.
- **§8's `r` cannot lift a `"none"` shadow in one press.** A row a
  previous `d` silenced holds a user-layer `"none"` entry, which
  `derive_rows` then resolves as *unbound* — nothing on the row names
  the key that would need removing, so `r` cannot find it. Recovery is
  `enter` then retyping the same keystroke, which `apply_rebind`'s
  existing overwrite-in-place path (`set_key`) handles today; `r`'s
  refusal message now names this recovery instead of denying that a
  user override exists. Closing the gap for real needs `derive_rows` to
  record *which* user-layer entry is suppressing a row — a change to the
  row vocabulary, not a call-site fix — and is parked as a follow-up,
  not built here.
- **§8's `d` on a user-layer row reverts to the lower layer rather than
  unbinding the action outright.** This falls out of `apply_unbind`'s
  existing shadow-vs-remove split (`is_user_layer`, from Task 2): a row
  whose effective binding *is* the user's own is removed from the user
  layer, which un-shadows whatever a lower layer already bound to that
  key (builtin or desk) rather than leaving the action with no key at
  all. A trader who wants the action fully unbound therefore presses `d`
  twice — once to drop back to the lower layer's binding (if any), once
  more to shadow that. This is the same shape `apply_rebind`'s
  displacement step already had; §8 did not call out the two-press case
  and this note is the reconciliation.
- **Task 2 fixed two pre-existing `keymap_edit` defects surfaced by
  wiring `apply_unbind` to a live keystroke**, beyond anything §8 asked
  for: a keymap whose `bindings` was a plain TOML array of inline tables
  (legal per `build.rs`, but not `toml_edit::ArrayOfTables`) was silently
  replaced with an empty array and the whole keymap destroyed on the
  next write; and an inline `keys = { ... }` table passed
  `Item::is_table_like` but panicked on the very next line, which
  assumed `as_table_mut` would succeed. Both are now guarded (a
  wrong-shaped `bindings` is rejected with the file left untouched; both
  table shapes are handled via `as_table_like_mut`) and covered by
  tests. Neither defect is dialog-interaction-model behaviour — they are
  pre-existing `keymap_edit` bugs this task's new caller happened to
  reach first.

- **§7's `tab` reclaim had to grow, and §7 has been corrected above.**
  The `tab`/`shift+tab` → `NoAction` reclaim was scoped to
  `"GeodeModal"` — the modal panel's own key context, on the dispatch
  stack only while focus is inside the panel. Normal mode parks focus on
  the shell root, so in the state this branch made the default, the
  reclaim was not enabled at all: `Root`'s window-wide `Tab` binding won
  and `window.focus_next` moved focus off the shell root, where a caret
  can land on a field behind the modal and `Input`-context bindings go
  live. A raw-key-listener check cannot fix it (the action has already
  fired by the time raw listeners run), so the fix is a second keymap
  scope: `ShellView::render` puts a `"GeodeModalOpen"` key context on the
  shell root while `modal.is_some()`, and
  `dialog::init_reclaimed_keybindings` binds `tab`/`shift+tab` to
  `NoAction` there too. Kept as a separate context rather than reusing
  `"GeodeModal"` on the root, so the existing `"GeodeModal > Input"`
  `ctrl+a` reclaim keeps meaning "an `Input` inside the modal panel".
  Covered by `tab_in_normal_mode_leaves_focus_on_the_shell_root`, which
  asserts the focus state itself — "the modal is still open" is exactly
  the assertion that could not see this — and by a mutation-harness
  entry, since a green suite plainly could not see it before.
- **`d`'s recovery promise is honest per row, and contexted recovery is
  NOT made to work.** `RECOVERY` ("press enter and type that key again
  to restore it") holds only for a binding with no context. Roughly 60
  of the ~80 builtin bindings carry one, and for those `d` writes
  `"none"` into the *contexted* `[[bindings]]` entry while the recovery
  rebind — running on a row that is unbound by then, so passing
  `context: None` — writes the *no-context* entry: a different table,
  not an overwrite. The shadow survives, so whether the key works again
  is decided by array order (the matcher is last-wins), and even when it
  appears to work the binding is escalated from contexted to global.
  Making it work needs `derive_rows` to carry the row's pre-shadow
  context — the same row-vocabulary change as the parked `r`-cannot-
  lift-a-shadow follow-up above — and landing that on this branch's last
  commit was ruled out. So the *honesty* is fixed instead: `recovery()`
  picks per row between `RECOVERY` and `RECOVERY_CONTEXTED` ("undo that
  in keymap.toml — retyping the key would rebind it globally instead of
  in that context"), which is cheap because `d` already holds
  `bound.context_source`. `r`'s unbound-row message, which has no
  `context_source` left to read, names both doors rather than picking
  one.
- **`d` and `r` acknowledge in the present tense.** `d` said "silenced"
  before its background `apply_unbind` reported, and `r`'s success path
  said nothing at all — so an `apply_unbind` returning `removed: false`
  (a stale row, or a key the user file spells differently from
  `render_binding`) left the footer asserting something that did not
  happen, with only stderr disagreeing. Both now acknowledge what is
  actually known — that the write was dispatched — as "silencing …" and
  "removing your … override". Confirming the outcome for real would mean
  reporting the background result back into `KeybindingsState`, which is
  a structural change to `spawn_unbind`'s deliberately state-free
  contract; not built here.

Everything else — the pure `dialogmode` core, the escape ladder, the
modal vocabulary, which surfaces are modal (§3), and §7's reclaim-count
observation (as corrected) — was built as specified.
