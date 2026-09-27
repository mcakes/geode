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
| `notice` | `Notice` (prepared text and a `Tone`: `Status`, `Warning`, `Danger`) and its one paint in theme tokens. Precedence between a tile's notice slots stays the tile's. |
| `popover` | Popup geometry (`ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH`, `SNAP_MARGIN`), the popover `surface`, `anchor_popup` (deferred, anchored, snapped, priority 1), and the `row_shell`/`empty_row` row frames. |

## Commands

```sh
cargo test -p geode-tile
```
