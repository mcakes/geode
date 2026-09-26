# Cross-cutting performance review — Geode workspace

Scope: whole workspace (14 crates, ~230k lines), judged against `docs/PHILOSOPHY.md`
§3 ("Latency is a feature") and §6 ("Performance is a discipline"),
`docs/current/performance.md` (budgets, cache/allocation contracts, known gaps),
`docs/current/architecture.md` "Performance contracts", and the gpui-kit coding
guides' "Performance rules" / "Lists, tables, and large data" / element
best-practices "Minimize Allocations in Paint Phase".

Read-only review. Every finding below cites file:line ranges that were actually
read. Nothing was built, tested or benchmarked.

## (a) Summary

1. **The data surfaces are excellent and the shell chrome is where the debt is.**
   Every module tile prepares its model outside render and pays only refcounts
   inside it; the status bar, palette, and three config dialogs derive and format
   in render. No Critical: nothing stalls the UI thread or shows a wrong number.
2. **The largest per-frame derivation is the keybindings dialog** (M-1), which
   rebuilds one row per registered action — with nested binding scans, ~350
   allocations and two sorts — on every frame. Its current cost is tens of
   microseconds, so it is filed for its *contract violation and its scaling*, not
   for blowing the budget today. The object and settings dialogs repeat the shape.
3. **`performance.md` contradicts a code comment about caching.** The document
   promises config dialogs "derive small row sets on change";
   `objectdialog/mod.rs:3189-3193` states they are "derived fresh at every call
   site (render, key handling, click resolution), never cached". Both cannot be
   right, and the comment is what the next author will follow.
4. **Benchmarks are in unusually good shape**: all 13 published reference numbers
   map to a committed bench at the stated fixture shape, and fixture construction
   is correctly hoisted out of every timed closure. The gap is coverage of the
   *render* layer — which is exactly where this review's findings are, and is
   `performance.md`'s own first known gap.
5. **Concurrency and wakeup discipline are the strongest part.** No lock is held
   during render or across an await, every lock degrades on poisoning rather than
   panicking, and the only always-on timer is one consolidated 500 ms poll that
   does nothing on a quiet tick. The app does not wake itself to repaint.

## (b) Render-path allocation census

Legend: **T** tolerable (O(1) per frame, small), **V** violation (per-row /
per-cell / per-point, or repeated formatting), **D** debug-only (gpui drops the
`debug_selector` closure unevaluated outside test/`test-support` builds — verified
against the pinned rev's `div.rs` per the comment at
`crates/geode-blotter/src/delegate.rs:953-958`).

| Location | What allocates | Per unit | Verdict |
|---|---|---|---|
| `crates/geode-shell/src/shell/render.rs:407-441` | `Vec<StripSpec>` + `divider_strips`/`dock_edge_strips` `Vec`s | per frame, per divider | T (bounded by tile count; only while `dividers_active`) |
| `crates/geode-shell/src/shell/render.rs:444-454` | `Vec<DockCell>` + `dock_tree.layout(r)` `Vec` | per frame, per dock | T |
| `crates/geode-shell/src/shell/render.rs:459` (`tree.layout(tree_area)`) | `Vec<(TileId, Rect)>` — `crates/geode-shell/src/tiling/tree.rs:527` | per frame | T (tile count is small) |
| `crates/geode-shell/src/shell/render.rs:560` | `format!("tile {}", id.0)` | per placeholder tile per frame | V (small; only for placeholders) |
| `crates/geode-shell/src/shell/render.rs:599-607` | `"…".to_string()` / `format!("ctrl+k → …")` | per empty-dock hint per frame | V (small) |
| `crates/geode-shell/src/shell/render.rs:640,868,925,974,1265,1304,1386,1465,1553,1620` | `debug_selector` closures (`format!`, `to_string`) | per element per frame | D |
| `crates/geode-shell/src/shell/render.rs:651,770` | `Entity` handle clone (refcount) | per tile per frame | T |
| `crates/geode-shell/src/shell/render.rs:1169-1171` | modal `title`/`title_extra`/`build` clones | per frame while a modal is open | T |
| `crates/geode-shell/src/shell/render.rs:1521-1536` | `Vec<stacklist::Row>` + `o.content.title(cx)` per member + `list.members.clone()` | per frame while the stack list is open | T (transient chrome, ≤ stack size) |
| `crates/geode-shell/src/shell/render.rs:536-560,854-900,1250-1265,1464` | repeated `cx.theme()` resolution | ~10+ per frame | T (`cx.theme()` is a global read, not a derivation) |
| `crates/geode-shell/src/shell/status.rs:113-117` | `pending.iter().map(format_keystroke).collect::<Vec<_>>().join(" ")` — and `format_keystroke` itself builds a `Vec<&str>` + `join` (`status.rs:280-295`) | per frame, per pending keystroke | V — runs on *every* frame even when `pending` is empty (an empty iterator still allocates the `Vec` and the `String` from `join`) |
| `crates/geode-shell/src/shell/status.rs:129` | `format!("{count}")` | per frame while a count is pending | V (small) |
| `crates/geode-shell/src/shell/status.rs:139,150,158,168,188` | `message.to_string()` for error/notice/restart/diagnostics text | per frame per visible message | V — the source is already a string; a `SharedString` would be a refcount |
| `crates/geode-shell/src/shell/status.rs:219` | `format!("AS OF {t} · Return to live in the palette")` → `SharedString` | per frame while historical | V — the as-of text changes rarely; comment at 210-218 notes it is built once *per render* and reused twice within it, but not across frames |
| `crates/geode-shell/src/shell/status.rs:245` | `theme_name.to_string()` | per frame | V (small, unconditional) |
| `crates/geode-shell/src/shell/sidebar.rs:163` | `WORKSPACE_SITE[..].to_string()` in `debug_selector` | per workspace per frame | D |
| `crates/geode-shell/src/shell/sidebar.rs:178` | `ActionId(format!("workspace::switch_{n}"))` **inside the listener closure** | per workspace per frame (the closure is boxed and the `format!` runs on click) | T — the `format!` is inside the click handler, so it is per *click*, not per frame; the closure box is per frame |
| `crates/geode-shell/src/shell/sidebar.rs:214` | `n.to_string()` for the disc label | per workspace per frame | V (tiny; ≤9 workspaces) |
| `crates/geode-shell/src/shell/sidebar.rs:221,236,264` | `to_string()` in debug selector / listener | per frame | D / per-click |
| `crates/geode-shell/src/shell/toolbar.rs:237,268` | `format!("scope-chip-{body_column}")` / `-close-` | per scope chip per frame | D |
| `crates/geode-shell/src/shell/toolbar.rs:146,296,326,349,379,417,440,445,459,489,530` | `debug_selector` closures | per element per frame | D |
| `crates/geode-shell/src/palette.rs:640` | `format!("palette-row-{i}")` | per visible row per frame | D |
| `crates/geode-shell/src/palette.rs:656-661` | `cat_ix: Vec<usize>` (rebased category match indices) | **per row per frame** | V — the comment at 649-655 explicitly accepts it ("done in place here because this runs once per row per frame"); title indices are a no-alloc slice, categories are not |
| `crates/geode-shell/src/palette.rs:665-671` | `highlighted_title(&item.title(), …)` — `title()` returns `String` (`palette.rs:39-45`: a `clone()` for an action, a `format!` for a theme or scope row) | per row per frame | V — verified owned allocation per row |
| `crates/geode-shell/src/palette.rs:677` | `item.binding().unwrap_or("").to_string()` | per row per frame | V |
| `crates/geode-shell/src/palette.rs:622` (row loop) | the whole palette list is **not virtualized** — `for (i, …) in state.rows().enumerate()` builds an element per filtered item | per filtered item per frame (~84 today) | V — see M1 |
| `crates/geode-shell/src/shell/keybindings_view.rs:1028,1042` | `derive_rows` (registry walk, nested binding scans, ~6 allocations per action, one sort) + `visible_rows` (`format!` per row, fuzzy rank, second sort) | per frame while the dialog is open | V — the largest per-frame derivation found; see M-1 |
| `crates/geode-shell/src/shell/settings_view.rs:822,854` | `rows_for` (`theme.names()` → `Vec<String>` of 44 names, `fetch_source_names` → `Vec<String>`, `derive_rows`) + `visible_rows` | per frame while the dialog is open | V — see M0 |
| `crates/geode-shell/src/shell/objectdialog/render.rs:2763` | `visible_rows` → `Vec<String>` of `format!("{} {}", display_name, summary)` per row + `listfilter::rank` | per frame while the dialog is open | V — see M0 |
| `crates/geode-shell/src/shell/whichkey.rs:53,104-117` | `sort_by_key` with a `String`-returning key fn; `render_keystroke` + `title_for` per row | per frame while a chord prefix is held | V — see m6b |
| `crates/geode-blotter/src/delegate.rs:898` (`render_th`) | `self.column(col_ix, cx).name.clone()` → and `column()` itself builds `label.clone()` + `push_str` + two `SharedString::from(String)` (`delegate.rs:750-790`) | per header cell per `render_th` | V — `column()` allocates two `String`s and two `SharedString`s |
| `crates/geode-blotter/src/delegate.rs:905-908` | `themed_cell_colour` → 28-`Hsla` signature compare, no derivation on the steady path (`delegate.rs:253-275`) | per named-colour header cell | T (explicitly designed) |
| `crates/geode-blotter/src/delegate.rs:957,995,1032,1040` | `debug_selector` `format!`s | per cell per frame | D |
| `crates/geode-blotter/src/delegate.rs:1000-1003` | `self.chevron_states(theme)` — `ControlInputs::new` + compare, memoised (`delegate.rs:282-297`) | per tree cell per frame | T (7 `Hsla` copies + one compare) |
| `crates/geode-blotter/src/delegate.rs:1017-1022` | `self.numbers.get(i).cloned()` (`SharedString` refcount) after a stamped `ensure_numbers` (`delegate.rs:330-351`) | per tree cell per frame | T |
| `crates/geode-blotter/src/delegate.rs:1057-1058` | `SharedString::from(Arc::clone(&cell.text))` | per cell per frame | T — refcount only; this is the contract working |
| `crates/geode-blotter/src/delegate.rs:1034-1048` | `cx.listener(move |…|)` boxed chevron click handler | per tree cell per frame | T (comment at 1031-1033 acknowledges gpui boxes it either way) |
| `crates/geode-blotter/src/tile.rs:1626-1627` | `datasets: Vec<_> = p.datasets.iter().collect()` **then `sort_by`** | per frame | V — a sort in render; §6 forbids per-frame churn and the guides forbid non-trivial work in render |
| `crates/geode-blotter/src/tile.rs:1628` | `chrono::Utc::now()` | per frame | V (minor, but it is a clock read per frame; the shell deliberately hoisted its own — `shell/mod.rs:1330-1354`) |
| `crates/geode-blotter/src/tile.rs:1630-1633` | `format!("{} {}", f.dataset, short_time(t, clock))` | per dataset per frame | V |
| `crates/geode-blotter/src/tile.rs:1643-1645` | `DateTime::parse_from_rfc3339(req)` + `format!("AS OF {}")` + `.format(...)` | per frame while historical | V — RFC-3339 parse *and* strftime per frame |
| `crates/geode-blotter/src/tile.rs:1682,1687-1689,1696` | `format!("{} rows", …)`, `semi_joined.join(", ")` + `format!`, `format!("{} rows unplaced")` | per frame | V — the `join` is per frame for a value that changes only on requery |
| `crates/geode-blotter/src/tile.rs:1506,1533,1589,1592-1594,1613` | `Arc<Snapshot>` clone, `view_name`/`asof_chip`/tip `SharedString` clones | per frame | T (refcounts) |
| `crates/geode-pricer/src/delegate.rs:184` (`render_th`) | `self.column(col_ix, cx)` → `SharedString::new_static` + `c.label.clone()` (`delegate.rs:148-175`) | per header cell | T — `new_static` for the key, refcount for the label; no `String` built |
| `crates/geode-pricer/src/delegate.rs:193,259,296,301,348,351` | `debug_selector` `format!`s / `.into()` | per cell per frame | D |
| `crates/geode-pricer/src/delegate.rs:243` | `Rc::clone(&self.model)` | per cell per frame | T (refcount; comment explains the borrow reason) |
| `crates/geode-pricer/src/delegate.rs:250-253` | `cx.theme()` + `radius_tokens()` per cell | per cell per frame | T (two `Copy` reads) |
| `crates/geode-pricer/src/delegate.rs:325,371-372,391` | `row.tree.clone()`, `cell.text.clone()`, editor clone | per cell per frame | T (refcounts — the documented contract at 231-236) |
| `crates/geode-pricer/src/delegate.rs:339-356` | `cx.listener` boxed chevron handler | per package row per frame | T |
| `crates/geode-marketdata/src/delegate.rs:317-336` (`render_th`) | `self.column(col_ix, cx)` → `self.model.columns.get(..).cloned()` (a `SharedString` clone) + `name.clone()` for the key (`delegate.rs:283-299`) | per header cell | T (refcounts) |
| `crates/geode-marketdata/src/delegate.rs:333,391,398,424,445,469,478` | `debug_selector` `format!`s | per cell per frame | D |
| `crates/geode-marketdata/src/delegate.rs:364-380` | `r.label.clone()` (refcount) | per label cell per frame | T |
| `crates/geode-marketdata/src/delegate.rs:381-384,441-444` | `self.editor_at(..).cloned()` | per cell per frame | T (≤1 editor exists; `Option` is `None` for every other cell) |
| `crates/geode-marketdata/src/delegate.rs:386,434` | `cell_paint(theme, …)` — pure `Copy` theme reads (`delegate.rs:~490-512`) | per cell per frame | T |
| `crates/geode-marketdata/src/delegate.rs:446-457` | `Rc::clone(&c.paint)` for the choice popup | per cell per frame | T (`Option` filtered to one cell) |
| `crates/geode-chart/src/element.rs:356-357` | `gx: Vec<Pixels>` from x ticks | per pane per frame | T — comment at 351-355 notes `Grid` takes `Vec`s as its API |
| `crates/geode-chart/src/element.rs:358-361` | `gy: Vec<Pixels>` from y ticks | per pane per frame | T |
| `crates/geode-chart/src/element.rs:398-416` | `ShapeKey` build + `caches.slot(k).get(key, …)`; `polyline(...)` only on a miss, into a reused `scratch` | per visible slot per frame | T — the documented cache contract |
| `crates/geode-chart/src/element.rs:478-497` | `tags: Vec<Text>` with `label.clone()` (refcount) per percentile | per pane per frame, ≤ `MAX_PERCENTILES` per slot | T |
| `crates/geode-chart/src/element.rs:503-547` | density bars: uncached `paint_quad` per bin, capped by `MAX_DENSITY_QUADS` | per bin per frame | T — documented and bounded at 2,000 quads |
| `crates/geode-chart/src/element.rs:526` | `slot.bins.iter().map(...).max()` per slot | per slot per frame | V (minor) — the per-slot max is recomputed every frame though `bins` changes only on a query |
| `crates/geode-chart/src/element.rs:745-747` (`paint_y_axis`) | `AxisText::new(label.clone(), …)` iterator into `PlotAxis` | per tick per axis per frame | T — refcount clones; the component's allocation is noted as a known gap in performance.md |
| `crates/geode-chart/src/element.rs:559-568` (`bucket_title`) | `FixedOffset` + `DateTime` + `t.format(...).to_string()` | per tooltip paint | T — tooltip only, not the steady frame |
| `crates/geode-chart/src/element.rs:712-717` | `SharedString::from(fmt_value(value))` per visible slot | per tooltip paint | T (tooltip only) |
| `crates/geode-timeseries/src/tile/mod.rs:1066-1076` | `theme_signature(cx.theme())` + `Arc::as_ptr` compare; `rebuild_chrome` only on a change | per frame | T — 28 `Hsla` copies + compare, the documented memo |
| `crates/geode-timeseries/src/tile/mod.rs:1082-1084` | `render_chart_surface` → `self.chart.clone()` (`Rc`/`Arc` of the model) | per frame | T |
| `crates/geode-timeseries/src/tile/mod.rs:1065` (`render_chart_surface:967`) | `format!("timeseries-chart-{tile_id}")` in `debug_selector` | per frame | D |
| `crates/geode-timeseries/src/tile/mod.rs:1132-1139` | `self.notice.clone()`, `self.footer.clone()` (`SharedString`) | per frame | T |
| `crates/geode-diagnostics/src/tile.rs:613-650` | `self.rows.clone()` (`Rc<Vec<Row>>`) then a `uniform_list` closure building `Vec<AnyElement>` for the visible range only | per visible row per frame | T — virtualized, and `rows` is an `Rc` refcount (`tile.rs:91`) |
| `crates/geode-diagnostics/src/tile.rs:596,609,652` | `debug_selector` `format!`s | per frame | D |
| `crates/geode-shell/src/shell/perf_overlay.rs:46+` | overlay text formatting | per frame while the overlay is open | T (opt-in surface) |
| `crates/geode-shell/src/shell/control.rs:173` (`paint`) | `ControlPaint` derivation — called through per-surface memos | per memo miss | T |
| `crates/geode-widgets/src/datefield/paint.rs:57` | segment paint | per frame while a date field is open | T (not yet read in detail — see "not completed") |

## (c) Findings

Severity is judged by distance from the stated contract and by how much of the
frame budget the item can plausibly consume.

### Critical

None. No finding in this review reaches "wrong numbers shown" or "the UI thread
blocks on data work". The contract violations below are frame-budget and
scaling defects, not correctness or stall defects.

### Major

**M-1. The keybindings dialog rebuilds its entire row set — a full registry walk
plus nested binding scans, ~350 heap allocations and two sorts — on every
frame.**

Locations:
- `crates/geode-shell/src/shell/keybindings_view.rs:1019-1042` — `build` is the
  dialog's render function (it returns the `AnyElement` the modal paints). Its
  first act is `derive_rows(&shell.services.registry, &shell.services.keymap)`
  (1028), and it then calls `visible_rows(state, &rows)` (1042).
- `crates/geode-shell/src/shell/keybindings_view.rs:94-116` — `derive_rows`
  iterates **every registered action** and per action clones `ActionId`, `title`
  and `category` (three `String`s), clones `keystrokes` (a `Vec`),
  `context_source` and `key_source`, calls `effective_binding`, calls
  `user_overrides_for`, and finally sorts the whole row vector (114).
- `crates/geode-shell/src/keymap/build.rs:180-188` — `effective_binding` is a
  reverse scan of *all* bindings, and for each candidate calls `is_shadowed`.
- `crates/geode-shell/src/keymap/build.rs:226-230` — `is_shadowed` is itself a
  scan of every later binding. So `effective_binding` alone is O(bindings²) in
  the worst case, per action.
- `crates/geode-shell/src/keymap/build.rs:198-221` — `user_overrides_for`
  allocates a `Vec<&Binding>` of every non-user binding **per action** (199),
  then for each user binding runs a nested scan over that vector (205-211), then
  allocates a `Vec<UserOverride>` with two `String` clones per entry (214-218)
  and sorts it (220).
- `crates/geode-shell/src/shell/keybindings_view.rs:242-245` — `visible_rows`
  then builds a `Vec<String>` via `searchable_text` (a `format!` per row,
  `keybindings_view.rs:316-318`) and runs `listfilter::rank`.
- `crates/geode-shell/src/listfilter.rs:29-52` — `rank` allocates a `Ranked`
  per row (with a `Vec<usize>` of match indices for a non-empty query), plus a
  `scored` vector, plus a stable sort, plus a final `collect`.

What and why: this is the exact pattern the guides forbid ("Avoid rebuilding
entities, subscriptions, focus handles, and expensive data structures per frame";
"business logic … embedded in a long `render` method" is listed as a common
failure mode) and it is the opposite of what `performance.md` claims under
"Cache and allocation contracts" — "Config dialogs derive small row sets on
change." The keybindings row set is neither small nor derived on change: it is
one row per registered action, rebuilt from the registry and the full binding
list on every repaint.

Impact, sized honestly. The current scale is modest: ~36 shell builtin actions
(`crates/geode-shell/src/defaults.rs:110-146`, counted) plus the app's and the
modules' registrations, so on the order of 50-60 rows, against ~75 builtin keymap
entries (`crates/geode-shell/src/defaults.rs:24-500`, counted). That works out to
roughly 50 `Vec<&Binding>` allocations of ~75 elements each, ~300 `String`
clones, two sorts, and low tens of thousands of `Vec`/`Option<String>`
comparisons per frame — tens of microseconds, not milliseconds. So this is not
currently blowing the 8 ms budget.

It is filed Major anyway for three reasons: (1) it contradicts a stated contract
in `performance.md` ("Config dialogs derive small row sets on change"), and a
contract that the code does not keep is worse than no contract — the next reader
will trust it; (2) the dialog is a filter box, so this runs on the typing path,
which is the one place PHILOSOPHY §3 is least willing to spend; (3) it scales as
the product of two things the product is designed to grow — registered actions
(every new module adds some) and user keymap entries (PHILOSOPHY §2 promises
remappability), and the growth is multiplicative through `is_shadowed`. It is
also invisible to the existing benchmarks: `shell_cores.rs` benches
`no_match_full_keymap` (the matcher) and `flush_build_keymap` (the reload), never
this.

Suggested direction: derive the rows once when the dialog opens and on the events
that can change them (a keymap reload, an override write), storing them in
`KeybindingsState`; keep `visible_rows`' ranking keyed on the query so it
re-ranks only when the query changes. Inside `keymap/build.rs`, index bindings by
`ActionId` once per keymap build so `effective_binding` and `user_overrides_for`
become lookups rather than nested scans.

How to measure: add a `keybindings_derive_rows` Criterion bench beside
`shell_cores.rs:381`'s `flush_build_keymap`, at the real registry size (the
composition root registers them, so a bench fixture should register a
representative few hundred), and a second entry for `derive_rows + visible_rows`
together. Then confirm with the frame histogram: open the dialog, hold a
character in the filter, and read p95.

### Major

**M0. The object dialog and the settings dialog repeat the same
derive-and-rank-in-render shape.**

- `crates/geode-shell/src/shell/objectdialog/render.rs:2763` calls
  `super::visible_rows(state, &rows)` from the dialog's render path, and
  `crates/geode-shell/src/shell/objectdialog/mod.rs:3194-3200` builds a
  `Vec<String>` via `searchable_text` — `format!("{} {}", row.display_name(),
  row.summary)` (`mod.rs:3186-3188`) — and runs the full `listfilter::rank`.
  The doc comment immediately below it (`mod.rs:3189-3193`) states the design
  outright: "derived fresh at every call site (render, key handling, click
  resolution), **never cached**".
- The other `visible_rows` on the state (`objectdialog/mod.rs:1086-1111`) also
  builds a `Vec<String>` of row labels, ranks, and then sorts (1109).
- `crates/geode-shell/src/shell/settings_view.rs:813-854` — `build` calls
  `rows_for(shell, cx)` (822), which calls `theme.names()` (a `Vec<String>` of
  every bundled theme name — 44 of them per the theme work) and
  `fetch_source_names(cx)` (another `Vec<String>`) before `derive_rows`
  (`settings_view.rs:553-563`), then `visible_rows` (854) with its own
  `format!`-per-row and rank (`settings_view.rs:278-287`).

Why Major rather than Critical: neither has C1's O(bindings²) inner loop, and
both row sets are genuinely smaller (settings is a fixed handful of rows; an
object dialog lists one domain). But both contradict the same documented "derive
on change" contract, and the allocation is per row per frame.

Direction: same as C1 — derive on open and on change, rank on query change.
Measure: `config_edit` in `shell_cores.rs:1281` (per the measurement log) already
covers a keystroke's *state* cost; extend it to include the row derivation so the
render-side cost is visible.

**M1. The palette list is not virtualized and allocates three times per row per
frame — currently at a benign ~84 rows, so the defect is the shape, not today's
cost.**
`crates/geode-shell/src/palette.rs:622-681` — a plain `for (i, (item, indices,
title_len)) in state.rows().enumerate()` loop over the *entire* filtered set,
each iteration building a `Vec<usize>` of rebased category indices (656-661), a
`to_string()` for the binding (677), and two `highlighted_title` calls (665-671).
The coding guides require virtualization "when data can grow beyond a small,
bounded collection" and `performance.md` states "Large module tables use
virtualization or prepared visible rows" — the palette is neither. The diagnostics tile (`uniform_list`,
`crates/geode-diagnostics/src/tile.rs:620`) shows the shape the palette should
take.

Impact, sized honestly: the palette bench's own comment records the real
population — "Today's real size is 84 (40 registry actions + 44 bundled themes)"
(`crates/geode-shell/benches/shell_cores.rs:229-234`). At 84 rows, ~250
small allocations per frame is not a budget problem, and an empty query is the
worst case. So this is filed Major for its shape and its trajectory, not its
present cost: the palette is PHILOSOPHY §2's "universal, discoverable fallback",
typed into character by character, its row count is the sum of every action every
future module registers plus every theme and every saved scope, and the bench
already measures 500 and 2,000 items precisely because the authors expect growth.
The state-side curve is benched; the render side is not.

Direction: `uniform_list` over `filtered`, and move the category-index rebase
(656-661) and the binding string (677) into the prepared row — both change when
the query changes, not per frame. Measure: extend `shell_cores.rs:237`
(`set_query_{n}_items`) with a render-side companion at the same three sizes, so
the render curve sits beside the filter curve.

**M2. The blotter header sorts, reads the clock, and formats timestamps on every
frame.**
`crates/geode-blotter/src/tile.rs:1624-1656` — `p.datasets.iter().collect()`
followed by `sort_by` (1626-1627), `chrono::Utc::now()` (1628), a `format!` per
dataset (1630-1633), and, while historical, a full
`DateTime::parse_from_rfc3339` plus `.format("%Y-%m-%d %H:%M")` (1643-1645).
None of it is stamped or cached, and all of it depends only on the snapshot
(which changes on requery) and the day (which the shell already hoists —
`crates/geode-shell/src/shell/mod.rs:1330-1354` explicitly removed a per-repaint
clock read for exactly this reason, so that a held key does not pay for it).
Impact: with N datasets this is N formats + one sort + one RFC-3339 parse per
frame, on the surface the trader holds `j`/`k` in. Direction: prepare a
`Vec<SharedString>` of freshness chips when a snapshot is applied
(`BlotterTile::apply`), re-deriving only on snapshot change or a staleness-band
crossing. Measure: a Criterion bench of the header-model build, plus the frame
histogram under a held `j` on a wide-provenance snapshot.

**M3. The status bar formats unconditionally on every frame, including when
nothing is pending.**
`crates/geode-shell/src/shell/status.rs:113-117` builds
`pending.iter().map(format_keystroke).collect::<Vec<_>>().join(" ")` before any
check on `pending`, and `format_keystroke` itself allocates a `Vec<&str>` and a
joined `String` per keystroke (280-295). Add `theme_name.to_string()` (245),
unconditional, and `message.to_string()` at 139/150/158/168/188 for values that
are already owned strings upstream. Impact: a handful of small allocations on
literally every frame of the application's life, on the one element that is
always visible. It is small individually — but §6 names per-frame heap churn a
reviewable defect, and this is the clearest instance that no cache covers.
Direction: early-return an empty `SharedString` when `pending.is_empty()`; make
the message parameters `&SharedString` and clone the refcount; hold
`theme_name` as a `SharedString`. Measure: the frame histogram's p50 with the
overlay open on an idle window before/after — this should be visible as a
floor change if anything is.

**M4. The blotter `column()` builds two `String`s and two `SharedString`s, and
`render_th` calls it per header cell.**
`crates/geode-blotter/src/delegate.rs:750-790` — `let mut label = c.label.clone()`
then `push_str(" ⋈")` / `push_str(" |x|")`, then
`SharedString::from(c.name.clone())` and `SharedString::from(label)`.
`render_th` (896-916) calls `self.column(col_ix, cx)` and clones `.name` again.
The component also calls `column()` once per column in `prepare_col_groups`
(`gpui-component-0.6.2/src/table/state.rs:636-648`), which runs on construction
and on every `refresh` (300, 388-390) — so the cost is per refresh *and* per
header paint. Contrast the pricer, which uses `SharedString::new_static` for the
key and a refcount for the label (`crates/geode-pricer/src/delegate.rs:148-175`).
Impact: bounded by column count, but it is the documented "no repeated
formatting in the render path" rule broken for a value that changes only when
the plan or the sort does. Direction: precompute the decorated label as a
`SharedString` in the `ColumnPlan` (or a parallel `Vec<SharedString>` rebuilt on
plan/sort change) and have `column()`/`render_th` clone refcounts. Measure:
extend `blotter.rs`'s `plan_build_100_columns` with a `column()`-per-column
bench at 100 columns.

**M5. Series query results come back row-wise through the DuckDB `Row` API while
the main view query is Arrow-columnar.**
`crates/geode-data/src/query/series.rs:386-397` — `while let Some(row) =
rows.next()?` with a `row.get::<_, i64>(0)` for the bucket and a
`row.get::<_, Option<f64>>(i+1)` per slot per row. The main requery path uses
`query_arrow` (`crates/geode-data/src/query/pool.rs:440`) and keeps the
`RecordBatch` (`crates/geode-core/src/snapshot.rs:387-395`), which is what
PHILOSOPHY §6's "columnar data flowing end-to-end without materializing into row
objects" asks for. The series path does materialize per-row accessor calls,
though it does at least land in struct-of-arrays (`values: Vec<Vec<f64>>`,
388). Impact: the measured 9.56 ms / 14.5 ms series-query medians in
`performance.md` include this extraction; at the 500,000-point cap the per-row
virtual `get` calls are a real fraction. Direction: `query_arrow` for the points
statement and a columnar drain into the existing `Vec<f64>`s. Measure:
`series_query/4_slots_plus_ratio_1d_1y` before/after, and split the bench so
extraction is timed separately from SQL execution.

**M6. Percentile, bin and coverage statements are prepared and executed per slot
inside the query — the documented "repeats the bucketing prefix" gap, and it is
worse than the doc implies.**
`crates/geode-data/src/query/series.rs:400-470` — for each slot, up to three
separate `conn.prepare` + `query` round trips (percentiles 405-418, bins
423-449, coverage 459-470). `performance.md`'s known gap says "Series statistics
repeat the bucketing prefix for points, percentiles, and density"; what the code
shows is that each is also a *separate prepared statement per slot*, so the cost
is O(slots × 3) preparations, not just a repeated prefix. Impact: the 14.5 ms
"with stats" figure is for *two* slots over one month; the documented four-y-axis
/ multi-slot design means the shape that matters is larger than what is measured.
Direction: as the doc proposes — one grouping-set or materialized bucketing CTE
shared by all three statistics across all slots. Measure: add a
`series_query/4_slots_with_stats` bench (the current stats bench is 2 slots) so
the scaling is visible before the rewrite.

**M8. A pricer delivery rebuilds the whole grid; there is no delivery-side patch
path, and a visible pricer tile wakes the app every 30 s by default to do it.**

- `crates/geode-pricer/src/tile.rs:1242-1280` — `deliver` ends with
  `self.rebuild(cx)` (1278) followed by `self.submit(cx)`. `rebuild` is the full
  `GridModel::build`, measured at 1.85 ms for 1,000 rows with every package open
  (`performance.md`'s reference table; bench at
  `crates/geode-pricer/benches/core.rs:171`).
- `crates/geode-pricer/src/tile.rs:1293-1305` — `restart_timer` installs a
  periodic reprice task, correctly gated on `self.visible` (1296), whose interval
  falls back to the shared setting; the default is 30 s
  (`crates/geode-pricer/src/content.rs:190`, `refresh: Some(Duration::from_secs(30))`).

What and why: `performance.md` states the contract as "A pricer grid model is
rebuilt on edit, delivery, expansion, view, clock or entry change, never in
render" — so the rebuild-on-delivery is documented and deliberate, and it is the
render-side contract that matters most. But it sits oddly beside the market-data
panel, which has exactly the patch path the pricer lacks: "A market-data delivery
or structural edit builds a `MatrixModel`; an ordinary cell commit patches it",
measured at 116 ns versus 8.18 ms for the rebuild
(`crates/geode-marketdata/benches/matrix.rs:281,304`). A price delivery changes
only the delivered lines' value cells — structurally the same situation a cell
commit is in.

Impact: 1.85 ms of the 8 ms budget, every 30 s per visible pricer tile, plus the
repaint it notifies. Within budget today. It becomes a problem at a shorter
`:refresh` interval (the command exists —
`crates/geode-pricer/src/core/commands.rs:91`), at more than 1,000 rows, or with
several pricer tiles visible at once, since each has its own timer.

Direction: a `patch_line` on `GridModel` mirroring `MatrixModel::patch_cell`, used
when a delivery changes only values and no row structure. Measure: a
`patch_line_1000` entry beside `grid_build_1000` in
`crates/geode-pricer/benches/core.rs`, and the frame histogram with a pricer tile
at `:refresh 1s`.

### Minor

**m1. Chart density-bar maxima are recomputed per slot per frame.**
`crates/geode-chart/src/element.rs:526` — `slot.bins.iter().map(|(_, _, n)| *n).max()`
runs inside `paint_pane`, per visible slot, every frame, though `bins` changes
only when a query lands. The bars themselves are correctly bounded
(`MAX_DENSITY_QUADS`, 535-537) and the doc acknowledges they are uncached, but
the max is pure repeated work rather than painting. Direction: store the max
alongside `bins` in `ChartSlot` when the model is built. Measure: the chart
element has no paint bench; the cheapest signal is `note_density_quad`'s
counter plus the frame histogram with density on and four slots.

**m2. `format_keystroke` is a status-strip formatter that allocates two heap
objects per keystroke and is called from render.**
`crates/geode-shell/src/shell/status.rs:280-295`. Direction: write into a
reused `String` buffer owned by the shell, or into a `SmallVec`-backed stack
buffer. Same measurement as M3.

**m3. The blotter footer rebuilds `semi_joined.join(", ")` per frame.**
`crates/geode-blotter/src/tile.rs:1687-1690` — the joined column list changes
only when the plan does. Direction: prepare it with the plan. Measure: folds
into M2's header-model bench.

**m4. The `ActionRegistry` hash map is behind an `RwLock` that the crash hook
shares, and `name_of_hash` does a linear scan of every registered id.**
`crates/geode-shell/src/actions.rs:36-78` — `hashes: Arc<RwLock<HashMap<u64,
String>>>` written on registration (44-48) and `name_of_hash` scanning
`self.actions.keys()` with a `fnv1a` per key (66-72). Neither is on the per-key
path (verified: `dispatch` at `crates/geode-shell/src/shell/input.rs:92-96` takes
the `action_tail` `Mutex`, not this lock), so this is not a render or key-path
lock — but it is an O(actions) scan on a path (crash reporting) that runs when
the process is already in trouble. Direction: leave it; note it. Measure: none
needed.

**m5. Per-key context-stack construction allocates a `String` per context flag.**
`crates/geode-shell/src/shell/input.rs:35-48` builds
`vec![KeyContext::new("workspace")]` plus up to three more, and
`KeyContext::new` allocates `flags: vec![name.into()]`
(`crates/geode-shell/src/keymap/context.rs:12-17`), i.e. a `Vec` plus a `String`
per context, per keystroke — and `context_stack` is called twice on some paths
(`input.rs:249` in the main press path, and again in `is_palette_toggle` at
50-53). Impact: ~8 small allocations per keystroke; well inside an 8 ms budget,
but it is the hottest path in a keyboard-first app. Direction: `&'static str`
flags (every caller passes a literal except the occupant's own
`key_context(cx)`), or a `SmallVec`. Measure: `shell_cores.rs:177`
(`no_match_full_keymap`) already covers the matcher; add a
`context_stack + press` bench beside it.

**m6. Binding resolution is a linear scan of the whole keymap with predicate
evaluation, twice per press in the pending case.**
`crates/geode-shell/src/keymap/matcher.rs:58-70` iterates every binding,
evaluating `predicate.eval(stack)` for each, and `single_keystroke_binding`
(`input.rs:55-64`) does a second `rfind` over the same list for the
palette-toggle check. `no_match_full_keymap` benches the scan, so this is
measured — recorded here only because the review brief asks for the per-key
path. Direction: none required at current keymap sizes; if the keymap grows,
index by first keystroke. Measure: already covered.

**m6b. The which-key overlay rescans the keymap and sorts with a
`String`-allocating key function on every frame it is visible.**
`crates/geode-shell/src/shell/render.rs:1153-1156` computes
`whichkey::continuations(...)` inside render — correctly gated on
`!pending.is_empty()`, so it costs nothing except while a chord prefix is held.
When it is held, `crates/geode-shell/src/shell/whichkey.rs:28-55` scans every
binding, evaluates each predicate, builds a map, collects, and then
`result.sort_by_key(|(key, _)| render_keystroke(key))` (53) — `sort_by_key` calls
the key function O(n log n) times and `render_keystroke` returns an owned
`String`, so the sort allocates a string per comparison. `whichkey::render` then
allocates `render_keystroke(keystroke)` and `title_for(registry, action)` (both
`String`) per row (`whichkey.rs:104-117`). Impact: bounded and transient (a chord
prefix is held for a fraction of a second), which is why this is Minor — but it
is on a keyboard path in a keyboard-first app, and the `sort_by_key` allocation
is free to remove. Direction: `sort_by_cached_key`, or sort on a `Copy` key.
Measure: `shell_cores.rs`'s keymap group is the natural home for a
`whichkey_continuations` entry.

**m6c. The 500 ms reload poll does a full `read_dir` + per-file `metadata()` of
the desk and user config directories, forever, on a background thread.**
`crates/geode-shell/src/shell/mod.rs:1268-1271` (the always-on loop) →
`crates/geode-shell/src/reload.rs:40-46` → `scan_dir` (48-70), which
`read_dir`s each directory and calls `entry.metadata()` per `.toml` file, twice a
second for the life of the process. It is correctly off the UI thread
(`background_executor().spawn`, `shell/mod.rs:1381-1384`) and the result is
compared before anything is rebuilt (1404-1416), so an unchanged config costs no
UI work at all — this is well built. The note is about the *desk* directory
specifically: PHILOSOPHY §5 puts desk defaults in "a shared location" and §3
explicitly names "a slow network share" as something that must not stall the app.
Two `read_dir`s plus N `stat`s per second against an SMB/NFS mount is a steady
background load that the app never stops generating, and on a stalled mount the
`await` at 1381 simply never returns — which silently stops the *whole* poll loop,
including the session flush and the flip-barrier sweep that share it (the loop's
own comment at 1274-1290 notes the barrier release is already bounded by the whole
iteration, not the interval). Direction: a filesystem watch where the platform
offers one, or a longer interval for the desk layer than the user layer; at
minimum, run the desk scan on its own task so a hung share cannot take the
session flush with it. Measure: instrument the poll iteration's duration and
report its p95 in the diagnostics tile; on a real share, compare against a local
dir.

**m7. `debug_selector` closures are pervasive on per-cell paths.**
Roughly 40 sites across the render census above. Verified non-issue in release:
the comment at `crates/geode-blotter/src/delegate.rs:951-958` states gpui drops
the closure unevaluated outside test/`test-support` builds, citing
`gpui-pre-0.3.5/src/elements/div.rs`. Recorded so a future reader does not
re-flag them, and so that the claim is tied to the pinned rev — if the
gpui/gpui-component pin moves, this assumption needs re-checking (CLAUDE.md
makes the registry source the API authority).

**m8. `render_td` calls `cx.theme()` per cell in all three tables.**
`crates/geode-blotter/src/delegate.rs:940`,
`crates/geode-pricer/src/delegate.rs:250-253`,
`crates/geode-marketdata/src/delegate.rs:350`. Each is a global lookup plus
`Copy` field reads, not a derivation — the derivations are correctly memoised
(blotter `themed_cell_colour`/`chevron_states`, pricer `Paints`, marketdata
`FlooredTones`). Tolerable; noted because the census asked for repeated
`cx.theme()` resolution specifically.

**M7. Every documented reference measurement has a bench, but no budget-table
path has a whole-frame bench — and the paths this review found are exactly the
ones benchmarks cannot see.**

Verified bench-to-contract mapping (all 17 bench files read):

| `performance.md` reference row | Bench | Shape matches? |
|---|---|---|
| Warm database reopen | `geode-data/benches/ingest.rs:256` `reopen_populated_db` | yes |
| View requery, 1,000,000 rows, depth two | `geode-data/benches/query.rs:423,463` (`for rows in [100_000, 1_000_000]`, `_grouped_scoped_depth_2`) | yes |
| Series query, four slots plus ratio | `geode-data/benches/series_query.rs:185` | yes |
| Series query with stats, two slots | `geode-data/benches/series_query.rs:196` | yes — but see M6, two slots is the *small* stats case |
| Chart path rebuild, 500,000 → 1,600 | `geode-chart/benches/decimate.rs:41-42,63` (`n = 500_000`, `cols = 1_600`) | yes |
| Timeseries chart model, 500k × four | `geode-timeseries/benches/chart_model.rs:36-39` | yes |
| Blotter fully expanded flatten, 720,881 | `geode-blotter/benches/blotter.rs:106` `flatten_all_729k_rows` | yes |
| Market-data pivot build, 20 × 30 | `geode-marketdata/benches/matrix.rs:230` | yes |
| Market-data flat build, 10,000 × five | `geode-marketdata/benches/matrix.rs:235` | yes |
| Market-data cell patch | `geode-marketdata/benches/matrix.rs:281,304` | yes |
| Line-pricer sheet shift + undo, 1,000 | `geode-pricer/benches/core.rs:75` | yes |
| Line-pricer single cell edit + undo | `geode-pricer/benches/core.rs:94` | yes |
| Line-pricer grid build, 1,000 rows | `geode-pricer/benches/core.rs:171` | yes |

That is a genuinely good record: every published number is reproducible from a
committed bench at the stated shape. Fixture construction is correctly hoisted
out of every timed closure I read (`decimate.rs:43-56` before `bench_function`;
`matrix.rs:228,234` likewise; `chart_model.rs:36`; `blotter.rs:101-105`), and
`pricer/benches/core.rs:129-133` uses `iter_batched` with `batch.clone()` in the
*setup* closure so the clone is excluded from the measurement — the right idiom.

Hot paths in or implied by the budgets table with **no bench**:
1. A painted frame. This is `performance.md`'s own first known gap and it is the
   one that matters most, because every *core* is now measured and fast — which
   means the remaining risk has moved entirely into the render composition layer.
   Findings M-1, M0, M1, M2 and M3 all live there, and none of them could have
   been caught by the existing suite.
2. Config-dialog row derivation (M-1, M0). `shell_cores.rs` benches
   `config_edit`-style state transitions and `flush_build_keymap`, but nothing
   calls `keybindings_view::derive_rows` or any `visible_rows`.
3. The blotter's per-cell read path. `blotter.rs:120-131` benches
   `cache_fill_40x{cols}` (filling the window) but nothing benches the
   `render_td`-side `cache.get` + `SharedString::from(Arc::clone(...))` sequence,
   which is what actually runs 40 × columns times per frame.
4. The palette's render side (M1) — the filter curve is benched at 66/500/2000,
   the render is not.
5. `whichkey::continuations` (m6b).
6. Chart element `paint`. `decimate`/`build_path` are benched; `paint_pane`'s
   per-frame grid `Vec`s, `ShapeKey` construction, cache lookup and density-bar
   loop are not. The `geode-chart` crate has a `Plot`-level element with no
   bench harness for it.

Fidelity nit (not a defect): `blotter.rs:123-129` constructs
`FormatCache::default()` *inside* the timed closure, so `cache_fill_40x7` includes
the cache's own allocation. That is arguably what "fill a fresh window" means, but
the published number should say so; it is the one place where a fixture allocation
sits inside a measurement.

Another fidelity note: `query.rs`'s timed `requery` (368-410) submits to the real
`DataService` and blocks on the mpsc receiver, so the 2.51 ms figure includes
worker hand-off and channel latency, plus a `view.to_string()` and
`scope.clone()` per iteration. That is the honest end-to-end shape for the
"view requery" contract — worth keeping, worth stating.

Direction: add a `VisualTestContext` frame bench (the shape of
`shell_cores.rs:342`'s `keystroke_toggle_validate_render`, which already proves
the pattern is available) over a realistic tile set, and a
`keybindings_derive_rows` / `object_visible_rows` pair. Measure: those benches
plus the frame histogram's p95 with each dialog open.

### Idea

**i1. Give the shell chrome the same "prepared model" treatment the modules
already have.** The status bar, sidebar, blotter header/footer and palette rows
are the only surfaces left that format in render. Every module tile already
proves the pattern works (`HeaderModel::prepare`,
`crates/geode-timeseries/src/tile/mod.rs:846-870`; the pricer's `Paints` memo,
`crates/geode-pricer/src/tile.rs:419-426`). A `StatusModel`/`HeaderModel` for the
shell, rebuilt on the events that actually change it, would close M2, M3, m2 and
m3 in one move and give them a benchable seam.

**i0. Two cheap, mechanical wins, in order of payoff per line changed.**
(1) `sort_by_cached_key` in `whichkey.rs:53` — one word, removes an allocation
per comparison. (2) Early-return the empty case in `status.rs:113-117` — three
lines, removes two allocations from every frame the app ever paints. (3) Hold
`theme_name` and the status messages as `SharedString` — removes the remaining
unconditional `to_string()`s from the always-visible surface. None of these
require a design decision.

**i1b. Profile settings and dependency weight are in good order; the one
question is `lto = "thin"` for a latency-sensitive binary.**
`Cargo.toml`'s `[profile.release]` sets `debug = true` and `lto = "thin"`, and
`[profile.bench]` sets `debug = true` — matching CLAUDE.md ("Release and benchmark
profiles retain debug symbols for profiling") and `performance.md`'s closing line.
`codegen-units` is left at the release default of 16; with `lto = "thin"` that is
a deliberate-looking build-time/runtime tradeoff, but for a binary whose whole
thesis is frame latency, `lto = "fat"` with `codegen-units = 1` is worth *measuring*
once — not adopting on faith. Every one of the 14 crates sets `bench = false`
(verified: no `crates/*/Cargo.toml` lacks it), so the CLAUDE.md rule holds with no
exceptions. Duplicate-version count is 93 names of 917 packages, and the
duplication is almost entirely the `windows-*` family (4 versions of `windows`,
`windows-sys`, `windows-core`, …) plus `rand`/`getrandom`/`phf`/`hashbrown`/
`itertools` at 2-3 each — the normal transitive spread of a gpui + DuckDB + arrow
tree, not something this workspace controls or should chase. Dev-dependency
weight: `criterion` pulls `plotters`, `plotters-svg`, `criterion-plot`, `clap`,
`rayon` and `regex` (`Cargo.lock:1588-1601`), and `resvg`/`usvg`/`image` come in
via the gpui asset path; none of it reaches the shipped binary. Nothing to fix —
recorded so the next reviewer does not re-derive it. Measure, if the LTO question
is taken up: build both ways and compare the frame histogram's p95 under a held
key, plus the 1M-row requery bench.

**i2. Bench the frame, not just the cores.** `performance.md`'s first known gap
("A real painted frame is not covered by headless Criterion benchmarks") is the
one that matters most now that every core path is measured and fast. The
existing `keystroke_toggle_validate_render` bench
(`crates/geode-shell/benches/shell_cores.rs:342`) is the right shape to extend:
a `VisualTestContext` frame over a realistic tile set would catch exactly the
chrome-formatting class of regression this review found.

## (c-bis) Status of each gap listed in performance.md

| Documented gap | Status after reading the code | Next measurement it needs |
|---|---|---|
| "A real painted frame is not covered by headless Criterion benchmarks." | **Open, and now the most valuable gap.** Every core path is measured and fast, so the residual risk has migrated entirely into render composition — which is where M-1, M0, M1, M2, M3 all live, none of them visible to the current suite. | A `VisualTestContext` frame bench over a realistic tile set, in the shape of `shell_cores.rs:342`. |
| "Series statistics repeat the bucketing prefix for points, percentiles, and density." | **Open and understated.** `crates/geode-data/src/query/series.rs:400-470` shows it is not only a repeated prefix but a separate `conn.prepare` + `query` *per statistic per slot* — up to 3k round trips for k slots. The published "with stats" number is the two-slot case. | A `series_query/4_slots_with_stats` entry before any rewrite, so the slot-scaling is on record. |
| "Series append deduplication reads every stored version for the pair." | Open (not re-derived in this review — the append path was read only as far as `store/series.rs:191`'s sweep call). | UNVERIFIED here; confirm by timing `append_series` at a pair with many historical versions vs. few. |
| "Measure and document live/archive retention has no production scheduler; the sweep API is exercised by tests." | **Open, unchanged, and confirmed.** `retention::sweep` (`crates/geode-data/src/store/retention.rs:101`) has no production caller anywhere in `crates/` — the only references are three doc comments. Series retention by contrast *does* run on the append path (`store/series.rs:191`), exactly as documented. | Nothing to measure; this needs a scheduler, then a growth measurement under it. |
| "Diagnostics perf rows … Histogram copying compares sample count and maximum, so idle-only changes and a reset/refill with the same count and maximum can be missed." | Open, unchanged, accurately described. Confirmed at `crates/geode-shell/src/shell/mod.rs:1300-1322`: the copy is gated on `watchers() > 0` and `refresh_frame_hist` compares before copying. The comment there records that the gating was itself a fix. | None; the limitation is correctly written down and cheap. |
| "The measured parallel CSV result covers `read_csv`, not the complete staging pipeline." | Open, unchanged. The benches exist (`geode-data/benches/ingest.rs:339,346`, `sequential/` and `parallel_4/`). | Blocked on a real share, as the doc says. |
| "CI compiles benchmarks but has no stable regression baseline." | Open, unchanged, and deliberate — `shell_cores.rs:9-12` records the deferral. | A baseline needs a dedicated runner before it means anything; noisy shared runners would produce false regressions. |

## (d) Systemic patterns

1. **The modules learned the lesson; the shell chrome has not.** Every module
   tile prepares a model outside render and pays only refcounts inside it
   (`geode-pricer/src/delegate.rs:231-236`, `geode-marketdata/src/delegate.rs:339-346`,
   `geode-timeseries/src/tile/mod.rs:1052-1076`, `geode-blotter/src/delegate.rs:330-351`).
   The shell's own surfaces — status bar, sidebar, palette rows, config dialogs,
   blotter header/footer — format and derive in render. The asymmetry is almost
   certainly historical: the modules were built under explicit per-spec budgets
   with benches attached, and the chrome was not. Findings M-1, M0, M1, M2, M3,
   m2, m3 are all one pattern.
2. **"Derived fresh at every call site, never cached" is stated as a virtue in one
   place and as a violation in another.** `objectdialog/mod.rs:3189-3193` defends
   it explicitly; `performance.md` promises "Config dialogs derive small row sets
   on change". Both cannot be right. Whichever is chosen, the two documents need
   to agree, because the next author will follow the comment nearest the code.
3. **Memoisation is done well and done consistently where it was done at all.**
   The 28-value theme signature appears in the blotter
   (`delegate.rs:253-275`), the timeseries tile (`tile/mod.rs:1066`) and the
   market-data panel, each with the same reasoning and the same full-signature
   compare rather than a sentinel. The `ShapeKey`/`ChartKey` pair is the same idea
   at the data level. When this codebase caches, it gets the invalidation key
   right — which is the hard half.
4. **Allocation discipline is documented at the point of use.** Several hot
   functions carry a comment naming the allocation they are avoiding and why
   (`geode-blotter/src/delegate.rs:303-309` on `colour_kind` not cloning a
   `String`; `delegate.rs:697-702` on `tree_glyph` being resolved per window fill
   rather than per paint). This is the "slightly unusual Rust that is fast — with
   a comment explaining why" that PHILOSOPHY §6 asks for, and it is why the
   per-cell paths are clean.
5. **The one always-on wakeup is deliberately consolidated.** The 500 ms poll
   loop (`shell/mod.rs:1268-1437`) absorbed what used to be per-mutation spawned
   timers, and its comment (1274-1290) explains the tradeoff it accepted to do so.
   Every other timer in the workspace is one-shot or visibility-gated. The perf
   overlay has no timer by design. This is the part of §3 the codebase honours
   most carefully.
6. **Row-wise extraction survives only where Arrow was not reached for.** The
   main view path is columnar end to end (`query_arrow` → `RecordBatch` →
   `Snapshot`); the series path and every catalog/metadata query use the row API.
   For metadata that is right-sized; for series points (up to the 500k cap) it is
   the one place PHILOSOPHY §6's "columnar end-to-end" is not yet true.

## (e) What is done well

- **Every published reference number is reproducible.** All 13 rows of
  `performance.md`'s measurement table map to a committed bench at the stated
  shape (table in M7). That is rarer than it sounds and it is what makes the rest
  of the document trustworthy.
- **Fixtures are hoisted out of timed closures**, and where a batch must be
  consumed per iteration the bench uses `iter_batched` with the clone in setup
  (`geode-pricer/benches/core.rs:129-133`) rather than timing the clone.
- **The per-cell render paths in all three tables are genuinely allocation-free**
  beyond refcounts: `SharedString::from(Arc::clone(&cell.text))` in the blotter
  (`delegate.rs:1057`), prepared `SharedString`s in the pricer and market-data
  models, with `Copy` theme reads for colour. The `debug_selector` closures that
  look like violations are dropped unevaluated in release, and the code says so
  with a citation to the pinned rev.
- **`ColourKind` exists specifically so `render_td` need not clone a `String` to
  classify a column** (`geode-blotter/src/delegate.rs:300-315`) — a small type
  introduced purely to keep a hot path clean, with the reasoning recorded.
- **The chart's caching story is complete**: `ShapeKey` includes every geometry
  input, `PathCaches` are per-tile so two charts cannot serve each other's paths
  (`geode-timeseries/src/tile/mod.rs:972-977`), decimation reuses buffers, content
  masks are intersected rather than replaced, and the uncached density bars are
  explicitly bounded at `MAX_DENSITY_QUADS` with the bound enforced mid-loop
  (`geode-chart/src/element.rs:535-537`).
- **The known-gaps section is honest and still accurate.** Seven gaps, none of
  which I found to be quietly fixed or quietly worse — except series statistics,
  which is worse in a way the original wording did not anticipate. A performance
  document whose limitations section survives an adversarial read is doing its job.
- **Locks are nowhere near the render path.** Of the ~30 lock sites outside
  `geode-data`, none is held during render or across an await: `config_write` holds
  a `Mutex` around a file transaction on a background task, `action_tail` is taken
  and released inside `dispatch` (`shell/input.rs:92-96`), `geode-pricing`'s
  overrides `Mutex` lives behind the request/outcome door, and `geode-core::log`'s
  ring is a bounded critical section. Every one uses
  `unwrap_or_else(|e| e.into_inner())` so a poisoned lock degrades rather than
  panicking the UI thread.
- **Frame instrumentation is honest about what it measures.** `perf.rs`'s module
  doc distinguishes render-to-render intervals from frame time, excludes idle gaps
  above a cutoff rather than averaging them in, and the overlay deliberately has no
  timer so that measuring cannot itself cause repaints. The histogram is fixed
  buckets with saturating counters and no allocation.

## Passes: completion status

- Pass 1 (render-path allocation census): **complete**. Every `fn render` /
  `render_td` / `render_th` / `paint` / `prepaint` / `request_layout` in
  non-test, non-bench code was enumerated by grep and each body read. Not read in
  detail: `crates/geode-widgets/src/datefield/paint.rs:57` (a segmented date
  field's paint, open only while the as-of dialog is up) — the only render body in
  the workspace I did not open.
- Pass 2 (per-key and per-delivery paths): **complete**. Key dispatch read end to
  end (`context_stack` → `Matcher::press` → `dispatch`); frame observers read for
  all five module tiles plus the bridge; delivery handling read for pricer,
  blotter and market-data. Notify amplification: the tiles are careful (each
  observer early-returns on `!self.visible` and compares versions before acting),
  and the one asymmetry found is M8.
- Pass 3 (data path): **complete** for snapshot representation (Arrow
  `RecordBatch`, `TreeIndex` built once on the worker), extraction (columnar for
  views, row-wise for series and metadata — M5), and the four formatting caches
  named in `performance.md`, whose invalidation keys I read and found complete.
- Pass 4 (locks on the UI thread): **complete**. ~30 non-`geode-data` sites
  enumerated; none held during render or across an await.
- Pass 5 (timers and wakeups): **complete**. Six timer sites in shell/module code,
  inventoried in m6c and M8.
- Pass 6 (benchmarks): **complete**. All 17 bench files read; contract mapping
  table and the no-bench list are in M7.
- Pass 7 (build/binary): **complete** (i1b).
- Pass 8 (known gaps): **complete** — per-gap status table in section (c-bis).
  One sub-item left UNVERIFIED and labelled as such (series append dedup).
- Pass 9 (ideas): **complete** (i0, i1b, i2).

Finding count: 1 "no Critical" determination, 9 Major (M-1, M0, M1 … M8),
10 Minor (m1 … m7 including m6b, m6c), 3 Idea. Total 22 substantive findings.
