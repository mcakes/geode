# Diagnostics Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the diagnostics tile with a full page reached from the
sidebar, over a new shell page seam that later hosts a database explorer.

**Architecture:** `geode-shell` gains a `PageContent`/`PageFactory` seam and a
`PageRoster` beside the module seam; `ShellView` holds at most one page and,
while it is open, paints it where the toolbar, tile surface, and command line
would be, hides the tiles beneath, and routes keys through a `page` context
stack. `geode-diagnostics` drops its tile and implements the page: pure typed
row models, one `TableDelegate` over a prepared table for every table section,
a detail strip, and controls that change application state only through the
`Diagnostics` entity's request channels or a shell-actions handle. `geode-app`
registers the page factory.

**Tech Stack:** Rust, GPUI (`gpui-pre =0.3.5`), gpui-component 0.6.2
(`DataTable`/`TableDelegate`, `Input`, `Select`, `Switch`, `Popover`, `Button`),
gpui-kit-assets 0.6.2 icons, TOML session persistence.

**Spec:** `docs/superpowers/specs/2026-09-27-diagnostics-page-design.md`

## Global Constraints

- Work in a git worktree (`superpowers:using-git-worktrees`), never on main.
- `geode-shell` never depends on a feature crate or on `geode-data`. The page
  seam lives in `geode_shell::module`; the diagnostics page lives in
  `geode-diagnostics`; `geode-app` composes.
- While a page is open: toolbar, tile surface, and command line are not
  painted; sidebar and status bar are. Modals, palette, which-key, perf
  overlay, and notifications paint above the page.
- Context stack while a page is open: `page`, then the page's own context,
  then `palette` if open. No `workspace`, no `tile`.
- One toggle action per page, `page::toggle_<kind>`, titled "<Title>: Open
  page" in category `<Title>`; one shell builtin `page::close` ("Close page",
  category "Workspace") bound to `escape` in context `page`.
- The diagnostics toggle's default binding is `mod+d`, supplied by the
  factory's `toggle_binding` and emitted by the roster as a shell-generated,
  unchecked fragment.
- A page changes application state only through `Diagnostics` request
  channels (`request_level`, `request_overlay_toggle`, `request_catalog`) or
  the `ShellActions` handle it was created with. It never holds `ShellView`.
- The session file gains `[pages.<kind>]`; whether a page was open at quit
  is not saved. The app starts in the workspace.
- The retained log tail stays at 4,096 records; the loss gap is measured at
  the last drain, not cumulatively.
- Warning and error tones come from `geode_shell::shell::chip::chip_paint`;
  no literal colors, radii, or unexplained fixed pixels (`shell::scale`).
- Say "color", not "colour", in any new user-facing text. Key hints in
  tooltips use `shell::kbd`, never inline chips in copy.
- Before building any gpui-component surface, read the registry source at
  `~/.cargo/registry/src/*/gpui-component-0.6.2/src/` and `gpui-base-0.6.2/src/`
  for the exact signature; do not translate from memory. Signatures quoted in
  this plan were read from that source on 2026-09-27.
- CLAUDE.md commands must pass at the end of every task: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
  `cargo check -p geode-shell --features test-support --all-targets`,
  `zsh scripts/mutation-check.sh --anchors-only`.
- Every mutation-harness entry names its detecting test (6th argument) and
  lives inline in `scripts/mutation-check.sh` as a `run_mutation` call. Where
  this plan writes a multi-line anchor, write literal newlines inside the
  quoted argument as the existing entries do.
- Commit after every task with a conventional message and the trailer
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

## Review Focus

1. **A `page::toggle_diagnostics` dispatched from the palette while a modal
   is open.** Expected: ignored with the "close the dialog first" notice; the
   modal stays. Pinned in Task 2 (`toggle_under_a_modal_is_refused`).
2. **A workspace switch chord pressed while the page is open and the page's
   filter input is focused.** Expected: the chord still switches (chords
   resolve against the whole stack in the insert branch), the page closes,
   the input is blurred by the close. Pinned in Task 2
   (`a_workspace_switch_chord_from_a_focused_page_input_closes_the_page`).
3. **A session whose `[pages.diagnostics]` table names a section that no
   longer exists.** Expected: the page opens on Sources with no panic and the
   record is replaced on the next flush. Pinned in Task 7
   (`an_unknown_saved_section_restores_as_sources`).
4. **The log ring wraps while the page is closed but created.** Expected: on
   reopening, the Log section's loss row reports the gap since the last drain
   and the tail is capped at 4,096. Pinned in Task 10
   (`a_wrap_while_closed_is_reported_on_the_next_drain`).
5. **A frame as-of change while the page is open with Data selected, catalog
   still pending.** Expected: the resolved marker is hidden until the catalog
   as-of matches, and a watched refresh was requested. Pinned in Task 8
   (`resolved_markers_wait_for_a_matching_catalog`).

---

## File structure

**geode-shell**

- Modify `crates/geode-shell/src/module.rs`: `PageContent`, `PageOccupant`,
  `ShellActions`, `PageFactory` (+ `Rc<F>` forwarder), `PageRoster`, and the
  `recording::RecordingPageFactory` test double.
- Modify `crates/geode-shell/src/defaults.rs`: `page::close` builtin action
  and binding, `register_page_actions`.
- Create `crates/geode-shell/src/shell/page.rs`: `OpenPage`, open/close/
  toggle, the `ShellActions` handle, `page_open`, `page_stack`.
- Modify `crates/geode-shell/src/shell/mod.rs`: `ShellServices.pages`,
  `ShellServices.restored_pages`, `ShellView.page`, `last_pages_written`,
  `set_perf_overlay`.
- Modify `crates/geode-shell/src/shell/input.rs`: dispatch routing and
  `context_stack`.
- Modify `crates/geode-shell/src/shell/occupants.rs`: `fill_active_tiles`,
  `visible_tile_keys`, `occupant_insert_stack` consult the page.
- Modify `crates/geode-shell/src/shell/render.rs`: page extent, guards.
- Modify `crates/geode-shell/src/shell/sidebar.rs`: page buttons.
- Modify `crates/geode-shell/src/shell/status.rs`: tooltip copy.
- Modify `crates/geode-shell/src/session.rs`, `shell/session_io.rs`:
  `[pages.<kind>]`.
- Modify `crates/geode-shell/src/diagnostics.rs`: `DIAGNOSTICS_PAGE_KIND`,
  `overlay_visible` mirror.
- Modify `crates/geode-shell/src/perf.rs`: `FrameHistogram::buckets`,
  `overflow`.
- Create `crates/geode-shell/src/shell/tests/pages.rs`; modify
  `tests/diagnostics.rs`, `tests/mod.rs`, `tests/session.rs`.

**geode-diagnostics** (rewritten)

- `src/lib.rs`: `DiagnosticsPageFactory`, `ACTIONS`, `DEFAULT_KEYMAP`,
  `PageContent` adapter.
- `src/section.rs`: `Section` enum (moved from `commands.rs`, which is
  deleted).
- `src/model.rs`: typed rows per section and badge counts. Pure.
- `src/prepared.rs`: `PreparedTable`, `PreparedRow`, `Cell`, `ColumnSpec`,
  builders from typed rows with expansion and filtering. Pure.
- `src/table.rs`: `SectionDelegate: TableDelegate` over `Rc<PreparedTable>`.
- `src/page.rs`: `DiagnosticsPage` entity: observers, state, key handling,
  render of header, rail, toolbar, table, detail strip.
- `src/log.rs`: `LogTail` (drain, cap, loss gap) and `LogFilter`. Pure.
- `src/levels.rs`: `LevelsState` (pure) and the popover render.
- `src/perf_view.rs`: stat tiles and the histogram element.
- `src/tile.rs`, `src/commands.rs`, `src/sections.rs`: deleted.

**geode-app**

- Modify `src/main.rs`: `PageRoster`, page factory, fragments, actions.
- Modify `src/assets.rs`: `Activity` icon in `ExtraIcons`.
- Modify `src/bridge.rs` test that creates a diagnostics tile.

**docs / scripts**

- `docs/current/shell.md`, `features.md`, `input-and-dialogs.md`,
  `keymaps.md`, `crates/geode-diagnostics/README.md`,
  `crates/geode-shell/README.md`, `docs/perf.md`, `scripts/mutation-check.sh`.

---

### Task 1: The page seam in `geode_shell::module` and `defaults`

**Files:**
- Modify: `crates/geode-shell/src/module.rs` (after `ModuleRoster`, ~line 455; and inside `pub mod recording`, ~line 573)
- Modify: `crates/geode-shell/src/defaults.rs` (`BUILTIN_KEYMAP` ~line 85, `register_builtin_actions` ~line 219, after `register_add_actions` ~line 466)
- Test: `crates/geode-shell/src/module.rs` (tests module), `crates/geode-shell/src/defaults.rs` (tests module)

**Interfaces:**
- Consumes: `fragments::{fragment_doc, check_fragment}`, `ActionRegistry`, `KeyContext`, `Frame`, `Diagnostics`, `gpui_kit_assets::IconName`.
- Produces (used by every later task):

```rust
pub trait PageContent {
    fn key_context(&self, cx: &App) -> KeyContext;
    fn dispatch(&self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut App) -> bool;
    fn set_visible(&self, visible: bool, cx: &mut App);
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle;
    fn holds_focus(&self, window: &Window, cx: &App) -> bool;
    fn title(&self, cx: &App) -> SharedString;
    fn serialize(&self, cx: &App) -> toml::Table;
}
pub struct PageOccupant { pub kind: &'static str, pub view: AnyView, pub content: Box<dyn PageContent> }
pub type ShellActions = Rc<dyn Fn(&ActionId, &mut Window, &mut App)>;
pub trait PageFactory {
    fn kind(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn icon(&self) -> gpui_kit_assets::IconName;
    fn register_actions(&self, registry: &mut ActionRegistry);
    fn contexts(&self) -> Vec<&'static str>;          // default vec![self.kind()]
    fn default_keymap(&self) -> Option<&'static str>; // default None
    fn toggle_binding(&self) -> Option<&'static str>; // default None
    fn create(&self, restored: Option<&toml::Table>, frame: Entity<Frame>, diagnostics: Entity<Diagnostics>, actions: ShellActions, window: &mut Window, cx: &mut App) -> PageOccupant;
}
pub struct PageRoster { .. }  // new, add, factory, kinds, entries, register_actions, keymap_fragments
pub struct PageEntry { pub kind: &'static str, pub title: &'static str, pub icon: gpui_kit_assets::IconName }
pub fn defaults::register_page_actions(reg: &mut ActionRegistry, pages: &[(&str, &str)]);
pub mod recording { pub struct RecordingPageFactory; pub enum PageRecorded { Created, Visible(bool), Action(String) } }
```

- [ ] **Step 1: Write the failing roster test**

Append to the `#[cfg(test)] mod tests` at the bottom of `crates/geode-shell/src/module.rs`:

```rust
    #[test]
    fn page_roster_emits_checked_fragments_and_an_unchecked_toggle_doc() {
        let mut roster = PageRoster::new();
        roster.add(Box::new(recording::RecordingPageFactory::new("diagnostics")));
        assert_eq!(roster.kinds(), vec!["diagnostics"]);
        let entries: Vec<_> = roster.entries().map(|e| (e.kind, e.title)).collect();
        assert_eq!(entries, vec![("diagnostics", "Diagnostics")]);
        let (docs, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        // One doc from `default_keymap` (checked against `contexts`) and one
        // generated from `toggle_binding` (context-free, unchecked).
        assert_eq!(docs.len(), 2);
        let toggle = docs.iter().find(|d| d.file.to_string_lossy().contains("page:diagnostics"))
            .expect("the toggle doc is named after the page");
        let text = toml::to_string(&toggle.table).unwrap();
        assert!(text.contains("\"mod+d\" = \"page::toggle_diagnostics\""), "{text}");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p geode-shell page_roster_emits -- --nocapture`
Expected: FAIL to compile, `PageRoster` not found.

- [ ] **Step 3: Add the seam types after `ModuleRoster`'s impl in `module.rs`**

Insert before `pub mod placeholder {`:

```rust
/// The content contract for a page: a surface that replaces the workspace
/// (toolbar, tile surface, command line) while open. Pages own their own
/// inputs, have no `:` line, and receive no deliveries; they persist through
/// `[pages.<kind>]` rather than the layout tree.
pub trait PageContent {
    /// Pushed innermost on the key context stack while the page is open.
    /// Carries `mode = insert` while one of the page's inputs is focused.
    fn key_context(&self, cx: &App) -> KeyContext;
    /// An action the shell did not recognise. `true` if handled. For
    /// `page::close`, `true` means the page consumed the close (it had
    /// something of its own to dismiss) and the shell must not close it.
    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool;
    /// Opening announces `true`; closing `false`. A fresh occupant assumes it
    /// is hidden until the first call.
    fn set_visible(&self, visible: bool, cx: &mut App);
    /// The handle the shell focuses on open; the page view tracks it.
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle;
    /// `true` while one of the page's own text inputs owns keyboard focus,
    /// the same contract as [`TileContent::holds_focus`]: the shell then
    /// routes bare keys to the input and only chords to the keymap.
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

/// Dispatches a registered shell action on the page's behalf. The shell
/// builds it from its own weak entity; a page never holds `ShellView`.
pub type ShellActions = Rc<dyn Fn(&ActionId, &mut Window, &mut App)>;

pub trait PageFactory {
    fn kind(&self) -> &'static str;
    /// Sidebar tooltip and palette row text, e.g. "Diagnostics".
    fn title(&self) -> &'static str;
    /// Sidebar glyph. Catalog icons outside gpui-component's default set
    /// must be listed in the app's `ExtraIcons`.
    fn icon(&self) -> gpui_kit_assets::IconName;
    /// Runs once, before the keymap builds.
    fn register_actions(&self, registry: &mut ActionRegistry);
    /// The contexts this page's [`PageContent::key_context`] can name. The
    /// fragment checker requires each default binding's predicate to begin
    /// with one of these.
    fn contexts(&self) -> Vec<&'static str> {
        vec![self.kind()]
    }
    /// Default bindings as keymap TOML containing only `[[bindings]]` tables.
    fn default_keymap(&self) -> Option<&'static str> {
        None
    }
    /// A context-free default binding for `page::toggle_<kind>`, such as
    /// `"mod+d"`. The roster emits it as a shell-generated doc; a module
    /// fragment cannot carry a context-free binding.
    fn toggle_binding(&self) -> Option<&'static str> {
        None
    }
    /// Build the page, optionally restoring its `[pages.<kind>]` state. The
    /// occupant must assume it is hidden until [`PageContent::set_visible`].
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

/// Every trait method is forwarded, defaulted ones included, for the same
/// reason the `ModuleFactory` forwarder does: a forwarder inheriting a
/// default answers for itself, not the factory it wraps.
impl<F: PageFactory + ?Sized> PageFactory for Rc<F> {
    fn kind(&self) -> &'static str {
        (**self).kind()
    }
    fn title(&self) -> &'static str {
        (**self).title()
    }
    fn icon(&self) -> gpui_kit_assets::IconName {
        (**self).icon()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        (**self).register_actions(registry)
    }
    fn contexts(&self) -> Vec<&'static str> {
        (**self).contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        (**self).default_keymap()
    }
    fn toggle_binding(&self) -> Option<&'static str> {
        (**self).toggle_binding()
    }
    fn create(
        &self,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        actions: ShellActions,
        window: &mut Window,
        cx: &mut App,
    ) -> PageOccupant {
        (**self).create(restored, frame, diagnostics, actions, window, cx)
    }
}

/// What the sidebar needs to paint one page button.
#[derive(Debug, Clone, Copy)]
pub struct PageEntry {
    pub kind: &'static str,
    pub title: &'static str,
    pub icon: gpui_kit_assets::IconName,
}

/// The app's registered page factories, in sidebar order.
#[derive(Default)]
pub struct PageRoster {
    factories: Vec<Box<dyn PageFactory>>,
}

impl PageRoster {
    pub fn new() -> PageRoster {
        PageRoster::default()
    }

    pub fn add(&mut self, factory: Box<dyn PageFactory>) {
        self.factories.push(factory);
    }

    pub fn factory(&self, kind: &str) -> Option<&dyn PageFactory> {
        self.factories
            .iter()
            .find(|f| f.kind() == kind)
            .map(|f| f.as_ref())
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.factories.iter().map(|f| f.kind()).collect()
    }

    pub fn entries(&self) -> impl Iterator<Item = PageEntry> + '_ {
        self.factories.iter().map(|f| PageEntry {
            kind: f.kind(),
            title: f.title(),
            icon: f.icon(),
        })
    }

    pub fn register_actions(&self, registry: &mut ActionRegistry) {
        for f in &self.factories {
            f.register_actions(registry);
        }
    }

    /// Each factory's default keymap checked against its own contexts (the
    /// pairing happens here, as in `ModuleRoster::keymap_fragments`), plus
    /// one shell-generated, unchecked doc per `toggle_binding` named
    /// `<page:kind>` — the shell wrote it, so the module checker's context
    /// rule does not apply.
    pub fn keymap_fragments(&self) -> (Vec<LayerDoc>, Vec<Diagnostic>) {
        let mut docs = Vec::new();
        let mut diags = Vec::new();
        for factory in &self.factories {
            if let Some(text) = factory.default_keymap() {
                match fragments::fragment_doc(factory.kind(), text) {
                    Ok(doc) => {
                        let (doc, d) = fragments::check_fragment(doc, &factory.contexts());
                        docs.push(doc);
                        diags.extend(d);
                    }
                    Err(d) => diags.push(d),
                }
            }
            if let Some(key) = factory.toggle_binding() {
                let text = format!(
                    "[[bindings]]\n[bindings.keys]\n{key:?} = \"page::toggle_{}\"\n",
                    factory.kind()
                );
                match fragments::fragment_doc(&format!("page:{}", factory.kind()), &text) {
                    Ok(doc) => docs.push(doc),
                    Err(d) => diags.push(d),
                }
            }
        }
        (docs, diags)
    }
}
```

`{key:?}` on a `&str` prints it double-quoted and escaped, which is valid TOML for a key. `LayerDoc` and `Diagnostic` are already imported at the top of `module.rs`.

- [ ] **Step 4: Add the recording page factory inside `pub mod recording`**

Append inside `pub mod recording { ... }` (the existing `#[cfg(any(test, feature = "test-support"))]` module):

```rust
    /// What a recording page saw, in order.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PageRecorded {
        Created,
        Visible(bool),
        Action(String),
    }

    struct RecordingPageView {
        focus_handle: gpui::FocusHandle,
        kind: &'static str,
        input: Entity<gpui_component::input::InputState>,
    }

    impl gpui::Render for RecordingPageView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            use gpui::prelude::*;
            let kind = self.kind;
            gpui::div()
                .size_full()
                .track_focus(&self.focus_handle)
                .debug_selector(move || format!("page-{kind}"))
                .child(gpui_component::input::Input::new(&self.input))
        }
    }

    struct RecordingPageContent {
        view: Entity<RecordingPageView>,
        log: Rc<RefCell<Vec<PageRecorded>>>,
        consume_close: Rc<Cell<bool>>,
    }

    impl PageContent for RecordingPageContent {
        fn key_context(&self, cx: &App) -> KeyContext {
            let kind = self.view.read(cx).kind;
            KeyContext::new(kind)
        }
        fn dispatch(&self, action: &ActionId, _count: Option<u32>, _window: &mut Window, _cx: &mut App) -> bool {
            self.log.borrow_mut().push(PageRecorded::Action(action.0.clone()));
            action.0 == "page::close" && self.consume_close.replace(false)
        }
        fn set_visible(&self, visible: bool, _cx: &mut App) {
            self.log.borrow_mut().push(PageRecorded::Visible(visible));
        }
        fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
            self.view.read(cx).focus_handle.clone()
        }
        fn holds_focus(&self, window: &Window, cx: &App) -> bool {
            self.view.read(cx).input.read(cx).focus_handle(cx).is_focused(window)
        }
        fn title(&self, cx: &App) -> SharedString {
            SharedString::from(self.view.read(cx).kind)
        }
        fn serialize(&self, _cx: &App) -> toml::Table {
            let mut t = toml::Table::new();
            t.insert("recorded".into(), toml::Value::Boolean(true));
            t
        }
    }

    /// A page factory for shell tests: records lifecycle and actions, paints
    /// one real `Input` so `holds_focus` can be exercised, and can be told to
    /// consume the next `page::close` (standing in for a page with a
    /// dismissable surface of its own).
    pub struct RecordingPageFactory {
        kind: &'static str,
        title: &'static str,
        log: Rc<RefCell<Vec<PageRecorded>>>,
        consume_close: Rc<Cell<bool>>,
        toggle_binding: Option<&'static str>,
        created_input: Rc<RefCell<Option<Entity<gpui_component::input::InputState>>>>,
    }

    impl RecordingPageFactory {
        pub fn new(kind: &'static str) -> RecordingPageFactory {
            RecordingPageFactory {
                kind,
                title: Box::leak(super::super::defaults::capitalize(kind).into_boxed_str()),
                log: Rc::new(RefCell::new(Vec::new())),
                consume_close: Rc::new(Cell::new(false)),
                toggle_binding: Some("mod+d"),
                created_input: Rc::new(RefCell::new(None)),
            }
        }
        pub fn without_toggle_binding(mut self) -> Self {
            self.toggle_binding = None;
            self
        }
        pub fn log(&self) -> Rc<RefCell<Vec<PageRecorded>>> {
            self.log.clone()
        }
        pub fn consume_next_close(&self) -> Rc<Cell<bool>> {
            self.consume_close.clone()
        }
        /// The input the created page paints, once created.
        pub fn input(&self) -> Rc<RefCell<Option<Entity<gpui_component::input::InputState>>>> {
            self.created_input.clone()
        }
    }

    impl PageFactory for RecordingPageFactory {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn title(&self) -> &'static str {
            self.title
        }
        fn icon(&self) -> gpui_kit_assets::IconName {
            gpui_kit_assets::IconName::Activity
        }
        fn register_actions(&self, registry: &mut ActionRegistry) {
            let _ = registry.register(crate::actions::ActionDef {
                id: ActionId(format!("{}::noop", self.kind)),
                title: "Recording page no-op".to_string(),
                category: "Test".to_string(),
            });
        }
        fn default_keymap(&self) -> Option<&'static str> {
            // Leaked once per factory: the fragment text must be 'static.
            Some(Box::leak(
                format!(
                    "[[bindings]]\ncontext = {:?}\n[bindings.keys]\n\"n\" = \"{}::noop\"\n",
                    self.kind, self.kind
                )
                .into_boxed_str(),
            ))
        }
        fn toggle_binding(&self) -> Option<&'static str> {
            self.toggle_binding
        }
        fn create(
            &self,
            _restored: Option<&toml::Table>,
            _frame: Entity<Frame>,
            _diagnostics: Entity<Diagnostics>,
            _actions: ShellActions,
            window: &mut Window,
            cx: &mut App,
        ) -> PageOccupant {
            self.log.borrow_mut().push(PageRecorded::Created);
            let kind = self.kind;
            let input = cx.new(|cx| gpui_component::input::InputState::new(window, cx));
            *self.created_input.borrow_mut() = Some(input.clone());
            let view = cx.new(|cx| RecordingPageView {
                focus_handle: cx.focus_handle(),
                kind,
                input,
            });
            PageOccupant {
                kind,
                view: view.clone().into(),
                content: Box::new(RecordingPageContent {
                    view,
                    log: self.log.clone(),
                    consume_close: self.consume_close.clone(),
                }),
            }
        }
    }
```

`capitalize` in `defaults.rs` is private today; make it `pub(crate) fn capitalize(s: &str) -> String`. Check the existing `use` lines at the top of `mod recording` and add `use std::cell::{Cell, RefCell};` if absent.

- [ ] **Step 5: Register `page::close`, its binding, and `register_page_actions` in `defaults.rs`**

In `BUILTIN_KEYMAP`, after the `context = "tile"` table:

```toml
[[bindings]]
context = "page"
[bindings.keys]
"escape" = "page::close"
```

In `register_builtin_actions`, beside the workspace actions:

```rust
    action(reg, "page::close", "Close page", "Workspace");
```

After `register_add_actions`:

```rust
/// Register one toggle per page kind, mirroring `register_add_actions`:
/// `page::toggle_<kind>` titled "<Title>: Open page" in category `<Title>`.
pub fn register_page_actions(reg: &mut ActionRegistry, pages: &[(&str, &str)]) {
    for (kind, title) in pages {
        action(
            reg,
            &format!("page::toggle_{kind}"),
            &format!("{title}: Open page"),
            title,
        );
    }
}
```

Add a test beside the `register_add_actions` test (~line 765):

```rust
    #[test]
    fn page_actions_register_one_toggle_per_kind() {
        let mut reg = ActionRegistry::default();
        register_page_actions(&mut reg, &[("diagnostics", "Diagnostics")]);
        let def = reg
            .get(&ActionId("page::toggle_diagnostics".into()))
            .expect("registered");
        assert_eq!(def.title, "Diagnostics: Open page");
        assert_eq!(def.category, "Diagnostics");
    }
```

If `ActionRegistry` has no `get`, use whichever lookup the existing `expect("tile::add_diagnostics", ...)` helper in that test module uses.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-shell --features test-support page_roster_emits page_actions_register`
Expected: PASS.

- [ ] **Step 7: Gates and commit**

Run: `cargo fmt --check && cargo clippy -p geode-shell --all-targets --features test-support -- -D warnings`

```bash
git add crates/geode-shell/src/module.rs crates/geode-shell/src/defaults.rs
git commit -m "feat(shell): page seam: PageContent, PageFactory, PageRoster, page::close

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `ShellView` opens, closes, and routes a page

**Files:**
- Create: `crates/geode-shell/src/shell/page.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (`ShellServices` ~line 83; `ShellView` fields ~line 374; `new` ~line 715; `on_diagnostics_changed` ~line 1444)
- Modify: `crates/geode-shell/src/shell/input.rs` (`context_stack` ~line 57; `dispatch` ~line 106; `perf::toggle_overlay` arm ~line 262)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (`occupant_insert_stack` ~line 495)
- Modify: `crates/geode-shell/src/diagnostics.rs` (`DIAGNOSTICS_PAGE_KIND`, `overlay_visible`)
- Modify: `crates/geode-shell/src/shell/tests/mod.rs` (`services_with_recorders` literal ~line 225; new `services_with_page`)
- Modify: `crates/geode-app/src/main.rs` (`ShellServices` literal ~line 551)
- Create: `crates/geode-shell/src/shell/tests/pages.rs`; add `mod pages;` to `tests/mod.rs`

**Interfaces:**
- Consumes: Task 1's seam.
- Produces:

```rust
// shell/page.rs
pub(super) struct OpenPage { pub(super) occupant: PageOccupant, pub(super) open: bool }
impl ShellView {
    pub(crate) fn page_open(&self) -> bool;
    pub(crate) fn open_page_kind(&self) -> Option<&'static str>;
    pub(super) fn open_page(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>);
    pub(super) fn close_page(&mut self, window: &mut Window, cx: &mut Context<Self>);
    pub(super) fn toggle_page(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>);
    pub(super) fn shell_actions(&self, cx: &Context<Self>) -> ShellActions;
}
// ShellServices gains:
pub pages: PageRoster,
pub restored_pages: BTreeMap<String, toml::Table>,
// diagnostics.rs gains:
pub const DIAGNOSTICS_PAGE_KIND: &str = "diagnostics";
impl Diagnostics { pub fn overlay_visible(&self) -> bool; pub fn set_overlay_visible(&mut self, visible: bool) -> bool; }
// ShellView gains:
pub(super) fn set_perf_overlay(&mut self, visible: bool, cx: &mut Context<Self>);
```

- [ ] **Step 1: Write the failing shell tests**

Create `crates/geode-shell/src/shell/tests/pages.rs`:

```rust
//! The page seam from the shell's side: toggle, close, context stack,
//! visibility announcements, and the insert-mode route for page inputs.

use super::{dispatch_action, open_shell, services_with_page, shell_of};
use crate::module::recording::{PageRecorded, RecordingPageFactory};

#[gpui::test]
fn toggle_opens_then_closes_the_page_and_announces_visibility(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));

    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert!(shell.read_with(&cx, |s, _| s.page_open()));
    assert_eq!(shell.read_with(&cx, |s, _| s.open_page_kind()), Some("diagnostics"));
    assert_eq!(
        *log.borrow(),
        vec![PageRecorded::Created, PageRecorded::Visible(true)]
    );
    // The page view holds focus after open.
    assert!(cx.update(|window, cx| {
        let s = shell.read(cx);
        s.page.as_ref().unwrap().occupant.content.focus_handle(cx).is_focused(window)
    }));

    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
    assert_eq!(log.borrow().last(), Some(&PageRecorded::Visible(false)));
    assert!(cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)));
    // Retained: a second open does not create again.
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert_eq!(log.borrow().iter().filter(|r| **r == PageRecorded::Created).count(), 1);
}

#[gpui::test]
fn the_context_stack_is_page_then_kind_while_open(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    shell.read_with(&cx, |s, cx| {
        let stack = s.context_stack(cx);
        assert_eq!(stack.len(), 2, "{stack:?}");
        assert!(stack[0].has_flag("page"));
        assert!(stack[1].has_flag("diagnostics"));
        assert!(!stack.iter().any(|c| c.has_flag("workspace") || c.has_flag("tile")));
    });
}

#[gpui::test]
fn escape_closes_the_page_through_page_close(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("alt-d");
    assert!(shell.read_with(&cx, |s, _| s.page_open()), "mod+d opens via the toggle fragment");
    cx.simulate_keystrokes("escape");
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
    assert!(log.borrow().contains(&PageRecorded::Action("page::close".into())),
        "the page saw page::close before the shell acted");
}

#[gpui::test]
fn a_page_that_consumes_close_stays_open(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let consume = factory.consume_next_close();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    consume.set(true);
    cx.simulate_keystrokes("escape");
    assert!(shell.read_with(&cx, |s, _| s.page_open()), "consumed: still open");
    cx.simulate_keystrokes("escape");
    assert!(!shell.read_with(&cx, |s, _| s.page_open()), "second escape closes");
}

#[gpui::test]
fn toggle_under_a_modal_is_refused(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "settings::open", &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(s.modal_open());
        assert!(!s.page_open());
        assert!(s.notice.is_some(), "refused with a notice");
    });
}

#[gpui::test]
fn escape_under_a_modal_closes_the_modal_not_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("ctrl-,");
    assert!(shell.read_with(&cx, |s, _| s.modal_open()));
    cx.simulate_keystrokes("escape");
    shell.read_with(&cx, |s, _| {
        assert!(!s.modal_open());
        assert!(s.page_open(), "the modal took the escape");
    });
}

#[gpui::test]
fn a_workspace_switch_closes_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("alt-2");
    shell.read_with(&cx, |s, _| {
        assert!(!s.page_open());
        assert_eq!(s.services.workspaces.active_index(), 2);
    });
}

#[gpui::test]
fn a_workspace_switch_chord_from_a_focused_page_input_closes_the_page(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let input = factory.input();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    let input = input.borrow().clone().expect("created");
    cx.update(|window, cx| input.update(cx, |i, cx| i.focus(window, cx)));
    // A bare key types into the input rather than reaching the keymap.
    cx.simulate_keystrokes("j");
    assert_eq!(cx.update(|_, cx| input.read(cx).value().to_string()), "j");
    cx.simulate_keystrokes("alt-3");
    shell.read_with(&cx, |s, _| {
        assert!(!s.page_open(), "the chord resolved against the whole stack");
        assert_eq!(s.services.workspaces.active_index(), 3);
    });
}

#[gpui::test]
fn tile_bindings_are_inert_while_a_page_is_open(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("alt-n");
    shell.read_with(&cx, |s, _| {
        assert!(!s.modal_open(), "tile::add's picker did not open: no workspace context");
        assert!(s.page_open());
    });
}

#[gpui::test]
fn the_overlay_mirror_follows_a_keyboard_toggle(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    assert!(!diagnostics.read_with(&cx, |d, _| d.overlay_visible()));
    dispatch_action(&shell, "perf::toggle_overlay", &mut cx);
    assert!(diagnostics.read_with(&cx, |d, _| d.overlay_visible()));
    // The entity channel toggles it back and the mirror follows.
    diagnostics.update(&mut cx, |d, cx| {
        d.request_overlay_toggle();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(!diagnostics.read_with(&cx, |d, _| d.overlay_visible()));
    assert!(!shell.read_with(&cx, |s, _| s.perf_overlay));
}
```

`KeyContext::has_flag` is the accessor (`keymap/context.rs:29`). The test fixture's `mod` alias is Alt (`defaults::default_mod`), so chords are spelled `alt-…` in `simulate_keystrokes`.

Add `services_with_page` to `tests/mod.rs`, after `services_with_recorders`:

```rust
/// A shell with one registered page and no modules.
pub(super) fn services_with_page(page: RecordingPageFactory) -> ShellServices {
    with_page(services_with_recorders(Vec::new()), page)
}
```

with `with_page` as defined in Task 4 (write both now; Task 4's tests are the first to pass a module roster through it):

```rust
pub(super) fn with_page(mut services: ShellServices, page: RecordingPageFactory) -> ShellServices {
    use crate::module::PageFactory as _;
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    register_pick_actions(&mut registry, &crate::shell::pickable_columns(&services.config));
    register_scope_actions(&mut registry, &crate::shell::saved_scopes(&services.config, false));
    crate::defaults::register_add_actions(&mut registry, &services.roster.kinds());
    services.roster.register_actions(&mut registry);
    crate::defaults::register_page_actions(&mut registry, &[(page.kind(), page.title())]);
    let mut pages = crate::module::PageRoster::new();
    pages.add(Box::new(page));
    pages.register_actions(&mut registry);
    let (mut fragments, mut diags) = services.roster.keymap_fragments();
    let (page_fragments, page_diags) = pages.keymap_fragments();
    fragments.extend(page_fragments);
    diags.extend(page_diags);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = test_keymap_with_fragments(&registry, &fragments, &[]);
    services.registry = registry;
    services.keymap_fragments = fragments;
    services.pages = pages;
    services
}
```

Add `mod pages;` to the test module list in `tests/mod.rs`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support pages::`
Expected: FAIL to compile (`pages` field, `page_open`, `services_with_page`).

- [ ] **Step 3: Add the services fields and the diagnostics mirror**

In `ShellServices` (`mod.rs`):

```rust
    /// The app's registered pages, in sidebar order. Empty in tests that
    /// build no page.
    pub pages: PageRoster,
    /// `[pages.<kind>]` tables from the loaded session, consumed by the
    /// page's first open. Unmatched tables are carried to the next save.
    pub restored_pages: std::collections::BTreeMap<String, toml::Table>,
```

Add both to the literal in `tests/mod.rs` (`pages: crate::module::PageRoster::new(), restored_pages: std::collections::BTreeMap::new(),`) and in `main.rs` (`pages: PageRoster::new(), restored_pages: std::collections::BTreeMap::new(),`; import `geode_shell::module::PageRoster`). Grep for every other `ShellServices {` literal (`rg "ShellServices \{" crates`) and add the two fields there too.

In `diagnostics.rs`, near `CatalogRequest`:

```rust
/// The kind under which the diagnostics page registers. The shell's status
/// bar summary click dispatches `page::toggle_<this>`; the feature crate's
/// factory returns it from `kind()`.
pub const DIAGNOSTICS_PAGE_KIND: &str = "diagnostics";
```

Add a private field `overlay_visible: bool` (initialise `false` in `new`) and, beside `request_overlay_toggle`:

```rust
    /// Whether the performance overlay is showing, mirrored here by the
    /// shell on every toggle so a page can paint a controlled switch. Bumps
    /// the perf counter so a perf-section observer repaints. Returns whether
    /// the value changed.
    pub fn set_overlay_visible(&mut self, visible: bool) -> bool {
        if self.overlay_visible == visible {
            return false;
        }
        self.overlay_visible = visible;
        self.version += 1;
        self.versions.perf += 1;
        true
    }

    pub fn overlay_visible(&self) -> bool {
        self.overlay_visible
    }
```

In `mod.rs` add to `ShellView`:

```rust
    /// Set the overlay and mirror it into `Diagnostics` in one place. Both
    /// the keyboard action and the entity's toggle channel come through here
    /// so the page's switch and the readout can never disagree.
    pub(super) fn set_perf_overlay(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.perf_overlay = visible;
        self.diagnostics.update(cx, |d, cx| {
            if d.set_overlay_visible(visible) {
                cx.notify();
            }
        });
    }
```

Replace the `perf::toggle_overlay` arm body in `input.rs` with `let next = !self.perf_overlay; self.set_perf_overlay(next, cx); cx.notify();` and the `if pending_overlay { self.perf_overlay = !self.perf_overlay; }` in `on_diagnostics_changed` with `if pending_overlay { let next = !self.perf_overlay; self.set_perf_overlay(next, cx); }`. `set_perf_overlay` updates the entity from inside its own observer callback; GPUI permits nested `update` on an entity whose observer is running because the borrow has already been released by the time the callback runs. If it panics with a re-entrancy error, defer with `cx.defer(move |view, cx| view.set_perf_overlay(next, cx))`.

- [ ] **Step 4: Create `shell/page.rs`**

```rust
//! One page at a time over the workspace. A page is created on first open
//! and retained for the window's lifetime so its state survives a round trip;
//! `open` is the only flag that changes between toggles.

use std::rc::Rc;

use gpui::{App, Context, Window};

use crate::actions::ActionId;
use crate::module::{PageOccupant, ShellActions};
use crate::shell::ShellView;

pub(super) struct OpenPage {
    pub(super) occupant: PageOccupant,
    pub(super) open: bool,
}

impl ShellView {
    pub(crate) fn page_open(&self) -> bool {
        self.page.as_ref().is_some_and(|p| p.open)
    }

    pub(crate) fn open_page_kind(&self) -> Option<&'static str> {
        self.page.as_ref().filter(|p| p.open).map(|p| p.occupant.kind)
    }

    /// The handle a page dispatches registered shell actions through. Built
    /// from the weak entity so a page never holds `ShellView`.
    pub(super) fn shell_actions(&self, cx: &Context<Self>) -> ShellActions {
        let weak = cx.entity().downgrade();
        Rc::new(move |action: &ActionId, window: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |view, cx| {
                view.dispatch(action, None, window, cx);
                cx.notify();
            });
        })
    }

    /// Create on first open, then show. Focus moves to the page. Tiles beneath
    /// are hidden on the next render's `ensure_occupants` pass.
    pub(super) fn open_page(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = &self.page
            && page.occupant.kind != kind
        {
            // One page at a time: a different kind replaces the retained one.
            page.occupant.content.set_visible(false, cx);
            self.page = None;
        }
        if self.page.is_none() {
            let Some(factory) = self.services.pages.factory(kind) else {
                tracing::warn!(target: "geode::shell", "no page registered as '{kind}'");
                return;
            };
            let restored = self.services.restored_pages.remove(kind);
            let actions = self.shell_actions(cx);
            let occupant = factory.create(
                restored.as_ref(),
                self.frame.clone(),
                self.diagnostics.clone(),
                actions,
                window,
                cx,
            );
            self.page = Some(OpenPage { occupant, open: false });
        }
        let page = self.page.as_mut().expect("created above");
        if page.open {
            return;
        }
        page.open = true;
        page.occupant.content.set_visible(true, cx);
        page.occupant.content.focus_handle(cx).focus(window, cx);
        self.session_dirty = true;
        cx.notify();
    }

    pub(super) fn close_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.page.as_mut() else { return };
        if !page.open {
            return;
        }
        page.open = false;
        page.occupant.content.set_visible(false, cx);
        self.focus_handle.focus(window, cx);
        self.session_dirty = true;
        cx.notify();
    }

    pub(super) fn toggle_page(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_page_kind() == Some(kind) {
            self.close_page(window, cx);
        } else {
            self.open_page(kind, window, cx);
        }
    }
}
```

Add `mod page;` to `shell/mod.rs` and the field `page: Option<page::OpenPage>` (initialised `None` in `new`). The borrow in `open_page` reads `self.services.pages.factory(kind)` while later mutating `self.page`; if the borrow checker objects, clone the factory lookup into a local `Option<&dyn PageFactory>` before the `restored` line, or restructure as `let created = { ... }; self.page = Some(created);`.

- [ ] **Step 5: Route dispatch and the context stack in `input.rs`**

In `context_stack`, replace the body with:

```rust
        if let Some(page) = self.page.as_ref().filter(|p| p.open) {
            let mut stack = vec![KeyContext::new("page"), page.occupant.content.key_context(cx)];
            if self.palette.is_some() {
                stack.push(KeyContext::new("palette"));
            }
            return stack;
        }
        // ...existing workspace/tile/palette construction unchanged...
```

In `dispatch`, right after the existing modal guard for `tile::command_line | tile::find | stack::pick`, add:

```rust
        // A page surface cannot host a `:` line, find, or stack list.
        if self.page_open()
            && matches!(action.0.as_str(), "tile::command_line" | "tile::find" | "stack::pick")
        {
            self.notice = Some(CLOSE_PAGE_FIRST);
            return;
        }
        if let Some(kind) = action.0.strip_prefix("page::toggle_") {
            if self.modal_open() {
                self.notice = Some(CLOSE_DIALOG_FIRST);
                return;
            }
            let kind = kind.to_string();
            self.toggle_page(&kind, window, cx);
            return;
        }
        if action.0 == "page::close" {
            if self.page_open() {
                let consumed = self
                    .page
                    .as_ref()
                    .map(|p| p.occupant.content.dispatch(action, count, window, cx))
                    .unwrap_or(false);
                if !consumed {
                    self.close_page(window, cx);
                }
            }
            return;
        }
        // A workspace switch is a route home from any page.
        if self.page_open() && action.0.starts_with("workspace::switch_") {
            self.close_page(window, cx);
        }
```

Define `const CLOSE_PAGE_FIRST: &str = "close the page first (esc)";` beside `CLOSE_DIALOG_FIRST`.

At the end of `dispatch`, replace the occupant fallback with:

```rust
            if let Some(page) = self.page.as_ref().filter(|p| p.open) {
                page.occupant.content.dispatch(action, count, window, cx);
            } else if let Some(tile) = self.services.workspaces.active().focused_tile()
                && let Some(o) = self.occupants.get(&tile)
            {
                o.content.dispatch(action, count, window, cx);
            }
```

`toggle_page` may drop `self.page` while `page` is borrowed in the `page::close` branch; the `map(...)` returns before `close_page` runs, so the borrow ends first.

- [ ] **Step 6: Let the insert branch consult the page**

In `occupants.rs`, `occupant_insert_stack`, before the `let tile = ...` line:

```rust
        if let Some(page) = self.page.as_ref().filter(|p| p.open) {
            if !page.occupant.content.holds_focus(window, cx) {
                return None;
            }
            return Some(self.context_stack(cx));
        }
```

The existing tail of the function filters the stack; keep the page branch returning the full stack so `insert_contexts` can apply its own `mode == insert` filter for bare keys (the recording page's context has no `mode`, so bare keys resolve to nothing and reach the input, which is what the test asserts; the diagnostics page will set `mode = insert`).

- [ ] **Step 7: Run the tests**

Run: `cargo test -p geode-shell --features test-support pages::`
Expected: PASS.

- [ ] **Step 8: Gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support && cargo check -p geode-shell --features test-support --all-targets`

```bash
git add crates/geode-shell crates/geode-app/src/main.rs
git commit -m "feat(shell): open, close, and route one page over the workspace

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Session persistence of `[pages.<kind>]`

**Files:**
- Modify: `crates/geode-shell/src/session.rs` (`Restored` ~line 87; `to_toml` ~line 259; `from_toml` ~line 467; `to_string_pretty` ~894; `save` ~925; `load` ~941; module doc ~line 7)
- Modify: `crates/geode-shell/src/shell/session_io.rs` (`take_dirty_session_write` ~line 33; `save_session` ~70)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`last_pages_written` field)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (add `current_pages`)
- Modify: `crates/geode-app/src/main.rs` (~line 200 restore block)
- Test: `crates/geode-shell/src/session.rs` tests; `crates/geode-shell/src/shell/tests/pages.rs`

**Interfaces:**
- Produces: `pub type PageRecords = BTreeMap<String, toml::Table>;` `Restored.pages: PageRecords`; every `to_toml`/`to_string_pretty`/`save` gains a `pages: &PageRecords` parameter after `palette_usage`; `ShellView::current_pages(&self, cx) -> PageRecords`.

- [ ] **Step 1: Write the failing session round-trip test**

In `session.rs` tests, beside the palette-usage round-trip test:

```rust
    #[test]
    fn pages_round_trip_and_an_unknown_kind_is_kept() {
        let workspaces = Workspaces::new();
        let mut pages = PageRecords::new();
        let mut diag = toml::Table::new();
        diag.insert("section".into(), toml::Value::String("log".into()));
        pages.insert("diagnostics".into(), diag);
        let text = to_string_pretty(&workspaces, &TileRecords::new(), None, &PaletteUsage::new(), &pages).unwrap();
        assert!(text.contains("[pages.diagnostics]"), "{text}");
        let (restored, warnings) = from_toml(&text.parse::<toml::Table>().unwrap()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored.pages, pages);
        // Empty pages are omitted.
        let text = to_string_pretty(&workspaces, &TileRecords::new(), None, &PaletteUsage::new(), &PageRecords::new()).unwrap();
        assert!(!text.contains("[pages"), "{text}");
    }

    #[test]
    fn a_non_table_page_record_warns_and_is_dropped() {
        let text = "config_version = 1\nactive = 1\n[pages]\ndiagnostics = 3\n";
        let (restored, warnings) = from_toml(&text.parse::<toml::Table>().unwrap()).unwrap();
        assert!(restored.pages.is_empty());
        assert!(warnings.iter().any(|w| w.contains("pages.diagnostics")), "{warnings:?}");
    }
```

Match the return shape of `from_toml` to what the existing tests in that module use (it may return `Result<(Restored-like parts, warnings), Vec<String>>`); read the neighbouring palette-usage test and mirror it exactly.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell pages_round_trip a_non_table_page_record`
Expected: FAIL to compile.

- [ ] **Step 3: Thread `pages` through `session.rs`**

Add `pub type PageRecords = BTreeMap<String, toml::Table>;` beside `TileRecords`, `pub pages: PageRecords` to `Restored`, and the `pages: &PageRecords` parameter to `to_toml`, `to_string_pretty`, and `save`. In `to_toml`, after the palette block:

```rust
    // Omit empty page state; an absent table restores as empty. Each page's
    // table is opaque to the shell, like a tile's `state`.
    if !pages.is_empty() {
        let mut table = toml::Table::new();
        for (kind, state) in pages {
            table.insert(kind.clone(), toml::Value::Table(state.clone()));
        }
        root.insert("pages".to_string(), toml::Value::Table(table));
    }
```

In `from_toml`, after the palette block:

```rust
    let mut pages = PageRecords::new();
    match table.get("pages") {
        None => {}
        Some(toml::Value::Table(t)) => {
            for (kind, value) in t {
                match value {
                    toml::Value::Table(state) => {
                        pages.insert(kind.clone(), state.clone());
                    }
                    _ => warnings.push(format!("pages.{kind} is not a table; ignored")),
                }
            }
        }
        Some(_) => warnings.push("pages is not a table; ignored".to_string()),
    }
```

Add `pages` to the `Restored` literal, to `load`'s `fresh` closure (`pages: PageRecords::new()`), and to the module doc's TOML shape list (`[pages.<kind>]`: one opaque table per page kind).

- [ ] **Step 4: Thread it through the shell**

In `occupants.rs`, after `current_tiles`:

```rust
    /// Every created page's state (open or not), plus restored tables no
    /// page has consumed, so an unknown kind survives a save.
    pub(super) fn current_pages(&self, cx: &App) -> session::PageRecords {
        let mut pages: session::PageRecords = self.services.restored_pages.clone();
        if let Some(page) = &self.page {
            pages.insert(page.occupant.kind.to_string(), page.occupant.content.serialize(cx));
        }
        pages
    }
```

In `mod.rs` add `last_pages_written: crate::session::PageRecords` (initialised empty) beside `last_tiles_written`. In `take_dirty_session_write`, compute `let pages = self.current_pages(cx);`, add `&& pages == self.last_pages_written` to the early-return condition, pass `&pages` to `to_string_pretty`, and set `self.last_pages_written = pages;` on success. Pass `&self.current_pages(cx)` in `save_session`. In `main.rs` restore block add `services.restored_pages = restored.pages;`. Fix every other `to_toml`/`to_string_pretty`/`save` call (`rg "session::(to_toml|to_string_pretty|save)\(" crates`).

- [ ] **Step 5: Add the shell-level round trip test to `tests/pages.rs`**

```rust
#[gpui::test]
fn the_session_flush_writes_pages_and_restore_hands_them_to_create(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    // Nothing created yet: no pages table.
    let none = shell.update(&mut cx, |s, cx| {
        s.services.session_path = Some(std::path::PathBuf::from("/nonexistent/session.toml"));
        s.session_dirty = true;
        s.take_dirty_session_write(cx)
    });
    assert!(!none.unwrap().1.contains("[pages"), "no page created, nothing written");
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    let (_, text) = shell.update(&mut cx, |s, cx| s.take_dirty_session_write(cx)).unwrap();
    assert!(text.contains("[pages.diagnostics]") && text.contains("recorded = true"), "{text}");
    // A flush with nothing changed writes nothing.
    assert!(shell.update(&mut cx, |s, cx| s.take_dirty_session_write(cx)).is_none());
}
```

`take_dirty_session_write` is `pub(super)`; the tests module is inside `shell`, so it is reachable. If `session_path` is private, build the fixture through `open_shell_with_user_dir` instead and read the file it writes.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-shell --features test-support session pages::`
Expected: PASS.

- [ ] **Step 7: Gates and commit**

```bash
git add crates/geode-shell crates/geode-app/src/main.rs
git commit -m "feat(shell): persist [pages.<kind>] in the session file

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Render extent, tiles hidden beneath, sidebar buttons, status-bar route

**Files:**
- Modify: `crates/geode-shell/src/shell/render.rs` (focus restore ~line 76; drag cancels ~116-158; strips ~229-235; sizes ~193-211; status click ~772-785; sidebar ~819; composition ~969-1000)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (`fill_active_tiles` ~120, `visible_tile_keys` ~135)
- Modify: `crates/geode-shell/src/shell/sidebar.rs`
- Modify: `crates/geode-shell/src/shell/status.rs` (~line 190 tooltip)
- Modify: `crates/geode-shell/src/shell/tests/diagnostics.rs` (tests at lines 73, 109, 223)
- Test: `crates/geode-shell/src/shell/tests/pages.rs`

**Interfaces:**
- Produces: `sidebar::sidebar(active: u8, non_empty: &[u8], pages: &[PageEntry], open_page: Option<&str>, cx)`; debug selectors `sidebar-page-<kind>` and `shell-page`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/pages.rs`:

```rust
#[gpui::test]
fn the_page_paints_where_the_workspace_was_and_the_toolbar_is_gone(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| { let _ = window.draw(cx); });
    assert!(cx.debug_bounds("shell-page").is_none());
    assert!(cx.debug_bounds("shell-sidebar").is_some());
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.update(|window, cx| { let _ = window.draw(cx); });
    let page = cx.debug_bounds("shell-page").expect("page painted");
    let sidebar = cx.debug_bounds("shell-sidebar").expect("sidebar stays");
    let status = cx.debug_bounds("shell-status-bar").expect("status bar stays");
    assert_eq!(page.origin.x, sidebar.origin.x + sidebar.size.width);
    assert_eq!(page.origin.y, gpui::px(0.));
    assert_eq!(page.origin.y + page.size.height, status.origin.y, "the page reaches the status bar; no toolbar above it since it starts at y = 0");
}

#[gpui::test]
fn tiles_beneath_are_hidden_on_open_and_shown_on_close(cx: &mut gpui::TestAppContext) {
    use crate::module::recording::{Recorded, RecordingFactory};
    let recorder = RecordingFactory::new("blotter");
    let tile_log = recorder.log();
    let mut services = super::services_with_recorders(vec![recorder]);
    // Splice the page in beside the module.
    let page = RecordingPageFactory::new("diagnostics");
    services = super::with_page(services, page);
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "tile::add_blotter", &mut cx);
    cx.update(|window, cx| { let _ = window.draw(cx); });
    assert!(tile_log.borrow().iter().any(|r| matches!(r, Recorded::Visible(true))));
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.update(|window, cx| { let _ = window.draw(cx); });
    assert!(matches!(tile_log.borrow().last(), Some(Recorded::Visible(false))), "hidden beneath the page");
    let keys = shell.read_with(&cx, |s, _| { let mut v = Vec::new(); s.visible_tile_keys(&mut v); v });
    assert!(keys.is_empty(), "no visible tile keys while a page is open");
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.update(|window, cx| { let _ = window.draw(cx); });
    assert!(matches!(tile_log.borrow().last(), Some(Recorded::Visible(true))));
}

#[gpui::test]
fn the_sidebar_button_toggles_the_page_and_shows_it_active(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_page(RecordingPageFactory::new("diagnostics")));
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| { let _ = window.draw(cx); });
    let button = cx.debug_bounds("sidebar-page-diagnostics").expect("one button per page");
    let profile = cx.debug_bounds("sidebar-profile").unwrap();
    assert!(button.origin.y < profile.origin.y, "above the settings avatar");
    cx.simulate_mouse_down(button.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_up(button.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.update(|window, cx| { let _ = window.draw(cx); });
    assert!(shell.read_with(&cx, |s, _| s.page_open()));
    assert!(cx.debug_bounds("sidebar-page-diagnostics-active").is_some(), "active treatment");
    // A mouse-opened page must then receive keys: escape closes it.
    cx.simulate_keystrokes("escape");
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
}
```

Add `with_page` to `tests/mod.rs` and refactor `services_with_page(page)` into `with_page(services_with_recorders(Vec::new()), page)`:

```rust
/// Add one page to already-built services, keeping the module roster's
/// actions and fragments: the registry is rebuilt in `main.rs` order
/// (builtins, picks, scopes, add actions for the roster's kinds, module
/// actions, page actions, page-owned actions) and the keymap from module
/// fragments followed by page fragments.
pub(super) fn with_page(mut services: ShellServices, page: RecordingPageFactory) -> ShellServices {
    use crate::module::PageFactory as _;
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    register_pick_actions(&mut registry, &crate::shell::pickable_columns(&services.config));
    register_scope_actions(&mut registry, &crate::shell::saved_scopes(&services.config, false));
    crate::defaults::register_add_actions(&mut registry, &services.roster.kinds());
    services.roster.register_actions(&mut registry);
    crate::defaults::register_page_actions(&mut registry, &[(page.kind(), page.title())]);
    let mut pages = crate::module::PageRoster::new();
    pages.add(Box::new(page));
    pages.register_actions(&mut registry);
    let (mut fragments, mut diags) = services.roster.keymap_fragments();
    let (page_fragments, page_diags) = pages.keymap_fragments();
    fragments.extend(page_fragments);
    diags.extend(page_diags);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = test_keymap_with_fragments(&registry, &fragments, &[]);
    services.registry = registry;
    services.keymap_fragments = fragments;
    services.pages = pages;
    services
}
```

`shell-status-bar` and `shell-sidebar` are the existing selectors.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support pages::the_page_paints pages::tiles_beneath pages::the_sidebar_button`
Expected: FAIL (`shell-page` not painted, `sidebar-page-diagnostics` absent).

- [ ] **Step 3: Hide tiles beneath**

In `occupants.rs`, at the top of `fill_active_tiles` and `visible_tile_keys`, after `out.clear()`:

```rust
        // A page covers the workspace: nothing beneath is visible, so no tile
        // is announced shown and no flip barrier waits on one.
        if self.page_open() {
            return;
        }
```

- [ ] **Step 4: Sidebar page buttons**

Change `sidebar`'s signature to `pub fn sidebar(active: u8, non_empty: &[u8], pages: &[PageEntry], open_page: Option<&str>, cx: &Context<ShellView>) -> impl IntoElement` (import `crate::module::PageEntry`). Between `indicators` and `profile`, build:

```rust
    let mut bottom = v_flex().w_full().items_center().gap_2().pb_2();
    for entry in pages {
        let kind = entry.kind;
        let is_open = open_page == Some(kind);
        let selector: SharedString = format!("sidebar-page-{kind}").into();
        let tip_selector: SharedString = format!("tip-sidebar-page-{kind}").into();
        let action_id: SharedString = format!("page::toggle_{kind}").into();
        bottom = bottom.child(
            div()
                .id(gpui::ElementId::Name(selector.clone()))
                .debug_selector({
                    let s = selector.clone();
                    move || s.to_string()
                })
                .tooltip(crate::tips::tip_with(
                    tip_selector,
                    SharedString::from(entry.title),
                    Some(Box::leak(action_id.to_string().into_boxed_str())),
                    None,
                ))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _event, window, cx| {
                        view.dispatch(&ActionId(format!("page::toggle_{kind}")), None, window, cx);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id(gpui::ElementId::Name(format!("sidebar-page-{kind}-box").into()))
                        .when(is_open, |d| {
                            d.debug_selector(move || format!("sidebar-page-{kind}-active"))
                        })
                        .size(scale::design(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius)
                        // Open = the selected tab, same treatment as the active
                        // workspace disc and, like it, no pointer states.
                        .when(is_open, |d| {
                            d.bg(theme.sidebar_primary.opacity(0.2)).text_color(theme.sidebar_primary)
                        })
                        .when(!is_open, |d| d.pointer_states(gear_states).text_color(theme.sidebar_foreground))
                        .child(gpui_component::Icon::new(entry.icon).small()),
                ),
        );
    }
    let bottom = bottom.child(profile_box);
```

where `profile_box` is the existing profile element with its `.pb_2()` removed (the column carries the padding). Read `tips::tip_with`'s exact parameter types (line 107 of `tips.rs`: `selector: SharedString, title: SharedString, action: Option<&'static str>, detail: Option<SharedString>`) and `Icon::new`'s accepted type (`toolbar.rs` line 32 passes a `gpui_kit_assets::IconName`, so it accepts the catalog enum). The `Box::leak` per render for the action id is an allocation per render; avoid it by giving `PageEntry` a `toggle_action: &'static str` leaked once in `PageRoster::entries` (cache in the roster as `Vec<&'static str>` built in `add`). Do that instead of leaking in render.

In `render.rs`, replace the sidebar call with:

```rust
        let page_entries: Vec<crate::module::PageEntry> = self.services.pages.entries().collect();
        let sidebar = sidebar::sidebar(active_index, &non_empty, &page_entries, self.open_page_kind(), cx);
```

Collecting per render allocates; instead store `page_entries: Vec<PageEntry>` on `ShellView` at construction (the roster never changes after startup) and pass `&self.page_entries`.

- [ ] **Step 5: Paint the page instead of the workspace**

In `render.rs`, immediately after computing `content_height`, `tile_width`, and friends:

```rust
        let page_view = self
            .page
            .as_ref()
            .filter(|p| p.open)
            .map(|p| p.occupant.view.clone());
```

Guard everything workspace-only on `page_view.is_none()`:
- the divider/tile drag cancellation conditions (add `|| self.page_open()` beside `self.modal_open()`);
- `interactive` for divider strips (add `&& !self.page_open()`);
- the focus-restore block: when `self.pending_focus_restore` and a page is open, focus the page's handle instead of the root; and the `window.focused(cx).is_none()` fallback likewise;
- the command line, stack list, tile drag ghost: they cannot be armed while the page is open (dispatch refuses them), so no change.

Build the body as:

```rust
        let body = match page_view {
            Some(view) => h_flex()
                .w_full()
                .h(px(content_height + toolbar_height + stripe_height))
                .flex_none()
                .child(sidebar)
                .child(
                    div()
                        .id("shell-page")
                        .debug_selector(|| "shell-page".to_string())
                        .w(px(tile_width))
                        .h(px(content_height + toolbar_height + stripe_height))
                        .flex_none()
                        .overflow_hidden()
                        .bg(cx.theme().background)
                        .child(view),
                ),
            None => h_flex()
                .w_full()
                .h(px(content_height))
                .flex_none()
                .child(sidebar)
                .child(surface),
        };
```

and in the root `v_flex`, paint `toolbar` and the as-of stripe only when `!page_open` (`.when(!page_open, |el| el.child(toolbar))`, and fold the stripe's `when(is_historical, ...)` into `when(is_historical && !page_open, ...)`). `toolbar` is built before this point; wrap its construction in `if page_open { None } else { Some(toolbar::toolbar(...)) }` so no toolbar work happens while the page is open, and use `when_some`.

- [ ] **Step 6: Status bar route and copy**

In `render.rs` replace the click closure body with `view.dispatch(&crate::actions::ActionId(format!("page::toggle_{}", crate::diagnostics::DIAGNOSTICS_PAGE_KIND)), None, window, cx); cx.notify();`. In `status.rs` change the tooltip title to `"Open the diagnostics page"`. The stopped-segment click at the same site (`tests/diagnostics.rs:223`) routes through the same closure; keep them together.

Rewrite the three tests in `tests/diagnostics.rs`:
- `hovering_the_diagnostics_summary_says_it_opens_the_tile` → `..._page`, asserting the new title text.
- `clicking_the_diagnostics_summary_opens_a_tile` → `clicking_the_diagnostics_summary_opens_the_page`: build services with `with_page(services, RecordingPageFactory::new("diagnostics"))`, click, assert `page_open()` and `open_page_kind() == Some("diagnostics")`, and that the tree is still empty.
- `clicking_the_stopped_segment_opens_the_diagnostics_tile` → the same assertions.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p geode-shell --features test-support pages:: diagnostics::`
Expected: PASS. Also run the sidebar hover tests: `cargo test -p geode-shell --features test-support hovering_`.

- [ ] **Step 8: Gates and commit**

```bash
git add crates/geode-shell
git commit -m "feat(shell): paint a page over the workspace; sidebar page buttons

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: Pure models for the diagnostics page: sections, typed rows, badges, log tail

**Files:**
- Create: `crates/geode-diagnostics/src/section.rs`
- Create: `crates/geode-diagnostics/src/model.rs`
- Create: `crates/geode-diagnostics/src/log.rs`
- Modify: `crates/geode-diagnostics/src/lib.rs` (add `pub mod section; pub mod model; pub mod log;` — the old `commands`, `sections`, `tile` modules stay until Task 7)
- Test: inline `#[cfg(test)]` modules in each new file

**Interfaces:**
- Consumes: `geode_shell::diagnostics::{Diagnostics, Health, SourceShape, SourceState, DatasetState}`, `geode_core::log::{Record, Ring, Level}`, `geode_core::query::{AsOf, CatalogSnapshot}`, `geode_core::config::{Config, Diagnostic, Severity}`, `geode_shell::perf::{RequeryStats, FrameHistogram, format_ms}`, `geode_core::clock::Clock`.
- Produces:

```rust
// section.rs
pub enum Section { Sources, Data, Config, Log, Perf }
impl Section { pub const ALL: [Section; 5]; pub fn name(self) -> &'static str; pub fn title(self) -> &'static str; pub fn from_name(s: &str) -> Option<Section>; pub fn next(self) -> Section; pub fn prev(self) -> Section; }

// model.rs
pub enum Tone { Normal, Muted, Warn, Error, Marked }
pub struct SourceRow { pub name: String, pub tone: Tone, pub health: String, pub since: Option<SystemTime>, pub since_hms: String, pub shape: String, pub last_poll: String, pub next_poll: String, pub ready: String, pub loading: String, pub detail: Vec<String>, pub history: Vec<(String, Health)> }
pub fn source_rows(d: &Diagnostics, clock: Clock) -> Vec<SourceRow>;
pub fn age_text(since: Option<SystemTime>, now: SystemTime) -> String;
pub struct DatasetRow { pub name: String, pub live_rows: String, pub archive_rows: String, pub partitions: usize, pub latest_gen: String, pub published: String, pub resolved: String, pub has_catalog: bool, pub children: Vec<PartitionRow> }
pub struct PartitionRow { pub label: String, pub gen_id: String, pub source_time: String, pub loaded: String, pub rows: String, pub kind: &'static str, pub marked: bool }
pub fn dataset_rows(d: &Diagnostics, as_of: &AsOf, clock: Clock) -> Vec<DatasetRow>;
pub fn catalog_matches_frame(d: &Diagnostics, as_of: &AsOf) -> bool;
pub enum Lane { Config, Data }
pub struct DiagnosticRow { pub severity: Severity, pub lane: Lane, pub batch: Option<String>, pub location: String, pub message: String, pub full: String }
pub fn current_diagnostics(d: &Diagnostics) -> Vec<DiagnosticRow>;
pub fn history_diagnostics(d: &Diagnostics, clock: Clock) -> Vec<DiagnosticRow>;
pub struct ConfigDoc { pub name: String, pub leaves: Vec<ConfigLeaf>, pub omitted: usize }
pub struct ConfigLeaf { pub key: String, pub value: String, pub layer: String }
pub const MAX_LEAVES_PER_DOC: usize = 2_000;
pub fn config_docs(config: &Config, filter: &str) -> Vec<ConfigDoc>;
pub struct LogRow { pub hms_millis: String, pub level: Level, pub target: &'static str, pub message: String, pub seq: u64 }
pub fn log_rows<'a>(records: impl Iterator<Item = &'a Record>, filter: &LogFilter, clock: Clock) -> Vec<LogRow>;
pub struct PerfModel { pub frame: Option<Percentiles>, pub frame_count: u64, pub submit: Option<Percentiles>, pub paint: Option<Percentiles>, pub dropped: u64, pub buckets: Vec<(u64 /*upper bound µs*/, u32)>, pub overflow: u32, pub database: String, pub used: String, pub block_size: String, pub memory: String, pub threads: String, pub overlay: bool }
pub struct Percentiles { pub p50: String, pub p95: String, pub max: String }
pub const FRAME_BUDGET_MICROS: u64 = 8_000;
pub const REQUERY_BUDGET_MICROS: u64 = 50_000;
pub fn perf_model(d: &Diagnostics, requery: &RequeryStats) -> PerfModel;
pub struct Badges { pub sources: (Option<Health>, usize), pub datasets: usize, pub config: (usize, usize), pub log_errors: usize, pub perf_p95: String }
pub fn badges(d: &Diagnostics, log_errors: usize) -> Badges;
pub fn header_chips(d: &Diagnostics, clock: Clock) -> Vec<(String, Tone)>;

// log.rs
pub struct LogTail { .. }  // new(ring: Arc<Ring>) starts at latest_seq; drain(&mut self) -> bool; records() -> impl Iterator<Item=&Record>; lost() -> u64; clear(); len()
pub const LOG_CAP: usize = 4_096;
pub struct LogFilter { pub levels: [bool; 5], pub target: Option<String>, pub text: String }
impl LogFilter { pub fn all() -> Self; pub fn accepts(&self, r: &Record) -> bool; pub fn level_index(level: Level) -> usize; }
pub const LEVELS: [Level; 5] = [Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG, Level::TRACE];
```

- [ ] **Step 1: `section.rs` with its test**

```rust
//! The page's sections, in rail order.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Sources,
    Data,
    Config,
    Log,
    Perf,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Sources,
        Section::Data,
        Section::Config,
        Section::Log,
        Section::Perf,
    ];

    /// The session and `:section` spelling.
    pub fn name(self) -> &'static str {
        match self {
            Section::Sources => "sources",
            Section::Data => "data",
            Section::Config => "config",
            Section::Log => "log",
            Section::Perf => "perf",
        }
    }

    /// The rail label.
    pub fn title(self) -> &'static str {
        match self {
            Section::Sources => "Sources",
            Section::Data => "Data",
            Section::Config => "Config",
            Section::Log => "Log",
            Section::Perf => "Perf",
        }
    }

    pub fn from_name(s: &str) -> Option<Section> {
        Section::ALL.iter().copied().find(|x| x.name() == s)
    }

    pub fn next(self) -> Section {
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Section::ALL[(i + 1) % Section::ALL.len()]
    }

    pub fn prev(self) -> Section {
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Section::ALL[(i + Section::ALL.len() - 1) % Section::ALL.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_cycling_wraps() {
        for s in Section::ALL {
            assert_eq!(Section::from_name(s.name()), Some(s));
        }
        assert_eq!(Section::from_name("nope"), None);
        assert_eq!(Section::Perf.next(), Section::Sources);
        assert_eq!(Section::Sources.prev(), Section::Perf);
    }
}
```

- [ ] **Step 2: `log.rs` with failing tests first**

```rust
//! The retained log tail and its page-side filter. Pure: no GPUI, no clock.

use std::collections::VecDeque;
use std::sync::Arc;

use geode_core::log::{Level, Record, Ring};

pub const LOG_CAP: usize = 4_096;
pub const LEVELS: [Level; 5] = [Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG, Level::TRACE];

/// A bounded copy of the ring from the sequence at creation. `drain` reports
/// the gap the ring overwrote since the last drain, never a lifetime total.
pub struct LogTail {
    ring: Arc<Ring>,
    since: u64,
    lost: u64,
    buf: Vec<Record>,
    records: VecDeque<Record>,
}

impl LogTail {
    pub fn new(ring: Arc<Ring>) -> LogTail {
        let since = ring.latest_seq();
        LogTail { ring, since, lost: 0, buf: Vec::new(), records: VecDeque::new() }
    }

    /// Pull new records. Returns whether anything arrived.
    pub fn drain(&mut self) -> bool {
        let latest = self.ring.latest_seq();
        if latest <= self.since {
            return false;
        }
        self.lost = self
            .ring
            .oldest_seq()
            .map(|oldest| oldest.saturating_sub(self.since + 1))
            .unwrap_or(0);
        self.ring.drain_since(self.since, &mut self.buf);
        self.since = latest;
        self.records.extend(self.buf.drain(..));
        while self.records.len() > LOG_CAP {
            self.records.pop_front();
        }
        true
    }

    pub fn has_new(&self) -> bool {
        self.ring.latest_seq() > self.since
    }

    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.iter()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Records overwritten before the last drain reached them.
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// Forget retained records; the next drain continues from where it was.
    pub fn clear(&mut self) {
        self.records.clear();
        self.lost = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    /// Indexed by [`LogFilter::level_index`]; all true by default.
    pub levels: [bool; 5],
    pub target: Option<String>,
    pub text: String,
}

impl LogFilter {
    pub fn all() -> LogFilter {
        LogFilter { levels: [true; 5], target: None, text: String::new() }
    }

    pub fn level_index(level: Level) -> usize {
        LEVELS.iter().position(|l| *l == level).unwrap_or(2)
    }

    pub fn accepts(&self, r: &Record) -> bool {
        self.levels[Self::level_index(r.level)]
            && self.target.as_deref().is_none_or(|t| t == r.target)
            && (self.text.is_empty() || r.message.contains(&self.text) || r.target.contains(&self.text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn rec(level: Level, target: &'static str, message: &str) -> Record {
        Record { at: SystemTime::now(), level, target, message: message.to_string(), seq: 0 }
    }

    #[test]
    fn a_tail_starts_at_the_current_sequence_and_caps_at_log_cap() {
        let ring = Arc::new(Ring::new(8_192));
        ring.push(rec(Level::INFO, "geode::shell", "before"));
        let mut tail = LogTail::new(ring.clone());
        assert!(!tail.drain(), "nothing since creation");
        for i in 0..(LOG_CAP + 10) {
            ring.push(rec(Level::INFO, "geode::shell", &format!("m{i}")));
        }
        assert!(tail.drain());
        assert_eq!(tail.len(), LOG_CAP);
        assert_eq!(tail.records().next().unwrap().message, "m10");
        assert_eq!(tail.lost(), 0);
    }

    #[test]
    fn a_wrap_between_drains_reports_the_gap_measured_at_that_drain() {
        let ring = Arc::new(Ring::new(4));
        let mut tail = LogTail::new(ring.clone());
        for i in 0..10 {
            ring.push(rec(Level::WARN, "geode::ingest", &format!("w{i}")));
        }
        tail.drain();
        // seq 1..=10 pushed, capacity 4 keeps 7..=10: 6 lost since `since` (0).
        assert_eq!(tail.lost(), 6);
        ring.push(rec(Level::WARN, "geode::ingest", "w10"));
        tail.drain();
        assert_eq!(tail.lost(), 0, "not cumulative");
    }

    #[test]
    fn the_filter_gates_on_level_target_and_text() {
        let mut f = LogFilter::all();
        let r = rec(Level::DEBUG, "geode::query", "planned 3 tables");
        assert!(f.accepts(&r));
        f.levels[LogFilter::level_index(Level::DEBUG)] = false;
        assert!(!f.accepts(&r));
        f.levels[LogFilter::level_index(Level::DEBUG)] = true;
        f.target = Some("geode::shell".into());
        assert!(!f.accepts(&r));
        f.target = None;
        f.text = "tables".into();
        assert!(f.accepts(&r));
        f.text = "query".into();
        assert!(f.accepts(&r), "target text matches too");
    }
}
```

Run: `cargo test -p geode-diagnostics log::` → PASS (three tests).

- [ ] **Step 3: `model.rs` typed rows, with tests**

Port each builder from the old `sections.rs` to a typed row. The old file is the reference for every string format; keep those formats where the column keeps the same meaning. Write the file:

```rust
//! Typed row models per section, built from the shell's `Diagnostics`
//! entity, the loaded `Config`, the log tail, and the frame's requery stats.
//! Pure: explicit `now` and clock inputs, no GPUI, no I/O.

use std::time::SystemTime;

use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_core::config::{Config, Diagnostic, Severity};
use geode_core::log::{Level, Record};
use geode_core::query::AsOf;
use geode_shell::diagnostics::{Diagnostics, Health, SourceShape};
use geode_shell::perf::{FrameHistogram, RequeryStats, format_ms, BUCKET_UPPER_BOUNDS_MICROS};

use crate::log::LogFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Warn,
    Error,
    Marked,
}

pub fn health_tone(health: Option<&Health>) -> Tone {
    match health {
        None => Tone::Muted,
        Some(Health::Ok) => Tone::Normal,
        Some(Health::Pending | Health::PendingTooLong) => Tone::Muted,
        Some(Health::Degraded { .. }) => Tone::Warn,
        Some(Health::Failed { .. }) => Tone::Error,
    }
}

fn local_hms(t: SystemTime, clock: Clock) -> String {
    clock.hms(DateTime::<Utc>::from(t))
}

fn local_hms_utc(t: DateTime<Utc>, clock: Clock) -> String {
    clock.hms(t)
}

fn local_hms_millis(t: SystemTime, clock: Clock) -> String {
    let dt = DateTime::<Utc>::from(t);
    format!("{}.{:03}", clock.hms(dt), dt.timestamp_subsec_millis())
}

// ------------------------------------------------------------ Sources

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub name: String,
    pub tone: Tone,
    /// "Ok", "Degraded — reason", "no report yet".
    pub health: String,
    pub since: Option<SystemTime>,
    pub since_hms: String,
    pub shape: String,
    pub last_poll: String,
    pub next_poll: String,
    pub ready: String,
    pub loading: String,
    /// Detail-strip lines: the spec detail by shape.
    pub detail: Vec<String>,
    /// Oldest first, as the entity keeps it.
    pub history: Vec<(String, Health)>,
}

/// Worst health first, then name; unreported sources last by name. The same
/// order the tile used.
pub fn source_rows(d: &Diagnostics, clock: Clock) -> Vec<SourceRow> {
    let mut reported: Vec<_> = d.sources.iter().filter(|(_, s)| s.health.is_some()).collect();
    reported.sort_by(|a, b| b.1.health.cmp(&a.1.health).then_with(|| a.0.cmp(b.0)));
    let mut unreported: Vec<_> = d.sources.iter().filter(|(_, s)| s.health.is_none()).collect();
    unreported.sort_by(|a, b| a.0.cmp(b.0));

    reported
        .into_iter()
        .chain(unreported)
        .map(|(name, state)| {
            let loading = d
                .ingest
                .as_ref()
                .filter(|a| a.source == *name)
                .map(|a| format!("{} since {}", a.path, local_hms(a.since, clock)))
                .unwrap_or_default();
            let (health, since, since_hms) = match &state.health {
                None => ("no report yet".to_string(), None, String::new()),
                Some(h) => {
                    let (label, reason) = h.to_parts();
                    let text = match reason.filter(|r| !r.is_empty()) {
                        Some(r) => format!("{label} — {r}"),
                        None => label,
                    };
                    (text, Some(state.since), local_hms(state.since, clock))
                }
            };
            let (shape, detail) = match &state.spec {
                None => (String::new(), Vec::new()),
                Some(spec) => match spec.shape {
                    SourceShape::Directory => (
                        "directory".to_string(),
                        vec![
                            format!("path: {}", spec.paths.join(", ")),
                            format!(
                                "adapter: {} · priority: {} · readiness: {}",
                                spec.adapter, spec.priority, spec.readiness
                            ),
                        ],
                    ),
                    SourceShape::Fetch => (
                        "fetch".to_string(),
                        vec![format!("adapter: {}", spec.adapter), "fetch".to_string()],
                    ),
                    SourceShape::Subscribed => (
                        format!("subscribed · {} topics", spec.topics.len()),
                        vec![
                            format!("adapter: {}", spec.adapter),
                            format!("topics: {}", spec.topics.join(", ")),
                        ],
                    ),
                },
            };
            let polled = state.last_poll.is_some() || state.next_poll.is_some();
            SourceRow {
                name: name.clone(),
                tone: health_tone(state.health.as_ref()),
                health,
                since,
                since_hms,
                shape,
                last_poll: state.last_poll.map(|t| local_hms(t, clock)).unwrap_or_default(),
                next_poll: state.next_poll.map(|t| local_hms(t, clock)).unwrap_or_default(),
                ready: if polled { state.last_ready.to_string() } else { String::new() },
                loading,
                detail,
                history: state
                    .history
                    .iter()
                    .map(|(at, h)| (local_hms(*at, clock), h.clone()))
                    .collect(),
            }
        })
        .collect()
}

/// "4 m", "12 s", "1 h 3 m"; empty when unknown.
pub fn age_text(since: Option<SystemTime>, now: SystemTime) -> String {
    let Some(since) = since else { return String::new() };
    let secs = now.duration_since(since).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3_600 => format!("{} m", s / 60),
        s => format!("{} h {} m", s / 3_600, (s % 3_600) / 60),
    }
}

// ------------------------------------------------------------ Data

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionRow {
    /// "2026-09-27 · EU_TECH" or "2026-09-27 · (bookless)".
    pub label: String,
    pub gen_id: String,
    pub source_time: String,
    pub loaded: String,
    pub rows: String,
    pub kind: &'static str,
    /// The generation `resolve_generations` chose for the frame's as-of,
    /// shown only when the catalog was answered for that as-of.
    pub marked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetRow {
    pub name: String,
    pub has_catalog: bool,
    pub live_rows: String,
    pub archive_rows: String,
    pub partitions: usize,
    pub latest_gen: String,
    pub published: String,
    pub resolved: String,
    pub children: Vec<PartitionRow>,
}

pub fn catalog_matches_frame(d: &Diagnostics, as_of: &AsOf) -> bool {
    d.catalog.as_ref().is_some_and(|c| &c.as_of == as_of)
}

pub fn dataset_rows(d: &Diagnostics, as_of: &AsOf, clock: Clock) -> Vec<DatasetRow> {
    let matches = catalog_matches_frame(d, as_of);
    d.datasets
        .iter()
        .map(|(name, state)| {
            let Some(catalog) = &state.catalog else {
                return DatasetRow {
                    name: name.clone(),
                    has_catalog: false,
                    live_rows: String::new(),
                    archive_rows: String::new(),
                    partitions: 0,
                    latest_gen: String::new(),
                    published: String::new(),
                    resolved: String::new(),
                    children: Vec::new(),
                };
            };
            let mut children = Vec::new();
            let mut latest: Option<(i64, DateTime<Utc>)> = None;
            let mut resolved = Vec::new();
            for part in &catalog.partitions {
                let book = part.book.as_deref().unwrap_or("(bookless)");
                for g in &part.generations {
                    let marked = !as_of.is_live() && matches && part.resolved_gen == Some(g.gen_id);
                    if marked {
                        resolved.push(g.gen_id.to_string());
                    }
                    if g.live && latest.is_none_or(|(_, t)| g.source_time > t) {
                        latest = Some((g.gen_id, g.source_time));
                    }
                    children.push(PartitionRow {
                        label: format!("{} · {book}", part.batch),
                        gen_id: g.gen_id.to_string(),
                        source_time: local_hms_utc(g.source_time, clock),
                        loaded: g.loaded_at.map(|t| local_hms_utc(t, clock)).unwrap_or_else(|| "?".into()),
                        rows: g.file_rows.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        kind: if g.live { "live" } else { "archive" },
                        marked,
                    });
                }
            }
            DatasetRow {
                name: name.clone(),
                has_catalog: true,
                live_rows: catalog.live_rows.to_string(),
                archive_rows: catalog.archive_rows.to_string(),
                partitions: catalog.partitions.len(),
                latest_gen: latest.map(|(g, _)| g.to_string()).unwrap_or_default(),
                published: latest.map(|(_, t)| local_hms_utc(t, clock)).unwrap_or_default(),
                resolved: resolved.join(", "),
                children,
            }
        })
        .collect()
}

// ------------------------------------------------------------ Config

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Config,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRow {
    pub severity: Severity,
    pub lane: Lane,
    /// History rows carry their batch time; current rows none.
    pub batch: Option<String>,
    /// "file › path" when the diagnostic carries them; else its layer or "".
    pub location: String,
    pub message: String,
    /// `Diagnostic`'s own Display, for the detail strip.
    pub full: String,
}

fn diagnostic_row(diag: &Diagnostic, lane: Lane, batch: Option<String>) -> DiagnosticRow {
    let file = diag
        .file
        .as_ref()
        .and_then(|f| f.file_name())
        .map(|f| f.to_string_lossy().to_string());
    let location = match (file, &diag.path) {
        (Some(f), Some(p)) => format!("{f} › {p}"),
        (Some(f), None) => f,
        (None, Some(p)) => p.clone(),
        (None, None) => diag.layer.map(|l| l.name().to_string()).unwrap_or_default(),
    };
    DiagnosticRow {
        severity: diag.severity,
        lane,
        batch,
        location,
        message: diag.message.clone(),
        full: diag.to_string(),
    }
}

/// The current config batch, then retained data conditions.
pub fn current_diagnostics(d: &Diagnostics) -> Vec<DiagnosticRow> {
    d.config
        .iter()
        .map(|x| diagnostic_row(x, Lane::Config, None))
        .chain(d.data_diagnostics.iter().map(|(_, x)| diagnostic_row(x, Lane::Data, None)))
        .collect()
}

/// Prior batches, newest first, each row tagged with its batch time. The
/// current batch (index 0) is excluded.
pub fn history_diagnostics(d: &Diagnostics, clock: Clock) -> Vec<DiagnosticRow> {
    d.config_history
        .iter()
        .skip(1)
        .flat_map(|(at, diags)| {
            let batch = local_hms(*at, clock);
            diags
                .iter()
                .map(move |x| diagnostic_row(x, Lane::Config, Some(batch.clone())))
        })
        .collect()
}

pub const MAX_LEAVES_PER_DOC: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLeaf {
    pub key: String,
    pub value: String,
    pub layer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDoc {
    pub name: String,
    pub leaves: Vec<ConfigLeaf>,
    /// Leaves past the cap, after filtering.
    pub omitted: usize,
}

/// Every loaded document with its leaves (`a.b.0.c = value [layer]`),
/// filtered by substring over `doc.key` and value, capped per document.
pub fn config_docs(config: &Config, filter: &str) -> Vec<ConfigDoc> {
    config
        .doc_names()
        .filter_map(|doc_name| {
            let doc = config.doc(doc_name)?;
            let mut leaves = Vec::new();
            walk_leaves(&doc.value, "", &mut leaves);
            let mut kept: Vec<ConfigLeaf> = leaves
                .into_iter()
                .filter(|(path, value)| {
                    filter.is_empty()
                        || format!("{doc_name}.{path}").contains(filter)
                        || value.contains(filter)
                })
                .map(|(path, value)| ConfigLeaf {
                    layer: config.explain(doc_name, &path).map(|l| l.name().to_string()).unwrap_or_else(|| "?".into()),
                    key: path,
                    value,
                })
                .collect();
            let omitted = kept.len().saturating_sub(MAX_LEAVES_PER_DOC);
            kept.truncate(MAX_LEAVES_PER_DOC);
            Some(ConfigDoc { name: doc_name.to_string(), leaves: kept, omitted })
        })
        .collect()
}

fn walk_leaves(table: &toml::Table, prefix: &str, out: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        walk_value(value, &path, out);
    }
}

fn walk_value(value: &toml::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(t) => walk_leaves(t, path, out),
        toml::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                walk_value(v, &format!("{path}.{i}"), out);
            }
        }
        other => out.push((path.to_string(), other.to_string())),
    }
}

// ------------------------------------------------------------ Log

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub hms_millis: String,
    pub level: Level,
    pub target: &'static str,
    pub message: String,
    pub seq: u64,
}

pub fn log_rows<'a>(records: impl Iterator<Item = &'a Record>, filter: &LogFilter, clock: Clock) -> Vec<LogRow> {
    records
        .filter(|r| filter.accepts(r))
        .map(|r| LogRow {
            hms_millis: local_hms_millis(r.at, clock),
            level: r.level,
            target: r.target,
            message: r.message.clone(),
            seq: r.seq,
        })
        .collect()
}

/// Distinct targets in the tail, sorted, for the target select.
pub fn log_targets<'a>(records: impl Iterator<Item = &'a Record>) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = records.map(|r| r.target).collect();
    v.sort_unstable();
    v.dedup();
    v
}

// ------------------------------------------------------------ Perf

pub const FRAME_BUDGET_MICROS: u64 = 8_000;
pub const REQUERY_BUDGET_MICROS: u64 = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Percentiles {
    pub p50: String,
    pub p95: String,
    pub max: String,
}

fn percentiles(h: &FrameHistogram) -> Option<Percentiles> {
    (h.count() > 0).then(|| Percentiles {
        p50: h.percentile_micros(50.0).map(format_ms).unwrap_or_default(),
        p95: h.percentile_micros(95.0).map(format_ms).unwrap_or_default(),
        max: format_ms(h.max_micros()),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerfModel {
    pub frame: Option<Percentiles>,
    pub frame_count: u64,
    pub submit: Option<Percentiles>,
    pub paint: Option<Percentiles>,
    pub dropped: u64,
    /// `(upper bound µs, count)` per bucket, then the overflow bucket as
    /// `(u64::MAX, overflow)`.
    pub buckets: Vec<(u64, u32)>,
    pub database: String,
    pub used: String,
    pub block_size: String,
    pub memory: String,
    pub threads: String,
    pub overlay: bool,
}

pub fn perf_model(d: &Diagnostics, requery: &RequeryStats) -> PerfModel {
    let h = &d.frame_hist;
    let mut buckets: Vec<(u64, u32)> = BUCKET_UPPER_BOUNDS_MICROS
        .iter()
        .copied()
        .zip(h.buckets().iter().copied())
        .collect();
    buckets.push((u64::MAX, h.overflow()));
    let (database, used, block_size, memory, threads) = match &d.catalog {
        Some(c) => (
            format_bytes(c.database_bytes),
            format_bytes(c.used_blocks.saturating_mul(c.block_size)),
            format_bytes(c.block_size),
            format_bytes(c.memory_bytes),
            c.threads.to_string(),
        ),
        None => (String::new(), String::new(), String::new(), String::new(), String::new()),
    };
    PerfModel {
        frame: percentiles(h),
        frame_count: h.count(),
        submit: percentiles(requery.submit_to_snapshot()),
        paint: percentiles(requery.snapshot_to_paint()),
        dropped: d.dropped_events,
        buckets,
        database,
        used,
        block_size,
        memory,
        threads,
        overlay: d.overlay_visible(),
    }
}

pub fn format_bytes(n: u64) -> String {
    // Port the old `sections::format_bytes` verbatim.
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

// ------------------------------------------------------------ Badges and header

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badges {
    /// Worst reported health and the source count.
    pub sources: (Option<Health>, usize),
    pub datasets: usize,
    /// (errors, warnings) in the current config batch plus data conditions.
    pub config: (usize, usize),
    pub log_errors: usize,
    pub perf_p95: String,
}

pub fn badges(d: &Diagnostics, log_errors: usize) -> Badges {
    let worst = d.sources.values().filter_map(|s| s.health.clone()).max();
    let current = current_diagnostics(d);
    let errors = current.iter().filter(|r| r.severity == Severity::Error).count();
    let warnings = current.iter().filter(|r| r.severity == Severity::Warning).count();
    Badges {
        sources: (worst, d.sources.len()),
        datasets: d.datasets.len(),
        config: (errors, warnings),
        log_errors,
        perf_p95: d.frame_hist.percentile_micros(95.0).map(format_ms).unwrap_or_default(),
    }
}

/// The header chips, worst first: source health, config errors, data
/// errors, catalog time or pending.
pub fn header_chips(d: &Diagnostics, clock: Clock) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    if let Some(worst) = d.sources.values().filter_map(|s| s.health.clone()).max() {
        let n = d.sources.values().filter(|s| s.health.as_ref() == Some(&worst)).count();
        out.push((format!("{n} {}", worst.label().to_lowercase()), health_tone(Some(&worst))));
    }
    let errors = d.config.iter().filter(|x| x.severity == Severity::Error).count();
    if errors > 0 {
        out.push((format!("config {errors} error{}", if errors == 1 { "" } else { "s" }), Tone::Error));
    }
    let data_errors = d.data_diagnostics.iter().filter(|(_, x)| x.severity == Severity::Error).count();
    if data_errors > 0 {
        out.push((format!("data {data_errors} error{}", if data_errors == 1 { "" } else { "s" }), Tone::Error));
    }
    match d.catalog.as_ref() {
        Some(_) => out.push((format!("catalog {}", local_hms(d.catalog_at.unwrap_or(SystemTime::UNIX_EPOCH), clock)), Tone::Normal)),
        None => out.push(("catalog pending".to_string(), Tone::Muted)),
    }
    out
}
```

Notes for the implementer:
- `Diagnostics` has no `catalog_at` today. Add `pub catalog_at: Option<SystemTime>` to the entity, set in `set_catalog` from a new `at: SystemTime` parameter (the bridge passes `SystemTime::now()`, matching how `note_health` takes its timestamp). Callers to update: `crates/geode-app/src/bridge.rs:1059`, and the tests in `crates/geode-timeseries/src/tile/tests.rs:254` and `crates/geode-pricer/src/tile.rs:8331` (the old `sections.rs` callers go with that file).
- `Diagnostics` field names used above (`sources`, `datasets`, `ingest`, `config`, `data_diagnostics`, `config_history`, `catalog`, `frame_hist`, `dropped_events`) are the ones the old `sections.rs` and `build_summary` read; confirm each is `pub` and expose any that is not with a reader.
- `FrameHistogram::buckets()` and `overflow()` do not exist yet: add to `perf.rs`

```rust
    /// Per-bucket counts, aligned with [`BUCKET_UPPER_BOUNDS_MICROS`].
    pub fn buckets(&self) -> &[u32; NUM_BUCKETS] {
        &self.buckets
    }
    /// Samples above the last bound but below the idle cutoff.
    pub fn overflow(&self) -> u32 {
        self.overflow
    }
```
- `Health::label()` is what `to_parts` uses; if it is private, make it `pub`.
- `Option::is_none_or` requires Rust 1.82; the workspace already uses let-chains, so it is available.

Tests for `model.rs` (port the old `sections.rs` tests' fixtures: `dataset_catalog()` at its lines 835–864, and the health/source fixtures):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::log::LogLevels;
    use geode_core::query::{CatalogSnapshot, DatasetCatalog, GenerationInfo, PartitionCatalog};
    use std::time::{Duration, SystemTime};

    fn clock() -> Clock {
        Clock::utc()
    }

    #[test]
    fn sources_order_worst_first_then_name_and_unreported_last() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::now();
        d.note_health("b_ok", Health::Ok, String::new(), t);
        d.note_health("a_degraded", Health::Degraded { reason: "stale".into() }, String::new(), t);
        d.note_polled("zz_unreported", 0, t, t + Duration::from_secs(60));
        let rows = source_rows(&d, clock());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a_degraded", "b_ok", "zz_unreported"]);
        assert_eq!(rows[0].health, "Degraded — stale");
        assert_eq!(rows[0].tone, Tone::Warn);
        assert_eq!(rows[2].health, "no report yet");
        assert_eq!(rows[2].ready, "0", "polled: ready shown");
        assert_eq!(rows[0].ready, "", "never polled: blank");
    }

    #[test]
    fn a_loading_row_shows_for_an_unreported_source_too() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::now();
        d.note_loading("cold", "/x/a.csv", 2, t);
        let rows = source_rows(&d, clock());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].loading.starts_with("/x/a.csv since "), "{}", rows[0].loading);
    }

    #[test]
    fn age_text_scales() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        assert_eq!(age_text(Some(now - Duration::from_secs(5)), now), "5 s");
        assert_eq!(age_text(Some(now - Duration::from_secs(240)), now), "4 m");
        assert_eq!(age_text(Some(now - Duration::from_secs(3_780)), now), "1 h 3 m");
        assert_eq!(age_text(None, now), "");
    }

    pub(crate) fn dataset_catalog() -> DatasetCatalog {
        DatasetCatalog {
            name: "risk".into(),
            partitions: vec![PartitionCatalog {
                batch: "2026-09-08".into(),
                book: Some("EU_TECH".into()),
                generations: vec![
                    GenerationInfo {
                        gen_id: 1,
                        source_time: chrono::DateTime::UNIX_EPOCH,
                        loaded_at: Some(chrono::DateTime::UNIX_EPOCH),
                        file_rows: Some(100),
                        live: false,
                    },
                    GenerationInfo {
                        gen_id: 2,
                        source_time: chrono::DateTime::UNIX_EPOCH,
                        loaded_at: Some(chrono::DateTime::UNIX_EPOCH),
                        file_rows: Some(120),
                        live: true,
                    },
                ],
                resolved_gen: Some(1),
            }],
            live_rows: 120,
            archive_rows: 100,
            series: Vec::new(),
        }
    }

    #[test]
    fn resolved_markers_need_a_matching_catalog_and_a_historical_frame() {
        let mut d = Diagnostics::new(LogLevels::default());
        let at = AsOf::At(chrono::Utc::now());
        d.set_catalog(CatalogSnapshot { as_of: at.clone(), datasets: vec![dataset_catalog()], database_bytes: 0, used_blocks: 0, block_size: 0, memory_bytes: 0, threads: 1, identities: Vec::new() }, SystemTime::now());
        let rows = dataset_rows(&d, &at, clock());
        assert_eq!(rows[0].children.iter().filter(|c| c.marked).count(), 1);
        assert_eq!(rows[0].resolved, "1");
        assert_eq!(rows[0].latest_gen, "2");
        let live = dataset_rows(&d, &AsOf::Live, clock());
        assert!(live[0].children.iter().all(|c| !c.marked), "live: nothing resolved");
        let other = AsOf::At(chrono::Utc::now() + chrono::Duration::hours(1));
        let stale = dataset_rows(&d, &other, clock());
        assert!(stale[0].children.iter().all(|c| !c.marked), "catalog for another as-of: hidden");
        assert!(!catalog_matches_frame(&d, &other));
    }

    #[test]
    fn config_docs_carry_provenance_recurse_into_arrays_and_filter() {
        use geode_core::config::{ConfigSources, LayerDoc};
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "config_version = 1\n[theme]\nname = \"Solarized\"\n").unwrap(),
                LayerDoc::builtin("keymap", "config_version = 1\n[[bindings]]\n[bindings.keys]\n\"j\" = \"x::down\"\n").unwrap(),
            ],
            desk: None,
            user: None,
        });
        let docs = config_docs(&config, "");
        let app = docs.iter().find(|d| d.name == "app").unwrap();
        let theme = app.leaves.iter().find(|l| l.key == "theme.name").unwrap();
        assert_eq!(theme.value, "\"Solarized\"");
        assert_eq!(theme.layer, "builtin");
        let keymap = docs.iter().find(|d| d.name == "keymap").unwrap();
        assert!(keymap.leaves.iter().any(|l| l.key == "bindings.0.keys.j"), "indexed array path");
        let filtered = config_docs(&config, "theme");
        assert!(filtered.iter().all(|d| d.leaves.iter().all(|l| format!("{}.{}", d.name, l.key).contains("theme"))));
        assert!(docs.iter().all(|d| d.leaves.len() <= MAX_LEAVES_PER_DOC && d.omitted == 0));
    }

    #[test]
    fn badges_count_errors_and_warnings_and_worst_health() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::now();
        d.note_health("a", Health::Ok, String::new(), t);
        d.note_health("b", Health::Failed { reason: "x".into() }, String::new(), t);
        d.note_config(vec![
            Diagnostic { severity: Severity::Error, layer: None, file: None, message: "e".into(), path: None },
            Diagnostic { severity: Severity::Warning, layer: None, file: None, message: "w".into(), path: None },
        ], t);
        let b = badges(&d, 3);
        assert_eq!(b.sources, (Some(Health::Failed { reason: "x".into() }), 2));
        assert_eq!(b.config, (1, 1));
        assert_eq!(b.log_errors, 3);
        let chips = header_chips(&d, clock());
        assert_eq!(chips[0].0, "1 failed");
        assert_eq!(chips[1], ("config 1 error".to_string(), Tone::Error));
        assert_eq!(chips.last().unwrap(), &("catalog pending".to_string(), Tone::Muted));
    }

    #[test]
    fn perf_model_carries_buckets_overflow_and_the_overlay_mirror() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.watch();
        let mut hist = FrameHistogram::new();
        hist.record_micros(1_000);
        hist.record_micros(200_000);
        d.refresh_frame_hist(&hist);
        d.set_overlay_visible(true);
        let m = perf_model(&d, &RequeryStats::new());
        assert_eq!(m.frame_count, 2);
        assert_eq!(m.buckets.len(), BUCKET_UPPER_BOUNDS_MICROS.len() + 1);
        assert_eq!(m.buckets.iter().map(|(_, n)| *n as u64).sum::<u64>(), 2);
        assert_eq!(m.buckets.last(), Some(&(u64::MAX, 1)));
        assert!(m.overlay);
    }
}
```

The `dataset_catalog` fixture is the old `sections.rs` one, made `pub(crate)` because Task 8's page tests reuse it.

- [ ] **Step 4: Run all three modules' tests**

Run: `cargo test -p geode-diagnostics section:: model:: log::`
Expected: PASS.

- [ ] **Step 5: Gates and commit**

```bash
git add crates/geode-diagnostics crates/geode-shell/src/perf.rs crates/geode-shell/src/diagnostics.rs crates/geode-app
git commit -m "feat(diagnostics): typed section models, badges, and the log tail

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Prepared tables and the one `TableDelegate`

**Files:**
- Create: `crates/geode-diagnostics/src/prepared.rs`
- Create: `crates/geode-diagnostics/src/table.rs`
- Modify: `crates/geode-diagnostics/src/lib.rs` (`pub mod prepared; mod table;`)
- Test: inline tests in `prepared.rs`

**Interfaces:**
- Consumes: Task 5 models; gpui-component `table::{Column, ColumnSort, TableDelegate, TableState}` (registry: `src/table/delegate.rs`, `column.rs`, `state.rs`).
- Produces:

```rust
// prepared.rs (pure)
pub struct ColumnSpec { pub key: &'static str, pub name: &'static str, pub width: f32 /* design px */, pub right: bool }
pub struct Cell { pub text: SharedString, pub tone: Tone, pub indent: u8 }
pub enum RowKind { Plain, Parent { expanded: bool }, Child, Notice }
pub struct PreparedRow { pub key: String, pub kind: RowKind, pub cells: Vec<Cell>, pub detail: Vec<SharedString>, pub tone: Tone }
pub struct PreparedTable { pub columns: Vec<ColumnSpec>, pub rows: Vec<PreparedRow> }
impl PreparedTable { pub fn empty() -> Self; pub fn parent_key_at(&self, ix: usize) -> Option<&str>; }
pub fn sources_table(rows: &[SourceRow], now: SystemTime, filter: &str) -> PreparedTable;
pub fn data_table(rows: &[DatasetRow], collapsed: &BTreeSet<String>, filter: &str) -> PreparedTable;
pub fn diagnostics_table(rows: &[DiagnosticRow], history: bool) -> PreparedTable;
pub fn config_table(docs: &[ConfigDoc], collapsed: &BTreeSet<String>) -> PreparedTable;
pub fn log_table(rows: &[LogRow], lost: u64) -> PreparedTable;

// table.rs
pub struct SectionDelegate { .. }
impl SectionDelegate { pub fn new(on_select: Rc<dyn Fn(usize, &mut Window, &mut App)>) -> Self; pub fn set(&mut self, table: Rc<PreparedTable>); pub fn table(&self) -> &Rc<PreparedTable>; pub fn set_line_numbers(..) — not needed }
impl TableDelegate for SectionDelegate { .. }
```

- [ ] **Step 1: Write `prepared.rs` tests first**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::collections::BTreeSet;

    fn dataset(name: &str, children: usize) -> DatasetRow {
        DatasetRow {
            name: name.into(),
            has_catalog: true,
            live_rows: "1".into(),
            archive_rows: "0".into(),
            partitions: 1,
            latest_gen: "1".into(),
            published: "".into(),
            resolved: "".into(),
            children: (0..children)
                .map(|i| PartitionRow {
                    label: format!("p{i}"),
                    gen_id: i.to_string(),
                    source_time: "".into(),
                    loaded: "".into(),
                    rows: "".into(),
                    kind: "live",
                    marked: i == 0,
                })
                .collect(),
        }
    }

    #[test]
    fn a_collapsed_dataset_contributes_only_its_parent_row() {
        let rows = vec![dataset("risk", 2), dataset("vol", 1)];
        let open = data_table(&rows, &BTreeSet::new(), "");
        assert_eq!(open.rows.len(), 5);
        assert!(matches!(open.rows[0].kind, RowKind::Parent { expanded: true }));
        assert!(matches!(open.rows[1].kind, RowKind::Child));
        assert_eq!(open.rows[1].tone, Tone::Marked, "resolved child is marked");
        let mut collapsed = BTreeSet::new();
        collapsed.insert("risk".to_string());
        let t = data_table(&rows, &collapsed, "");
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.parent_key_at(0), Some("risk"));
        assert_eq!(t.parent_key_at(2), Some("vol"), "a child resolves to its parent");
        let filtered = data_table(&rows, &BTreeSet::new(), "vol");
        assert_eq!(filtered.rows.len(), 2);
    }

    #[test]
    fn the_log_table_leads_with_a_loss_notice_when_records_were_lost() {
        let rows = vec![LogRow { hms_millis: "09:00:00.000".into(), level: geode_core::log::Level::ERROR, target: "geode::shell", message: "boom".into(), seq: 1 }];
        let t = log_table(&rows, 7);
        assert!(matches!(t.rows[0].kind, RowKind::Notice));
        assert!(t.rows[0].cells[0].text.contains("7 records lost"));
        assert_eq!(t.rows[1].tone, Tone::Error);
        assert_eq!(t.rows[1].detail, vec![gpui::SharedString::from("09:00:00.000 ERROR geode::shell boom")]);
        assert_eq!(log_table(&rows, 0).rows.len(), 1);
    }

    #[test]
    fn a_source_row_carries_the_age_and_its_detail_lines() {
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let rows = vec![SourceRow {
            name: "s".into(), tone: Tone::Normal, health: "Ok".into(),
            since: Some(now - std::time::Duration::from_secs(30)), since_hms: "00:01:10".into(),
            shape: "fetch".into(), last_poll: "".into(), next_poll: "".into(), ready: "".into(),
            loading: "".into(), detail: vec!["adapter: X".into(), "fetch".into()],
            history: vec![("00:00:01".into(), geode_shell::diagnostics::Health::Ok)],
        }];
        let t = sources_table(&rows, now, "");
        let since = &t.rows[0].cells[2].text;
        assert_eq!(since.as_ref(), "00:01:10 · 30 s");
        assert_eq!(t.rows[0].detail.len(), 3, "spec lines then one history line");
        assert!(t.rows[0].detail[2].contains("Ok 00:00:01"));
        assert!(sources_table(&rows, now, "zzz").rows.is_empty());
    }
}
```

- [ ] **Step 2: Implement `prepared.rs`**

```rust
//! Prepared tables: what a section paints, built from its typed rows with
//! expansion and filtering applied. Pure; `Rc`-shared with the delegate.

use std::collections::BTreeSet;
use std::time::SystemTime;

use geode_core::config::Severity;
use geode_core::log::Level;
use gpui::SharedString;

use crate::model::{
    age_text, ConfigDoc, DatasetRow, DiagnosticRow, Lane, LogRow, SourceRow, Tone,
};

#[derive(Debug, Clone, Copy)]
pub struct ColumnSpec {
    pub key: &'static str,
    pub name: &'static str,
    /// Width in pixels at the design rem (`shell::scale`).
    pub width: f32,
    pub right: bool,
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub text: SharedString,
    pub tone: Tone,
    pub indent: u8,
}

fn cell(text: impl Into<SharedString>, tone: Tone) -> Cell {
    Cell { text: text.into(), tone, indent: 0 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Plain,
    Parent { expanded: bool },
    Child,
    /// A full-width message row such as the log's loss report.
    Notice,
}

#[derive(Debug, Clone)]
pub struct PreparedRow {
    /// Stable identity: the source name, dataset name, `doc` or `doc.key`,
    /// or the log record's sequence.
    pub key: String,
    pub kind: RowKind,
    pub cells: Vec<Cell>,
    /// Lines for the detail strip when this row is the cursor.
    pub detail: Vec<SharedString>,
    pub tone: Tone,
}

#[derive(Debug, Clone, Default)]
pub struct PreparedTable {
    pub columns: Vec<ColumnSpec>,
    pub rows: Vec<PreparedRow>,
}

impl PreparedTable {
    pub fn empty() -> PreparedTable {
        PreparedTable::default()
    }

    /// The expandable key this row belongs to: itself for a parent, the
    /// nearest parent above for a child, none otherwise.
    pub fn parent_key_at(&self, ix: usize) -> Option<&str> {
        let row = self.rows.get(ix)?;
        match row.kind {
            RowKind::Parent { .. } => Some(row.key.as_str()),
            RowKind::Child => self.rows[..ix]
                .iter()
                .rev()
                .find(|r| matches!(r.kind, RowKind::Parent { .. }))
                .map(|r| r.key.as_str()),
            RowKind::Plain | RowKind::Notice => None,
        }
    }
}

const fn col(key: &'static str, name: &'static str, width: f32) -> ColumnSpec {
    ColumnSpec { key, name, width, right: false }
}
const fn num(key: &'static str, name: &'static str, width: f32) -> ColumnSpec {
    ColumnSpec { key, name, width, right: true }
}

pub const SOURCE_COLUMNS: [ColumnSpec; 8] = [
    col("source", "Source", 160.0),
    col("health", "Health", 220.0),
    col("since", "Since", 130.0),
    col("shape", "Shape", 130.0),
    col("last_poll", "Last poll", 90.0),
    col("next_poll", "Next poll", 90.0),
    num("ready", "Ready", 60.0),
    col("loading", "Loading", 260.0),
];

pub fn sources_table(rows: &[SourceRow], now: SystemTime, filter: &str) -> PreparedTable {
    let rows = rows
        .iter()
        .filter(|r| filter.is_empty() || r.name.contains(filter) || r.health.contains(filter))
        .map(|r| {
            let since = if r.since_hms.is_empty() {
                String::new()
            } else {
                format!("{} · {}", r.since_hms, age_text(r.since, now))
            };
            let mut detail: Vec<SharedString> = r.detail.iter().map(|s| s.clone().into()).collect();
            if !r.history.is_empty() {
                let history = r
                    .history
                    .iter()
                    .map(|(at, h)| format!("{} {at}", h.label()))
                    .collect::<Vec<_>>()
                    .join(" → ");
                detail.push(format!("history: {history}").into());
            }
            PreparedRow {
                key: r.name.clone(),
                kind: RowKind::Plain,
                cells: vec![
                    cell(r.name.clone(), Tone::Normal),
                    cell(r.health.clone(), r.tone),
                    cell(since, Tone::Muted),
                    cell(r.shape.clone(), Tone::Muted),
                    cell(r.last_poll.clone(), Tone::Muted),
                    cell(r.next_poll.clone(), Tone::Muted),
                    cell(r.ready.clone(), Tone::Muted),
                    cell(r.loading.clone(), Tone::Muted),
                ],
                detail,
                tone: r.tone,
            }
        })
        .collect();
    PreparedTable { columns: SOURCE_COLUMNS.to_vec(), rows }
}

pub const DATA_COLUMNS: [ColumnSpec; 8] = [
    col("dataset", "Dataset", 220.0),
    num("partitions", "Partitions", 80.0),
    num("gen", "Latest gen", 90.0),
    col("published", "Published", 90.0),
    num("rows", "Rows", 110.0),
    num("resolved", "Resolved", 90.0),
    col("live", "Live", 70.0),
    col("loaded", "Loaded", 90.0),
];

pub fn data_table(rows: &[DatasetRow], collapsed: &BTreeSet<String>, filter: &str) -> PreparedTable {
    let mut out = Vec::new();
    for r in rows.iter().filter(|r| filter.is_empty() || r.name.contains(filter)) {
        let expanded = !collapsed.contains(&r.name);
        let tone = if r.has_catalog { Tone::Normal } else { Tone::Muted };
        let name = if r.has_catalog { r.name.clone() } else { format!("{} (no catalog yet)", r.name) };
        out.push(PreparedRow {
            key: r.name.clone(),
            kind: RowKind::Parent { expanded },
            cells: vec![
                cell(name, tone),
                cell(r.partitions.to_string(), Tone::Muted),
                cell(r.latest_gen.clone(), Tone::Normal),
                cell(r.published.clone(), Tone::Muted),
                cell(format!("{} live · {} archive", r.live_rows, r.archive_rows), Tone::Muted),
                cell(r.resolved.clone(), Tone::Marked),
                cell(String::new(), Tone::Muted),
                cell(String::new(), Tone::Muted),
            ],
            detail: vec![format!("{}: {} rows (est., live) · {} rows (est., archive)", r.name, r.live_rows, r.archive_rows).into()],
            tone,
        });
        if !expanded {
            continue;
        }
        for c in &r.children {
            let tone = if c.marked { Tone::Marked } else { Tone::Normal };
            out.push(PreparedRow {
                key: format!("{}/{}/{}", r.name, c.label, c.gen_id),
                kind: RowKind::Child,
                cells: vec![
                    Cell { text: c.label.clone().into(), tone: Tone::Muted, indent: 1 },
                    cell(String::new(), Tone::Muted),
                    cell(c.gen_id.clone(), tone),
                    cell(c.source_time.clone(), tone),
                    cell(c.rows.clone(), tone),
                    cell(if c.marked { "●" } else { "" }, Tone::Marked),
                    cell(c.kind, tone),
                    cell(c.loaded.clone(), Tone::Muted),
                ],
                detail: vec![format!("gen {} · source {} · loaded {} · rows {} · {}", c.gen_id, c.source_time, c.loaded, c.rows, c.kind).into()],
                tone,
            });
        }
    }
    PreparedTable { columns: DATA_COLUMNS.to_vec(), rows: out }
}

pub const DIAGNOSTIC_COLUMNS: [ColumnSpec; 4] = [
    col("sev", "Sev", 70.0),
    col("lane", "Lane", 70.0),
    col("where", "Where", 240.0),
    col("message", "Message", 520.0),
];
pub const HISTORY_COLUMNS: [ColumnSpec; 5] = [
    col("batch", "Batch", 90.0),
    col("sev", "Sev", 70.0),
    col("lane", "Lane", 70.0),
    col("where", "Where", 240.0),
    col("message", "Message", 520.0),
];

pub fn diagnostics_table(rows: &[DiagnosticRow], history: bool) -> PreparedTable {
    let rows = rows
        .iter()
        .map(|r| {
            let (sev, tone) = match r.severity {
                Severity::Error => ("error", Tone::Error),
                Severity::Warning => ("warn", Tone::Warn),
            };
            let lane = match r.lane {
                Lane::Config => "config",
                Lane::Data => "data",
            };
            let mut cells = Vec::with_capacity(5);
            if history {
                cells.push(cell(r.batch.clone().unwrap_or_default(), Tone::Muted));
            }
            cells.extend([
                cell(sev, tone),
                cell(lane, Tone::Muted),
                cell(r.location.clone(), Tone::Muted),
                cell(r.message.clone(), Tone::Normal),
            ]);
            PreparedRow {
                key: format!("{}|{}", r.batch.clone().unwrap_or_default(), r.full),
                kind: RowKind::Plain,
                cells,
                detail: vec![r.full.clone().into()],
                tone,
            }
        })
        .collect();
    PreparedTable {
        columns: if history { HISTORY_COLUMNS.to_vec() } else { DIAGNOSTIC_COLUMNS.to_vec() },
        rows,
    }
}

pub const CONFIG_COLUMNS: [ColumnSpec; 3] = [
    col("key", "Key", 360.0),
    col("value", "Value", 360.0),
    col("layer", "Layer", 90.0),
];

pub fn config_table(docs: &[ConfigDoc], collapsed: &BTreeSet<String>) -> PreparedTable {
    let mut out = Vec::new();
    for doc in docs {
        let expanded = !collapsed.contains(&doc.name);
        let count = doc.leaves.len() + doc.omitted;
        out.push(PreparedRow {
            key: doc.name.clone(),
            kind: RowKind::Parent { expanded },
            cells: vec![
                cell(doc.name.clone(), Tone::Normal),
                cell(format!("{count} leaves"), Tone::Muted),
                cell(String::new(), Tone::Muted),
            ],
            detail: vec![format!("{}: {count} leaves", doc.name).into()],
            tone: Tone::Normal,
        });
        if !expanded {
            continue;
        }
        for leaf in &doc.leaves {
            out.push(PreparedRow {
                key: format!("{}.{}", doc.name, leaf.key),
                kind: RowKind::Child,
                cells: vec![
                    Cell { text: leaf.key.clone().into(), tone: Tone::Normal, indent: 1 },
                    cell(leaf.value.clone(), Tone::Normal),
                    cell(leaf.layer.clone(), Tone::Muted),
                ],
                detail: vec![format!("{}.{} = {}  [{}]", doc.name, leaf.key, leaf.value, leaf.layer).into()],
                tone: Tone::Normal,
            });
        }
        if doc.omitted > 0 {
            out.push(PreparedRow {
                key: format!("{}.…", doc.name),
                kind: RowKind::Child,
                cells: vec![
                    Cell { text: format!("… {} more", doc.omitted).into(), tone: Tone::Muted, indent: 1 },
                    cell(String::new(), Tone::Muted),
                    cell(String::new(), Tone::Muted),
                ],
                detail: Vec::new(),
                tone: Tone::Muted,
            });
        }
    }
    PreparedTable { columns: CONFIG_COLUMNS.to_vec(), rows: out }
}

pub const LOG_COLUMNS: [ColumnSpec; 4] = [
    col("time", "Time", 110.0),
    col("level", "Lvl", 60.0),
    col("target", "Target", 170.0),
    col("message", "Message", 700.0),
];

fn level_tone(level: Level) -> Tone {
    match level {
        Level::ERROR => Tone::Error,
        Level::WARN => Tone::Warn,
        Level::INFO => Tone::Normal,
        _ => Tone::Muted,
    }
}

pub fn log_table(rows: &[LogRow], lost: u64) -> PreparedTable {
    let mut out = Vec::with_capacity(rows.len() + 1);
    if lost > 0 {
        out.push(PreparedRow {
            key: "lost".into(),
            kind: RowKind::Notice,
            cells: vec![cell(format!("{lost} records lost — the ring wrapped before the last drain"), Tone::Warn)],
            detail: Vec::new(),
            tone: Tone::Warn,
        });
    }
    out.extend(rows.iter().map(|r| {
        let tone = level_tone(r.level);
        PreparedRow {
            key: r.seq.to_string(),
            kind: RowKind::Plain,
            cells: vec![
                cell(r.hms_millis.clone(), Tone::Muted),
                cell(r.level.to_string(), tone),
                cell(r.target, Tone::Muted),
                cell(r.message.clone(), tone),
            ],
            detail: vec![format!("{} {} {} {}", r.hms_millis, r.level, r.target, r.message).into()],
            tone,
        }
    }));
    PreparedTable { columns: LOG_COLUMNS.to_vec(), rows: out }
}
```

Run: `cargo test -p geode-diagnostics prepared::` → PASS.

- [ ] **Step 3: Implement `table.rs`, the delegate**

Read `crates/geode-marketdata/src/delegate.rs` lines 471–600 (a working `TableDelegate` at this version) and the registry's `table/delegate.rs` before writing. Then:

```rust
//! One `TableDelegate` for every table section, over a shared prepared
//! table. The page owns the truth (cursor, expansion, filters); the
//! delegate paints and reports clicks.

use std::rc::Rc;

use geode_shell::fonts;
use geode_shell::shell::{chip, scale};
use gpui::prelude::*;
use gpui::{App, Context, SharedString, TextAlign, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::table::{Column, TableDelegate, TableState};

use crate::model::Tone;
use crate::prepared::{PreparedTable, RowKind};

pub struct SectionDelegate {
    table: Rc<PreparedTable>,
    rem_px: f32,
}

impl SectionDelegate {
    pub fn new() -> SectionDelegate {
        SectionDelegate { table: Rc::new(PreparedTable::empty()), rem_px: 16.0 }
    }

    pub fn set(&mut self, table: Rc<PreparedTable>) {
        self.table = table;
    }

    pub fn table(&self) -> &Rc<PreparedTable> {
        &self.table
    }

    /// The page passes the window's rem so column widths follow the font
    /// size, like the sidebar width does.
    pub fn set_rem(&mut self, rem_px: f32) {
        self.rem_px = rem_px;
    }
}

impl Default for SectionDelegate {
    fn default() -> Self {
        Self::new()
    }
}

impl TableDelegate for SectionDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.table.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.table.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let spec = self.table.columns[col_ix];
        Column {
            key: SharedString::new_static(spec.key),
            name: SharedString::new_static(spec.name),
            align: if spec.right { TextAlign::Right } else { TextAlign::Left },
            sort: None,
            width: px(spec.width * self.rem_px / 16.0),
            fixed: None,
            movable: false,
            resizable: true,
            ..Column::default()
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(row) = self.table.rows.get(row_ix) else {
            return div();
        };
        // A notice row paints its one cell in the first column and nothing
        // elsewhere; the table has no colspan.
        let Some(cell) = row.cells.get(col_ix) else {
            return div();
        };
        let color = match cell.tone {
            Tone::Normal => theme.foreground,
            Tone::Muted => theme.muted_foreground,
            Tone::Warn => chip::chip_paint(theme, chip::Tone::WarningText).text,
            Tone::Error => chip::chip_paint(theme, chip::Tone::DangerText).text,
            Tone::Marked => theme.primary,
        };
        let expander = match (col_ix, row.kind) {
            (0, RowKind::Parent { expanded: true }) => "▾ ",
            (0, RowKind::Parent { expanded: false }) => "▸ ",
            _ => "",
        };
        div()
            .w_full()
            .pl(scale::design(cell.indent as f32 * 12.0))
            .font_family(fonts::MONO)
            .text_color(color)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .child(if expander.is_empty() {
                cell.text.clone()
            } else {
                SharedString::from(format!("{expander}{}", cell.text))
            })
    }
}
```

The `Column` struct's public fields are `key, name, align, sort, paddings, width, fixed, resizable, movable, selectable, min_width, max_width` (registry `column.rs`); `..Column::default()` covers the rest. `render_td` builds a `SharedString` for parent rows per paint; parents are few. Do not allocate for plain cells.

- [ ] **Step 4: Compile and commit**

Run: `cargo check -p geode-diagnostics && cargo test -p geode-diagnostics prepared::`

```bash
git add crates/geode-diagnostics
git commit -m "feat(diagnostics): prepared tables and the section table delegate

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: The page entity, factory, tile removal, and app wiring

**Files:**
- Create: `crates/geode-diagnostics/src/page.rs`
- Rewrite: `crates/geode-diagnostics/src/lib.rs`
- Delete: `crates/geode-diagnostics/src/tile.rs`, `src/commands.rs`, `src/sections.rs`
- Modify: `crates/geode-diagnostics/Cargo.toml` (add `gpui-kit-assets.workspace = true`)
- Modify: `crates/geode-app/src/main.rs`, `crates/geode-app/src/assets.rs`, `crates/geode-app/src/bridge.rs` (test ~line 4340)
- Modify: `scripts/mutation-check.sh` (delete entries anchored in `tile.rs`/`commands.rs`/`sections.rs`; Task 12 re-adds targeted ones)
- Test: `crates/geode-diagnostics/src/page.rs` tests (harness ported from the old tile tests)

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces:

```rust
pub struct DiagnosticsPage { .. }
impl DiagnosticsPage {
    pub fn new(frame, diagnostics, ring: Arc<Ring>, config: Rc<RefCell<Config>>, actions: ShellActions, restored: Option<&toml::Table>, window, cx: &mut Context<Self>) -> Self;
    pub fn section(&self) -> Section; pub fn set_section(&mut self, s: Section, cx); pub fn cursor(&self) -> usize;
    pub fn key_context(&self) -> KeyContext; pub fn dispatch(&mut self, action: &ActionId, count: Option<u32>, window, cx) -> bool;
    pub fn set_visible(&mut self, visible: bool, cx); pub fn serialize(&self) -> toml::Table; pub fn holds_focus(&self, window, cx) -> bool;
    pub fn prepared(&self) -> &Rc<PreparedTable>;
}
pub struct DiagnosticsPageFactory { .. }  // new(ring, config), set_config(config)
pub const ACTIONS: &[(&str, &str)]; pub const DEFAULT_KEYMAP: &str;
```

- [ ] **Step 1: Port the test harness and write the first failing tests in `page.rs`**

At the bottom of the new `page.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::{Level, LogLevels, Record};
    use geode_core::scopes::SavedScopes;
    use geode_shell::diagnostics::{Diagnostics, Health};
    use geode_shell::frame::Frame;
    use std::time::SystemTime;

    struct Host {
        page: Entity<DiagnosticsPage>,
    }
    impl gpui::Render for Host {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.page.clone())
        }
    }

    pub(super) struct Harness {
        pub page: Entity<DiagnosticsPage>,
        pub frame: Entity<Frame>,
        pub diagnostics: Entity<Diagnostics>,
        pub ring: Arc<Ring>,
        pub actions: Rc<RefCell<Vec<String>>>,
    }

    pub(super) fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_with(cx, None)
    }

    pub(super) fn open_with(cx: &mut gpui::TestAppContext, restored: Option<&toml::Table>) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let ring = Arc::new(Ring::new(64));
        let config = Rc::new(RefCell::new(Config::default()));
        let dispatched: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorder = dispatched.clone();
        let actions: ShellActions = Rc::new(move |a: &ActionId, _w: &mut Window, _cx: &mut App| {
            recorder.borrow_mut().push(a.0.clone());
        });
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame = cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let (ring2, config2, actions2) = (ring.clone(), config.clone(), actions.clone());
                    cx.new(|cx| {
                        let page = cx.new(|cx| {
                            DiagnosticsPage::new(frame.clone(), diagnostics.clone(), ring2, config2, actions2, restored, window, cx)
                        });
                        Host { page }
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let page = window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| h.page.clone());
        let (frame, diagnostics) = page.read_with(&vcx, |p, _| (p.frame.clone(), p.diagnostics.clone()));
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        (Harness { page, frame, diagnostics, ring, actions: dispatched }, vcx)
    }

    #[gpui::test]
    fn visibility_watches_and_requests_a_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        assert_eq!(h.diagnostics.read_with(&vcx, |d, _| d.watchers()), 1);
        assert!(h.diagnostics.update(&mut vcx, |d, _| d.take_pending_catalog_request()));
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        assert_eq!(h.diagnostics.read_with(&vcx, |d, _| d.watchers()), 0);
    }

    #[gpui::test]
    fn sections_cycle_with_the_bracket_actions_and_persist(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Sources);
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                assert!(p.dispatch(&ActionId("diagnostics::next_section".into()), None, window, cx));
            });
        });
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Data);
        let t = h.page.read_with(&vcx, |p, _| p.serialize());
        assert_eq!(t.get("section").and_then(|v| v.as_str()), Some("data"));
        assert!(t.get("filter").is_none(), "filters are transient");
    }

    #[gpui::test]
    fn an_unknown_saved_section_restores_as_sources(cx: &mut gpui::TestAppContext) {
        let mut t = toml::Table::new();
        t.insert("section".into(), toml::Value::String("database".into()));
        let (h, vcx) = open_with(cx, Some(&t));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Sources);
    }

    #[gpui::test]
    fn an_unchanged_entity_does_not_rebuild(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_health("risk", Health::Ok, String::new(), SystemTime::now());
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
        // A perf tick does not rebuild the Sources section.
        h.diagnostics.update(&mut vcx, |d, cx| {
            let mut hist = geode_shell::perf::FrameHistogram::new();
            hist.record_micros(1);
            d.refresh_frame_hist(&hist);
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
    }

    #[gpui::test]
    fn the_open_config_directory_button_goes_through_the_shell_actions_handle(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Config, cx));
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let b = vcx.debug_bounds("diagnostics-open-config-dir").expect("button painted");
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(*h.actions.borrow(), vec!["config::open_directory".to_string()]);
    }
}
```

`rebuild_count` is a `#[cfg(test)]` field as the tile had. `Frame::new`'s signature is what the old harness used; keep it.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-diagnostics page::`
Expected: FAIL to compile.

- [ ] **Step 3: Write `page.rs`**

```rust
//! The diagnostics page entity: one section at a time over the shell's
//! `Diagnostics` entity, the log ring, the loaded config, and the frame's
//! requery stats. Observers rebuild only the selected section from its own
//! inputs; the table paints a shared prepared table; the detail strip shows
//! the cursor row.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use geode_core::config::Config;
use geode_core::log::Ring;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::ShellActions;
use gpui::prelude::*;
use gpui::{App, Context, Entity, FocusHandle, SharedString, Task, Window, div};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::table::{DataTable, TableEvent, TableState};

use crate::log::{LogFilter, LogTail};
use crate::model::{self, Badges, Tone};
use crate::prepared::{self, PreparedTable, RowKind};
use crate::section::Section;
use crate::table::SectionDelegate;

fn diag_version_for(section: Section, v: geode_shell::diagnostics::DiagVersions) -> u64 {
    match section {
        Section::Sources => v.sources,
        Section::Data => v.data,
        Section::Config => v.config,
        Section::Log => v.log_levels,
        Section::Perf => v.perf,
    }
}

pub struct DiagnosticsPage {
    pub(crate) frame: Entity<Frame>,
    pub(crate) diagnostics: Entity<Diagnostics>,
    ring: Arc<Ring>,
    config: Rc<RefCell<Config>>,
    actions: ShellActions,
    focus_handle: FocusHandle,
    section: Section,
    /// Cursor row per section, kept across switches.
    cursors: [usize; 5],
    /// Filter text per section; the one input shows the selected section's.
    filters: [String; 5],
    filter_input: Entity<InputState>,
    table: Entity<TableState<SectionDelegate>>,
    prepared: Rc<PreparedTable>,
    collapsed_datasets: BTreeSet<String>,
    collapsed_docs: BTreeSet<String>,
    config_history: bool,
    log: LogTail,
    log_filter: LogFilter,
    follow: bool,
    badges: Badges,
    header_chips: Vec<(SharedString, Tone)>,
    perf: Option<model::PerfModel>,
    visible: bool,
    /// A page input holds focus; see `key_context`.
    insert_mode: bool,
    ages_timer: Option<Task<()>>,
    ages_now: SystemTime,
    last_diag_versions: geode_shell::diagnostics::DiagVersions,
    last_frame_versions: FrameVersions,
    #[cfg(test)]
    pub(crate) rebuild_count: u32,
}

impl DiagnosticsPage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        ring: Arc<Ring>,
        config: Rc<RefCell<Config>>,
        actions: ShellActions,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let section = restored
            .and_then(|t| t.get("section"))
            .and_then(|v| v.as_str())
            .and_then(Section::from_name)
            .unwrap_or(Section::Sources);

        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("filter…"));
        cx.subscribe_in(&filter_input, window, |this, input, event, _window, cx| match event {
            InputEvent::Change => {
                let text = input.read(cx).value().to_string();
                let ix = this.section as usize;
                if this.filters[ix] != text {
                    this.filters[ix] = text;
                    this.rebuild(cx);
                }
            }
            InputEvent::Focus => {
                this.insert_mode = true;
                cx.notify();
            }
            InputEvent::Blur => {
                this.insert_mode = false;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {}
        })
        .detach();

        let table = cx.new(|cx| {
            TableState::new(SectionDelegate::new(), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .col_resizable(true)
                .col_movable(false)
                .sortable(false)
                .loop_selection(false)
        });
        cx.subscribe_in(&table, window, |this, _table, event: &TableEvent, _window, cx| {
            if let TableEvent::SelectRow(ix) = event {
                this.set_cursor(*ix, cx);
            }
        })
        .detach();

        let last_diag_versions = diagnostics.read(cx).versions();
        let last_frame_versions = frame.read(cx).versions();

        cx.observe(&diagnostics, |this, diagnostics, cx| {
            let now = diagnostics.read(cx).versions();
            let relevant = if this.section == Section::Log {
                this.log.has_new() || now.log_levels != this.last_diag_versions.log_levels
            } else {
                diag_version_for(this.section, now) != diag_version_for(this.section, this.last_diag_versions)
            };
            // Badges read every counter: refresh them on any counter change.
            let any = now != this.last_diag_versions;
            this.last_diag_versions = now;
            if relevant {
                this.rebuild(cx);
            } else if any {
                this.refresh_badges(cx);
            }
        })
        .detach();
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| this.rebuild(cx)).detach();
        // The app registers its config-refresh frame observer before pages
        // are created; it must update the shared `Config` before this
        // observer rebuilds config rows on the same version change.
        cx.observe(&frame, |this, frame, cx| {
            let now = frame.read(cx).versions();
            let as_of_changed = now.as_of != this.last_frame_versions.as_of;
            let config_changed = now.config != this.last_frame_versions.config;
            let relevant = match this.section {
                Section::Data => as_of_changed,
                Section::Config => config_changed,
                Section::Sources | Section::Log | Section::Perf => false,
            };
            this.last_frame_versions = now;
            if relevant {
                this.rebuild(cx);
            }
            if as_of_changed && this.visible {
                this.diagnostics.update(cx, |d, cx| {
                    d.request_catalog_refresh();
                    cx.notify();
                });
            }
        })
        .detach();

        let log = LogTail::new(ring.clone());
        let mut this = DiagnosticsPage {
            frame,
            diagnostics,
            ring,
            config,
            actions,
            focus_handle: cx.focus_handle(),
            section,
            cursors: [0; 5],
            filters: Default::default(),
            filter_input,
            table,
            prepared: Rc::new(PreparedTable::empty()),
            collapsed_datasets: BTreeSet::new(),
            collapsed_docs: BTreeSet::new(),
            config_history: false,
            log,
            log_filter: LogFilter::all(),
            follow: true,
            badges: Badges { sources: (None, 0), datasets: 0, config: (0, 0), log_errors: 0, perf_p95: String::new() },
            header_chips: Vec::new(),
            perf: None,
            visible: false,
            insert_mode: false,
            ages_timer: None,
            ages_now: SystemTime::now(),
            last_diag_versions,
            last_frame_versions,
            #[cfg(test)]
            rebuild_count: 0,
        };
        this.rebuild(cx);
        this
    }

    pub fn section(&self) -> Section {
        self.section
    }

    pub fn cursor(&self) -> usize {
        self.cursors[self.section as usize]
    }

    pub fn prepared(&self) -> &Rc<PreparedTable> {
        &self.prepared
    }

    fn clock(cx: &App) -> geode_core::clock::Clock {
        cx.try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0)
    }

    /// Rebuild the selected section's prepared table, the badges, and the
    /// header chips. Only the selected section's builder runs.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.rebuild_count += 1;
        }
        let clock = Self::clock(cx);
        let now = SystemTime::now();
        self.ages_now = now;
        if self.section == Section::Log {
            self.log.drain();
        }
        let filter = self.filters[self.section as usize].clone();
        let prepared = {
            let d = self.diagnostics.read(cx);
            let frame = self.frame.read(cx);
            match self.section {
                Section::Sources => prepared::sources_table(&model::source_rows(d, clock), now, &filter),
                Section::Data => prepared::data_table(&model::dataset_rows(d, frame.as_of(), clock), &self.collapsed_datasets, &filter),
                Section::Config => {
                    // The diagnostics table paints in the left panel; the
                    // effective-values table is the cursor table. Both are
                    // rebuilt here; the left panel's rows are cached on self.
                    prepared::config_table(&model::config_docs(&self.config.borrow(), &filter), &self.collapsed_docs)
                }
                Section::Log => prepared::log_table(&model::log_rows(self.log.records(), &self.log_filter, clock), self.log.lost()),
                Section::Perf => {
                    self.perf = Some(model::perf_model(d, &frame.requery));
                    PreparedTable::empty()
                }
            }
        };
        self.prepared = Rc::new(prepared);
        let len = self.prepared.rows.len();
        let ix = self.section as usize;
        if self.section == Section::Log && self.follow {
            self.cursors[ix] = len.saturating_sub(1);
        } else {
            self.cursors[ix] = self.cursors[ix].min(len.saturating_sub(1));
        }
        let cursor = self.cursors[ix];
        let shared = self.prepared.clone();
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set(shared);
            t.refresh(cx);
            if len > 0 {
                t.set_selected_row(cursor, cx);
            }
        });
        self.refresh_badges(cx);
        cx.notify();
    }

    fn refresh_badges(&mut self, cx: &mut Context<Self>) {
        let clock = Self::clock(cx);
        let d = self.diagnostics.read(cx);
        let log_errors = self.log.records().filter(|r| r.level == geode_core::log::Level::ERROR).count();
        self.badges = model::badges(d, log_errors);
        self.header_chips = model::header_chips(d, clock).into_iter().map(|(s, t)| (SharedString::from(s), t)).collect();
        cx.notify();
    }

    pub fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
        self.rebuild(cx);
        self.sync_ages_timer(cx);
    }

    fn set_cursor(&mut self, ix: usize, cx: &mut Context<Self>) {
        let len = self.prepared.rows.len();
        let ix = ix.min(len.saturating_sub(1));
        let slot = self.section as usize;
        if self.cursors[slot] == ix {
            return;
        }
        self.cursors[slot] = ix;
        if self.section == Section::Log {
            self.follow = false;
        }
        self.table.update(cx, |t, cx| t.set_selected_row(ix, cx));
        cx.notify();
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let cur = self.cursor() as isize;
        let target = (cur + delta).max(0) as usize;
        self.set_cursor(target, cx);
    }

    fn toggle_expansion_at_cursor(&mut self, expand: Option<bool>, cx: &mut Context<Self>) {
        let Some(key) = self.prepared.parent_key_at(self.cursor()).map(str::to_string) else { return };
        let set = match self.section {
            Section::Data => &mut self.collapsed_datasets,
            Section::Config => &mut self.collapsed_docs,
            _ => return,
        };
        let collapsed_now = set.contains(&key);
        let collapse = match expand {
            Some(e) => !e,
            None => !collapsed_now,
        };
        if collapse { set.insert(key); } else { set.remove(&key); }
        self.rebuild(cx);
    }

    /// `mode = insert` while a page input owns focus, tracked by the input
    /// subscriptions' `Focus`/`Blur` events because the shell asks for the
    /// context without a `Window` (the market-data panel does the same
    /// with its editor flag). The shell's insert branch confirms with
    /// `holds_focus`.
    pub fn key_context(&self) -> KeyContext {
        KeyContext::new("diagnostics")
            .pair("section", self.section.name())
            .pair("mode", if self.insert_mode { "insert" } else { "normal" })
            .counts()
    }

    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.filter_input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn dispatch(&mut self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(name) = action.0.strip_prefix("diagnostics::") else { return false };
        let n = count.unwrap_or(1).max(1) as isize;
        match name {
            "down" => self.move_cursor(n, cx),
            "up" => self.move_cursor(-n, cx),
            "top" => self.set_cursor(0, cx),
            "bottom" => {
                let last = self.prepared.rows.len().saturating_sub(1);
                self.set_cursor(last, cx);
                if self.section == Section::Log {
                    self.follow = true;
                }
            }
            "page_down" => self.move_cursor(5 * n, cx),
            "page_up" => self.move_cursor(-5 * n, cx),
            "page_down_full" => self.move_cursor(10 * n, cx),
            "page_up_full" => self.move_cursor(-10 * n, cx),
            "next_section" => self.set_section(self.section.next(), cx),
            "prev_section" => self.set_section(self.section.prev(), cx),
            "expand" => self.toggle_expansion_at_cursor(Some(true), cx),
            "collapse" => self.toggle_expansion_at_cursor(Some(false), cx),
            "activate" => self.toggle_expansion_at_cursor(None, cx),
            "filter" => self.filter_input.update(cx, |i, cx| i.focus(window, cx)),
            "blur" => self.focus_handle.focus(window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.diagnostics.update(cx, |d, cx| {
            if visible { d.watch(); } else { d.unwatch(); }
            cx.notify();
        });
        if visible {
            self.rebuild(cx);
        }
        self.sync_ages_timer(cx);
    }

    pub fn serialize(&self) -> toml::Table {
        let mut t = toml::Table::new();
        t.insert("section".into(), toml::Value::String(self.section.name().to_string()));
        t
    }

    pub fn title(&self) -> SharedString {
        SharedString::from(format!("Diagnostics · {}", self.section.title()))
    }

    fn sync_ages_timer(&mut self, cx: &mut Context<Self>) {
        // Task 11 fills this in; until then no timer.
        let _ = cx;
    }
}
```

`dispatch` takes the `Window` the shell passes; tests call it as `vcx.update(|window, cx| h.page.update(cx, |p, cx| p.dispatch(&id, None, window, cx)))`. Update Task 7's `sections_cycle_with_the_bracket_actions_and_persist` test to that form.

Render (in the same file), a `gpui::Render` impl:

```rust
impl gpui::Render for DiagnosticsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the table's column widths on the rem scale.
        let rem = f32::from(window.rem_size());
        self.table.update(cx, |t, _| t.delegate_mut().set_rem(rem));
        let header = crate::page_chrome::header(&self.header_chips, cx.entity().downgrade(), cx);
        let rail = crate::page_chrome::rail(self.section, &self.badges, cx.entity().downgrade(), cx);
        let toolbar = self.render_toolbar(window, cx);
        let body: gpui::AnyElement = match self.section {
            Section::Perf => crate::perf_view::render(self.perf.as_ref(), cx.entity().downgrade(), cx).into_any_element(),
            _ => gpui_component::v_flex()
                .size_full()
                .child(
                    DataTable::new(&self.table)
                        .with_size(gpui_component::Size::XSmall)
                        .bordered(false)
                        .stripe(false),
                )
                .child(crate::page_chrome::detail_strip(self.prepared.rows.get(self.cursor()), self.section, cx))
                .into_any_element(),
        };
        gpui_component::v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .debug_selector(|| "diagnostics-page".to_string())
            .child(header)
            .child(
                gpui_component::h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(rail)
                    .child(gpui_component::v_flex().flex_1().min_w_0().child(toolbar).child(body)),
            )
    }
}
```

Create `src/page_chrome.rs` for `header`, `rail`, and `detail_strip`:
- `header(chips, weak, cx)`: `h_flex` with `.debug_selector("diagnostics-header")`, a title `"Diagnostics"` (`text_sm`, `font_weight` semibold), one chip per entry painted with `chip::chip_paint(theme, tone)` (map `Tone::Warn → chip::Tone::Warning`, `Tone::Error → chip::Tone::Danger`, else `chip::Tone::Neutral`), a spacer, and a back control: `Button::new("diagnostics-back").ghost().xsmall().icon(IconName::ChevronLeft).tooltip("Close page")` whose `on_click` calls `weak.update(cx, |p, cx| (p.actions)(&ActionId("page::close".into()), window, cx))`. The back control dispatches through the shell-actions handle: `page::close` is a shell action.
- `rail(section, badges, weak, cx)`: `v_flex().w(scale::design(120.)).border_r_1().border_color(theme.border)`, one row per `Section::ALL` with `.id(ElementId::Name(format!("diagnostics-rail-{}", s.name()).into()))`, `.debug_selector(same)`, painted through `listrow::paint_row(row, listrow::row_paint(theme), s == section)`, `on_mouse_down(Left, ...)` → `weak.update(cx, |p, cx| p.set_section(s, cx))`. The badge: Sources = a `size(scale::design(7.))` rounded-full dot in the worst health's tone + count; Data = count; Config = `"{e} · {w}"` in danger/warning text when nonzero; Log = `"err {n}"` when nonzero; Perf = p95 text.
- `detail_strip(row, section, cx)`: `v_flex().h(scale::design(64.)).flex_none().border_t_1().border_color(theme.border).px_2().py_1().font_family(fonts::MONO).text_xs().debug_selector("diagnostics-detail")`, one `div` per `row.detail` line with `.whitespace_nowrap().overflow_hidden().text_ellipsis()`; empty rows show a muted "select a row". The Log section adds a `Clipboard`-style copy button: `Button::new("diagnostics-copy").ghost().xsmall().icon(IconName::Copy)` with `on_click` writing `cx.write_to_clipboard(ClipboardItem::new_string(text))`; pass the joined detail text in.

`render_toolbar` in `page.rs` returns per section:
- Sources: `h_flex().gap_2().p_2().child(Input::new(&self.filter_input).cleanable(true).w(scale::design(240.)))`.
- Data: the input, a chip "catalog as-of = frame" (Neutral) or "catalog pending" (Warning) from `model::catalog_matches_frame`, and `Button::new("diagnostics-refresh-catalog").outline().xsmall().label("Refresh catalog")` → `diagnostics.update(cx, |d, cx| { d.request_catalog(); cx.notify(); })`, plus "Expand all"/"Collapse all" buttons that clear/fill `collapsed_datasets` from the current dataset names then `rebuild`.
- Config: `Button::new("diagnostics-diag-current").selected(!self.config_history).label("Current")`, `...-history` `.selected(self.config_history).label(format!("History ({n} batches)"))`, the key filter input, `Button::new("diagnostics-open-config-dir").outline().xsmall().label("Open config directory")` with `.debug_selector(|| "diagnostics-open-config-dir".to_string())` → `(self.actions)(&ActionId("config::open_directory".into()), window, cx)`. (The Config section's left diagnostics panel lands in Task 9; this task paints only the effective-values table.)
- Log and Perf toolbars land in Tasks 10 and 11; paint the filter input alone for Log now and nothing for Perf.

`Button` has no `debug_selector` builder; wrap the button in `div().id(..).debug_selector(..)` for the tested selectors. The `on_click` closures capture `cx.entity().downgrade()` and update through it; never capture `self`.

- [ ] **Step 4: Rewrite `lib.rs` and delete the tile**

```rust
//! The diagnostics page: five sections over the shell-owned `Diagnostics`
//! entity, the log ring, the loaded config, and the frame's requery stats.
//! Registered by the app as a `PageFactory`; reached from the sidebar, the
//! palette, the status-bar summary, and `mod+d`.

pub mod log;
pub mod model;
mod page;
mod page_chrome;
mod perf_view;
pub mod prepared;
pub mod section;
mod table;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use geode_core::config::Config;
use geode_core::log::Ring;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::{DIAGNOSTICS_PAGE_KIND, Diagnostics};
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{PageContent, PageFactory, PageOccupant, ShellActions};
use gpui::{App, Entity, SharedString, Window};

pub use page::DiagnosticsPage;

pub fn init(_cx: &mut App) {}

pub const ACTIONS: &[(&str, &str)] = &[
    ("diagnostics::down", "Cursor down"),
    ("diagnostics::up", "Cursor up"),
    ("diagnostics::top", "Cursor to top"),
    ("diagnostics::bottom", "Cursor to bottom"),
    ("diagnostics::page_down", "Half page down"),
    ("diagnostics::page_up", "Half page up"),
    ("diagnostics::page_down_full", "Page down"),
    ("diagnostics::page_up_full", "Page up"),
    ("diagnostics::next_section", "Next section"),
    ("diagnostics::prev_section", "Previous section"),
    ("diagnostics::expand", "Expand"),
    ("diagnostics::collapse", "Collapse"),
    ("diagnostics::activate", "Toggle expansion"),
    ("diagnostics::filter", "Filter"),
    ("diagnostics::blur", "Leave the filter"),
];

pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "diagnostics"
[bindings.keys]
"j" = "diagnostics::down"
"k" = "diagnostics::up"
"g g" = "diagnostics::top"
"shift+g" = "diagnostics::bottom"
"ctrl+d" = "diagnostics::page_down"
"ctrl+u" = "diagnostics::page_up"
"ctrl+f" = "diagnostics::page_down_full"
"ctrl+b" = "diagnostics::page_up_full"
"pagedown" = "diagnostics::page_down_full"
"pageup" = "diagnostics::page_up_full"
"[" = "diagnostics::prev_section"
"]" = "diagnostics::next_section"
"z o" = "diagnostics::expand"
"z c" = "diagnostics::collapse"
"enter" = "diagnostics::activate"
"/" = "diagnostics::filter"

[[bindings]]
context = "diagnostics && mode == insert"
[bindings.keys]
"escape" = "diagnostics::blur"
"#;

struct DiagnosticsContent {
    page: Entity<DiagnosticsPage>,
}

impl PageContent for DiagnosticsContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.page.read(cx).key_context()
    }
    fn dispatch(&self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut App) -> bool {
        self.page.update(cx, |p, cx| p.dispatch(action, count, window, cx))
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.page.update(cx, |p, cx| p.set_visible(visible, cx))
    }
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.page.read(cx).focus_handle()
    }
    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.page.read(cx).holds_focus(window, cx)
    }
    fn title(&self, cx: &App) -> SharedString {
        self.page.read(cx).title()
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        self.page.read(cx).serialize()
    }
}
```

`key_context` pairs `mode` from the page's `insert_mode` flag (set by the input subscriptions), the same mechanism `crates/geode-marketdata/src/tile.rs:934` uses for its editor.

```rust
pub struct DiagnosticsPageFactory {
    ring: Arc<Ring>,
    config: Rc<RefCell<Config>>,
}

impl DiagnosticsPageFactory {
    pub fn new(ring: Arc<Ring>, config: Config) -> DiagnosticsPageFactory {
        DiagnosticsPageFactory { ring, config: Rc::new(RefCell::new(config)) }
    }
    /// The app refreshes this before page frame observers rebuild config rows.
    pub fn set_config(&self, config: Config) {
        *self.config.borrow_mut() = config;
    }
}

impl PageFactory for DiagnosticsPageFactory {
    fn kind(&self) -> &'static str {
        DIAGNOSTICS_PAGE_KIND
    }
    fn title(&self) -> &'static str {
        "Diagnostics"
    }
    fn icon(&self) -> gpui_kit_assets::IconName {
        gpui_kit_assets::IconName::Activity
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            registry
                .register(ActionDef { id: ActionId(id.to_string()), title: title.to_string(), category: "Diagnostics".to_string() })
                .expect("diagnostics action ids are unique");
        }
    }
    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }
    fn toggle_binding(&self) -> Option<&'static str> {
        Some("mod+d")
    }
    fn create(
        &self,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        actions: ShellActions,
        window: &mut Window,
        cx: &mut App,
    ) -> PageOccupant {
        let (ring, config) = (self.ring.clone(), self.config.clone());
        let page = cx.new(|cx| DiagnosticsPage::new(frame, diagnostics, ring, config, actions, restored, window, cx));
        PageOccupant { kind: DIAGNOSTICS_PAGE_KIND, view: page.clone().into(), content: Box::new(DiagnosticsContent { page }) }
    }
}
```

Delete `src/tile.rs`, `src/commands.rs`, `src/sections.rs`. Add to `Cargo.toml` `[dependencies]`: `gpui-kit-assets.workspace = true`. Create `src/perf_view.rs` with a stub `pub fn render(model: Option<&PerfModel>, weak: WeakEntity<DiagnosticsPage>, cx: &mut Context<DiagnosticsPage>) -> impl IntoElement` that paints a muted "perf: no samples yet" for now (Task 11 fills it).

- [ ] **Step 5: Wire the app**

In `main.rs`:
- imports: `use geode_diagnostics::DiagnosticsPageFactory;` (replacing `DiagnosticsFactory`), `use geode_shell::defaults::register_page_actions;`, `use geode_shell::module::{ModuleRoster, PageRoster};`.
- `build_shell_services`: replace the diagnostics module registration with

```rust
    let mut pages = PageRoster::new();
    // Diagnostics needs no data handle and is always registered. Return its
    // shared factory so the window's frame-config observer can refresh it.
    let diagnostics_factory = Rc::new(DiagnosticsPageFactory::new(log_ring.clone(), config.clone()));
    pages.add(Box::new(diagnostics_factory.clone()));
```

and after `roster.register_actions(&mut registry);`:

```rust
    let page_titles: Vec<(&str, &str)> = pages.entries().map(|e| (e.kind, e.title)).collect();
    register_page_actions(&mut registry, &page_titles);
    pages.register_actions(&mut registry);
```

and fragments:

```rust
    let (mut fragments, mut frag_diags) = roster.keymap_fragments();
    let (page_fragments, page_diags) = pages.keymap_fragments();
    fragments.extend(page_fragments);
    frag_diags.extend(page_diags);
```

Add `pages,` and `restored_pages: std::collections::BTreeMap::new(),` to the literal; change the return type's last element to `Rc<DiagnosticsPageFactory>`. The frame-config observer block (lines 267–292) keeps calling `diagnostics_factory.set_config(...)` unchanged. Fix the test sites at 767–780, 808–858, 912–923, 954 (the roster no longer holds diagnostics; the full-roster test adds the page to a `PageRoster` and asserts its fragments are clean). In `assets.rs`, add `Activity` to `icon_assets!(ExtraIcons, [ Save, Activity ])`. In `bridge.rs` ~line 4340, the test that created a diagnostics tile and expected `Request::Catalog`: create the page instead (`DiagnosticsPageFactory::new(..).create(None, frame, diagnostics, Rc::new(|_, _, _| {}), window, cx)` then `occupant.content.set_visible(true, cx)`).

In `scripts/mutation-check.sh`, delete every `run_mutation` block whose file is `crates/geode-diagnostics/src/tile.rs`, `commands.rs`, or `sections.rs`, and the `open_module` entry at ~line 6680 if its test was rewritten away (Task 4 kept `open_module` for other kinds; keep that entry if its test still exists). Run `zsh scripts/mutation-check.sh --anchors-only` and fix whatever it names.

- [ ] **Step 6: Run everything**

Run: `cargo test -p geode-diagnostics && cargo test -p geode-app && cargo test -p geode-shell --features test-support && zsh scripts/mutation-check.sh --anchors-only`
Expected: PASS. Then `cargo run -p geode-app -- --demo 1000`, press `mod+d`: the page opens on Sources with the rail, header chips, and a table; `]` cycles; Escape returns. Note anything that paints wrong for the display-check list.

- [ ] **Step 7: Gates and commit**

```bash
git add -A crates/geode-diagnostics crates/geode-app scripts/mutation-check.sh
git commit -m "feat(diagnostics): the diagnostics page replaces the tile

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: Sources and Data sections complete

**Files:**
- Modify: `crates/geode-diagnostics/src/page.rs` (toolbar for Data, expansion by click, Expand/Collapse all)
- Test: `crates/geode-diagnostics/src/page.rs` tests

- [ ] **Step 1: Failing tests**

```rust
    #[gpui::test]
    fn resolved_markers_wait_for_a_matching_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| { p.set_visible(true, cx); p.set_section(Section::Data, cx); });
        let at = geode_core::query::AsOf::At(chrono::Utc::now());
        h.frame.update(&mut vcx, |f, cx| { let _ = f.set_as_of(at.clone()); cx.notify(); });
        vcx.run_until_parked();
        // A watched refresh was requested for the new as-of.
        assert!(h.diagnostics.update(&mut vcx, |d, _| d.take_pending_catalog_request()));
        // Catalog for another as-of: no markers.
        let other = geode_core::query::AsOf::At(chrono::Utc::now() + chrono::Duration::hours(2));
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_catalog(snapshot_with(other, vec![crate::model::tests::dataset_catalog()]), SystemTime::now());
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(h.page.read_with(&vcx, |p, _| p.prepared().rows.iter().all(|r| r.tone != Tone::Marked)));
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_catalog(snapshot_with(at, vec![crate::model::tests::dataset_catalog()]), SystemTime::now());
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.iter().filter(|r| r.tone == Tone::Marked).count()), 1);
    }

    #[gpui::test]
    fn clicking_a_dataset_row_then_enter_collapses_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_catalog(snapshot_with(geode_core::query::AsOf::Live, vec![crate::model::tests::dataset_catalog()]), SystemTime::now());
            cx.notify();
        });
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Data, cx));
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 3, "parent + 2 generations");
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| { let _ = p.dispatch(&ActionId("diagnostics::activate".into()), None, window, cx); });
        });
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 1);
        // Collapse-all / expand-all buttons.
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let b = vcx.debug_bounds("diagnostics-expand-all").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 3);
    }

    #[gpui::test]
    fn the_refresh_button_records_an_explicit_catalog_request(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Data, cx));
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let b = vcx.debug_bounds("diagnostics-refresh-catalog").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(h.diagnostics.update(&mut vcx, |d, _| d.take_catalog_request()), Some(geode_shell::diagnostics::CatalogRequest::Explicit));
    }
```

Add a test-module helper `snapshot_with(as_of, datasets) -> CatalogSnapshot` (all resource fields zero, `threads: 1`, `identities` empty). `Frame::set_as_of(&mut self, AsOf) -> bool` is at `frame.rs:525`; `model::tests::dataset_catalog` is `pub(crate)` from Task 5.

- [ ] **Step 2: Implement the Data toolbar and click expansion**

In `page.rs`: the `TableEvent::DoubleClickedRow(ix)` arm toggles expansion at `ix` (set cursor, then `toggle_expansion_at_cursor(None)`); the Data toolbar's buttons `diagnostics-expand-all` (clears `collapsed_datasets`) and `diagnostics-collapse-all` (inserts every dataset name from `diagnostics.read(cx).datasets.keys()`), each followed by `rebuild`. Single click stays selection only, so a click never surprises with a layout change.

- [ ] **Step 3: Run, gates, commit**

Run: `cargo test -p geode-diagnostics page::`

```bash
git add crates/geode-diagnostics
git commit -m "feat(diagnostics): data section expansion, refresh, and resolved markers

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: Config section: diagnostics panel with history, beside effective values

**Files:**
- Modify: `crates/geode-diagnostics/src/page.rs`
- Modify: `crates/geode-diagnostics/src/page_chrome.rs` (a second, non-cursor table for the diagnostics panel)
- Test: `crates/geode-diagnostics/src/page.rs` tests

**Interfaces:**
- Consumes: `model::{current_diagnostics, history_diagnostics}`, `prepared::diagnostics_table`, `SectionDelegate`.
- Produces: a second `Entity<TableState<SectionDelegate>>` field `diag_table` on the page, painted only in Config; `config_history: bool` toggled by the Current/History buttons.

- [ ] **Step 1: Failing tests**

```rust
    #[gpui::test]
    fn the_config_section_paints_current_diagnostics_and_switches_to_history(cx: &mut gpui::TestAppContext) {
        use geode_core::config::{Diagnostic, Severity};
        let (h, mut vcx) = open(cx);
        let t0 = SystemTime::now();
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_config(vec![Diagnostic { severity: Severity::Error, layer: None, file: Some("views.toml".into()), message: "first".into(), path: Some("blotter.columns.4".into()) }], t0);
            d.note_config(vec![Diagnostic { severity: Severity::Warning, layer: None, file: None, message: "second".into(), path: None }], t0 + std::time::Duration::from_secs(5));
            cx.notify();
        });
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Config, cx));
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let current = h.page.read_with(&vcx, |p, cx| p.diag_table.read(cx).delegate().table().clone());
        assert_eq!(current.rows.len(), 1);
        assert_eq!(current.rows[0].cells[3].text.as_ref(), "second");
        let b = vcx.debug_bounds("diagnostics-diag-history").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        let history = h.page.read_with(&vcx, |p, cx| p.diag_table.read(cx).delegate().table().clone());
        assert_eq!(history.columns.len(), 5, "batch column first");
        assert_eq!(history.rows[0].cells[4].text.as_ref(), "first");
        assert_eq!(history.rows[0].cells[3].text.as_ref(), "views.toml › blotter.columns.4");
    }

    #[gpui::test]
    fn a_config_version_change_rebuilds_only_the_config_section(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Config, cx));
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.frame.update(&mut vcx, |f, cx| { f.note_config_reloaded(); cx.notify(); });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Sources, cx));
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.frame.update(&mut vcx, |f, cx| { f.note_config_reloaded(); cx.notify(); });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before, "sources ignore a config bump");
    }
```

`Frame::note_config_reloaded` (`frame.rs:691`) bumps the config version.

- [ ] **Step 2: Implement**

- Add `diag_table: Entity<TableState<SectionDelegate>>` built like `table` in `new` (no subscription; its selection drives the detail strip only when the pointer is in the left panel: subscribe `TableEvent::SelectRow` to set `diag_cursor: usize`).
- In `rebuild` for `Section::Config`, also compute `let diags = if self.config_history { model::history_diagnostics(d, clock) } else { model::current_diagnostics(d) };` and `diag_table.update(cx, |t, cx| { t.delegate_mut().set(Rc::new(prepared::diagnostics_table(&diags, self.config_history))); t.refresh(cx); })`.
- Render for Config: `h_flex().flex_1().min_h_0()` with the left panel (`v_flex().w_1_2().min_w_0().border_r_1()` holding the Current/History buttons, `DataTable::new(&self.diag_table)`, and its own detail strip fed from `diag_cursor`) and the right panel (the key filter input, "Open config directory", `DataTable::new(&self.table)`, and the cursor detail strip). Keys (`j`/`k`, expand) drive the right table; the left is pointer-only, which the rail's help copy states.
- `config_history` toggles rebuild; the History button's label uses `d.config_history.len().saturating_sub(1)`.

- [ ] **Step 3: Run, gates, commit**

```bash
git add crates/geode-diagnostics
git commit -m "feat(diagnostics): config section: diagnostics panel with history beside effective values

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: Log section: level toggles, target select, follow, clear, copy, and the Levels popover

**Files:**
- Create: `crates/geode-diagnostics/src/levels.rs`
- Modify: `crates/geode-diagnostics/src/page.rs`
- Modify: `crates/geode-diagnostics/src/page_chrome.rs` (copy button on the detail strip)
- Test: `levels.rs` inline tests; `page.rs` tests

**Interfaces:**
- Consumes: registry `select.rs` (`SelectState::new(delegate, selected_index, window, cx)`, `SearchableVec`, `SelectEvent::Confirm`), `switch.rs` (`Switch::new(id).checked(b).label(..).on_change(|&bool, window, cx|)`), `popover.rs` (`Popover::new(id).trigger(..).open(bool).on_open_change(..).content(|state, window, cx| ..)`), `Diagnostics::{levels, request_level}`, `geode_core::log::TARGETS`.
- Produces:

```rust
// levels.rs (pure)
pub struct LevelsState { pub open: bool, pub new_target: String }
pub struct LevelRow { pub target: String, pub effective: Level, pub explicit: bool }
pub fn level_rows(levels: &LogLevels) -> Vec<LevelRow>;   // "default" first, then TARGETS stripped of "geode::", then any extra configured target
pub fn effective_level(levels: &LogLevels, target: &str) -> Level;
pub fn level_word(level: Level) -> &'static str;           // "error".."trace"
```

- [ ] **Step 1: `levels.rs` with tests**

```rust
//! The Levels popover's pure state: which targets to list and their
//! effective level, and what a pick requests.

use geode_core::log::{Level, LogLevels, TARGETS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelRow {
    /// "default" or the target with its `geode::` prefix stripped.
    pub target: String,
    pub effective: Level,
    /// Whether `levels.targets` names it explicitly (else it inherits).
    pub explicit: bool,
}

pub fn level_word(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// The level a target logs at: its explicit entry, else the longest
/// configured prefix, else the default.
pub fn effective_level(levels: &LogLevels, target: &str) -> Level {
    let full = if target.starts_with("geode::") { target.to_string() } else { format!("geode::{target}") };
    levels
        .targets
        .iter()
        .filter(|(t, _)| full == *t || full.starts_with(&format!("{t}::")))
        .max_by_key(|(t, _)| t.len())
        .map(|(_, l)| *l)
        .unwrap_or(levels.default)
}

pub fn level_rows(levels: &LogLevels) -> Vec<LevelRow> {
    let mut rows = vec![LevelRow { target: "default".into(), effective: levels.default, explicit: true }];
    let mut names: Vec<String> = TARGETS.iter().map(|t| t.strip_prefix("geode::").unwrap_or(t).to_string()).collect();
    for (t, _) in &levels.targets {
        let short = t.strip_prefix("geode::").unwrap_or(t).to_string();
        if !names.contains(&short) {
            names.push(short);
        }
    }
    for name in names {
        let full = format!("geode::{name}");
        rows.push(LevelRow {
            explicit: levels.targets.iter().any(|(t, _)| *t == full || *t == name),
            effective: effective_level(levels, &name),
            target: name,
        });
    }
    rows
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LevelsState {
    pub open: bool,
    pub new_target: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_list_default_then_known_targets_then_extras_with_inheritance() {
        let levels = LogLevels { default: Level::INFO, targets: vec![("geode::ingest".into(), Level::DEBUG), ("geode::custom".into(), Level::TRACE)] };
        let rows = level_rows(&levels);
        assert_eq!(rows[0], LevelRow { target: "default".into(), effective: Level::INFO, explicit: true });
        let ingest = rows.iter().find(|r| r.target == "ingest").unwrap();
        assert_eq!((ingest.effective, ingest.explicit), (Level::DEBUG, true));
        let query = rows.iter().find(|r| r.target == "query").unwrap();
        assert_eq!((query.effective, query.explicit), (Level::INFO, false));
        assert_eq!(rows.last().unwrap().target, "custom");
        assert_eq!(effective_level(&levels, "geode::ingest::csv"), Level::DEBUG, "prefix inherits");
    }
}
```

Check how the existing `LogLevels::with` spells targets (with or without the `geode::` prefix) by reading `crates/geode-core/src/log/mod.rs:297` and `choicedialog.rs`'s `effective_level`; match that spelling in `effective_level` and in what the page passes to `request_level`.

- [ ] **Step 2: Failing page tests**

```rust
    fn push(ring: &Ring, level: Level, target: &'static str, msg: &str) {
        ring.push(Record { at: SystemTime::now(), level, target, message: msg.into(), seq: 0 });
    }

    #[gpui::test]
    fn the_log_section_follows_until_the_cursor_moves_and_bottom_resumes(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Log, cx));
        for i in 0..5 { push(&h.ring, Level::INFO, "geode::shell", &format!("m{i}")); }
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.cursor()), 4);
        vcx.update(|window, cx| h.page.update(cx, |p, cx| { let _ = p.dispatch(&ActionId("diagnostics::up".into()), None, window, cx); }));
        push(&h.ring, Level::INFO, "geode::shell", "m5");
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.cursor()), 3, "not following");
        vcx.update(|window, cx| h.page.update(cx, |p, cx| { let _ = p.dispatch(&ActionId("diagnostics::bottom".into()), None, window, cx); }));
        push(&h.ring, Level::INFO, "geode::shell", "m6");
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.cursor()), 6, "following again");
    }

    #[gpui::test]
    fn level_toggles_and_the_target_select_filter_the_tail(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Log, cx));
        push(&h.ring, Level::DEBUG, "geode::query", "planned");
        push(&h.ring, Level::ERROR, "geode::shell", "boom");
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 2);
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let b = vcx.debug_bounds("diagnostics-level-DEBUG").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 1);
        h.page.update(&mut vcx, |p, cx| p.set_log_target(Some("geode::query".into()), cx));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 0, "DEBUG off and only query");
    }

    #[gpui::test]
    fn a_wrap_while_closed_is_reported_on_the_next_drain(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);   // ring capacity 64
        h.page.update(&mut vcx, |p, cx| { p.set_section(Section::Log, cx); p.set_visible(false, cx); });
        for i in 0..100 { push(&h.ring, Level::WARN, "geode::ingest", &format!("w{i}")); }
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        let rows = h.page.read_with(&vcx, |p, _| p.prepared().clone());
        assert!(matches!(rows.rows[0].kind, RowKind::Notice));
        assert!(rows.rows[0].cells[0].text.contains("36 records lost"), "{}", rows.rows[0].cells[0].text);
        assert_eq!(rows.rows.len(), 65);
    }

    #[gpui::test]
    fn clear_drops_the_retained_tail_and_keeps_draining(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Log, cx));
        push(&h.ring, Level::INFO, "geode::shell", "a");
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let b = vcx.debug_bounds("diagnostics-log-clear").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 0);
        push(&h.ring, Level::INFO, "geode::shell", "b");
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 1);
    }

    #[gpui::test]
    fn a_levels_pick_requests_the_level_through_the_entity(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Log, cx));
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let b = vcx.debug_bounds("diagnostics-levels-open").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let pick = vcx.debug_bounds("diagnostics-level-pick-ingest-debug").expect("popover row painted");
        vcx.simulate_click(pick.center(), gpui::Modifiers::default());
        let pending = h.diagnostics.update(&mut vcx, |d, _| d.take_pending_level());
        assert_eq!(pending, Some(("geode::ingest".to_string(), Level::DEBUG)));
        assert_eq!(h.diagnostics.read_with(&vcx, |d, _| crate::levels::effective_level(&d.levels, "ingest")), Level::DEBUG);
    }
```

`set_log_target` is a `pub` page method the Select subscription also calls; the test drives it directly because a `Select`'s popup cannot be clicked headlessly with confidence. The target spelling in `take_pending_level`'s expectation must match what `LogLevels::with` stores (Step 1's check).

- [ ] **Step 3: Implement the Log toolbar, popover, and copy**

Fields on the page: `target_select: Entity<SelectState<SearchableVec<SharedString>>>` (items: `"all"` then `model::log_targets(self.log.records())`; refreshed in `rebuild` for Log when the set changed, via `set_items`), `levels: LevelsState`, `new_target_input: Entity<InputState>`.

Toolbar (`Section::Log`):
- One `Button::new(("diagnostics-level", ix))` per `LEVELS` entry, `.xsmall().label(level.to_string()).selected(self.log_filter.levels[ix])`, wrapped in `div().id(..).debug_selector(|| format!("diagnostics-level-{level}"))`, `on_click` → flip `log_filter.levels[ix]`, rebuild.
- `Select::new(&self.target_select).xsmall().placeholder("target")`; subscribe `SelectEvent::Confirm(value)` → `set_log_target(value.filter(|v| v.as_ref() != "all").map(|v| v.to_string()))`.
- The message filter `Input`.
- `Switch::new("diagnostics-follow").checked(self.follow).label("Follow").on_change(..)` → set `follow`, and when turning on jump to the last row (same as `bottom`).
- `Button::new("diagnostics-log-clear").outline().xsmall().label("Clear")` → `self.log.clear()`, rebuild.
- `Popover::new("diagnostics-levels").trigger(Button::new("diagnostics-levels-open").outline().xsmall().label("Levels…")).open(self.levels.open).on_open_change(..)` → set `levels.open`; `.content(move |_, _, cx| ...)` builds a `v_flex` of `level_rows(&diagnostics.read(cx).levels)` rows: the target name, then five `Button::new(("diagnostics-level-pick", row_ix * 8 + level_ix)).xsmall().ghost().label(level_word(l)).selected(row.effective == l)` each wrapped in a `div().id(..).debug_selector(|| format!("diagnostics-level-pick-{target}-{word}"))`; `on_click` → `diagnostics.update(cx, |d, cx| { d.request_level(&full_target, l); cx.notify(); })` where `full_target` is `"geode::{target}"` (or the default's spelling: check what `choicedialog.rs` passes for the default row). A last row holds `Input::new(&self.new_target_input).placeholder("new target")` and the five level buttons for it, enabled when the input is nonempty. The popover content closure receives no `Context<DiagnosticsPage>`; capture `self.diagnostics.clone()` and `cx.entity().downgrade()` before building.
- `holds_focus` must also report the `new_target_input`, and `insert_mode` must follow both inputs' focus events.

Detail strip for Log gets the Copy button (Task 7's `detail_strip` signature takes an `Option<SharedString>` to copy; pass the joined detail for Log, `None` elsewhere).

- [ ] **Step 4: Run, gates, commit**

Run: `cargo test -p geode-diagnostics levels:: page::`

```bash
git add crates/geode-diagnostics
git commit -m "feat(diagnostics): log section filters, follow, clear, copy, and the Levels popover

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 11: Perf section and the Sources ages timer

**Files:**
- Modify: `crates/geode-diagnostics/src/perf_view.rs`
- Modify: `crates/geode-diagnostics/src/page.rs` (`sync_ages_timer`, overlay switch handler)
- Test: `perf_view.rs` inline tests (pure bucket geometry); `page.rs` tests

**Interfaces:**
- Consumes: `model::{PerfModel, FRAME_BUDGET_MICROS, REQUERY_BUDGET_MICROS}`, `Diagnostics::request_overlay_toggle`, `Switch`.
- Produces: `perf_view::render(model, weak, cx)`, `perf_view::bar_heights(buckets, max_height) -> Vec<(f32, bool /*over budget*/)>`.

- [ ] **Step 1: Failing tests**

In `perf_view.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_heights_scale_to_the_tallest_bucket_and_tint_past_the_budget() {
        let buckets = vec![(1_000u64, 2u32), (8_000, 4), (10_000, 1), (u64::MAX, 0)];
        let bars = bar_heights(&buckets, 40.0);
        assert_eq!(bars[1].0, 40.0);
        assert_eq!(bars[0].0, 20.0);
        assert!(!bars[1].1, "8 ms is the budget's own bucket: within");
        assert!(bars[2].1, "past the budget");
        assert_eq!(bars[3].0, 0.0);
        assert!(bar_heights(&[], 40.0).is_empty());
    }
}
```

In `page.rs` tests:

```rust
    #[gpui::test]
    fn the_overlay_switch_flips_through_the_entity_channel(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| { p.set_visible(true, cx); p.set_section(Section::Perf, cx); });
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let b = vcx.debug_bounds("diagnostics-overlay-switch").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert!(h.diagnostics.update(&mut vcx, |d, _| d.take_pending_overlay_toggle()));
        // The shell mirrors the value back; the page repaints on the perf counter.
        h.diagnostics.update(&mut vcx, |d, cx| { d.set_overlay_visible(true); cx.notify(); });
        vcx.run_until_parked();
        assert!(h.page.read_with(&vcx, |p, _| p.perf.as_ref().unwrap().overlay));
    }

    #[gpui::test]
    fn the_ages_timer_runs_only_while_visible_on_sources(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert!(h.page.read_with(&vcx, |p, _| p.ages_timer.is_none()), "hidden: no timer");
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        assert!(h.page.read_with(&vcx, |p, _| p.ages_timer.is_some()));
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Log, cx));
        assert!(h.page.read_with(&vcx, |p, _| p.ages_timer.is_none()), "log: no timer");
        h.page.update(&mut vcx, |p, cx| p.set_section(Section::Sources, cx));
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        assert!(h.page.read_with(&vcx, |p, _| p.ages_timer.is_none()));
    }

    #[gpui::test]
    fn a_tick_refreshes_ages_without_rebuilding_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| { d.note_health("s", Health::Ok, String::new(), SystemTime::now()); cx.notify(); });
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.page.update(&mut vcx, |p, cx| p.tick_ages(cx));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before);
        let since = h.page.read_with(&vcx, |p, _| p.prepared().rows[0].cells[2].text.clone());
        assert!(since.ends_with(" s"), "{since}");
    }
```

- [ ] **Step 2: Implement `perf_view.rs`**

```rust
//! The Perf section: stat tiles with their budgets, a frame-interval
//! histogram as a row of themed bars, database tiles, and the overlay switch.

use geode_shell::fonts;
use geode_shell::shell::{chip, scale};
use gpui::prelude::*;
use gpui::{AnyElement, Context, WeakEntity, div};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::model::{FRAME_BUDGET_MICROS, PerfModel, REQUERY_BUDGET_MICROS, Percentiles};
use crate::page::DiagnosticsPage;

const HISTOGRAM_HEIGHT: f32 = 56.0;

/// Bar heights in design px scaled to the tallest bucket, and whether the
/// bucket's upper bound is past the frame budget.
pub fn bar_heights(buckets: &[(u64, u32)], max_height: f32) -> Vec<(f32, bool)> {
    let tallest = buckets.iter().map(|(_, n)| *n).max().unwrap_or(0);
    buckets
        .iter()
        .map(|(bound, n)| {
            let h = if tallest == 0 { 0.0 } else { max_height * (*n as f32) / (tallest as f32) };
            (h, *bound > FRAME_BUDGET_MICROS)
        })
        .collect()
}

fn tile(label: &'static str, value: String, detail: String, cx: &Context<DiagnosticsPage>) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .flex_1()
        .min_w_0()
        .p_2()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .child(div().text_xs().text_color(theme.muted_foreground).child(label))
        .child(div().text_sm().font_family(fonts::MONO).child(value))
        .child(div().text_xs().text_color(theme.muted_foreground).child(detail))
        .into_any_element()
}

fn pct(p: Option<&Percentiles>) -> String {
    match p {
        Some(p) => format!("{} · {} · {}", p.p50, p.p95, p.max),
        None => "no samples yet".to_string(),
    }
}

pub fn render(model: Option<&PerfModel>, weak: WeakEntity<DiagnosticsPage>, cx: &mut Context<DiagnosticsPage>) -> impl IntoElement {
    let theme = cx.theme();
    let Some(m) = model else {
        return v_flex().p_2().text_color(theme.muted_foreground).child("perf: no samples yet");
    };
    let bars = bar_heights(&m.buckets, HISTOGRAM_HEIGHT);
    let over = chip::chip_paint(theme, chip::Tone::WarningText).text;
    let histogram = h_flex()
        .items_end()
        .gap_px()
        .h(scale::design(HISTOGRAM_HEIGHT))
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "diagnostics-histogram".to_string())
        .children(bars.iter().enumerate().map(|(i, (h, past))| {
            div()
                .id(("diagnostics-bar", i))
                .w(scale::design(6.0))
                .h(scale::design(*h))
                .bg(if *past { over } else { theme.primary })
        }));
    let diagnostics = weak.clone();
    let overlay = div()
        .id("diagnostics-overlay-switch")
        .debug_selector(|| "diagnostics-overlay-switch".to_string())
        .child(
            Switch::new("diagnostics-overlay")
                .checked(m.overlay)
                .label("Performance overlay")
                .on_change(move |_checked, _window, cx| {
                    let _ = diagnostics.update(cx, |p, cx| {
                        p.diagnostics.update(cx, |d, cx| {
                            d.request_overlay_toggle();
                            cx.notify();
                        });
                    });
                }),
        );
    v_flex()
        .p_2()
        .gap_2()
        .child(h_flex().justify_end().child(overlay))
        .child(
            h_flex().gap_2().child(tile("Frame p50 · p95 · max", pct(m.frame.as_ref()), format!("n = {} · budget {} ms", m.frame_count, FRAME_BUDGET_MICROS / 1_000), cx))
                .child(tile("Requery submit→snapshot", pct(m.submit.as_ref()), format!("budget {} ms at 1M rows", REQUERY_BUDGET_MICROS / 1_000), cx))
                .child(tile("Requery snapshot→paint", pct(m.paint.as_ref()), String::new(), cx))
                .child(tile("Dropped events", m.dropped.to_string(), "since start".into(), cx)),
        )
        .child(div().text_xs().text_color(theme.muted_foreground).child("Frame interval histogram (bars past the 8 ms budget tinted)"))
        .child(histogram)
        .child(
            h_flex().gap_2().child(tile("Database", m.database.clone(), format!("used {} · block {}", m.used, m.block_size), cx))
                .child(tile("DuckDB memory", m.memory.clone(), format!("threads {}", m.threads), cx)),
        )
}
```

`p.diagnostics` is `pub(crate)` on the page (Task 7 declared it so). `theme.primary` for bars is the established accent; the tinted bars use the warning text color from `chip_paint`. Check the `Switch::on_change` closure signature in the registry (`Fn(&bool, &mut Window, &mut App)`).

- [ ] **Step 3: The ages timer**

In `page.rs`:

```rust
    /// One-second ticks while the page is visible and Sources is selected.
    /// A tick refreshes age text in place; it never runs a section builder.
    fn sync_ages_timer(&mut self, cx: &mut Context<Self>) {
        let wanted = self.visible && self.section == Section::Sources;
        if !wanted {
            self.ages_timer = None;
            return;
        }
        if self.ages_timer.is_some() {
            return;
        }
        self.ages_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |page, cx| page.tick_ages(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Rewrite the Since cells from the retained `since` times. The prepared
    /// table is cloned-on-write only when an age text changed.
    pub(crate) fn tick_ages(&mut self, cx: &mut Context<Self>) {
        if self.section != Section::Sources {
            return;
        }
        let now = SystemTime::now();
        self.ages_now = now;
        let mut changed = false;
        let mut table = (*self.prepared).clone();
        for (row, since) in table.rows.iter_mut().zip(self.source_since.iter()) {
            let Some(since) = since else { continue };
            let text: SharedString = format!("{} · {}", row.cells[2].text.split(" · ").next().unwrap_or(""), model::age_text(Some(*since), now)).into();
            if row.cells[2].text != text {
                row.cells[2].text = text;
                changed = true;
            }
        }
        if changed {
            let shared = Rc::new(table);
            self.prepared = shared.clone();
            self.table.update(cx, |t, cx| { t.delegate_mut().set(shared); t.refresh(cx); });
            cx.notify();
        }
    }
```

Keep `source_since: Vec<Option<SystemTime>>` on the page, filled in `rebuild` for Sources from the typed rows in prepared order (so it stays aligned with `prepared.rows` after filtering: build it from the same filtered iteration; the simplest way is for `prepared::sources_table` to return the `since` list alongside, `(PreparedTable, Vec<Option<SystemTime>>)`; adjust its tests). Call `sync_ages_timer` from `set_visible` and `set_section` (Task 7 left the stub). The `cx.spawn` closure form (`async move |this, cx|`) is the one `geode-shell/src/shell/mod.rs`'s poll loop uses at ~line 928; copy its exact shape.

- [ ] **Step 4: Run, gates, commit**

Run: `cargo test -p geode-diagnostics`

```bash
git add crates/geode-diagnostics
git commit -m "feat(diagnostics): perf section tiles, histogram, overlay switch, and the ages timer

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 12: Documentation, mutation entries, measurement, and the display-check list

**Files:**
- Modify: `docs/current/shell.md` ("Module hosting and delivery", "Diagnostics state and demand", "Session format" table)
- Modify: `docs/current/features.md` ("Diagnostics")
- Modify: `docs/current/input-and-dialogs.md` ("Keyboard ownership")
- Modify: `docs/current/keymaps.md` (contexts paragraph)
- Rewrite: `crates/geode-diagnostics/README.md`
- Modify: `crates/geode-shell/README.md` (Layout table: `shell/page.rs`; Rules)
- Modify: `docs/perf.md` (one measurement)
- Modify: `scripts/mutation-check.sh`

- [ ] **Step 1: Mutation entries**

Append, in the diagnostics block, one `run_mutation` per contract. Each anchor must match the source text exactly (copy it from the file after Task 11) and each names its test:

| Label | File | Anchor (the line to break) | Replacement | Test |
|---|---|---|---|---|
| page seam: open never watches | `crates/geode-diagnostics/src/page.rs` | `if visible { d.watch(); } else { d.unwatch(); }` | `if visible { d.unwatch(); } else { d.unwatch(); }` | `visibility_watches_and_requests_a_catalog` |
| shell page: escape under a modal closes the page | `crates/geode-shell/src/shell/input.rs` | the `if self.modal_open() { self.notice = Some(CLOSE_DIALOG_FIRST); return; }` inside the `page::toggle_` branch | delete the guard (replace with empty) | `toggle_under_a_modal_is_refused` |
| shell page: a workspace switch leaves the page open | `crates/geode-shell/src/shell/input.rs` | `if self.page_open() && action.0.starts_with("workspace::switch_") {` | `if false {` | `a_workspace_switch_closes_the_page` |
| shell page: tiles beneath stay visible | `crates/geode-shell/src/shell/occupants.rs` | the `if self.page_open() { return; }` in `fill_active_tiles` | `if false { return; }` | `tiles_beneath_are_hidden_on_open_and_shown_on_close` |
| shell page: session omits pages | `crates/geode-shell/src/session.rs` | `if !pages.is_empty() {` | `if false {` | `pages_round_trip_and_an_unknown_kind_is_kept` |
| diagnostics page: badges ignore warnings | `crates/geode-diagnostics/src/model.rs` | `.filter(\|r\| r.severity == Severity::Warning).count();` | `.count() * 0;` | `badges_count_errors_and_warnings_and_worst_health` |
| diagnostics page: a levels pick requests nothing | `crates/geode-diagnostics/src/page.rs` | `d.request_level(&full_target, l);` | `let _ = (&full_target, l);` | `a_levels_pick_requests_the_level_through_the_entity` |
| shell: the overlay mirror never updates | `crates/geode-shell/src/shell/mod.rs` | `if d.set_overlay_visible(visible) {` | `if false {` | `the_overlay_mirror_follows_a_keyboard_toggle` |
| diagnostics log: loss gap is never measured | `crates/geode-diagnostics/src/log.rs` | `.map(\|oldest\| oldest.saturating_sub(self.since + 1))` | `.map(\|_\| 0)` | `a_wrap_between_drains_reports_the_gap_measured_at_that_drain` |
| diagnostics data: markers ignore the catalog as-of | `crates/geode-diagnostics/src/model.rs` | `let matches = catalog_matches_frame(d, as_of);` | `let matches = true;` | `resolved_markers_need_a_matching_catalog_and_a_historical_frame` |
| diagnostics page: follow never stops | `crates/geode-diagnostics/src/page.rs` | `if self.section == Section::Log {\n            self.follow = false;\n        }` in `set_cursor` | delete | `the_log_section_follows_until_the_cursor_moves_and_bottom_resumes` |
| diagnostics page: the ages timer runs on every section | `crates/geode-diagnostics/src/page.rs` | `let wanted = self.visible && self.section == Section::Sources;` | `let wanted = self.visible;` | `the_ages_timer_runs_only_while_visible_on_sources` |

Run `zsh scripts/mutation-check.sh --anchors-only`, then `zsh scripts/mutation-check.sh "shell page"` and `zsh scripts/mutation-check.sh "diagnostics"` and confirm every entry reports KILLED. Commit before running: the harness edits tracked files in place.

- [ ] **Step 2: Documentation**

- `docs/current/shell.md`: under "Module hosting and delivery" add a "Pages" subsection: the `PageContent`/`PageFactory` seam, one page at a time retained for the window's lifetime, what is and is not painted, the `page` context stack, modal and palette precedence over `page::close`, the insert branch for page inputs, tiles beneath hidden so no barrier waits on them, `[pages.<kind>]`, and the failure semantics (unknown kind on toggle: warning, nothing opens; unknown saved table: kept). Rewrite "Diagnostics state and demand"'s tile wording to "the diagnostics page"; add the overlay mirror and `catalog_at`. Add the `pages.<kind>` row to the "Session format" table.
- `docs/current/features.md`: replace the Diagnostics entry: the page, the rail and detail strip, the five sections' tables and toolbars, which controls change application state and through which door, the retained tail and loss-gap rule, the ages timer, and limits (uniform row heights, left Config panel is pointer-only, sorting only where defined).
- `docs/current/input-and-dialogs.md` "Keyboard ownership": the page context stack, `mod+d`, Escape order.
- `docs/current/keymaps.md`: add `page` to the contexts paragraph and the toggle fragment mechanism.
- `crates/geode-diagnostics/README.md`: rewrite the module table (`lib`, `section`, `model`, `prepared`, `table`, `page`, `page_chrome`, `log`, `levels`, `perf_view`), interaction, persistence, rules pinned (observer inputs per section, watch/unwatch, `Rc` sharing, 4,096 cap and gap rule, tone door, no `ShellView`), and limits.
- `crates/geode-shell/README.md`: add `shell/page.rs` to the layout table; add the rule "a page hides the tiles beneath it; `visible_tile_keys` is empty while one is open".
- `docs/perf.md`: record one measurement: page render with a 4,096-record tail, method (perf overlay p95 while holding `j`), hardware, and the reading. Target inside 8 ms. If it misses, record it and open a follow-up item in the handoff; do not tune blind.

- [ ] **Step 3: Full gates**

Run every CLAUDE.md command:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench --workspace --no-run
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed
```

- [ ] **Step 4: Commit**

```bash
git add docs crates/geode-diagnostics/README.md crates/geode-shell/README.md scripts/mutation-check.sh
git commit -m "docs: the diagnostics page and the shell page seam

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

- [ ] **Step 5: Display checks for the user's screen (not automatable)**

Run `cargo run -p geode-app -- --demo 100000` and check:
1. Sidebar: the diagnostics glyph sits above the settings avatar, hover state on it, active treatment while the page is open.
2. Header chips read correctly with one degraded source and one config error; the back chevron closes.
3. Rail badges: dot color, counts, p95; the selected row treatment.
4. Sources, Data, Log, Config tables at 1200 px and 800 px window widths: column alignment, ellipsis on the message column, no horizontal page scroll.
5. Detail strip: two and three lines fit; the Log copy button.
6. Histogram: bars tint past the budget line; tiles read in both a light and a dark theme.
7. Levels popover: anchored to its button, closes on outside click, the picked level highlights.
8. `mod+d` from a blotter with a focused cell, then Escape: focus returns to the blotter and it repaints without a jog.

---

## Self-review

**Spec coverage.** §2.1 traits → Task 1. §2.2 actions and bindings → Tasks 1, 7. §2.3 state and routing (retain, toggle under modal, switch closes, context stack, escape order, tiles hidden, dispatch fallthrough) → Tasks 2, 4. §2.4 render → Task 4. §2.5 sidebar → Task 4. §2.6 session → Task 3. §3.1 crate shape → Tasks 5–7, 10, 11. §3.2 frame → Task 7. §3.3 sections → Tasks 7 (Sources, Data toolbar partly), 8 (Data), 9 (Config), 10 (Log), 11 (Perf, ages). §3.4 rebuild rules → Tasks 7, 9. §3.5 keys → Task 7. §3.6 persistence → Task 7. §4 app → Task 7. §5 tests → each task; production routes (sidebar, status bar, palette row) → Task 4; mouse-opened page receives keys → Task 4. §6 docs → Task 12. §7 out of scope: nothing added.

**Placeholders.** Task 7's `sync_ages_timer` stub and the Perf stub are filled by Task 11; Task 7's Log toolbar paints only the filter input until Task 10. No TBDs.

**Type consistency.** `PageContent::dispatch(&self, action, count, window, cx) -> bool` (Task 1) is what `DiagnosticsContent` forwards (Task 7) and what the shell calls (Task 2). `ShellActions = Rc<dyn Fn(&ActionId, &mut Window, &mut App)>` is built in Task 2 and consumed in Task 7's harness and the Config button. `PreparedTable.rows[i].cells[2]` is the Since column in `SOURCE_COLUMNS` (Task 6) and what `tick_ages` rewrites (Task 11). `Diagnostics::set_catalog(snapshot, at)` gains its second parameter in Task 5; Tasks 8 and 9 tests pass it. `page.rs`'s `dispatch(&mut self, action, count, window, cx) -> bool` is called with a window in every test from Task 7 on.

**Review Focus.** All five lines name a test in the owning task: 1 → Task 2; 2 → Task 2; 3 → Task 7; 4 → Task 10; 5 → Task 8.
