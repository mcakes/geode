# Geode Phase 3 — Blotter Design

Phase 3 builds the flagship module: a blotter that renders any view
definition as a collapsible, keyboard-driven hierarchy over live or
historical data, inside the §7.1 budgets. It also builds the two things
the blotter cannot exist without and Phase 2 deliberately left undone:
the shell's module-hosting contract with the shared frame state, and the
`[sources]` configuration surface with a continuous ingest scheduler
behind it.

This document is subordinate to
`2026-08-28-geode-foundation-design.md` (referenced below as "the
foundation spec", with bare `§n` references pointing into it), to
`2026-08-30-geode-phase-2-data-design.md` ("the Phase 2 spec", `P2 §n`),
and to `docs/PHILOSOPHY.md`. Where it contradicts either spec it says so
in §2 — those are deliberate amendments made in writing, per the
charter's own rule.

## 1. Scope

### 1.1 What Phase 3 delivers

- The module-hosting contract in `geode-shell`: a factory that
  registers actions and creates tile occupants, an occupant trait the
  shell drives keys, commands and persistence through, and a roster in
  `geode-app`.
- The shared frame (§4): global scope, nine grouping slots from
  `groupings.toml`, as-of, and the data and config generations, as one
  shell entity every tile observes. Keyboard-only surface: `ctrl+1..9`
  and a `:` command vocabulary. No scope bar, pickers or time-travel
  widgets — those are Phase 4 and plug into this entity.
- A shell-owned per-tile command line serving both `/` and `:`.
- `sources.toml`, parsed into `SourceSpec`s, and a discovery scheduler
  inside `DataService` that polls each source on its interval and keeps
  a blotter left open all day current.
- `DataHandle`: a `Clone + Send + Sync` door to `DataService` for
  modules, with results and ingest events delivered on one channel the
  UI wakes on rather than polls.
- Snapshot additions in `geode-core`: index-based accessors, a tree
  index built off the render thread, column formats, and `AsOf` moved
  in from `geode-data`.
- `geode-blotter`: a pure core (column plan, expansion, flatten,
  cursor, format cache) tested without a window, and a thin adapter over
  gpui-component's `DataTable`.
- A `--demo` flag that boots the app on generated data with no real
  source, replacing `GEODE_PROBE_DIR`.
- Requery timing in the perf overlay: submit → snapshot → first paint,
  recorded by the blotter, always compiled.
- Deletion of the throwaway probe (`geode-shell::dataprobe`,
  `geode-app/src/probe.rs`, `mod+shift+d`, `examples/probe-config`).

### 1.2 Done state

Phase 3 is done when `geode --demo` opens with a blotter in the first
tile showing the demo view grouped by the active slot; when `j`/`k`,
`zo`/`zc`, `/`, `v`+`y`, `:group`, `:scope`, `:asof` and `ctrl+1..9` all
do what §3.2 and §4 say; when a cross-gamma cell at an underlying-level
row paints blank and a trading-PnL cell at a position row under a blank
parent paints muted with a dagger; when a file landing in the demo
source directory updates every visible blotter without a keypress and
without dropping a frame; when the perf overlay shows a requery inside
§7.1's 50 ms at 1M generated rows; and when the probe is gone with
nothing left reading `GEODE_PROBE_DIR`.

### 1.3 Explicitly not in Phase 3

- The scope bar, dimension pickers, as-of selector, and scope undo
  beyond one level (§4, Phase 4).
- Diagnostics and config-editor modules (§9.2). Ingest health reaches
  the status bar as a one-line worst-case label; details go to stderr.
- Column resize, reorder and hide *persisted to the view* (§9.2). The
  drags work, through `DataTable`, and are forgotten on close.
- `:filter` — a tile-local expression predicate (§4.2). The scope
  composition function accepts a tile layer from day one; nothing sets
  it yet.
- Atomic flip across tiles on a frame change (§4.3). Each tile is
  internally consistent; two tiles may repaint a frame apart.
- The "reload data now?" prompt for unsafe config changes (§8). A change
  to `sources` or `datasets` after start is a restart-required
  diagnostic.
- Multi-window (§3.6), scenario datasets, the sidecar split.

## 2. Amendments to earlier designs

**2.1 Foundation §9.1: the factory receives no service handles from the
shell.** §9.1 says a module's constructor takes "handles to shell
services: `DataService`, scope/as-of state, action registry". The shell
cannot name `DataService` (§2: `shell` never depends on `data`), so the
factory signature carries only shell-side handles — the frame entity and
the tile id. A module that needs data carries its own `DataHandle` as a
field of its factory, constructed in `geode-app` where both sides meet.
The contract is unchanged in spirit: everything a module touches still
arrives through one doorway; the doorway is the factory the app builds.

**2.2 Foundation §3.2: `/` and `:` are typed into a shell-owned input,
not captured keystroke by keystroke — and `find_style` still decides
what `/` does.** The dialog-filter-input design (2026-09-01) retired
the raw-keystroke find model for dialogs because `Keystroke` carries a
key name rather than a character, so shifted symbols append the base
character. A blotter query can contain `-`, `_` and digits, so the same
defect would bite here. The shell therefore owns one `InputState` for
the command line, and that is the only thing this amendment changes:
the *typing surface*. The `[ui] find_style` setting — kept, with its
settings row, for exactly this consumer — governs the behaviour in
full (§6.1): under `vim` the query jumps the cursor to matches as it is
typed and `n`/`N` repeat after commit, through `vimfind::find_match`
and `repeat_find`; under `fzf` the visible rows are filtered by the
query as it is typed, through `vimfind::filter_matches`, and `escape`
restores them. `VimFind`'s keystroke state machine is left in place,
unused, as the design that kept it said.

**2.3 Foundation §3.4: the keymap engine carries count prefixes.**
"Modules never bind keys; they expose actions" leaves nowhere for a
`5j` to live, since an `ActionId` carries no argument. Rather than let
the blotter read raw digits — which is binding keys by another name —
the engine gains counts as a first-class, per-context feature (§3.3).
Every module that opts in gets `5j`, `3zo` and `12G` the way the
dialogs' `VimListNav` already gave them, and the keymap stays the only
thing that maps keys to behaviour.

**2.4 P2 §6.7: the coalescing key is the tile, not the view.** "One
in-flight query per view" was keyed on the view name. Two tiles showing
the same view with different pins or expansion depth would supersede and
cancel each other. The pool keys on a `QueryKey` the caller supplies —
the tile id — and the view name is data on the request.

**2.5 P2 §5.4–5.5: one ingest runner for all datasets, owning the
`Store`.** The runner today is one thread per dataset taking the whole
`Store`, so a second dataset would mean a second writer and the
single-writer discipline of §5.3 would be a convention. Phase 3 makes
the runner one thread taking the `Store` and the `SchemaSpec`, resolving
the dataset per work item; `DataService` keeps only reader connections
cloned before the store moves. The probe's workaround — ingest once,
close, then open the service — goes with the probe.

**2.6 P2 §6.6: a `Snapshot` carries its tree index.** The parent and
child structure of a rollup result is a property of the result, not of
the renderer, and building it for a 729k-row result on the render
thread would spend the whole §7.1 budget. It is built in `from_batches`
on the query worker, immutable and `Arc`-shared with the rest.

**2.7 `AsOf` moves to `geode-core`.** The shell holds the frame's as-of
and cannot name `geode-data`. `AsOf` is a value type like `Scope` and
lives beside it; `geode-data` re-exports it. `geode-core` gains a
`chrono` dependency, which `geode-data` already carries.

**2.8 P2 §1.2's done state was not met as written.** "A file landing in
that directory updates the tile" held only for files present before
start: the probe ingests once and never discovers again. Phase 3 meets
it (§1.2 above).

## 3. Module hosting

### 3.1 The tile occupant

A tiling leaf stays a `TileId`; the tiling tree remains pure and
gpui-free. `ShellView` gains a map from `TileId` to an occupant:

```rust
pub struct TileOccupant {
    pub kind: &'static str,          // "blotter"
    pub view: AnyView,               // what the shell paints in the leaf
    pub content: Box<dyn TileContent>,
}

pub trait TileContent {
    /// Pushed onto the keymap context stack while this tile is focused,
    /// e.g. `blotter` with `mode = normal | visual`.
    fn key_context(&self, cx: &App) -> KeyContext;
    /// An action the shell did not recognise. `true` if handled.
    fn dispatch(&self, action: &ActionId, count: Option<u32>,
                window: &mut Window, cx: &mut App) -> bool;
    /// A `:` line, without the colon. `Err` is shown inline on the line.
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String>;
    /// Candidates for the word under `cursor` on a `:` line (§3.4). The
    /// shell ranks and shows them; the occupant only knows its vocabulary.
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String>;
    /// `/` text as it changes, the committed query on Enter, or a cancel.
    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App);
    /// A query result addressed to this tile (§5.1).
    fn deliver(&self, outcome: QueryOutcome, window: &mut Window, cx: &mut App);
    /// Hidden tiles (workspace switched away, dock collapsed) may drop
    /// subscriptions; shown tiles resubscribe and requery if stale.
    fn set_visible(&self, visible: bool, cx: &mut App);
    /// State for `session.toml` (§3.4).
    fn serialize(&self, cx: &App) -> toml::Table;
}
```

`QueryOutcome` is a `geode-core` value type (§5.1), so the shell can
route it without naming the data crate. `FindEvent` is
`Changed(String) | Committed(String) | Cancelled`.

### 3.2 The factory and the roster

```rust
pub trait ModuleFactory {
    fn kind(&self) -> &'static str;
    /// Runs once, before the keymap builds — `build_keymap` drops any
    /// binding whose action is unregistered.
    fn register_actions(&self, registry: &mut ActionRegistry);
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant;
}
```

`geode-app` builds the roster — a `Vec<Box<dyn ModuleFactory>>` — in
`main` after the config loads and before `build_keymap`, and hands it to
`ShellServices`. Day one the roster is the blotter. A new tile (a split,
a restored session entry, or the first tile of an empty workspace) is
filled by the module named in `[app] modules.default`, `"blotter"` when
unset. A restored tile whose module kind is not in the roster, or a
tile with no session entry, is painted as an empty tile with a hint
naming the palette — never a panic, never a silent blank.

The roster is the only place the app knows which modules exist (§9.1).

### 3.3 Keys, dispatch and modes

`context_stack` pushes the focused occupant's `key_context()` after
`workspace`. Bindings in the builtin keymap use `blotter && mode ==
normal` and `blotter && mode == visual`; the engine already resolves
`Eq` innermost-wins. `dispatch` tries `apply_workspace_action` first,
then the shell's own arms, then hands an unknown id to the focused
occupant (correction, 3b final review M16: an earlier draft of this
section had the order reversed). Module actions are namespaced
`blotter::…` and appear in the palette like any other, with their
binding shown.

The builtin keymap gains its first sequences (`gg`, `zc`, `zo`, `za`);
the status bar already shows pending keystrokes and the which-key
overlay already lists continuations, so both work for free.

**Count prefixes** are the engine's, not the module's (§2.3). A
`KeyContext` gains a `counts` flag; the blotter sets it in `normal` and
`visual`. While the innermost context on the stack carries it, an
unmodified digit with no binding pending accumulates into
`Matcher::count` rather than being matched: `1`–`9` always, `0` only
once a count has begun, exactly as vim does so that `0` stays bindable
as a motion. The next resolved action is dispatched as
`(ActionId, Option<u32>)`; `escape` or a `NoMatch` clears the count; a
`Pending` keeps it (`3zo` is a count, then a sequence). The count shows
in the status bar beside pending keystrokes and in the which-key
overlay's header. Shell-side actions ignore the count today; workspace
actions could take it later without any further engine change.
`vimnav::apply` already takes a signed step, so `5j` is
`Move(5)`, `12G` is a row index, and `3zo` opens three siblings down.
The count is capped at four digits, which is more than any tree needs
and stops a held key from overflowing anything.

Focus is one gpui `FocusHandle` on the shell root, as today. `DataTable`
tracks its own handle and gpui focuses a tracked element on mouse down,
so a row click moves focus into the table. The tile's existing
click-to-focus handler already reasserts the shell's handle on the next
frame (`pending_focus_restore`); that path is kept and tested. The
table's own key bindings (`up`, `down`, `tab`, …, in its `DataTable`
context) are bound to `NoAction` through the existing
`init_reclaimed_keybindings` door so they cannot act during the frame in
which focus sits there.

### 3.4 The command line

One `InputState` owned by `ShellView`, rendered as a single-line strip
along the bottom edge of the focused tile, with the prompt character —
`/` or `:` — in the muted token at its left. Two shell actions,
`tile::find` and `tile::command_line`, bound to `/` and `:` in the
`tile` context (any focused tile with an occupant), open it with the
matching prompt and focus it; the line is the shell's, so the actions
are too, and every module gets them without registering anything.
While it is focused the shell's key handler treats `escape` as cancel
and `enter` as commit, and lets everything else through to the input,
exactly as it does for the toolbar's filter input today. `/` text is
forwarded on every change (`FindEvent::Changed`), on Enter
(`Committed`), and on `escape` (`Cancelled`); `:` text is forwarded on
Enter only. An `Err`
from `command` is shown in the danger token beside the prompt until the
next keystroke. The strip carries no history; that is a later
nicety.

**Completions.** On every change to a `:` line the shell asks the
occupant for `completions(line, cursor)` — the vocabulary for the word
under the cursor: subcommands after `:sort` or `:group`, column names
after `:sort`, `:group` and inside `:scope`, view names after `:view`,
slot numbers after `:group slot`. The shell ranks them with
`listfilter::rank`, the fuzzy matcher the palette and dialogs already
share, and paints a popup of ranked rows with match highlighting
directly above the strip, reusing the palette's row rendering. `tab`
accepts the top row and cycles on repeat; `ctrl+n`/`ctrl+p` move the
highlight; Enter with exactly one match accepts it and submits, so
`:sort del` runs as `:sort delta01`. An ambiguous Enter is an inline
error naming the matches — the line never guesses. A word with no
candidates (a literal in `:scope`, a `desc`) has no popup. The
occupant supplies words, nothing else: ranking, the popup and the
key vocabulary are the shell's, so every module completes the same way.

### 3.5 Session persistence

`session.toml` gains, per workspace, a `tiles` table keyed by tile id
alongside the existing `node` tree:

```toml
[workspaces.1.tiles.3]
module = "blotter"
[workspaces.1.tiles.3.state]
view = "tree"
pinned = ["lhu", "underlying_ref"]   # `:group a,b,c`; absent when following the frame
# pinned_slot = 3                    # `:group slot 3`; mutually exclusive with `pinned`
unscoped = false
```

`state` is whatever `serialize` returned; the shell stores it opaquely.
Cursor, expansion and scroll are deliberately not persisted: a restored
tile requeries and starts collapsed at the top, which is honest about
the data having moved. A `tiles` entry for an id not in the tree is
dropped with a warning, matching the healing `from_toml` already does.

## 4. The frame

### 4.1 The entity

```rust
pub struct Frame {
    scope: Scope,                       // the global layer (geode-core)
    previous_scope: Option<Scope>,      // one level of `:scope undo`
    slots: GroupingSlots,               // from groupings.toml
    active_slot: Option<u8>,            // 1..=9, None = views' own default
    as_of: AsOf,
    versions: FrameVersions,            // one counter per field below
}

pub struct FrameVersions {
    pub scope: u64,
    pub grouping: u64,
    pub as_of: u64,
    pub data: u64,      // bumped on every Published event
    pub config: u64,    // bumped when views/dimensions/groupings reload
}
```

Every mutation bumps exactly the counters it affects and notifies.
Tiles `observe` the entity and compare the counters they follow against
the ones they last acted on: a pinned tile ignores `grouping`, an
unscoped tile ignores `scope`, every tile follows `as_of`, `data` and
`config`. This is the "one uniform frame-subscription mechanism" of
§9.1, and it costs one integer compare per counter per notification.

`effective_scope(tile: &Scope) -> Scope` is `global.and_then(tile)`,
using the composition `Scope` already implements. Phase 3 passes an
empty tile layer; `:filter` will fill it.

### 4.2 Grouping slots

`groupings.toml` — a recognised atomic doc name that nothing read until
now:

```toml
config_version = 1

1 = ["desk", "book", "lhu", "position_ref"]
2 = ["underlying_ref", "book", "position_ref"]
```

Slots have no names. Users rebind them often, and inventing a name each
time is friction for nothing; a slot is referred to everywhere by its
number and its grouping string — `lhu / underlying_ref / position_ref`
— which is what a trader would say out loud anyway. The keys are the
slot numbers `1`–`9` as bare TOML keys, and the doc is atomic at depth
one, so a user layer overriding `3` replaces only slot 3 and inherits
the rest (§8). Any other key is a warning and ignored. A grouping naming
a column no dataset declares is an error for that slot only.
`GroupingSlots::from_doc` lives in `geode-core` beside the other config
readers, and `GroupingSlots::label(n)` renders the grouping string.

`:group save N` writes the tile's current grouping into slot N of the
*user* layer's `groupings.toml`, through the same `toml_edit` write path
theme, font size and find style already use, and updates the frame in
place. Rebinding a slot is one line, no name required.

`ctrl+1..9` → `frame::slot_1..9`; `ctrl+0` → `frame::slot_clear`,
returning every following tile to its view's own grouping. Unlike
`mod+N`, which switches workspaces, these are frame-level and flip every
following tile in every workspace.

Correction (3b final review): under the default `keymap.mod = "ctrl"`,
the shipped `workspace::switch_1..9` bindings already claim `ctrl+1..9`
and win — the keymap engine has no notion of "frame" taking priority
over "workspace" at the same chord, so setting a slot by key is not
reachable there. In practice a slot is set from the palette (every
action, including `frame::slot_1..9`, is always reachable there
regardless of what owns its default chord) or by rebinding the keymap.
`ctrl+0` → `frame::slot_clear` is unaffected: no `mod+0` binding exists
to collide with it, so it works as written above.

### 4.3 The `:` vocabulary

Every command below is a `blotter::` action reachable from the palette
too; the line is the fast path. `:` commands that change the frame do so
globally, as §4.3 says a `:scope` from any tile should.

| Line | Effect |
|---|---|
| `:group a,b,c` | Pin this tile to that grouping; detaches it from the slot. |
| `:group slot N` | Pin this tile to slot N regardless of the active slot. |
| `:group save N` | Write this tile's current grouping into slot N of the user layer, and rebind the frame's slot. |
| `:unpin` | Rejoin the frame's active slot. |
| `:unscoped` | Toggle ignoring the frame scope (§4.2). |
| `:scope <expr>` | Set the global expression filter, parsed by `parse_expr`, validated against the tile's dataset; a parse error shows at the caret offset on the line. |
| `:scope text <words>` | Set the global text filter. |
| `:scope clear` | Clear both, remembering the previous scope. |
| `:scope undo` | Restore the remembered scope. One level. |
| `:asof <time>` | Set the frame as-of. `HH:MM` means today; RFC 3339 for anything else. |
| `:live` | Return to live. |
| `:view <name>` | Show a different view in this tile. |
| `:sort <column> [desc]` / `:sort clear` | Sort siblings by a column (§6.3); `s` on the cursor column is the no-typing form. |

### 4.4 The readout

The toolbar's reserved middle shows, left to right: the active slot as
`2 · underlying_ref / book / position_ref` (number and grouping string,
or `view default`), the
scope as a compact summary (`book ∈ {3} · text "spx" · expr`), and, when
as-of is set, `AS OF 14:05` in the warning token with a warning
background across the whole readout. §4.5 asks for an unmissable
indicator; the readout is in the title bar of every window and coloured
end to end, which is as far as Phase 3 goes without the Phase 4
selector.

A pinned or unscoped tile shows `pinned` / `unscoped` in its own header
strip (§6.5), as §4.2 and §4.4 require.

### 4.5 Config reload

The shell's reload loop already diffs docs. Views, dimensions and
groupings are safe changes (§8), but they are not all handled the same
way: a `groupings`/`datasets`/`dimensions` change replaces the frame's
slots directly — pure presentation, recomputed shell-side from whatever
schema is on hand, nothing the data thread needs to hear about. Only a
`views`/`dimensions` change emits `ShellEvent::ConfigReloaded`, which
the app bridge forwards to the data thread as `ReplaceViews` (correction,
3b final review: an earlier draft of this section had groupings firing
the event too). Both kinds of change bump `config`. A change to
`sources` or `datasets` sets a status-bar diagnostic "sources changed —
restart to apply" and does nothing else.

## 5. The data path

### 5.1 `DataHandle`

`DataService` owns a DuckDB connection and is not `Sync`, so it lives on
one thread, as the probe already established. `DataHandle` is the
`Clone + Send + Sync` door:

```rust
pub struct DataHandle { requests: Sender<Request> /* bounded */ }

pub enum Request {
    Query { key: QueryKey, view: String, grouping: Option<Vec<String>>,
            scope: Scope, as_of: AsOf, max_depth: usize, tag: u64 },
    Cancel { key: QueryKey },
    ReplaceViews { views: Vec<ViewSpec>, dimensions: DerivedDimensions },
    Shutdown,
}

pub enum DataEvent {
    Query(QueryOutcome),                        // geode-core
    Published { dataset: String, batch: String, gen_id: i64,
                books: Vec<Option<String>> },
    Health { source: String, worst: Health, detail: String },
    Diagnostics(Vec<Diagnostic>),
}
```

`QueryOutcome` lives in `geode-core` so the shell can route it:

```rust
pub struct QueryKey(pub u64);            // the tile id
pub struct QueryOutcome {
    pub key: QueryKey,
    pub tag: u64,                        // echoed from the request
    pub snapshot: Result<Arc<Snapshot>, String>,
    pub submitted: Instant,
}
```

`DataService::spawn(config, events: async_channel::Sender<DataEvent>)
-> DataHandle` starts the service thread, which loops on the request
channel; the pool's result sender and the runner's event sender are
adapters onto the same outbound channel, so no forwarding thread exists.
The outbound channel is bounded (§7.3): one in-flight result per key
bounds it by tile count, and `Published` storms coalesce because the
frame only bumps a counter. A full channel drops the event and counts
it; the count is a diagnostic.

`grouping` overrides the named view's own grouping for this query: the
frame's active slot, or a tile's pin, is applied here rather than by
registering a view per slot. A column the schema does not declare fails
at compile time as this key's `Err` outcome, like an unknown view.

`geode-app`'s bridge drains the channel in one foreground task that
awaits the receiver — gpui's executor wakes it on send, so delivery
latency is a frame, not the probe's 250 ms poll. `Query` events go to
the occupant whose tile id is the key through `TileContent::deliver`;
`Published` bumps `Frame.versions.data`; `Health` updates the status
bar; `Diagnostics` go to stderr and the status bar.

The pool's coalescing map is keyed on `QueryKey` (§2.4). `tag` is the
tile's own request counter: a result whose `tag` is older than the
tile's latest submission is dropped on arrival even if the pool let it
through, so §7.3's "a stale result is never rendered" holds at both
ends.

`DataHandle::for_tests()` (feature `test-support`) returns a handle,
the `Receiver<Request>` a test drains, and the event sender it answers
on.

### 5.2 `sources.toml`

```toml
config_version = 1

[risk_files]
dataset = "risk_snapshot"
paths = ["/mnt/risk/current/*.csv", "//desk-share/risk/history/**/*.csv"]
readiness = "sentinel"            # | { stable_mtime = 2 }
priority = "latest_risk"          # | "latest_other" | "backfill"
poll_interval = "30s"
pending_timeout = "10m"
batch_pattern = '^risk_\d{4}-\d{2}-\d{2}_(?P<batch>.+)$'
```

One named table per source; atomic by name like the rest. `dataset`
must name a declared dataset (error, source skipped). `paths` are the
directory globs `discover` already takes. Durations are `Ns`, `Nm`,
`Nh`. `SourceSpec::from_doc(&MergedDoc) -> (Vec<SourceSpec>,
Vec<Diagnostic>)` lives in `geode-data::source`, because `Readiness` and
`Priority` are that crate's; the config *reading* pattern matches
`SchemaSpec::from_doc`. `StableMtime` is parsed and still reports
`Orphaned` at discovery, as today; implementing it is not Phase 3.

### 5.3 The scheduler

`DataServiceConfig` gains `sources: Vec<SourceSpec>`. `open` becomes:

1. Open the `Store`; apply schemas; ensure catalog tables.
2. Clone the reader connections the service needs: one for compilation,
   one per pool worker, one for discovery.
3. Move the `Store` into the single ingest runner (§2.5) with the
   `SchemaSpec`.
4. Start the discovery thread (`geode-discovery`): a min-heap of
   `(next_due, source)`; on each wake, `discover` that source against
   the catalog through its reader connection, `build_plan`, submit to
   the runner, re-arm at `now + poll_interval`. Every source is polled
   once immediately at start, so cold start is the same code path as the
   thirtieth poll. Discovery I/O runs on this thread and nothing waits
   on it (§5.1).
5. Validate views; hold diagnostics.

Publishes serialize through the runner's writer (§5.3). Readers see a
publish atomically. The runner's `Published` event becomes a `DataEvent`
and the frame's `data` counter bumps; every visible tile requeries with
latest-wins, so a burst of eleven files landing at once costs each tile
one query, not eleven.

A discovery error (unreadable share, bad glob) is a `Health` event for
that source and the poll re-arms; it is never fatal (§10.1).

### 5.4 The database path

`[app] data.db_path`, defaulting to the platform application-data
directory (`~/Library/Application Support/Geode/geode.duckdb`,
`%LOCALAPPDATA%\Geode\geode.duckdb`), resolved in `geode-app` beside
`user_config_dir`. `--demo` uses a path under the temp directory keyed
by row count and seed, so a demo never touches a real database.

### 5.5 Snapshot additions

All in `geode-core::snapshot`, with tests against what DuckDB emits
(narrow integers, both dictionary widths, nulls) rather than fixtures
alone — the prerequisites document's standing warning.

- **Index-based access.** `column_index(name) -> Option<usize>`,
  `meta_at(idx)`, `f64_at(idx, row)`, `i64_at(idx, row)`,
  `text_at(idx, row)`, `display_at(idx, row)`, each with the exact
  null and width semantics of its by-name twin. The by-name accessors
  remain, implemented over these. The blotter resolves every column
  once per snapshot into its column plan (§6.1) and never searches by
  name in a cell. `depth_of_row` reads a depth column index cached at
  construction.
- **`TreeIndex`.** Built in `from_batches` on the query worker:

  ```rust
  pub struct TreeIndex {
      parent: Vec<u32>,            // u32::MAX for the grand total
      child_start: Vec<u32>,       // CSR: children of row r are
      children: Vec<u32>,          //   children[child_start[r]..child_start[r+1]]
      depth: Vec<u8>,
  }
  ```

  The compiler orders by `row_depth` first, so every parent precedes
  its children. Within a depth the order is the view's declared sort,
  then the grouping columns — so siblings are *not* guaranteed
  contiguous, and the index does not assume it. Pass one hashes each
  row's grouping prefix (dictionary codes where present, strings under
  as-of, with NULL as its own token) into a per-depth map and looks up
  its parent in the map of the depth above; pass two counts children;
  pass three fills the CSR. O(rows × depth) hashes, no dependence on
  DuckDB's ENUM collation, and children keep the row order — which
  makes a declared `sort` the default sibling order for free.
  `children_of(row) -> &[u32]`, `parent_of(row)`, `depth(row)`. A row
  whose parent is not found (a stale ENUM blanked a value, P2 §3.6's
  known ambiguity) attaches to the grand total and is counted; the
  count is visible in the tile's footer as `n rows unplaced`, never
  silently dropped.
- **`ColumnFormat`** on `ViewColumn` (§6.2), parsed by
  `ViewSpec::from_doc`.
- **`AsOf`** (§2.7).

## 6. The blotter

### 6.1 The pure core (`geode-blotter::core`, no gpui)

**Column plan.** Built when a snapshot arrives whose column set differs
from the last (compared by name list, once per snapshot): per visible
column, `{ name, index, kind: Grouping | Measure | Attribute | Depth,
format, width, header }`. The first column is the tree column: the
grouping value for the row's own depth, indented by depth, with a
disclosure glyph when the row has children or could (depth <
grouping length). A view's dimension columns that belong to the active
grouping fold into the tree column rather than repeating beside it;
dimension columns outside the grouping stay as their own columns. A
rolled-up row shows its own level's value only, which is what the tree
column is.

**Expansion.** A set of *paths* — the grouping values from the root to
the node, as `Vec<String>` — not row indices, so it survives requery,
regroup and a snapshot that reorders siblings. Built at keypress time,
tens to hundreds of entries, never touched per frame. `za` toggles,
`zo` opens, `zc` closes the cursor row, or its parent when the cursor
row is a leaf or already closed, the way vim folds behave. `zR`/`zM`
open and close everything materialised.

**Flatten.** DFS over the tree index that descends only into expanded
nodes, writing visible row indices into a reused `Vec<u32>`. Cost
tracks the output. Runs on snapshot arrival, expand, collapse and sort
— never per frame. A sibling range is sorted inside flatten when a sort
is set (§6.3).

**Depth bound.** The tile requests `max_depth = min(grouping_len,
deepest expanded depth + 1)`, so one more expand is already in hand
(`docs/perf.md`). Expanding a node at the bound requeries with a larger
bound; that row shows a loading glyph until the result lands. Collapse
never requeries. The bound is computed from the expansion set at
request time, which over-fetches only for expanded nodes hidden under a
collapsed ancestor — bounded and harmless.

**Cursor.** `(visible_row, column)`. Row motion is `vimnav::apply` over
the flattened length: `j`/`k`, `gg`/`G`, `ctrl+d`/`ctrl+u` as the
existing vocabulary defines, each multiplied by the engine's count
prefix (§3.3): `5j` moves five, `12G` goes to row 12, `3ctrl+d` pages
three times. `h`/`l` move the column, also counted; `home`/`end` go to
the first and last. The cursor's *path* is remembered across requery so a
new snapshot puts the cursor back on the same node, falling back to a
clamped index when the node is gone.

**Visual mode.** `v` sets an anchor and enters `mode = visual`; motions
extend; `escape` leaves; `y` yanks the range (§6.4) and leaves.

**Find.** `/` opens the command line (§3.4); `[ui] find_style` decides
what typing into it does (§2.2). Under `vim` the query is matched with
`vimfind::find_match` against the tree column's text of each visible
row: the cursor jumps to the first match at or after it as the query
changes, Enter commits and closes the line, `escape` returns the cursor
to where it started, and `n`/`N` repeat with `repeat_find` (counted:
`3n` is the third match on). Under `fzf` the visible list is narrowed
with `vimfind::filter_matches` as the query changes, with the cursor on
the best match; Enter keeps the narrowed list with the cursor where it
is, `escape` restores the full list. `n`/`N` are vim-style only; under
`fzf` every visible row is a match. Matching is over the flattened
list, so it is pure UI within the materialised depth; it does not
requery.

**Format cache.** `Vec<Option<SharedString>>` sized visible rows ×
columns for the current visible window, keyed by `(snapshot Arc pointer,
window range)`. `DataTable` calls `visible_rows_changed` when the
window moves; the adapter refills only the rows that entered. A new
snapshot invalidates everything. `render_td` reads a cached string or
returns an empty cell; it never formats.

### 6.2 Formatting

`ColumnFormat` per view column, with per-type defaults:

```toml
[[tree.columns]]
name = "npv"
format = { precision = 0, thousands = true, negative = "parens", colour = "sign", scale = "k" }
label = "NPV (k)"
width = 110
```

| Field | Default (measure) | Default (dimension/attribute) |
|---|---|---|
| `precision` | 2 | — |
| `thousands` | true | — |
| `negative` | `"minus"` | — |
| `colour` | `"sign"` | `"none"` |
| `scale` | `"none"` | — |

`scale` is `"none"`, `"k"` or `"M"`: `k` divides the value by 1 000 and `M`
by 1 000 000 before display, and `precision` applies to the divided
number, so `scale = "k", precision = 0` shows 1 234 567.89 as `1,235`. The
header shows the scale after the label. `colour = "sign"` paints negatives `chart_bearish` and positives
`chart_bullish`, the tokens this repo's bundled themes already set for
exactly this (`theme.rs` records the Nord adjustment). Zero is the
foreground token. Numbers are `fonts::MONO`, right-aligned; dimensions
are left-aligned in the UI face. Dates and booleans go through
`display_at`.

### 6.3 Sorting

`s` in normal mode cycles the cursor column through ascending,
descending and cleared — `h`/`l` already put the cursor on the column,
so the common case needs no typing. `:sort delta01 desc` (completed,
§3.4) covers a column that is off-screen, and a header click does the
same by mouse. All three set the same tile-local sort, shown as an
arrow in the column header. It is applied inside flatten to each sibling range: siblings are compared on
the column's value with NULL last, ties keep row order. This is
view-shaping in-app (PHILOSOPHY §1 lists sorting as such) and it keeps
the compiler's determinism order untouched. A `NonAttributable` cell
sorts as NULL; a `DeterminedNonAdditive` cell sorts on its value — it is
real for its row, and sorting siblings is not totalling.

### 6.4 Yank

`y` in visual mode writes the range to the clipboard as TSV: a header
line of column labels, then one line per visible row with the tree
column indented by two spaces per depth, numbers at full precision
(not the display format, and unscaled), blanks for NULL. Raw values are what a
spreadsheet paste wants; the display format is what a screen wants.
`y` in normal mode yanks the cursor row.

### 6.5 Attribution and provenance rendering

The read path's opinions, applied without exception:

- A `NonAttributable` cell is NULL and paints **blank**. Never `0.00`.
  The adapter reads only through `f64_at`, which honours the null
  bitmap; `f64_column` is not called anywhere in the blotter, and a
  mutation entry proves a renderer routed through it would fail a test.
- A `DeterminedNonAdditive` cell paints its value in `muted_foreground`
  with a trailing `†`; the footer carries `† shown for this row, do not
  total`.
- A `SemiJoined` column marks its header with `⋈`; the footer names the
  dimensions applied by membership, in the probe's wording, so
  "positions with SPX risk" never reads as "the SPX share".
- Markers are per row: `attribution_by_depth[depth_of_row(row)]`,
  resolved once per (column, depth) into a small table in the column
  plan rather than per cell.
- The tile header strip shows per-dataset freshness from
  `Provenance`, stalest first: `risk 14:32 · ivol 07:00`. Past `[app]
  blotter.stale_after` (default `15m`) the strip turns to the warning
  token. When `as_of_request` is set the strip is prefixed `AS OF` in
  the warning tokens in addition to the title-bar readout.

### 6.6 The `DataTable` adapter

One `TableState<BlotterDelegate>` per tile: `row_selectable(true)`,
`col_selectable(false)`, `cell_selectable(false)`, `loop_selection(false)`,
`col_resizable(true)`, `col_movable(true)`, `sortable(true)`. The
delegate owns the snapshot `Arc`, the column plan, the flattened list,
the expansion set, the cursor and the format cache — every `render_td`
is a lookup.

- Cursor row → `set_selected_row`; cursor column is painted by the
  delegate in `render_td` with the `table_active_border` token; the
  visual range is painted in `render_tr` with `selection` at reduced
  opacity.
- `scroll_to_row` on every cursor move that leaves the visible window,
  `scroll_to_col` on `h`/`l` likewise.
- `TableEvent::SelectRow` from a click sets the cursor; the shell
  restores its focus (§3.3).
- `perform_sort` from a header click sets the tile sort (§6.3).
- `move_column` reorders the column plan in memory; not persisted.
- `rows_count` is the flattened length; the component paints its own
  filler rows past it, as it expects to.

The swap trigger is stated now: if a wide demo view (100 columns, 40
visible rows) misses the §7.1 8 ms frame on `j` in the perf overlay,
the adapter is replaced by a custom `Element` painting quads and shaped
lines. The pure core does not change.

### 6.7 Requery discipline

A tile requeries when: the frame counters it follows change; it becomes
visible with a stale counter; its own pin, view, or depth bound changes.
Each submit bumps the tile's `tag`, records `submitted`, and cancels
nothing — the pool supersedes by key. A result with a stale tag is
dropped. After 50 ms without a result the tile header shows a subtle
in-flight glyph (§7.1's "50–200 ms affordance"); an `Err` result paints
the tile's last good snapshot with the error in the header strip, never
a blank table (§10.1).

### 6.8 Timing

On `deliver`, the blotter records `submitted → now` as submit-to-snapshot
and marks the tile; the first `render` after the mark records
snapshot-to-paint and clears it. Both go to a shell-owned
`perf::RequeryStats` (last, p50, p95, count; fixed-size, allocation-free
like `FrameHistogram`) shown as two more lines in the `mod+shift+p`
overlay. This is the painted-frame half of §7.1 the benchmarks stop
short of, kept for good, always compiled.

## 7. Demo mode and the test story

### 7.1 `--demo`

`geode --demo [rows]` (default 100 000):

1. Emit the generated source directory to
   `<temp>/geode-demo/<rows>-<seed>/src` if absent, through
   `geode_demo_data::emit_directory`, sentinels included.
2. Layer a compiled-in demo config (`examples/demo-config/*.toml`,
   `include_str!`'d: `datasets`, `views`, `dimensions`, `groupings`,
   `sources` with `paths` rewritten to the emitted directory) between
   the builtin and desk layers.
3. Point `data.db_path` at `<temp>/geode-demo/<rows>-<seed>/geode.duckdb`.

Everything else is the normal startup. Drop a further generated file
into the source directory and the blotter updates on the next poll,
which is the §1.2 check. `geode-app` gains `geode-demo-data` as an
ordinary dependency; the generator is small and deterministic.

### 7.2 Tests

Test weight stays data ≫ shell logic ≫ module (§10.3).

- **Pure core (`geode-blotter::core`):** column plan against snapshots
  with dictionary and string dimensions and both integer widths;
  expansion by path across a reordered snapshot; flatten output for
  every combination of expanded, collapsed and bound-limited nodes;
  depth bound from an expansion set; cursor path restore; sort with
  NULL and `NonAttributable` cells; TSV yank; format for each field and
  default; find under both styles. Property test: flatten's output is a
  prefix-closed subsequence of a DFS of the tree index.
- **`geode-core`:** `TreeIndex` on results with a declared sort (siblings
  non-contiguous), with a blanked ENUM value, with 729k rows for cost;
  index accessors mirror by-name accessors under every type the by-name
  tests cover.
- **`geode-data`:** `sources` `from_doc` with every error; the scheduler
  discovering a file that appears after start and emitting `Published`;
  per-key coalescing with two keys on one view; `DataHandle` round trip;
  a full outbound channel counting drops.
- **`geode-shell`:** `Frame` counters bump exactly the fields touched;
  `GroupingSlots::from_doc` with a user layer overriding one slot and a
  non-numeric key; `:group save` round-trips through the user file; context stack
  includes the occupant's context; unknown action reaches the occupant
  with its count; the matcher accumulates digits only under a `counts`
  context, treats a leading `0` as a key, keeps the count across a
  `Pending`, clears it on `NoMatch` and `escape`, and caps it;
  session round trip with a `tiles` table and a dangling id; command
  line prompt and routing; completions ranked, accepted on `tab`,
  submitted on a unique Enter, refused inline on an ambiguous one;
  reclaimed `DataTable` bindings do not fire.
- **Module (`TestAppContext`):** a blotter tile with a `for_tests`
  snapshot and a `for_tests` handle: it paints (`debug_selector`); `j`
  moves the cursor and `5j` moves it five; `/` under each `find_style`
  jumps or narrows; `zo` expands and the flattened length grows; a
  `NonAttributable` cell's element has no text; a
  `DeterminedNonAdditive` cell carries the dagger; a frame slot change
  submits exactly one `Query` with the new grouping; a stale-tag result
  is not applied; a row click leaves the shell focused on the next
  frame; the timing record is written on deliver-then-render.
- **Mutation entries** for every behaviour above that a marker-only test
  could miss: tree index parent lookup, flatten's descent guard,
  `f64_at`'s null check, the tag comparison, the coalescing key, the
  matcher's count gate and its leading-zero rule, the
  scheduler's re-arm, the format defaults.

Timing is not asserted in CI. The recipe is `geode --demo 1000000`, the
overlay, and a row in `docs/perf.md`.

### 7.3 Benchmarks

`cargo bench -p geode-blotter`: `TreeIndex::build` and `flatten` at the
three result shapes `docs/perf.md` already records (133, 136 868 and
729 466 rows), format-cache fill for a 40 × 20 window, and the column
plan build over a 100-column snapshot. Not CI-gated, same as the rest.

## 8. Crate layout after Phase 3

```
geode-app       main, roster, data bridge, --demo, config/data dirs
  ├─ geode-blotter   core (pure), delegate (DataTable adapter), tile entity, factory
  ├─ geode-shell     + module (contract), frame, command line, groupings
  ├─ geode-data      + sources config, scheduler, DataHandle, per-key pool
  └─ geode-core      + index accessors, TreeIndex, ColumnFormat, AsOf, QueryOutcome
geode-demo-data     unchanged; now a dependency of geode-app
```

`geode-blotter` depends on `shell`, `data` and `core` (§2). `shell` and
`data` still never depend on each other. `geode-blotter` opens no file
and no socket.

## 9. Sequencing

The order is dictated by the constraint in `CLAUDE.md`: `[sources]`
lands before the probe leaves, and a test story for the painted frame
exists before the probe leaves.

1. **Data first, probe alive.** `AsOf` and `QueryOutcome` to core;
   per-key pool; single runner owning the store; `sources` from_doc and
   the scheduler; `DataHandle` and `spawn`. The probe is switched to
   `DataHandle` as the first consumer and keeps running.
2. **Core snapshot additions.** Index accessors, `TreeIndex`,
   `ColumnFormat`.
3. **Shell hosting and frame.** Occupant map, factory, roster plumbing,
   context stack, dispatch fall-through, command line, session `tiles`,
   `Frame`, `groupings.toml`, `ctrl+1..9`, readout, reload wiring,
   `RequeryStats` in the overlay.
4. **The blotter.** Pure core with its tests and benches, then the
   adapter, the tile entity, the factory; `--demo`.
5. **Delete the probe.** `dataprobe.rs`, `probe.rs`, `data::toggle_probe`
   and its binding, `examples/probe-config` (replaced by
   `examples/demo-config`), the `GEODE_PROBE_DIR` recipe in
   `docs/perf.md` and `CLAUDE.md`, and the `geode-data` dependency
   comment in `geode-app`'s manifest.

Each step is green on all four CI checks on both platforms and adds its
mutation entries before the next begins.

## 10. Open questions

None block the plan. Recorded so they are not rediscovered:

1. **`DataTable` under a wide view.** §6.6 states the swap trigger; the
   measurement happens in step 4 with the 100-column demo view.
2. **Where sibling sort belongs long-term.** In flatten (§6.3) it is
   free and correct within the materialised depth. If a future shape
   wants the *top N* siblings of a 100k-child node without materialising
   all of them, sort moves into the compiler with a `limit` — a
   benchmark question, not an assumption.
3. **Scope undo depth.** One level is what `:scope undo` needs to feel
   safe. Phase 4's scope bar decides whether it becomes a stack.
4. **`StableMtime` readiness** stays unimplemented; the vol and
   instrument-reference sources that need it have no upstream yet
   (P2 §3.7).
