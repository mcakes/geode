# Market-Data Documents, Part 3 — The Panel, Insert Mode, Keymap Fragments and Delivery

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The palette's "CVI: Split" opens a tile that shows one CVI document as a virtualised matrix — key, attributes, source time and staleness in the header, terms down the side, nodes across the top — follows the bus as new generations land, lets a trader move a cell cursor, yank, find, edit cells into a draft that survives a restart and a newer publish, and says honestly which generation the draft sits on.

**Architecture:** One new module crate, `geode-marketdata`, with a pure core (`PanelSpec`, `MatrixModel`, `Draft` — no gpui) and one gpui entity per tile whose body is a gpui `uniform_list` over the row axis. Three shell changes ride along, each generic: `TileContent::deliver` takes a `Delivery` enum; modules ship their default keybindings as keymap fragments through `ModuleFactory::default_keymap`, retiring the reserved action tables in `geode_shell::defaults`; and the shell's key handler gains an insert-mode rule so a tile-owned `Input` receives typed characters. Nothing in the data tier changes. Egress (`:upload`) is Part 4.

**Tech Stack:** Rust, gpui (pinned rev `e3adf43`, `uniform_list` + `UniformListScrollHandle`), gpui-component (`Input`/`InputState` for the cell editor), `geode-data`'s `DataHandle::document`, `scripts/mutation-check.sh`, criterion.

**Spec:** `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md` — **§8 (as amended 2026-09-13: §8.2 uniform list, §8.3 insert mode, §8.4 base = source time, §8.6 the three shell changes) and §12 part 3 are this plan's whole brief**; §4.5 and §5.6 are the binding as-built record of what the panel consumes; §9 (egress) is Part 4 and NOT in scope beyond the `:upload` stub. **Also binding:** roadmap ruling 6 as revised 2026-09-13 (`docs/superpowers/specs/2026-09-12-geode-modules-roadmap.md` §2), CLAUDE.md's "Workspace invariants" (the focus-restore rule, `DataTable` reclaim, the `UiSettings` global), `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md` §3 (module hosting) and §6.2 (formatting), `docs/PHILOSOPHY.md` §2, §3, §6.

## Global Constraints

- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. Run all five before every commit that touches Rust. No platform-specific code.
- **TDD**: failing test first, watch it fail for the right reason, then implement. Module tests use `gpui::TestAppContext` with a real window (`crates/geode-diagnostics/src/tile.rs`'s `open`/`open_with` harness is the template) and drive keys through `TileContent::dispatch`/`command`/`find`, never by poking private state.
- **A mutation entry for every behaviour changed**, appended immediately after the last `run_mutation` entry in `scripts/mutation-check.sh` (before the `if [[ -n "$changed_ref" ]]` block), each naming its covering test as the 6th argument (`geode-shell`, `geode-marketdata`, `geode-blotter`, `geode-diagnostics`, `geode-app`, `geode-core`, `geode-data`). Anchors unique (`grep -c -F` = 1; multi-line anchors counted whole). `zsh scripts/mutation-check.sh --anchors-only` exits 0 before every commit — `defaults.rs`, `shell/input.rs`, `module.rs`, `occupants.rs`, `bridge.rs` and `blotter/core/format.rs` are anchored; re-anchor to the same site and meaning, never delete. **Commit before you mutate.** Run the harness detached with ONE bounded background wait; never repeated pollers, never a foreground sleep.
- **Nothing stalls the render thread**: the `MatrixModel` is built once per snapshot or draft change, never in `render`; `render` clones prepared `SharedString`s; only the visible rows are laid out (`uniform_list`); no per-frame heap churn beyond gpui's own element tree.
- **Never a raw colour** — every colour from `cx.theme()` tokens. Fonts: `geode_shell::fonts::MONO` for cells (the data face), the UI face for the header.
- **A module never touches the shell's focus handle** (CLAUDE.md focus rule): the cell editor's `InputState` is dropped on commit/cancel and the shell's own `render` net restores focus. A module never binds keys outside the keymap engine and never opens a file, socket or config file.
- **Every action is keyboard-reachable** (PHILOSOPHY §2); a mouse path may exist only beside a key path.
- **Doc comments explain WHY, densely. A comment contradicting the code is a defect.** Comments this plan makes false and must rewrite: `defaults.rs`'s "Mirrors `geode_blotter::tile::ACTIONS` exactly — the shell cannot depend on the blotter crate…" (the whole rationale for the reserved tables), `module.rs`'s `deliver` doc ("A query result addressed to this tile"), `input.rs`'s filter-field branch comment ("The filter field owns its own key handling while it has focus" — now one of two such rules), `crates/geode-blotter/src/core/format.rs`'s module doc if `format_number` moves.
- **Layering**: `geode-marketdata` depends on `geode-core`, `geode-shell`, `geode-data` (for `DataHandle`) — never on `geode-blotter` or `geode-diagnostics`; `geode-shell` never on `geode-data` or a module. Shared formatting therefore lives in core (Task 5), not in the blotter.
- **Every new lib target `bench = false`; every `[[bench]] harness = false`.**

## As-built vocabulary this plan builds on

```rust
// crates/geode-shell/src/module.rs
pub trait TileContent {
    fn key_context(&self, cx: &App) -> KeyContext;                  // e.g. KeyContext::new("blotter").pair("mode", "normal").counts()
    fn dispatch(&self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut App) -> bool;
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String>;   // Err shown inline on the `:` line
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String>;                // bare WORDS for the cursor's position
    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App);                     // FindEvent::{Changed(String), Committed(String), Cancelled}
    fn deliver(&self, outcome: QueryOutcome, window: &mut Window, cx: &mut App);            // Task 2 changes this
    fn set_visible(&self, visible: bool, cx: &mut App);            // a fresh occupant treats itself as hidden until the first call
    fn serialize(&self, cx: &App) -> toml::Table;
}
pub struct TileOccupant { pub kind: &'static str, pub view: AnyView, pub content: Box<dyn TileContent> }
pub trait ModuleFactory {
    fn kind(&self) -> &'static str;
    fn register_actions(&self, registry: &mut ActionRegistry);     // runs before build_keymap
    fn create(&self, tile: TileId, restored: Option<&toml::Table>, frame: Entity<Frame>, diagnostics: Entity<Diagnostics>, window: &mut Window, cx: &mut App) -> TileOccupant;
}
pub struct ModuleRoster;  // add(Box<dyn ModuleFactory>), factory(kind), kinds(), register_actions(registry)
pub mod placeholder { pub struct PlaceholderFactory; pub const PLACEHOLDER_KIND: &str; }
#[cfg(any(test, feature = "test-support"))] pub mod recording { pub struct RecordingFactory { pub log, pub completions, pub command_result, pub last_focus: Rc<RefCell<Option<FocusHandle>>> }; pub enum Recorded { Created, Dispatch, Command, Find, Visible, Delivered(TileId, u64) } }

// crates/geode-shell/src/shell/occupants.rs
impl ShellView { pub fn deliver(&mut self, outcome: QueryOutcome, window, cx);    // routes to occupants[TileId(outcome.key.0)].content.deliver
                 pub(super) fn ensure_occupants(..); fn holds_shell_focus(&self, handle: &FocusHandle, cx) -> bool }   // shell root + four Entity<InputState>s
// crates/geode-shell/src/shell/input.rs
impl ShellView { pub(super) fn handle_key_down(&mut self, event: &KeyDownEvent, window, cx);   // modal → command line → filter field (chords via single_keystroke_binding against [workspace]) → matcher
                 pub(super) fn context_stack(&self, cx) -> Vec<KeyContext>;                     // [workspace, tile, <occupant context>, (palette)]
                 fn single_keystroke_binding(&self, ks: &Keystroke, stack: &[KeyContext]) -> Option<&Binding>;
                 fn dispatch(&mut self, action: &ActionId, count: Option<u32>, window, cx) }
// crates/geode-shell/src/keymap/build.rs
pub fn build_keymap(layered: &[LayerDoc], mod_alias: Modifiers, registry: &ActionRegistry) -> (Keymap, Vec<Diagnostic>)   // reads each doc's [[bindings]] with `context` predicate + [bindings.keys]
// crates/geode-core/src/config/mod.rs
pub enum Layer { Builtin, Desk, User }
pub struct LayerDoc { pub layer: Layer, pub name: String, pub file: PathBuf, pub table: toml::Table }   // LayerDoc::builtin(name, text) -> Result<LayerDoc, _>
impl Config { pub fn layered_docs(&self, name: &str) -> &[LayerDoc] }   // builtin first
// crates/geode-shell/src/defaults.rs
pub const BUILTIN_KEYMAP: &str;   // holds [[bindings]] for workspace, tile, "blotter && mode == normal", "blotter && mode == visual", "diagnostics"
pub const BLOTTER_ACTION_DEFS / BLOTTER_ACTIONS / DIAGNOSTICS_ACTION_DEFS / DIAGNOSTICS_ACTIONS;   // retired by Task 3
pub fn register_add_actions(reg: &mut ActionRegistry, kinds: &[&str]);   // "Kind: Split" rows per roster kind — the panel gets its palette rows for free
// crates/geode-shell/src/shell/mod.rs
pub struct ShellServices { pub config: Config, .., pub registry: ActionRegistry, pub keymap: Keymap, pub roster: ModuleRoster, pub keymap_diagnostics: Vec<Diagnostic>, .. }
// crates/geode-shell/src/shell/hot_reload.rs  apply_reload: build_keymap(new_config.layered_docs("keymap"), mod_alias, &self.services.registry)
// crates/geode-app/src/main.rs ~710: let (keymap, keymap_diags) = build_keymap(config.layered_docs("keymap"), mod_alias, &registry);
//                             ~499/525: BlotterFactoryHandle(Rc<BlotterFactory>) / DiagnosticsFactoryHandle — thin ModuleFactory forwarders

// crates/geode-shell/src/frame.rs
impl Frame { pub fn versions(&self) -> FrameVersions /* scope, grouping, as_of, data, config, flip */; pub fn as_of(&self) -> &AsOf; pub fn recent_publishes(&self) -> &VecDeque<Publish>; pub fn note_published(&mut self, Publish) /* bumps versions.data */ }
pub struct Publish { pub dataset: String, pub batch: String, pub books: usize, pub at: DateTime<Utc> }
// crates/geode-shell/src/diagnostics.rs
pub struct Diagnostics { pub catalog: Option<CatalogSnapshot>, .. }   // request_catalog(&mut self) queues; CALLER MUST cx.notify() in the same update block
// crates/geode-core/src/query.rs
pub struct DocumentParams { pub key: QueryKey, pub tag: u64, pub submitted: Instant, pub dataset: String, pub document_key: Vec<String>, pub as_of: AsOf }
pub struct QueryOutcome { pub key: QueryKey, pub tag: u64, pub snapshot: Result<Arc<Snapshot>, String>, pub submitted: Instant }
pub struct CatalogSnapshot { pub datasets: Vec<DatasetCatalog { name, partitions: Vec<PartitionCatalog { batch, book, generations, resolved_gen }> , .. }>, .. }
// crates/geode-data/src/handle.rs
impl DataHandle { pub fn document(&self, params: DocumentParams) -> bool; pub fn cancel(&self, key: QueryKey) -> bool; pub fn for_tests() -> (DataHandle, Receiver<Request>) }
// crates/geode-core/src/snapshot.rs
impl Snapshot { fn rows(&self) -> usize; fn column_index(&self, name) -> Option<usize>; fn f64_at(idx, row) -> Option<f64>; fn text_at(idx, row) -> Option<&str>; fn display_at(idx, row) -> Option<String>; fn provenance(&self) -> &Provenance { datasets: Vec<Freshness { dataset, as_of: Option<String> /* RFC 3339, per document for live */, generation }>, as_of_request } }
// crates/geode-core/src/document.rs
pub fn join_key(parts: &[String]) -> String; pub fn split_key(batch: &str) -> Vec<String>;
// crates/geode-core/src/view.rs
pub struct ColumnFormat { pub precision: u8, pub thousands: bool, pub negative: Negative, pub colour: Colour, pub scale: Scale }   // ColumnFormat::MEASURE, ::TEXT
// crates/geode-blotter/src/core/format.rs
pub fn format_number(value: f64, format: &ColumnFormat) -> Formatted   // Task 5 moves this to core
// crates/geode-blotter/src/content.rs — BlotterFactory { data: DataHandle, .., find_style: Rc<Cell<FindStyle>>, stale_after: Rc<Cell<Duration>> }; the app's bridge builds it and refreshes it on ConfigReloaded
// crates/geode-diagnostics/src/tile.rs — the uniform_list + UniformListScrollHandle + sync_scroll() cursor pattern; tests: open()/open_with(restored) harness, Host { tile, frame, diagnostics, .. }, VisualTestContext
// gpui e3adf43: uniform_list(id, item_count, |range, window, cx| Vec<impl IntoElement>).track_scroll(&handle); UniformListScrollHandle::new(); .scroll_to_item(ix, ScrollStrategy::Nearest)
// gpui-component: cx.new(|cx| InputState::new(window, cx)); Input::new(&state) element; state.read(cx).value(); state.update(cx, |s, cx| s.set_value(text, window, cx)) — set_value emits no Change; state.read(cx).focus_handle(cx)
```

---

### Task 1: Part 2's parked items (one batched dispatch)

Five small, independent edits of the same shape. One implementer, one review.

**Files:**
- Modify: `crates/geode-diagnostics/src/sections.rs` (discriminate on `adapter`)
- Modify: `crates/geode-core/src/source_config.rs` (topic pattern validation)
- Modify: `crates/geode-data/src/ingest/subscribe.rs` (`first_sighting`, capped unknown set, deterministic shutdown test)
- Modify: `crates/geode-demo-data/src/documents.rs` (skip a month whose third Friday precedes the anchor)
- Modify: `scripts/mutation-check.sh`

- [ ] **(a) `sections.rs`:** the sources section decides "subscribed" by `summary.adapter != geode_core::source_config::CSV_DIR_ADAPTER`, not `topics.is_empty()`. Test: a `SourceSummary` with `adapter: "demo_bus"` and empty topics still paints the subscribed row shape. Entry: the comparison flipped.
- [ ] **(b) `source_config.rs`:** each `topics` entry is validated at load: an empty string, an empty level (`a//b`), or a `>` in a non-final level is an Error at `sources.<name>.topics` naming the pattern, source skipped; `*` and a final `>` are fine. Tests for the three refusals and one acceptance; entry on the non-final-`>` check.
- [ ] **(c) `subscribe.rs`:** extract `fn first_sighting(&mut self, path: &str) -> bool` (true once per distinct path; the set is capped at `UNKNOWN_PATH_CAP = 256` entries — past the cap every path reports `true` once more and a single `warn` says the cap was hit); unit test on the pure method (first true, second false, cap behaviour); entry on the `contains` guard. Replace the shutdown test's 125 ms timing bound with the liveness form: unsubscribe, do NOT set the stop flag, `wait_until` the thread `is_finished()` (the only remaining exit is the `Disconnected` arm), then `join`; keep the existing `mem::forget`-a-sink mutation biting (it now makes the thread never finish → the bounded `wait_until` fails).
- [ ] **(d) `documents.rs` (demo generator):** `expiries` starts at the first month whose third Friday is on or after the anchor; test: anchor 2026-09-19 (the day after September's third Friday) → first term is 2026-10-16; entry on the skip condition.
- [ ] **(e)** Gates, commit `parked: Part 2 residuals — adapter discriminator, topic validation, first_sighting, expiry skip, liveness shutdown test`, run the new entries filtered and detached, confirm `caught`.

---

### Task 2: `Delivery` — the tile route becomes an enum

**Files:**
- Modify: `crates/geode-shell/src/module.rs` (`Delivery`, `TileContent::deliver`, placeholder + recording impls)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (`ShellView::deliver`)
- Modify: `crates/geode-app/src/bridge.rs` (`DataEvent::Query(outcome)` → `s.deliver(Delivery::Query(outcome), ..)`)
- Modify: `crates/geode-blotter/src/content.rs`, `crates/geode-diagnostics/src/lib.rs` (their `TileContent::deliver` impls)
- Test: `module.rs` tests, `shell/tests/` (whatever test today asserts a `Recorded::Delivered`), blotter/diagnostics tests that call `deliver`

**Interfaces:**
- Produces:

```rust
/// What the shell routes to a tile by its id (market-data spec §8.6).
/// One variant in Part 3; Part 4 adds `Upload(UploadOutcome)` and every
/// `match` on this enum then refuses to compile until its arm exists —
/// which is the point of an enum over a second trait method.
#[derive(Debug)]
pub enum Delivery {
    Query(QueryOutcome),
}
impl Delivery { pub fn key(&self) -> QueryKey }
// TileContent
fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App);
// ShellView
pub fn deliver(&mut self, delivery: Delivery, window: &mut Window, cx: &mut Context<Self>);   // routes by delivery.key()
```

- [ ] **Step 1: Write the failing tests** — in `module.rs`: `RecordingContent::deliver` records `Recorded::Delivered(tile, tag)` from a `Delivery::Query`; in the shell's hosting tests (find the existing "a query outcome reaches the addressed tile and no other" test) change the call to `Delivery::Query(..)`. Run → compile errors.
- [ ] **Step 2: Implement** — the enum, the trait signature, `ShellView::deliver` matching on `delivery.key()`, the bridge arm, the four occupant impls (`match delivery { Delivery::Query(outcome) => .. }` — an exhaustive match, no wildcard, so Part 4's arm is forced). Rewrite `deliver`'s doc comment.
- [ ] **Step 3: Gates, commit** `shell: TileContent::deliver takes a Delivery enum (Query now, Upload in Part 4)`. Mutation entry: `ShellView::deliver` routing by key (mutate `TileId(delivery.key().0)` to `TileId(0)`) naming the hosting test.

---

### Task 3: Keymap fragments — modules ship their own default bindings

**Files:**
- Create: `crates/geode-shell/src/keymap/fragments.rs`
- Modify: `crates/geode-shell/src/keymap/mod.rs` (`pub mod fragments;`)
- Modify: `crates/geode-shell/src/module.rs` (`ModuleFactory::default_keymap`, `contexts`; `ModuleRoster::keymap_fragments`)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`ShellServices.keymap_fragments: Vec<LayerDoc>`), `crates/geode-shell/src/shell/hot_reload.rs` (`apply_reload` splices), `crates/geode-app/src/main.rs` (builds fragments from the roster, splices, fills `ShellServices`; the two `*FactoryHandle` forwarders forward the new methods)
- Modify: `crates/geode-shell/src/defaults.rs` (delete the `blotter && …` and `diagnostics` `[[bindings]]` sections from `BUILTIN_KEYMAP`; delete `BLOTTER_ACTION_DEFS`, `BLOTTER_ACTIONS`, `DIAGNOSTICS_ACTION_DEFS`, `DIAGNOSTICS_ACTIONS`, their registration in the shell's builtin action registration, and the `defaults.rs` mirror tests)
- Modify: `crates/geode-blotter/src/content.rs` + `tile.rs` (ship the fragment; delete `the_shells_reserved_blotter_actions_match_ours`), `crates/geode-diagnostics/src/lib.rs` (same)
- Test: `fragments.rs`, `module.rs`, `shell/keybindings_view.rs` tests (fragment bindings show as `Layer::Builtin` and `r` reset restores them), `shell/tests/` hosting tests, `defaults.rs` tests
- Modify: `scripts/mutation-check.sh` (defaults.rs anchors will move — `--anchors-only` after every edit)

**Interfaces:**
- Produces:

```rust
// module.rs
pub trait ModuleFactory {
    ..existing..,
    /// The contexts this module's `key_context` can name (`["blotter"]`).
    /// A fragment binding whose predicate does not name one of these as
    /// its FIRST identifier is dropped with an error diagnostic, so a
    /// fragment can never shadow a shell binding or another module's.
    fn contexts(&self) -> &'static [&'static str] { std::slice::from_ref(&self.kind()) }   // default: the kind itself
    /// Default bindings, as keymap TOML (`[[bindings]]` tables only).
    fn default_keymap(&self) -> Option<&'static str> { None }
}
impl ModuleRoster { pub fn keymap_fragments(&self) -> (Vec<LayerDoc>, Vec<Diagnostic>) }   // one LayerDoc per factory with a fragment, in roster order, each passed through check_fragment
// keymap/fragments.rs
pub fn fragment_doc(kind: &str, text: &str) -> Result<LayerDoc, Diagnostic>;   // Layer::Builtin, name "keymap", file "<module:{kind}>"
pub fn check_fragment(doc: LayerDoc, contexts: &[&str]) -> (LayerDoc, Vec<Diagnostic>);   // drops [[bindings]] entries whose predicate's first identifier is not in `contexts`, or that have no context at all
pub fn splice(layered: &[LayerDoc], fragments: &[LayerDoc]) -> Vec<LayerDoc>;   // builtin docs, then fragments, then desk/user docs — fragments sit above the built-in keymap and below every layer a trader edits
```

`main.rs` after the roster is complete and actions registered: `let (fragments, frag_diags) = roster.keymap_fragments(); let layered = fragments::splice(config.layered_docs("keymap"), &fragments); let (keymap, keymap_diags) = build_keymap(&layered, mod_alias, &registry);` — `frag_diags` join `keymap_diagnostics`. `hot_reload::apply_reload` splices `self.services.keymap_fragments` the same way. The keybindings dialog reads bindings with their `Layer`; a fragment binding is `Builtin`, so `r` (reset) restores it and `d` (unbind) records a user-layer unbind over it exactly as for a built-in binding — one test each in `keybindings_view.rs`'s test module using a `RecordingFactory` with a fragment.

The blotter's fragment is `BUILTIN_KEYMAP`'s two `blotter && mode == …` sections moved verbatim into `crates/geode-blotter/src/content.rs` as `pub const DEFAULT_KEYMAP: &str`; the diagnostics fragment likewise. `RecordingFactory` gains `pub fragment: Option<&'static str>` and `pub contexts: &'static [&'static str]` so the shell's own tests can host a module with a fragment.

- [ ] **Step 1: Write the failing tests**

`fragments.rs`:

```rust
    #[test]
    fn a_fragment_binding_outside_the_modules_contexts_is_dropped_with_a_diagnostic() {
        let doc = fragment_doc("rec", "[[bindings]]\ncontext = \"rec && mode == normal\"\n[bindings.keys]\n\"j\" = \"rec::down\"\n\n[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"ctrl+q\" = \"rec::quit\"\n\n[[bindings]]\n[bindings.keys]\n\"x\" = \"rec::x\"\n").unwrap();
        let (kept, diags) = check_fragment(doc, &["rec"]);
        let entries = kept.table["bindings"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert!(diags[0].message.contains("workspace") && diags[0].message.contains("rec"));
        assert!(diags[1].message.contains("no context"));
        assert_eq!(kept.layer, Layer::Builtin);
        assert_eq!(kept.file.to_string_lossy(), "<module:rec>");
    }

    #[test]
    fn splice_puts_fragments_after_the_builtin_docs_and_before_desk_and_user() {
        let builtin = LayerDoc::builtin("keymap", "").unwrap();
        let desk = LayerDoc { layer: Layer::Desk, name: "keymap".into(), file: "/desk/keymap.toml".into(), table: toml::Table::new() };
        let user = LayerDoc { layer: Layer::User, name: "keymap".into(), file: "/user/keymap.toml".into(), table: toml::Table::new() };
        let frag = fragment_doc("rec", "").unwrap();
        let out = splice(&[builtin.clone(), desk.clone(), user.clone()], &[frag.clone()]);
        let files: Vec<String> = out.iter().map(|d| d.file.to_string_lossy().to_string()).collect();
        assert_eq!(files, vec![builtin.file.to_string_lossy().to_string(), "<module:rec>".to_string(), "/desk/keymap.toml".to_string(), "/user/keymap.toml".to_string()]);
    }
```

`module.rs`: `ModuleRoster::keymap_fragments` returns one doc per factory that ships one, in roster order, with `check_fragment` applied (a `RecordingFactory` with a bad-context fragment yields the diagnostic). Shell hosting test (`shell/tests/`): a `RecordingFactory` with fragment `"[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n"` → after `build_keymap` over `splice(..)`, pressing `q` on a focused `rec` tile records `Dispatch(.., rec::noop)`; and a user-layer doc binding `q` to something else in the same context wins. Blotter test: `DEFAULT_KEYMAP` parses and binds every `blotter::*` action exactly once per context (replacing `the_shells_reserved_blotter_actions_match_ours`); same for diagnostics. Keybindings dialog: a fragment binding lists as `Builtin`.

- [ ] **Step 2: Run to verify failure** — compile errors.
- [ ] **Step 3: Implement** — as specified. `check_fragment`'s "first identifier" is the first `[A-Za-z_][A-Za-z0-9_]*` token of the predicate string (`parse_predicate` already exists in `keymap/build.rs`; if it exposes the leading identifier, use it). Delete the four reserved tables and everything that referenced them (the shell's builtin action registration of those defs, the `defaults.rs` mirror tests, the two module mirror tests). Rewrite `defaults.rs`'s rationale comment into a one-paragraph pointer at fragments.
- [ ] **Step 4: Gates** — `cargo test --workspace`; the palette must still list every blotter/diagnostics action (registration is unchanged — `register_actions` still runs first); `--anchors-only` → 0 (re-anchor `defaults.rs` entries that sat inside the deleted sections to the moved text in the module crates, same meaning).
- [ ] **Step 5: Mutation entries and commit** — entries: `check_fragment` drops a foreign context (→ keeps it); `splice` order (fragments after user → the user-wins test catches); the blotter fragment binding `j` (remove the line; named the blotter fragment test). Commit `keymap: modules ship default bindings as fragments; the reserved action tables are retired`.

---

### Task 4: Insert mode — the shell lets a tile-owned input type

**Files:**
- Modify: `crates/geode-shell/src/shell/input.rs` (`handle_key_down`: the insert-mode branch)
- Modify: `crates/geode-shell/src/module.rs` (`RecordingFactory` gains an insert-mode toggle: `pub insert: Rc<Cell<bool>>` flipping `key_context` to `.pair("mode", "insert")`, and its view owns an `Option<Entity<InputState>>` it creates and focuses on `rec::edit` and drops on `rec::commit`/`rec::cancel`)
- Test: `crates/geode-shell/src/shell/tests/input.rs` (or wherever the filter-field chord tests live)

**Interfaces:**
- Produces, in `handle_key_down`, placed after the filter-field branch and before the matcher:

```rust
        // Insert mode (market-data spec §8.6): a tile occupant that owns
        // a focused `Input` — the cell editor — reports `mode == insert`
        // in its key context. While it does, and window focus is on a
        // handle the shell does not own, only SINGLE-keystroke bindings
        // resolve against the context stack (the module fragment's
        // `escape`/`enter` in `<kind> && mode == insert`, and chords);
        // every other keystroke propagates untouched to the focused
        // input, and the matcher's sequence/count state is never fed —
        // the same shape as the filter-field branch above, generalised to
        // a tile. This exists because this listener sits on the window
        // root and sees every raw keystroke: without it a typed `j`
        // would also move the panel's cursor. The occupant drops its
        // `InputState` on commit or cancel; the dropped handle is what
        // `render`'s `window.focused(cx).is_none()` net turns back into
        // shell focus — no module touches the shell's focus handle.
        let stack = self.context_stack(cx);
        let tile_in_insert = stack.iter().any(|c| c.get("mode") == Some("insert"));   // adapt to KeyContext's real accessor
        if tile_in_insert
            && window.focused(cx).is_some_and(|f| !self.holds_shell_focus(&f, cx))
        {
            if let Some(ks) = convert_keystroke(&event.keystroke)
                && let Some(action) = self.single_keystroke_binding(&ks, &stack).map(|b| b.action.clone())
                && action.0 != UNBOUND_ACTION
            {
                self.dispatch(&action, None, window, cx);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
```

- [ ] **Step 1: Write the failing tests** (shell hosting tests with the recording module):
  - `typed_keys_reach_a_tiles_focused_input_in_insert_mode`: host a `rec` tile with a fragment binding `escape` → `rec::cancel` and `enter` → `rec::commit` in context `rec && mode == insert`, and `j` → `rec::down` in `rec && mode == normal`; dispatch `rec::edit` (the view creates and focuses its `InputState`); simulate typing `j`, `1`, `.`, `5` → the input's value is `"j1.5"` and NO `Dispatch(rec::down)` was recorded; press `escape` → `Dispatch(rec::cancel)` recorded, the view drops its input, and after one render `window.focused(cx)` is the shell root again (the existing net).
  - `chords_still_dispatch_from_insert_mode`: `ctrl+k` opens the palette (the workspace binding resolves as a single keystroke).
  - `a_count_prefix_typed_in_insert_mode_is_text_not_a_count`: `3` then `j` → value `"3j"`, no dispatch.
- [ ] **Step 2: Run to verify failure** — `j` dispatches `rec::down` today.
- [ ] **Step 3: Implement** — the branch above; `RecordingFactory`'s insert plumbing. Rewrite the filter-field branch's "owns its own key handling" comment to say there are now two such rules.
- [ ] **Step 4: Gates, commit** `shell: insert mode — a tile-owned focused input receives typed keys; only single-keystroke bindings resolve`. Mutation entries (`geode-shell`): the branch removed (→ `j` dispatches); the `holds_shell_focus` guard inverted (→ the filter field's own typing would go through this branch — cover with the existing filter tests plus this one).

---

### Task 5: `geode-marketdata`'s pure core — spec, matrix model, draft, formatting in core

**Files:**
- Create: `crates/geode-marketdata/Cargo.toml`, `src/lib.rs`, `src/core/mod.rs`, `src/core/spec.rs`, `src/core/matrix.rs`, `src/core/draft.rs`, `benches/matrix.rs`
- Move: `crates/geode-blotter/src/core/format.rs`'s `format_number`/`Formatted` → `crates/geode-core/src/format.rs` (blotter's `core::format` re-exports; its tests stay where they are, importing the re-export; mutation entries anchored inside the moved body are re-anchored to the new file)
- Modify: root `Cargo.toml` members
- Test: each core file's tests; a proptest that `MatrixModel::build` over a pivot never loses or duplicates a cell

**Interfaces:**
- Produces (`geode_marketdata::core`, no gpui):

```rust
pub enum Columns { Axis(&'static str), Values }
pub struct PanelSpec { pub kind: &'static str, pub title: &'static str, pub dataset: &'static str, pub document: &'static str, pub rows: &'static str, pub columns: Columns, pub header: &'static [&'static str], pub format: ColumnFormat }
pub const CVI: PanelSpec = PanelSpec { kind: "cvi", title: "CVI", dataset: "cvi_params", document: "cvi_params", rows: "term", columns: Columns::Axis("node"), header: &["anchor_date", "spot_ref"], format: ColumnFormat { precision: 4, thousands: false, negative: Negative::Minus, colour: Colour::None, scale: Scale::Unit } };   // adapt variant names to view.rs

pub struct CellRef { pub row: usize, pub col: usize }
pub struct MatrixModel {
    pub key: Vec<String>,
    pub source_time: Option<String>,             // provenance as_of, RFC 3339
    pub header: Vec<(SharedString, SharedString)>,   // (label, value) per header attribute
    pub columns: Vec<SharedString>,              // column labels
    pub rows: Vec<RowModel>,                     // one per row-axis value (pivot) or per snapshot row (flat)
}
pub struct RowModel { pub label: SharedString, pub cells: Vec<Cell> }
pub struct Cell { pub text: SharedString, pub value: Option<f64>, pub edited: bool, pub sent: bool, pub cell_ref: (usize, usize) /* (snapshot row, value column) for Values; (row, col) grid index for Axis */ }
impl MatrixModel {
    /// Pivot or flatten `snapshot` per `spec`, overlay `draft`'s edits (formatted with `spec.format`),
    /// prepare every string once. Allocation: one Vec per row plus the strings; nothing per frame.
    pub fn build(snapshot: &Snapshot, spec: &PanelSpec, draft: &Draft) -> Result<MatrixModel, String>;
    pub fn empty(spec: &PanelSpec, key: &[String]) -> MatrixModel;   // "no document received" shape
    pub fn label_of(&self, cell: (usize, usize)) -> (SharedString, SharedString);   // (row label, column label)
}
/// Row and column labels resolve a cell across generations (spec §8.4 rebase).
pub struct Draft { pub base: Option<String>, pub edits: BTreeMap<(usize, usize), f64>, pub state: DraftState, labels: BTreeMap<(usize, usize), (String, String)> }
pub enum DraftState { Clean, Editing, Behind { newer: String }, Sent }
impl Draft {
    pub fn set(&mut self, cell: (usize, usize), labels: (String, String), value: f64, base: &str);   // Clean→Editing on the first edit; records base
    pub fn revert(&mut self) -> usize;
    pub fn bump(&mut self, cells: impl Iterator<Item = ((usize, usize), (String, String), f64 /* current */)>, delta: f64, base: &str) -> usize;
    pub fn on_delivered(&mut self, as_of: &str) -> bool;   // Editing + as_of != base → Behind { newer }; returns whether state changed
    pub fn rebase(&mut self, model_of_newer: &MatrixModel) -> (usize /* kept */, Vec<(String, String)> /* dropped labels */);
    pub fn discard(&mut self);
    pub fn summary(&self) -> String;   // "3 edits on 14:02's document" / "newer document received 14:07" / ""
    pub fn to_toml(&self) -> toml::Table; pub fn from_toml(t: &toml::Table) -> Draft;   // edits as label pairs + base
}
pub fn parse_cell(text: &str, ty: ColumnType) -> Result<f64, String>;   // f64/i64 only; message names the text
```

- [ ] **Step 1: Write the failing tests**

`matrix.rs`: build over a six-row CVI-shaped `Snapshot::for_tests` (two terms × three nodes) with `Columns::Axis("node")` → `rows.len() == 2`, `columns == ["-20", "-1", "3.5"]`, cell (0,1) text is the formatted param, `cell_ref` maps back to the grid; with `Columns::Values` over a flat five-column snapshot → one row per snapshot row, `columns` are the value column names; an edited cell paints the draft's value and `edited == true`; a snapshot missing a `(term,node)` cell (a hole) errors naming the pair; `empty()` has no rows and the key; `header` picks the attributes from row 0. Proptest: for random T×N grids the pivot's cell count equals T×N and every source value appears exactly once.

`draft.rs`: `set` twice on one cell keeps the latest; `on_delivered` with the same `as_of` stays `Editing`, with a different one goes `Behind` and does not touch `edits`; `rebase` onto a model that has (term A, node -1) but not (term B, ..) keeps A's edit at its NEW index and reports B's labels dropped; `discard` clears; `to_toml`/`from_toml` round-trips edits by labels and `base`; `summary` strings; `bump` adds delta to each given cell's current value.

`geode_core::format`: the blotter's existing `format_number` tests pass unchanged through the re-export.

- [ ] **Step 2: Run to verify failure** — compile errors.
- [ ] **Step 3: Implement** — as specified. Time-of-day in `summary` is the trader's LOCAL clock (`HH:MM` from the RFC 3339 `base`, converted with `chrono::Local`), matching the frame's rule.
- [ ] **Step 4: Bench** — `benches/matrix.rs`: `MatrixModel::build` at 20×30 (`Axis`) and 10,000×5 (`Values`), and `Draft::rebase` over 1,000 edits.
- [ ] **Step 5: Gates, commit** `marketdata: the pure core — PanelSpec, MatrixModel, Draft; format_number moves to core`. Mutation entries (`geode-marketdata`): the pivot's row order (first appearance → sorted); the hole check; `on_delivered`'s `as_of != base`; `rebase` keeps by label (→ by index); `from_toml` reading `base`. Plus the re-anchored blotter format entries confirmed `caught`.

---

### Task 6: `MarketDataTile` — the entity, the request, the uniform-list body, the factory, the roster

**Files:**
- Create: `crates/geode-marketdata/src/tile.rs`, `src/content.rs` (factory + `TileContent` impl), `src/commands.rs` (the `:` vocabulary and completions, pure)
- Modify: `crates/geode-app/src/main.rs` (`MarketDataFactoryHandle`, roster add — after the bridge, since it needs the `DataHandle`), `crates/geode-app/src/bridge.rs` (`Bridge` exposes what the factory needs; `ConfigReloaded` refreshes `stale_after` on it like the blotter's), `crates/geode-app/Cargo.toml`
- Test: `tile.rs` tests with the diagnostics harness pattern (`open`/`open_with`), `commands.rs` tests

**Interfaces:**
- Produces:

```rust
pub struct MarketDataFactory { data: DataHandle, spec: &'static PanelSpec, stale_after: Rc<Cell<Duration>> }
impl MarketDataFactory { pub fn new(data: DataHandle, spec: &'static PanelSpec, stale_after: Duration) -> Self; pub fn set_stale_after(&self, d: Duration) }
impl ModuleFactory for MarketDataFactory { kind = spec.kind; contexts = &["marketdata"]; register_actions = ACTIONS below under category "Market data"; default_keymap = DEFAULT_KEYMAP; create → MarketDataTile }
pub const ACTIONS: &[(&str, &str)] = &[
    ("marketdata::down", "Cursor down"), ("marketdata::up", "Cursor up"), ("marketdata::left", "Cursor left"), ("marketdata::right", "Cursor right"),
    ("marketdata::top", "Cursor to top"), ("marketdata::bottom", "Cursor to bottom"), ("marketdata::first_col", "First column"), ("marketdata::last_col", "Last column"),
    ("marketdata::page_down", "Half page down"), ("marketdata::page_up", "Half page up"),
    ("marketdata::yank", "Yank cell"), ("marketdata::yank_row", "Yank row"), ("marketdata::yank_col", "Yank column"),
    ("marketdata::edit", "Edit cell"), ("marketdata::commit", "Commit edit"), ("marketdata::cancel", "Cancel edit"),
    ("marketdata::find_next", "Find next"), ("marketdata::find_prev", "Find previous"), ("marketdata::escape", "Escape"),
];
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "marketdata && mode == normal"
[bindings.keys]
"j" = "marketdata::down"
"k" = "marketdata::up"
"h" = "marketdata::left"
"l" = "marketdata::right"
"g g" = "marketdata::top"
"shift+g" = "marketdata::bottom"
"0" = "marketdata::first_col"
"$" = "marketdata::last_col"
"home" = "marketdata::first_col"
"end" = "marketdata::last_col"
"ctrl+d" = "marketdata::page_down"
"ctrl+u" = "marketdata::page_up"
"y" = "marketdata::yank"
"y y" = "marketdata::yank_row"
"y c" = "marketdata::yank_col"
"i" = "marketdata::edit"
"enter" = "marketdata::edit"
"n" = "marketdata::find_next"
"shift+n" = "marketdata::find_prev"
"escape" = "marketdata::escape"

[[bindings]]
context = "marketdata && mode == insert"
[bindings.keys]
"enter" = "marketdata::commit"
"escape" = "marketdata::cancel"
"#;

pub struct MarketDataTile {
    id: TileId, spec: &'static PanelSpec, frame: Entity<Frame>, diagnostics: Entity<Diagnostics>, data: DataHandle,
    key: Option<Vec<String>>, tag: u64, acted: Option<(AsOf, u64 /* data version */)>, visible: bool,
    snapshot: Option<Arc<Snapshot>>, model: MatrixModel, draft: Draft,
    cursor: (usize, usize), scroll: UniformListScrollHandle,
    editor: Option<Entity<InputState>>,          // Some while in insert mode
    find: Option<FindState>, notice: Option<String>, stale_after: Rc<Cell<Duration>>,
}
```

Behaviour:
- **Request.** `requery` submits `DocumentParams { key: QueryKey(id.0), tag: next, dataset: spec.dataset, document_key: key, as_of: frame.as_of() }` whenever visible and (`key` set) and (`acted` differs in `as_of` or the frame's `data` version — every publish bumps it; the document select is cheap). `deliver(Delivery::Query(o))`: drop a stale tag; on `Ok`, `draft.on_delivered(as_of)`, rebuild `model` (from the NEW snapshot when not `Behind`, from the retained base snapshot when `Behind` — keep `base_snapshot: Option<Arc<Snapshot>>` alongside), clamp the cursor, `sync_scroll`, `cx.notify()`; on `Err`, keep the last model and set `notice`. A `flip` version is ignored (the blotter's rule, same reason). `set_visible(false)` cancels the in-flight key.
- **Header.** `"{title} {key}"`, then each header attribute `label: value`, then the source time as local `HH:MM:SS` with the blotter's staleness rule (`now - source_time > stale_after` → the theme's warning colour and the word `stale`), then `draft.summary()`, then `notice`. `"no document received for {key}"` when the model is empty and a key is set; `"no key — :key <value>"` when none.
- **Body.** Column labels row (a fixed strip, `MONO`), then `uniform_list("marketdata-rows", model.rows.len(), move |range, _, cx| ..)` rendering each row as label + cells, the cursor cell in the theme's accent background, an edited cell in the theme's `warning`-tinted background, a sent cell in `muted`; the editor `Input` painted IN the cursor cell while `editor.is_some()`. `sync_scroll` after every cursor move (`ScrollStrategy::Nearest`).
- **Catalog keys.** `completions("key <prefix>")` reads `diagnostics.read(cx).catalog` → the dataset's partitions → `split_key(batch).join(KEY_SEPARATOR display "/")`; when the catalog is `None` or stale (no entry for the dataset), the tile calls `diagnostics.update(cx, |d, cx| { d.request_catalog(); cx.notify(); })` — the trap in CLAUDE.md — on `set_visible(true)` and on every `:` line that starts with `key`.
- **`:` vocabulary** (`commands.rs`, pure `Command` enum + `parse(line) -> Result<Command, String>` + `completions(line, cursor, keys: &[String]) -> Vec<String>`): `key <value>`, `revert`, `bump <delta> [row|col]`, `rebase`, `discard`, `upload` (→ `Err("upload is not built yet")`); `rebase`/`discard` are offered only when `draft.state` is `Behind`.
- **Session.** `serialize` → `{ key = [..], draft = { base, edits = [[row_label, col_label, value], ..] } }`; `create(restored)` restores the key and the draft; the first delivery with a different `as_of` lands in `Behind`.
- **Yank.** `cx.write_to_clipboard(ClipboardItem::new_string(text))` — the cell's text; the row as tab-separated `label\tcell..`; the column as newline-separated.
- **Find.** `/` over row labels through the shared find line using `geode_shell::vimfind` as the blotter does (`FindEvent::Changed` narrows the cursor to the first matching row; `Committed` keeps it; `n`/`N` step matches; `Cancelled` restores the origin).

- [ ] **Step 1: Write the failing tests** (`tile.rs`, harness `open(spec, restored) -> (Harness { tile, frame, diagnostics, data_rx }, VisualTestContext)` with `DataHandle::for_tests()`):
  - `a_tile_with_a_key_requests_its_document_when_shown` — `:key SPX.Z`, `set_visible(true)` → a `Request::Document { dataset: "cvi_params", document_key: ["SPX.Z"], key: QueryKey(tile) }` on `data_rx`; hidden → none.
  - `a_delivered_snapshot_builds_the_matrix_and_the_header` — deliver a six-row CVI snapshot (build with `Snapshot::for_tests`, provenance `as_of: Some("2026-09-12T14:00:00Z")`) → `model.rows.len() == 2`, header has `spot_ref`, source time painted local.
  - `a_stale_tag_is_dropped`; `an_error_outcome_keeps_the_last_model_and_shows_the_notice`.
  - `a_publish_bumps_the_frame_and_the_tile_requeries` — `frame.update(note_published(..))` → a second `Request::Document`.
  - `cursor_moves_with_counts_and_scrolls` — `dispatch(down, Some(3))` → cursor row 3; `bottom` → last row; the scroll handle's target (assert through a `#[cfg(test)]` accessor like the diagnostics tile's `last_scroll_target`).
  - `key_completions_come_from_the_catalog` — seed `diagnostics.catalog` with two partitions → `completions("key ", 4)` is `["NDX.Z", "SPX.Z"]`; with no catalog, the tile requested one (`diagnostics.read(cx).pending_catalog_request()` — add a `#[cfg(any(test, feature = "test-support"))]` accessor if none exists).
  - `serialize_round_trips_key_and_draft`.
  - `yank_writes_the_cell_row_and_column` (assert through `cx.read_from_clipboard()`).
  - `find_moves_the_cursor_to_a_matching_row_label`.
- [ ] **Step 2: Run to verify failure** — compile errors.
- [ ] **Step 3: Implement** — as specified; `content.rs` mirrors the blotter's `BlotterContent` shape; `main.rs` adds `MarketDataFactoryHandle` and `roster.add(..)` after the bridge start with `bridge.handle.clone()`; `ConfigReloaded` refreshes `stale_after`.
- [ ] **Step 4: Gates, commit** `marketdata: the panel tile — document request, uniform-list matrix, header, catalog keys, yank, find; registered as "cvi"`. Mutation entries: stale-tag drop; requery on `data` bump (→ never); catalog request when none (→ no request); `serialize` writes the draft (→ omits it); the cursor clamp on a shorter document.

---

### Task 7: Cell editing — insert mode, `:bump`, `:revert`

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs`, `src/commands.rs`
- Test: `tile.rs`

Behaviour: `marketdata::edit` on a value cell (`Axis` pivot: every cell is a value; `Values`: only value columns — an attribute column, if a spec ever lists one, refuses with a notice) creates `editor = Some(cx.new(|cx| InputState::new(window, cx)))` seeded with the cell's current text (draft value if edited), focuses it (`window.focus(&state.read(cx).focus_handle(cx))`), and `key_context` reports `mode = insert`. `marketdata::commit` reads the value, `parse_cell(text, ty)` (the column's declared type — `f64` for CVI), on `Ok` → `draft.set(cell, labels, value, base)` and rebuild the model, on `Err` → `notice = Some(msg)` and stay in insert mode; `marketdata::cancel` drops the editor. Both drop `editor` on success — the dropped handle returns focus to the shell root (the shell net; Task 4). `:bump <delta> [row|col]` (default row) applies `draft.bump` over the cursor's row or column; `:revert` clears the draft.

- [ ] **Step 1: Write the failing tests** — `edit_commit_paints_the_cell_as_edited_and_the_header_counts_it` (dispatch `edit`, set the editor's value via `update(|s, cx| s.set_value("0.5", window, cx))`, dispatch `commit` → cell text `"0.5000"`, `edited`, header contains `1 edit`, `editor.is_none()`, `key_context` back to normal); `cancel_drops_the_editor_without_a_change`; `a_non_numeric_commit_refuses_inline_and_stays_in_insert_mode`; `bump_adds_to_the_cursors_row_by_default_and_to_its_column_on_request`; `revert_clears_every_edit`; `key_context_reports_insert_while_the_editor_exists`; and one hosting test in the SHELL's tests using the real factory (`geode-shell` cannot depend on `geode-marketdata` — so this lives in `geode-marketdata` with a full `ShellView` harness like the blotter's hosting tests, if one exists; otherwise the Task 4 recording test is the rule's evidence and this task asserts only the tile's side).
- [ ] **Step 2–4: RED, implement, GREEN, gates, commit** `marketdata: cell editing — insert mode with a tile-owned input, :bump, :revert`. Mutation entries: commit parses before writing (→ writes the raw text as 0.0); the editor dropped on commit (→ kept: the key-context test catches); `bump` default `row` (→ `col`).

---

### Task 8: Draft states — `Behind`, `:rebase`, `:discard`, session into `Behind`

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs`, `src/commands.rs`
- Test: `tile.rs`

Behaviour per spec §8.4: a delivery whose `as_of` differs from `draft.base` while `Editing` → `Behind { newer }`; the tile keeps painting the BASE snapshot under the edits (retain `base_snapshot`), the header says `newer document received HH:MM`; `:rebase` rebuilds the model from the newer snapshot and `draft.rebase(&new_model)` (dropped labels → notice `"dropped N edits whose rows or columns the new document lacks: …"`), state → `Editing` with `base = newer`; `:discard` drops the edits and shows the newer snapshot, `Clean`. `rebase`/`discard` are `Err("nothing to rebase — the draft is on the live document")` outside `Behind`. Restoring a session with a draft and receiving a newer document lands in `Behind` on the first delivery.

- [ ] **Tests:** `a_newer_generation_under_a_draft_goes_behind_and_keeps_painting_the_base`; `rebase_reapplies_edits_by_label_and_reports_dropped_ones`; `discard_shows_the_newer_document_clean`; `rebase_outside_behind_is_refused`; `a_restored_draft_lands_in_behind_on_a_newer_delivery`; `completions_offer_rebase_and_discard_only_while_behind`.
- [ ] **Implement, gates, commit** `marketdata: draft states — Behind keeps the base painted; :rebase by label; :discard`. Mutation entries: `Behind` paints the base (→ the newer); `rebase` drops a missing label (→ keeps a stale index); `discard` clears edits.

---

### Task 9: Documentation, as-built §8.7, perf numbers

**Files:**
- Modify: `CLAUDE.md` (a "Market-data documents Part 3" paragraph after Part 2's; the Commands block gains `cargo bench -p geode-marketdata`; the workspace-invariants list gains one bullet on insert mode and one on keymap fragments)
- Modify: spec §8 (a new §8.7 "As built (Part 3)": the insert-mode rule as built, `Delivery` with one variant, fragments' context rule and layer position, `format_number` in core, the catalog-request trap, `Behind` retaining the base snapshot, `:upload` stub), §12 part 3 marked done
- Modify: `docs/perf.md` (a "Market-data panel" section: the Task 5 bench p50s for `MatrixModel::build` at 20×30 and 10,000×5 and `Draft::rebase` at 1,000 edits; the paint at 10,000 rows recorded from the perf overlay's frame histogram on a display if one is available, else the template line the blotter section used and an explicit "not yet measured on a display")
- Modify: `scripts/mutation-check.sh` header (add "the panel's matrix model and draft")
- Modify: the roadmap spec §6 (Part 3 done; Part 4 remaining)

- [ ] Run the two benches once and record; write the docs, every sentence checked against code; gates; commit `docs: Part 3 of market-data documents — CLAUDE.md, spec §8.7, perf numbers`; run the harness over changed files detached (`--changed=<branch base>`), report survivors, then hand off per `superpowers:finishing-a-development-branch`.

---

## Self-review

**Spec coverage (§8, §12 part 3):** §8.1 crate/roster/`PanelSpec` → Tasks 5, 6; §8.2 header/uniform list/scroll/model → Task 6 (model in Task 5); §8.3 keys, insert mode, `:` vocabulary, fragment → Tasks 4, 6, 7 (`:upload` stub in Task 6's `commands.rs`); §8.4 draft and states → Tasks 5, 7, 8; §8.5 session → Tasks 6, 8; §8.6 `Delivery` → Task 2, fragments → Task 3, insert mode → Task 4; §12 part 3 "the crate, the spec, the matrix model, keys, the draft with Behind/rebase/discard, session, Delivery, keymap fragments, the reserved-list deletion" → all above; Part 2's parked items → Task 1; §11's panel tests → Tasks 6–8, measured → Tasks 5, 9.

**Placeholders:** Task 4's `c.get("mode")` is marked "adapt to KeyContext's real accessor"; Task 5's `ColumnFormat` literal is marked "adapt variant names to view.rs"; Task 7 notes the hosting-test placement depends on whether a `ShellView` harness exists outside `geode-shell`. Everything else is concrete.

**Type consistency:** `Delivery` (Task 2) is what Task 6's `deliver` matches; `ModuleFactory::{contexts, default_keymap}` and `DEFAULT_KEYMAP` (Tasks 3, 6) agree; `MatrixModel`/`Draft`/`parse_cell` (Task 5) are what Tasks 6–8 call by those names; `RecordingFactory`'s `insert`/fragment fields (Tasks 3, 4) are what the shell tests use; `ScrollStrategy::Nearest` and `UniformListScrollHandle` match the pinned gpui.
