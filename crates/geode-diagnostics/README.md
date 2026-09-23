# geode-diagnostics

The diagnostics module: one tile with five sections over the shell-owned
`Diagnostics` entity and the log ring. It is opened from the status bar's
diagnostics summary or the palette's `Diagnostics: Split` rows. There is
no separate `diagnostics::open` action because tile creation is the one
consistent way to open a module.

## Layout

| Module | Holds |
|---|---|
| `sections` | Pure row builders for the five sections (`sources`, `data`, `config`, `log`, `perf`). Same inputs, same rows; the tile calls one only when an observed version changed. |
| `commands` | The `:` line: `:section <name>`, `:level <target> <level>`, `:overlay`. Pure, no gpui, no I/O. |
| `tile` | `DiagnosticsTile`: observes the `Diagnostics` entity and the frame, rebuilds one row list on a version change, paints it as a `uniform_list` in the mono face. `[`/`]` cycle sections. |

## Commands

```sh
cargo test -p geode-diagnostics
```

## Rules this crate pins

- `Diagnostics::watch()` and `request_catalog()` queue a request but never
  `cx.notify()`; the caller must, in the same update.
- `DiagVersions` is one counter per section, so a perf tick never rebuilds
  the config section.
- The tile never requeries, so it must still self-arrive under the flip
  barrier. Until 2026-09-14 it held every blotter to the 250 ms deadline.
- The sources section paints a source's two detail rows by
  `SourceSummary::shape`, resolved once in the app bridge: a fetch source
  is `adapter` plus `fetch`, never an empty `topics:`.
- Warning and error rows take their colours through
  `geode_shell::shell::chip::chip_paint`, never a semantic foreground
  token over a tint by hand.
