# Review: `geode-marketdata` (+ `geode-documents`, egress path)

Reviewer pass status: all six passes (correctness, GPUI/perf, architecture, clarity,
tests, ideas) **completed**. Every finding below was read in the source and carries
file:line. Nothing was built or run (read-only brief).

## (a) Summary

1. The crate is in much better shape than its headline size suggests: `tile.rs` is
   14,729 lines but only **5,405 of those are production code** — the `#[cfg(test)]`
   module at `crates/geode-marketdata/src/tile.rs:5406` runs to the end of the file
   and holds **303 test functions**, almost all driven through the real
   `TileContent` trait rather than internal mutators.
2. The draft state machine (`Editing`/`Behind`/`Sent`, park-per-underlying, rebase
   guard on same-day groups, echo compare as a multiset) is unusually carefully
   built, and `apply_snapshot` decides everything on a *copy* of the draft before
   committing a field — the single best design decision in the crate.
3. The one Critical is a disclosed-but-open hole: a republish that keeps its source
   time is treated as the same generation, so index-keyed edits stay put while the
   grid is rebuilt underneath them — a wrong number on screen that `:upload` will
   then send upstream.
4. Performance: two real hot-path defects — `serialize()` does a full
   `MatrixModel::build` on the shell's 500 ms session-dirty tick (8.18 ms at 10k
   rows, twice a second), and `/` rebuilds a whole-document text index per
   keystroke. Neither is in `render`, which is genuinely clean.
5. Philosophy: no financial reasoning anywhere. The CVI slice values are read,
   painted and written back; disagreement within a slice is *refused*, never
   averaged; both `KindAction`s answer "not built yet". `:bump`/nudge are data
   entry at a painted precision. This is compliant and deliberate.

### `tile.rs` table of contents (production half, lines 1–5405)

| Lines | Region | Contents |
|---|---|---|
| 1–52 | Module doc | body-is-a-table ruling, cursor-is-truth, `install_model` invariant, draft states |
| 53–99 | Imports | |
| 100–118 | Constants / find | `HALF_PAGE`, `FULL_PAGE`, `FindState` |
| 120–202 | `FlooredTones` | 6-colour theme memo, `derive`/`key`/`refresh` |
| 204–355 | Refusal strings + upload state | `NO_DOCUMENT`, `CELL_MOVED`, `BEHIND_REFUSED`, `ECHO_REFUSED`, `DELETED_REFUSED`, `UPLOAD_*`, `PendingUpload`, `InFlightUpload`, `Echo`, `EchoStep`, `NOT_BEHIND`, `REBASE_AWAITING_ECHO`, `NOT_A_ROW`, `ALREADY_DELETED` |
| 356–434 | Editor types | `Editing`, `EditorState` (Text/Date), `DateFieldPaint`, `EditorPaint` |
| 435–494 | Edit targets | `EditTarget::{Cell,Attr,RowLabel}`, `AttrInput`, `Yank` |
| 495–711 | `struct MarketDataTile` | 40 fields, each with a doc comment |
| 714–1053 | `new` | session restore (`underlying`/`drafts`/`draft`/`auto`), `TableState` construction, 4 subscriptions (table events, frame, diagnostics, AppClock) |
| 1054–1092 | Key ownership | `key_context` (insert/menu/normal), `holds_focus` |
| 1094–1223 | Request lane | `versions`, `follows_changed`, `differs_on_followed`, `self_arrive`, `arrive`, `arrive_and_release`, `requery` |
| 1224–1363 | Delivery entry | `deliver` (stale tag, barrier stage, Err), `deliver_upload` |
| 1364–1593 | `:upload` | `arm_upload`, `confirm_key`, `cancel_upload_on_pointer`, `disarm_upload`, `cancel_upload`, `not_live`, `submit_upload` |
| 1594–1643 | Apply + confirm withdrawal | `apply`, `withdraw_upload_if_moved` |
| 1644–1923 | **Delivery merge** | `apply_snapshot`: draft copy, `on_delivered`, echo, `:auto` policy, base retention, build, restore-rebase, `install_model` |
| 1924–1992 | Echo | `echo_of` → `EchoStep` |
| 1993–2052 | Lifecycle | `promote`, `set_visible`, `set_stack`, `title`, `compute_title` |
| 2053–2137 | Model plumbing | `painted_snapshot`, `capture_groups_if_base`, `rebuild_model`, `install_model` |
| 2138–2263 | Cursor/mirror | `grid`, `clamp_cursor`, `sync_cursor`, `delegate_editor`, `delegate_choice`, `sync_editor` |
| 2264–2331 | Mouse | `cursor_to`, `cursor_to_attr`, `attr_clicked` |
| 2332–2401 | Chrome | `changed`, `rebuild_chrome`, `is_stale` |
| 2402–2710 | **Key dispatch** | `dispatch`: one 308-line match on the verb string |
| 2711–2771 | Edit gates | `held_refusal`, `edit_base`, `attr_edit_base` |
| 2772–2962 | Editor open | `begin_edit`, `date_field_key`, `date_segment_clicked` |
| 2963–3113 | **Commit** | `commit_edit` (6 arms), `commit_row_label` |
| 3114–3209 | `nudge` | arrow stepping of the open editor |
| 3210–3402 | **Cell write** | `commit_cell_edit`, `column_required`, `commit_cell_value` (patch path) |
| 3403–3516 | Attr write + close | `commit_attr_edit`, `close_editor` (blur-then-drop) |
| 3517–3693 | Row verbs | `row_verb_target`, `insert_row`, `begin_label_edit`, `delete_row` |
| 3694–3840 | Action list | `toggle_menu`, `close_popup_with_window`, `menu_hover`, `picker_hover`, `menu_pick` |
| 3841–3937 | Picker | `open_picker`, `commit_picker`, `picker_pick` |
| 3938–4120 | Choice cell | `open_choice`, `commit_choice`, `choice_pick`, `pick_option`, `choice_hover`, `step_choice` |
| 4121–4390 | Draft verbs | `revert`, `leave_behind`, `bump` (170 lines), `rebase` |
| 4391–4570 | Find/yank | `cursor_row`, `set_cursor_row`, `yank_text`, `row_labels`, `find`, `repeat_find` |
| 4571–4740 | `:` line | `command`, `set_policy`, `policy`, `set_attr_command` |
| 4741–4914 | Key switch + catalog | `set_key`, `parked_marks`, `completions`, `catalog_keys`, `needs_catalog`, `request_catalog{,_if_needed}` |
| 4915–4977 | Session | `serialize` |
| 4978–5200 | Test accessors | 24 `#[cfg(test)]` readers |
| 5201–5303 | Free functions | `display_key`, `parse_display_key`, `same_edits`, `as_of_text`, `source_time_of`, `dropped_notice`, `declared_type` |
| 5304–5405 | `Render` | tones refresh, staleness, header + popup + `DataTable`, confirm-cancel capture |
| 5406–14729 | `#[cfg(test)] mod tests` | 303 tests, harness at 5520–6200 |

---

## (b) Findings

### Critical

**C1. A republish that keeps its source time re-points every index-keyed edit onto a
rebuilt grid, silently.**
`crates/geode-marketdata/src/core/draft.rs:585-633` (`on_delivered`),
`crates/geode-marketdata/src/tile.rs:1644-1923` (`apply_snapshot`),
`crates/geode-marketdata/src/core/matrix.rs:72-99` (`Cell::cell_ref`),
`crates/geode-marketdata/src/core/matrix.rs:1127-1152` (`cell_of`).

A generation's identity is its source time alone. `on_delivered` returns `false`
when the delivered `as_of` equals `draft.base`, so the draft stays `Editing` — but
`apply_snapshot` unconditionally rebuilds the model from the *new* snapshot
(`MatrixModel::build`, line ~1782) and installs it. `Draft::edits` is keyed by
`(document row, model column)`, and under `Columns::Axis` the model's column order
is the **document's own node order** (`matrix.rs:637` `pivot`, first-appearance
order, deliberately unsorted). So a republish under the same source time that
reorders or adds a node re-points every edit onto a different node, paints it there
(`cell_of` looks up by `cell_ref`), and `:upload` will assemble and send it. The
draft's `labels` side map exists and would detect this, but is consulted only by
`rebase`. `draft.rs:596-606` documents the hazard ("a `gen_id` in the provenance is
the fix") and notes it is unreachable under `--demo` (`source_time = "receive"`),
but reachable for a source configured `source_time = "document"`.

*Impact:* a wrong number on a trading screen, indistinguishable from a right one,
that the trader can then upload upstream — the exact failure CLAUDE.md ranks above
an explicit error.
*Direction:* the model already knows every edited cell's recorded labels. On any
delivery whose source time equals `draft.base`, compare the built model's
`label_of(cell)` against `draft.labels` for each edit; on any mismatch treat the
delivery as a new generation (`Behind`, with a notice naming it) rather than as a
redelivery. That is a pure check in `core`, testable without a window, and it closes
the gap without waiting for a `gen_id`.

### Major

**M1. `serialize()` runs a full `MatrixModel::build` on the shell's 500 ms session
tick.**
`crates/geode-marketdata/src/tile.rs:4915-4977` (`serialize` →
`capture_groups_if_base`), `tile.rs:2080-2093` (`capture_groups_if_base` →
`MatrixModel::build`), `crates/geode-shell/src/shell/session_io.rs:34`
(`take_dirty_session_write` → `current_tiles`),
`crates/geode-shell/src/shell/occupants.rs:37` (`content.serialize(cx)`),
`crates/geode-shell/src/shell/hot_reload.rs:30` (`RELOAD_POLL_INTERVAL = 500ms`).

The shell cannot know whether module state changed, so it calls `serialize()` on
every occupant every 500 ms purely to *compare* the result. This module's
`serialize` does a clone of the draft plus `capture_groups_if_base`, which builds a
whole clean `MatrixModel` of the base document. `docs/current/performance.md`
records that build at **8.18 ms for a 10,000-row schedule**, i.e. at the pure-UI
budget. So a dividend panel with one unsent edit spends ~8 ms of UI thread twice a
second, forever, to recompute same-day group sizes that only change when the
document changes.

`capture_groups_if_base` returns early unless the painted snapshot *is* the draft's
base — which is the ordinary case (a dirty draft on the live document), so the build
happens whenever it matters. The same tick also clones every parked draft's table
(`serialize`, `drafts.insert(display_key(key), table.clone())`).

*Impact:* a sustained frame-budget hazard on exactly the panel shape the perf guide
flags, invisible because it is not in `render`.
*Direction:* capture the group sizes when the model is installed (the tile already
knows at `install_model`/`apply_snapshot` whether the painted model is the draft's
base) and store them on the draft; `serialize` then reads a `BTreeMap` it already
holds. `capture_groups_if_base` stays the one rule, but is called on state change
rather than on the persistence tick.

**M2. `/` rebuilds a whole-document text index on every keystroke.**
`crates/geode-marketdata/src/tile.rs:4459-4490` (`row_labels`), called from
`find` (`tile.rs:4530`) per `FindEvent::Changed` and from `repeat_find`
(`tile.rs:4553`) per `n`/`N`.

`row_labels()` allocates a `Vec<String>` with one `String` per model row. Under
`RowLabel::Hidden` (the dividend panel) it also joins every painted cell of every
row with `" "`, so a 10,000-row × 5-column schedule allocates 10,001 strings and
copies ~50,000 cell texts **per character typed** into the find field.

*Impact:* the find field becomes the slowest keyboard path in the panel on the one
document shape that is already at budget; nothing measures or caps it.
*Direction:* the searchable text is a pure function of the model. Prepare it once in
`MatrixModel::build` (or beside it, keyed by the model `Rc`) and have `find_match`
take `&[String]`/`&[SharedString]` from that cache. If the join is only for hidden
labels, prepare one `SharedString` per row at build time — the model already
prepares every cell's text there.

**M3. An `I64` cell value round-trips through `f64`, losing integers above 2^53 —
the exact hazard already ruled on for attributes.**
`crates/geode-marketdata/src/core/draft.rs:1212-1240` (`parse_cell` returns
`f64`, `I64` arm does `.parse::<i64>().map(|v| v as f64)`),
`crates/geode-marketdata/src/tile.rs:3210-3270` (`commit_cell_edit`:
`ColumnType::I64 => Value::I64(parsed as i64)`),
`crates/geode-marketdata/src/core/draft.rs:1161-1174` (`bumped`:
`Value::I64((current + delta) as i64)` from an `f64`),
`crates/geode-marketdata/src/core/draft.rs:267-275` (`numeric_edit` widens `I64` to
`f64`).

`parse_attr` was explicitly fixed for this — `draft.rs:1241-1263` carries the
comment *"Parsed as `i64` DIRECTLY, never through `parse_cell`'s `f64` (final
review, A4): a round trip through a double loses every integer above 2^53,
silently."* The cell path still does exactly what that comment forbids, so the same
typed value is exact in an attribute and truncated in a cell. `:bump` on an `I64`
column has the same shape.

*Impact:* a plausible wrong number, silently, on any `I64` document column. No
shipped spec has one today (`DIVIDEND.amount` is `F64`), so this is latent — but
the test fixture `SCHEDULE_I64` shows the shape is expected to exist.
*Direction:* give `parse_cell` a `Value`-returning sibling (or return
`Result<Value, String>`) so the `I64` arm never becomes an `f64`; make `bumped`
take the current value as a `Value` and do integer arithmetic for `I64`.

**M4. `display_key`/`parse_display_key` are not injective, so a key part containing
`/` names a different document — and restores a parked draft onto the wrong
underlying.**
`crates/geode-marketdata/src/tile.rs:5201-5226` (`display_key`,
`parse_display_key`), `crates/geode-marketdata/src/commands.rs:24`
(`KEY_DISPLAY_SEPARATOR = '/'`), `tile.rs:4857-4882` (`catalog_keys` builds display
keys from stored partitions), `tile.rs:3910-3937` (`picker_pick` →
`parse_display_key`), `tile.rs:4915-4977` (`serialize` writes
`drafts.<display key>`), `tile.rs:726-768` (`new` reads them back with
`parse_display_key`).

`commands::parse` refuses a part containing the *storage* separator
(`commands.rs:126-131`) but nothing refuses a part containing `/`. A catalog
partition whose key part contains `/` (a vendor ticker such as `BRK/B`, or any
composite the desk spells that way) is displayed as `BRK/B`, and picking that row
splits it back into `["BRK", "B"]` — a different document key. The same
non-injectivity runs through the session: `parked` is keyed by `Vec<String>`,
written as one dotted table name, and re-split on restore, so a parked draft can be
restored under a key it was not made against.

*Impact:* silently querying the wrong document, and — worse — a parked draft of
unsent edits landing on another underlying, where `unresolved_restore` will happily
rebase it by label.
*Direction:* refuse a key part containing `KEY_DISPLAY_SEPARATOR` at the two doors
that can create one (`commands::parse` already refuses the storage separator; add
the display separator) and make `catalog_keys` skip or escape a partition it cannot
spell back. A round-trip property test over `display_key`/`parse_display_key`
belongs in `core`.

**M5. Staleness has no invalidation source: an idle panel never starts saying
"stale".**
`crates/geode-marketdata/src/tile.rs:5304-5310` (`render` sets
`self.header.stale = self.is_stale(chrono::Utc::now())`), `tile.rs:2391-2400`
(`is_stale`), and the tile's only wake-ups: `cx.observe(&frame)`,
`cx.observe(&diagnostics)`, `cx.observe_global::<AppClock>` (`tile.rs:875-1050`).

The staleness reading is correct *given a repaint*, and deliberately a comparison
rather than a format. But nothing schedules that repaint: the production half of
`tile.rs` contains no `cx.spawn`, no `timer(` and no `Task` field (verified by grep
over lines 1–5405). A document that goes stale
while the application is idle — which is precisely the degraded-source case the
marker exists for — keeps painting its time in the quiet tone until something
unrelated notifies. The shell's 500 ms poll notifies only when the calendar date
changes (`crates/geode-shell/src/shell/mod.rs:1338-1354`).

*Impact:* "latency is a feature, silence is a bug" — the one signal that says a
number is not current can be arbitrarily late. PHILOSOPHY §3 makes this a
correctness matter, not cosmetics.
*Direction:* arm a single timer while `source_at.is_some()` and the tile is visible,
firing once at `source_at + stale_after` (and not repeating) to notify; or have the
shell own one coarse "freshness tick" that visible occupants observe, so every
module's marker is driven by one clock rather than none.

**M6. Two consecutive `o`s on a `Typed` axis drop the first row and anchor the
second on a row the trader did not choose.**
`crates/geode-marketdata/src/tile.rs:3517-3533` (`row_verb_target` closes the
editor first), `tile.rs:3485-3516` (`close_editor` drops a *provisional* row and
calls `rebuild_model`), `tile.rs:3558-3618` (`insert_row` then reads
`self.model.rows[row].label` for its anchor).

On a `RowIdentity::Typed` axis (CVI's `term`), `o` inserts a cell-less row and opens
the row-label editor on it (`insert_row` sets `cursor = Cell { row: at, col: 0 }`).
A second `o` runs `row_verb_target`, which closes that editor; `close_editor` sees
`Inserted { cells }` with `cells.is_empty()`, deletes the row and rebuilds, so the
grid shrinks by one and `clamp_cursor` leaves the cursor *index* pointing at
whatever row now occupies it. `insert_row` then reads `model.rows[row].label` as its
anchor — the row *below* the original one when the provisional row was not last.

*Impact:* the new row lands one position away from where `o` was pressed, with no
notice; the trader's first row is gone. Visible, so not a wrong number, but it
breaks the "muscle memory is an asset" contract for the fastest row-entry path.
Verified in three parts: `row_verb_target` closes the editor at its first statement
and reads `self.cursor` only four lines later (3522-3529); `close_editor` drops the
row only when the open target is `EditTarget::RowLabel` with empty cells (3712-3727),
which is exactly the state a bare `o` leaves; and the existing test
`a_second_o_on_the_same_row_keeps_the_first_typed_row_below_it`
(`tile.rs:~13950`) types and commits the first label, then cancels the *cell* editor
the commit opened — a `Cell` target, which `close_editor` leaves alone. So the
committed path is pinned and the provisional path is not.

*Direction:* capture the anchor label **before** `row_verb_target` closes the
editor (or have `close_editor` report that it dropped a row and re-resolve the
cursor by label, not index). The missing probe is `o` then `o` with nothing typed
between them.

**M7. Nothing links `ACTIONS` to `dispatch`, so a renamed verb silently does
nothing.**
`crates/geode-marketdata/src/content.rs:30-79` (`ACTIONS`, 43 ids),
`crates/geode-marketdata/src/tile.rs:2402-2710` (`dispatch` matches the verb as a
`&str`, `_ => return false`),
`crates/geode-marketdata/src/content.rs:467-505`
(`the_default_keymap_binds_exactly_the_actions_this_module_registers`).

The excellent existing test proves `ACTIONS` ↔ keymap ↔ registry agree in both
directions. No test proves `ACTIONS` ↔ `dispatch`: the only other use of the table
in `tile.rs` is `install_fragment_chords` (`tile.rs:10744-10760`), a tooltip-chords
fixture that registers every id but dispatches none. An id that is registered, bound
and palette-listed but misspelled in the match ladder falls to `_ => return false`
— the shell treats it as unhandled and the key does nothing at all.

*Impact:* a silently dead keybinding or palette row, the failure mode the mirrored
keymap tables were retired to prevent.
*Direction:* either a test that dispatches every registered id against a built tile
and asserts `true` (with the `NO_DEFAULT_KEY`-style exception list for the
"not built yet" kind actions), or — better — an `enum Verb` with a `FromStr` derived
from one table that both `ACTIONS` and `dispatch` read.

### Minor

**m1. `FindState.origin` is stale across find sessions.**
`crates/geode-marketdata/src/tile.rs:4491-4545`. `find` reuses the existing
`FindState`'s `origin` on `Changed`, and `self.find` is cleared only by
`FindEvent::Cancelled` or `marketdata::escape` (`tile.rs:2570-2575`). After a
committed find, a *new* `/` session's incremental search starts from the old origin
and `escape` returns the cursor there rather than to where the trader pressed `/`.
`FindEvent` has no "opened" variant, so the module cannot tell a new session from a
changed query. Direction: on `Committed`, reset `origin` to the current cursor — the
committed match is the honest place for the next `escape` to return to.

**m2. The `EchoStep::Unchecked` path lets the `:auto` policy delete work reported as
sent.** `crates/geode-marketdata/src/tile.rs:1924-1992` (`echo_of` sets
`Behind` and returns `Unchecked`), `tile.rs:1690-1697` (`moved = true`, then the
policy branch runs on `draft.is_behind()`). `docs/current/features.md` states "The
update policy does not apply to a sent draft", but a `Sent` draft with no `sent`
rows is converted to `Behind` and then handed to `Replace`, which reverts it. The
tile's own comment calls the state unreachable; the test
`a_sent_draft_with_nothing_to_compare_goes_behind` shows it is constructible.
Direction: carry a flag through `EchoStep::Unchecked` that suppresses the policy for
that delivery, and add the `Replace` variant of that test.

**m3. `unmatched_rows` can desynchronise when both sides are missing a cell.**
`crates/geode-marketdata/src/core/upload.rs:~470-500` (`merge_order`, `cell_eq`,
`cell_cmp`). `cell_eq` requires `a.get(i).is_some()`, so two rows whose cell is
absent on *both* sides are "not equal"; `cell_cmp` returns `Equal` for that pair, and
`merge_order` maps `Equal.then(Less)` to `Less`, advancing only one side. The merge
walk then slips by one and reports a large spurious difference. Unreachable while
every column has `rows()` entries (which `assemble` guarantees), but the honest
answer for a ragged document is "echo not comparable", not an inflated count.
Direction: check column lengths once in `echo_differs` and return the
not-comparable path.

**m4. A `y/n` confirm swallows every chord, contradicting the crate's own standing
rule.** `crates/geode-marketdata/src/tile.rs:1464-1485` (`confirm_key` consumes
every key, "chords included"), against `tile.rs:1054-1066` (`key_context` reports
`insert` for the confirm) and the rule stated at `tile.rs:990-1020` and enforced by
`content.rs:659-688`
(`ctrl_k_still_opens_the_palette_from_the_open_picker`): a module must never take a
shipped chord away from an insert-mode field. The prompt does. The cost is one
keystroke (any key cancels), so this may well be the right ruling — but it is
currently an undocumented exception to a rule the crate tests elsewhere.
Direction: either let chords through and cancel (the picker's rule), or record the
exception where the rule is stated and add a test pinning it deliberately.

**m5. `Delivery::Price(_) => {}` discards a routing bug in silence.**
`crates/geode-marketdata/src/content.rs:219-221`; the same arm exists in
`crates/geode-blotter/src/content.rs:128` and
`crates/geode-timeseries/src/content.rs:194`. The comment says "an outcome
addressed here is a routing bug" and then drops it. One `tracing::warn!` costs
nothing and turns an invisible mis-route into a diagnostic.

**m6. `LABEL_WIDTH`/`CELL_WIDTH` are off the rem scale.**
`crates/geode-marketdata/src/delegate.rs:52-53`, honestly recorded as a known gap in
the crate README and the constants' own doc. The stated reason —
`TableDelegate::column` has no window — is no longer binding: the delegate already
carries `tones: FlooredTones` refreshed at paint (`delegate.rs:162`, 209), so it can
carry a rem refreshed the same way (or set at `install_model`, which already runs on
every swap). Worth doing: a base-font change currently resizes every panel's text
but not its columns.

**m7. `render` mutates retained state.** `crates/geode-marketdata/src/tile.rs:5306-5310`
(`self.tones.refresh(theme)`, `self.header.stale = …`). Both are memo refreshes with
no `notify`, and both are documented, so this does not risk a redraw loop — but it
is the pattern the GPUI guide names first under Performance rules, and it is what
makes M5 (staleness with no invalidation) hard to see. If the freshness tick in M5
lands, `stale` becomes a prepared field like every other and `render` stops writing.

**m8. Three near-identical popup renderers.**
`crates/geode-marketdata/src/popup.rs:378-518` (`render_menu`), `520-612`
(`render_picker`), `626-705` (`render_choice`): the same `popover_surface` +
`occlude()` + `on_mouse_down_out` + accent-highlighted row loop + `deferred(anchored())`
with `with_priority(1)`, differing only in anchor corner and row source. ~250 lines
for what is one list popup with three callers. See also I3 — the same three
functions exist again in `geode-pricer` and `geode-timeseries`.

**m9. `commit_cell_value`'s constant-time patch depends on an unenforced `Rc`
count.** `crates/geode-marketdata/src/tile.rs:3388-3402`: the delegate's clone is
swapped for `Rc::new(MatrixModel::default())` so `Rc::make_mut` finds the model
uniquely held. Correct today, and well explained — but if any future code holds a
second `Rc<MatrixModel>` (a render cache, a popup, a test door) the patch silently
becomes a full clone of every row and the 116 ns commit becomes the 8.18 ms build,
with no failure and no test to notice. Direction: assert `Rc::strong_count == 1`
before `make_mut` behind `debug_assert!`, or return the count in the patch's answer
so a bench can pin it.

**m10. `MatrixModel::empty` takes a `spec` it discards.**
`crates/geode-marketdata/src/core/matrix.rs:376-391` (`let _ = spec;`). A dead
parameter kept for a future caller, with a comment saying so. Either use it (an
empty panel could paint its column strip) or drop it.

**m11. Per-open `detach()`ed input subscriptions.**
`crates/geode-marketdata/src/tile.rs:3855-3875` (`open_picker`), `3960-3980`
(`open_choice`). A fresh `cx.subscribe_in(&input, …).detach()` per popup open, whose
lifetime is then the `InputState`'s rather than the popup's. It works — the state
dies with the popup — but the guide asks for subscriptions whose lifetime follows the
entity that owns them. Storing the `Subscription` in `PickerState`/`ChoicePopup`
makes "this handler cannot outlive its popup" a compiler fact instead of a
reachability argument.

**m12. `geode-documents`: an empty `<currency>` is accepted from the wire but
refused from the keyboard.** `crates/geode-documents/src/dividend.rs:~330-340`
(`Shape::Leaf(Leaf::Currency)` stores `trimmed.to_string()` with no emptiness
check, unlike `underlying` immediately above it, which refuses "underlying is
empty"), against `crates/geode-marketdata/src/core/draft.rs:1258`
(`ColumnType::Utf8 if trimmed.is_empty() => Err("a value is required")`). A feed can
deliver an attribute the panel will not let a trader type, and `write` re-emits it.

**m13. `geode-documents`: `unknown_paths` is unbounded per document.**
`crates/geode-documents/src/cvi.rs:~370` and
`crates/geode-documents/src/dividend.rs:~305`: one `String` per unknown-element
*occurrence* on the receiver thread, deduped only later by the receiver. Bounded by
document size, so not a leak, but a document with a large unrecognised subtree pays
an allocation per element for a report that collapses to one line.

**m14. `cvi::parse` assumes `<nodes>` precedes `<slices>`.**
`crates/geode-documents/src/cvi.rs:~430-445`: the param-count check reads
`nodes.len()` when each slice closes, and a document ordering `<slices>` first
fails with "nodes is missing or has no node (no node was read before the first
slice)". That message is honest and the ordering is part of the assumed XSD, so this
is a documentation matter rather than a defect — worth stating in the module doc's
tag-name caveat, since it is the same class of assumption.

### Ideas

**I1. Multi-select (shift+move) is feasible here without touching the table
component, but should start with yank and delete only.**
The cursor is already a pure value in `core/cursor.rs` (`Cursor::{Cell,Attr}`,
`Grid`, `step`, `clamp`), and the cursor *border* is painted by the delegate from its
own mirror (`delegate.rs:407`, `at_cursor`), not by the component — the component's
selection is only mirrored for scroll-into-view (`tile.rs:2171-2198`). So a range is:
an `anchor: Option<Cursor>` on the tile, range math in `core/cursor.rs` (pure,
testable), a range in `MatrixDelegate`, and an `in_range` fill in `render_td` beside
the existing `cell_paint`. The pinned `TableState` needs no range API.
The hard part is verbs, not painting: `:bump` already means "the cursor's row or
column", so a selection gives one verb two meanings; and `dd` over a range must be
one draft mutation, which the draft cannot currently undo (`geode-pricer` has
`Sheet`/`Edit`/undo; this crate has only `:revert`, all-or-nothing). Recommendation:
ship `y` over a range (a TSV block — `yank_text` already builds one) and `dd` over a
range (add `Draft::delete_rows`, one call, one notice), and leave *edits* over a
range until the draft has an undo. Since TODO.md wants the same in the blotter, the
range math belongs in the shell or `geode-widgets`, not in this crate.

**I2. A cell context menu is cheap and should extend `core/menu.rs`, not add a
second popup.** The decision half already exists as a pure function
(`core/menu.rs:82-…`, `rows(&MenuInputs, Clock) -> Vec<MenuRow>` with enablement
reasons and ticks), the popup can already anchor under a *cell* (the choice popup
does exactly that, `delegate.rs:461-482`), and `geode-timeseries` already ships the
right-click door to copy (`crates/geode-timeseries/src/header.rs:376-380`,
`on_mouse_down(MouseButton::Right, …)` → `chip_context_menu`). So: route a right
press to `cursor_to(row, col)` then `toggle_menu`, and add the cell verbs (edit,
step, insert above/below, delete row, yank) to `menu::rows` gated on the cursor's
kind — one vocabulary, one enablement table, one keyboard route each, which is what
"every pointer action has a keyboard route" asks for. Do I3 first so all three
modules get the surface.

**I3. Promote the popup surface and the editor door to shared homes.**
`on_mouse_down_out` + `deferred(anchored)` + a highlighted row list now exists three
times: `crates/geode-marketdata/src/popup.rs`, `crates/geode-pricer/src/popup.rs`
(`render_menu` at 241, `on_mouse_down_out` at 70 and 251, `deferred(` at 116 and
338), `crates/geode-timeseries/src/popup.rs`. Likewise the blur-then-drop editor
rule is implemented twice, with `geode-pricer/src/tile.rs:1040-1053` citing this
crate ("the market-data rule, CLAUDE.md") in its own comment, and the arrow-nudge
door exists as `geode_core::nudge_text` here but as a private `cell::nudge` in the
pricer (`geode-pricer/src/tile.rs:1054-1078`). The `ChoiceList` typeahead core is
already shared (`geode_shell::choice`) and it works well — that is the template.
Candidates, in order of payoff: (1) one list-popup element in `geode-widgets`
(surface, occlusion, outside-click, hover/pick, window snapping); (2) one
`close_editor` helper that owns blur-then-drop for an `Option<Entity<InputState>>`;
(3) one confirm-prompt widget — this crate's `PendingUpload` (focus handle + blur
subscription + "every key is mine" handler, `tile.rs:266-300`, 1464-1519) is the
only y/n-in-a-header in the workspace and the next module to want one will copy it.

**I4. Split `tile.rs` along the seams it already has.** The production half is
5,405 lines in one `impl`, and four regions are already almost pure:
- **draft/delivery decision** (`apply_snapshot` 1644–1923 + `echo_of` 1924–1992):
  already written as "decide on a copy, then commit". The decision half is a pure
  function of `(draft, snapshot, policy, unresolved_restore, sent, echo)` returning
  a plan; only the commit needs `&mut self`. Moving the decision into
  `core/delivery.rs` would make the `:auto` matrix, the echo states and the restore
  rule testable without a window — today they need a full `VisualTestContext`.
- **upload arming** (`arm_upload` 1364–1463): every refusal is a pure predicate over
  `(draft, model, spec, targets, as_of, in_flight)`; only the focus handle and the
  subscription need a window. `core/upload.rs` is the obvious home and already holds
  `assemble`/`echo_differs`.
- **key dispatch** (2402–2710): the 308-line match is really three tables —
  motions, popup-scoped verbs, draft verbs. A `Verb` enum with `FromStr` (see M7)
  plus one `fn route(verb, popup_kind, editor_open) -> Route` in `core` would make
  the popup-exclusivity rules (which verbs must *not* close the popup, line
  2419-2445) checkable in isolation.
- **row verbs** (3517–3693): anchor selection is pure over
  `(model rows, draft rows, cursor, below)`; M6 exists because it is entangled with
  editor teardown.
That is roughly 1,200 lines of production code moved behind pure interfaces, and it
is where the mutation harness would gain the most.

**I5. Keep the review chronology out of the comments.** CLAUDE.md: "A code comment
should state the local invariant and failure it prevents; it should not require a
task number or spec section to make sense." Most comments here do state the
invariant and then cite — fine — but a large number are pure provenance:
`tile.rs:1075` ("review C-1, 2026-09-17"), `1659` ("M-2, final whole-branch
review"), `1745` ("the M-1 path"), `2404-2410` ("review fix round 1, MIN-5,
extended by Task 5"), `3210-3230` ("final review, B2", "I-3"),
`draft.rs:596` ("M-3"), `popup.rs:196` ("review fix round 1, IMPORTANT-3"),
`delegate.rs:113` ("review Minor 4"). Those tags are unresolvable for a future
reader and belong in `docs/phase-history.md`. A mechanical sweep that deletes the
tag and keeps the sentence would shorten the file measurably without losing a single
invariant.

---

## (c) Systemic patterns

1. **"Decide on a copy, then commit" is the crate's best idea and is applied
   unevenly.** `apply_snapshot` (`tile.rs:1655-1800`) builds every decision against
   a cloned draft and writes no field until the generation is known to build —
   `Draft::bump` (`draft.rs:544-584`) does the same check-then-write. The row verbs
   do not: `insert_row` mutates the draft, rebuilds, then looks for the row
   (`tile.rs:3597-3612`), which is how M6 arises.
2. **Pure cores exist and are good, but the gpui half keeps growing around them.**
   `core/{spec,matrix,draft,cursor,menu,upload}` are genuinely window-free and
   heavily tested. Every *new* rule of the last few slices (upload arming, echo
   states, row-verb anchoring, popup exclusivity) landed in `tile.rs` instead. The
   ratio is the architecture finding, not the line count.
3. **Persistence and paint are treated as free.** M1 (a model build on the session
   tick) and M2 (a document index per keystroke) are both "this is not `render`, so
   it is not a hot path". The perf guide's budget is per *path*, and both of these
   are on paths a trader drives continuously.
4. **Invisible-until-repainted signals.** M5 (staleness) is the clearest case, and
   the same shape would apply to any future time-derived chip: the panel has no
   clock of its own and the shell's tick deliberately notifies only on a date
   change.
5. **Component side effects are load-bearing.** `set_selected_row` calling
   `cx.stop_propagation()` is relied on (and documented) in
   `popup.rs:454-480`, and `clear_selection` not doing so means propagation
   behaviour differs by where the cursor is. `install_model`'s mandatory `refresh`
   (`tile.rs:2127-2137`) is the other instance. Both are correctly commented, but
   they are pinned-version facts, not contracts.
6. **Duplication across sibling modules is drifting rather than shrinking.** Three
   popup renderers, two editor-close doors, two nudge implementations, two
   `DataTable` key reclaims (`lib.rs:37-63`, deliberately a copy). The shared
   `ChoiceList` shows the alternative works.

## (d) What is done well

- **Charter compliance is real, not claimed.** No interpolation, solving or
  conversion anywhere: the CVI slice values (`fwd`/`atm`/`skew`) are read from the
  document (`matrix.rs:637-830`), painted at their own precision
  (`spec.rs:SliceValue.format`), edited as values and written back
  (`upload.rs:long`), and a slice whose rows *disagree* is refused rather than
  averaged (`cvi.rs:~780-800` on write, `matrix.rs` on build). Both `KindAction`s are
  `built: false` and answer "not built yet" (`tile.rs:2688-2700`), with the comment
  stating that a built one must be an egress request. `:bump`/`nudge_text` are data
  entry at a painted precision, not arithmetic on a model.
- **Time discipline.** No `chrono::Local` in either crate (verified by grep); every
  displayed time goes through `geode_core::clock::Clock`, carried on the tile and
  refreshed by an `observe_global::<AppClock>` handler (`tile.rs:1030-1045`), so
  `HeaderModel::prepare` and `menu::rows` stay pure functions of their inputs.
- **Element identity.** Repeated elements use tile-derived ids
  (`ElementId::NamedInteger("marketdata-state", tile_id)`, `header.rs:490`), tooltip
  selectors are built once in `new` (`tile.rs:684-691`), and every positional string
  is inside a `debug_selector` closure that gpui drops unevaluated in release. No
  index-keyed `ElementId` for reorderable content anywhere.
- **Prepared-state rendering.** `HeaderModel::prepare` formats everything once per
  change (`header.rs:254-301`); `render_td` clones `SharedString`s and copies `Copy`
  colours (`delegate.rs:347-483`); the choice popup's rows are prepared into an `Rc`
  and mirrored as a refcount (`popup.rs:152-163`); `DateFieldPaint` is prepared per
  keystroke rather than per frame (`tile.rs:414-433`). The 116 ns cell patch
  (`matrix.rs:303-375`) is proven equal to a rebuild by
  `patch_cell_matches_a_rebuild` on both grid shapes.
- **Theme honesty.** `FlooredTones` (`tile.rs:155-202`) floors `warning`/`danger`/
  `primary_foreground` to a 3:1 ratio with a 6-colour memo key, and three
  bundled-theme sweeps (`tile.rs:5448`, `delegate.rs:579`, `header.rs:901`) check
  every theme rather than assuming the token pairs are readable. The finding that
  `warning_foreground` is a *background*-family token is exactly the kind of thing
  reviews usually miss.
- **Tests exercise production routes.** The harness drives the tile through
  `Box<dyn TileContent>` (`tile.rs:6100-6160`), wraps the view in
  `gpui_component::Root` *because* the focus registry is load-bearing
  (`tile.rs:6120-6140`), installs the real keybinding reclaims, and counts
  bubble-phase clicks/keys/moves on a host element to prove this crate's
  capture-phase handlers do not swallow the shell's own listeners
  (`Host`, `tile.rs:5660-5700`). 303 tests, ~110 mutation-harness entries naming
  them.
- **`geode-documents` guards every index behind a vocabulary check.** `write` in both
  kinds verifies names *and order* before indexing `rows.axes[0]`/`values[3]`
  (`cvi.rs:594-632`, `dividend.rs:525-558`), refuses non-finite numbers, refuses a
  status outside the closed set in both directions, and keeps one `(tag, column)`
  table per kind so the parser and writer cannot drift. `mint_ids`
  (`dividend.rs:505-524`) is deterministic, never collides with a draft's `new-`
  labels, and its one real limitation (a pure same-day reorder) is documented in the
  README, `features.md` and the rebase guard that refuses such edits.
- **Refusals are honest and named.** Every gate returns a sentence a trader can act
  on, with the door out in it (`BEHIND_REFUSED`, `ECHO_REFUSED`,
  `REBASE_AWAITING_ECHO`, `ALREADY_DELETED`, `"upload: the panel shows … not live"`),
  and a refused commit keeps the editor open with the typed text rather than
  discarding the line.
