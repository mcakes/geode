# Review: `geode-pricer` / `geode-pricing` and the pricing request seam

Scope read: `crates/geode-pricer` (all 21 source files + `benches/core.rs`),
`crates/geode-pricing/src/lib.rs`, `crates/geode-core/src/pricing.rs`,
`crates/geode-data/src/pricing/{mod,worker}.rs`, the `service.rs`/`handle.rs`/
`bridge.rs` pricing lanes, plus `docs/PHILOSOPHY.md`, `CLAUDE.md`, both crate
READMEs, `docs/current/features.md` §Pricing, `request-delivery.md`,
`performance.md`, and the GPUI coding guides. All passes completed, including
the bench file and the cross-crate greps.

## Summary

1. The leaf boundary itself is genuinely clean: `geode-pricing` depends on
   `geode-core` alone, no module or shell crate names it, and `geode-pricer`'s
   manifest pins the prohibition in a comment — this is the best-enforced
   architectural rule I found in the scope.
2. The one real Philosophy §1 breach is expiry resolution: the app resolves
   `Z26`/`DEC26` to the third Friday for *every* underlying, which is a
   per-index calendar convention and produces a plausible wrong date silently.
3. Correctness of Sheet/Edit/undo is unusually well pinned (identity tests,
   LIFO rule, `Restore` trusting revisions), but two seams leak: a
   worker-queue refusal parks lines in `Failed` with nothing retrying them,
   and free-text underlyings can corrupt the `spot_overrides` encoding.
4. Performance: `Sheet::index_of` is a linear scan called per id inside
   `apply`'s and `install`'s loops, making sheet-wide edits and deliveries
   O(n²); the 1.52 ms/1000-row bench is that cost, and there is no
   patch-a-cell path where market-data has one.
5. Duplication is the largest structural cost: the popup/menu/typeahead/cell-
   editor stack is now written a third time (marketdata, timeseries, pricer)
   with three `step`s and three `MenuItem`s, and `tile.rs` carries six
   separable seams in 2.4k production lines.

---

## Critical

### C1. The app resolves a month code to the third Friday for every underlying — a per-index calendar convention decided in the module

**Location:** `crates/geode-pricer/src/core/shorthand.rs:1-9` (module doc),
`:33-38` (`third_friday`), `:56-107` (`parse_expiry`, IMM branch `:59-71`,
month-name branch `:73-83`); contrast `crates/geode-core/src/pricing.rs:1-14`
and `:48-70`.

`parse_expiry` turns `Z26` and `DEC26` into `Expiry::Date(third Friday)` at
parse time, for any underlying, and the module doc calls this "a date
CONVENTION, not a financial calculation (the spec says so); holidays are not
considered." But the third-Friday rule is not universal across the index
products this desk trades: it holds for SX5E and SPX, and does not for NKY
(second Friday convention) or HSI (penultimate business day). Once resolved,
the date is what goes over the seam in `PriceRequest`, so the library prices
the wrong expiry and answers a plausible number with no error anywhere. The
same file's sibling behaviour shows the line was understood: `Expiry::tenor`
(`geode-core/src/pricing.rs:48-70`) refuses to resolve `3m` precisely because
"that is the library's calendar, not ours", and the `geode-core` module doc
states "nothing here resolves ... a tenor against a calendar". A month code is
the same class of thing as a tenor.

**Impact:** a silently wrong expiry on any non-third-Friday underlying — the
"plausible wrong" failure CLAUDE.md ranks above an explicit error. It also
undermines the leaf claim: if the pricer is moved out of process, the caller
has already committed to a calendar the service may disagree with.

**Direction:** pass the month code through unresolved — add
`Expiry::MonthCode(String)` beside `Tenor` and let the library resolve it, the
way the tenor already is. If a resolved date must be shown in the grid, it
belongs in the *outcome* (an echoed resolved expiry), not in the request. If
the current behaviour is kept deliberately, the ruling should be recorded
against the underlyings it is wrong for, and the cell should mark a resolved
date as app-derived rather than as the trader's input.

---

## Major

### M1. A worker-queue refusal leaves every line `Failed("resubmit")` with nothing that resubmits

**Location:** `crates/geode-data/src/service.rs:1394-1422`;
`crates/geode-pricer/src/tile.rs:1158-1219` (`submit`), `:1242-1281`
(`deliver`); `crates/geode-pricer/src/core/sheet.rs` `install`'s
`Err(message) => self.state[row] = LineState::Failed(message)`.

There are two refusal paths and the tile only handles one. If the *channel*
refuses, `data.price` returns false, `refusals` increments and `arm_retry`
backs off — correct and well tested. If the channel accepts but the *pricing
worker's* queue is full, `service.price` synthesises a `PriceOutcome` at the
same key/tag with `Err("the pricing queue is full; resubmit")` for every line.
The tile's `deliver` accepts it (key and tag match), `install` writes
`LineState::Failed` per line, and the lines leave `stale_lines()`. `submit` is
then a no-op — there is nothing stale to send — `refusals` is still 0, so no
retry is armed. The status column literally tells the trader to resubmit and
nothing in the tile does.

**Impact:** with `:refresh off` (or `[pricing] refresh = "off"`) the sheet is
stuck showing `N failed` until a manual `:price`. With the 30 s default it
self-heals at the next tick, which is why no test caught it: the tile harness's
`fill_queue()` (`tile.rs:2594`) fills the *DataHandle* channel, so the whole
worker-full path is untested at the tile.

**Direction:** treat a per-line error whose text marks it as a queue refusal as
a refusal rather than a result — or, cleaner, give `PriceOutcome` a
`refused: bool` (or a `Refused` variant) so the tile can keep those lines
`Stale` and arm the same backoff. Add a tile test that drives the synthetic
outcome.

### M2. Strategy leg tables are financial definitions living in the module, with no mutation coverage

**Location:** `crates/geode-pricer/src/core/template.rs:33-39` (the `CS`, `PS`,
`STRD`, `STRG`, `RR`, `FLY`, `CAL` tables), consumed by
`shorthand.rs:317-341`; mutation harness: `scripts/mutation-check.sh` has
**zero** entries touching `template.rs`.

`RR = [-1 put@k0, +1 call@k1]`, `FLY = [+1 c@k0, -2 c@k1, +1 c@k2]`,
`CAL = [+1 c@expiry1, -1 c@expiry0]` are assertions about what a risk reversal,
a butterfly and a calendar *are*. A flipped sign or a wrong weight here does
not error: it builds a package whose legs price fine and whose folded total is
wrong — exactly the class CLAUDE.md calls "plausible wrong totals". The one
`fold_packages` sign is mutation-guarded ("a package sums its legs unsigned",
`mutation-check.sh:15964`), but the tables that decide the signs in the first
place are not, and `template.rs` has only round-trip token tests.

**Impact:** the highest-consequence, least-guarded data in the crate.

**Direction:** two options, not exclusive. (a) Add mutation entries per
template (flip `RR`'s put weight, change `FLY`'s `-2` to `-1`, swap `CAL`'s
expiry indices) each naming a test that asserts the resulting legs. (b) Longer
term, treat the table as config (Philosophy §5) or have the leaf echo the legs
it priced, so the app is not the authority on the decomposition.

### M3. `Sheet::index_of` is a linear scan called per id inside per-edit and per-delivery loops

**Location:** `crates/geode-pricer/src/core/sheet.rs:180-182` (`index_of`),
`:~350` (`install` calls it per result), `crates/geode-pricer/src/core/edit.rs:119-135`
(`apply` calls it twice per touched id), `:141-163` (`touched_by` returns
*every* line id for `SetSheetShift`), `sheet.rs` `has_id` (`ids.contains`)
called per record in `restore`.

`index_of` is `self.ids.iter().position(...)`. `apply` of a `SetSheetShift`
collects every line id, then does one `index_of` before and one after per id;
`deliver_all` calls `install` per result, each doing an `index_of`. Both are
O(n²) in sheet length. The recorded benchmark confirms it:
`performance.md:78` lists "Line-pricer sheet shift + undo, 1,000 rows,
1.52 ms" — 1.5 µs per row for what is arithmetic-free bookkeeping, against a
single-cell edit at 6.66 µs total.

**Impact:** at 1,000 lines a sheet-wide shift or a full delivery is already
a millisecond-scale synchronous cost on the UI thread, inside the 8 ms budget
but with no headroom for a larger book. The refresh tick hits this path every
interval.

**Direction:** keep a `HashMap<LineId, usize>` (or a sorted side index)
rebuilt in `reindex_parents`, which every structural edit already calls. That
turns `index_of`, `has_id` and `install` into O(1) and makes
`grid_row_of`/`GridModel` lookups cheap too.

### M4. The popup/menu/typeahead stack is now implemented three times across sibling modules

**Location:** `crates/geode-pricer/src/popup.rs:28-35` (`popover_surface`),
`:61-121` (`render_choice`), `:130-147` (`MenuItem`), `:164-176` (`step`),
`:179-187` (`snap`), `:203-231` (`menu_row_paint`), `:241-343` (`render_menu`)
versus `crates/geode-marketdata/src/popup.rs:40,378,626` and
`crates/geode-marketdata/src/core/menu.rs:171` (its own `step`), and
`crates/geode-timeseries/src/popup.rs:551,1048` plus
`crates/geode-timeseries/src/core/menu.rs:168` (a third `step`). The pricer's
own comments say so out loud: "the market-data list's rule", "the market-data
arrangement", "which this crate may not import (CLAUDE.md)".

Three independent `MenuItem` enums, three highlight-stepping functions with the
same separator/section semantics, three popover surfaces, three
`on_mouse_down_out` closers, three hover-moves-the-highlight rules. Each was
re-derived correctly, but each is also a separate place for the idiom to drift,
and the review history shows the drift is real (the pricer needed its own
`deferred`/`anchored` fix for occlusion that market-data had already made).

**Impact:** Philosophy §4 ("a module never invents its own navigation idiom")
is being honoured in spirit and violated in mechanism; a fourth module pays the
cost again, and a fix to the idiom has to be made three times.

**Direction:** the sibling-dependency rule points the way — this belongs behind
a shell or widgets door, as `shell::choicedialog`, `shell::listrow` and
`shell::control` already are for their surfaces. A `shell::actionmenu` taking
`&[MenuRow]` (title, trailing lane, enabled-with-reason, tick, section,
separator) plus the highlight-step/snap pair would absorb all three. The
better home is `crates/geode-widgets`, which exists for exactly this: it
depends on `geode-core` + `gpui` only and its manifest states the direction —
"`geode-shell` and every module depend on this crate, never the reverse"
(`crates/geode-widgets/Cargo.toml:12`) — and it currently holds one widget
(`datefield`), so the pattern is established but barely used.

### M5. Cell editing, the editor lifecycle and the click-anchor dance are duplicated between pricer and marketdata

**Location:** `crates/geode-pricer/src/tile.rs:132-188` (`Editor`),
`:823-910` (`begin_edit`), `:912-1000` (`commit_edit`), `:1002-1052`
(`choice_hover`/`choice_pick`/`close_editor`), `:1054-1107`
(`nudge`/`sync_editor`), `:2140-2177` (`follow_editor`/
`drop_orphaned_editor`); the same shapes at
`crates/geode-marketdata/src/tile.rs:364` (`EditorState`), `:425`
(`EditorPaint`), `:2772`, `:2963`, `:3485`, `:4009`, `:4043`.

Both modules implement: an editor keyed by row identity + column kind,
re-checked at commit; a mirror struct handed to the delegate for paint; blur-
before-drop; a deferred blur when there is no `Window`; a free-vs-closed
typeahead with the "highlight is only a guess" rule. The pricer's version is
the better-specified one (the `moved` flag and its hover exemption at
`tile.rs:142-153` and `:1002-1014` is a genuinely subtle correctness rule), and
it is invisible to the module that will need it next.

**Impact:** the most intricate keyboard semantics in the app exist twice, with
one copy's hard-won rules unavailable to the other.

**Direction:** extract the pure half — target identity, commit re-check,
free/closed choice resolution, the `moved` rule — into a shell-side
`CellEditorState` that owns no `InputState`, leaving each tile the text/focus
bridge it already has. The pricer's `Choice::{Value,Keep,NoMatch}` enum
(`tile.rs:157-163`) is the right vocabulary to lift.

### M6. Free-text underlyings can silently corrupt the persisted `spot_overrides` attribute

**Location:** `crates/geode-pricer/src/core/storage.rs:114-125`
(`encode_overrides`), `:127-148` (`parse_overrides`);
`crates/geode-pricer/src/core/cell.rs:~148-153` (the `Underlying` commit arm).

Overrides persist as `A=1;B=2` in one UTF-8 attribute. The underlying cell is a
*free* typeahead whose commit validation is only "not empty, no whitespace"
(`cell.rs`: `if t.is_empty() || t.contains(char::is_whitespace)`), so `A=B;C`
is an accepted underlying. `:spot A=B;C 100` then encodes `A=B;C=100`, and
`parse_overrides` splits on `;` and `=` to produce two unrelated overrides
(`A` → `B` fails to parse as a number → the whole sheet fails to load, or,
with other shapes, two wrong overrides installed). The encoding is the only
place a value's own characters are structural.

**Impact:** a save/reload round trip either refuses the whole document
(`from_rows` returns `Err`, which sets `save_blocked` and the tile shows the
fallback) or installs overrides the trader never set. Reachable by typing, not
just by hand-editing.

**Direction:** reject `;` and `=` in the underlying commit (and in `:spot`'s
argument) with a footer reason — cheapest and consistent with the "one word"
rule already there. Alternatively store overrides as per-row values rather than
a packed attribute, which also removes the `f64`-through-`Display` round trip.

### M7. The action menu's enabled state ignores the loading gate that `dispatch` enforces

**Location:** `crates/geode-pricer/src/tile.rs:1764-1833` (`menu_items`),
`:1869-1896` (`menu_pick`), `:2045-2064` (`set_view`), `:1751-1762`
(`refuse_while_loading`).

`menu_items` computes `enabled` from cursor position and undo depth, but never
from `self.loading`. The verbs behind those rows *do* refuse while loading:
`set_view` calls `refuse_while_loading`, and `dispatch`'s
`delete|undo|redo|put|move|group|ungroup` arm refuses with "the sheet is still
loading". So during a pending load the menu offers `Group into package`,
`Delete row`, `Undo` and every `View` row as live rows that answer with a
footer refusal when picked — while the crate's own convention (README: "a
disabled row never takes the highlight fill"; features.md: "on a disabled row
the reason") is to say why *before* the click. `Reprice all lines` is worse: it
is enabled, and picking it silently does nothing (`reprice_all` →
`mark_all_stale` on the empty fallback → `submit` returns early on `loading`).

**Impact:** the one surface whose job is to state what is possible disagrees
with the dispatcher, and one row is a no-op with no feedback — "silence is a
bug" (Philosophy §3).

**Direction:** thread `self.loading` into `menu_items` and disable those rows
with "the sheet is still loading", including the `View` section and `price`.
A single `fn refuse_reason(&self, verb) -> Result<(), &'static str>` shared by
`menu_items` and `dispatch` would make the two unable to disagree.

---

## Minor

### m1. `apply_edits`'s partial-rollback failure changes the sheet while reporting a refusal

**Location:** `crates/geode-pricer/src/tile.rs:653-699`.

When one edit of a multi-edit (`:spot clear`) is refused, earlier edits are
unwound in reverse. If an *inverse* is itself refused, the code logs
`tracing::error!`, clears the undo history and stops unwinding — leaving the
sheet partly mutated with no undo entry — then returns the original refusal,
which is all the footer shows. The trader sees "refused" and the sheet has
changed, unrecoverably. Reachable only if `Sheet::undo` of a just-recorded
inverse fails, which should be impossible, but the branch exists precisely
because it isn't proven so.

**Direction:** if the branch is genuinely unreachable, `debug_assert!` it and
say so; if not, surface it in the footer (the log line is invisible to the
trader) with the "history cleared" wording `history_step` already uses.

### m2. Index-derived `ElementId`s on reorderable rows

**Location:** `crates/geode-pricer/src/delegate.rs:196-206` (`render_tr`'s
`div().id(("row", row_ix))`), `:296` (`div().id(("pricer-chevron", row_ix))`).

Rows are reorderable (`shift+j`/`shift+k`, `Edit::Move`) and insertable, and
both ids are positional. The coding guides are explicit: "Never derive identity
from ... a mutable list index when items can be inserted or reordered", and the
chevron id keys hover/pressed element state, so after a move the pressed state
belongs to the position rather than the package. `("row", row_ix)` matches the
component's own default so it is at least consistent with the library; the
chevron is the pricer's own.

**Direction:** key the chevron on the row's `LineId` (the model already carries
it: `GridRow::id`), namespaced by the tile as `menu_tip` already is.

### m3. No patch-a-cell path: every delivery rebuilds ~14k `SharedString`s

**Location:** `crates/geode-pricer/src/grid.rs:126-200` (`GridModel::build`),
`crates/geode-pricer/src/core/columns.rs:~290-370` (`cell_text` allocates a
`String` per cell); `crates/geode-pricer/src/tile.rs:2098-2110` (`rebuild`),
`:1242-1281` (`deliver` ends at `rebuild`); `docs/current/performance.md:99`
and the market-data contract two lines above it.

performance.md records the precedent explicitly: "A market-data delivery or
structural edit builds a `MatrixModel`; an ordinary cell commit patches it",
and market-data's measured patch is 116 ns against an 8.18 ms build. The pricer
has only the build path (1.85 ms at 1,000 rows × 14 columns), so a refresh tick
— which changes only result cells, `priced_at` and `status` — reallocates every
cell text on the sheet, including qty/underlying/expiry/strike that cannot have
changed.

**Direction:** not urgent at current sizes and honestly documented, but the
shape is known: a `patch_results(&mut GridModel, &Sheet, &[LineId])` that
rewrites only the result-bearing columns for the delivered lines. Worth doing
before the sheet grows or before the refresh interval shortens.

### m4. Half the benchmarks have no recorded reference value

**Location:** `crates/geode-pricer/benches/core.rs` (six benchmarks:
`parse_1000_lines`, `apply_undo_sheet_shift_1000`,
`apply_undo_set_instrument_1000`, `deliver_all_1000`,
`to_rows_from_rows_1000`, `grid_build_1000`) versus
`docs/current/performance.md:78-80` (three rows).

The three unrecorded ones include `deliver_all_1000` (the refresh-tick path)
and `to_rows_from_rows_1000` (the save/load path) — the two most likely to
regress with the O(n²) `index_of` in M3. CLAUDE.md requires measured changes to
named hot paths to be recorded with their conditions.

**Direction:** add the three missing rows with their conditions, or drop the
benches if they are not reference points.

### m5. The stateful `Pricer` trait is the one part of the door that would not survive the move out of process

**Location:** `crates/geode-core/src/pricing.rs:160-175` (`Pricer::set_overrides`
+ `price`), `crates/geode-data/src/pricing/worker.rs:~130-160` (the one
`set_overrides` per batch under `catch_unwind`),
`crates/geode-pricing/README.md` ("reached through requests rather than a
direct call").

The *wire* shape is fine — `PriceParams` carries `overrides` — but the trait is
stateful by design ("spec ruling 1"), so its correctness depends on the worker
serialising batches on one thread. That is a real constraint documented in the
worker, not a defect; it is worth naming because the README's claim that the
crate "can be moved out of the process without a caller changing" is true of
`PriceParams`/`PriceOutcome` and not of `Pricer`.

**Direction:** no change needed now; when a second pricer or a parallel worker
arrives, fold overrides into `price(&self, req, overrides)` (or a per-batch
session handle) so the trait matches the wire.

### m6. The request/outcome types are not serialisable or versioned

**Location:** `crates/geode-core/src/pricing.rs:176-205` (`PriceLine`,
`PriceParams`, `PriceOutcome`).

`PriceParams.submitted: Instant` (and the same field on `PriceOutcome`) is
process-local and has no wire representation; `PriceResult` is a fixed struct
of `price` + five named greeks with no version tag, so adding a greek is a
breaking change with no negotiation; `MarketOverrides` has only `spot` with
CVI/dividends noted as later fields. Keying and tagging are done well
(`QueryKey` + `u64` tag, and the "a lower tag cannot replace a higher one" rule
in `request-delivery.md:69`), which is the harder half.

**Direction:** when the seam is first crossed for real, replace `Instant` with
a monotonic submission id the tile already has (the tag) plus a `DateTime<Utc>`
if a timestamp is wanted, and give the results a named struct rather than the
`(u64, u64, Result<PriceResult, String>)` triple (see m11).

### m7. `PricerSettings::default()` duplicates the app's configured defaults

**Location:** `crates/geode-pricer/src/content.rs:158-168` (30 s refresh,
15 min `stale_after`) versus `crates/geode-app/src/bridge.rs:218-240`
(`pricing_refresh_from_config`, "using 30s") and the shell's `stale_after`.

Two independent statements of the same default in two crates; a change to one
is silently a divergence. The module's copy is only reachable through a
fixture, which is the argument for it being test-only.

**Direction:** make the module's default obviously a fixture (`#[cfg(test)]` or
a named `PricerSettings::for_tests()`), leaving `geode-app` the single
authority, as the composition-root rule intends.

### m8. `:spot`'s `clear` keyword is case-sensitive while its underlying is upper-cased

**Location:** `crates/geode-pricer/src/core/commands.rs:63-80`.

`["spot", und, "clear"]` matches the literal lower-case token, but the same
parser upper-cases the underlying, and every other pricer token is parsed
case-insensitively (`Template::parse`, `parse_barrier_kind`, `parse_expiry`,
`parse_strike`). `:spot SPX CLEAR` therefore falls through to the level branch
and refuses with "needs a positive level" rather than clearing. Same for
`:spot CLEAR` / `:refresh OFF` / `:refresh DEFAULT`.

**Direction:** lower-case the keyword position before matching, as the rest of
the grammar does.

### m9. `priced_at` paints a time on a package whose price is blank

**Location:** `crates/geode-pricer/src/core/sheet.rs` `fold_packages`
(`oldest` folding: `(Some(a), None) => Some(a)`), read with
`crates/geode-pricer/src/core/columns.rs:~351-355` (the `PricedAt` arm) and the
`complete` rule two blocks above.

A package with one unpriced leg gets `result = None` (correct: no sum) but
`priced_at = Some(oldest priced leg)`. The row then shows a `priced at` time
beside an empty `price`, which reads as "priced at 14:32, no price". The
`status` column does say `pricing…`, so it is not ambiguous overall, but the
time column is asserting something untrue of that row.

**Direction:** fold `priced_at` to `None` unless every leg has one — the same
`complete` gate the result already uses.

### m10. `find` and `completions` allocate the whole label/underlying set per keystroke

**Location:** `crates/geode-pricer/src/tile.rs:1949-1951` (`row_labels`,
a `Vec<String>` of every grid row via `r.tree.to_string()`), called from
`:585-637` (`find`, on every `FindEvent::Changed`) and `:1953-1973`
(`repeat_find`); `:2029-2043` (`completions`, a scan + sort + dedup of every
line's underlying per command-line keystroke).

Both are per-keystroke, not per-frame, so neither breaches the render rule —
but `row_labels` copies every `SharedString` into a fresh `String` (the
`find_match` signature wants `&[String]`) on each character typed into `/`.

**Direction:** have `find_match` take `&[SharedString]` or an iterator, or
cache the labels on the model (`GridModel` already owns them). Low priority.

### m11. `PriceOutcome.results` is an unnamed triple, and `deliver` clones it to re-pair

**Location:** `crates/geode-core/src/pricing.rs:198-205`;
`crates/geode-pricer/src/tile.rs:1242-1260`.

`Vec<(u64, u64, Result<PriceResult, String>)>` has a documented meaning
("`(id, revision, result)` per line") that a struct would carry in the type —
`PriceLine` already exists for the request side. The cost shows up in
`deliver`, which builds a parallel `Vec<(u64, u64)>` of every id/revision
purely so it can `zip` the `Delivered` answers back after `deliver_all`
consumed the results.

**Direction:** a `PriceAnswer { id, revision, result }` removes both the
comment and the clone; `deliver_all` could return
`Vec<(PriceAnswer, Delivered)>` or take a callback.

### m12. `RowKind::Underlying` and `Sheet::deliver` are carried but unreachable in production

**Location:** `crates/geode-pricer/src/core/sheet.rs:26-35` (`RowKind::Underlying`,
"Reserved for slice 2 ... Nothing in slice 1 constructs it; `from_rows`
refuses it"), with match arms in `sheet.rs` (`shorthand`), `columns.rs`
(`cell_text`), `clip.rs` (`spec_of`), `grid.rs` (`build`);
`sheet.rs:~300-310` (`Sheet::deliver`, the single-result form — the tile uses
`deliver_all`, so the only callers are tests); `tile.rs:190-196`
(`#[allow(dead_code)] frame`).

All three are deliberate and documented, and the `Underlying` arms are cheap.
Noting them together because they are the crate's whole dead-code surface and
the `Underlying` variant costs a wrong-looking arm in four files
(`RowKind::Line | RowKind::Underlying` reads as though the two behave alike).

**Direction:** leave as is, or drop `Sheet::deliver` and let tests call
`deliver_all` with one element.

### m13. Part 4's load lane is complete, tested and unreachable

**Location:** `crates/geode-pricer/src/store.rs:1-30` (the `Pending` contract),
`crates/geode-pricer/src/tile.rs:1331-1366` (`loaded` — **no callers** outside
this crate's tests), `crates/geode-pricer/src/content.rs` (`Delivery::Query(_)
=> {}`), `crates/geode-pricer/src/core/storage.rs:22-110`
(`PRICER_SHEETS_DECLARATION` — referenced only by `core/mod.rs:31`'s re-export);
`MemorySheetStore` is the only `SheetStore` anywhere
(`geode-app/src/bridge.rs:389,1229,1514`, `main.rs:1134`).

Honestly documented in features.md ("the sheet store is in memory until the
DuckDB store lands") and the README, and the `loaded`/`Pending` path has real
tests (`a_pending_load_paints_loading_until_the_rows_arrive`, the
`save_blocked` pair). Recording it so the next reader knows the dataset
declaration is not wired and `Delivery::Query` is the seam still to connect.

### m14. Mutation coverage is heavily concentrated on `tile.rs`

**Location:** `scripts/mutation-check.sh` — 74 pricer entries, distributed:
`tile.rs` 44, `sheet.rs` 5, `edit.rs` 4, `undo.rs` 4, `commands.rs` 4,
`paint.rs` 3, `popup.rs` 2, `storage.rs` 2, `shorthand.rs` 2, `cell.rs` 1,
`entry.rs` 1, `views.rs` 1, `header.rs` 1; **zero** for `grid.rs`,
`template.rs` (see M2), `tree.rs`, `clip.rs`, `columns.rs`, `session.rs`,
`store.rs`, `delegate.rs`.

`grid.rs` is the notable gap beside `template.rs`: the entry-placeholder
placement rules (`grid.rs:126-200`, including the closed-package fallback added
by a 2026-09-24 review fix) and `package_label`'s fallback are pure functions
with rich tests and no mutation entry proving those tests fail when the
behaviour breaks. `clip.rs`'s `put_place` (the "a package always lands at a
root boundary" rule) is the other.

**Direction:** add entries for the grid placeholder placement, `put_place`'s
root-boundary rule and `shift_cell`'s inherited/own distinction — each already
has a named test to point at.

### m15. Tests reach the tile through `TileContent::dispatch`, never through a keystroke

**Location:** `crates/geode-pricer/src/tile.rs:2577-2585` (the harness's
`dispatch`, used at 217 sites); zero `simulate_keystrokes` in the crate;
keymap resolution tested separately at
`crates/geode-pricer/src/content.rs:411-489` via a `Matcher` over a synthetic
context stack.

This is a defensible split — `TileContent::dispatch` *is* the shell's
production entry point, `simulate_input` is used for the text fields, and the
matcher test proves `y y`, `g p`, `d d`, `z shift+r` and the per-mode bindings
resolve. What no test covers is the join: that the tile's own
`key_context()`/`mode()` (`tile.rs:504-525`) put the real window in the state
the matcher test assumes, so e.g. the `shift+d`-would-duplicate-the-tile hazard
the `mode()` doc names is argued in a comment rather than pinned by a test.
Pointer routes, by contrast, *are* driven for real (`click_at`, the six
entry-placeholder click tests).

**Direction:** one end-to-end test per mode — a real keystroke into a hosted
tile with an open field, asserting a bare letter types rather than dispatching
— would close the argument. Lower priority than it sounds, since the shell's
own tests cover `occupant_insert_stack`.

### m16. Comments lean on archived spec and planning-decision numbers

**Location:** 111 `spec §` references, 40 `planning decision` references and 8
`ruling` references across `crates/geode-pricer/src`; e.g.
`tile.rs:190-196` ("Planning decision 6: arrive at every flip barrier at
once"), `columns.rs` ("the vocabulary is the spec's table in order"),
`sheet.rs:26-35` ("spec ruling 5").

CLAUDE.md: "A code comment should state the local invariant and failure it
prevents; it should not require a task number or spec section to make sense."
Most of these *do* state the invariant and then cite — which is fine and even
useful. A minority are citation-only (`Applies`/`COLUMNS` ordering, several
`planning decision N` tags with no restated reason), and those become
unreadable once `docs/phase-history.md` is the only home for the number.

**Direction:** on the next pass through, keep the invariant sentence and drop
the bare numbers where no invariant accompanies them. Not worth a dedicated
change.

### m17. `rebuild_chrome` clones the settings and re-scans the sheet three times per verb

**Location:** `crates/geode-pricer/src/tile.rs:2179-2216` (`settings.borrow().clone()`),
`crates/geode-pricer/src/header.rs:76-127` (`prepare`: a `stale_lines().count()`
scan, a `(0..len)` failed scan, a `(0..len)` `priced_at` max scan, plus an
eagerly built `time_stale` string).

`rebuild_chrome` runs at the end of nearly every verb and every rebuild; the
clone copies two `String`s, and the three scans are O(n) each. Trivially cheap
at current sizes and clearly outside render — recording it only because it is
the one place the chrome path does avoidable work, and because `time_stale` is
formatted whether or not the sheet is stale.

**Direction:** borrow rather than clone the settings; fold the three scans into
one pass; build `time_stale` at render (it is already a per-frame `bool`).

---

## Ideas

### i1. Let the leaf echo what it priced

`PriceOutcome` carries only numbers. If it also echoed the resolved expiry
date, the resolved strike level and the reference spot each line was priced
against, C1 disappears (the app never resolves anything), the `priced at`
column could show the library's own valuation time rather than the app's
`Utc::now()` (`tile.rs:1256`), and a trader could see *why* two lines differ.
It also gives the seam a natural place for a version/capability field (m6).

### i2. A `shell::gridtile` door for the tile's mechanical half

Between this crate and market-data there are now two implementations of:
cursor-by-row-identity with a last-position fallback, a prepared-model swap
through one `install_model`, a per-theme paint memo floored against hover and
selected grounds, a `⋯` capture-phase header trigger, a footer that shows the
verb error or the cursor row's failure, and the click-anchor off-by-one fix.
That is a tile *kind*, not a coincidence. Extracting it would make the next
grid module a week's work instead of a month's, and would make the pricer's
`tile.rs` roughly a third its size.

### i3. Name the six seams inside `tile.rs`

At 5,236 lines (≈2,400 production, ≈2,800 tests in the same file) the file has
clean internal boundaries already: the editor (`:823-1107`, `:2140-2177`), the
menu (`:1764-1896`), the pricing lane (`:1158-1330`), persistence
(`:1119-1156`, `:1331-1383`), the cursor (`:1898-1947`, `:2218-2284`), and the
pointer (`:2286-2364`). Splitting along them — as `geode-timeseries` already
does with `tile/popups.rs` — would also let the tests split, which matters
because the memory notes a 10k-line file stalls a subagent.

### i4. `:name` would make the sheet store usable

`commands.rs:33` lists `e`, `name`, `new`, `rm` as parsed-and-refused, so
every sheet is `untitled-N` unless a session record names it, and the store is
keyed by name. The load/save/`untitled-N`/open-set machinery is all built and
tested; `:name` is a rename plus a store move, and it is what turns the whole
persistence tier into a feature a trader can use. Documented as a gap, but it
looks like the highest value-per-line item left in the module.

---

## Systemic patterns

**Two-authority drift.** The recurring defect shape is one rule stated in two
places that can disagree: `menu_items` vs `dispatch` on what is allowed while
loading (M7), the channel-refusal path vs the worker-refusal path (M1),
`PricerSettings::default` vs `bridge.rs` (m7), the app's expiry calendar vs the
library's (C1). Each is individually small; together they argue for deriving
enabled-state and defaults from one function rather than restating them.

**Correct-but-copied.** M4 and M5 are the same pattern at module scale: the
sibling-dependency rule (correct) plus no shell door (missing) yields a third
correct copy. The pricer consistently re-derived the better version of the
idiom, which is the strongest possible argument for hoisting it — the newest
copy is the one worth keeping.

**Linear scans behind an identity API.** `index_of`, `has_id`, `grid_row_of`
and `Expansion`'s `names_package` are all O(n) lookups by id, called from
loops. Keying by identity rather than index is exactly right (it is what makes
the cursor and the editor survive deliveries); it just needs an index.

**Documented gaps are genuinely documented.** Every limitation I found by
reading was already stated in features.md, the README or a comment — the
in-memory store, the pixel widths, the unbound `:name`, the menu's hardcoded
key hints, the missing patch path. The docs are a reliable map of this crate,
which is rare and worth saying.

## What is done well

- **The leaf boundary is airtight and mechanically enforced.** `geode-pricing`
  depends on `geode-core` alone; nothing but `geode-app` names it; the
  prohibition is written into `geode-pricer/Cargo.toml:16` where a future
  `cargo add` would have to read it. `PricerRegistry` mirrors `AdapterRegistry`
  so a missing pricer fails every line with a recovery sentence rather than
  failing startup (`PricerConfig::missing_reason`, surfaced in the header with
  "set [pricing] adapter and restart"). This is Philosophy §1 implemented, not
  merely asserted.
- **The worker is defensive in exactly the right places.** `catch_unwind` +
  `panic::contained` around both `set_overrides` and `price`; a panic is one
  line's error; a refused override fails the whole batch because "a line priced
  against the wrong data source is worse than no price"; cancel checked at each
  line boundary with the already-priced lines still delivered; latest-wins per
  key in place. Sixteen tests, one per rule.
- **The undo contract is stated, enforced and mutation-guarded.** "Every
  inverse is recorded against the exact rows the edit left" is the kind of
  invariant that is usually discovered by a bug; here it is in the module doc,
  enforced by routing every mutation through `apply`/`apply_edits`, protected
  by clearing the history when `Sheet::undo` fails, and pinned by
  `apply_then_undo_is_identity_for_every_edit` plus four mutation entries.
  `loaded` clearing the stack because the inverses were recorded against the
  fallback sheet is a subtle catch.
- **Staleness and failure are never colour-only.** Both bundled views end in a
  `status` column that spells `pricing…` or the failure reason; the header
  counts both; a failed cell paints `—` *and* the row's reason reaches the
  footer. Philosophy §3 taken literally.
- **The paint memo and its readability sweep.** Per-theme derivation via
  `observe_global::<Theme>`, floored against every ground a row can wear
  (its own, hover, selected) because flooring against one left 23 themes
  unreadable, with a no-exception-list sweep over every bundled theme. The
  `floor_toward_pole` fix — a fixed anchor gives the bisection nothing to move
  toward on a monochrome theme — is the sort of thing only a real sweep finds.
- **The click-anchor off-by-one is solved properly.** The entry placeholder is
  a grid row, so closing it slides the rows below up under a stationary
  pointer. Resolving the row to a `LineId` *before* any close, and handing the
  first press's line to the next press's `DoubleClickedCell` only, is the
  correct fix rather than a deferral hack — and six tests cover chevron, cell,
  placeholder, double-click, tree-column and repeat-click variants.
- **The typeahead equality rule.** `enter` takes the highlighted underlying
  only when the query equals it case-insensitively or the highlight was moved
  by key or click, and a *hover* deliberately does not count as moving it
  (`HSI` must not commit `HSCEI`). That is a real trading-desk failure mode
  reasoned about before it happened.
- **The column fit test.** `delegate.rs`'s test computes worst-case text width
  from the actual formatter at the largest font size inside the real cell
  padding and cursor border, on the stated grounds that a right-aligned
  overflow drops *leading* digits — a plausible-wrong number. Deriving the
  check from the failure mode instead of eyeballing the widths is exemplary.
- **Test-feature parity and the bench discipline.** Dev-dependencies mirror the
  workspace's `geode-*` features with a comment explaining why; `bench = false`
  on the lib; Criterion `harness = false`; benchmarks cover the parse, edit,
  delivery, storage round trip and grid build paths with `iter_batched` where
  the operation mutates.
