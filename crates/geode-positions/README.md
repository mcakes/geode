# geode-positions

Row menu actions that command the position system. The crate provides one
action, "Move LHU…", on the `position_ref` column. It acts on every selected
top-most row when the pressed row is inside a `V` selection, else on the
pressed row; every acting row must name exactly one position, else the row is
disabled with `{n} selected rows hold several positions`. Without a configured
position service (`positions.toml`) it is disabled with `no position service
configured`. Picked, it opens a choice of the live `lhu` values (leaving out
the LHU the positions already share), asks `Move {n} positions to LHU {x}?`,
and on yes sends one `MoveLhuParams` through the data handle. The move is
request-then-wait: the notice reads `moving {n} positions to LHU {x} · sent`,
the position system's answer replaces it, and the grid changes only when a
snapshot carries the move. `MoveLhu::new` takes the data handle and whether a
service is configured; `geode-app` registers it on the module roster and the
shell's row menu runs it.

## Commands

```sh
cargo test -p geode-positions
```
