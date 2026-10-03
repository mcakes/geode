# geode-diagnostics

The diagnostics page: five sections over the shell-owned `Diagnostics`
entity, the log ring, the loaded configuration, and the frame's requery
statistics, hosted through the shell's page seam. Open it with `mod+d`, the
sidebar's diagnostics button, the palette's "Diagnostics: Open page" row, or
the status bar's diagnostics summary.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#diagnostics).
The seam it sits on: [pages](../../docs/current/shell.md#pages).

## Layout

| Module | Holds |
|---|---|
| [`lib`](src/lib.rs) | `DiagnosticsPageFactory`: kind, title, icon, action registration, the default keymap fragment (`diagnostics && mode == normal` for the bare keys, `diagnostics && mode == insert` for Escape), the `mod+d` toggle binding, and the `PageContent` adapter over the page entity. |
| [`section`](src/section.rs) | The five sections in rail order: names, titles, and cycling. |
| [`model`](src/model.rs) | Typed rows per section (`SourceRow`, `DatasetRow`, `DiagnosticRow`, `ConfigDoc`, `LogRow`, `PerfModel`), the badges, and the header chips. Pure: explicit `now` and clock inputs, no GPUI, no I/O. |
| [`prepared`](src/prepared.rs) | `PreparedTable`: the column specs and rows a section paints, with expansion and filtering applied; `cell_at` places a notice row's one cell in the widest column. Pure; `Rc`-shared with the delegate. |
| [`table`](src/table.rs) | `SectionDelegate`, the one `TableDelegate` for every table section: paints a shared prepared table, scales column widths with the window rem, and formats nothing per paint but a parent row's expander. |
| [`page`](src/page.rs) | `DiagnosticsPage`: the observers, the selected section, per-section cursors and filters, the expansion sets, the log tail and its filter, the target select, the Levels state, the cached badge and header strings, the ages timer, key dispatch, visibility, serialization, and the frame layout. |
| [`page_chrome`](src/page_chrome.rs) | The breadcrumb header and Back control, native section buttons with counts, wrapped and scrollable row details with Copy, and the shell-style keyboard hints. |
| [`config_view`](src/config_view.rs) | The Config body: Current issues, History, and Effective values share one full-width table region. Keyboard motions and Copy follow the visible view. |
| [`log_view`](src/log_view.rs) | The Log toolbar: level toggles, the target select, the text filter, Follow, Clear, and the Levels popover. |
| [`levels`](src/levels.rs) | The Levels popover's pure rows: the read-only default, then the known targets, then any configured target outside that list (a hand-edited `[log]` key, shown but not offered for adding), each with the effective level resolved by the longest configured prefix, spelled as `LogLevels` stores them. |
| [`perf_view`](src/perf_view.rs) | The Performance body and its prepared readouts: aligned percentile and sample-count columns, a labeled frame-interval histogram, storage metrics, dropped events and refused requests (both warning-toned when non-zero), and the overlay switch in a scrolling region. |
| [`log`](src/log.rs) | `LogTail`, a bounded copy of the ring from the sequence at creation (4,096 records, the loss gap measured per drain), and `LogFilter` over level, target, and text. Pure. |

## Interaction

`[`/`]` cycle sections, and a rail row's press selects one. The page
publishes `grid` beside its mode, so the cursor takes the shell's shared
`motion::*` bindings (`j`/`k` and the arrows, `g g`/`G`, `ctrl+d`/`ctrl+u`,
`ctrl+f`/`ctrl+b` and the page keys, with count prefixes), applied to rows by
`geode_tile::motion`; column motions are ignored, and the detail strip
follows the cursor. The fragment binds only the page's own keys. `z o`/`z c`,
Enter, and Space expand or collapse the cursor row where it expands (Data
datasets and Config documents); a double-click on a row does the same, and a single
click only selects. `/` focuses the selected section's filter input, which
puts the page's context in `mode == insert`. Enter keeps the filter and
Escape restores its entry value; both return to normal mode. Clicking a row
keeps the filter and returns focus to row navigation. On Performance, which
paints no input, `/` is consumed and does nothing. Escape in normal mode reaches the shell's `page::close`, as does
the header's back control through the shell-actions handle. The retired
`diagnostics::` motion ids are registered as renames (`RENAMED_ACTIONS`).

`g s`, `g d`, `g c`, `g l`, and `g p` jump directly to a section.
`tab` / `shift+tab` (and `ctrl+tab` / `ctrl+shift+tab`) step the section's
views, wrapping; only Config has views (Current issues, History, Effective
values), and elsewhere the keys are consumed and do nothing. `y` copies the
active row's full details in any table; `r` refreshes the catalog;
`z shift+r` / `z shift+m` expand or collapse all datasets; `o` in Config
opens the config directory. In Log, `-` / `=` step the minimum shown level
(ERROR stays), `t` / `shift+t` step the target filter, `f` toggles Follow,
`ctrl+l` clears the log, and `shift+l` opens the shell's `log::level`
chooser, because the Levels popover's buttons take no keyboard focus.
Escape over an open popover closes it and keeps the page.
`alt+backspace` and Reset filters clear the visible section’s filters and
return focus to navigation. In Log this also restores all levels and targets;
other sections retain their filters. Toolbar tooltips name each control's
key; the footer names the section's main keys.

Sources filters by name and health; Data by dataset name and generation
fields (partition/book label, generation ID, source/load time, row count,
and live/archive status); Config by issue text or `document.key` and value;
Log by message and target text, plus the level toggles and the target select. Data matching is case insensitive: a
matching dataset includes all its generations; a leaf-only match shows just
matching generations beneath their dataset. A nonempty filter temporarily
expands results without changing the stored collapse state. Clearing it restores
the stored expansion, and dataset totals always describe the full catalog.
Configuration matching is case insensitive, hides unmatched documents, and
reveals matches in collapsed
documents without losing their collapse state. Selection follows row identity
through refreshes and filtering while the selected row remains visible. The Log section follows new records until a
row motion; a bare `G`, `f`, or the Follow switch resumes following, and a
counted `G` jumps to that row without following. Clear forgets
the retained records without moving the drain point.

The result strip shows visible versus total sources, datasets, issues, or
retained log records. Dataset counts exclude generation rows; log record
counts exclude loss notices. Effective values reports documents and visible
leaves, so collapsed values and omitted leaves are not counted as shown.
Details identifies the selected row’s position. Log also names whether
following is active and shows the retention limit.

Performance labels each timing stage and its own sample count. Frame intervals
measure time between shell renders, not pure UI work, so the 8 ms UI budget
does not color their histogram as failures. The axis labels show logarithmic
bucket upper bounds; hover gives ranges and counts, and samples above 100 ms
have a separate readout. Empty timing stages show dashes and zero samples.
Percentiles are bucket estimates, and storage values come from the last
catalog snapshot. The page retains the existing perf invalidation limits.

Three controls change application state, each through a channel the shell
owns: a Levels pick and the overlay switch queue requests on the
`Diagnostics` entity, Refresh catalog queues an explicit catalog request the
bridge serves, and Open config directory dispatches `config::open_directory`
through the shell-actions handle.

## Persistence

`serialize` writes `section = "<name>"` into `[pages.diagnostics]`; an
unknown or missing section restores as Sources. Filters, cursors, expansion,
the level toggles, the target, and the retained tail are not saved. They do
survive a close and reopen, because the shell keeps the page for the
window's lifetime.

## Commands

```sh
cargo test -p geode-diagnostics
# The headless rebuild reading recorded in docs/perf.md:
cargo test -p geode-diagnostics --release -- --ignored log_rebuild_timing --nocapture
```

## Rules this crate pins

- Observers compare only the selected section's inputs: its `DiagVersions`
  counter, plus the frame as-of for Data, the frame config version for
  Config, or new ring records for Log. Clock changes and local section,
  filter, or expansion changes also rebuild. A perf tick never walks the
  config documents. Badges and header chips refresh on any counter change
  or new record, whatever section is shown.
- A hidden page builds nothing: the page is retained for the window's
  lifetime, so its observers move their version baselines and return while
  it is closed, and `set_visible(true)` rebuilds once with everything that
  arrived meanwhile. A clock change waits for the show the same way.
- Fragment keys spell modifiers with `+` and shift explicitly (`alt+backspace`,
  `z shift+r`); the keystroke parser refuses `alt-backspace` and `z R`, and
  `geode-app`'s production keymap test fails on any refusal.
- Every bare-key table in the fragment carries `mode == normal`. The
  page's context carries `mode == insert` while the filter holds focus,
  and the shell's insert route resolves bare keys against every context
  carrying that pair, so a table on the bare context would fire `j`, `G`,
  or `enter` inside the filter. Only Escape lives in the insert table.
- Both `DataTable`s paint inside `table::table_el`, whose capture-phase
  mouse-down calls `prevent_default`: the table's root tracks its own
  focus handle, and a row click that moved focus into it would hand the
  next Escape to the table's `Cancel`, which clears the selection and
  stops there, so the page would close only on the second press. The row's
  own click (select, double-click) still fires. The wrapper explicitly
  focuses the page so a row click also exits a focused filter.
- Visibility calls `Diagnostics::watch()`/`unwatch()` and notifies in the
  same update; a watch queues the initial catalog. A visible as-of change
  requests a refresh; a hidden page requests one when it is shown again.
- The app refreshes the factory's shared `Config` in a frame observer
  registered before the page's own, so the config section rebuilds from the
  new document on the same version bump. Preserve that order.
- Paint shares prepared tables through `Rc`. Rail texts, header chips, the
  History label, result summaries, detail position, copy text, and title are
  formatted at rebuild, selection change, or badge refresh, never per paint.
  Performance readouts, bar heights, and tooltip text are prepared at rebuild.
- The tail retains at most 4,096 records and reuses its drain buffer. It is
  drained on every badge refresh while the page is visible, so the Log
  badge is live from any section, and never while hidden, so a wrap during
  a closed page is reported by the drain that shows it again. The loss row
  names the gap measured at the last drain, not a lifetime total.
- A notice row paints its single cell in the table's widest column and
  nothing elsewhere; the table clips every cell to its column.
- `TableState::column()` is read only on `refresh`: every rebuilt table
  is followed by one, and render refreshes both tables when the
  window's rem moves off the one the widths were prepared at. The ages
  tick replaces the table without a refresh, because columns do not change.
- The ages timer exists only while the page is visible and Sources is
  selected; dropping the task is what stops it. A tick rewrites the Since
  cells from the retained `since` times and is not a rebuild.
- `set_section` blurs a focused page input (focusing the page handle) and
  closes the Levels popover before switching, because the next section may
  not paint them. It needs a window to set the input's text; nothing syncs
  the input from render, and setting its value emits no `Change`.
  `set_visible(false)` closes the popover too, so a closed page cannot
  reopen with it armed.
- Health ranks by variant, never by `Health`'s derived `Ord`, which
  compares reason text; the header chip counts sources per variant, as the
  status summary does. Health is ranked by `Health::severity` (geode-core),
  the same rank the status summary and a tile's header chip use. A source
  known only from an ingest load still gets a row.
- The Levels popover spells targets as `LogLevels` stores them, bare
  suffixes without `geode::`. Its default row is read-only, because
  `request_level` files a target and `default` is not one, and it offers no
  add-a-target row, because `[log]` keeps only the known targets across a
  reload.
- Warning and error tones come from `geode_shell::shell::chip::chip_paint`;
  navigation and toolbar controls use native Button variants. Geometry uses
  relative helpers or `shell::scale`; colors and radii come from the theme.
- The page never holds `ShellView`. Application state changes go through
  the `Diagnostics` request channels or the `ShellActions` handle, whose
  dispatch the shell defers past the page's own entity update.

## Limits

Every table row has one height; full details wrap and scroll below it and
can be copied; the detail strip scrolls only by pointer. The level keys reach
contiguous minimum-severity sets only; a non-contiguous set of levels still
needs the toggles. Columns
resize but do not move or sort. The config explainer shows at most 2,000
leaves per document with an omitted-count row, but still traverses every
leaf. Stopped data threads are shown on the status bar, not in Sources.
The Levels popover cannot add a target.
