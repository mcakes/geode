# geode-positions

Row menu actions that command the position system. The crate provides one
action, "Move LHU…", on the `position_ref` column. It acts on every selected
top-most row when the row the menu opened on is inside a `V` selection, else on
that row; every acting row must name exactly one position, else the row is
disabled with `{n} selected rows hold several positions`. Without a configured
position service (`positions.toml`) it is disabled with `no position service
configured`. Picked, it opens a choice of the live, unscoped `lhu` values
(leaving out the LHU the positions already share; none left closes it with `no
LHU values to move to`), asks `Move {n} position|positions to LHU {x}?`, and on
yes sends one `MoveLhuParams` through the data handle. The move is
request-then-wait: the notice reads `moving {n} position|positions to LHU {x} ·
sent`, the position system's answer replaces it (`… · accepted` or `move to LHU
{x} refused: {reason}`, worded by `geode_core::positions`), and the grid
changes only when a snapshot carries the move. A refusal at submission reads
`move to LHU {x} refused: {reason}` at once. `MoveLhu::new` takes the data
handle and whether a service is configured; `geode-app` registers it on the
module roster and the shell's row menu runs it. See [Move
LHU](../../docs/current/features.md#move-lhu).

## Commands

```sh
cargo test -p geode-positions
```
