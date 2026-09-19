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
| Settings (`ctrl+,`) | filter-only → **modal** since §18 (2026-09-12) | `space`/`shift+space` step · `/` filter |
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

**Amended (as built, 2026-09-13).** The step keys are five, not two:
`space`, `l` and `tab` forward, `shift+space`, `h` and `shift+tab` back
(user ruling: "I'd like cycling to also be done with tab and h/l — I
keep reaching for them"). All six spellings are claimed in
`dialogmode::normal_command`, the one table every modal surface reads,
so the settings dialog gained `h`/`l` with no code of its own; `h` and
`l` are no longer reachable as `NormalCommand::Verb`, which was safe
because no surface had claimed either letter. In the object dialog's
FILTER mode `tab`/`shift+tab` step too — the settings dialog's own "tab
steps in both modes" rule — while `h` and `l` stay letters on their way
to the focused `Input`. The one exception is an open text field, whose
`tab` still completes a chain segment (4c §18.8) or stays inert.

The same ruling made the footer **row-sensitive**, which §9's "a footer
hint row listing the current mode's vocabulary" now has to mean per row
as well as per mode: a footer that names `space` while the cursor is on
a read-only `Text`, or `i` while it is on a `Choice`, is the inert-key
lie in its most ordinary form. The object dialog's edit and column
stages compute both groups from `Draft::selected_vocabulary` — the
change group on a `Choice`/`Bool`/`Number` row, `i` on a `Number` or an
editable `Text`, both on a `Number`, neither on a read-only one; a list
row gets the forward key alone with the word (`toggle`, `add`) that says
which way it travels. The **reorder group goes the same way**
(`shift+j`/`shift+k`, and `x` where the domain offers it): those move a
list *item* and answer "that is as far as this row goes" anywhere else,
so they are named only on an item row. The filter-mode footer names the
change group shrunk to `tab`/`shift+tab`, since those are the only two
spellings that step with the `Input` focused — the same rule read the
other way round, about keys that type rather than keys that are dead —
and for the same reason a step that finds nothing to change names
`tab`/`shift+tab` there and `space`/`shift+space` in normal mode, never
the aliases, which no footer advertises. Groupings' `i` is the one thing
that is not row-derived: there it opens the slot's whole chain rather
than a row's value, so it is live everywhere and stated unconditionally.

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
  any kind. (Settings has since gone modal — §18, 2026-09-12; the other
  three stand.)
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
   rather than after. (Resolved the other way on 2026-09-12: §18 made
   settings modal, so `j` moves there too.)
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

## 16. Amendment — one owner for mode, focus and text (2026-09-11)

Approved 2026-09-11 after the Phase 4c Part 2 refinement branch. Four of
that branch's review findings, and at least two earlier ones (the
keybinding dialog's first modal cut, `an_object_opened_from_filter_mode_
still_escapes_back_a_stage`), were the same defect: a modal surface has
three things that must agree at every transition — the pure `mode`, which
gpui focus handle holds the keyboard, and the shared `Input`'s text against
the mirrored `query` — and every transition site assembled its own subset
by hand. The object dialog had 20 such sites across eight functions, the
keybinding dialog ten more. The mirror is one-directional by construction:
typing updates `query` through the `InputEvent::Change` subscription, but
`InputState::set_value` emits no `Change`, so every clear had to be written
twice, and the naming row's first cut forgot one of them.

### 16.1 The rule

**The pure state is the truth; gpui is reconciled to it after every
mutation.** `mode` and `query` stay where they are — `KeybindingsState`,
`ObjectDialogState`, `Draft` — so the pure cores remain testable without a
window. One function, `dialog::sync_dialog_text(shell, window, cx)`, reads
the open dialog's *effective* mode and query and makes gpui match:

- focus: the shared `Input` in `DialogMode::Filter`; the shell root
  otherwise — and the shell root whenever the keybinding dialog is
  *listening* for a capture, whatever the mode says;
- text: `set_value(query)` when the `Input`'s value differs from the
  effective query; nothing when they already agree, so a keystroke the
  subscription has just mirrored costs a string compare and no write.

**As-built correction (§16.6): this runs at four seam classes, not
three** — the tail of the modal branch in `ShellView::handle_key_down`
(after the dialog's `on_key` returns, claimed or not), the tail of every
mouse handler that ends a dialog transition — a click never reaches the
key path — which is the three row-click handlers and the object dialog's
two confirm-button closures (`run_confirmed` is reachable by mouse), with
`press_verb` audited as the one exception because it can only arm a
`Confirm`; and `open_shell_dialog_with_key`, which calls the sync
unconditionally (`focus_filter` is unchanged and still serves the
mode-less dialogs).
Transition sites become pure mutations — `state.mode = DialogMode::Filter`,
`draft.query.clear()` — and there is no second half to forget.

### 16.2 The effective query

The object dialog carries two queries, the browse/naming one on
`ObjectDialogState` and the edit one on `Draft`, mirrored one-way per
stage (4c §18.6). That ruling becomes a getter,
`ObjectDialogState::effective_query(&self) -> &str`, which the sync and the
`Change` subscription both use; the subscription writes through
`set_query`, which already routes to the stage's own slot — the getter is
its read half. A stale query in the other stage's slot is then
unreachable rather than merely avoided.

### 16.3 What decides focus is pure

`dialogmode::focus_target(mode, listening) -> FocusTarget { Input, Shell }` is
the whole decision, in the pure core beside `escape_step`, unit-tested for
every combination. `sync_dialog_text` only applies it.

### 16.4 Untouched

The filter-only dialogs — picker, as-of, and settings until §18 made it
modal on 2026-09-12 (it now has a third arm in the sync) — never blur and
have no mode; they keep focusing the `Input` on open and are not routed
through the sync. `init_reclaimed_keybindings` is unchanged. The `Change` subscription
keeps its per-dialog routing.

### 16.5 Tests and harness

Pure: `focus_target` over every `(mode, listening)` pair; `effective_query`
per stage. Window: the four one-switch regressions already on the branch
stay as they are (`an_object_opened_from_filter_mode_still_escapes_back_a_
stage`, `n_opens_an_empty_name_field_even_after_a_browse_filter`,
`slash_filters_the_edit_stage_and_escape_walks_the_full_ladder`,
`clicking_an_edit_row_while_filtering_keeps_the_filter_focused`) and prove
the sync reproduces every behaviour the hand-written sites had; one new
window test per removed site class is not needed — the point is that the
class is gone. Mutation entries: one per reconcile rule (focus in filter,
shell in normal, shell while listening, text written only on difference).

### 16.6 As built

Tasks 1–3 implemented this amendment (commits 5237c17, 9fedd06, 296a5b8;
8b62ae2 between them is unrelated — a repair of two prompt tests main had
left red). §16.1's "exactly three places" is corrected in place above to
name four seam *classes*, not three — the true count once the object
dialog's confirm buttons are counted honestly:

1. the tail of the modal branch in `ShellView::handle_key_down`
   (`shell/input.rs`), after the dialog's `on_key` returns, claimed or
   not;
2. `open_shell_dialog_with_key` (`shell/dialog.rs`), unconditionally, as
   the dialog's initial reconcile;
3. every row-click handler — a click never reaches the key path, so the
   handler is the transition's only tail: the keybinding dialog's one,
   the object dialog's two (`on_row_clicked`, `on_edit_row_clicked`);
   `press_verb` is the audited exception, because it can only arm a
   `Confirm` and moves neither mode nor query;
4. the object dialog's two confirm-button `on_click` closures (the "yes"
   and the Cancel button built in `confirm_row`) — `run_confirmed` is
   reachable by mouse from either button, and neither passes through
   `handle_key_down` at all, so this class is the one the "three seams"
   count originally missed.

`press_verb` deliberately carries no sync of its own: arming a confirm
(`d`/`r`/`o`) mutates `ObjectDialogState::confirm`, not `mode` or
`query`, so there is nothing for a reconcile to do there — the seam list
above is "handlers that can move mode or query", not "every mouse
handler that mutates a dialog's own state" (§16.1's own prose is
corrected to say so).

`ObjectDialogState::effective_query` (Task 1, §16.2) is the *read* half
of the one-way mirror 4c §18.6 already had; `set_query` remains the
*write* half, called from the `Change` subscription exactly as before.
Neither Task 2 nor Task 3 changed which stage's query the subscription
writes into — only what reads it back.

**Re-anchoring.** `keybindings: leaving filter mode clears the query`
was **not** re-anchored, contrary to this plan's own premise: it anchors
on `state.mode = DialogMode::Normal;`, the surviving pure line, not on
the `shell.focus_handle.focus(..)` call Task 2 deleted beneath it, so
the deletion left the anchor unique and live. `objectdialog: n empties a
leftover browse filter before naming` was re-anchored onto
`begin_naming`'s own `self.query.clear();` (carrying the two preceding
lines, since that clear alone recurs in four other transitions in the
file) once the render-side `set_value`/focus pair it used to pair with
was gone. `objectdialog: an edit-stage click takes the keyboard off the
filter` was re-anchored onto `on_edit_row_clicked`'s
`dialog::sync_dialog_text` call, mutated back to the old unconditional
`shell.focus_handle.focus(..)` blur it replaced — the same defect, one
seam later. One new entry, `objectdialog: a mouse-confirmed delete
leaves its filter text in the field`, covers seam class 4 above (anchored
on the "yes" closure's sync call plus its preceding comment line, since
the identical call recurs four times in the file and the Cancel
closure's is indented identically). Two of seam class 3's three
row-click syncs — the keybinding dialog's and the object dialog's browse
click — have no entries: no test can observe either today (the key-path
sync already covers every transition those clicks can drive), and an
entry with no discriminating test behind it is one of the four ways the
harness header says an entry can lie.

**One behaviour change, ruled a fix (2026-09-11).** Confirming a delete
or revert with the mouse while filtering used to force focus to the
shell root under a pill still reading `filter`, with the just-cleared
query's stale text left sitting in the field — a one-switch disagreement
between mode and focus with nothing to clear it until the trader pressed
a key. It now leaves the dialog in filter mode, with an empty, focused
field over the unfiltered browse list — the sync's ordinary behaviour
for a `Filter`-mode transition, applied to a mouse-driven one for the
first time. No test asserted the old behaviour; the ruling treats it as
the class of defect this amendment exists to remove, not as a
regression to preserve.

**Incidental.** Eight functions in `objectdialog/render.rs`
(`handle_browse_key`, `handle_naming_key`, `handle_edit_key`,
`create_from_name`, `open_selected`, `enter_edit_stage`, `leave_edit`,
`run_confirmed`) lost a `Window` parameter that only their deleted
focus/`set_value` calls had needed — a future transition site added to
any of them has no handle in scope with which to hand-write the old
pairing, which is the point.

**Deferred minors**, recorded rather than fixed: `KeybindingsState::
set_query` clears `listening` as a side effect of the `Change`
subscription, which is not a sync seam and is unreachable in practice —
a capture in progress blurs the `Input`, so nothing routes a keystroke
through `set_query` while `listening` is `Some`. And the sync's `value()`
comparison allocates the whole field's text on every keystroke at the
pinned gpui-component rev (`SharedString::new(self.text.to_string())`);
a cheaper read may exist there, unexplored.

**`press_verb` stopped being this section's audited exception on
2026-09-14** — see §20.3/§20.8: its `i` arm now opens a focused field, so
it syncs like every other transition.

## 17. Amendment — mouse parity (2026-09-12)

Approved 2026-09-12 from a user request the same day: "Dialogs need to
get mouse friendlier. In general, when in normal mode, clicking on the
filter text field should enter filter mode. On Key Bindings, clicking a
row should immediately enter capture mode, not requiring a second click.
On Edit Views, clicking on a row should enter the edit specific view
screen, and on that screen, we should be able to use the mouse to
reorder rows and show/hide columns. Similar should apply to Edit Scopes
and Edit Groupings."

The modal model (§2) made letters verbs and put the keyboard first; it
did not say what the mouse does on a modal surface, and what shipped was
the filter-first dialogs' mouse behaviour carried over unchanged: a
click selects, and acting is a second, deliberate keystroke or button.
That rule served a surface whose only mouse-reachable act was "commit
the highlighted row", and it left a trader with a mouse in hand one
step short of everything — a second click to capture, `enter` to open,
no way to reorder or hide at all.

### 17.1 The rules

Three rules, each applying to every modal dialog (§3) and to none of the
filter-only ones (§10 stands: the palette, the picker and the as-of
selector are untouched, and they never freeze their filter row; settings
joined the modal side in §18 and these rules apply to it as §18.2 says):

1. **A click on the frozen filter row enters filter mode.** The frozen
   row (`dialog::filter_row`'s `FrozenFilter` branch) is what every
   modal dialog paints in normal mode — static text where the `Input`
   would be. A mouse-down on it is an unambiguous "I want to type a
   query", so it is the mouse form of `/`: `mode = DialogMode::Filter`,
   a pure mutation, reconciled by the sync. While the keybinding dialog
   is *listening* for a capture — the one frozen state where `/` is not
   the filter's key (`FrozenFilter::slash_filters == false`) — the click
   cancels the capture first and then enters filter mode, because a
   click on a text field is never a keystroke to bind. The live-`Input`
   branches (filter mode, the naming row, the chain field) are already
   focused and need nothing.

2. **A row click does what `enter` would.** A single click on a row is
   the mouse form of moving the cursor there and pressing `enter`, on
   every modal surface whose `enter` opens something:
   - the keybinding dialog: select the row and start listening at once
     (`click_selects_or_listens` becomes `click_listens`; a click on a
     different row mid-capture retargets the capture, a click on the
     same row restarts it with the partial sequence dropped);
   - the object dialog's browse stage: select the row and open its edit
     stage through the one door, `enter_edit_stage` — on Groupings that
     lands in the chain field (4c §18.8), because the door decides, not
     the click. While the naming row is open (`Stage::Naming`) a click
     only selects, as today: a typed name must not be discarded by a
     stray click, and `enter` there creates rather than opens.
   The edit stage's own rows are the subject of 4c §18.9, since `enter`
   has no meaning there and the mouse gains verbs the keyboard has under
   other keys (`space`, `shift+j`/`shift+k`, `x`, `tab`).

3. **Every mouse handler that ends a transition is a sync seam.** §16.1's
   four seam classes become five: the new class is *every mouse handler
   that mutates a dialog's mode, query, stage or draft*, and it is the
   same rule as class 3 stated for the handlers this amendment adds (the
   frozen-row click, the browse click's open, the edit stage's tick
   click, drop and completion click). Each is a pure mutation followed
   by `dialog::sync_dialog_text` on the handler's return, and nothing
   else. `press_verb` stays the audited exception for the reason §16.6
   records.

### 17.2 What does not change

- Filter mode's own mouse behaviour, per dialog — the two dialogs do
  not agree, and rule 2's own `listening` >> `mode` precedence
  (`dialogmode::focus_target`) is why: on the object dialog a filtered
  click still follows the current mode for focus (the field keeps the
  caret) and also opens the row's edit stage; on the keybinding dialog
  a filtered click *captures* — capture outranks mode, so the click
  takes the keys off the field exactly as `enter` would.
- The escape ladder (§5), the vocabulary (§4), the pill.
- No mouse verb has a meaning its keyboard twin lacks. The mouse gets
  parity, never a private capability — the charter's "every action must
  be keyboard-reachable" is the direction that holds.

### 17.3 Tests and harness

Window tests (`shell/tests/keybindings_dialog.rs`,
`shell/tests/objectdialog.rs`): a mouse-down on the frozen row of each
dialog leaves the pill reading `filter` with the `Input` focused; a
mouse-down on the keybinding dialog's frozen row while listening cancels
the capture and enters filter mode; a single click on a keybinding row
that is not the selected one starts listening on it; a single click on
a Views browse row opens `Stage::Edit` on that object, on a Groupings
row opens the chain field, and while naming only selects. Pure tests for
`click_listens`. Harness entries for the frozen-row transition (mutated
back to select-only), the browse click's open (mutated back to
select-only), and `click_listens` (mutated back to the two-click rule).

### 17.4 As built

Implemented 2026-09-12 across the dialog-mouse-parity plan's commit range
`64db372..92af48a` (`git log --oneline ae25ce3..HEAD` on branch
`worktree-dialog-mouse-parity`); rule 3's edit-stage mouse verbs are 4c
§18.9's own surface and their as-built lives at §18.9.6, cross-referenced
below rather than duplicated.

**Rule 1** (commit `64db372`): `FrozenFilter<'a>` gained a third field,
`pub entity: Entity<ShellView>`, so the frozen row's mouse-down can reach
the shell. `dialog::enter_filter_by_mouse(shell: &mut ShellView)` is the
pure mutation — sets `mode = DialogMode::Filter` on whichever of
`keybindings`/`object_dialog` is open, and on the keybinding dialog also
clears `listening` first, since a click on a text field is never a
keystroke to bind and `listening` outranks `mode` in
`dialogmode::focus_target`. `filter_row`'s frozen branch got
`.cursor_text()`, `.debug_selector(|| "dialog-filter-frozen")` and an
`on_mouse_down` running the mutation then `sync_dialog_text`. Window
tests: `clicking_the_browse_frozen_filter_row_enters_filter_mode`,
`clicking_the_edit_frozen_filter_row_enters_filter_mode`,
`clicking_the_frozen_filter_row_enters_filter_mode` and
`clicking_the_frozen_filter_row_while_listening_cancels_the_capture`
(both keybindings). Deferred minors: the frozen-row handler always
`cx.notify()`s even when `enter_filter_by_mouse` is a no-op (unreachable
today — only two callers, both already modal when the row can be
frozen); the four window tests click at `row.origin + (20, 4)`, coupled
to the row's own padding.

**Rule 2** (commits `df25c21`, `5b42b4b` for the keybinding dialog;
`37b369f`, `b6e3915` for the object dialog). `click_selects_or_listens`
**no longer exists in the codebase** — it was replaced outright by
`click_listens`, which always sets `state.selected` and
`state.listening = Some(Vec::new())` in one step, matching what `enter`
already did on a selected row; a click on a different row mid-capture
retargets it, a click on the same row restarts it with any
partially-typed sequence dropped. Pure test:
`a_click_selects_and_listens_in_one_step`. Window test:
`a_single_click_on_a_row_starts_listening`. The rename surfaced three
stale doc/comment sites describing the retired two-click rule (the
module's "Rebind capture" doc section, an inline comment on
`setting_a_query_cancels_an_in_progress_capture`, and a window test's own
doc comment) — all three fixed in the review follow-up `5b42b4b` rather
than left to drift; `settings_view.rs`'s unrelated
`click_selects_or_steps` (a real, still-current two-step rule on a
different, filter-only surface) was left untouched apart from one
comment's cross-reference by name.

The object dialog's `on_row_clicked` now opens the clicked row through
`enter_edit_stage` — on Groupings that lands in the chain field per
§18.8 — unless `Stage::Naming` is open, where a click still only
selects, since a typed name must not be discarded by a stray click and
`enter` there creates rather than opens. Window tests:
`clicking_a_browse_row_opens_its_edit_stage`,
`clicking_a_groupings_row_lands_in_the_chain_field`,
`clicking_a_browse_row_while_naming_only_selects` (the last one's typed
probe text was tightened in a review follow-up, `b6e3915`, from the
object's own name — which made the assertion pass regardless of whether
the click actually left the query alone — to a genuine subsequence that
differs from it). Deferred minor: `on_row_clicked`'s
`scroll_to_item(ix)` is redundant on the open path, since
`enter_edit_stage` scrolls to item 0 itself — plan-mandated code, left
in as harmless.

**Rule 3, the fifth seam class.** `dialog::sync_dialog_text`'s doc
comment (`crates/geode-shell/src/shell/dialog.rs`) now names five seam
classes rather than §16.6's four: the modal-branch tail in
`handle_key_down`, `open_shell_dialog_with_key`, the object dialog's two
confirm-button closures, and every mouse handler that ends a dialog
transition — the frozen-row click above, both dialogs' row clicks
above, and (4c §18.9, see §18.9.6) the edit stage's tick click, row
drop and chain-field completion click. `press_verb` remains the one
audited exception, since it only ever arms a `Confirm` and moves
neither mode nor query. Reaching the sync at the end of a mouse handler
is conditional on getting past that handler's own guards, though — the
tick and drop handlers each open with a notice-clear and an
armed-confirm early return (§18.9.6's "Ruling 2") that returns before
the sync, mutating nothing the sync would have read; that is a claimed-
and-dropped click, not a class the sync forgot.

One consequence worth noting on a dialog Rule 3 does not otherwise
touch: the keybinding dialog's `d`/`r` verbs are keyboard-only, and
Rule 2's `click_listens` above means a click on a row there always
enters capture — so `d` typed right after such a click is a keystroke
being *bound*, not the delete verb. `escape` cancels the capture and
nothing is written until `enter`; this is §17.2 rule 2 working as
designed, not a gap this rule closes.

**Verification.** `cargo test -p geode-shell` was green after every task
in the range (culminating at 1148 lib tests plus the `keymap_integration`
and `tiling_integration` binaries, per the task-7 report); `cargo fmt`
and `cargo clippy -p geode-shell --all-targets -- -D warnings` were
clean after every commit; `zsh scripts/mutation-check.sh --anchors-only`
reported 0 stale/0 ambiguous throughout, rising from 557 (the plan's
starting count, before Task 1) to 576 anchors across the range as each
task's entries landed. Whole-workspace verification (`cargo test
--workspace`, workspace clippy, `--changed`) for the finished branch is
recorded in this plan's task-8 report.

**Follow-up — the command palette (2026-09-12, same day, after merge).**
Rule 2 was written for the modal dialogs and §17.2 left the filter-only
surfaces untouched; the user then asked for the palette too ("allow
entries in the command palette to be chosen by clicking on it"). A
palette row click now selects the clicked row and commits it — the
mouse form of `enter` — through one new door, `ShellView::
commit_selected` (`shell/palette_ctl.rs`), which the `enter` arm of
`handle_palette_key` and `ShellView::render`'s `on_row_click` closure
both call, so the key and the click cannot drift (close first, then
dispatch, and the use is recorded for the ranking either way). Nothing
else on the palette moved: click-outside still dismisses, the panel
still stops propagation, the keyboard is unchanged, and the other three
filter-only surfaces (the picker, the as-of selector — and settings,
which §18 has since made modal but whose click keeps its two-step
select-then-step rule for the reason §18.2 gives) keep select-only
clicks — none of them commits on `enter` in a way a click would
sensibly stand in for. Proved by `click_on_a_result_row_dispatches_
it_like_enter` (`shell/tests/palette.rs`), which clicks row 1 while row
0 is highlighted and asserts the *clicked* theme became active, and
guarded by the harness entry `palette: a row click dispatches the
clicked row` (the click mutated back to select-only). The old
`click_on_a_result_row_selects_it_without_dispatching` test asserted the
superseded rule and was rewritten, not weakened.
## 18. Amendment — the settings dialog goes modal (2026-09-12)

Approved 2026-09-12 from a user request the same day: "Convert the user
settings dialog to having normal mode / filter mode like the key
bindings dialog."

### 18.1 Why §3's ruling is superseded

§3 kept settings filter-only on two grounds: stepping is `tab`, which a
focused input leaves free, and "if settings ever grows a
reset-to-default, it becomes modal by this same rule". Neither ground
changed. What changed is the app around the dialog: the keybinding
dialog and all of Phase 4c's config dialogs are modal, so settings had
become the one *dialog-shaped* surface (title row, filter row, a flat
list of rows with a footer) that read the keyboard the other way. §11's
risk 2 — "a user who learns the config dialogs will press `j` in
settings and type a `j` into its filter" — is the cost that §3's
mitigations were meant to make visible, and the ruling here is that it
is cheaper to remove the difference than to keep signposting it. The
palette, the picker and the as-of selector stay filter-only: none is
dialog-shaped, and §3's reasoning for each stands.

### 18.2 The vocabulary, as this dialog wears it

The shared table of §4, with the settings-specific verbs filled in:

| Key | Normal mode | Filter mode |
|---|---|---|
| `j` / `k` / `g` / `shift+g` | move | (text) |
| arrows, `ctrl+d`/`ctrl+u`, `ctrl+f`/`ctrl+b`, `pageup`/`pagedown` | move | move |
| `space` / `shift+space` | step the value forward / back (§4's `Toggle`/`ToggleBack`, the keys 4c's `Choice` rows use) | (text) |
| `l` / `h` | step forward / back (§4's 2026-09-13 amendment) | (text) |
| `tab` / `shift+tab` | step forward / back | step forward / back |
| `/` | enter filter mode | (text) |
| `enter` | claimed and dropped | claimed and dropped |
| `escape` | the ladder of §5, no nested stage | the ladder's first rung |
| any other bare key | claimed and dropped | text |

`enter` stays inert in both modes: a step applies the instant it
happens, so there is nothing to confirm, and the reason it must be
*claimed* rather than ignored (an unclaimed `enter` reaching the focused
`Input` fires a `Change` that resets the selection) is unchanged.
`tab`/`shift+tab` stay live in both modes rather than being retired:
they were the dialog's stepping keys and a hand that learned them
should not be retrained; normal mode adds `space` beside them. No
per-surface letter verb is claimed — `h`/`l` as a second spelling of
step were considered and dropped as one action under two names.

Mouse parity (§17) applies unchanged: the frozen filter row is the
mouse form of `/` (`dialog::enter_filter_by_mouse` gains a settings
arm), and a row click keeps the dialog's existing two-step rule
(`click_selects_or_steps`: a first click selects, a click on the
selected row steps forward) because this dialog's `enter` opens
nothing, so §17.1 rule 2 has nothing for a click to be the mouse form
of. The click handler ends in `sync_dialog_text` as every row click
does (rule 3), so a click in normal mode leaves the shell root holding
the keys and a click in filter mode keeps the caret in the field.

**Superseded 2026-09-14 (§20.3/§20.8):** the two-step rule and
`click_selects_or_steps` are gone — a row click only selects, and the
value chip is the click target that steps, on this dialog exactly as on
the object dialog.

### 18.3 As built

`settings_view::SettingsState` gained `mode: DialogMode`, opening in
`Normal` through a hand-written `Default` (the same greppable line the
keybinding and object dialogs have). `settings_view::route(mode,
query_is_empty, ks) -> KeyAction` is the whole key table above as one
pure function — `Nav`, `Step`, `EnterFilter`, `LeaveFilter`,
`ClearQuery`, `Drop`, `PassThrough` — and `handle_key` only applies the
answer; every arm is a pure mutation of the state, and
`dialog::sync_dialog_text` (which gained a third arm reading
`shell.settings`) moves focus and the field on the handler's return.
`open` passes `focus_filter: false` and sets the state before the door
runs, and puts the mode pill in the title row through
`dialog::set_title_extra`. The filter row is frozen throughout normal
mode with `slash_filters: true` (this dialog has no capture state in
which `/` means something else), and the footer states the current
mode's vocabulary. One deliberate widening: a `tab` on an empty
filtered list is now claimed and dropped rather than passed through —
before, it fell into the `Input` as a literal tab character.

Window tests (`shell/tests/chrome_and_dialogs.rs`):
`opening_the_settings_dialog_leaves_the_filter_blurred` (replacing
`..._focuses_the_filter`),
`the_settings_dialog_opens_in_normal_mode_and_letters_do_not_type`,
`slash_enters_settings_filter_mode_and_typing_narrows`,
`j_and_k_move_in_the_settings_dialogs_normal_mode`,
`settings_escape_walks_the_ladder_one_rung_at_a_time`,
`the_settings_mode_pill_paints_the_mode_it_is_actually_in`,
`space_and_shift_space_step_the_selected_value_in_normal_mode`,
`space_types_in_settings_filter_mode_rather_than_stepping`,
`tab_still_steps_in_settings_normal_mode`,
`l_and_h_step_the_selected_value_in_settings_normal_mode` (§4's
2026-09-13 amendment, which also put `tab`/`shift+tab` into the
normal-mode footer — withheld there until then on the grounds that they
were filter mode's only stepping keys, which withheld a live key from
the mode with the most of them — marked by a `settings-hint-change`
selector on the `tab` chip; with its `route` test
`h_and_l_step_in_normal_mode_and_type_in_filter_mode` and the harness
entry "settings: h reaches the step table through dialogmode" — that
dialog grew no code of its own for the two keys, so the entry names the
`dialogmode` arm and the settings test together),
`clicking_the_settings_frozen_filter_row_enters_filter_mode` and
`a_settings_row_click_keeps_focus_where_the_mode_says`; the pre-modal
tests now press `/` first where they type. Pure tests on `route` cover
each row of the table. Harness entries: the dialog opening in filter
mode, the query cleared on leaving filter, the clear rung skipped,
`space` stepping in filter mode, filter mode dropping text, and the two
`dialog.rs` arms (the sync's and `enter_filter_by_mouse`'s) ignoring
the settings state.

The display check on a real window is pending, as it is for every
dialog change on this branch's lineage.

## 19. Amendment — footer rows by category (2026-09-14)

User request, 2026-09-14: "the dialog footer could be organized more
consistently, grouping together on lines commands for navigation,
interaction, etc so user can quickly know where to look to find what
they want. Sometimes the `space`/`shift+space` is on the same row as
`j`/`k`, sometimes not." Labelled rows were chosen over unlabelled ones.

**The rule.** Every modal dialog's footer is a flat list of hints, each
tagged with the row it belongs to, and one shared renderer lays them out
as fixed rows in a fixed order, each led by a dim label:

- **move** — the cursor and the viewport: `j`/`k` or `up`/`down`, the
  `ctrl+d`/`ctrl+u`/`ctrl+f`/`ctrl+b` scroll chords, the chain field's
  `up`/`down`, and the prose "type to filter" / "type a value".
- **edit** — anything that changes a value or an object: the stepping
  group (`space shift+space tab h l · change`, or `toggle`/`add` on a
  list row, `tab shift+tab` in filter mode), `shift+j`/`shift+k`
  reorder, `x` remove, `i` type, `n` new, and the keybindings dialog's
  `enter` rebind / `d` unbind / `r` reset.
- **go** — stage and mode changes: `enter` open, `1`–`9` open or jump to
  a slot, `/` filter, `escape` with its honest next rung, the chain
  field's `tab` complete, `enter` apply / `escape` cancel in a field, and
  `enter` go ahead / `escape` leave it alone under a confirm.

A row with nothing in it is not painted (a read-only Schema stage has no
edit row; the naming stage and an armed confirm have only a go row). The
dialogs decide *which* hints are live — §9's mode-honesty rule is
untouched — and never which row a hint sits on; that is the whole point.

**As built.** `geode_shell::footer` is the pure core (`HintRow`, `Hint`,
`rows`), in `dialogmode`'s mould; `dialog::hint_rows` is the one
renderer, taking the hints and the chip colours. The four hand-built
footers — settings, keybindings, the object dialog's browse and edit
stages — now declare hints and call it. Two spellings changed to make
the surfaces read the same: the settings dialog's stepping group is the
object dialog's (`space shift+space tab h l · change`, and `tab shift+tab
· change` in filter mode) rather than its own "next value / previous
value" pair, and the keybindings dialog's capture line is three go hints
rather than one sentence. A hint with a selector gives every chip
`"<selector>-<key>"` and its first chip the bare `"<selector>"`, so a
window test can ask whether a particular key is taught, not only whether
its group is (`settings-hint-change-tab` is how the settings test proves
`tab` is named in normal mode now that the selector no longer rides the
`tab` chip itself). Harness: "footer: a hint's row is its position, not
its category" and "footer: an empty row is painted anyway". Display check
pending on a real window, as §16.6's and §18.3's are.

**Amended 2026-09-19 (user report): every row is laid out every time,
empty or not.** An empty row was dropped, so a footer whose edit row
came and went with the selected row's vocabulary — a `Choice` names the
step keys, a read-only `Text` names nothing — grew and shrank by a line
under the cursor and shifted everything below it. `footer::rows` now
returns all three rows always, and `dialog::hint_rows` paints an empty
one as its label plus an unpainted (`invisible`) chip, so it keeps a
full row's height — the label alone is a `text_xs` line, shorter than a
chip, and would still have moved the footer by a few pixels. Every
modal dialog gets it through the one painter; each row carries a
`hint-row-<label>` selector. Harness: the two entries above were
replaced by "footer: an empty row is dropped from the layout" and
"footer: an empty row is shorter than a full one".

## 20. Amendment — one answer per verb, across every surface (2026-09-14)

User request, 2026-09-14: "review the ui and ensure that verbs and
actions are presented consistently to the user. The same key actions,
focus changes and mouse clicks should behave similarly across the same.
If sometimes esc loses focus and undoes the result and sometimes loses
focus and confirms the results, that would be inconsistent ux."

The audit that followed read every key and mouse handler on every
surface — the scope bar's text field, the palette, the per-tile `/` and
`:` lines, the dimension picker, the as-of selector, the settings,
keybindings and object dialogs, and the blotter, market-data and
diagnostics tiles — and found that `escape` already means one thing
everywhere: **it discards what was being *typed* and never reverts what
was *stepped or ticked*.** Every text field cancels on it (the scope bar
reverts to its pre-focus text, a find line returns the cursor to its
origin, the `:` line, the object dialog's `i` field, the chain field,
the naming row, a rebind capture, the market-data cell editor); every
modal walks §5's ladder; the filter-only surfaces close. That rule is
now stated here so it is a rule and not a coincidence.

Six things did diverge, and the six rulings below settle them. Each
section states the rule first and the mechanism second.

### 20.1 `d`/`r` confirm on every surface that has them

**Ruling.** A destructive verb asks the same question on every surface.
The keybindings dialog's `d` (unbind) and `r` (reset) adopt the object
dialog's confirm: the verb arms a question, `y`/`enter` runs it,
`n`/`escape` withdraws it, every other key is claimed and dropped while
it stands, and both verbs are also offered as `danger` buttons under
the list. This is the direction that adds safety rather than removing
it: the keybinding `d` is the *less* recoverable of the two (a `"none"`
shadow it writes over a builtin binding is one `r` cannot lift —
`reset_selected`'s own doc), and it was the one that asked nothing.

**Mechanism.** `Confirm`, the confirm row (question, two buttons) and
the key router that answers it move out of `objectdialog/render.rs`
into `shell/dialog.rs` as one shared implementation
(`dialog::confirm_row`, `dialog::ConfirmAnswer::from_key`), which both
dialogs consume; the object dialog's behaviour is unchanged by the move.
`KeybindingsState` gains `confirm: Option<Confirm>`. The verbs keep
their existing "nothing to do" notices — `d` on an unbound row, `r` on
a row with no user override — and arm nothing in those cases; a confirm
arms only where the write would actually happen. The footer swaps to
the confirm's own go row while armed (`enter`/`y` go ahead, `escape`/`n`
leave it alone), the same three hints the object dialog paints. Capture
and confirm are mutually exclusive: `d`/`r` are not verbs while
listening (already so — a captured keystroke is never a verb), and a
row click, the frozen-row click and the action buttons are claimed and
dropped while a confirm is armed, exactly as the object dialog's tick
click and drop are (§18.9.2). The buttons ride the object dialog's
`action_bar` shape: an outline button per live verb showing its key
chip and label, `keybindings-action-d` / `keybindings-action-r`
selectors, routed through a `press_verb` that arms and never writes.

### 20.2 The picker walks the escape ladder

**Ruling.** A surface with a previous stage steps back to it on `escape`
before it closes. The dimension picker keeps its staged model — ticks
apply on `enter`, because a scope change is a requery every tile on
screen follows and one per tick would repaint the whole desk on every
keystroke — but its Values stage now takes `EscapeStep::PreviousStage`:
`escape` returns to the Columns stage with the ticks and the Values
query dropped and the cursor on the column just left; a second `escape`
closes, as `escape` on Columns always has. §5's `PreviousStage` rung was
written with "the picker's own two-stage shape" in mind (the
keybindings dialog's own comment says so) and the picker had never
taken it.

**Mechanism.** `handle_values_key` claims a bare `escape` (modifier-
agnostic, the modal branch's own rule) and calls a new
`picker::back_to_columns`, which rebuilds `Stage::Columns` with an empty
query — `commit_column` already gives each stage a fresh query, so this
is the same door walked backwards — and sets `selected` to the position
of the column being left in the unfiltered column list. Nothing is
applied on the way back: `PickerState::apply` is still reached only from
`enter`. The Values footer's `escape` hint reads `back`, Columns' reads
`close`.

The Values row click also changes, as §20.3's rule applied here: a
click on a row **selects** it, and the tick glyph (`✓`/`·`) is the click
target that toggles — the split the object dialog's list rows already
have (a row click moves the cursor; the tick is `space`'s mouse form).
Until now the whole Values row toggled on one click, the one list in the
shell where a click to look at a row changed it.

### 20.3 The value chip is the mouse form of `space`

**Ruling.** A row whose value steps — a `Choice`, a `Number`, a `Bool`,
the settings dialog's five rows — paints that value as a chip, and the
chip is the click target: a click steps forward, a shift+click steps
back, the exact mouse form of `space`/`shift+space`. A click on the rest
of the row selects it and nothing more, on every dialog. This replaces
the settings dialog's two-step rule (§18.2: first click selects, a click
on the selected row steps) — a mouse verb whose keyboard twin was an
inert `enter` — and gives the object dialog's value rows a mouse route
they never had (a trader could change a theme with the mouse but not a
column's `scale`). §17.2's "no mouse verb has a meaning its keyboard
twin lacks" now holds in both directions, because the object dialog's
two remaining keyboard-only verbs gain buttons too: `i` (`Edit value`)
on the edit stage's action bar whenever the selected row is one `i`
opens, and `n` (`New <object>`) on the browse stage wherever
`Domain::writable` holds and the domain has no fixed roster.

**Mechanism.** `dialog::value_chip(text, selector, on_step)` is one
element both dialogs paint: a `mono` chip in the muted fill the key
chips use, `on_mouse_down` reading `event.modifiers.shift` to pick the
direction, `stop_propagation` so the row's own select does not also
run, ending in `sync_dialog_text` like every mouse handler that mutates
a dialog (§17.1 rule 3). It routes through each surface's ONE step path
— `settings_view::apply_setting` via `step`, and
`objectdialog::render::step_selected_row` — so a chip click gives every
refusal and every write the key gives (a read-only domain's notice, a
`Step::Refused`, a fork's notice). The chip degrades to the plain value
text — no handler, no chip fill — on a read-only domain, on a one-option
`Choice` (`Step::Inert`), while a confirm is armed and while a text
field is open, the same four conditions under which the keys are inert;
`Draft::selected_vocabulary` already answers "does this row step", and
the chip reads the same answer for the row it paints. Selectors:
`settings-value-{row}`, `objectdialog-value-{field key}`.
`settings_view::click_selects_or_steps` is deleted and `on_row_clicked`
becomes select-only. The `i` and `n` buttons are two more entries in
`objectdialog::render::actions` (`destructive: false`) reaching
`press_verb`, which is the audited exception of §16.6 precisely because
it only arms or opens — `i` opens the text field through
`open_text_field` and so must end in `sync_dialog_text` (the field takes
focus), which makes `press_verb` no longer an exception: it syncs on
every path, and §16.6's note is amended to say so.

### 20.4 Clicking away from a `/` line commits it

**Ruling.** Clicking away from a text field whose contents have already
applied keeps them — the scope bar's rule (blur ends the session and the
typed text stands) — and a tile's `/` line is such a field: its matches
have moved the cursor on every keystroke. A tile mouse-down while a `/`
line is open therefore **commits** the find (`FindEvent::Committed` with
the field's text: the cursor stays on the match, `n`/`N` repeat it, an
fzf narrowing stays until `escape`) rather than cancelling it (the
cursor jumped back to its origin, the narrowing vanished, and `n` had no
target — a trader who found a row and clicked on it lost the find). A
`:` line still cancels on a click away: nothing typed there has applied,
and a stray click must not run a command. `escape` cancels both, as
before. An empty `/` line commits as a cancel, `vimfind`'s own "an empty
`enter` is a cancel" rule.

**Mechanism.** `ShellView::cancel_command_line` (the `escape` door,
unchanged) gains a sibling `leave_command_line` for the mouse: `Find`
with non-empty text → `Committed(text)` then close; `Find` with empty
text, and `Command` → `cancel_command_line`. The three tile mouse-down
sites (`render.rs`'s tree and dock tile listeners, `drag.rs`'s
`try_arm_tile_drag` path through them) and the render-time backstop
(the focused-tile / focus check at the top of `render`) call
`leave_command_line`; `dialog::open_shell_dialog` and
`toggle_palette` keep calling `cancel_command_line`, since a chord that
opens an overlay is not a click away.

### 20.5 A single step wraps, anything larger clamps

**Ruling.** On every list and every tile cursor, a bare ±1 step —
`j`/`k`, `up`/`down`, `ctrl+p`/`ctrl+n` — wraps at both ends; every
larger step (`ctrl+d`/`ctrl+u`, `ctrl+f`/`ctrl+b`, `pageup`/`pagedown`)
and every count-prefixed step (`5j`) clamps. And every list accepts the
whole `listfilter::nav_command` set. Until now the palette, the picker
and the as-of selector wrapped on ±1 while the three modal dialogs and
every tile clamped; the picker took only four of the ten nav keys and
the as-of selector only two; and the market-data tile lacked the four
full-page keys the blotter has.

**Mechanism.** The rule lives in one place, `vimnav::apply`: a
`NavCommand::Move(d)` with `d.abs() == 1` wraps (the palette's own
modular arithmetic, moved here), every other `Move` clamps, `Top`/
`Bottom` are unchanged, and an empty list still answers `0`. Its doc
comment's "**no wrap**" paragraph is replaced by this rule. Every list
that had its own `move_selection` — `PaletteState`, `PickerState`, the
as-of selector's free function — deletes it and routes
`listfilter::nav_command` through `apply`, which is what gives the
picker and the as-of selector the full key set for free. Two
consequences worth stating: `Cursor::move_rows` multiplies the count
into the delta *before* calling `apply`, so `1j` wraps exactly as `j`
does and `2j` clamps, with no special case for a count of one; and
**visual mode is the one exception** — a wrapping `j` at the bottom
would put the cursor above the anchor and invert the selection, so
`move_rows` clamps a ±1 step while `Mode::Visual`. Columns (`h`/`l`,
`move_cols`, the panel's second axis) clamp: the ruling was about rows
and a horizontal wrap is a separate question, left as it is. The
market-data tile's `move_cursor` takes the same rule on its row axis and
its fragment gains `ctrl+f`/`ctrl+b`/`pagedown`/`pageup` at the
blotter's `page_down_full`/`page_up_full` step. The diagnostics tile is
untouched (§20.6).

**The palette's `tab`.** `dialog::init_reclaimed_keybindings` binds
`tab`/`shift+tab` to `NoAction` inside `GeodeModal` and
`GeodeCommandLine` but never inside the palette overlay, so
gpui-component `Root`'s focus cycling could take the keys off
`palette_input` while the palette stayed open. The palette's panel gains
a `GeodePalette` key context and the same two reclaims. This is a
display-check item — the sandbox cannot show whether `Root` actually
cycled — fixed on the reading of the pinned rev rather than on a
reproduction.

### 20.6 What stays as it is

- **The diagnostics tile** — no `escape`, no `space`/`z a`, no mouse
  handling, a `/` that filters rather than finds — is left untouched by
  ruling: it is to be reworked whole, and this amendment does not
  pre-empt that. Recorded so the audit's finding is not lost.
- **The backdrop click** closes a modal from any depth (a column stage,
  an open text field, an armed confirm, a capture in progress), where
  `escape` walks one rung. Deliberate: the backdrop is "close", not
  "back", and it is the only mouse route out that does not need a
  target.
- **`escape` mid-drag**: a tile drag cancels, a divider drag finishes
  and keeps its resizes. Deliberate and already recorded — a divider's
  resizes applied live, so "stop tracking the mouse" is the honest
  meaning; it is the one `escape` in the shell that keeps a result, and
  it keeps it because nothing was pending.
- **Overlay focus return** goes back to the scope bar's field when it
  was focused at open, and to the shell root otherwise — a market-data
  cell editor open at the time stays open, unfocused, with `escape`
  still cancelling it through the matcher (pinned by
  `the_insert_branch_needs_the_tile_to_hold_focus_not_just_insert_mode`).
  Whether a tile occupant may hold focus through the shell's restore is
  the deferred shell-side decision of market-data §8.8.6; this amendment
  does not make it.
- **`enter` on a row with nothing to open** posts a notice naming the
  right verb in the object dialog and is silently dropped in settings.
  Left: every settings row steps, so there is no verb to name that the
  footer does not already show.

### 20.7 Tests and harness

Pure tests: `vimnav::apply` wraps on ±1 and clamps on ±5/±10 and on a
counted ±1 (`move_rows` with `Some(2)`), and clamps ±1 in visual mode;
`dialog::ConfirmAnswer::from_key` answers `y`/`enter`/`n`/`escape` and
drops the rest; `leave_command_line`'s three arms.

Window tests (`shell/tests/`): keybindings — `d` on a bound row arms and
paints the confirm row, `y` writes the shadow, `n` leaves the file
untouched, a row click while armed is dropped, `d` while listening is
captured not armed; picker — `escape` in Values returns to Columns with
the ticks gone and the cursor on the column, a Values row click selects
without toggling and the tick click toggles; settings and object dialog
— a chip click steps forward, a shift+click steps back, a click on the
row text only selects, the chip is inert on a read-only domain and
under an armed confirm, `i` and `n` buttons open what their keys open;
command line — a tile click with `/foo` typed commits (cursor stays,
`n` repeats), with an empty line cancels, with a `:` line cancels;
palette/picker/as-of — `ctrl+d` moves five, `up` at row 0 lands on the
last row, `ctrl+u` at row 0 stays; market-data — `pagedown` moves ten
and `j` at the last row wraps to the first; blotter — `j` at the last
row wraps, `2j` at the last row stays, visual `j` at the last row stays.

Harness entries (each naming its test): `apply`'s wrap mutated back to
a clamp; the picker's Values `escape` mutated back to fall-through; the
picker's row click mutated back to toggling; `leave_command_line`'s
`Find` arm mutated to cancel; the chip's shift check mutated away; the
keybindings confirm gate mutated so `d` writes unarmed; the visual-mode
clamp mutated away. `--anchors-only` before merge.

### 20.8 As built (2026-09-14)

**§20.5, motion.** `vimnav::apply(selected, len, cmd)` and its sibling
`apply_clamped` are the two doors this whole amendment resolves through;
`Cursor::move_rows(len, cmd, count, wrap)` multiplies the count into the
delta before calling one or the other, with the blotter's own call site
passing `wrap = mode is Normal` (visual mode passes `false`). The
market-data tile's `move_cursor` routes its row axis through `apply` and
its column axis through `apply_clamped`, matching the ruling's "columns
clamp" clause; its fragment's page keys are `page_down_full`/
`page_up_full` at `FULL_PAGE = 10`, the blotter's own step. `PaletteState::
move_selection` is deleted too (the final whole-branch review's I3): it
had survived Task 3 as a thin delegate to `vimnav::apply` on the grounds
that fourteen pure tests named it, but `handle_palette_key` had stopped
calling it — every palette motion already went through
`listfilter::nav_command` + `vimnav::apply` + `set_selected` — so by
controller ruling the method went and the fourteen tests were ported
onto `apply` through a test-local `step` helper spelling exactly what
the fallback arm does, every assertion kept. The picker's free
`nav_delta` function and the as-of selector's own `move_selection` free
function were deleted outright as well, as written. The palette's `tab` reclaim landed as a `GeodePalette` key
context on the palette's panel plus the same `tab`/`shift-tab` → `NoAction`
bindings the modal and command-line contexts already carry — and the
window test asserting `tab` stays on `palette_input` was genuinely red
before the reclaim landed, so this is a fixed defect the sandbox could
see, not only the display-check item §20.5 called it.

**§20.2, the picker.** `picker::back_to_columns` is the function
`handle_values_key` calls on a bare `escape`; the Values stage's tick
glyph carries the selector `picker-tick-{value}` and is the click target
`§20.3`'s split moved the toggle onto. One consequence, by ruling rather
than a bug: `frame::pick_<col>` (and a chip body click) opens straight
into Values, so the first `escape` there lands on a Columns stage the
trader never saw — the ladder is the same ladder whichever door opened
the picker, and a second `escape` closes it.

**§20.1, the confirm.** `dialog::ConfirmAnswer::from_key(&Keystroke) ->
Option<ConfirmAnswer>` and `dialog::ConfirmHandler` are the shared
router and closure type; `dialog::confirm_row(prompt, yes_label,
selector_prefix, entity, on_yes, on_no, cx)` is the shared row, keyed
off `selector_prefix` for its three selectors (`keybindings-confirm`,
`-yes`, `-no` on this dialog; the object dialog's own prefix unchanged).
One deviation from §20.1's wording, worth stating plainly: `Confirm`
itself — the enum, its `Overwrite` variant and its `prompt()` method —
stays where it was, in `objectdialog`, because it is that dialog's own
vocabulary (`d`/`r`/`o`) and the keybindings dialog needed no `Overwrite`
arm. What moved out to `shell/dialog.rs` is the row and the router alone,
never the question type. The keybindings side of it is
`KeybindingConfirm { Unbind, Reset }` on `KeybindingsState.confirm`, with
`can_unbind`/`can_reset` deciding whether a press arms a confirm or falls
through to the existing "nothing to do" notice, `arm_verb` holding that
one arm-or-notice decision so both the key and the button read it once,
and `press_verb`/`action_block` the button-side mirror of the same logic
(`keybindings-action-d`/`keybindings-action-r` selectors).

**§20.3, the chip.** `dialog::StepHandler` is the click closure type and
`dialog::value_chip(text, selector, fg, bg, on_step)` the shared element,
used by both the settings dialog (`on_value_chip_clicked`, replacing the
deleted `click_selects_or_steps`) and the object dialog
(`objectdialog::render::on_value_chip_clicked`). The object dialog's is
`pub(in crate::shell)` rather than private — deliberately, so the Schema
domain's read-only gate can be asserted on the door directly, since every
row Schema's edit stage paints is a display-only `Text` and can never
itself paint a chip to click. `Draft::vocabulary_of(row, domain)` is what
both the chip and the footer read to decide whether a row steps.
`press_verb`'s `i` arm and the `i` keystroke both call the same
`open_field` door, which is why Groupings' whole-chain field is reachable
from the button exactly as it is from the key. `begin_new_object` is the
extracted body `browse_action_bar`'s `n` button and the `n` keystroke
both call; the button reads its dataset seed at click time
(`seed_dataset_under_cursor`), not at paint, so it never derives the
Sources rows once per frame while the bar is up. `actions()` offers `i`
whenever the selected row's `RowVocabulary` is `Types` or
`StepsAndTypes`, or the domain is Groupings (whose whole-chain `i` is
live on every row). Its `d` and `r` pushes each carry a per-push
`!in_column` guard — there is no early return for the destructive verbs
as a group; each is decided on its own line — while `i`'s own condition
carries no such guard, which is how a column stage now paints an `i`
button where §18's original build painted none.

One thing worth stating precisely against §16.6: `press_verb` is no
longer that section's audited exception. It syncs now, because its `i`
arm opens a focused field (`open_field` → `sync_dialog_text` at the tail
of `press_verb` itself, unconditionally on every arm) — the one
condition §16.6 said would make it stop being the exception has now
happened. Its read-only early return (the `READ_ONLY_NOTICE` path) still
returns before that sync, which is harmless by the same reasoning the
tick-click and row-drop handlers' own early returns are: nothing was
mutated that a sync would need to reconcile.

**§20.4, the command line.** `ShellView::leave_command_line` is the
mouse-side sibling of `cancel_command_line`: a non-empty `Find` commits
through `FindEvent::Committed` before closing, an empty `Find` and any
`Command` fall through to `cancel_command_line` unchanged.

**Display checks pending on a real window**, as every dialog change on
this branch's lineage: the value chip's look on both the settings and
object dialogs; the keybindings action bar and its confirm row; the
picker's tick glyph as a click target distinct from the row; and the
palette's `tab` reclaim's actual effect inside a real `Root` (the window
test proves the binding is registered and wins over `Root`'s own cycling
in the test harness, not that a real compositor's tab-cycle agrees).
