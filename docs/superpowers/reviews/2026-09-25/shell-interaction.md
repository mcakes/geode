# geode-shell interaction layer review

Scope: `crates/geode-shell/src/shell/mod.rs` and the ShellView-adjacent surfaces —
tiles and the `TileContent` contract, module factories, input routing and key
dispatch, focus and focus restoration, the per-tile command line, the palette,
frame state, notices, tooltips, frame pickers, chrome, popups, drag and drop,
the perf overlay, and theme/colour handling. Read-only; no build was run.

## Summary

1. The keyboard path is unusually disciplined: one `handle_key_down` ladder with
   documented owner precedence, one `dispatch` door, and a focus-restoration
   protocol (`pending_focus_restore` / `holds_shell_focus` /
   `occupant_insert_stack`) whose invariants are written down where they are enforced.
2. The real architectural debt is not in the shell: it is the flip-barrier and
   version-following protocol every module re-implements by hand
   (`differs_on_followed`, `staged`, `acted`, `barrier_wants`/`arrived`), which is
   the single largest correctness surface outside the shell's own tests.
3. Two correctness findings worth fixing: `frame::slot_clear` and the grouping
   picker can leave the "slot gone" notice unreported, and the `escape` branch of
   the scope field silently skips reverting when `filter_session_base` is `None`.
4. GPUI hygiene is good — four globals exactly as documented, no `chrono::Local`,
   no literal colours outside the colour doors, scratch buffers reused across
   frames. The remaining per-frame costs are `Vec` rebuilds in `render`
   (`context_stack`, `palette.filtered()`, `current_tiles`) and one per-key
   allocation in `insert_contexts`.
5. `ShellView` is at the size where it should split: 60+ fields, 12 of which are
   "one open dialog and its scroll handle", plus five baselines that exist only
   to answer "does this reload need a restart".

---

## Critical

None. Nothing found can corrupt data, show a plausible wrong number, or
deadlock the UI thread.

---

## Major

### M1. `frame::slot_clear` and `frame::slot_N` swallow a missing slot; only the picker reports it

`crates/geode-shell/src/shell/input.rs:206-230` (the `frame::slot_` and
`frame::slot_clear` branches) call `Frame::set_active_slot` and notify only when
it returned `true`. `crates/geode-shell/src/shell/choicedialog.rs:350-364`
(`commit`, `Pick::Slot`) does the same call but additionally re-checks
`f.slots().get(n).is_some()` and sets `shell.notice = Some(SLOT_GONE)` when the
slot has vanished. So the same logical command reports the failure from the
mouse/picker route and is silent from the keyboard route — a `ctrl+3` bound to a
slot that a config reload removed does nothing at all, with no status line.
`dispatch` also clears `self.notice` at its top
(`crates/geode-shell/src/shell/input.rs:149`), so even a notice set by an earlier
path is gone by the time the branch runs.

**Impact.** A keyboard grouping command that silently no-ops after a desk config
change; the trader cannot tell the binding from a dead key. Violates the
CLAUDE.md rule that one logical command has one owner method.

**Direction.** Move the existence re-check and notice into a
`ShellView::activate_slot(Option<u8>)` method and have both the `frame::slot_*`
branches and `choicedialog::commit` call it.

### M2. Escape in the scope field skips the revert when no session base was recorded

`crates/geode-shell/src/shell/input.rs:662-684`: the Escape branch is wholly
inside `if let Some(base) = self.filter_session_base.take()`. `filter_session_base`
is set only by the `InputEvent::Focus` subscription
(`crates/geode-shell/src/shell/mod.rs:1066-1070`) and is cleared by
`InputEvent::PressEnter` and `InputEvent::Blur`
(`crates/geode-shell/src/shell/mod.rs:1082-1092`). A chord dispatched from the
field runs `reflect_frame_text_into_focused_field`
(`crates/geode-shell/src/shell/input.rs:537-556`), which calls `set_value` — which
by the surrounding comments emits no `Change` event, so the base survives — but
`frame::scope_clear`, `scope_undo`, `scope_redo` and a saved-scope load all call
`Frame::set_scope`/`clear_scope`, which are *not* the in-session setters and push
their own undo entries while `scope_session` is still open. After such a chord the
recorded `base` no longer corresponds to the frame's undo stack, and Escape
restores text that is no longer an undo-coherent state. The narrower failure is
plainer: if `filter_session_base` is ever `None` while the field holds focus,
Escape neither reverts nor ends the session — it only moves focus
(`crates/geode-shell/src/shell/input.rs:685-686`), leaving `scope_session`
open on `Frame` with no owner. `Frame::end_scope_session`
(`crates/geode-shell/src/frame.rs:273-283`) is then only reached by the next
`Blur`, which has already happened.

**Impact.** A stuck scope session coalesces later unrelated edits into one undo
entry; the documented "Escape restores the entry text while still inside the
session" guarantee (docs/current/input-and-dialogs.md, "Scope text and stack
selection") does not hold on every path into that branch.

**Direction.** Move `self.frame.update(|f| f.end_scope_session())` out of the
`if let` so it always runs on Escape, and make the chord branch either refuse
frame-mutating scope actions while the session is open or re-seed
`filter_session_base` from the frame after the dispatch.

### M3. Every module re-implements the flip-barrier and version-following protocol

`crates/geode-shell/src/frame.rs:601-677` exposes `open_flip`, `barrier_wants`,
`arrived`, `sweep` and `FrameVersions::same_flip_identity` as raw primitives. Each
module then hand-writes the same state machine: `geode-blotter/src/tile.rs:600-606`
(`differs_on_followed`), `:585-590` (`follows_changed`), `:608-630`
(`on_frame_changed` with the `flip` compare and the self-arrive), `:673-681`
(`promote`), plus `acted`/`staged`/`in_flight` fields at `:176-217`.
`geode-marketdata` and `geode-timeseries` carry their own copies
(`differs_on_followed` present in both; `staged` appears 31 and 12 times
respectively), and the counts diverge — `barrier_wants` appears 5 times in
blotter, 3 in marketdata, 3 in timeseries, 1 in pricer, 2 in diagnostics, while
`arrived` appears 5/7/2/1/1. A protocol whose call sites differ per module by
that much is one whose invariants are not centrally enforced.

**Impact.** The failure mode is exactly the one the barrier exists to prevent —
one frame showing tiles evaluated under different global states — and it is
re-derivable per module. The blotter's own comments record two already-fixed bugs
in this area (the `acted`-stamped staging note at tile.rs:205-211, the MIN-3 fix
at :768). A new module is expected to rediscover all of it.

**Direction.** The shell should own a `FrameFollower` (or extend `TileContent`
with a follow declaration: which of scope/grouping/as-of this tile follows) that
owns `acted`, `staged`, the flip compare, the self-arrive, and `promote`, leaving
the module only `requery(scope, grouping, as_of)` and `apply(snapshot)`. This is
the single highest-leverage change in the whole boundary.

### M4. `TileContent::deliver` forces every module to write arms for deliveries it can never receive

`crates/geode-shell/src/module.rs:40-67` makes `Delivery` exhaustive on purpose,
and `ShellView::deliver` (`crates/geode-shell/src/shell/occupants.rs:62-108`)
matches on the variant with every keyed arm named — both well-reasoned. The cost
lands on modules: `geode-blotter/src/content.rs:125-133` and
`geode-diagnostics/src/lib.rs:100-107` each carry four or five empty arms
(`Delivery::Price(_) => {}`, `Series | SeriesFetched => {}`, `Upload(_) => {}`).
`geode-shell/src/module.rs:60-62` even documents that `Upload` is acted on by
exactly one module. The compile-time forcing is real, but it is being paid by
every module for every outcome type in the workspace, and an empty arm is not
evidence of a decision — it is indistinguishable from an oversight.

**Impact.** Adding a sixth outcome type edits five module crates to add `=> {}`.
The contract's stated benefit (a forced explicit decision) degrades into
boilerplate that reviewers stop reading.

**Direction.** Keep the exhaustive enum for the shell's routing, but give
`TileContent` narrower defaulted hooks (`fn deliver_query`, `fn deliver_price`,
…) with a default no-op and one non-defaulted method a module must implement to
opt into a kind. Alternatively have the factory declare which delivery kinds it
accepts, so the shell can refuse to route the rest and the module writes only its
own arms.

### M5. Restart-required detection depends on five hand-maintained baselines inside `ShellView`

`crates/geode-shell/src/shell/mod.rs:780-784` holds `sources_baseline`,
`datasets_baseline`, `egress_baseline` and `pricing_baseline`; they are seeded at
`:1518-1521` and compared in `apply_reload` at
`crates/geode-shell/src/shell/hot_reload.rs:187-201`. The list of
restart-requiring documents is therefore encoded in three places — the field set,
the seed, and the comparison array — and a new restart-requiring document
(`pricing` was already bolted on as a special case at hot_reload.rs:199-200,
because it is a key rather than a document) needs all three edited in step. A
missed edit is silent: the app keeps running against stale source paths with no
warning, which is the "ambiguity is never acceptable" line in PHILOSOPHY.md §3.

**Impact.** Silent staleness after a config edit; the failure is invisible until
a trader notices a source that never updates.

**Direction.** Move the restart-required document list into one `const` table
(name → reason) beside `reload::decide`, and derive both the baseline snapshot
and the comparison from it, so adding a document is one line.

---

## Minor

### N1. `notice` is `Option<&'static str>` and cannot carry a name

`crates/geode-shell/src/shell/mod.rs:829` (`notice: Option<&'static str>`),
rendered at `crates/geode-shell/src/shell/render.rs:1023` and
`crates/geode-shell/src/shell/status.rs:159-168`. The two producers are
`NOT_IN_A_STACK` (`crates/geode-shell/src/shell/input.rs:29`) and `SLOT_GONE`
(choicedialog). Because the type is `&'static str`, no notice can name the thing
it is about — "not in a stack" cannot say which tile, "slot gone" cannot say
which slot. The lifetime is also "until the next dispatch" only
(`crates/geode-shell/src/shell/input.rs:149`), with no time-based expiry, so a
notice raised and then never followed by a dispatch stays on the status bar
indefinitely.

**Direction.** `Option<SharedString>` with an `Instant` and a short expiry swept
by the existing 500 ms poll loop. One allocation per notice, not per frame.

### N2. `context_stack` allocates a `Vec<KeyContext>` on every key and several times per frame

`crates/geode-shell/src/shell/input.rs:35-50` returns a fresh `Vec` with up to
four `KeyContext` values. It is called from `is_palette_toggle`
(input.rs:55-62, which itself calls it once per palette-toggle test — i.e. once
per keystroke on several branches), from the matcher path (input.rs:800), from
`occupant_insert_stack` (`crates/geode-shell/src/shell/occupants.rs:636`), and
from `render` for which-key (`crates/geode-shell/src/shell/render.rs:1155`).
`handle_key_down`'s insert branch then calls `insert_contexts`
(input.rs:836-851), which in the bare-key case allocates a *second* `Vec` by
filtering and cloning. So a single character typed into a module's cell editor
allocates two `Vec<KeyContext>` plus the per-`KeyContext` string data.

**Impact.** Against the crate's own "hot paths are allocation-free" rule
(PHILOSOPHY.md §6, README "per-frame heap churn is a defect"). Not a frame-budget
risk at these sizes, but it is the kind of churn the codebase elsewhere goes out
of its way to avoid (see the `scratch_*` fields).

**Direction.** A reusable `scratch_contexts: Vec<KeyContext>` on `ShellView`
filled by an out-parameter `fill_context_stack(&mut Vec<_>)`, matching the
existing `fill_all_tiles`/`visible_tile_keys` pattern; `insert_contexts` can then
filter in place over a second scratch buffer.

### N3. `palette.filtered()` clones every match's index vector on every navigation key

`crates/geode-shell/src/palette.rs:401-407` returns
`Vec<(&PaletteItem, Vec<usize>)>` and clones each match's highlight indices.
`crates/geode-shell/src/shell/palette_ctl.rs:148` calls it purely to read
`.len()` — building and discarding the whole vector on every `j`/`k`/arrow press.
`rows()` (palette.rs:414-422) already returns borrowed slices and is the
allocation-free form.

**Direction.** Add `fn filtered_len(&self) -> usize` and use it at
palette_ctl.rs:148; consider whether `filtered()` has any remaining callers that
`rows()` cannot serve.

### N4. `take_dirty_session_write` serializes every occupant's state on every 500 ms tick

`crates/geode-shell/src/shell/session_io.rs:33-36`: the first thing it does is
`self.current_tiles(cx)`, which calls `TileContent::serialize` on every
non-placeholder occupant (`crates/geode-shell/src/shell/occupants.rs:27-45`),
building a `toml::Table` per tile — and only then compares against
`last_tiles_written` to decide whether anything is dirty. The poll runs every
500 ms forever (`crates/geode-shell/src/shell/hot_reload.rs:21`,
`crates/geode-shell/src/shell/mod.rs:1285-1290`). The comparison *needs* the
serialized form, so this is not trivially avoidable, but it means an idle app
with eight tiles allocates eight TOML tables twice a second on the UI thread.
docs/current/shell.md already documents that snapshot collection and
serialization run on the UI thread.

**Direction.** Let occupants report a cheap state generation counter (the modules
already track their own versions) so the serialize pass runs only when some
counter moved; keep the full compare as the fallback.

### N5. `perf` histogram is cloned out of the entity on every watched poll

`crates/geode-shell/src/shell/mod.rs:1320` (`view.perf.clone()`), inside the
500 ms loop, guarded by `watched` (diagnostics tile visible). The comment at
mod.rs:1305 acknowledges the ~176-byte copy. Fine as written; noting it because
the guard is "a diagnostics tile is visible", which for a trader watching
diagnostics is "always", making this a permanent 2 Hz copy. `FrameHistogram` is
`[u32; NUM_BUCKETS]` plus five scalars (`crates/geode-shell/src/perf.rs:72-87`),
so the cost is bounded and known; no action strictly needed.

### N6. `ActionRegistry::name_of_hash` is a linear scan that recomputes FNV per entry

`crates/geode-shell/src/actions.rs:57-63` iterates every registered action and
calls `fnv1a` on each id to find one hash — while `register`
(actions.rs:31-41) already maintains exactly that `HashMap<u64, String>` in
`self.hashes`. The method ignores the map it is next to.

**Direction.** Read `self.hashes` instead. It is used on the crash-report path
(`ActionTail`), so the cost is not hot, but the duplication is a trap: the map
and the scan can disagree.

### N7. `whichkey::continuations` sorts by a freshly formatted key string

`crates/geode-shell/src/shell/whichkey.rs:47-53`: `result.sort_by_key(|(key, _)|
render_keystroke(key))` allocates a `String` per comparison key, and this runs
during `render` whenever a sequence is pending
(`crates/geode-shell/src/shell/render.rs:1154-1156`) — i.e. on every frame while
the user holds a prefix. `render_keystroke` (palette.rs:459-468) builds a `Vec`
of parts and joins it.

**Direction.** `sort_by_cached_key`, or derive an `Ord` on `Keystroke` that
matches the intended display order.

### N8. Comments cite task numbers and spec sections instead of the invariant

CLAUDE.md: "A code comment should state the local invariant and failure it
prevents; it should not require a task number or spec section to make sense."
Counter-examples are pervasive in the reviewed files:
`crates/geode-shell/src/shell/mod.rs:79` ("spec §3, §8"), `:296` ("spec §3.3"),
`crates/geode-shell/src/shell/occupants.rs:147` ("Phase 4b M8"), `:211`
("Phase 4b Task 5 fix round 1, MAJ-2"), `:240` ("I2, final review"),
`crates/geode-shell/src/shell/render.rs:63` ("spec §7.4"), `:466` ("tile-drag
task"), `:1145` ("M10 (3b final review)"), `crates/geode-shell/src/shell/drag.rs`
("finding 7"), `crates/geode-shell/src/shell/toolbar.rs:205` ("fix round 1,
Finding 2"). Most of these comments *also* state the invariant clearly — the
citation is additive, not a substitute — but several are review-round
archaeology ("fix round 1, Minor 5" at occupants.rs:455) that will be
meaningless to the next reader.

**Direction.** On the next touch of each file, keep the invariant sentence and
drop the round/finding identifier. The archive (docs/phase-history.md) is where
the chronology belongs, per the same CLAUDE.md section.

### N9. `dispatch` is a 200-line `else if` ladder on string action ids

`crates/geode-shell/src/shell/input.rs:84-351`: after three early-return special
cases (`stack::next`/`prev`/`unstack`/`pick`), the body is ~35 sequential
`else if action.0 == "…"` comparisons plus four `strip_prefix` branches. The
ordering is load-bearing and documented as such in one place —
`scope::save_current` must precede the generic `scope::` prefix match
(input.rs:294-297) — which is precisely the fragility a ladder invites: the
constraint is invisible unless you read that comment, and a new `scope::`-prefixed
action id added above it would be silently shadowed. String comparison per
branch also means the worst case walks every arm.

**Direction.** A `match action.0.as_str()` gives the compiler a jump table and
makes duplicate arms a compile error; the prefix cases become a small `parse`
helper called once before the match. Grouping the frame branches into
`dispatch_frame` and the config branches into `dispatch_config` would also let
`dispatch` fit on a screen.

### N10. `ShellView` holds twelve fields that are "the one open dialog, and its scroll handle"

`crates/geode-shell/src/shell/mod.rs:459-473` and `:787-801`: `modal`,
`keybindings` + `keybindings_scroll`, `settings` + `settings_scroll`,
`palette` + `palette_scroll`, `picker` + `picker_scroll`, `as_of_dialog` +
`as_of_scroll`, `scope_expr_dialog`, `choice_dialog` + `choice_dialog_scroll`,
`object_dialog` + `object_dialog_scroll`. `close_modal`
(`crates/geode-shell/src/shell/mod.rs:1695-1710`) must remember to clear seven of
them by hand, and `sync_dialog_text`
(`crates/geode-shell/src/shell/dialog.rs:214-250`) and the `dialog_input`
subscription (`crates/geode-shell/src/shell/mod.rs:1130-1176`) each contain an
`if let … else if let …` chain over the same seven states, in a hand-maintained
order. Three parallel chains that must agree on which states are mutually
exclusive.

**Impact.** Adding a dialog kind means editing `close_modal`, `sync_dialog_text`
and the input subscription, and the compiler checks none of it — a missed arm is
a dialog whose typing goes nowhere. The memory index records this class of bug
having shipped before (scopes-dialog handoff: "a new projection stage touches
four Edit|Column matches").

**Direction.** One `enum ActiveDialog { Settings(…), Keybindings(…), … }` with a
scroll handle per variant, so `close_modal` is `self.dialog = None` and the two
chains become exhaustive `match`es the compiler completes.

### N11. Two focus backstops plus a flag, with the relationship only in prose

`crates/geode-shell/src/shell/render.rs:110-115` consumes
`pending_focus_restore`; `:145` is the `window.focused(cx).is_none()` net;
`crates/geode-shell/src/shell/occupants.rs:530-556` is the departed-tile backstop
that takes focus directly because the flag would be a frame late. The three are
correct and each carries a long comment explaining why it is not redundant with
the others — but the reasoning lives only in those comments, and the flag has
eight writers (`crates/geode-shell/src/shell/drag.rs:409`, `:474`,
`crates/geode-shell/src/shell/render.rs:721`, `:741`, `:805`, `:823`,
`crates/geode-shell/src/shell/occupants.rs:554`,
`crates/geode-shell/src/shell/hot_reload.rs:275`).

**Direction.** No behaviour change proposed — the mechanism has been debugged
into shape. Worth a short table in docs/current/shell.md's Focus section naming
each of the three mechanisms, the failure it covers, and why the other two do
not cover it, so the next reader does not have to reconstruct it from three
comment blocks.

### N12. Right-press tile focus duplicates the left-press handler instead of sharing it

`crates/geode-shell/src/shell/render.rs:700-745` (main tree) and `:775-830`
(docks): each tile gets a `MouseButton::Left` handler and a
`MouseButton::Right` handler whose bodies are the same four statements
(`leave_command_line`, `focus_*_tile` + `session_dirty`,
`pending_focus_restore = true`, `notify`), the right one simply omitting the
double-click/drag gesture attempts. Four copies of that body across the two
loops.

**Direction.** One `fn focus_tile_from_pointer(&mut self, id, region, window,
cx)` called by all four, with the gesture attempts staying in the left handlers.
The documented rule ("a right press focuses exactly as a left one does",
docs/current/shell.md) would then be true by construction rather than by
four-way copy discipline.

### N13. Pointer callbacks each clone `cx.entity()` into their own closure

`crates/geode-shell/src/shell/render.rs:1011-1101`: eight separate
`let x_entity = cx.entity();` bindings (`diagnostics_click_entity`,
`chip_close_entity`, `chip_open_entity`, `pick_chip_entity`, `save_chip_entity`,
`grouping_entity`, `as_of_entity`, `expr_entity`), each moved into one closure,
all rebuilt every frame. An `Entity` clone is a refcount bump, so the cost is
small, but eight near-identical five-line blocks in `render` is noise, and the
`weak.update`/`entity.update` split with the `cx.listener` form used elsewhere in
the same function is inconsistent.

**Direction.** A small helper — `fn shell_action(cx, f: impl Fn(&mut ShellView,
&mut Window, &mut Context<ShellView>))` returning the closure — collapses all
eight to one line each.

### N14. The as-of dialog refresh reads the wall clock during a frame-change callback

`crates/geode-shell/src/shell/mod.rs:1799`:
`state.refresh(&as_of, &publishes, chrono::Utc::now())` inside
`on_frame_changed`. Every other displayed-time path routes through
`geode_core::clock::Clock` via the `AppClock` global (`clock()` at
`crates/geode-shell/src/shell/mod.rs:1956`), and `asof_rows` deliberately
captures its clock at open time and retains it across refreshes
(documented in docs/current/input-and-dialogs.md, "As-of selector":
"The clock is captured at open and is retained across refreshes"). The bare
`Utc::now()` here is the *current instant* rather than a clock read, which is
consistent with that contract (the retained clock does the interpreting) — but it
is the one place in the file where "now" does not come from the clock, and
`asof_view::open` at `crates/geode-shell/src/shell/asof_view.rs:55` does the same.
Flagging as UNVERIFIED-as-a-bug: I could not find a path where this produces a
wrong display, because the retained `Clock` is what formats. Confirming it would
mean checking whether `AsOfState::refresh` uses the instant for anything but
"which presets are in the past".

**Direction.** If it is only an instant, rename the parameter to `now_utc` and
say in the doc comment that it is deliberately not a clock read; otherwise route
it through `self.clock(cx)`.

### N15. `overlay_return_to_filter` is a single bool shared by two overlay kinds

`crates/geode-shell/src/shell/mod.rs:692`, written by
`crates/geode-shell/src/shell/palette_ctl.rs:53` (palette open) and
`crates/geode-shell/src/shell/dialog.rs:173` (any shell dialog open), consumed by
`return_focus_from_overlay` (`crates/geode-shell/src/shell/mod.rs:1712-1726`)
with `std::mem::take`. Because it is one flag, a dialog opened *from* the palette
overwrites the palette's recorded answer — which is in fact handled, since
`open_shell_dialog_with_key` calls `close_palette` *before* recording
(dialog.rs:160-173, with a comment saying exactly that). The ordering is
load-bearing and correct; the risk is that it is implicit in a call sequence
rather than a type. A third overlay kind that opens over the palette without
closing it first would silently lose the caret's home.

**Direction.** Either a small `focus_return: Option<FocusReturn>` enum, or a
comment on the field itself (not only at the two write sites) naming the
invariant "recorded after every competing overlay has closed".

### N16. `divider_drag` cancel-on-hidden-surface fires from `render`, mutating state

`crates/geode-shell/src/shell/render.rs:178-215` (divider) and `:213-250`
(tile drag) both mutate `self` during `render` — `cancel_divider_drag()` sets
`session_dirty` (`crates/geode-shell/src/shell/drag.rs:240`). The same is true of
the stack-list staleness check (render.rs:148-170), the command-line invariant
check (`:251-286`), and the focus-restore consumption (`:110-115`). Each carries a
comment justifying "render is the one place every path funnels through with a
`Window` in hand", which is a real constraint. But the GPUI guide's rule is
explicit: "Do not mutate state or notify unconditionally in `render`" and
"business logic … embedded in a long `render` method" is listed as a failure
mode. These mutations are conditional and do not notify, so they cannot loop —
the letter of the loop hazard is respected.

**Impact.** Correctness is fine today. The cost is that `render` is now the
de-facto reconciliation tick for six unrelated concerns, which is why it is 1697
lines and why a reader cannot tell what `render` is responsible for.

**Direction.** Extract the six checks into one
`fn reconcile_transient_state(&mut self, window, cx)` called as the first line of
`render`, so `render` itself becomes description-only and the reconciliation has
a name, a doc comment, and a place for the seventh check to go.

### N17. `ensure_occupants` sends `set_visible(true)` twice to a newly created active tile

`crates/geode-shell/src/shell/occupants.rs:330-335` calls
`occupant.content.set_visible(active.contains(id), cx)` at creation, and the
diff loop at `:365-370` sends it again because the tile is also new to
`self.visible_tiles`. The comment at `:229-238` acknowledges this ("told `true`
twice … harmless, and simpler"). It *is* harmless for the current implementors —
the blotter's `set_visible` (`geode-blotter/src/tile.rs:853-861`) is idempotent
because the second call finds `follows_changed` false. But the contract in
`crates/geode-shell/src/module.rs:247-254` does not promise idempotence, and a
module that treated `set_visible(true)` as "open a subscription" without checking
would double-subscribe.

**Direction.** Either state idempotence as a requirement in the `set_visible` doc
comment, or seed `self.visible_tiles` with the ids the creation loop already
announced so the diff loop skips them.

### N18. `holds_shell_focus` enumerates four inputs by hand

`crates/geode-shell/src/shell/occupants.rs:641-652` lists `palette_input`,
`dialog_input`, `command_input`, `filter_input`. The comment at occupants.rs:250
says "If `ShellView` ever gains another focusable field, it belongs in
`holds_shell_focus` below" — an instruction to a future reader that the compiler
cannot enforce. A fifth input added without that edit makes the departed-tile
backstop yank the caret out of it mid-type, and makes
`note_keyboard_focus_move` arm a restore it should not.

**Direction.** Group the four into one `inputs: ShellInputs` struct with an
`iter()`, so adding a field to that struct extends the predicate automatically.

---

## Ideas (assessed against the current design)

### I1. Separate windows (TODO.md) — the largest structural ask

`ShellView` is documented as "the retained GPUI entity for one window"
(docs/current/shell.md, State ownership) and mostly honours that. The obstacles
are the globals and the singleton services: `UiSettings`, `Chords`, `AppClock`,
`SeriesSettings` are `cx.set_global` (`crates/geode-shell/src/shell/mod.rs:1445`,
`:1453`, `:1457`, `:1461`) and therefore process-wide, which is *correct* for a
second window (both windows should share the rem scale and the clock). The real
blockers are: `session_path` and the session writer assume one window's
`Workspaces` is the whole session (`crates/geode-shell/src/shell/session_io.rs:33-68`
serializes `self.services.workspaces` as the document); the hot-reload poll loop
is per-`ShellView` (`crates/geode-shell/src/shell/mod.rs:1285-1440`), so two
windows would scan the config directory twice and both call `apply_reload`; and
`ThemeService` is owned per-`ShellView` (`services.theme`) while
`Theme::global_mut` (`crates/geode-shell/src/theme.rs:357-364`) is process-wide,
so two windows cannot disagree about the theme but each thinks it owns it.

**Assessment.** Feasible, and the `Frame`/`Diagnostics`-as-entities design already
supports sharing. Sequence: (1) lift the reload poll and `ThemeService` into an
app-level service owned by `geode-app`, leaving `ShellView` an observer;
(2) extend the session format to a list of windows; (3) only then add the window.
Doing it in the other order would duplicate the reload work per window.

### I2. Shared key bindings (TODO.md)

The keymap is already layered (Builtin → desk → user) and
`reload::scan` reads desk and user directories
(`crates/geode-shell/src/reload.rs:36-54`), so a desk-level `keymap.toml` is
*already* the sharing mechanism — PHILOSOPHY.md §5 is satisfied. If the ask is
"share my bindings with the desk from inside the app", that is a
`config_write`-to-desk-directory operation, and every current write is
user-layer-only by rule (CLAUDE.md: "Runtime config writes target only the user
layer"). That rule exists for a reason and should not be relaxed casually; the
right shape is an explicit export ("copy my overrides to a file I can hand
over"), not a desk-layer write.

### I3. Mod-key hint helper (TODO.md: "show hints when holding down mod key")

`whichkey` already does this for *sequences*
(`crates/geode-shell/src/shell/whichkey.rs:8-54`, gated on
`!self.matcher.pending().is_empty()` at
`crates/geode-shell/src/shell/render.rs:1153-1156`). The ask is the same panel
keyed on a held modifier rather than a pending prefix. GPUI delivers bare
modifier presses as `ModifiersChangedEvent`
(noted in `crates/geode-shell/src/shell/keys.rs:17-20`, where
`convert_keystroke` deliberately returns `None` for them), so the input already
exists and is currently discarded. The filter would be
"single-keystroke bindings whose `mods` match the held set, evaluated against
`context_stack`" — which is exactly `single_keystroke_binding`
(`crates/geode-shell/src/shell/input.rs:65-77`) generalised from one keystroke to
all matching. Cheap and well-supported; the only design question is the reveal
delay, and the render gating must reuse `whichkey`'s existing suppression of
drags (`crates/geode-shell/src/shell/render.rs:218-222` cancels a tile drag when
a hint appears) so a held modifier does not cancel an in-flight drag.

### I4. Palette word-order matching ("scope clear" should match "Clear scope")

`crates/geode-shell/src/palette.rs:88-155` is a single-sequence fuzzy matcher: it
requires the query's characters to appear in order in the candidate
(`if c[j] == q[i] && j >= i`), so "scope clear" cannot match "Clear scope" —
confirmed by construction, and the TODO's report is accurate. The scoring already
has the machinery the fix needs: `char_base` gives a `WORD_START_BONUS` of 8 after
` :_-` (palette.rs:207-217), `RUN_BONUS` is 9, and `discounted`
(palette.rs:195-205) halves anything past `title_len` so category matches weigh
half.

**Assessment.** The clean change is to split the query on whitespace and require
each term to match independently (each still in-order within itself), summing
scores, plus a bonus when the terms' matched positions are themselves in
ascending order — which is precisely the TODO's "right order increases score".
Note two current behaviours that must be preserved: palette queries are
deliberately *not* trimmed (documented in docs/current/input-and-dialogs.md,
Palette and which-key: "palette queries are not trimmed to turn whitespace-only
input into an empty filter"), so a trailing space must not become an empty term
that matches everything; and `highlight_runs` (palette.rs:187-230) consumes a
single ascending `indices` slice, so multi-term matching must merge and sort
indices before handing them over or the highlight will be wrong. `recompute_filtered`
already runs once per query edit, not per row read
(guarded by the `match_calls` counter test at palette.rs:1184), so the extra cost
is bounded by term count.

### I5. Autosize columns for all tiles (TODO.md)

This is not a shell concern today: column widths live in each module's table
state, and the shell's only column vocabulary is `Pickable`
(`crates/geode-shell/src/shell/mod.rs:296-316`) for the dimension picker. Making
it uniform across tiles means either a shared `geode-shell` table wrapper (a new
app-component layer, which the GPUI guide's layering would permit) or the same
`:` command word implemented per module — and the latter is what the
`TileContent::command` contract already expects, with a per-module sweep test.
Given PHILOSOPHY.md §4 ("a module never invents its own navigation idiom"), the
shared wrapper is the right answer, but it is a real new surface, not a small
change. The abs-sort handoff already records "width reset on refresh" as deferred,
which suggests the module-side width story needs settling first.

---

## Systemic patterns

**Prose carries invariants the type system could.** The strongest and weakest
feature of this code is the same thing: comments do an extraordinary amount of
load-bearing work. `occupants.rs:224-256` explains the focus backstop better than
most codebases document anything. But `holds_shell_focus`'s "if ShellView gains
another focusable field, add it here" (N18), `close_modal`'s seven hand-cleared
fields (N10), the restart baselines (M5), `overlay_return_to_filter`'s ordering
(N15) and `dispatch`'s "this branch must precede that one" (N9) are all
invariants a struct or an exhaustive `match` would enforce for free. Every one of
them is currently a note to a future reader.

**One logical command, two implementations.** The CLAUDE.md rule is mostly
honoured — pointer handlers route to `dispatch` or to the same `open` function —
but it leaks at the edges where the mouse path grew a refinement the keyboard
path lacks (M1's slot notice) or where two handlers were copied rather than
shared (N12's right-press). The pattern to watch: when a pointer route needs
extra behaviour, it gets added inline instead of being pushed down into the shared
method.

**`render` as the reconciliation tick.** Six unrelated "check this at the top of
render because it is the only place with a Window" concerns (N16). Each
justification is individually sound; collectively they are why `render.rs` is
1697 lines and why the GPUI guide's "no logic in render" rule reads as violated
even though no loop hazard exists.

**The module boundary pushes protocol, not policy, to modules.** M3 and M4
are the same shape: the shell exposes correct primitives (`barrier_wants`,
`Delivery`) and leaves each module to assemble the protocol. Five modules have
now assembled the flip protocol five times with diverging call counts. The shell
should own the state machine and leave modules the domain decision.

**Tests concentrate in window-level files.** `shell/tests/` is 30k lines across
23 files, with `objectdialog.rs` alone at 9436. The pure cores are well covered
(palette 51 tests, frame 27, listfilter 10, whichkey 14, keys 9), but the
interaction modules I reviewed have zero in-file tests: `shell/input.rs`,
`shell/occupants.rs`, `shell/drag.rs`, `shell/palette_ctl.rs`,
`shell/commandline_ctl.rs` all rely entirely on the window-level files. That is
the right layer for focus and key routing (per the README's own rule), but it
means the pure-decidable parts — `insert_contexts`' filter, `dispatch`'s branch
ordering, `holds_shell_focus`' membership — have no cheap test and are only
covered incidentally.

---

## What is done well

- **Key-ownership precedence is explicit, ordered, and documented.**
  `handle_key_down` (`crates/geode-shell/src/shell/input.rs:558-834`) checks
  owners in one place, in one order, and that order matches the table in
  docs/current/input-and-dialogs.md exactly. The chord-vs-typing rule (Control /
  Alt / Command is a chord, Shift alone is typing) is applied uniformly at every
  branch, and each branch says why it stops propagation.
- **The focus protocol has been debugged into a real design.** Three
  complementary mechanisms with non-overlapping responsibilities, each with the
  gpui internals it depends on named (`focus_node_id_in_rendered_frame`,
  `FocusHandle::for_id`), and the one predicate behind two doors
  (`occupant_insert_stack`) explicitly marked as "must not drift" with the
  review finding that proved the ownership half load-bearing.
- **Scratch buffers instead of per-frame allocation.** `scratch_all_tiles`,
  `scratch_active_tiles`, `scratch_visible_keys` with out-parameter fills
  (`crates/geode-shell/src/shell/occupants.rs:111-190`), and the
  `take`/restore-around-the-borrow idiom. The comments record that this replaced
  two fresh `HashSet`s per render.
- **Exactly four globals, exactly as documented.** `grep 'impl Global'` across
  the whole workspace returns four: `SeriesSettings`, `UiSettings`, `AppClock`,
  `Chords`. Modules read them with `try_global`, the shell with `global`, and
  `hot_reload` republishes only on change (`crates/geode-shell/src/shell/hot_reload.rs:238-256`)
  specifically to avoid waking every observer on every poll.
- **No literal colours or `chrono::Local`.** Every colour goes through
  `chip_paint` / `row_paint` / `control::paint` / `colours::to_rgb`, each with a
  WCAG sweep over every bundled theme; the only raw `hsla` is the deliberate
  neutral-black overlay shadow (`crates/geode-shell/src/shell/dialog.rs:251-256`).
  `chrono::Local` appears nowhere in the crate. Chrome geometry goes through
  `scale::design`/`design_px`, and radii come from the theme's radius tokens.
- **Stable domain-derived element IDs.** `ElementId::NamedInteger` keyed on the
  workspace number (`crates/geode-shell/src/shell/sidebar.rs:159`, `:191`), chip
  index within a per-render model (`crates/geode-shell/src/shell/toolbar.rs:232`),
  and `debug_selector`s keyed on the *column name* rather than position
  (`toolbar.rs:237`, `:268`) — so a test and a tooltip both address the chip by
  what it is.
- **The occluding `×` inside the chip.** `crates/geode-shell/src/shell/toolbar.rs:255-285`
  uses `occlude()` alone — no `stop_propagation` — so the close glyph's hitbox
  makes the body's hitbox un-hovered, and the comment explains that this is the
  whole mechanism. Correct use of the framework instead of fighting it.
- **`Delivery` as an exhaustive enum with every keyed variant named** in
  `ShellView::deliver` (`crates/geode-shell/src/shell/occupants.rs:71-107`), so a
  new outcome type cannot be silently dropped by a wildcard — even though the
  cost lands on modules (M4), the shell side of the decision is right.
- **The command line's tile capture and staleness backstop.** The owner tile is
  captured at open (`crates/geode-shell/src/shell/commandline_ctl.rs:26-31`),
  every commit routes back to that captured tile, `"the tile is gone"` is an
  explicit error rather than a panic (commandline_ctl.rs:166), and the
  generic invariant check at `crates/geode-shell/src/shell/render.rs:251-286`
  covers the two surfaces that can break the invariant without going through a
  tile mouse-down. `leave_command_line` distinguishes commit-on-blur for `/`
  from cancel-on-blur for `:` (commandline_ctl.rs:73-85) — the right semantics
  for each.
- **Byte-cursor handling in `word_at` is correct on UTF-8.** `commandline.rs:88-108`
  clamps to `line.len()`, walks back to a `char_boundary`, and uses
  `char_indices` on both sides — no panic reachable from a multibyte `:` line.
  The UTF-8 double-encoding incident in the memory index makes this worth noting.
