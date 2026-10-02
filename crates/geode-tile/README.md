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
| `edit` | `EditCaret` seeds a text cell editor with its caret at the start or end, without selecting text. The tile owns focus and editor lifetime. |
| `confirm` | The in-tile y/n confirm over a `ConfirmHost` (one `Option<Confirm<P>>` slot, `confirmed`, `cancelled`): `arm`, `key` (bare `y` confirms; every other key cancels and is consumed), `answer` (the one yes/no behind the keys and the buttons), `cancel` (pointer, blur, a verb from elsewhere), `withdraw` (no `Window`: drops the blur answer at once, defers the blur, answers nothing), `prompt` (the focused question in the foreground color, a press on it taking no focus, and its Yes/No buttons, selected `{selector}-yes`/`-no`, whose press moves no focus), `cancel_on_press` (capture-phase press on the tile root, except on a button as last painted; the gap between them cancels). Every answer blurs the prompt before its handle drops; the shell's restoration path, not the door, returns the keyboard to the tile. |
| `following` | The flip-barrier state machine a following tile runs for its own query: `FollowingQuery<T>` (the versions last asked under, the in-flight instant, the result held for the barrier, the last flip seen, the tag) with `on_flip` (followed counters decide, not flip identity; promotion runs before the tile's visibility check), `follows_changed`, `self_arrive` (never for a same-identity query still out), `begin` (drops the stage), `submitted` (`Unanswered::Retry` arrives then forgets; `KeepActed` arrives and remembers), `deliver` (a stale tag is not an arrival; a failure arrives; a releasing arrival promotes at once, and is `Superseded` if a followed counter moved since it was asked; an answer no barrier wants is `Superseded` if a followed counter moved since it was asked, the same check promotion makes), `reset`, `close` (the tile's `closed`, never its hide: supersede, then answer any barrier still waiting, under the frame's current versions); `arrive_immediately` for tiles that submit no frame query; the `Barrier` trait over `FrameViewMut` (pure tests) and `FrameDoor` (a tile's `FrameRef`, notifying on release). Both read one tile's versions: its workspace's lane, with the scope generation of the link group it follows. A tile in a pinned workspace therefore answers the barrier with its own lane's versions and a follower with its group's; the barrier itself is frame-wide and holds each awaited tile to its own reading. Methods return decisions; the tile applies results and formats notices. |
| `grid` | `WindowCache<C>`: the cells a grid table shows, prepared outside render — one row-major `Vec` with a stride, a move keeps the rows still in view and fills only rows entering it, a column-count change clears it, `get` outside the window is `None`, `refill_cell` re-prepares one cell. `WindowRequest`: the range the table last reported, refilled after an invalidation (the pinned table never re-reports an unchanged range or one of length ≤ 1), clamped to the rows that exist, or, when a shrink leaves no recorded row, the last rows of the same height; `with_first(FIRST_WINDOW)` prepares the first 64 rows before any report. `RowCache<C>`: the cells of a set of rows that need not be contiguous, keyed by document row — a fuzzy `/` result table's ranking — where `set_rows` keeps rows still reported, fills only rows entering and drops the rest. The cell type stays each module's own. |
| `header` | The strip every tile header is: `HEADER_HEIGHT` (22 at the design rem), `frame(marker, left, cluster)` (marker, the module's left side taking the width the cluster leaves, with a zero minimum and clipping its overflow; then the cluster's status items and notices, which shrink, one line each cut with an ellipsis, within at most `TEXT_SHARE` (half) of the header; then times, the link chips, the health chip and `⋯`, which never shrink — neither a long left side nor a long notice pushes them off the tile, which they leave only when the tile is narrower than they are), and the cluster in a fixed order — the mode icon (`Cluster::mode`, a `Mode` the module reads from its key context's `mode` through `Mode::from_key_mode`: `insert` paints a pencil in the floored warning text tone, `visual` a dashed selection square in the floored info text tone, `normal` and `menu` nothing; no fill, never shrinks, tooltip `Editing` / `Visual selection` with "`escape` leaves" (the default leaving key; a rebound one is not reflected); selectors `tile-mode-edit-{tile}` and `tile-mode-visual-{tile}`), `status` (module items), `notices` (the notice door), `times` (`TimeRun`: prepared label and optional stale label; stale takes the warning text tone, and the run's selector `time_selector` is `tile-time-{tile}-{i}`, suffixed `-stale` while it paints that tone), the link chips (`Cluster::links`, which a module fills from `link_chips(&frame, cx)` at paint and never stores: one chip per link group the tile follows or emits into, the group's letter with a down arrow for follow, an up arrow for emit and both for one group both ways, on a solid fill in the group's color (`geode_shell::link::group_color`, painted unchanged through `chip::colored` so the bundled-theme sweep guards the painted value) with text floored to text contrast on that fill; they never shrink, take no press, and their tooltip names `tile::link_group` with its live key; selectors `tile-link-{tile}-{letter}-{follow|emit|both}`; a frame handle bound to no tile has none), the health chip, the `⋯` `MenuTrigger`. `HealthWatch` keeps the last `DiagVersions::sources` read and the `HealthChip` prepared from `Diagnostics::health_for_*`: `refresh` re-asks only when that version moved, `reask` when the tile's question changed. The chip shows only while a read source is PendingTooLong (`pending`), Degraded or Failed (Warning, Warning, Danger tones), tooltips `<source>: <reason>` and `+N more`, and a click queues `Diagnostics::request_diagnostics_page`, which the shell opens and never closes. The frame formats nothing. |
| `menu` | Re-exported from `geode_shell::menu`, which owns it (the shell paints its row menu with it). The `.` action menu: `Row` (`Action`/`Separator`/`Section`), `ActionRow` (pick, title, `Hint` resolved to a `Lane` through the live keymap, `enabled` with its reason, optional short reason, `checked` tick slot read back as `tick()`), `Menu` (highlight, `step`, `highlight`, `pick`, `replace_rows`, `rehint`), and `render_menu` over a `MenuHost`. Stepping lands only on enabled actions, and from a non-action row on the first enabled one; an all-disabled menu has no cursor; a rebuild snaps the highlight to the nearest action. Which keys step it are the shared `motion::menu_down`/`menu_up` (the shell's builtin bindings under `tilelist`, which the tile publishes while the menu is open); what picks and closes stays the module's. |
| `motion` | The shared motion vocabulary: the `motion::*` id constants, `HALF_PAGE`/`FULL_PAGE`, `Motion`, `parse` (id and count to a motion; menu steps are not grid motions), `row` (a bare single step wraps unless selecting; counted moves and pages clamp; counted top/bottom is row N; an empty axis is a no-op) and `col` (clamps). The keys are the shell's builtin bindings under `grid`/`tilelist`; quirks around the result stay the tile's. |
| `notice` | `Notice` (prepared text and a `Tone`: `Status`, `Warning`, `Danger`) and its one paint in theme tokens; `truncated` is the width-bound form (one line, ellipsis, the whole text in a tooltip) the header cluster uses. Precedence between a tile's notice slots stays the tile's. |
| `stale` | `StaleTimer`: wake-ups at `source_at + stale_after` for a set of source times (`arm_each`; `arm` for one), armed after each delivery and on show (idempotent for the same set and threshold), dropped on hide (`disarm`) and when the tile drops (it owns its `Task`). One wake-up is pending at a time, the earliest still ahead; when it fires it records that time (and every earlier one) stale, notifies the tile and arms the next, so each time turns stale at its own deadline. Render ORs `fired_for(at, after)` with its clock comparison, so the verdict holds where the wall clock disagrees. Arming a different set or threshold clears the verdict, so a fresh delivery never paints stale. Used by market-data (its painted generation) and the blotter (every dataset time); the pricer keeps its own reprice timer. |
| `popover` | Re-exported from `geode_shell::popover`, which owns it. Popup geometry (`ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH`, `SNAP_MARGIN`), the popover `surface`, `anchor_popup` (deferred, anchored, snapped, priority 1), and the `row_shell`/`empty_row` row frames. |

Used by the pricer (all four doors, `colour`, `motion` for its grid cursor,
`following::arrive_immediately` at flip barriers, and `grid` for its windowed
cells), market-data (all four, `motion` for its grid cursor, `following` for
its document request, `grid` for its windowed cells, and `stale` for its
source time), timeseries
(popover, menu, notice, and `following` for its series query), the blotter
(notice, `colour`, `motion` for its grid cursor, `following` for its view
query, `grid` for its windowed cells, and `stale` for each dataset
time) and the diagnostics page (`motion` for its row cursor
only: it has no popover, menu, confirm or notice line, and submits no frame
query). Every module tile (pricer, market-data, timeseries, blotter) paints its
header through `header`.

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
