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
| [`lib`](src/lib.rs) | `DiagnosticsPageFactory`: kind, title, icon, action registration, the default keymap fragment (context `diagnostics`), the `mod+d` toggle binding, and the `PageContent` adapter over the page entity. |
| [`section`](src/section.rs) | The five sections in rail order: names, titles, and cycling. |
| [`model`](src/model.rs) | Typed rows per section (`SourceRow`, `DatasetRow`, `DiagnosticRow`, `ConfigDoc`, `LogRow`, `PerfModel`), the badges, and the header chips. Pure: explicit `now` and clock inputs, no GPUI, no I/O. |
| [`prepared`](src/prepared.rs) | `PreparedTable`: the column specs and rows a section paints, with expansion and filtering applied; `cell_at` places a notice row's one cell in the widest column. Pure; `Rc`-shared with the delegate. |
| [`table`](src/table.rs) | `SectionDelegate`, the one `TableDelegate` for every table section: paints a shared prepared table, scales column widths with the window rem, and formats nothing per paint but a parent row's expander. |
| [`page`](src/page.rs) | `DiagnosticsPage`: the observers, the selected section, per-section cursors and filters, the expansion sets, the log tail and its filter, the target select, the Levels state, the cached badge and header strings, the ages timer, key dispatch, visibility, serialization, and the frame layout. |
| [`page_chrome`](src/page_chrome.rs) | The header with its chips and back control, the rail with its badges, and the detail strip with the Copy button. Pointer routes only; every action has a keyboard route in `page`. |
| [`config_view`](src/config_view.rs) | The Config body: the diagnostics panel (Current or History) beside the effective-values table, each with its own toolbar row and detail strip. |
| [`log_view`](src/log_view.rs) | The Log toolbar: level toggles, the target select, the text filter, Follow, Clear, and the Levels popover. |
| [`levels`](src/levels.rs) | The Levels popover's pure rows: the read-only default, then the known targets, each with the effective level resolved by the longest configured prefix, spelled as `LogLevels` stores them. |
| [`perf_view`](src/perf_view.rs) | The Perf body: stat tiles, the histogram bars (`bar_heights` is pure), the database tiles, and the overlay switch. |
| [`log`](src/log.rs) | `LogTail`, a bounded copy of the ring from the sequence at creation (4,096 records, the loss gap measured per drain), and `LogFilter` over level, target, and text. Pure. |

## Interaction

`[`/`]` cycle sections, and a rail row's press selects one. `j`/`k` move the
cursor by one, `ctrl+d`/`ctrl+u` by five, `ctrl+f`/`ctrl+b` and the page
keys by ten, all with count prefixes, and `g g`/`G` jump; the detail strip
follows the cursor. `z o`/`z c` and
Enter expand or collapse the cursor row where it expands (Data datasets and
Config documents); a double-click on a row does the same, and a single click
only selects. `/` focuses the selected section's filter input, which puts
the page's context in `mode == insert`, and Escape there returns to normal
mode; on Perf, which paints no input, `/` is consumed and does nothing.
Escape in normal mode reaches the shell's `page::close`, as does the
header's back control through the shell-actions handle.

Sources filters by name and health; Data by dataset name; Config by
`document.key` and value; Log by message and target text, plus the level
toggles and the target select. The Log section follows new records until
the cursor moves; `G` or the Follow switch resumes following. Clear forgets
the retained records without moving the drain point.

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
- Visibility calls `Diagnostics::watch()`/`unwatch()` and notifies in the
  same update; a watch queues the initial catalog. A visible as-of change
  requests a refresh; a hidden page requests one when it is shown again.
- The app refreshes the factory's shared `Config` in a frame observer
  registered before the page's own, so the config section rebuilds from the
  new document on the same version bump. Preserve that order.
- Paint shares prepared tables through `Rc`. Rail texts, header chips, the
  History label, the copy text, and the title are formatted at rebuild or
  badge refresh, never per paint.
- The tail retains at most 4,096 records and reuses its drain buffer. It is
  drained on every badge refresh while the page is visible, so the Log
  badge is live from any section, and never while hidden, so a wrap during
  a closed page is reported by the drain that shows it again. The loss row
  names the gap measured at the last drain, not a lifetime total.
- A notice row paints its single cell in the table's widest column and
  nothing elsewhere; the table clips every cell to its column.
- `TableState::column()` is read only on `refresh`: every new prepared
  table is followed by one, and render refreshes both tables when the
  window's rem moves off the one the widths were prepared at. The ages
  tick replaces the table without a refresh, because columns do not change.
- The ages timer exists only while the page is visible and Sources is
  selected; dropping the task is what stops it. A tick rewrites the Since
  cells from the retained `since` times and is not a rebuild.
- `set_section` blurs a focused page input (focusing the page handle) and
  closes the Levels popover before switching, because the next section may
  not paint them. It needs a window to set the input's text; nothing syncs
  the input from render, and setting its value emits no `Change`.
- Health ranks by variant, never by `Health`'s derived `Ord`, which
  compares reason text; the header chip counts sources by label, as the
  status summary does. A source known only from an ingest load still gets a
  row.
- The Levels popover spells targets as `LogLevels` stores them, bare
  suffixes without `geode::`. Its default row is read-only, because
  `request_level` files a target and `default` is not one, and it offers no
  add-a-target row, because `[log]` keeps only the known targets across a
  reload.
- Warning and error tones come from `geode_shell::shell::chip::chip_paint`;
  the rail's rows take `listrow::paint_row`; geometry is authored in design
  pixels through `shell::scale`. No literal colors, radii, or unexplained
  pixels.
- The page never holds `ShellView`. Application state changes go through
  the `Diagnostics` request channels or the `ShellActions` handle, whose
  dispatch the shell defers past the page's own entity update.

## Limits

Every table row has one height, so a row's detail lives in the strip below
the table. The Config left panel is pointer-only; the keys stay with the
effective-values table. Clear, Copy, Refresh catalog, Expand all, Collapse
all, the level toggles, and the target select are pointer-only too: they
have no page binding or palette action yet, unlike the Levels picks, the
overlay switch, Open config directory, and Follow. Columns resize but do
not move or sort. The config
explainer shows at most 2,000 leaves per document with an omitted-count
row, but still traverses every leaf. Stopped data threads are shown on the
status bar, not in the Sources section. The Levels popover cannot add a
target.
