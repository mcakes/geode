# The as-of dialog, redesigned — design

**Date:** 2026-09-20
**Status:** approved in brainstorm, awaiting implementation plan
**Mockups:** https://claude.ai/artifact/PZgrQzqMuXVGDwZfpiJ2Vx (option A, in
three states, beside options B and C and the shared field; drawn in
Bloomberg Modern at the dialog's 640 px width)
**Supersedes:** Phase 4a §3.6 (`docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md`)
for the dialog's contents; the indicator and undo paragraphs there stand.

## 1. Purpose

`frame::as_of` (`mod+t`) today opens one text field that *is* the value
(`HH:MM`, `YYYY-MM-DD[ HH:MM[:SS]]`, RFC 3339 or `live`), a list of
recent publishes and a calendar. There is no named preset, no fuzzy
filter, and the field's grammar has to be known to be used. Traders
overwhelmingly want one of a handful of instants — last night's close,
this morning's open, the close two or three days back — and only
occasionally an exact time.

The redesigned dialog is one ranked list under a filter: named presets
first, an in-place segmented date-time field for the exact case, and the
publish events as a secondary way to search. The instant every row
resolves to is painted on the row, in the trader's configured clock, so
nothing is committed blind.

## 2. Rulings (2026-09-20)

1. **`T-n` counts business days.** Monday's `T-1` is Friday. Weekend skip
   only; no holiday calendar in this work.
2. **One clock everywhere.** `[time] zone` (IANA name, default the
   machine's zone) replaces `chrono::Local` at every displayed time in the
   workspace, not only in the dialog. The Phase 4a ruling "every displayed
   time is the trader's local clock" becomes "the trader's configured
   clock".
3. **A shared widget crate holds the field's core AND its painter**
   (`geode-widgets`), so the dialog's field and the market-data strip's
   cannot drift. The market-data `DateField` moves there and is
   generalised; nothing date-shaped stays in the panel but its editor
   ownership.
4. **Option A** from the mockups: one list, filter on top, sections
   `Current` (while pinned) / `Live` / `Presets` / `Custom` / `Recent
   publishes`. The calendar pane and the free-text grammar go.

## 3. Vocabulary and config

### 3.1 `[time]`

```toml
[time]
zone = "America/New_York"   # IANA name; absent = the machine's zone
sod  = "08:00"              # start of day, HH:MM in `zone`
eod  = "18:00"              # end of day,   HH:MM in `zone`
```

- Any layer may set them (desk or user); the usual provenance rules.
- A `zone` that `chrono_tz::Tz::from_str` refuses, or a `sod`/`eod` that
  is not `HH:MM`, is an **error** diagnostic at `time.<key>` and falls
  back to that key's default — the same shape as every other refused
  config key. The section is reloadable live: a change repaints every
  displayed time and re-resolves the presets; nothing requeries, because
  the data layer is UTC throughout.
- The machine's zone is read ONCE, at startup, through
  `iana_time_zone::get_timezone()` (already in the lockfile as chrono's
  own dependency); an unreadable or unknown machine zone falls back to
  UTC with a warning diagnostic at `time.zone`.

### 3.2 `geode_core::clock::Clock`

Pure; `Clone + Copy` (a `Tz` is a `Copy` enum in `chrono-tz`; the two
times are `NaiveTime`s). The one owner of "which zone is this?":

| Method | Answer |
|---|---|
| `today(now: DateTime<Utc>) -> NaiveDate` | `now` on the clock's date |
| `sod_of(date) / eod_of(date) -> Result<DateTime<Utc>, ClockError>` | `date` at `sod`/`eod` in `zone`, mapped to UTC |
| `resolve_local(date, time) -> Result<DateTime<Utc>, ClockError>` | the existing DST-gap/overlap refusal (`query::resolve_local`, moved) |
| `business_days_back(date, n) -> NaiveDate` | walk back `n` weekdays; a `date` that is itself a weekend first snaps to the preceding Friday |
| `local(t: DateTime<Utc>) -> DateTime<Tz>` | for formatting |
| `hms(t) / hm(t) / full(t) -> String` | `HH:MM:SS`, `HH:MM`, `YYYY-MM-DD HH:MM:SS %Z` — the three displayed forms already in use |
| `abbreviation(t) -> String` | the zone's abbreviation at `t` (`EDT`), for the field's suffix |

`ClockError` is one variant, `NoSuchLocalTime`, carrying the text that
did not resolve — the message `parse_as_of` shows today.

`chrono-tz` (0.10) is a dependency of `geode-core` alone. No other crate
names a `Tz`; it reaches a zone through `Clock`.

### 3.3 Presets

A fixed list in `geode_core::clock::presets`:

| Label | Resolves to |
|---|---|
| `EOD T-1` | `eod_of(business_days_back(today, 1))` |
| `SOD T` | `sod_of(today)` |
| `EOD T-2` | `eod_of(business_days_back(today, 2))` |
| `EOD T-3` | `eod_of(business_days_back(today, 3))` |
| `EOD T-5` | `eod_of(business_days_back(today, 5))` |

`presets(clock, now) -> Vec<Preset { label, at }>` in that order. A
preset that resolves **after `now`** (`SOD T` before 08:00) is dropped
from the list, not shown — it would mean live, and the `Live` row already
says that. One that fails `resolve_local` (a DST gap landing exactly on
`sod`/`eod`) is dropped too, with a `geode::shell` debug line; it cannot
be an error diagnostic because it depends on the date.

Configurable presets (`[time] presets = [...]`) are a follow-up, not
built.

## 4. `geode-widgets`

### 4.1 Placement

A new crate, `crates/geode-widgets`, below the shell:

```
geode-app
  ├─ geode-shell ──┐
  ├─ geode-marketdata ─┤── geode-widgets ── geode-core
  └─ …                 │      (gpui, gpui-component)
```

It depends on `geode-core`, `gpui` and `gpui-component`; never on
`geode-shell` or a module. `bench = false`; dev-dependencies mirror the
workspace's `geode-*` feature set (test-feature parity, `cccd2fb`); both
CI platforms. Its first and only occupant is the date-time field; later
shared widgets go beside it.

### 4.2 `DateTimeField` (pure)

Today's `geode_marketdata::core::datefield::DateField`, moved and
generalised:

```rust
pub enum Precision { Date, DateTime }
pub enum Segment { Year, Month, Day, Hour, Minute, Second }

pub struct DateTimeField {
    value: NaiveDateTime,
    precision: Precision,
    segment: Segment,
    pending: String,   // digits typed into `segment` this visit
}
```

- `open(value, precision, segment)` — the market-data strip opens at
  `Precision::Date` on `Day` (as today); the dialog at
  `Precision::DateTime` on `Day` (a trader mostly moves the day).
- `left`/`right` stop at the precision's first/last segment (`Day` under
  `Date`, `Second` under `DateTime`); `select(segment)` refuses a segment
  past the precision.
- `step(n)`: year/month/day exactly as today (roll, clamp to month end,
  saturate at the `NaiveDate` bounds); hour/minute/second wrap within
  their range without carrying — `23 ↑` is `00`, the day unchanged. This
  is the "step this segment" reading; a trader who wants the next day
  moves to the day segment.
- `digit(d)`: the same refusal rule (a digit that cannot begin a valid
  value for the segment is refused; two digits complete an hour/minute/
  second, four a year). `complete_pending()` before commit, unchanged.
- `value() -> NaiveDateTime` is valid at every moment; `date()` for the
  `Date` precision. `segments() -> Vec<SegmentText>` answers only the
  precision's segments, in painted order.
- `route(&Keystroke) -> Option<FieldKey>` is the ONE key table both hosts
  call, the shape of `choice::route`:

  | Key | `FieldKey` |
  |---|---|
  | `left` / `right` | `Left` / `Right` |
  | `up` / `down` | `Step(1)` / `Step(-1)` |
  | `shift+up` / `shift+down` | `Step(10)` / `Step(-10)` |
  | `0`–`9` | `Digit(d)` |
  | `backspace` | `Backspace` |
  | `enter` | `Commit` |
  | `escape` | `Cancel` |
  | anything with ctrl/alt/cmd | `None` (a chord falls through to the shell) |

  `apply(&mut self, key)` performs every arm but `Commit`/`Cancel`, which
  the host owns.

The existing `datefield` tests move unchanged (they are the `Date`
precision's contract) and gain the time segments' cases: wrap without
carry, precision stops, `select` past the precision refused, a `"2"`
then `"5"` into the hour refused at the second digit, `route`'s chord
fall-through.

`geode_marketdata::core::nudge::nudge_text` stays in the panel: it nudges
the TEXT editor's value, not this field.

### 4.3 The painter

```rust
pub struct SegmentPaint {           // Copy; derived once by the host
    pub rest: (Hsla, Option<Hsla>), // text, fill (None = bare)
    pub active: (Hsla, Hsla),
    pub typing: (Hsla, Hsla),
    pub separator: Hsla,
    pub suffix: Hsla,
    pub radius: Pixels,
}

pub fn paint(
    field: &DateTimeField,
    suffix: Option<&str>,          // the zone abbreviation, or None
    paint: SegmentPaint,
    on_segment_click: impl Fn(Segment, &mut Window, &mut App) + 'static,
) -> AnyElement
```

One `h_flex` in the mono face: each segment a `Stateful` span with the
state's colours, `-`/` `/`:` separators between, the suffix after a gap.
A click on a segment calls back with that segment; the host decides what
to do with it (both today's hosts select it and, if their field has lost
the keyboard, take it back). Every colour is the host's:

- the market-data header keeps deriving from its `FlooredTones` exactly
  as `date_segment_paint` does today (`primary`/`primary_foreground`
  active, `accent`/`accent_foreground` typing, `foreground` bare), and
  its three bundled-theme sweeps keep pinning it;
- the shell derives the same three states through its own doors
  (`control`/`chip`), floored to 3:1 over the popover, and adds them to
  the existing `every_control_state_is_readable_on_every_bundled_theme`
  sweep's `shipped()` list.

The painter never reads `cx.theme()` itself: it is called from closures
that cannot borrow it (the `key_chip` precedent), and one derivation per
host is the memo the panel already keeps.

### 4.4 Market-data migration

`EditorState::Date { field: DateTimeField, … }` at `Precision::Date`;
`DateFieldPaint::of` becomes `field.segments()` plus the crate's `paint`;
`date_field_key` becomes `route` + `apply`, with `Commit`/`Cancel`
landing in the tile's existing `commit_edit`/`close_editor`.
`date_segment_clicked` is the click callback. No behaviour changes; the
tile's existing date-field tests (open on the day segment, step, digit,
click reclaims focus, commit) are the migration's check and stay green
untouched.

## 5. The dialog

### 5.1 Model

Filter-first, like the palette: no normal mode, the shared dialog field
is focused on open and typing filters at once. State on `ShellView` as
today (`as_of_dialog: Option<AsOfState>`), pure:

```rust
pub struct AsOfState {
    rows: Vec<Row>,            // built on open and on every data/as-of/clock change
    ranked: Vec<Ranked>,       // listfilter::rank over row labels, section-stable
    highlighted: usize,        // index into `ranked`
    field: Option<DateTimeField>, // Some while the Custom row is open
    query: String,
}

pub enum Row {
    Current(DateTime<Utc>),    // only while the frame is pinned
    Live,                      // only while the frame is pinned
    Preset(usize),             // into presets(clock, now)
    Custom,
    Publish(usize),            // into Frame::recent_publishes
}
```

- **Sections** keep a fixed order — Current, Live, Presets, Custom,
  Recent publishes — and rank order inside a section. Under a query a
  section with no match disappears with its eyebrow. `ranked` is the
  concatenation of each section's ranked rows, so `highlighted` walks the
  painted order.
- **Labels** the filter matches: `current`, `live`, the preset label
  (`EOD T-1`), `custom`, and a publish's `dataset / batch · N books`.
  The right column (the resolved instant, `Clock::full` for Current and
  Custom, `Ddd D Mon HH:MM` for presets, `Ddd HH:MM:SS` for publishes) is
  paint only — not matched, so `18` does not rank a preset.
- **`Current` and `Live`** appear only while `frame.as_of()` is `At(_)`.
  Under live, `Live` is a no-op and `Current` has no instant; both are
  omitted rather than painted disabled. `Current` reads the pinned
  instant; `Live` reads `follow new publishes` on the right.
- **Custom's seed** is the pinned instant while pinned, else `now` on
  the clock at second precision. `tab` onto Custom from a highlighted
  preset or publish row seeds the field with THAT row's instant, so a
  trader nudges from a preset rather than from now.
- **Presets** come from `presets(clock, now)` at open and are rebuilt on
  a clock reload; the publish rows keep the `presets_cache` shape keyed
  on `Frame::versions().data`.

### 5.2 Keys

| Key | Effect |
|---|---|
| `↑` `↓` `ctrl+p` `ctrl+n` `ctrl+u` `ctrl+d` `pageup` `pagedown` `ctrl+b` `ctrl+f` | `listfilter::nav_command` moves `highlighted` through `vimnav::apply` (the existing rule; `j`/`k` would type) |
| `1`–`5` on an EMPTY field | jump to that preset and commit (the grouping picker's digit-jump precedent, `choicedialog`); a digit past the painted presets is inert |
| `enter` | commit the highlighted row (§5.4) |
| `tab` | open the Custom field (highlighting the Custom row); inside it, `tab` leaves the field exactly as `escape` does — there is no state where the field is open and the filter takes keys |
| any key while the field is open | `widgets::route`: `Commit` commits the field's value, `Cancel` closes the field and returns focus to the filter, the rest `apply`; a chord falls through to the shell as everywhere |
| `escape` | with the field open: close the field (as `Cancel`); otherwise close the dialog (the modal branch's own rule) |
| typing | filters; the Custom row keeps its `custom` label so it is findable |

Focus: the field is keyboard-owned through the shell's modal key handler,
not a focused gpui `Input` — the dialog's shared `Input` stays the focus
holder, and while `field.is_some()` the handler routes every bare key to
the widget before the filter sees it. This is the same "modal owns the
keyboard" contract the rest of the dialogs keep; there is no second
focus handle to blur.

Footer (`hint_rows`, all three rows always laid out):

- list: move `↑ ↓` row · `1…5` preset; edit `tab` custom time; go
  `enter` set as-of · `esc` close
- field open: move `← →` segment · `↑ ↓` step · `shift ↑` ×10; edit
  `0–9` type · `⌫` clear segment; go `enter` set as-of · `esc` back to
  list

### 5.3 Mouse

- A row click commits it (today's `on_mouse_down` on preset rows,
  extended to every `Row`).
- A click on a Custom segment opens the field on that segment (the
  painter's callback → `select`), without committing.
- Rows take `row_paint` (highlighted `list_active`/`foreground`, pointer
  `list_hover`); the Custom row's segments take the shell's
  `SegmentPaint`.
- Opened through `dialog::open_shell_dialog_with_key` as today; the
  toolbar's AS OF chip already opens it by mouse and keeps its
  `prevent_default` door.

### 5.4 Commit

`commit_at(t)` / `commit_live()` are unchanged: `Frame::set_as_of`,
`previous_as_of` recorded, `close_modal`. Per row: `Current(t)` →
`commit_at(t)` (a no-op set, still closes); `Live` → `commit_live`;
`Preset(i)` → its `at`; `Custom` → `field.complete_pending()` then
`clock.resolve_local(value)` — a DST gap is the one refusal, painted as
the row's right column in `danger` text with the field left open;
`Publish(i)` → `p.at`.

### 5.5 Removed

- The calendar pane, `as_of_calendar` on `ShellView`, the
  `CalendarEvent::Selected` subscription, `on_calendar_selected`,
  `compose_with_date`, `calendar_date`, `shows_calendar`, and the
  `gpui-base` `CalendarView` reset in `open` — with it `geode-shell`'s
  direct `gpui-base` dependency, whose only use it was (user ruling
  2026-09-19 recorded that as the sole exception; the exception is now
  moot and the dependency goes).
- `resolve_input`, `on_query_changed`'s parse, `AsOfState::{error,
  resolved}`, the `→` preview and the inline error.
- `parse_as_of` stays in `geode-core` for `:asof` (tile-local per the
  command-line locality design, 2026-09-20), now taking `&Clock` for its
  date and zone (§6).

## 6. One clock everywhere

### 6.1 The global

`geode_shell::clock::AppClock(pub Clock)`, `impl gpui::Global` — the
workspace's third global beside `UiSettings` and `Chords`, and under the
same rule: written by the shell alone (startup in `ShellView::new`, a
`[time]` change in `apply_reload`), read by modules with
`observe_global`. The market-data tile calls `rebuild_chrome` on the
change (header text is formatted only in `HeaderModel::prepare`); the
blotter and diagnostics tiles `cx.notify()`. Nothing requeries.

### 6.2 Pure code takes `&Clock`

Every `chrono::Local` site in the workspace, by crate:

| Site | Becomes |
|---|---|
| `geode-core` `query::parse_as_of` (+ `resolve_local`) | `parse_as_of(text, now, &Clock)`; `HH:MM[:SS]` and bare dates resolve on `clock.today(now)` in `clock.zone`; RFC 3339 unchanged |
| `geode-shell` `frame.rs` bar model `today`, `shell/mod.rs` (two `today` reads), `scopebar.rs` (the as-of chip's `HH:MM`) | take `clock.today(now)` / `clock.hm(t)` from `AppClock` at the call |
| `geode-shell` `asof_view.rs` | rewritten (§5) |
| `geode-marketdata` `header.rs` (three), `tile.rs` (six), `core/draft.rs` (one) | the tile reads `AppClock` once per `prepare`/render and hands `&Clock` down; `draft.rs` takes it as a parameter |
| `geode-diagnostics` `sections.rs` (two formatters) | take `&Clock` from the tile, which reads the global |
| `geode-app` `main.rs:174` (the calendar seed) | deleted with the calendar |

The status bar's `AS OF HH:MM · …` segment and the toolbar chip format
through the frame's bar model, so they follow.

### 6.3 The guard

`chrono::Local` leaves the workspace except for the ONE machine-zone
read inside `geode_core::clock` (and it is `iana_time_zone`, not
`Local`, that reads it — `Local` disappears entirely). A textual guard
test in `geode-core` (the UTF-8 double-encoding guard's pattern) walks
every `crates/*/src/**/*.rs` and fails on `chrono::Local`, `Local::now`
or `with_timezone(&Local)` anywhere. A stray site is silent wrong-time on
every zone but the machine's, which no fixture on a developer's machine
would catch.

### 6.4 Left as is, on purpose

- The daily log file and crash file names roll on the UTC date
  (`CLAUDE.md`, config/logging rule) — unchanged.
- Storage timestamps are naive UTC micros; `DataService`, the query
  compiler and the series family never see a zone.
- `Coalescer`, retention windows and every duration are zone-free.

### 6.5 Docs

- `CLAUDE.md`: the Phase 4a ruling line becomes "every *displayed* time
  is the trader's configured clock (`[time] zone`, the machine's zone by
  default), including `HH:MM` as-of input"; a rule for `AppClock` joins
  the `UiSettings`/`Chords` globals bullet; the `gpui-base` exception
  sentence is removed; a status-table row and a `docs/phase-history.md`
  paragraph.
- The settings dialog gains no row in this work: a zone is typed, not
  stepped, and the settings dialog has no typed-text row today. `[time]`
  is edited in `app.toml` (`config::open_directory` opens it). A
  `Choice` row over the IANA list is a follow-up.

## 7. Testing

Weight, as ever: pure ≫ window.

**`geode-core::clock`:** `business_days_back` across a weekend (Mon−1 =
Fri, Mon−5 = the previous Mon, a Saturday start snaps to Friday first);
`sod_of`/`eod_of` on a DST-observing zone either side of the change;
`presets` drops a future `SOD T` and keeps the rest in order;
`resolve_local` refuses the spring-forward gap; `parse_as_of("14:05")`
resolves on the clock's date in the clock's zone, independent of the
machine's (the existing test's pattern, now with an explicit zone);
`Clock::default` on an unknown machine zone is UTC.

**`geode-widgets`:** the moved `datefield` suite; hour wrap without
carry; `Second` is the last segment under `DateTime`, `Day` under
`Date`; `select(Hour)` refused under `Date`; `route` maps every key in
§4.2's table and answers `None` to a chord; `segments()` length per
precision.

**Dialog pure core:** `Current`/`Live` present only while pinned; section
order under a filter, an empty section's eyebrow gone; `1` on an empty
query commits preset 0, `1` after typing is a filter character; `tab`
from a highlighted preset seeds Custom with that instant; the DST-gap
refusal leaves the field open.

**Window tests** (`TestAppContext`, `shell/tests/asof.rs` rewritten):
open, type `eod`, `enter` commits `EOD T-1` to the frame; `tab`, `up`
steps the day, `enter` commits the stepped instant; `escape` with the
field open returns to the list, a second closes; a preset row click
commits; a `[time] zone` reload changes the status-bar chip's text and
bumps no data/as-of version; the toolbar chip's mouse-open still types
(the mouse-opened-dialog rule); the footer's `hint-row-move` reads the
field's keys while it is open.

**Market-data:** the existing date-field tests unchanged and green.

**Harness** (`scripts/mutation-check.sh`, every entry naming its test):

| Entry | Mutation | Caught by |
|---|---|---|
| `clock:weekend` | `business_days_back` counts calendar days | Mon−1 = Fri |
| `clock:future-preset` | drop the `at > now` filter | future `SOD T` present |
| `clock:local-guard` | the guard test's pattern list emptied | the guard itself (a sentinel `Local` in a test fixture file it must find) |
| `asof:pinned-rows` | `Current`/`Live` painted under live | rows-only-while-pinned |
| `asof:digit-empty` | digit jump ignores the empty-query gate | `1` after typing is a character |
| `asof:seed` | `tab` seeds from `now` not the row | seed-from-preset |
| `widgets:route-commit` | `enter` → `Cancel` | route table |
| `widgets:precision-stop` | `right` past `Day` under `Date` | precision stop |
| `widgets:wrap` | hour step carries into the day | wrap without carry |
| `marketdata:day-segment` | strip opens on `Year` | the existing open-on-day test |

`--anchors-only` before every merge.

## 8. Delivery

Three parts, each its own worktree, review and merge:

1. **`geode-widgets` + market-data migration.** No visible change; the
   panel's tests are the check.
2. **`[time]`, `Clock`, `AppClock`, the `Local` sweep and guard.** Visible
   only to a trader whose zone differs from the machine's.
3. **The dialog.** Depends on 1 and 2.

Parts 1 and 2 are independent and may run in parallel.

## 9. Display checks (pending on a real window)

- The Custom row's segment colours over the popover in Bloomberg Modern
  and Default Light (active on `primary`, typing on `accent`).
- The right column's alignment across sections at the 640 px width, and
  under `FontSize::Large`.
- The footer swapping to the field's keys without changing height.
- The zone abbreviation suffix reading as a label, not a segment.

## 10. Out of scope

- A holiday calendar (`T-n` over exchange holidays).
- Configurable presets, and a settings-dialog row for `[time]`.
- Per-tile as-of (the command-line locality design owns `:asof`).
- A `CalendarView` anywhere; the market-data strip's date-only field is
  the only other segmented field.

## As built (Part 1)

`geode-widgets` (`crates/geode-widgets`) shipped as designed: `DateTimeField`,
`Precision::{Date, DateTime}`, six `Segment`s, the `route`/`FieldKey`/`apply`
key table and the `SegmentPaint`/`paint` painter, with the market-data panel
migrated onto it (no visible change). Five tasks, one whole-branch fix wave;
`docs/phase-history.md`'s "As-of dialog Part 1" entry has the day-by-day
record. Parts 2 (`Clock`) and 3 (the dialog) are next and unstarted.

**`route(key: &str, shift: bool, chord: bool) -> Option<FieldKey>`** takes
three primitives rather than a `Keystroke` because the two hosts that call
it — the market-data panel (a gpui `KeyDownEvent`) and the future as-of
dialog (`geode_shell::keymap::Keystroke`) — carry two different keystroke
types, and the crate sits below both in the dependency graph (below
`geode-shell`, and a module never shares a type with another module through
the shell). A shared vocabulary would have had to live somewhere above
both, which is exactly the layering this crate exists to avoid; a caller
computes its own `chord` (`modifiers.control || modifiers.alt ||
modifiers.platform`) and hands `route` the key's name string instead.

**`SegmentText.text` is a `gpui::SharedString`**, not a `String`, and
`DateTimeField::segments()` builds each one fresh (`format!(..).into()`)
so every segment of an open field costs one string per call, not one per
render: a host is expected to call `segments()` once per field CHANGE
(`DateFieldPaint::of`, on the market-data panel) and hand the prepared
value through unchanged for however many frames render before the next
change, never inside its own `render`. The final review's fix wave made
that ruling load-bearing rather than aspirational: `DateFieldPaint.segments`
is now `Rc<[SegmentText]>` (a clone at a render site is a refcount bump)
and `DateFieldPaint.selector` a `SharedString` built once in `of` alongside
it, rather than a `format!("marketdata-date-seg-{tile_id}")` rebuilt in
`render_date_field` on every frame.

**Part 3's obligations, deferred out of Part 1 by ruling:** the painter
takes every colour as a `SegmentPaint` value and never reads `cx.theme()`,
which is what lets it paint from a closure — but it also means it has no
route to `geode_shell::shell::control`, the design-guide's hover/pressed
door, since that lives above this crate. A segment currently has no hover
or pressed state at all. When the dialog (or any future host) needs one:

1. Hover/pressed colours must arrive as NEW fields on `SegmentPaint`
   (or a sibling struct), derived by the SHELL (the one place that can
   call `control::paint`) and handed down the same way every other colour
   already is — the crate must not gain a `geode_shell` dependency to
   compute them itself.
2. Each segment's `div` needs an `.id(..)` and to become `Stateful` for
   `.hover(..)` to repaint at all at the pinned gpui-component rev (a
   stateless element's hover is a no-op, per the control-affordance
   handoff); `paint`'s per-segment `div()` is bare today.
3. The new `(rest, surface, text)` pairing a segment's hover introduces
   joins `control::shipped()`, the list `every_control_state_is_readable_
   on_every_bundled_theme` and `every_control_state_is_distinct_from_
   rest_on_every_bundled_theme` sweep with no exception list — a segment
   hover that does not clear both sweeps cannot ship.

**Display check pending:** the market-data panel's date field now nests
its segments in a second `h_flex` (the crate's `paint` builds its own row
inside the panel's existing container `div`) rather than painting them
as direct children of one `h_flex` as before the migration — the panel's
own selector-keyed tests all still find every segment and click target,
but nobody has yet looked at the nested row on a real window to confirm
it reads identically to the pre-migration layout.
