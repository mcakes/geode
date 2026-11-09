# geode-calibration

The calibration graph tile: one pipeline of the `calibration` document over
one underlying, drawn as a DAG of stages. The tile computes no fit itself:
every run goes through the data tier's calibration door
(`DataHandle::calibrate`) and comes back as a `Delivery::Calibration`.

This crate is at its first slice: the factory, the config push, the header
and a placeholder body. The graph layout, the data flow, the stage
painting, the verbs and the edits follow in later slices; the keys and
actions below are registered and bound now so a binding, a menu row or a
footer hint can name a verb before the tile carries it out.

## Module map

- `lib.rs`: `KIND` (`calibration`) and `init`, which reserves `tab` and
  `shift-tab` in the picker fields' context (`tile::PICKER_CONTEXT`) for
  completion.
- `content.rs`: `CalibrationConfig` (what the app pushes: the pipelines
  `from_doc` accepted and `check_against` kept, the schema the service
  serves, the egress targets with the documents each accepts, and every
  finding of both reads), `CalibrationFactory` (kind `calibration`, title
  `Calibration`, accepts `underlying_ref`, launch table `{ underlying =
  "<u>" }`, `set_config` to store a config and tell every live tile), the
  tile's `TileContent` door (`follows()` is `true`), `ACTIONS` and the
  `DEFAULT_KEYMAP` fragment.
- `tile/`: the hosted entity and its tests. `tile/tests/harness.rs` hosts
  the tile as the shell does: built by the factory, keys resolved through
  the app's keymap with the module's fragment spliced in, deliveries
  through `TileContent::deliver`, config through the factory.

## Commands

```sh
cargo test -p geode-calibration
```

## What it paints

The header: `Calibration · {underlying}` (the followed group's single
underlying, else the tile's own), a muted `{n} stages · {k} fits` for the
shown pipeline (`k` counts its `kalman_fit` stages), then the shared
cluster: the mode icon, the config's findings as notices (a dropped
pipeline is a warning, a structural error danger), link chips, the health
chip for the source stages' datasets, the `⋯` trigger and the shell's ×.
The `⋯` trigger opens nothing yet.

The body: an empty surface for the graph while a pipeline is shown, or
`no pipeline` when the config has none of the tile's name (or no config
has been pushed). The footer row is empty.

## Keys

Every action is bound in the fragment; none but `cancel` does anything yet.

| Key | Action |
|---|---|
| `h` / `left`, `l` / `right` | Cursor to an input, to a consumer |
| `k` / `up`, `j` / `down` | Cursor up and down among the stages of one depth |
| `1`..`9` | Cursor to stage N |
| `/` | Filter stages |
| `shift+l` | Relayout the graph |
| `r`, `shift+r` | Run from the cursor, run the whole pipeline |
| `shift+p` | Publish |
| `c`, `shift+c` | Connect, disconnect an input |
| `a`, `d` | Add, delete a stage |
| `.` | Action menu |
| `escape` | Dismiss the header's notices (`calibration::cancel`) |

Under `mode == insert` (the filter and the picker fields) `enter` commits,
`escape` cancels and `up`/`down` step; under `mode == menu` with `popup ==
actions` `enter` picks, `escape` closes and `.` toggles; with `popup ==
list` `enter` applies and `escape` discards, the steps being the shared
ones.

## Commands and session

The `:` line reads no command yet; every line is refused `unknown command`.

The session table holds `underlying`, `pipeline` and `cursor`, each while
set. A record without `pipeline` means the config's first pipeline, and
once the tile knows which that is it saves the name. The launch table
`{ underlying = "<u>" }` restores through the same reader.

## Failure states

- `no pipeline` in the body: the config holds no pipeline of the tile's
  name, or none has been pushed.
- A header notice per config finding, in the message the diagnostics page
  shows (`calibration: pipeline 'p' dropped: …`); a click or `escape`
  dismisses it, and it shows again when the finding leaves and returns.

## Invariants

- The key context does not opt into counts: the bare digits are stage
  jumps, and a counting context would swallow them.
- The tile reads no `Config` itself: everything it needs arrives in
  `CalibrationConfig`, pushed whole, so it never reads a half-updated
  configuration.
- A `set_config` push prunes closed tiles and holds no borrow across a
  tile's update; the tile reads the config just stored.
- Paint formats nothing: the title, the counts, the health question and
  the notices are prepared when the tile is built, when a config is pushed
  and when the followed group's underlying changes.
- A registered action's key is the tile's whether or not the verb acts
  yet: `dispatch` answers `true` for every `calibration::*` id, except
  `cancel` with nothing to dismiss.
