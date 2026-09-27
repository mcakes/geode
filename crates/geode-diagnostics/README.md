# geode-diagnostics

The diagnostics module: one tile with five sections over the shell-owned
`Diagnostics` entity and the log ring. Open it from the status bar's
diagnostics summary or the palette's `Diagnostics: Split` rows.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#diagnostics).

## Layout

| Module | Holds |
|---|---|
| [`lib`](src/lib.rs) | Module factory, action registration, default keymap, and `TileContent` adapter. |
| [`sections`](src/sections.rs) | Pure row builders for `sources` (led by any stopped data threads), `data`, `config`, `log`, and `perf`; time and clock values are explicit inputs. |
| [`commands`](src/commands.rs) | Parser and word completions for `:section <name>`, plus refusals for application-wide commands. No GPUI or I/O. |
| [`tile`](src/tile.rs) | Observers, selected section, filter, cursor, log tail, and prepared rows rendered through `uniform_list`. |
| [`section`](src/section.rs) | The page's five sections in rail order: names, titles, and cycling. |
| [`model`](src/model.rs) | Typed row models per section for the page (`SourceRow`, `DatasetRow`, `DiagnosticRow`, `ConfigDoc`, `LogRow`, `PerfModel`), plus badges and header chips. Pure: explicit time and clock inputs. |
| [`log`](src/log.rs) | The page's retained log tail (`LogTail`, capped at 4,096 records, loss gap measured per drain) and `LogFilter` over level, target, and text. |

## Interaction and persistence

`[`/`]` cycle sections; `:section <name>` selects one directly. `j`/`k`,
`gg`/`G`, and page bindings move the cursor, with count prefixes for row and
page movement. `zo`/`zc` expand or collapse datasets in the data section.
The log follows new records until the cursor moves; `G` resumes following.

`/` supplies a substring filter used by the config and log sections. The
header shows when a filter is set. Only the selected section and filter are
saved in the session; cursor, collapsed datasets, and log records are not.

`:level` and `:overlay` return messages directing users to `Set log level…`
and `Toggle performance overlay` in the palette. They are not offered as
completions because tile commands cannot change application state.

## Commands

```sh
cargo test -p geode-diagnostics
```

## Rules this crate pins

- Visibility changes call `Diagnostics::watch()`/`unwatch()` and notify in
  the same update. A watch queues the initial catalog. Visible as-of changes
  use `request_catalog_refresh()` and notify; hiding the last tile cancels
  watched demand while explicit consumers retain their own requests.
- Observers compare only the selected section's inputs: its `DiagVersions`
  counter, plus frame as-of for data, frame config for config, or the ring
  sequence for logs. Clock changes and local section/filter/collapse changes
  also rebuild. A perf tick does not rebuild config rows.
- The app refreshes the factory's shared config before tile frame observers
  rebuild the config section. Preserve that observer registration order.
- The tile submits no view query, so it signals its own arrival at the flip
  barrier. Otherwise other tiles would wait for the 250 ms deadline.
- Historical resolved-generation markers display only when the catalog and
  frame as-of match; an outstanding refresh must not show a stale selection.
- The sources section paints a source's two detail rows by
  `SourceSummary::shape`, resolved once in the app bridge: a fetch source
  is `adapter` plus `fetch`, never an empty `topics:`.
- Warning and error rows take their colors through
  `geode_shell::shell::chip::chip_paint`, never a semantic foreground
  token over a tint by hand.
- Paint shares prepared rows through `Rc` and cached header/title strings.
  Log drains reuse their scratch allocation and retain at most 4,096 records
  per tile. A loss warning reports the gap measured at the last drain, not
  a lifetime total or records overwritten before the tile opened.

## Limits

Source ages are sampled at rebuild time; they do not tick while a source is
quiet. The absolute timestamp remains available beside the age. The config
explainer displays at most 2,000 leaves per document with an omitted-count
row, but still traverses all leaves before applying that cap.
