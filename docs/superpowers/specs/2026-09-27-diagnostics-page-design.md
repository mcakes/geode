# Diagnostics page

Status: approved in conversation 2026-09-27; spec awaiting review.

## 1. Why

Diagnostics is a tile today: five sections painted as one flat list of
strings, driven by vim keys, inside the workspace layout. A tile cannot change
application state, so `:level` and `:overlay` refuse and point at the palette.
It competes for space with the trader's working tiles, and its row-list
presentation has no room for tables, expansion, or controls.

The desk wants a backstage surface: a full page outside the workspace layout,
mouse-friendly, reached from the sidebar, that later grows a database explorer
and other developer tooling. This spec introduces the shell's page seam and
rebuilds diagnostics as its first page. It removes the diagnostics tile.

Rulings (user, 2026-09-27):

1. **Extent.** While a page is open, the toolbar, tile surface, and command
   line give way to it. The sidebar and the status bar stay.
2. **Tile removal.** The page is the only diagnostics surface. The tile kind
   is gone; old sessions that name it take the existing unknown-kind path.
3. **Navigation.** One toggle action opens and closes the page. Escape with
   nothing above the page closes it. Any workspace switch closes it.
4. **Frame.** A section rail beside a content pane (option A of the mockups
   under `.superpowers/brainstorm/`), not tabs and not a dashboard landing.
5. **Section content.** Real tables with expansion and a toolbar per
   section, and controls that change application state where the state has
   a request channel (log levels, overlay, catalog refresh).
6. **Architecture.** A page seam in `geode-shell` with the content in
   `geode-diagnostics`, registered by `geode-app`, so the shell stays
   ignorant of diagnostics and a later database explorer can carry its own
   data handle.

## 2. The page seam (`geode-shell`)

### 2.1 Traits

`geode_shell::module` gains, beside the tile seam:

```rust
pub trait PageContent {
    /// Pushed innermost on the key context stack while the page is open.
    fn key_context(&self, cx: &App) -> KeyContext;
    /// An action the shell did not recognise. `true` if handled. A page
    /// returns `false` for `page::close` when it has nothing of its own
    /// to close first (a focused input, an expanded row) so the shell acts.
    fn dispatch(&self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut App) -> bool;
    /// The page is on screen. Opening announces `true`; closing `false`.
    /// A fresh occupant assumes it is hidden until the first call.
    fn set_visible(&self, visible: bool, cx: &mut App);
    /// The handle the shell focuses on open; the page view tracks it.
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle;
    /// `true` while one of the page's own text inputs owns keyboard focus.
    /// The shell then routes bare keys to the input and only chords to
    /// the keymap, exactly as `TileContent::holds_focus` does for a tile's
    /// insert mode; the page's `key_context` carries `mode = insert` then.
    fn holds_focus(&self, window: &Window, cx: &App) -> bool;
    fn title(&self, cx: &App) -> SharedString;
    /// Opaque state for `[pages.<kind>]` in the session file.
    fn serialize(&self, cx: &App) -> toml::Table;
}

pub struct PageOccupant {
    pub kind: &'static str,
    pub view: AnyView,
    pub content: Box<dyn PageContent>,
}

/// Dispatches a registered shell action on the page's behalf. Built by the
/// shell from its own weak entity; a page never holds `ShellView`.
pub type ShellActions = Rc<dyn Fn(&ActionId, &mut Window, &mut App)>;

pub trait PageFactory {
    fn kind(&self) -> &'static str;
    /// Sidebar tooltip and palette row text, e.g. "Diagnostics".
    fn title(&self) -> &'static str;
    /// Sidebar glyph.
    fn icon(&self) -> IconName;
    fn register_actions(&self, registry: &mut ActionRegistry);
    fn contexts(&self) -> Vec<&'static str> { vec![self.kind()] }
    fn default_keymap(&self) -> Option<&'static str> { None }
    /// A context-free default binding for `page::toggle_<kind>`, e.g.
    /// `"mod+d"`. The roster turns it into a shell-generated fragment; a
    /// module fragment could not carry it because the fragment checker
    /// requires a page context and the toggle must fire from the workspace.
    fn toggle_binding(&self) -> Option<&'static str> { None }
    fn create(
        &self,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        actions: ShellActions,
        window: &mut Window,
        cx: &mut App,
    ) -> PageOccupant;
}
```

- No `command`, `completions`, `deliver`, or `find`. A page has no `:` line,
  receives no query deliveries, and owns its own filter inputs.
- `impl<F: PageFactory + ?Sized> PageFactory for Rc<F>` forwards every method,
  defaulted ones included, for the same reason the module forwarder does.
- `PageRoster`: ordered factories with `add`, `factory(kind)`, `kinds`,
  `entries` (kind, title, icon for the sidebar), `register_actions`, and
  `keymap_fragments` (each factory's default keymap checked against its
  `contexts`, plus one unchecked shell-generated doc per `toggle_binding`),
  spliced with the module fragments.

### 2.2 Shell-registered actions

For each roster entry the shell registers `page::toggle_<kind>` with title
"<Title>: Open page" in category `<Title>`, mirroring the per-kind add-tile
rows. It registers `page::close` ("Close page", category Workspace) once.
Builtin keymap:

```toml
[[bindings]]
context = "page"
[bindings.keys]
"escape" = "page::close"
```

`mod+d` is unbound today; the diagnostics factory's `toggle_binding` returns
it, and the roster emits the context-free binding as a shell-generated
fragment (a module fragment cannot carry a context-free binding).

### 2.3 `ShellView` state and routing

- `page: Option<OpenPage { kind: &'static str, occupant: PageOccupant, open: bool }>`.
  Created on first open; retained for the window's lifetime so filters,
  expansion, and the log tail survive a round trip.
- **Open**: create if absent, `set_visible(true)`, focus the page view's
  focus handle, `cx.notify()`. **Close**: `set_visible(false)`, focus the
  shell root, arm nothing. Tile focus restoration is not armed by page
  transitions; a workspace switch from the page restores that workspace's
  focused tile through the existing switch path.
- **Toggle** while a modal or the palette is open: the modal or palette keeps
  priority; the toggle is ignored, as `settings::open` is under a modal.
- `workspace::switch_n` while open: close the page, then switch.
- **Context stack** while open: `["page", "<kind>"]`, then `"palette"` if
  open. `workspace` and `tile` are absent. Tile movement, `:`, `/`, add-tile,
  dock, and stack bindings cannot fire into a page.
- **Escape order**: modal → palette → insert branch (a page input holds
  focus: the page's own `mode == insert` binding for Escape blurs it) →
  matcher resolves `page::close` → page's own `dispatch(page::close)`
  returning `true` → shell closes the page. Drag cancellation precedes all
  of these as today. `occupant_insert_stack` consults the open page's
  `holds_focus` before the focused tile's.
- Divider strips, tile drags, the command line, the stack list, and
  `ensure_occupants` visibility announcements treat an open page as they
  treat an open modal: not created, and cancelled if armed. Tile occupants
  are hidden while the page is open (their `set_visible(false)` runs), so a
  page open removes watched demand from the tiles beneath and restores it on
  close.
- `dispatch` falls through to `page.content.dispatch` when a page is open
  and the action is not a shell builtin, instead of to the focused occupant.

### 2.4 Render

When a page is open, render paints the page view in the rect that the
toolbar, tile surface, and command line would occupy: from the sidebar's
right edge to the viewport's right edge, from the top to the status bar. The
sidebar and status bar paint unchanged. Modals, the palette, which-key, the
perf overlay, and notifications paint above the page in their existing order.

The status bar's diagnostics summary click dispatches
`page::toggle_diagnostics`; its tooltip reads "Open the diagnostics page".
`open_module("diagnostics")` and its palette rows go away.

### 2.5 Sidebar

Between the workspace discs and the settings avatar, the rail paints one
button per roster entry, in roster order, anchored above the avatar. Each is
a bare glyph (`factory.icon()`) in a box with the gear's pointer states, a
tooltip of `factory.title()` naming `page::toggle_<kind>`, and `sidebar-page-
<kind>` as its debug selector. While that page is open the box takes the
active workspace disc's treatment (`sidebar_primary` on its tint) and no
pointer states, because selected must stay distinct from hovered.

### 2.6 Session

`session.toml` gains `[pages.<kind>]`, written on the coalesced flush from
each created page's `serialize`. Restore passes the table to `create` on the
page's first open. Whether a page was open at quit is not saved; the app
starts in the workspace. An unknown `[pages.<kind>]` table is kept and
ignored with a warning, like an unmatched tile record.

## 3. The diagnostics page (`geode-diagnostics`)

### 3.1 Crate shape

| Module | Holds |
|---|---|
| `lib` | `DiagnosticsPageFactory`, action registration, default keymap fragment, `PageContent` adapter. |
| `sections` | Pure typed row builders per section; badge counts; explicit `now` and clock inputs; no GPUI or I/O. |
| `page` | The `DiagnosticsPage` entity: observers, selected section, per-section state, log tail, prepared rows, layout. |
| `tables` | `TableDelegate` implementations over prepared rows for Sources, Data, Config diagnostics, and Log. |
| `levels` | The Levels popover state: targets, level choice, add-target row. Pure state plus a render function. |

`commands.rs` and `tile.rs` are deleted.

### 3.2 Frame

- **Header**: title "Diagnostics"; chips derived from the same inputs as the
  status summary (worst source health with count, config errors, data
  errors, catalog time or "catalog pending"), painted through
  `shell::chip::chip_paint`; a back control (‹) that dispatches
  `page::close`. Key hints live in tooltips, not inline, per the render rule.
- **Section rail**: fixed design width via `shell::scale`, rows in order
  Sources, Data, Config, Log, Perf. Each row shows the section name and a
  badge: Sources worst-health dot and source count; Data dataset count;
  Config error and warning counts; Log error count in the retained tail;
  Perf frame p95. Click selects. The selected row takes the selected-row
  treatment from `shell::listrow`.
- **Content pane**: the selected section's toolbar row, then its body,
  filling the remainder. Tables virtualise through the component; the perf
  section is a fixed layout.

### 3.3 Sections

**Detail strip.** gpui-component's `DataTable` paints every row at one
height, so a row cannot grow to show more. Instead, every table section has
a detail strip under the table showing the cursor row's detail lines. Click
or `j`/`k` move the cursor; the strip follows.

**Sources.** Toolbar: filter input. Table columns: Source, Health (dot and
label with reason), Since (time and age), Shape, Last poll, Next poll,
Ready, Loading (path and start time, shown for the cold-start case too).
Order: worst health first, then name; unreported sources last. The detail
strip shows the cursor source's spec detail by shape (paths, adapter,
priority, readiness; adapter and topics; adapter and fetch) and the health
history as ordered chips (up to the entity's 16). Ages come from a
one-second timer that runs only while the page is visible and Sources is
selected, updating the age text without rebuilding the table.

**Data.** Toolbar: filter input, "Expand all" / "Collapse all", a chip
stating "catalog as-of = frame" or "catalog pending", and a "Refresh
catalog" button (`request_catalog`). Table columns: expander, Dataset,
Partitions, Latest gen, Published, Rows, Resolved (frame as-of), Live. A
dataset row expands to its partition rows with generation, publish time,
rows, live or archive, and the resolved marker. The resolved marker is
hidden until catalog and frame as-of agree, as today.

**Config.** Two panels side by side. Left, diagnostics: a Current / History
toggle; a table with Severity chip, Lane (config or data), Where (document
and path when the diagnostic carries them, else the diagnostic's own text),
Message; history groups by batch time; the detail strip shows the cursor
diagnostic in full. An "Open config directory" button dispatches
`config::open_directory` through the shell-actions handle. Right, effective
values: a table with one expandable row per document and, under an expanded
document, one row per leaf with Key, Value, and Layer (the layer chip from
`Config::explain`); a key filter that narrows leaves across documents; the
2,000-leaf cap per expanded document with an omitted-count row.

**Log.** Toolbar: level toggles (ERROR, WARN, INFO, DEBUG, TRACE; all on by
default), a target select over targets seen in the tail plus "all", a message
filter input, a Follow switch, "Clear", and "Levels…". Table columns: Time
(with milliseconds), Level chip, Target, Message. The detail strip shows the
cursor record in full with a Copy button (clipboard). A loss row reports the
gap measured at the last drain. Follow turns off when the cursor moves and on
with `G` or the switch. Retained tail stays at 4,096 records, drained with a
reused buffer, starting at the ring's sequence when the page is first
created.

The Levels popover lists the default level and each configured target with a
level select, plus an add-target row (target input and level). A pick calls
`Diagnostics::request_level`; the shell's existing drain applies it and
persists it to the user layer. The popover reads `Diagnostics::levels` so
the applied value is what it shows.

**Perf.** Stat tiles: frame p50 / p95 / max with sample count and the 8 ms
budget; requery submit→snapshot p50 / p95 with the 50 ms budget; requery
snapshot→paint p50 / p95; dropped events. A frame-interval histogram painted
as a flex row of themed bars, one per `FrameHistogram` bucket plus overflow,
bars past the budget in the warning chip color. `FrameHistogram` gains a
`buckets()` reader and a `bucket_bounds()` associated constant for the axis
labels. Database tiles: database bytes with used blocks and block size,
DuckDB memory with threads. A Performance overlay switch, controlled by the
overlay value the shell now mirrors into `Diagnostics`, flipping through
`request_overlay_toggle`.

### 3.4 Rebuild rules

Carried over from the tile and enforced by tests:

- Observers compare only the selected section's inputs: its `DiagVersions`
  counter, plus frame as-of for Data, config version for Config, or ring
  sequence for Log. Local section, filter, sort, and expansion changes also
  rebuild. A perf tick never walks config documents.
- The app refreshes the factory's shared config before the page's frame
  observer rebuilds config rows. Observer registration order is preserved.
- Visibility calls `watch()` / `unwatch()` and notifies in the same update.
  Visible as-of changes call `request_catalog_refresh()`.
- Prepared rows are shared through `Rc`; header and badge strings are cached
  and rebuilt only when their inputs change.
- Historical resolved-generation markers display only when catalog and frame
  as-of match.

### 3.5 Keys

Default keymap fragment, context `diagnostics`:

| Key | Action |
|---|---|
| `j` / `k` | `diagnostics::down` / `diagnostics::up` |
| `g g` / `shift+g` | `diagnostics::top` / `diagnostics::bottom` (bottom resumes Follow in Log) |
| `ctrl+d` / `ctrl+u` | half page |
| `ctrl+f` / `ctrl+b`, `pagedown` / `pageup` | full page |
| `[` / `]` | `diagnostics::prev_section` / `diagnostics::next_section` |
| `z o` / `z c` | `diagnostics::expand` / `diagnostics::collapse` (Data datasets, Config documents) |
| `/` | `diagnostics::filter` — focus the selected section's filter input |
| `enter` | `diagnostics::activate` — toggle expansion of the cursor row where it expands |
| `escape` (mode == insert) | `diagnostics::blur` — blur the focused input, back to normal mode |

Action ids that keep their meaning keep their names so user overrides of
the tile bindings still apply. Tab moves between the toolbar controls and
the table through the components' own focus order. While an input is
focused the page's context carries `mode = insert`, so bare keys type and
the insert-mode Escape binding blurs; `page::close` is reached only from
normal mode.

### 3.6 Persistence

`serialize` writes `section = "<name>"`. Nothing else persists.

## 4. App composition (`geode-app`)

- Build `DiagnosticsPageFactory::new(ring, config)` and add it to the
  `PageRoster`; keep the `Rc` for the config-refresh reload handler that
  today targets the tile factory.
- Remove `DiagnosticsFactory` from the module roster and the diagnostics
  kind from the add-tile picker and launch-context lists.
- Register roster actions and splice page keymap fragments beside module
  fragments before the keymap builds.

## 5. Tests

**Pure (`sections`, `levels`).** Source ordering and unreported-last;
expansion membership; Data partition rows and resolved-marker gating;
Config diagnostic columns, history grouping, tree with provenance and the
leaf cap; Log level and target filtering, loss-gap arithmetic; badge counts;
histogram bucket mapping and over-budget tint decision; Levels popover state.

**Shell context (`geode-shell`).** Toggle opens and closes through
`dispatch`; toggle ignored under a modal or the palette; Escape closes the
page but not while a modal or palette is above it; `workspace::switch_n`
closes the page and restores that workspace's focused tile; context stack is
`page`, `diagnostics` with no `workspace` or `tile`, so `mod+n` and `:` are
inert; open watches, close unwatches and notifies; tile occupants beneath
are hidden on open and shown on close; divider strips and tile drags are
absent while open; session flush writes `[pages.diagnostics]` and restore
reopens the saved section; a stale `diagnostics` tile record paints a
placeholder with the record kept; the overlay mirror in `Diagnostics`
follows a keyboard toggle.

**Production routes.** Sidebar page button click, status-bar summary click,
and the palette row each reach the toggle; after a mouse open the page
receives typed keys (`]` switches section). A Levels pick reaches the
shell's drain and the log control. The overlay switch reflects the mirrored
value.

**Page UI (`geode-diagnostics`, test-support).** Section switch by click and
by `[` / `]`; row click sets the cursor and the detail strip follows; `enter`
and click expand a dataset; `/` focuses the filter input and the page reports
`holds_focus`, and `diagnostics::blur` returns to normal mode; Follow
turns off on cursor move and on with `G`; the ages timer runs only while
visible and on Sources.

**Mutation entries.** watch/unwatch on open and close; Escape precedence
under a modal; workspace switch closing the page; section persistence round
trip; badge counts; level request routing; overlay mirror; loss-gap
arithmetic; resolved-marker gating; tile-beneath hiding on open.

**Display checks** on the user's screen: rail active state; header chips;
table density and column alignment at wide and narrow widths; histogram tint
at the budget line; Levels popover geometry; light and dark theme pass.

**Performance.** Page render inside the 8 ms budget with a full 4,096-record
tail, measured with the overlay and recorded in the measurement log.

## 6. Documentation

- `docs/current/shell.md`: a "Pages" subsection under module hosting
  (seam, roster, context stack, render extent, session table, modal
  precedence); the diagnostics state section loses tile wording.
- `docs/current/features.md`: the diagnostics entry rewritten for the page.
- `docs/current/input-and-dialogs.md`: the page context stack and Escape
  order.
- `docs/current/keymaps.md`: the `page` context and the builtin bindings.
- `crates/geode-diagnostics/README.md` and `crates/geode-shell/README.md`
  updated in the same change.

## 7. Not in scope

- The database explorer, ref-data map, and any DB maintenance action. They
  are later sections of this page; the seam and the shell-actions handle are
  what they need.
- Persisting whether the page was open at quit.
- A config reload action (reload is watcher-driven) and a histogram reset.
- Sorting by columns that have no natural order; sorting exists only where
  a section defines one.
- Separate windows.
