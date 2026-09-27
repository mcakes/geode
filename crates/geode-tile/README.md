# geode-tile

The kit tile modules are built from. A tile mechanism two modules would
otherwise each write lives here, and that covers interaction behavior (keys,
focus, open and close, precedence), not only paint. `geode-shell` hosts
tiles and never depends on this crate; this crate never depends on
`geode-data` or a feature module.

Current architecture:
[`docs/current/architecture.md`](../../docs/current/architecture.md).

## Modules

| Module | Holds |
|---|---|
| `confirm` | The in-tile y/n confirm over a `ConfirmHost` (one `Option<Confirm<P>>` slot, `confirmed`, `cancelled`): `arm`, `key` (bare `y` confirms; every other key cancels and is consumed), `cancel` (pointer, blur, a verb from elsewhere), `withdraw` (no `Window`: drops the blur answer at once, defers the blur, answers nothing), `prompt` (the focused question in the foreground color; a press on it takes no focus), `cancel_on_press` (capture-phase press on the tile root). Every answer blurs the prompt before its handle drops; the shell's restoration path, not the door, returns the keyboard to the tile. |
| `menu` | The `.` action menu: `Row` (`Action`/`Separator`/`Section`), `ActionRow` (pick, title, `Hint` resolved to a `Lane` through the live keymap, `enabled` with its reason, optional short reason, `checked` tick slot read back as `tick()`), `Menu` (highlight, `step`, `highlight`, `pick`, `replace_rows`, `rehint`), and `render_menu` over a `MenuHost`. Stepping lands only on enabled actions, and from a non-action row on the first enabled one; an all-disabled menu has no cursor; a rebuild snaps the highlight to the nearest action. Which keys step and pick stays the module's. |
| `notice` | `Notice` (prepared text and a `Tone`: `Status`, `Warning`, `Danger`) and its one paint in theme tokens. Precedence between a tile's notice slots stays the tile's. |
| `popover` | Popup geometry (`ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH`, `SNAP_MARGIN`), the popover `surface`, `anchor_popup` (deferred, anchored, snapped, priority 1), and the `row_shell`/`empty_row` row frames. |

## Menu hints

A row's hint is an action identity, not a string. `Menu::new`,
`replace_rows` and `rehint` resolve it against the bindings they are given —
modules pass `menu::live_bindings(cx)`, the shell's last `Chords` publish —
so a user rebind shows. A module re-resolves an open menu from its `Chords`
observer. An action bound nowhere shows its `Unbound` form: nothing, its `:`
verb, or a key its surface handles itself.

## Commands

```sh
cargo test -p geode-tile
```
