# geode-widgets

Shared application widgets below the shell and feature crates. A widget keeps
its reusable behavior in a pure core and accepts presentation values from its
host, preventing either host from becoming a dependency of the other.

Current architecture:
[`docs/current/architecture.md`](../../docs/current/architecture.md).

## Widgets

| Module | Holds |
|---|---|
| `datefield` | `DateTimeField` state, date/date-time precision, segment navigation and entry, and shared key routing. |
| `datefield::paint` | Stateless GPUI painter using prepared segment text and host-supplied colors, radius, and padding policy. |

## Commands

```sh
cargo test -p geode-widgets
```

## Host integration

Keep `DateTimeField` in the host's editing state. Route keys through `route`
and pass editing commands to `apply`; its return value reports whether the
field changed. Enter and Escape return commit/cancel requests, which `apply`
leaves to the host. Chords, Tab, and unrecognized keys also remain host-owned.

Before committing, call `complete_pending`. A lone valid digit completes, but
zero in the month/day or a partial year returns the incomplete segment and
leaves the field unchanged. Read the value only after success, then perform
any domain validation and timezone conversion. The field stores a timezone-free
`NaiveDateTime`; a valid field value need not identify a valid local instant.
Completed edits update the field immediately, so the host also owns restoring
or discarding that state on cancellation.

Prepare `segments()` after state changes and cache the result for rendering.
That method allocates the vector and formatted strings; the painter clones
the prepared `SharedString`s. Supply a prefix in year/month/day/hour/minute/second
order, since separator and click identity come from position. Segment mouse-down
calls the host callback and stops propagation; the callback owns selection,
focus, and repaint notification.

## Invariants

- Nothing here depends on `geode-shell` or a feature crate.
- The stored value is always valid; incomplete digits remain separate until
  completion. Selecting any visible segment, stepping, or Backspace clears them.
- Date precision shows three segments and preserves the hidden time; date-time
  precision shows six. The host chooses the initial active segment.
- Day stepping crosses month/year boundaries. Month and year changes clamp
  the day; date arithmetic saturates at chrono's bounds.
- Time segment stepping wraps within its segment without carrying into another.
- The painter receives presentation values from the host and does not read the
  theme or retain field state.
