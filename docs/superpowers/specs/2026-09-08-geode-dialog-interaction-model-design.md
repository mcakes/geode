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

The filter-only dialogs — settings, picker, as-of — never blur and have no
mode; they keep focusing the `Input` on open and are not routed through the
sync. `init_reclaimed_keybindings` is unchanged. The `Change` subscription
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
filter-only ones (§10 stands: the palette, settings, the picker and the
as-of selector are untouched, and they never freeze their filter row):

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

- Filter mode's own mouse behaviour: a click on a row while filtering
  still follows the current mode for focus (the field keeps the caret),
  and now also opens or captures per rule 2.
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
