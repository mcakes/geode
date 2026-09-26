# Review: `geode-shell` dialog layer

Scope: `src/shell/objectdialog/` (mod.rs 6189, render.rs 4755, views.rs 2460, siblings),
`shell::dialog`/`open_shell_dialog`, `dialogmode`, `choicedialog`/`ChoiceList`, settings
and keybinding dialogs, the as-of dialog, scope-expression dialog, log-level picker,
`sync_dialog_text`, `listrow`/`control` doors, confirm flows, naming/value fields.
Read-only review; every finding cites code read in this session.

## Summary

1. The dialog layer's core invariants hold: `sync_dialog_text` really is the only
   text/focus writer for the four mode-bearing dialogs, the pure draft is the truth,
   and Escape/Enter filter semantics are centralised in `dialogmode` and used by all
   three Normal/Filter surfaces.
2. The real cost is not correctness but *shape*: `objectdialog/render.rs` is one
   4,755-line module holding key routing, six stage transitions, three pointer
   vocabularies and two full render trees, threaded through `&mut ShellView` free
   functions rather than an owned dialog entity.
3. Six domains are dispatched through ~14 hand-written `match self` ladders in
   `mod.rs` plus ~28 ad-hoc `domain == Domain::X` special cases in `render.rs`; the
   latter are where a new domain or stage will silently be forgotten.
4. Four filter-only dialogs (`choicedialog`, `picker`, as-of, scope-expression) each
   write the shared `Input` directly, outside `sync_dialog_text` — a documented
   carve-out, but it means the "one writer" rule holds for 4 of 8 dialogs, not 8.
5. `Draft::visible_rows()` re-ranks and re-allocates the whole row list on every call,
   and is called 4–10 times per keystroke and several times per rendered frame.

---

## Critical

None found. No unwrap/index panic reachable from user-typed text, no draft/text desync
path, and no write that bypasses `config_write` or targets a non-user layer was verified
in this pass. The nearest misses are recorded as Major below.

---

## Major

### M1 — `visible_rows()` is recomputed and reallocated many times per keystroke and per frame

`crates/geode-shell/src/shell/objectdialog/mod.rs:1086-1115`,
`mod.rs:1284-1324` (`snap_selection`), `mod.rs:1135-1140` (`selected_row`),
`render.rs:4384-4430` (`on_edit_row_clicked`), `render.rs:3145-3175` (`build_edit`).

`Draft::visible_rows` builds `self.rows()` (a fresh `Vec<EditRow>`), then a
`Vec<String>` of every row label (one `String` clone per row), then calls
`listfilter::rank`, then sorts. `Draft::selected_row` calls both `rows()` and
`visible_rows()`; `snap_selection` calls both again; `move_selection` calls
`visible_rows()` and then `snap_selection` (two more); `on_edit_row_clicked` calls
`draft.visible_rows()` three times in one handler (`render.rs:4405`, `4412`, plus the
bound check). `is_cursor_stop` → `vocabulary_of`/`column_stage_target` →
`row_label` allocate per probe. One `j` keypress therefore walks the row model
roughly 4–6 times with a `String` per row each time. `snap_selection`'s own comment
(`mod.rs:1288-1292`) correctly identifies the per-probe rebuild as quadratic and
hoists it — but every *caller* still rebuilds.

Impact: per-keystroke and per-frame heap churn on the render thread, which CLAUDE.md
names a reviewable defect. Bounded by field count today (tens of rows), so not a
current budget breach; it becomes one on a wide dataset's column list.

Direction: give `Draft` a memoised `RowModel { rows: Vec<EditRow>, visible: Vec<Ranked> }`
invalidated by a single `dirty` flag set in the handful of mutators, and have
`selected_row`, `snap_selection`, `move_selection`, the click handlers and `build_edit`
borrow it. `row_label` should return `&str` or `Cow<str>` rather than `String`.

### M2 — `objectdialog/render.rs` (4,755 lines) mixes key routing, six stage transitions, pointer vocabularies and two render trees

`crates/geode-shell/src/shell/objectdialog/render.rs` in full; natural seams visible at
`:164-1058` (key routing + stage transitions), `:1059-1500` (edit key table),
`:1591-1828` (field entry), `:2053-2630` (confirm/destructive verbs),
`:2641-2708` (action model), `:2709-3107` (browse render), `:3108-4086` (edit render),
`:4088-4346` (bars), `:4347-4755` (pointer handlers, drag, delivery).

Every function takes `&mut ShellView` and re-derives its own state through
`shell.object_dialog.as_mut()?`; the file contains ~40 such re-borrow preludes. The
module doc is accurate and good, but the file has no internal boundary that the
compiler enforces: `handle_edit_key_inner` (`:1087-1496`, ~410 lines) is a single
function holding the confirm gate, the text-entry gate, the Values `ctrl+a`/`ctrl+x`
special case, the filter branch, the Escape ladder, the writability gate and 14 verb
arms.

Impact: the largest single obstacle to the TODO items (dialog stack, back button).
Any change to one stage requires reading a file no reviewer can hold in context, and
the memory note "a new projection stage touches four `Edit|Column` matches" is the
observed symptom.

Direction: the seams are already legible. Split into `render/` with `browse.rs`,
`edit.rs`, `keys.rs`, `stages.rs`, `verbs.rs`, `pointer.rs`, and lift the repeated
`&mut ShellView` → state → draft prelude into one `struct DialogCtx<'a> { state: &'a mut
ObjectDialogState, scroll: &'a ScrollHandle, config: &'a Config }` so a handler receives
what it needs rather than the whole shell.

### M3 — Domain behaviour is split between trait-like ladders in `mod.rs` and ad-hoc `domain ==` checks in `render.rs`

Ladders: `mod.rs:157-320` (`doc`, `title`, `crumb_noun`, `summary_fn`,
`presentation_doc`, `roster`, `prefix_fn`, `reserved_names`) and `mod.rs:2565-2757`
(`help`, `text_editable`, `parse_text`, `fields`, `fields_from_source`, `draft`,
`new_draft`, `to_table`, `validate`) — 14 `match self` arms over six domains.
Special cases: `render.rs` carries ~28 `state.domain == Domain::X` /
`domain != Domain::Y` / `is_scopes(shell)` tests, e.g. `:311` (`duplicable` for `c`),
`:1834-1852` (Scopes' `space` override), `:1394-1420` (Scopes' `x`),
`:2013-2023` (`is_scopes`), `:3874-3915` (Scopes hint arms), `:3648` (Scopes excluded
from drag), `:3592-3600` (Values-stage section header override), `:1591-1607`
(Groupings' `i`), `:2678-2684` (Groupings' `i` button exception).

The `mod.rs` ladders are defensible — each is one fact per domain and the compiler
forces exhaustiveness. The `render.rs` checks are not: they are scattered behaviour
that a seventh domain will not be reminded about, and `Domain::Scopes` alone reaches
into seven separate sites.

Impact: adding a domain or a projection stage requires finding non-exhaustive `==`
tests by reading, not by compiling.

Direction: promote the scattered predicates to named, exhaustive methods on `Domain`
(`reorderable()`, `space_opens_values()`, `i_edits_whole_object()`,
`removal_verb()`), so `render.rs` asks a question that a new variant must answer.
`Stage` likewise wants `Stage::projects_over_draft()` to replace the five
`Edit { .. } | Column { .. } | Values { .. }` matches at `mod.rs:3020, 3047, 3061,
3119, 3134` — which `set_query`'s own doc already warns must stay in sync by hand.

### M4 — Four filter-only dialogs write the shared `Input` directly, outside `sync_dialog_text`

`choicedialog.rs:380`, `:421` (`set_value("")` + `focus` on the log-level back-step),
`:432` (`let live = shell.dialog_input.read(cx).value()`), `:460-470` (Complete writes
`set_value`); `picker.rs:315`, `:342` (`set_value("")` on stage change);
`scope_expr_view.rs:88` (`set_value(seed)` after the door); `asof_view.rs` reads live
text at `:141`.

CLAUDE.md states `sync_dialog_text` is the only text/focus bridge, and
`sync_dialog_text` itself (`dialog.rs:214-250`) handles only as-of, keybindings,
objectdialog and settings — returning early for everything else. The four filter-only
surfaces are therefore a deliberate carve-out (documented in `input-and-dialogs.md`:
"A filter-only picker retains its own open-time focus path"), but the invariant as
written in CLAUDE.md is stronger than the code.

Impact: two consequences are already visible in the code. First,
`choicedialog::handle_key` must re-feed live text into the model before trusting the
highlight (`:432-437`) because its own `set_value` emits no `Change` — the same
workaround appears in `settings_view.rs:686` and
`objectdialog/render.rs:1694`, three copies of one hazard. Second,
`asof_view.rs:132-137` needs a `debug_assert_eq!` to check the model and field have not
diverged, which is an invariant a single writer would make unnecessary.

Direction: either extend `sync_dialog_text` with arms for the filter-only states (they
all have a single query and always-focused input, so the arms are two lines each), or
state the carve-out explicitly in CLAUDE.md and in `dialog.rs`'s module doc. The
"re-feed live text before Pick" pattern should become one helper rather than three
transcriptions.

### M5 — `revert_failed_write` rebuilds the draft from `services.config`, discarding edits still on the batch

`crates/geode-shell/src/shell/objectdialog/apply.rs:615-663`.

On a write failure the handler restores `pending.revert` docs, then rebuilds the open
draft with `state.domain.draft(&shell.services.config, &name)` (`:637`). Every other
stage-entry path in the dialog deliberately derives from `config_with_pending`
(`render.rs:750-760`, `:774-777`, `:941-948`, `:1937-1970` all carry comments
explaining why a plain `services.config` read is wrong). Here the pending batch has just
been taken (`:617`), so there is nothing to fold — which is consistent — but the
restored draft also silently drops any edit the trader made to a *different* object in
the same batch, and the notice (`:659`) names only the current one.

Impact: after a failed write the trader is told one change was reverted, while the
batch that was reverted may have held several objects' edits. `configuration-dialogs.md`
documents whole-batch restoration, so the behaviour is intended; the *notice* is the
part that under-reports.

Direction: name the batch's object count in the notice, or list the touched documents
as `run_confirmed` already does for removals (`render.rs:2588-2600`).

### M6 — `press_verb` is a string-keyed dispatch that silently drops unknown keys

`crates/geode-shell/src/shell/objectdialog/render.rs:4347-4376`;
mirrored in `keybindings_view.rs:919-940` with a `char` key.

`press_verb(shell, key, ...)` matches `"d" | "r" | "i" | "o"` and has `_ => {}`
(`:4373`). `actions()` (`:2641-2700`) independently decides which of those four to
paint. The two lists are related only by convention; adding a fifth verb to `actions()`
without touching `press_verb` produces a button that does nothing, and the `_ => {}`
arm makes that a silent failure rather than a compile error. The same shape exists in
`browse_action_bar` (`:4137-4283`), which hand-rolls `n` and `c` inline rather than
going through `press_verb`, and duplicates the notice-clearing prelude twice
(`:4188-4192`, `:4228-4232`).

Impact: the mouse and keyboard routes can disagree, which is exactly the parity rule the
module doc claims to enforce. Latent rather than live — all four verbs are wired today.

Direction: make `Action` carry the handler (`fn(&mut ShellView, &mut Window, &mut
Context<ShellView>)`) rather than a key string, so `actions()` is the single source and
the bar cannot paint an unhandled verb. `n` and `c` then join the same list.

### M7 — `handle_edit_key_inner`'s writability gate lists verbs by hand and can be outgrown

`crates/geode-shell/src/shell/objectdialog/render.rs:1338-1356`.

The read-only refusal matches an explicit set:
`Toggle | ToggleBack | EditText | MoveItem(_) | Verb('d' | 'r' | 'x' | 'n' | 'o')`.
A new mutating verb — or a new `NormalCommand` variant — is writable-by-default on
Schema until someone remembers to add it here. The inverse framing (list the *safe*
commands: `Nav`, `EnterFilter`, `Commit`, and a bare unclaimed letter) would fail
closed. Note the comment at `:1340-1343` correctly states the intent ("every verb that
would change the object is refused here, in one place") but the implementation is an
allow-list of mutations, not a deny-by-default.

Impact: a future verb reaching a read-only domain writes where the scaffold promised it
could not. `Domain::writable` is the whole guarantee for Schema.

Direction: invert to `matches!(cmd, Nav(_) | EnterFilter | Commit | Verb(_))` with the
mutating arms falling through to the refusal, or add a
`NormalCommand::mutates(&self) -> bool` on the enum so the match lives beside the
variants it classifies.

### M8 — The three `escape_step` consumers each re-derive "unreachable" rungs and fold them into `false`

`render.rs:236-273` (browse), `render.rs:1252-1330` (edit),
`keybindings_view.rs:514-533`, `settings_view.rs:700-720`.

Each site walks `dialogmode::escape_step` and then hand-comments which rungs cannot
fire from there, folding them into `_ => return false`. The comments are correct and
carefully reasoned (`render.rs:1315-1329` is 15 lines of explanation), but the net
effect is that the ladder's *shape* is asserted four times in prose and nowhere in a
type. The edit stage additionally reaches `PreviousStage` and then re-tests the stage
twice more (`:1276-1311`) to choose between `leave_values_stage`,
`leave_column_stage` and `leave_edit` — a third stage ladder.

Impact: this is the mechanism behind the TODO's "back button" being hard: "what does
going back mean here" is answered in three places by three different tests.

Direction: have `escape_step` return the *resolved* action for the caller's state —
e.g. `EscapeStep::PreviousStage(Stage)` or a `Back { to: Stage }` — computed from
`ObjectDialogState`, so there is one function that knows the stage graph. That function
is also exactly what a back button and a dialog stack need.

---

## Minor

### N1 — `hint_rows` parses hardcoded keystrokes on every rendered frame

`crates/geode-shell/src/shell/dialog.rs:658-720`, specifically `:664-667`.

`chip(spec)` calls `crate::keymap::parse_keystroke(spec, Modifiers::NONE)` with
`.expect("footer hint keystrokes are hardcoded valid")` for every chip of every hint row,
every frame. The edit footer paints 6–10 hints with 1–5 keys each, so this is roughly
15–30 parses plus a `Keystroke { key: String }` allocation each, per frame, for values
that are compile-time constants. Same pattern at
`render.rs:4106`, `:4165`, `:4205`, `:4254`, `keybindings_view.rs:994`, `:1157`,
`picker.rs:775`.

Direction: parse once into a `OnceLock<HashMap<&str, Keystroke>>`, or have
`footer::Hint` carry pre-parsed keystrokes since its keys are already `&'static str`
literals.

### N2 — Literal shadow colours and pixel gaps in `dialog.rs`

`crates/geode-shell/src/shell/dialog.rs:251-257` (`hsla(0., 0., 0., 0.1)` twice, and
raw `px(20.)/px(25.)/px(-5.)/px(8.)/px(10.)/px(-6.)`), and `:343` (`.gap(px(6.))`).

CLAUDE.md forbids literal colours and unexplained fixed pixels. The shadow has a doc
comment justifying the fixed neutral black in both themes, which satisfies the
"unexplained" half, but the values sit outside `scale::design` and the theme's own
tokens while every other geometry in the file goes through `scale::design`. The
`gap(px(6.))` at `:343` has no comment at all.

Direction: route the gap through `scale::design(6.)` for consistency with its
neighbours; if gpui-component has no shadow token, keep the literal but name it a
`const OVERLAY_SHADOW_ALPHA` so the two copies cannot drift.

### N3 — `objectdialog/mod.rs` is ~48% test code in the same file

`crates/geode-shell/src/shell/objectdialog/mod.rs:3217-6189` is `mod tests` — 2,972 of
6,189 lines. The production half is 3,217 lines, which is large but not extreme; the
headline "6.2k-line file" is mostly tests.

Direction: move to `objectdialog/tests/` (or `mod.rs` + `tests.rs` via
`#[path]`) so the production surface is what a reader opens. Worth noting because the
size figure drives the perception of the module's complexity, and the real complexity
is in `render.rs`.

### N4 — Six `unreachable!` arms in `Destination::doc` encode a two-dimensional fact as a flat match

`crates/geode-shell/src/shell/objectdialog/mod.rs:577-625`.

`Destination::doc(self, domain)` matches `(Destination, Domain)` pairs and panics on
seven of the twelve combinations, each with a 3–6 line comment explaining that no field
of that domain carries that destination. The invariant is real, but it is enforced by a
panic at a call site rather than by construction — a `Field` whose `dest` and domain
disagree is representable.

Impact: a panic in a dialog is a crashed shell. The comments assert unreachability from
adapter behaviour, which is exactly the kind of cross-module claim that a refactor
breaks.

Direction: either return `Option<&'static str>` and let the writer skip an
unrepresentable pair, or give each domain its own destination enum so the pairing is
type-level. At minimum the arms could collapse to one `_ =>` with a single comment,
since all six say the same thing.

### N5 — `select_item_named` and `leave_values_stage` reset the cursor with `unwrap_or(0)`, bypassing the cursor-stop rule

`render.rs:1005-1027` (`leave_values_stage`, `.unwrap_or(0)` at `:1023`);
`mod.rs:1629-1666` (`select_item_named`, `self.selected = 0` at `:1630`).

Both set a raw index before any settle. `leave_values_stage` does call
`scroll_to_cursor` but never `settle_selection`, so a Values stage left on a scope whose
dimension row has been filtered away lands the cursor on row 0 — which on Scopes is a
display-only summary `Text`, i.e. not a cursor stop. `apply.rs:653-656` explicitly
fixes this class of bug for the failed-write path with a comment
("Every arm above sets the cursor from something the rebuilt draft did not choose …
takes the same door"), so the rule is known; `leave_values_stage` looks like a missed
site.

UNVERIFIED whether reachable in practice: `leave_values_stage` is called from the
Escape ladder (`render.rs:1281`) and the stage's own exit, and the fold restores the
scope's fields, so the dimension row normally exists. Confirming would need a test that
filters the scope's rows, enters Values on a dimension, then escapes.

Direction: call `draft.settle_selection(domain)` after the `unwrap_or(0)` in
`leave_values_stage`, matching `apply.rs`'s treatment of the same hazard.

### N6 — `settings_view` has no notice slot, so refusals are silent

`crates/geode-shell/src/shell/settings_view.rs:693-697` ("Nothing lit: the field stays
open. The settings dialog has no notice slot; the empty list says it"), and
`:721` (`KeyAction::Drop => return true`).

Every other dialog in the layer reports a deliberately-inert keystroke —
`objectdialog` sets `state.notice` in ~25 places, `keybindings_view` has
`notice: Option<String>`. Settings drops claimed keys with no feedback. The comment
argues the empty list is the message, which is true for a failed Pick but not for
`KeyAction::Drop`, nor for `KeyAction::Step` on an empty filtered list
(`:660-667`, which is claimed and does nothing).

Impact: the "a key that appears inert is the defect class this interaction model exists
to remove" principle, stated repeatedly in `objectdialog`, is not applied in Settings.

Direction: add the same one-line notice slot; the footer already has the row for it.

### N7 — `choicedialog`'s digit jump tests `text().len() == 0` on a rope

`crates/geode-shell/src/shell/choicedialog.rs:483`.

`shell.dialog_input.read(cx).text().len() == 0` — `len()` here is the rope's byte
length, so the test is "no bytes", which is correct for emptiness but reads as a
character count. `is_empty()` would say what is meant. Separately, `is_digit`
(`:501-504`) indexes `key.as_bytes()[0]` after a `len() == 1` guard, which is safe for
byte length 1 but would be clearer as `key.chars().all(|c| c.is_ascii_digit()) &&
key.chars().count() == 1`.

No bug: both are guarded. Clarity only.

### N8 — `keybindings_view`'s confirm `on_yes` synthesises a `y` keystroke to reach its own handler

`crates/geode-shell/src/shell/keybindings_view.rs:949-956`.

The pointer confirm handler constructs `Keystroke { mods: NONE, key: "y" }` and calls
`handle_key(shell, &ks, window, cx)`. It works, and it guarantees pointer/keyboard
parity by construction, but it routes a mouse click through keystroke parsing and the
whole `handle_key` prelude (row derivation, notice clearing, capture check) to reach one
branch. `objectdialog` solves the same problem properly with
`answer_confirm(shell, true, cx)` as a named door (`render.rs:2053-2070`,
`:4297-4301`).

Direction: extract the confirm-answer body into `answer_confirm(shell, yes, cx)` as
`objectdialog` does, and have both the key branch and the button call it.

### N9 — `on_row_dropped` computes `resolves` before mutating, then uses it after

`crates/geode-shell/src/shell/objectdialog/render.rs:4676-4700`.

`let resolves = draft.locate(src).is_some() && draft.locate(dst).is_some();` runs
*before* `draft.drop_row(src, dst)`, and the result is consulted in the
`Step::Inert if !resolves` arm afterwards. Correct as written — the point is to know
whether the rows existed at drop time — but the ordering is load-bearing and
uncommented, and `locate` walks the row model twice more (see M1).

Direction: a one-line comment stating that `resolves` is deliberately a pre-mutation
snapshot; or have `drop_row` return the distinction in its `Step`.

### N10 — `i_hint_word` and `field_value` index `draft.fields[i]` without bounds checks

`render.rs:3089-3106` (`draft.fields[i].kind`), `mod.rs:1063-1085` (`row_label`:
`self.fields[i].label`, `items[item].name`), `mod.rs:1156-1180`
(`vocabulary_of`: `self.fields[i].kind`), `mod.rs:1988-2005` (`apply_text_entry`:
`&mut self.fields[index]`).

All are reached with an `EditRow` that was derived from `self.rows()`, so the indices
are valid by construction, and `Draft` is not `pub`-mutable from outside the module.
But `EditRow` is `Copy` and `pub`, and `TextEntry { row }` stores one across mutations
(`mod.rs:897-905`): `apply_text_entry` indexes `self.fields[index]` from a `TextEntry`
captured when the field opened. If any path between `begin_text_entry` and
`apply_text_entry` shortened `fields`, that is a panic on Enter.

UNVERIFIED that such a path exists: `enter_column`/`enter_values`/`leave_*` all clear
`text_entry` (`mod.rs:1411`, `1558`, `1580`, `1608`), and `reseed_fields` is only called
from `leave_column_stage` and `deliver_values`, both of which follow a
`text_entry`-clearing transition. Confirming would need a test that reseeds fields while
a plain field is open.

Direction: `self.fields.get(index)?` in the three read sites; `apply_text_entry` should
return `Step::Inert` rather than indexing.

### N11 — `crumb_text` derives all rows just to count them, on every frame

`crates/geode-shell/src/shell/objectdialog/render.rs:136-160`, specifically `:152`
(`let n = derive_rows(shell).len();`).

`derive_rows` → `Domain::objects` → `derive_rows(config, ...)` walks every layered doc,
builds a `BTreeMap<String, (Vec<Layer>, ObjectRow, Option<toml::Value>)>`, clones
summaries and shadow `toml::Value`s, computes drift against `overrides.toml`, and sorts —
all to produce a count for the title crumb. `build` then does it again at `:2763`.

Impact: two full config walks with per-object `toml::Value` clones per frame, in a modal
that repaints on every keystroke. The module doc's "derived fresh at every call site,
never cached" rule is a good default, but a *count* does not need the rows.

Direction: `build` already has `rows`; pass the count into the title-extra closure
through the state, or give `Domain` an `object_count(config)` that counts keys without
materialising rows.

### N12 — `filter_entry_query` is one field serving two query slots, correct only by an invariant kept in prose

`crates/geode-shell/src/shell/objectdialog/mod.rs:2862-2876`.

The doc comment is explicit and correct: one snapshot field is safe because "a filter
session can never span a stage change: every stage transition sets `DialogMode::Normal`
explicitly", naming `enter_edit`, `enter_column_stage`, `enter_values_stage`. That is
three call sites that must each remember one line, enforced by a comment. A fourth
transition preserving `Filter` would revert one stage's query to another's text.

Direction: move the snapshot into the slot it belongs to — `Draft::filter_entry_query`
for draft-owned stages, `state.filter_entry_query` for browse/naming — so the pairing is
structural. This is the same fix as the `effective_query`/`effective_selected` stage-list
duplication (M3).

### N13 — `Draft::begin_chain_entry` returns `()`, so its caller must probe `text_entry` to learn whether it worked

`render.rs:1591-1620` (`open_field`), `groupings.rs:180-204`.

`begin_chain_entry` returns nothing and silently does nothing on a draft with no
`dimensions` field (`groupings.rs:181-183`). `open_field` therefore checks
`if draft.text_entry.is_some()` afterwards (`render.rs:1610-1614`) to decide whether to
switch mode — with a comment explaining that a mode switch without a field open would
make the next Escape revert the query instead of cancelling the field. Its sibling
`begin_text_entry`/`begin_choice_entry` both return `Step`, and `open_text_field`
correctly tests `step == Step::Changed` (`render.rs:1655-1662`).

Direction: make `begin_chain_entry` return `Step` like its two siblings.

### N14 — `groupings::completed_names` slices by byte arithmetic on user text

`crates/geode-shell/src/shell/objectdialog/groupings.rs:152-157`.

`let head = &text[..text.len() - trailing_segment(text).len()];` — byte arithmetic on a
user-typed chain. It is safe: `trailing_segment` returns a suffix produced by
`rsplit(is_separator).next()` (`:146-150`), so `text.len() - suffix.len()` always lands
on a char boundary. Given the repo's double-encoding incident, worth stating.

Direction: a one-line comment that the subtraction is boundary-safe because the operand
is a suffix of the same string; or `text.strip_suffix(seg).unwrap_or(text)`, which makes
it obvious.

### N15 — Match-highlight indices are char offsets, correctly handled

`crates/geode-shell/src/listfilter.rs:18`, `:29-46`, and its test
`indices_are_char_offsets_into_the_row_text` (`:147-151`);
`keybindings_view.rs:901-918` (`highlighted_text` → `palette::highlight_runs` converts
char offsets to byte ranges); `palette.rs:508` (`split_label_indices`);
`render.rs:2808-2836` (prefix split uses `prefix.chars().count() + 3`);
`keybindings_view.rs:1060`, `settings_view.rs:~870` (`row.title.chars().count()`).

Verified clean: every consumer uses `chars().count()` for the split point and lets
`highlight_runs` do the char→byte conversion. No byte/char confusion found in the
highlight path. Recorded as a positive with locations, since it is the exact class the
repo has been bitten by.

### N16 — `search`/`crumb`/`summary` strings are rebuilt per frame per row

`mod.rs:3186-3192` (`searchable_text` → `format!`), `mod.rs:143-149`
(`display_name` → `format!`), `render.rs:2805` (`row.display_name()` per row per frame),
`render.rs:3194-3200` (`field_value(field)` → `String` per row per frame),
`render.rs:4030-4051` (`section_header_text`), `render.rs:3966-3990`
(notice newline flattening, correctly gated on `contains('\n')`).

Each browse row allocates 2–3 `String`s per frame (`display_name`, `searchable_text`
inside `visible_rows`, plus selector strings at `:2843`, `:2856`, `:2862`,
`:2870`). Ten visible rows is ~30 allocations per frame plus the selector closures.
Bounded and modest, but it is the pattern CLAUDE.md calls per-frame heap churn, and the
`debug_selector` closures allocate even in release unless gpui compiles them out.

UNVERIFIED whether `debug_selector` is a no-op in release builds — if it is, roughly
half these allocations vanish and this finding shrinks. Confirming means reading
gpui-component's `debug_selector` definition in the registry source.

Direction: cache `display_name`/`searchable_text` on `ObjectRow` at derivation (they are
already derived once per call); leave the rest.

---

## Ideas (TODO.md feasibility)

### I1 — Dialog stack ("allow dialogs on top of dialogs") — moderate, blocked on three singletons

Three things make the current design single-dialog by construction:
`ShellView` holds one `modal: Option<ShellModal>` (`mod.rs` field, used at
`input.rs:568-596`); one `dialog_input: Entity<InputState>` shared by every dialog
(`mod.rs:1161`, doc at `:1691`: "the retained input outlives dialogs"); and eight
sibling `Option<...State>` fields that `close_modal` clears together
(`mod.rs:1695-1708`) precisely because the `InputEvent::Change` subscription routes by
"whichever state is `Some`" (`mod.rs:1162-1200`).

That subscription is the real blocker: with two dialogs open, two states are `Some` and
the router's first-match wins arbitrarily (its own comment at `:1168-1172` says order
"carries no meaning" — true only while at most one is `Some`).

Feasible path: replace the eight fields with `Vec<DialogEntry>` where
`DialogEntry { modal: ShellModal, state: DialogState }` and `DialogState` is an enum
over the existing state types. Route `Change` to `stack.last_mut()`, and make
`sync_dialog_text` read the top entry. `open_shell_dialog` already clears competing
transient state in one place (`dialog.rs:150-197`) and every `open` guards on
`modal.is_some()` (nine sites), so the push/pop discipline has one door to change.
Escape's fallback (`input.rs:589-592`) becomes pop-one rather than close-all.
Estimated as a contained refactor of `dialog.rs` + the `Change` subscription, not of
the dialogs themselves.

### I2 — Back button — easy once M8's ladder is a function

The state is already there: `has_previous_stage()` (`mod.rs:3176-3184`) answers whether
a back target exists, and `crumb_text` (`render.rs:136-160`) already paints the path.
What is missing is a single function returning *where* back goes; today that decision is
spread across `escape_step` plus two stage re-tests in the Escape arm
(`render.rs:1276-1311`). Implement M8's `Back { to: Stage }` resolver, then the button is
`Button::new("objectdialog-back").on_click(|…| go_back(shell, …))` in the title row
beside the close button (`dialog.rs:585-599` is the slot), dispatching the same door
Escape uses. Keyboard route already exists (Escape), satisfying the parity rule.

### I3 — Drag-reorder in the view editor: the "clicking immediately goes to next page" report is explained by the click/drag overlap

`render.rs:3648-3707` attaches `on_drag` to list rows, and `render.rs:3628-3640`
attaches `on_mouse_down` → `on_edit_row_clicked` to the same element.
`on_edit_row_clicked` opens the column stage when the clicked row is a door row
(`:4434-4446`) — which for a Views member row it always is. So the mouse-down that
*starts* a drag also opens the column stage on the same gesture, and the drag then has
no list to drop onto. `click_opened_stage` (`mod.rs:2887-2897`) guards the
double-click case but not the drag case.

Direction: the fix is to defer the door-opening to mouse-*up* when no drag started, or
to gate `on_edit_row_clicked`'s stage-opening on `event.click_count == 2` for rows that
are also drag sources (keeping single-click as select-only there). The keyboard route
(Enter) is unaffected. This is the highest-value small fix in the TODO list, and it is a
real interaction bug rather than a missing feature.

### I4 — Grouping `i` semantics ("i != edit value, should say enter text or something")

`render.rs:1591-1607`: on Groupings, `i` ignores the selected row and opens the slot's
whole chain field. The footer already special-cases the label to "type a chain"
(`render.rs:3880-3884`), but two other surfaces still say "Edit value": the action-bar
button label (`render.rs:2681`) and `i_hint_word` (`render.rs:3089-3106`), which returns
"type a value"/"choose a value" and is not consulted on the Groupings path
(`:3878-3886` pushes its own hint instead).

Direction: give `actions()` the domain-aware label — `"Type the chain"` on Groupings,
`"Edit value"` elsewhere — reading it from the same place the footer does. Best folded
into M3's `Domain::i_edits_whole_object()` so the three sites answer from one fact.

### I5 — Extract a `dialog-kit` so feature crates stop half-reimplementing the field editor

`geode-timeseries/src/popup.rs` (1,278 lines), `geode-marketdata/src/popup.rs` (916),
`geode-pricer/src/popup.rs` (408) each build an anchored popup with a filtered choice
list, row paint, hover states and a segmented field.

They do reuse the right primitives — `geode_shell::choice::ChoiceList`,
`shell::scale`, `shell::listrow::row_paint`, `shell::control`, `shell::chip`
(imports at `timeseries/popup.rs:36-41`, `marketdata/popup.rs:13-14`,
`pricer/popup.rs:7-8`) — and `marketdata` even static-asserts its row cap equals
`choice::DEFAULT_CAP` (`:185`). So this is not duplicated *mechanism*.

What *is* triplicated is the popup surface geometry, verified byte-for-byte:

- `fn popover_surface(cx: &App) -> Div` is character-identical in all three —
  `timeseries/popup.rs:551-558`, `marketdata/popup.rs:40-47`,
  `pricer/popup.rs:28-35` — same seven lines
  (`v_flex().min_w(scale::design(MIN_WIDTH)).p_1().gap_y_0p5().text_sm().popover_style(cx)`),
  differing only in `pricer`'s `pub(crate)`.
- `const ROW_HEIGHT: f32 = 26.0;` and `const MIN_WIDTH: f32 = 240.0;` are declared
  separately in each crate — `timeseries:60,64`, `marketdata:29,33`,
  `pricer:16,20` — with identical values.

The *row* builders legitimately differ: `timeseries::row_shell` (`:641-663`) is a
generic highlight/hover row shell, while `pricer::menu_row_paint` (`:203-235`) encodes
menu-family disabled-row semantics against `shell::listrow`'s documented rule. Those are
per-crate concerns and should stay put.

So the duplication is ~10 lines of shared popup chrome, not a re-implemented field
editor. That is small but it is exactly the class the shell already solved with doors:
`listrow::row_paint`, `control::paint`, `chip::chip_paint` exist so three modules cannot
drift on a colour — and nothing plays that role for popup geometry, so three copies of
`240.0`/`26.0` can drift silently and a theme sweep would not catch it.

Direction: add a `shell::popover` door beside the existing three
(`popover_surface(cx) -> Div` plus the two constants) and have the three popups call it.
Ten lines moved, one owner. The larger `dialog_kit` extraction —
lifting `FieldKind`/`RowVocabulary` (`objectdialog/mod.rs:670-836`) out of its
`Domain`/`Destination` coupling so modules get the whole field-editor model — is *not*
warranted on this evidence: the popups reuse `ChoiceList` already and hand-roll only
their own row shapes. Revisit if a fourth module needs typed value rows.

---

## Systemic patterns

**Comments carry the invariants that types should.** The dialog layer is unusually
well commented — `mod.rs` and `render.rs` explain nearly every non-obvious decision, and
several comments document bugs that were fixed (`render.rs:617-626` on `open_save_scope`
mutating someone else's dialog; `mod.rs:1379-1391` on `enter_column` re-entry destroying
a view's column list). But a recurring shape is *"these N call sites must agree, and a
comment at each says so"*: the five `Edit|Column|Values` stage lists (M3), the three
`DialogMode::Normal` assignments guarding `filter_entry_query` (N12), the four
`escape_step` rung analyses (M8), the `actions()`/`press_verb` verb pair (M6), the three
"re-feed live text before Pick" copies (M4). Each comment is correct; collectively they
are a type system written in prose.

**Free functions over `&mut ShellView` instead of an owned dialog.** Every handler in
`render.rs`, `settings_view.rs` and `keybindings_view.rs` re-derives its state through
`shell.object_dialog.as_mut()?`, producing ~40 near-identical preludes in `render.rs`
alone and forcing the borrow gymnastics visible at `render.rs:187-199`,
`:773-800`, `:941-975` (read config, drop borrow, re-borrow mutably, re-check
`Option`). The gpui guides' "state ownership" section would put this state in its own
entity with its own `render`; the current shape exists because the shared `dialog_input`
and single `modal` slot live on `ShellView`. Fixing I1 would also fix this.

**Deliberate-inertia discipline, unevenly applied.** `objectdialog` is rigorous about
never letting a keystroke appear to do nothing — ~25 `set_notice` calls, a `Step`
enum distinguishing `Inert` from `Refused` (`mod.rs:1003-1024`), and
`refuse_step` (`render.rs:1883-1906`) that names the *right* remedial verb by reading
the same `target_row` the action bar uses. Settings has no notice slot at all (N6) and
`choicedialog` relies on the empty list to speak. The principle is stated in
`objectdialog` and honoured there; it has not propagated.

**Pointer/keyboard parity is achieved by call-site convention.** Every pointer handler
ends with `dialog::sync_dialog_text(shell, window, cx); cx.notify();`
(`render.rs:4376`, `:4479`, `:4523`, `:4563`, `:4592`, `:4613`, `:4703`) because
pointer paths do not pass through the key handler's tail (`input.rs:577-582`). Seven
copies of a two-line epilogue that a wrapper could own. The one that forgets is a
silent desync.

**Performance rules are followed where measured and not where assumed.** `build_edit`
hoists `flagged_rows`, `ProvenanceInputs` and the named-colour resolution out of the row
loop with comments citing per-frame churn (`render.rs:3160-3175`, `:2751-2762`,
`:4328-4340`), and `snap_selection` hoists the row model out of its probe loop. Yet
`visible_rows` is called 4–10× per keystroke (M1), `crumb_text` walks the whole config
per frame (N11), and `hint_rows` parses ~20 keystrokes per frame (N1). The hoists
happened where someone profiled; the rest is untouched.

**Test coverage is heavily window-test weighted.** `tests/objectdialog.rs` is 9,436
lines / 160 window tests; `chrome_and_dialogs.rs` 57, `keybindings_dialog.rs` 52,
`asof.rs` 20, `grouping.rs` 9, `picker.rs` 7, `scope_expr.rs` 5. Pure-state tests live
inside the production files (`mod.rs:3217-6189` ≈ 90 tests, `views.rs:1166+`,
`groupings.rs:367+`, `scopes.rs:531+`, `settings_view.rs:1080+`,
`keybindings_view.rs:1278+`, `dialog.rs:862+` — one test for `ConfirmAnswer::from_key`).
The split is sound and matches CLAUDE.md's "lowest layer that proves the behaviour".

Gaps worth naming, from the test-name listing:
- **Uncovered:** no test drives `leave_values_stage` with a filtered row list (N5's
  suspected cursor-stop bypass); no test reseeds fields while a plain text field is open
  (N10's suspected panic); no test asserts `press_verb`'s `_ => {}` is unreachable (M6);
  no test covers a multi-object batch failing its write and reporting the count (M5).
- **Only covered at window level:** the Escape ladder's per-stage rungs
  (`slash_filters_and_escape_walks_the_ladder`,
  `escape_goes_back_a_stage_before_it_closes_the_dialog`,
  `slash_filters_the_edit_stage_and_escape_walks_the_full_ladder`) — the *composition*
  of `escape_step` with stage state has no pure test, which is exactly what M8's
  resolver would make testable pure.
- **Well covered:** the failed-write/revert path (4 tests), confirm-target recording
  under reload (`a_reload_under_an_armed_confirm_refuses_the_answer`), cursor stops
  (5 tests), double-click-vs-door (4 tests), read-only refusals across every route
  (chip, tick, drop, click, key, button).

---

## What is done well

- **The pure/gpui split is real and load-bearing.** `dialogmode` (150 lines, no gpui),
  `listfilter`, `choice`, `vimnav`, `footer` are genuinely pure and shared by every
  surface; `Draft` and `ObjectDialogState` hold no gpui type, which is why ~90 pure
  tests can pin the semantics without a window.
- **`sync_dialog_text` is a genuine seam for the dialogs it covers.**
  `dialog.rs:214-250` is the only writer of text+focus for objectdialog, settings,
  keybindings and as-of, it is called from exactly one place on the key path
  (`input.rs:577-582`, deliberately even for unclaimed keys), and pointer handlers call
  it explicitly. The `set_value` emits-no-`Change` hazard is documented at the seam
  (`:203-213`) rather than rediscovered per call site.
- **Confirm flows record their target.** `confirm_target`
  (`mod.rs:2908-2918`) plus the compare in `run_confirmed`
  (`render.rs:2554-2566`) correctly close the reload-reorders-the-list-under-an-armed-
  question hole, `disarm()` (`mod.rs:2945-2948`) is the one door that clears both
  fields, and the prompt paints the *recorded* name (`render.rs:2990-2994`) rather than
  the cursor's current answer. The armed state blocks keys, row clicks, tick clicks and
  drops (`render.rs:1099-1110`, `:4401`, `:4553`, `:4686`).
- **Identity, not position, for click routing.** `filtered_position`
  (`mod.rs:3207-3216`), `keybindings_view::filtered_position` (`:252-263`) and
  `settings_view::filtered_position` all turn a domain identity back into a filtered
  index at the click, and every selector is domain-derived
  (`objectdialog-row-{name}`, `objectdialog-tick-{name}`,
  `objectdialog-drag-{field}-{own}-{name}`). This satisfies the stable-identity rule
  exactly, including the drag ID's explicit reasoning about collisions
  (`render.rs:3650-3660`).
- **Drag payloads resolve by name at drop time.** `RowDrag` carries
  `{field, own, name}` and never an index (`mod.rs:838-856`), with the reasoning stated:
  the keyboard stays live during a drag, so an index could land on a different column.
- **Pending-aware config reads at every stage entry.** `config_with_pending`
  (`apply.rs:441-455`) and its use at `render.rs:750`, `:774`, `:941`, `:1962`,
  `:2228-2237`, and in `create_from_name`'s copy/frame seeds (`:492`, `:518`) close the
  "reopened draft overwrites a debounced edit" hole consistently, each with a comment
  tracing the failure.
- **Colour and geometry doors with theme sweeps.** `listrow::row_paint`,
  `control::paint`, `chip::chip_paint` and `dialog::badge`/`swatch` are the only colour
  sources in the dialog files, and each has a readability/distinctness sweep over every
  bundled theme with no exception list (`listrow.rs:103-230`,
  `control.rs:421-590`, `asof_view.rs:445`). Geometry goes through `scale::design`
  almost everywhere.
- **Footer honesty.** The Escape hint names the *rung it will actually take*
  ("clear the filter" vs "close" vs "back to {name}") in browse, edit, column and
  Values (`render.rs:3053-3061`, `:3789-3800`, `:3925-3935`), and hints are filtered by
  the selected row's `RowVocabulary` so no key is advertised that the row would refuse.
  `hint_rows` keeps empty rows at full height (`dialog.rs:678-687`) so the footer never
  reflows.
- **Failure semantics are documented where they bite.** `apply.rs`'s module and function
  docs state plainly that this is not a transaction across files, that the baseline
  advances on queue rather than on disk, that an old completion cannot clobber a new
  batch (the `seq` check at `:539-545`), and that there is no shutdown flush — matching
  `configuration-dialogs.md` rather than overstating the guarantee.
- **`open_shell_dialog` is a real door.** All nine `open` functions guard on
  `modal.is_some()` and route through it; it cancels the matcher, closes the palette,
  cancels the command line, records focus-return, clears the shared input and calls
  `window.prevent_default()` (`dialog.rs:150-197`) — the last being the fix for
  mouse-opened dialogs being deaf to typing.

---

## Confidence and limits

All findings above were verified by reading the cited code in this session. Three are
explicitly marked UNVERIFIED with the test that would settle them: N5
(`leave_values_stage` cursor-stop bypass — needs a filtered-rows Values escape),
N10 (`apply_text_entry` indexing a stale `TextEntry.row` — needs a reseed while a plain
field is open), and N16 (whether `debug_selector` compiles out in release, which would
halve that finding).

Not covered by this pass, by scope: the ~30k lines of `shell/tests/` (another reviewer's
scope — test *names* were read to assess coverage, bodies were not), `ShellView::render`
and the palette, `tiling`/`keymap`/`session`, and the adapter internals of
`views.rs`/`scopes.rs`/`groupings.rs`/`sources.rs`/`colours.rs`/`schema.rs` beyond their
public shape and `Domain` wiring. No build, test, clippy or bench was run, per the brief.
