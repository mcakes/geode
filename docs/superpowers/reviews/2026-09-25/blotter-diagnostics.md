# Review: `geode-blotter` and `geode-diagnostics`

Read-only review, 2026-09-25. Scope: `crates/geode-blotter` (10.8k lines) and
`crates/geode-diagnostics` (3.3k lines), including benches and tests. Judged
against `docs/PHILOSOPHY.md`, `CLAUDE.md`, both crate READMEs,
`docs/current/{features,shell,performance,data-path}.md`, and the gpui-kit
Coding Guides.

## Summary

1. Both crates are in genuinely good shape: the pure/paint split is real, the
   format cache and glyph cache keep `render_td` a lookup, and the barrier and
   refusal seams are handled honestly at every site I checked.
2. The most serious finding is a correctness gap the memory index already
   suspected: `SortSpec.column` is a plan index that `move_column` does not
   remap, so dragging a column silently moves the sort onto a different column.
3. Two narrower correctness gaps: `sort_siblings`' NULL-last rule inverts for
   equal-severity pairs in a way that is subtly inconsistent, and the blotter's
   header can show a stale grouping label after a refused requery.
4. GPUI hygiene is strong but not uniform: `render` still formats per frame in
   three places (freshness rows, footer counts, grouping label), and both tiles
   use index-derived `ElementId`s for rows.
5. Architecture-wise, five modules now hand-roll the same frame-follower,
   stale-marker, header-strip and `AppClock` reading; that is the clearest
   candidate for promotion into the shell or `geode-widgets`.

---

## Critical

### C1. `move_column` does not remap `SortSpec.column`, so a drag moves the sort onto another column

**Location:** `crates/geode-blotter/src/core/plan.rs:145-153`
(`ColumnPlan::move_column`), `crates/geode-blotter/src/delegate.rs:857-871`
(`TableDelegate::move_column`), `crates/geode-blotter/src/core/flatten.rs:106-111`
(`SortSpec.column` is documented as "Index into `ColumnPlan::columns`").

**What and why.** `SortSpec.column` is a positional index into
`plan.columns`. `ColumnPlan::move_column` does a `remove`/`insert` on that
vector, which shifts every index between `from` and `to`, and the delegate's
`move_column` hook (delegate.rs:857) calls it and then only invalidates the
format cache — it never touches `self.sort`. So after a drag, a sort that was
on `delta01` is still recorded at the old position, which now holds a different
column. The rows on screen do not change (nothing reflattens), so the mismatch
is invisible until the next reflatten (an expand, a delivery, a sort cycle),
at which point the siblings silently reorder by a column the trader never
named. `column()` (delegate.rs:754) also paints the arrow on whichever column
now sits at that index, so the header actively asserts the wrong thing.
This is the "plausible wrong answer" failure class CLAUDE.md ranks above an
explicit error.

**Impact.** Silent wrong ordering plus a header that lies about it. A trader
sorting by the biggest exposure, then reordering columns for a screenshot, gets
rows ordered by something else with no indication.

**Suggested direction.** `ColumnPlan::move_column` should return the index
mapping (or take `&mut Option<SortSpec>`), and the delegate's hook should remap
`self.sort.column` through it before invalidating. The same remap is needed for
`cursor.col`, which has the identical bug in a milder form: the cursor stays on
a position, not on the column the trader was looking at, so a subsequent `s`
cycles the sort on a different column than the one under the highlight.
A test should drag a column and then assert both the painted arrow and the
resulting row order.

### C2. A sort on a column the plan rebuild drops keeps sorting by position

**Location:** `crates/geode-blotter/src/delegate.rs:398-404` (`apply_snapshot`).

**What and why.** On a plan rebuild the sort is kept whenever its index is
still in range:

```rust
self.sort = self
    .sort
    .filter(|s| s.column < self.plan.as_ref().unwrap().columns.len());
```

That is a bounds check, not an identity check. A view edit that hides a column
(plan.rs:72-76 drops hidden columns from the plan) or a regroup that folds a
dimension into the tree column (plan.rs:79-84 `continue`s on a grouping
dimension) shortens `columns` and shifts everything after the removed entry
down by one — so a surviving in-range `SortSpec` now names a different column,
and `reflatten_keeping` (called at the end of `apply_snapshot`) immediately
reorders the siblings by it. Unlike C1 the rows *do* change here, in the same
frame, with no marker.

**Impact.** Hiding a column or regrouping silently re-sorts the blotter by an
unrelated column. Same wrong-totals-adjacent severity as C1: the numbers are
right, the order is a lie, and the header arrow agrees with the lie.

**Suggested direction.** Carry the sort by column *name* rather than plan
index (the `:sort` command already resolves a name to an index at
tile.rs:1176-1191, so the name is the natural identity), and re-resolve it
against the fresh plan on every rebuild — dropping the sort with a notice when
the column is gone. That also fixes C1 for free.

---

## Major

### M1. `sort_siblings` inverts its own NULL-last rule for the `Equal` case and for text

**Location:** `crates/geode-blotter/src/core/flatten.rs:188-243`
(`sort_siblings`, `number_at`, `is_null`).

**What and why.** The comparator computes `ord` treating NULL as
greater-than-everything (`(Some(_), None) => Less`, `(None, Some(_)) =>
Greater`), then reverses for descending *except* when either side is NULL:

```rust
match (spec.order.descending(), ord) {
    (_, Ordering::Equal) => Ordering::Equal,
    (true, o) if is_null(..a) || is_null(..b) => o,
    (true, o) => o.reverse(),
    (false, o) => o,
}
```

The intent ("NULL last in both directions") is right, but the implementation
is not a consistent total order when *both* sides are NULL and the values are
otherwise comparable — `(None, None) => Equal` falls through the first arm, so
two NULLs tie, which is fine — however the carve-out is evaluated per *pair*,
not per element. For a three-way comparison of `[5, NULL, 3]` descending:
`5 vs NULL` → `Less` (kept, NULL last), `NULL vs 3` → `Greater` (kept, NULL
last), `5 vs 3` → `Greater` reversed... wait, `5 vs 3` is `Greater`, reversed
to `Less`. So 5 before 3 descending — correct. The order is in fact consistent
for this shape. Where it is *not* obviously consistent is the `Equal` arm
short-circuiting before the NULL carve-out: two rows whose values compare
`Equal` but where one is NULL and one is a real zero cannot both happen
(`is_null` implies `None` from `number_at`, and `None` vs `Some` never yields
`Equal`), so that is safe too.

**The real defect is narrower and verified:** `is_null` (flatten.rs:245-252)
is called with the *numeric* flag from the enclosing scope but re-reads the
snapshot twice per comparison per element — `number_at` runs inside `ord`
already, then again inside `is_null` for the descending branch. On a measure
column that is four snapshot reads per comparison instead of two, inside an
`O(n log n)` sort that `docs/perf.md:295-299` measures on the keypress path.

**Impact.** Doubled comparator cost on every sorted reflatten. Not a
correctness bug, but it sits on a path the performance guide names, and the
NULL rule is expressed in a way that is very hard to review for total-order
safety — which matters because `sort_by` panics on an inconsistent comparator
(the file's own `number_at` doc cites that hazard for NaN).

**Suggested direction.** Decorate-sort-undecorate: map each row to
`(is_null, key)` once, then sort by that tuple with the direction applied only
to `key`. That makes "NULL last in both directions" structural rather than a
per-pair carve-out, halves the snapshot reads, and removes the need for a
reader to reason about pair-wise consistency at all.

### M2. The header's grouping label and title survive a refused requery, claiming a grouping the rows are not at

**Location:** `crates/geode-blotter/src/tile.rs:712-799` (`requery`),
specifically 751-753 (`last_grouping`/`title` assigned before the submit) and
767-784 (the refusal branch); painted at tile.rs:1544 and
tile.rs:868-874.

**What and why.** `requery` sets `self.last_grouping = grouping.clone()` and
recomputes `self.title` *before* calling `self.data.query(...)`. When the
submit is refused, the branch at 767 sets an error, clears `in_flight`, answers
the barrier and clears `acted` — but leaves `last_grouping` and `title` at the
grouping that was never queried. `render` paints
`GroupingSlots::label_of(&self.last_grouping)` (tile.rs:1544) beside the view
name, and `title()` feeds the stack list and tile chrome. So after a refusal
the header reads `lhu / underlying_ref` while the rows on screen are still the
previous grouping's. The error chip does appear beside it (tile.rs:1663-1669),
so the trader is told *something* is wrong, but the two readings disagree and
the grouping label is the one that looks authoritative.

Same shape applies to `:group`/`:view` (tile.rs:1109-1135, 1163-1170): the pin
and `view_name` are committed before `requery`, so a refused submit leaves the
header naming a view whose rows never arrived.

**Impact.** Ambiguity about what is on screen, which
`docs/PHILOSOPHY.md:47-52` names as never acceptable ("Known-stale data,
clearly marked, is acceptable; ambiguity never is"). The failure window is a
full data-service queue during a burst, exactly when a trader is most likely
to be reading fast.

**Suggested direction.** Either assign `last_grouping`/`title` only on `apply`
(the delivery that paints), keeping a separate `pending_grouping` for the
in-flight question, or have the refusal branch roll both back. The former is
cleaner and matches how `apply` already owns "what is painted".

### M3. `render` formats per frame in three places, against the crate's own stated rule

**Location:** `crates/geode-blotter/src/tile.rs:1620-1640` (per-dataset
freshness), 1682-1697 (footer counts and the semi-join legend), 1544 (the
grouping label).

**What and why.** The tile is careful about this elsewhere — `asof_chip`,
`asof_tip`, `filter_tip`, `title` and the three tooltip selectors are all
cached fields with documented single assignment sites, precisely so `render`
clones a `SharedString` instead of formatting. But three sites still format
on every frame:

- freshness: `datasets.sort_by(...)` allocates a `Vec` of references and sorts
  it, then `format!("{} {}", f.dataset, short_time(t, clock))` per dataset,
  where `short_time` itself parses RFC 3339 and formats (tile.rs:1427-1434);
- footer: `format!("{} rows", delegate.shown.len())`, plus
  `format!("⋈ scoped by membership on {}...", delegate.semi_joined.join(", "))`
  which allocates a joined `String` per frame;
- header: `GroupingSlots::label_of(&self.last_grouping)` builds a `String`
  (groupings.rs:118) on every frame even though `self.title` already caches the
  same label.

`docs/current/performance.md:100-104` states the contract as "A blotter formats
the visible window into a cache rather than formatting in `render_td`" — these
are not `render_td`, so they are within the letter of that line, but
`PHILOSOPHY.md:88-90` ("per-frame heap churn is a reviewable defect") and the
Coding Guides' Performance rules ("Avoid cloning large strings or collections
solely to satisfy a closure") both reach them. Note the freshness path also
parses a timestamp per dataset per frame, which is the most expensive of the
three.

**Impact.** Bounded (a handful of datasets, two footer strings) but on the
8 ms frame path, and it recurs on every frame while idle-repainting, not only
on change. It also undercuts the file's own carefully-documented caching
discipline, which makes the next reader unsure which rule applies.

**Suggested direction.** Prepare the freshness rows and the footer strings in
`apply` (the one place a snapshot lands) into a small `Vec<(SharedString,
bool)>` and two `SharedString`s, and read `self.title`'s grouping half for the
header label. The stale flag must stay a per-frame *compare* (it depends on
`now`), which is what the pricer header already does
(`crates/geode-pricer/src/header.rs:1-4`: "compares the last priced time
against `stale_after` (a compare, never a format, per frame)").

### M4. Row and chevron `ElementId`s are derived from the visible index, which is a mutable position

**Location:** `crates/geode-blotter/src/delegate.rs:927` (`id(("row",
row_ix))`), 1030 (`id(("chevron", row_ix))`), 1017 and 1058 and 1073
(`debug_selector` on the same indices);
`crates/geode-diagnostics/src/tile.rs:606` (`uniform_list("diagnostics-rows",
...)` with rows addressed by `i`).

**What and why.** The Coding Guides' Stable identity section is explicit:
"Never derive identity from a translated label or a mutable list index when
items can be inserted or reordered", and `CLAUDE.md` repeats it ("Repeated
elements use stable domain-derived IDs"). `row_ix` here is a position in
`shown`, which is rebuilt by every expand, collapse, sort, narrow, regroup and
delivery — so the same `ElementId` addresses a different domain row from one
frame to the next. The blotter has a real domain id available: the tree path
(`path_of`) or at minimum the snapshot row id `shown[row_ix]`, which is already
in hand at both call sites.

What this costs in practice is narrow, because the only element-local state
keyed off these ids is the chevron's hover/pressed state — so a reflatten under
the pointer can leave a hover highlight on a row that scrolled away, and a
press begun on one row can complete as a click credited to whatever row now
occupies that index. I did not find a path where that produces a wrong toggle
(the `on_click` closure captures `row_ix` by value and the listener re-reads
`shown` through `toggle_row`, so it acts on the *current* occupant of that
index, which is at least self-consistent), so this is Major-on-principle
rather than a demonstrated wrong action.

**Impact.** Hover/press state can attach to the wrong row across a reflatten;
more importantly it is a standing violation of a rule the project states twice,
in the one crate most likely to be copied as the template for the next table.

**Suggested direction.** Key both on the snapshot row id: `id(("row",
shown[row_ix]))`. Keep the `debug_selector`s on the visible index if tests
address them positionally, since those are compiled out in release — but say so
in a comment, because the mismatch between the two identity schemes on the same
element will otherwise read as an oversight.

### M5. Five modules hand-roll the same frame-follower, and the blotter is the most elaborate copy

**Location:** `crates/geode-blotter/src/tile.rs:555-607` (`versions`,
`watch_view`, `follows_changed`, `differs_on_followed`), 608-711
(`on_frame_changed`, `promote`), 767-784 (refusal → `arrived` → clear `acted`);
compare `crates/geode-marketdata/src/tile.rs:1176-1224` and 1993-2001,
`crates/geode-timeseries/src/tile/data.rs`, `crates/geode-pricer/src/tile.rs`,
and `crates/geode-diagnostics/src/tile.rs:163-235`.

**What and why.** `barrier_wants` appears in five module files plus the shell;
`staged` in four; the string "is busy or gone" in four; `try_global::<AppClock>`
in five. The *policies* differ legitimately per module (which counters a tile
follows, whether it stages), but the *mechanism* is identical: read
`versions_for(&watches)`, compare against `acted`, requery or answer the
barrier, stage until `flip`, promote with a version re-check, and on a refused
submit answer-then-clear-`acted`. Each copy has been fixed separately — the
blotter's doc comments cite "fix round 1, Finding 1" and "I-1 (final
whole-branch review)" for the staging gate (tile.rs:672-687), and the
marketdata copy cites "review fix round 1, MIN-3" for the same refusal rule
(marketdata tile.rs:1211-1215). The memory index records the health seam being
"fixed five times"; this is the same pattern one layer up.

**Impact.** Four more places for the next barrier subtlety to be fixed in
three of four files. `PHILOSOPHY.md:66-70` ("A module never invents its own
navigation idiom, its own data-fetch path") is aimed exactly here.

**Suggested direction.** A `shell::follower::Follower` owning `acted`,
`staged`, `publications`, `tag` and `in_flight`, parameterised by a
`Follows { scope: bool, grouping: bool, as_of: bool }` policy the tile
supplies, exposing `should_requery(cx) -> bool`, `submitted(versions)`,
`refused(cx)`, `deliver(outcome) -> Delivered::{Apply, Staged, Dropped}` and
`flip(cx) -> Option<T>`. Each tile keeps its own `apply`. This is a large
refactor; the cheap first step is to move the refusal triple
(`arrived` + clear `acted` + notice) into one shared helper, since that is the
seam with the worst history.

---

## Minor

### m1. `is_stale` silently treats an unparseable freshness stamp as fresh

**Location:** `crates/geode-blotter/src/tile.rs:1382-1394`.

`is_stale` is `as_of.and_then(parse_from_rfc3339(..).ok()).is_some_and(..)`, so
both "no as-of" and "an as-of I cannot parse" answer `false` — not stale. The
companion `short_time` (tile.rs:1427-1434) deliberately echoes an unparseable
stamp whole rather than slicing (a real past panic, per its comment), so the
trader sees the garbage text — but with no warning colour, because `is_stale`
said fresh. A malformed source time therefore reads as current data.
Direction: return `true` (or a third `Unknown` state painted as a warning) when
a present stamp fails to parse. Silence about a value we cannot interpret is the
bug PHILOSOPHY §3 names.

### m2. The `…` in-flight affordance can stick until an unrelated repaint

**Location:** `crates/geode-blotter/src/tile.rs:786-796`, painted at 1655-1660.

`requery` spawns a timer that notifies once at `IN_FLIGHT_AFTER + 10ms` if
still in flight, and `render` shows `…` while
`in_flight.elapsed() > IN_FLIGHT_AFTER`. That is one shot: if the query is
still outstanding after the notify, the glyph paints correctly, but nothing
schedules a further repaint, and there is no timer at all for *removing* it —
removal rides `deliver`'s `cx.notify()`, which is fine. The gap is the reverse
case: a query that takes 40 ms shows nothing (correct), and one that takes
60 ms shows `…` from the 60 ms notify onward (correct). So the one-shot is
actually sufficient. What is *not* covered is a tile hidden and re-shown while
in flight — `set_visible` (tile.rs:853-861) may call `requery` again, which
re-arms, so that is covered too. Downgrading this to a note: the logic is
correct but the reasoning is non-obvious and undocumented, unlike every other
subtlety in the file. Direction: one comment stating why a single shot
suffices.

### m3. `restore_by_path`'s outward search has an adversarial O(n) path on the keypress thread

**Location:** `crates/geode-blotter/src/core/cursor.rs:100-158`; bench at
`crates/geode-blotter/benches/blotter.rs:150-190`
(`restore_by_path_729k_rows_far_fallback`).

The optimisation (O(1) depth reject, then search outward from the previous
cursor row) is well-reasoned and well-documented, and the bench honestly
records the adversarial case. But the adversarial case is not exotic: in the
729k fixture 720,000 of 720,881 rows share the target's depth, so the depth
reject filters nothing and every step allocates a `Path` (a `Vec<Option<String>>`
plus a `String` per ancestor) — and `gg`/`G`, which move the cursor the full
length of the list, are exactly the "fallback is far from the target" shape.
The bench measures a `fallback = 0` search *for a target at the end*, which is
the worst case, but a `G` on a fully-expanded deep tree reaches it.
Direction: nothing urgent (the measured median is recorded and within budget
for the nearby case), but the path deserves either a cheap
`(depth, tree_text)` pre-filter before the full `path_of`, or an explicit note
in `docs/perf.md` that `G` on a fully-expanded million-row tree is the known
worst case.

### m4. The visual-mode selection highlight is recomputed per row per frame

**Location:** `crates/geode-blotter/src/delegate.rs:918-929` (`render_tr`).

`render_tr` calls `selection(&self.mode, &self.cursor)` for every visible row,
constructing a `Range` and testing containment. That is cheap (no allocation),
but it is per-row work for a value that changes only when the mode or cursor
does. Direction: leave it; noted only because the file is otherwise
scrupulous about exactly this class of thing, so the inconsistency reads as an
oversight rather than a judgement.

### m5. `render_th` calls `self.column(col_ix, cx)` to get a label it then clones

**Location:** `crates/geode-blotter/src/delegate.rs:896-917`, calling
`column()` at 750-806.

`column()` builds a full `Column` value — cloning `c.name` into a
`SharedString` key, cloning `c.label` into a `String`, pushing ` ⋈` and
` |x|` onto it, and computing the gutter width — and `render_th` uses only
`.name`. So every header cell paints by allocating two strings and a `Column`
struct per frame. The label composition (`⋈`, `|x|` suffixes) is itself
per-frame string building.
Direction: compose the display label once per plan/sort change into a
`SharedString` on `PlannedColumn`, and have both `column()` and `render_th`
clone it.

### m6. `visible_texts` / `shown_texts` allocate a `Vec<String>` per find keystroke

**Location:** `crates/geode-blotter/src/delegate.rs:545-580`, called from
`crates/geode-blotter/src/tile.rs:1291-1299` and 1247.

Every `/` keystroke builds a fresh `Vec<String>` over *every* visible row
(`visible_texts` under fzf — the whole flatten, up to 720k rows on the
fully-expanded fixture) with one `String` allocation per row, then
`filter_matches` walks it. `n`/`N` do the same through `shown_texts`.
`geode_shell::vimfind::{find_match, filter_matches}` take `&[String]`
(vimfind.rs:236, 332), so the allocation is forced by the shared signature.
Direction: widen the `vimfind` helpers to `&[impl AsRef<str>]` or an iterator
so the blotter can pass borrowed `&str`s straight out of the snapshot's
dictionary arrays. This is the single largest per-keystroke allocation I found
in either crate, and it is on the interactive path PHILOSOPHY §3 governs.

### m7. Diagnostics source ages are sampled at rebuild and never tick

**Location:** `crates/geode-diagnostics/src/sections.rs:116-121`
(`format!(" · {}s ago", d.as_secs())` from the rebuild-time `now`).

Documented as a limitation in both the crate README ("Source ages are sampled
at rebuild time; they do not tick while a source is quiet") and
`docs/current/features.md:219-221`, and the absolute timestamp stays beside it,
so this is honest rather than hidden. But the displayed number is a
freshness claim that decays: a source quiet for an hour reads `· 3s ago` until
something unrelated rebuilds. The mitigation (absolute time beside it) means
the trader *can* tell, which is why this is Minor and not Major.
Direction: either drop the relative age (the absolute time is the honest
value) or rebuild the sources section on a coarse timer while visible. The
former is cheaper and strictly more honest.

### m8. The diagnostics data section's `zo`/`zc` target is recovered by re-parsing painted text

**Location:** `crates/geode-diagnostics/src/tile.rs:431-444`
(`nearest_header_name`), consuming the format written at
`crates/geode-diagnostics/src/sections.rs:200-222`.

`nearest_header_name` finds the dataset a collapse applies to by scanning
backward for a `depth == 0 && collapsible.is_some()` row and then
`r.text.split_once(": ")`. The comment defends this ("this tile owns the exact
format `sections::data_rows` writes"), and it is true that both live in one
crate — but the coupling is invisible from the writing side: a dataset name
containing `": "` splits wrong, and an edit to the header format in
`sections.rs` breaks collapse with no compile error and no test that pairs the
two. The `Row` struct already carries `collapsible: Option<bool>`, so it has a
place to carry the key.
Direction: add `key: Option<SharedString>` (or make `collapsible` carry the
name) so the target is data, not a reparse.

### m9. Diagnostics filters allocate the full row set before discarding most of it

**Location:** `crates/geode-diagnostics/src/sections.rs:316-324` (config) and
370-396 (log).

Both builders format *every* row and then `filter(|r| r.text.contains(filter))`
— for config, up to `MAX_LEAVES_PER_DOC` (2,000) leaves per document, each
with a `format!` and an `explain` lookup, before the filter drops them. The
2,000 cap and the "traversal still visits all leaves" behaviour are documented
(README Limits, features.md:220-222), but the *formatting* of discarded rows
is not, and it is the expensive half (`config.explain(doc_name, path)` per
leaf). A filter that matches ten rows still pays for two thousand.
Direction: test the filter against the path/value before formatting, and
before calling `explain`.

### m10. `Row::text` is `SharedString` but rebuilt wholesale on every filter keystroke

**Location:** `crates/geode-diagnostics/src/tile.rs:513-530` (`find`), 237-325
(`rebuild`).

Every `FindEvent::Changed` calls `rebuild`, which re-walks the source data and
re-formats every surviving row into a fresh `Rc<Vec<Row>>`. For the config
section that is the full document walk plus up to 2,000 `explain` calls per
keystroke. The `Rc` sharing with the paint closure (tile.rs:602) is good and
tested (`rebuilding_does_not_reallocate_rows_between_paints`), but it only
covers paint-to-paint, not keystroke-to-keystroke.
Direction: build the unfiltered rows once per input change and filter that
cached vector per keystroke — which also fixes m9.

### m11. `chip_paint` is called per render and does contrast work for the text tones

**Location:** `crates/geode-blotter/src/tile.rs:1516-1520`,
`crates/geode-diagnostics/src/tile.rs:582-584`; implementation at
`crates/geode-shell/src/shell/chip.rs:94-126`.

`Tone::WarningText` and `Tone::DangerText` route through `floored_text`, which
does `to_rgb`, `readable_on` (an OKLab lightness search) and `to_hsla` — per
render, in both tiles. The blotter's own delegate takes this seriously for the
chevron (`chevron_states`, delegate.rs:282-301, memoised behind
`control::ControlInputs`) and for the named-colour pair (`ensure_theme_inputs`,
delegate.rs:263-281, memoised behind a 28-value signature), with doc comments
explaining exactly why. The tile-level chip reads have no such memo.
Direction: memoise the four chip paints per theme the way the delegate
memoises its two, or give `chip` its own per-theme memo so every caller
benefits (there are eight callers across the workspace).

### m12. `ColourCache::misses` is a test-only accessor on a production type

**Location:** `crates/geode-blotter/src/colour_cache.rs:63` (the `misses`
field), 119-125 (`pub fn misses`).

The counter is incremented in production (`colour_cache.rs:96`) and read only
by tests — the field's own doc says "Test hook". It is a `u64` per delegate, so
the cost is nil, but it is production state that exists for tests without
being `#[cfg(test)]`-gated, unlike `BlotterDelegate::glyph_at` and
`gutter_text` (delegate.rs:355-367, 684-698) which are correctly gated in the
same crate.
Direction: gate the field and accessor, or state in the doc why it is
ungated (e.g. so an integration test in another crate can read it).

### m13. `ColumnPlan::same_columns` is dead

**Location:** `crates/geode-blotter/src/core/plan.rs:162-173`; its only other
reference is the assertion in its own test (plan.rs:357).

`apply_snapshot` deliberately rebuilds the plan unconditionally and compares by
value instead (delegate.rs:390-404, and the README pins that: "Do not reinstate
a cheaper gate"). `same_columns` is the cheaper gate that was removed. Keeping
it invites exactly the reinstatement the README forbids.
Direction: delete it, or move it under `#[cfg(test)]` with a comment pointing
at the README rule.

### m14. `BlotterTile::last_query` clones a `Vec<String>` and is unused in production

**Location:** `crates/geode-blotter/src/tile.rs:439-441`.

`pub fn last_query(&self) -> Option<(u64, Vec<String>)>` clones
`last_grouping` on every call; grep finds no production caller in the blotter,
the app or the shell. Direction: gate it to tests or drop it.

### m15. Comments cite task numbers, review rounds and findings rather than invariants

**Location:** pervasive in `crates/geode-blotter/src/tile.rs` (56 matches for
`Phase N` / `Task N` / `review round` / `Finding N` / `fix round` / `I-N`) and
`crates/geode-blotter/src/delegate.rs` (12); also
`crates/geode-blotter/src/core/mod.rs:1-2` ("Each module lands with the Plan 3c
task that needs it") and the `Cargo.toml` dependency comment
(`crates/geode-blotter/Cargo.toml:19-20`, "The restored-filter-parse-failure
site (Phase 4b Task 2)").

`CLAUDE.md` is explicit: "A code comment should state the local invariant and
failure it prevents; it should not require a task number or spec section to
make sense." Most of these comments *do* state the invariant and the failure —
they are unusually good comments — but they lead with the provenance
(`// Fix round 1, Finding 1:`, `// I-1 (final whole-branch review):`), which is
archive material per the Documentation maintenance section. By contrast
`geode-diagnostics` has zero such citations and reads cleanly.
Impact: the blotter is the crate a new reader is most likely to open, and its
comments read as a changelog. Direction: strip the provenance prefixes, keep
every sentence that states a rule or a failure. This is a mechanical pass over
about 70 comment blocks.

### m16. `tile.rs` is 5,044 lines with a 240-line `render` and a 1,300-line `dispatch`/`command` pair

**Location:** `crates/geode-blotter/src/tile.rs` — `render` at 1474-1714,
`dispatch` at 952-1106, `command` at 1108-1209; tests from 1732 to the end
(3,300 lines, two thirds of the file).

Every sibling module has already split this up: marketdata has `header.rs`,
`popup.rs`, `delegate.rs`, `core/`; pricer has `header.rs`, `grid.rs`,
`paint.rs`, `popup.rs`, `store.rs`, `session.rs`; timeseries has `header.rs`,
`popup.rs`, `tile/{mod,data,tests}.rs`. The blotter alone keeps the whole
tile — frame following, requery, command handling, header paint, footer paint
and every test — in one file. The memory index records that a 10k-line file
stalls a Sonnet-class agent mid-move, which makes this a practical cost, not
just a style one.
Direction: split along the seams the siblings already chose — `header.rs`
(the header strip and its chips, plus the freshness rows m3 wants prepared),
and `tile/tests.rs`. That alone removes ~3,500 lines from the file.

### m17. `Pin::Slot` restoration indexes the restored table twice with `unwrap`

**Location:** `crates/geode-blotter/src/tile.rs:256-266`.

```rust
Some(t) if t.get("pinned_slot").and_then(|v| v.as_integer()).is_some() => {
    Pin::Slot(t["pinned_slot"].as_integer().unwrap() as u8)
}
```

The guard makes the `unwrap` sound, but it reads the key twice and panics if
the two reads ever diverge (they cannot today). More substantively, the
`as u8` truncates: a hand-edited `pinned_slot = 300` restores as slot 44,
which `grouping()` (tile.rs:540-553) then looks up and falls back from
silently, whereas `:group slot` validates the 1..=9 range
(`core/commands.rs:60-65`). Session restore is a hand-editable surface, so it
deserves the same range check.
Direction: one `if let Some(n) = ... .as_integer()` binding, with the same
`(1..=9)` filter the command path uses, and a `tracing::warn!` on rejection —
matching how the neighbouring `filter.expr` and `as_of` restores already
report a bad value (tile.rs:275-300, 302-323).

### m18. No production-route keyboard test in either crate

**Location:** `crates/geode-blotter/src/tile.rs` tests (0 `simulate_keystrokes`,
13 direct `dispatch(&ActionId(...))` calls);
`crates/geode-diagnostics/src/tile.rs` tests (0 and 5).

Both crates test their action handlers by calling `dispatch` with a constructed
`ActionId`, never by pressing a key. `CLAUDE.md` is direct about this: "Test
production routes. Calling an internal mutation does not prove that a key,
pointer event, delivery, or focus transition reaches it." The mitigations are
real — `content.rs`'s `the_default_keymap_binds_exactly_the_actions_this_module_registers`
(content.rs:~330) proves every binding names a registered action and vice
versa, `caret_and_dollar_resolve_to_the_column_extremes` drives the real
`Matcher`, and the shell's own suites do press keys through occupants
(`crates/geode-shell/src/shell/tests/occupants.rs` has 35
`simulate_keystrokes`). So the binding→action edge and the action→mutation edge
are each covered, just never in one test. The uncovered composition is the key
context: `key_context` (tile.rs:876-882) pushes `mode` as a pair, and nothing
in the blotter's own suite presses `y` in visual mode through the keymap to
prove the `mode == visual` context actually resolves.
Direction: one test per crate that presses a real keystroke through the shell
into the tile — ideally the mode-sensitive one (`v`, `j`, `y`), since that is
the edge the unit tests structurally cannot reach.

### m19. The pointer parity for column resize and reorder has no keyboard route

**Location:** `crates/geode-blotter/src/tile.rs:356-357`
(`.col_resizable(true).col_movable(true)`), delegate at 857-871
(`move_column`); no corresponding action in `ACTIONS` (tile.rs:58-88) and no
binding in `DEFAULT_KEYMAP` (`crates/geode-blotter/src/content.rs:44-101`).

Column reorder and resize are mouse-only. `PHILOSOPHY.md:57-59` is
unconditional: "Every action reachable by mouse must be reachable by keyboard;
the reverse is not required." Header-click sorting *does* have its keyboard
twin (`s`/`S` → `sort_cycle`/`sort_cycle_abs`, and `perform_sort` at
delegate.rs:807 deliberately runs the same cycle), and the chevron click maps
to `space` — both explicitly reasoned about in the README. Reorder and resize
were left out. The persisted width is a `view_presentation.toml` value, so the
config route exists, but that is a file edit, not a keyboard route.
Related TODO items ("Autosize columns", "Blotter shortcut to edit column in
either view or schema") circle the same gap.
Direction: two actions (`blotter::move_column_left`/`_right` on the cursor
column, and a width nudge or autosize) bound in `DEFAULT_KEYMAP`, both writing
through the same `view_presentation.toml` door the drag does — which is also
the honest way to make a dragged width persist.

### m20. Diagnostics `find` accepts `Committed` without a preceding `Changed`, but `Cancelled` discards the query unconditionally

**Location:** `crates/geode-diagnostics/src/tile.rs:513-530`.

`FindEvent::Cancelled` does `self.filter.clear()` and rebuilds. `CLAUDE.md`
states the dialog-filter rule as "Escape restores the entry query and bare
Enter keeps the typed query", and the memory index records the filter-exit keys
as merged 2026-09-23 with "esc reverts the filter, enter keeps it". For a tile
find (as opposed to a dialog filter) the shell owns the input and the README
documents the behaviour as `/` supplying a substring filter, so clearing on
Escape is arguably the tile's own contract rather than the dialog rule — but
the blotter's `find` (tile.rs:1316-1331) restores the *cursor origin* on
Cancelled and the diagnostics one destroys the *query*, so the two tiles answer
the same shell event differently. Direction: confirm which is intended and
state it in the diagnostics README beside the existing `/` sentence; if Escape
should revert to the pre-`/` filter rather than to empty, the tile needs to
remember it (a restored session filter is currently destroyed by one Escape).

---

## Ideas (TODO.md blotter items)

### I1. Save cursor position on collapse, restore on re-expand

**Feasibility: straightforward.** The machinery exists: `Expansion` is a
`HashSet<Path>` (`core/expansion.rs:20-30`) and the cursor is already restored
by path across every reflatten (`restore_by_path`, `cursor.rs:100-158`).
Add `HashMap<Path, Path>` beside `Expansion` — closing a node records the
cursor's current path under the closed node's path; opening one looks it up and
hands it to `reflatten_keeping` as the `keep` argument instead of the current
cursor path. Bound the map by pruning alongside `prune_to`
(expansion.rs:85-88) so a regroup drops unreachable entries.
**Uniformity:** the pricer has the same expand/collapse shape
(`crates/geode-pricer/src/grid.rs`) and marketdata has row groups, so the
memory belongs in a small shared `expansion` type rather than in the blotter —
but the blotter is the only one with a path-keyed cursor today, so shipping it
here first and promoting later is reasonable.

### I2. Multi-select (shift+move)

**Feasibility: the model is already there.** `Mode::Visual { anchor }`
(`core/cursor.rs:20-27`) plus `selection()` (cursor.rs:75-85) is a contiguous
range selection, already honoured by yank (tile.rs:1048-1063) and the row
highlight (delegate.rs:918-929). `shift+j`/`shift+k` would be a second way in
to the same state: enter `Visual` if `Normal`, then move. The visual-mode wrap
suppression is already handled (`wrap = false`, cursor.rs:33-49, with a test).
**The real question is disjoint selection**, which the current `anchor` model
cannot express — that needs `Vec<Range<usize>>` or a `HashSet<Path>` of
selected nodes, and paths are the right choice so a selection survives a
requery the way expansion does.
**Uniformity:** marketdata wants the same gesture (TODO says "blotter and
market data"), so `Mode`/`selection` should move to the shell as a shared
`vimselect` beside `vimnav`/`vimfind` — which is where `NavCommand` and
`FindStyle` already live, so the precedent is clean.

### I3. Context menu with context-sensitive items (launch CVI, launch Nemo)

**Feasibility: three of the pieces exist, one seam is missing.** The menu
pattern is settled across the workspace: `crates/geode-marketdata/src/core/menu.rs`
is a pure row builder (`MenuInputs` → `Vec<MenuRow>` with
`enabled: Result<(), &'static str>` and `checked: Option<bool>`), and
`crates/geode-marketdata/src/popup.rs`, `crates/geode-pricer/src/popup.rs` and
`crates/geode-timeseries/src/popup.rs` all paint it on the same geometry
(`ROW_HEIGHT`/`ROW_INSET`/`MIN_WIDTH: 240`, `deferred(anchored(..))`,
`occlude()`, `on_mouse_down_out` into one closer). The timeseries module
already binds `.` and right-click to open it
(`docs/current/features.md:172-178`), and the memory index records the rule that
"a right tile press focuses like a left one" so the menu opens in the tile whose
keys answer it. So the blotter should copy `menu.rs` + `popup.rs`, not invent.
**The missing seam is cross-module launch.** "Launch CVI" means opening a
*marketdata* tile from the *blotter*, and feature modules may not depend on
siblings (`CLAUDE.md`). I found no module→shell action-dispatch route: the
`TileContent` trait (`crates/geode-shell/src/module.rs`) has `dispatch` for
inbound actions only, and no module references a `config::*` or tile-creation
`ActionId`. So this needs a new outbound door — either `TileContent::dispatch`
returning a request enum, or a `cx.emit` the shell subscribes to, carrying
`ActionId` plus a payload (the underlying under the cursor). The payload is the
interesting part: "launch CVI for the underlying on this row" is a *contextual*
action, so the door should carry a small `Context { dimension: String, value:
String }` the target module interprets. That design decision is worth its own
brainstorm before any code.
**Three menu rows fall out for free** from state the tile already holds:
`:unpin` (gated on `pin != Pin::None`), `:filter clear` (gated on
`!tile_scope.is_empty()`) and `:asof clear` (gated on `tile_as_of != Follow`) —
each with a disabled reason, exactly the `enabled: Result` shape `menu.rs`
uses.

### I4. Clicking a cell should focus that cell, not just the row

**Feasibility: blocked by a deliberate decision, cheap to unblock.** The tile
constructs the table with `.cell_selectable(false)` (tile.rs:354) and subscribes
only to `TableEvent::SelectRow` and `DoubleClickedRow` (tile.rs:361-377).
gpui-component already emits what is wanted:
`TableEvent::SelectCell(usize, usize)` and `SelectCellDoubleClicked`
(`gpui-component-0.6.2/src/table/state.rs:61-76`, verified). So this is
`.cell_selectable(true)` plus one match arm setting `cursor.col` alongside
`cursor.row`, then `sync_cursor`.
**Two cautions.** First, the cursor border is painted by `render_td` on
`cursor.row == row_ix && cursor.col == col_ix` (delegate.rs:934-935), so the
visual already supports a cell cursor — nothing new to paint. Second,
`cell_selectable(true)` may re-arm component key handling the crate
deliberately disarmed: `geode_blotter::init` (`lib.rs:16-38`) binds eleven keys
to `NoAction` in the `DataTable` context precisely so the table cannot swallow
vim keys, and the README pins that rule. Worth re-checking the component's cell
navigation against that list — `tab`/`shift-tab` and the arrows are already
bound away, which is probably sufficient.
**Uniformity:** the pricer and marketdata already have cell cursors, so this
makes the blotter consistent with its siblings rather than novel.

### I5. Shortcut to edit a column in the view or the schema

**Feasibility: needs the same outbound door as I3.** The target dialogs exist
as registered actions (`config::views`, `config::schema` —
`crates/geode-shell/src/defaults.rs:215-221`), and the blotter knows which
column the cursor is on (`cursor.col` → `plan.columns[col].name`). But as with
I3 there is no route for a module to dispatch a shell action, and no way to
pass "open the Views dialog *at this column's row*". The dialogs would also
need an entry point that accepts a target (the memory index notes the Schema
and Views dialogs already have column stages, so the landing spot exists).
**Direction:** implement the outbound door once for I3 and I5 together, with a
payload rich enough for "open dialog X focused on object Y" — that single seam
covers both, plus the diagnostics TODO ("Diagnostics to move to full page
screen") and any future "launch Nemo".

### I6. Autosize columns

**Feasibility: moderate, and it needs a measurement decision.** Widths come
from `presentation.width` with per-kind defaults
(`core/plan.rs:12-14, 96-99`), and `column()` reports them as `px`
(delegate.rs:777-783) — the README pins that "the whole tree cell stays in
`px`: column widths are the `view_presentation.toml` pixel contract".
The cheap, honest version needs no text measurement at all: the format cache
already holds the shaped text for the visible window
(`core/cache.rs:23-30`, `CachedCell.text`), so "autosize to the widest *visible*
cell" is a scan over `cache` plus a per-character advance estimate — and the
crate already has a mono advance constant to reuse (`GUTTER_DIGIT_PX: f32 = 8.0`,
delegate.rs:56-58, "the mono face's advance at the default UI size"). Cells are
monospace (`fonts::MONO`, delegate.rs:947), so `chars().count() * advance` is
accurate rather than a guess.
**Two design points.** (a) Autosizing to the *visible* window means the width
changes as you scroll, which is worse than a fixed width — so the action should
be an explicit one-shot (`blotter::autosize_column` on the cursor column, plus
an all-columns variant), not a mode. (b) The result must be written to the user
layer of `view_presentation.toml` through `geode_shell::config_write`
(`CLAUDE.md`) to persist, which is the same door a dragged width should use and
currently does not — so this and m19 are one piece of work.
**Uniformity:** TODO says "works on all tiles", and the pricer's columns are
explicitly fixed-width and non-resizable (`docs/current/features.md:383`), so
"all tiles" is really "the blotter and marketdata". The measurement helper
(mono advance → px) belongs in `geode-widgets` or `shell::scale` so both use
one rule.

---

## Systemic patterns

1. **Positional indices used as identities.** `SortSpec.column` (C1, C2),
   `cursor.col` (C1), row `ElementId`s (M4) and `narrowed`'s positions-into-
   `visible` (delegate.rs:522-536) are all positions standing in for domain
   identity. The crate already knows the fix — expansion is keyed by *path*
   precisely so it survives a requery (expansion.rs:1-5, and the cursor
   follows it via `restore_by_path`) — but that discipline stopped at rows and
   never reached columns. Columns are reorderable, hideable and foldable, so
   they need it just as much.

2. **The same subtlety fixed once per module.** M5's frame-follower, the
   refused-submit triple, the `try_global::<AppClock>` fallback (five copies),
   `HEADER_HEIGHT = 22.0` (five copies), and the stale-marker rule (blotter
   tile.rs:1382 vs marketdata tile.rs:2391, with the latter's comment citing
   "the blotter's own rule") are all duplicated mechanism. The comments prove
   the authors know they are copies. Every copy is a place the next fix can be
   forgotten.

3. **Caching applied unevenly within one file.** The blotter delegate memoises
   the expensive theme derivations behind precise keys with excellent comments
   (`ensure_theme_inputs`, `chevron_states`, `ensure_numbers`), and the tile
   caches five strings with documented single assignment sites — yet `render`
   formats freshness rows, footer counts and the grouping label per frame (M3),
   `render_th` allocates two strings per header per frame (m5), and
   `chip_paint` does contrast math per frame (m11). A reader cannot tell from
   the file which rule is in force.

4. **Provenance in place of invariants.** 68 comment blocks in the blotter lead
   with a task, round or finding number (m15) against an explicit `CLAUDE.md`
   rule. The content is usually excellent; the framing is archive material.
   `geode-diagnostics` shows the target state — zero citations, same rigour.

5. **Tests verify mutations, not routes.** Neither crate presses a key (m18).
   The keymap-completeness tests and the shell's occupant suites cover the two
   halves separately, which is why this is Minor — but the composition (mode
   pair in `key_context` → keymap resolution → dispatch) is untested in the one
   crate that has modes.

6. **Filtering after formatting.** Both diagnostics filter paths format every
   row then discard (m9, m10). The pattern costs the most in exactly the
   section with the most rows (config, 2,000 leaves per doc with an `explain`
   lookup each).

## What is done well

- **The pure/paint split is real and load-bearing.** `core/` has no `gpui`
  (verified: no `gpui` import in any of the eight core modules), is tested
  without a window, and is benched independently
  (`benches/blotter.rs`, five shapes). `docs/current/performance.md` records
  the resulting numbers. This is the architecture the philosophy asks for,
  actually delivered.

- **The format cache and glyph cache genuinely keep `render_td` a lookup.**
  `FormatCache::set_window` (cache.rs:44-63) moves overlapping rows rather than
  refilling them, with a test that counts fill invocations
  (`a_window_move_refills_only_the_rows_that_entered`). The tree glyph — which
  needs an allocating `path_of` — is resolved once per window fill
  (delegate.rs:701-728, `fill_window`) rather than per paint, with the reason
  stated. `render_td` does one `cache.get` and clones an `Arc<str>`.

- **`invalidate_cells` documents a real pinned-dependency bug and fixes it
  properly.** The `requested_window` field (delegate.rs:79-93) exists because
  gpui-component's `update_visible_range_if_need` returns early on
  `visible_range.len() <= 1` — which I verified at
  `gpui-component-0.6.2/src/table/state.rs:1288`. The comment cites the file,
  the version and the line behaviour, and the fix (refill the *requested*
  window, not the shrunken cache window, through a private `fill_window` that
  cannot shrink it) is exactly right. This is model work.

- **NULL and NaN semantics are taken seriously.** `NonAttributable` is NULL and
  paints blank, never `0.00`, enforced in the one place a cell becomes text
  (`core/cache.rs:70-105`) and tested cell-by-cell
  (`cells_honour_the_read_paths_opinions`). `number_at`
  (flatten.rs:237-241) maps NaN to `None` with the reason stated — including
  that `sort_by` panics on a non-total order in Rust ≥ 1.81. `f64_in`'s
  Decimal128 arm (`geode-core/src/snapshot.rs:250-268`) explains that turning
  "I cannot read this type" into a blank cell would be a false claim about the
  data. The philosophy's "plausible wrong totals are worse than an explicit
  error" is visibly internalised.

- **Expansion survives a requery by construction.** Paths not indices
  (expansion.rs:1-5), with NULL distinct from the empty string
  (`a_null_and_an_empty_string_are_different_paths`), vim's `zR`-then-`zc`
  carve-out modelled correctly with its own `closed` set, and `prune_to` on
  regroup. The cursor follows the same identity through `restore_by_path`.

- **The barrier and refusal seams are handled at every site I checked.** A
  refused submit answers the barrier and clears `acted` (tile.rs:767-784), a
  failed outcome counts as arrival (tile.rs:836-845), a stale tag is dropped
  *without* arriving (tile.rs:801-803, with marketdata's copy explaining why
  arriving would be wrong), a pinned/unscoped tile that will not requery still
  answers (tile.rs:625-645), and `promote` re-checks the staged versions
  against what the tile follows (tile.rs:672-687). The diagnostics tile signals
  its own arrival despite submitting no query (tile.rs:225-234) so a global flip
  cannot wait on it — with the reason in the README.

- **Every pointer gesture that exists routes to the keyboard's own path.** A
  chevron click and a row double-click both go through `expand_at_cursor`, the
  path `zo`/`zc`/`za`/`space` take (tile.rs:929-950, README-pinned); a header
  click runs the delegate's own five-state cycle rather than the component's
  three (delegate.rs:807-855), with the staleness of the component's cached
  arrow handled by a deferred `refresh` and the reasoning spelled out. The
  chevron listener stops propagation and ignores `click_count() > 1` so a fast
  double-click toggles exactly once — with a test for it.

- **`geode-diagnostics` observers are precisely scoped and tested for
  *absence* of work.** `diag_version_for_section` (tile.rs:43-52) means a perf
  tick cannot rebuild config rows, and there are tests asserting exactly that
  (`refresh_frame_hist_does_not_rebuild_the_config_section`,
  `a_scope_only_frame_change_does_not_rebuild_but_a_config_reload_does`,
  `a_config_reload_while_showing_sources_does_not_rebuild`). Negative
  performance tests are rare and valuable.

- **The diagnostics log tail is honest about loss.** `lost_records` is measured
  as a gap at the last drain, not a lifetime total, with `oldest_seq` read
  *before* draining and the off-by-one (`since + 1` is still readable) reasoned
  through against the ring's own test (tile.rs:240-262). `since` starts at the
  ring's current sequence so a freshly opened tile cannot claim records it never
  had — with a test named exactly that.

- **Allocation discipline is tested, not just asserted.**
  `rebuilding_does_not_reallocate_rows_between_paints` compares `Rc` identity,
  `a_no_op_log_drain_does_not_grow_the_drain_buffer` checks capacity, and
  `the_theme_input_memo_re_derives_only_when_a_theme_colour_moves` and
  `ColourCache::misses` verify the memos actually memoise.

- **Non-finite and malformed input cannot panic the render thread.**
  `format_number` spells `NaN`/`∞`/`-∞` (tested), `short_time` echoes an
  unparseable stamp whole rather than slicing (with a comment recording the
  panic that taught them), `completions` clamps a cursor down to a char
  boundary rather than slicing mid-codepoint (core/commands.rs:178-184), and the
  frame as-of fallback uses `req.get(..16)` rather than `&req[..16]`
  (tile.rs:1642). Every panic-shaped input I looked for was already closed.

- **Both crates' keymap fragments are proved complete in both directions.**
  `the_default_keymap_binds_exactly_the_actions_this_module_registers` (in each
  crate) asserts every binding names a registered action *and* every registered
  action is reachable from some key — the direction a mirrored id list could
  never cover, as the test's own comment explains.
