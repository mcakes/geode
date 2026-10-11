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
| `cell` | Stateless cell glyphs for dense grids, each a pure core plus a painter: `delta_chip` (the signed number on its sign's color, alpha by magnitude over the column scale up to a host ceiling), `percent_slider` (track filled to the rank, quartile ticks, warning marker at or beyond 0.10 / 0.90), `zero_bar` (a bar from a center axis on the sign's side, length by magnitude over the column scale), `heat` (one hue behind the number, alpha by position in the grid's range). `column_scale` and `heat::range` give the host its scales. |

## Commands

```sh
cargo test -p geode-widgets
cargo run -p geode-widgets --example cells   # every cell glyph at grid density; t toggles light/dark
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
- A cell glyph paints the host's preformatted number every time; color, length
  and position are second channels. A core returns `None` for a non-finite
  value, and the painter then draws the text alone in the plain text color.
- Alpha ceilings (`delta_chip`, `heat`) and the bar's fill alpha are host
  inputs, so light and dark readability is the host's theme choice. A zero or
  non-finite column scale, or a flat heat range, tints nothing.
- Glyph sizes are design-rem constants (`cell::design`), so they scale with
  interface zoom; the only fixed pixels are the one-pixel axis and ticks.
