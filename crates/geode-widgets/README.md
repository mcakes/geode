# geode-widgets

Shared application widgets below the shell and feature crates. A widget keeps
its reusable behavior in a pure core and accepts presentation values from its
host, preventing either host from becoming a dependency of the other.

Current architecture:
[`docs/current/architecture.md`](../../docs/current/architecture.md).

## Widgets

| Module | Holds |
|---|---|
| `datefield` | `DateTimeField`, date/date-time precision, six segments, key routing, prepared segment text, and the host-colored painter. |

## Commands

```sh
cargo test -p geode-widgets
```

## Rules this crate pins

- Nothing here depends on `geode-shell` or a feature crate.
- The host owns commit, cancel, focus, and final presentation.
- Segment text is prepared as shared strings instead of allocated in render.
- Time segment stepping wraps within the segment; it does not carry into the
  date.
