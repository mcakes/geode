# geode-tile

The kit tile modules are built from. A tile mechanism two modules would
otherwise each write lives here, and that covers interaction behavior (keys,
focus, open and close, precedence), not only paint. `geode-shell` hosts
tiles and never depends on this crate; this crate never depends on
`geode-data` or a feature module. The menu and popover are the shell's own
(its row menu is built on them) and are re-exported here, so module code
keeps naming `geode_tile::menu` and `geode_tile::popover`.

Current architecture:
[`docs/current/architecture.md`](../../docs/current/architecture.md).

## Modules

| Module | Holds |
|---|---|
| `colour` | `ColourCache`/`Resolved`: one resolve per named colour per theme input, shared by the blotter and pricer cell painters. |
| `confirm` | The in-tile y/n confirm over a `ConfirmHost` (one `Option<Confirm<P>>` slot, `confirmed`, `cancelled`): `arm`, `key` (bare `y` confirms; every other key cancels and is consumed), `answer` (the one yes/no behind the keys and the buttons), `cancel` (pointer, blur, a verb from elsewhere), `withdraw` (no `Window`: drops the blur answer at once, defers the blur, answers nothing), `prompt` (the focused question in the foreground color, a press on it taking no focus, and its Yes/No buttons, selected `{selector}-yes`/`-no`, whose press moves no focus), `cancel_on_press` (capture-phase press on the tile root, except on a button as last painted; the gap between them cancels). Every answer blurs the prompt before its handle drops; the shell's restoration path, not the door, returns the keyboard to the tile. |
| `following` | The flip-barrier state machine a following tile runs for its own query: `FollowingQuery<T>` (the versions last asked under, the in-flight instant, the result held for the barrier, the last flip seen, the tag) with `on_flip` (followed counters decide, not flip identity; promotion runs before the tile's visibility check), `follows_changed`, `self_arrive` (never for a same-identity query still out), `begin` (drops the stage), `submitted` (`Unanswered::Retry` arrives then forgets; `KeepActed` arrives and remembers), `deliver` (a stale tag is not an arrival; a failure arrives; a releasing arrival promotes at once, and is `Superseded` if a followed counter moved since it was asked; an answer no barrier wants is `Superseded` if a followed counter moved since it was asked, the same check promotion makes), `reset`, `close` (the tile's `closed`, never its hide: supersede, then answer any barrier still waiting, under the frame's current versions); `arrive_immediately` for tiles that submit no frame query; the `Barrier` trait over `FrameViewMut` (pure tests) and `FrameDoor` (a tile's `FrameRef`, notifying on release). Both read one workspace's lane, so a tile in a pinned workspace answers the barrier with its own lane's versions; the barrier itself is frame-wide. Methods return decisions; the tile applies results and formats notices. |
| `menu` | Re-exported from `geode_shell::menu`, which owns it (the shell paints its row menu with it). The `.` action menu: `Row` (`Action`/`Separator`/`Section`), `ActionRow` (pick, title, `Hint` resolved to a `Lane` through the live keymap, `enabled` with its reason, optional short reason, `checked` tick slot read back as `tick()`), `Menu` (highlight, `step`, `highlight`, `pick`, `replace_rows`, `rehint`), and `render_menu` over a `MenuHost`. Stepping lands only on enabled actions, and from a non-action row on the first enabled one; an all-disabled menu has no cursor; a rebuild snaps the highlight to the nearest action. Which keys step it are the shared `motion::menu_down`/`menu_up` (the shell's builtin bindings under `tilelist`, which the tile publishes while the menu is open); what picks and closes stays the module's. |
| `motion` | The shared motion vocabulary: the `motion::*` id constants, `HALF_PAGE`/`FULL_PAGE`, `Motion`, `parse` (id and count to a motion; menu steps are not grid motions), `row` (a bare single step wraps unless selecting; counted moves and pages clamp; counted top/bottom is row N; an empty axis is a no-op) and `col` (clamps). The keys are the shell's builtin bindings under `grid`/`tilelist`; quirks around the result stay the tile's. |
| `notice` | `Notice` (prepared text and a `Tone`: `Status`, `Warning`, `Danger`) and its one paint in theme tokens. Precedence between a tile's notice slots stays the tile's. |
| `popover` | Re-exported from `geode_shell::popover`, which owns it. Popup geometry (`ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH`, `SNAP_MARGIN`), the popover `surface`, `anchor_popup` (deferred, anchored, snapped, priority 1), and the `row_shell`/`empty_row` row frames. |

Used by the pricer (all four doors, `colour`, `motion` for its grid cursor,
and `following::arrive_immediately` at flip barriers), market-data (all four,
`motion` for its grid cursor, and `following` for its document request),
timeseries (popover, menu, notice, and `following` for its series query),
the blotter (notice, `colour`, `motion` for its grid cursor, and `following`
for its view query) and the diagnostics page (`motion` for its row cursor
only: it has no popover, menu, confirm or notice line, and submits no frame
query).

The crate takes a submission's outcome as `submitted: bool` rather than
depending on `geode-data`: every refusal path uses the data service's
`Refusal` only to word a notice, which the tile writes itself.

## Menu hints

A row's hint is an action identity, not a string. `Menu::new`,
`replace_rows` and `rehint` resolve it against the bindings they are given —
modules pass `menu::live_bindings(cx)`, the shell's last `Chords` publish —
so a user rebind shows. A module re-resolves an open menu from its `Chords`
observer. An action bound nowhere shows its `Unbound` form: nothing, or its
`:` verb.

## Commands

```sh
cargo test -p geode-tile
```
