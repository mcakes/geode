# Geode Phase 1b-ui (Shell UI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The shell becomes visible and drivable: gpui key interception feeding the Phase 1a Matcher, the Phase 1b-core tiling tree rendered as real tiles with focus styling and click-to-focus, workspace switching, a status bar, gpui-component theming with its bundled default themes selectable from config and palette, and the command palette itself.

**Architecture:** `geode-shell` gains gpui and owns the shell view (`shell` module), theming (`theme` module), and palette (`palette` module). `geode-app` shrinks to wiring: load `Config`, build registry/keymap/workspaces, apply the theme, open the window with the shell root view. The tiling tree stays the single source of truth: rendering positions tiles absolutely from `Tree::layout(pixel_bounds)`; no gpui state duplicates tree state. All chrome colors come from `cx.theme()` semantic roles — no raw hex/rgb anywhere (gpui-component design-guide rule).

**Tech Stack:** gpui + gpui-component (existing pinned revs), no other new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-28-geode-foundation-design.md` §3 (interaction model), §8 (config; theming is config), plus `docs/PHILOSOPHY.md` (keyboard-first; per-frame heap churn is a defect).

## Global Constraints

- gpui API drift rule (established in Phase 0 Task 4): the code in this plan is the **structural contract**; exact identifiers may differ at the pinned revs. Ground truth is the vendored skills (`.agents/skills/gpui/`, `.agents/skills/gpui-component/`) and the gpui-component checkout's `examples/` + source at `~/.cargo/git/checkouts/gpui-component-*/0e2fb7a*/`. Adapt names, keep structure, record every adaptation in the task report.
- Dependency rules hold: `geode-shell` gains gpui/gpui-component; `geode-data` and `geode-core` never do. gpui git deps stay **unpinned** with the Cargo.lock strategy (the geode-app Cargo.toml comment is authoritative); gpui-component stays rev-pinned. Single declaration via `[workspace.dependencies]`.
- No raw colors: every color read via `cx.theme()` semantic roles (`background`, `foreground`, `border`, `primary`, `muted`, `muted_foreground`, etc.).
- Keyboard-first: every feature in this plan must be fully drivable with no mouse; mouse adds convenience (click-to-focus) only.
- Render discipline (philosophy §6): the render path reads prepared state; no I/O, no config parsing, no allocation-heavy work per frame beyond what element construction inherently needs. `Tree::layout` is called once per render pass, not per tile.
- Pure logic stays pure and tested (fuzzy filter, palette item filtering, keystroke conversion mapping table); gpui-touching behavior is verified by `cargo run` eyeball checks documented per task, plus `#[gpui::test]` where the pinned revs make it practical (attempt in Task 2; if test-support proves impractical, document why in the report and fall back — do not burn more than ~30 minutes fighting it).
- Every commit ends with the project's standard co-author trailer.
- CI must stay green on both platforms: nothing platform-specific outside what gpui already abstracts.

## File Structure

```
Cargo.toml                              [workspace.dependencies] gains the gpui stack
crates/geode-app/Cargo.toml             deps become workspace = true references
crates/geode-app/src/main.rs            wiring only: config → services → theme → window
crates/geode-shell/Cargo.toml           gains gpui stack (workspace = true)
crates/geode-shell/src/shell/mod.rs     ShellView: state ownership, render, key dispatch
crates/geode-shell/src/shell/keys.rs    gpui Keystroke → keymap Keystroke conversion (pure-testable)
crates/geode-shell/src/shell/status.rs  status bar element
crates/geode-shell/src/theme.rs         bundled themes: load, list, apply, actions
crates/geode-shell/src/palette.rs       palette state + fuzzy filter (pure) + render
assets/themes/*.json                    vendored gpui-component default themes
```

---

### Task 1: Workspace-level gpui dependencies

**Files:**
- Modify: root `Cargo.toml`, `crates/geode-app/Cargo.toml`, `crates/geode-shell/Cargo.toml`

**Interfaces:**
- Consumes: the existing pinned revs in geode-app's Cargo.toml.
- Produces: `gpui`, `gpui_platform`, `gpui-component`, `gpui-component-assets` available as `workspace = true` deps; geode-shell builds against them.

- [ ] **Step 1: Move the gpui stack to `[workspace.dependencies]`**

In the root `Cargo.toml`, add a `[workspace.dependencies]` gpui block by moving the four git dependency declarations *verbatim* from `crates/geode-app/Cargo.toml` — including the multi-line comment explaining the unpinned-gpui/Cargo.lock strategy and the recorded zed SHA, and the `runtime_shaders` comment. The comment must move with the declarations (it documents them).

- [ ] **Step 2: Reference from both crates**

`crates/geode-app/Cargo.toml` dependencies become:
```toml
gpui.workspace = true
gpui_platform.workspace = true
gpui-component.workspace = true
gpui-component-assets.workspace = true
geode-shell = { path = "../geode-shell" }
geode-core.workspace = true
```
(Add `geode-shell`/`geode-core` path entries to `[workspace.dependencies]` if not present; keep existing entries consistent.)

`crates/geode-shell/Cargo.toml` gains:
```toml
gpui.workspace = true
gpui-component.workspace = true
```
(geode-shell does not need `gpui_platform` or the assets crate — those are app-level.)

- [ ] **Step 3: Verify no re-resolution**

Run: `cargo build --workspace 2>&1 | tail -5` then `git diff Cargo.lock`
Expected: build succeeds; `Cargo.lock` unchanged (or only trivially reordered) — the same revs resolve. If the zed SHA moved, STOP and report BLOCKED (the lockfile pin regressed).

- [ ] **Step 4: Full verification and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

```bash
git add Cargo.toml Cargo.lock crates/geode-app crates/geode-shell
git commit -m "build: move gpui stack to workspace dependencies; geode-shell joins the UI layer"
```

---

### Task 2: Shell view and key dispatch

**Files:**
- Create: `crates/geode-shell/src/shell/mod.rs`, `crates/geode-shell/src/shell/keys.rs`
- Modify: `crates/geode-shell/src/lib.rs` (declare `pub mod shell;`)
- Modify: `crates/geode-app/src/main.rs`

**Interfaces:**
- Consumes: everything from 1a/1b-core: `Config`, `ConfigSources`, `ActionRegistry`, `defaults`, `build_keymap`, `Matcher`, `MatchResult`, `KeyContext`, `Workspaces`, `apply_workspace_action`.
- Produces: `shell::ShellView` — the window root view (wrapped in gpui-component `Root` by the app); `shell::ShellServices { config, registry, keymap, mod_alias, workspaces }` constructor input built by the app; `keys::convert_keystroke(&gpui::Keystroke) -> Option<keymap::Keystroke>` (pure, unit-tested).

**Design (the contract):**
- `ShellView` owns: `services: ShellServices`, `matcher: Matcher`, `focus_handle: FocusHandle`, `palette_open: bool` (false until Task 6 uses it).
- The root element is focusable (`track_focus`) and intercepts keys with `on_key_down`. Handler: convert the gpui keystroke via `keys::convert_keystroke`; build the context stack (`[KeyContext::new("workspace")]`, later + palette); feed `matcher.press`; on `Matched`, route: `apply_workspace_action` first; if unhandled and the action is `palette::toggle`, flip `palette_open` (real palette in Task 6); notify (`cx.notify()`) after any state change. `Pending`/`NoMatch` also notify (status bar shows pending keys later).
- `keys.rs` conversion: gpui `Keystroke` exposes modifiers (control/alt/shift/platform) and a `key` string (already lowercase names like `"a"`, `"enter"`, `"escape"`, digits). Map: control→ctrl, alt→alt, shift→shift, platform→cmd; pass the key string through. Return `None` for bare-modifier presses (gpui may or may not deliver them; guard anyway). Unit-test the mapping table with constructed gpui keystrokes if the type is constructible at the pinned rev (it has public fields or a parse fn — check the gpui checkout); otherwise test via a thin internal struct mirroring the fields and a `#[cfg(test)]` seam, and note it.
- Render (this task's placeholder; Task 3 replaces): a `v_flex` with a text line showing `workspace {n} · {k} tiles · focused {id:?}` centered, themed via `cx.theme().background`/`foreground`.
- `main.rs` rewiring: load `Config` (`ConfigSources { builtin: [keymap doc], desk: env var GEODE_DESK_CONFIG as path if set, user: platform config dir geode/ }` — use `std::env::var` and a minimal platform-appropriate user dir: `dirs` crate is NOT allowed (no new deps); use `%APPDATA%`/`$HOME/.config` via `std::env::var("APPDATA")` fallback `HOME` + `.config`, in a small `config_dirs()` fn in main.rs with a unit-testable pure core if practical); build registry + keymap + workspaces; construct `ShellView` inside the window, wrapped in `Root`. Config diagnostics: print to stderr for now (diagnostics UI is a later phase) — one line each.
- Attempt one `#[gpui::test]` (in `crates/geode-shell/src/shell/mod.rs` tests or `tests/`): construct the view with test services, simulate a key event, assert `workspaces` changed. Consult `.agents/skills/gpui/references/test.md` and `test-examples.md` first. If the pinned rev makes simulated key dispatch through `Root`-less contexts impractical within ~30 minutes, document the blocker in the report and rely on the conversion unit tests + manual check.

- [ ] **Step 1:** Read `.agents/skills/gpui/references/action.md`, `focus-handle.md`, `context.md`, and `.agents/skills/gpui-component/references/usage.md` before writing code.
- [ ] **Step 2:** Write `keys.rs` with its unit tests first (TDD for the pure part); verify fail → implement → pass.
- [ ] **Step 3:** Write `shell/mod.rs` (ShellView, ShellServices, key handler, placeholder render) and declare the module.
- [ ] **Step 4:** Rewire `main.rs`; `cargo build --workspace`.
- [ ] **Step 5:** Attempt the `#[gpui::test]` per the design note.
- [ ] **Step 6:** Manual check: `cargo run -p geode-app` — window opens; `mod+s` visibly changes the tile count line; `mod+2`/`mod+1` change the workspace number; typing plain letters does nothing. Kill cleanly. Document what you saw (or that no display was available).
- [ ] **Step 7:** `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt`; commit `feat: shell view with keymap-driven dispatch replacing the phase-0 placeholder`.

---

### Task 3: Tile rendering

**Files:**
- Modify: `crates/geode-shell/src/shell/mod.rs`

**Interfaces:**
- Consumes: `Tree::layout`, `Tree::focused`, `Workspaces`.
- Produces: the tiled render; `ShellView` gains click-to-focus.

**Design (the contract):**
- Render pass: measure the content area (window bounds minus status bar height; gpui provides viewport size via the window or a measured container — the `canvas` element or `div` with known bounds; consult `.agents/skills/gpui/references/layout-style.md` and element references). Call `tree.layout(Rect{0,0,w,h})` once; for each `(TileId, Rect)` emit an absolutely-positioned child (`div().absolute().left(px(r.x)).top(px(r.y)).w(px(r.w)).h(px(r.h))`) with a 1px inset gap.
- Tile chrome: background `cx.theme().background`, border `cx.theme().border`; the focused tile's border uses `cx.theme().primary` (2px vs 1px is acceptable); tile content is a centered `muted_foreground` label `tile {id}` (module hosting arrives in Phase 3).
- Click-to-focus: `on_mouse_down` on each tile calls `tree.focus(id)` + notify. Keyboard remains the primary path.
- Empty workspace renders a centered muted hint: `mod+s / mod+v to open a tile` (copy exactly this — it teaches the two verbs).
- Fullscreen naturally renders one tile filling the content area (layout already does this).

- [ ] **Step 1:** Implement per the contract (read the gpui element/layout references first).
- [ ] **Step 2:** Manual check: `cargo run -p geode-app` — splits produce visible side-by-side/stacked tiles with gaps; hjkl moves the highlighted border; `mod+shift+h/j/k/l`… (move bindings don't exist yet — skip); `mod+f` fullscreens; clicking a tile focuses it; empty workspace shows the hint. Document observations.
- [ ] **Step 3:** Full verification; commit `feat: render the tiling tree as themed tiles with focus styling and click-to-focus`.

---

### Task 4: Status bar

**Files:**
- Create: `crates/geode-shell/src/shell/status.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs`

**Interfaces:**
- Consumes: `Workspaces::active_index/non_empty_indices`, `Matcher::pending`, theme name (Task 5 wires the real name; render a placeholder `"default"` until then and leave a `// Task 5 wires theme name` note).
- Produces: `status::status_bar(...) -> impl IntoElement` (a pure function of its inputs — no state).

**Design:** a fixed-height (~26px) bottom `h_flex` on `cx.theme().sidebar` (or `muted` if sidebar token absent at rev) with: left — workspace indicators 1..9 where the active one is `primary`-styled, non-empty ones normal, empty ones hidden (except active); middle-left — pending keystrokes rendered as text (e.g. `g` while a sequence is pending; empty otherwise); right — theme name in `muted_foreground`. All colors via `cx.theme()`.

- [ ] **Step 1:** Implement; wire under the tile area in ShellView's render (v_flex: content area grows, status bar fixed).
- [ ] **Step 2:** Manual check: workspace indicator follows `mod+N`; pressing `g` (bind nothing to it — actually no sequences exist in builtin: temporarily verify pending display via a desk/user keymap file with a `"g g"` binding, or note that pending display is exercised in Task 6's palette-Esc flow) — document what was verified.
- [ ] **Step 3:** Full verification; commit `feat: status bar with workspace indicators and pending-key display`.

---

### Task 5: Theming — gpui-component bundled themes

**Files:**
- Create: `assets/themes/` (vendored JSONs), `crates/geode-shell/src/theme.rs`
- Modify: `crates/geode-shell/src/lib.rs`, `crates/geode-app/src/main.rs`, `crates/geode-shell/src/shell/status.rs` (real theme name)

**Interfaces:**
- Consumes: `Config` (`app` doc: `[theme] name = "…"`, `mode = "light"|"dark"`), gpui-component `Theme` global.
- Produces: `theme::ThemeService` with `load_bundled() -> (ThemeService, Vec<String /*warnings*/>)`, `names() -> Vec<String>` (for the palette), `active_name() -> &str`, `apply(name: &str, mode: Mode, cx) -> bool`, `apply_from_config(&Config, cx)`; action `theme::toggle_mode` registered in `defaults` and handled in ShellView's dispatch chain.

**Design (the contract):**
- Vendor the default theme JSONs from the pinned gpui-component checkout (`~/.cargo/git/checkouts/gpui-component-*/0e2fb7a*/themes/*.json` — verify the exact path in the checkout; if the crate exposes them via an API/feature instead, prefer that and skip vendoring, recording the choice) into `assets/themes/`, embedded via `include_str!` in a generated-by-hand `const BUNDLED: &[(&str, &str)]` list in `theme.rs` (name, json). Binary stays self-contained; no runtime file I/O on the render path.
- Parse each with gpui-component's theme-config type (`ThemeConfig`/`ThemeSet` — verify name at rev; `Theme::global_mut(cx).apply_config(&config)` is the application seam per the vendored skill). A JSON that fails to parse becomes a warning string, not a crash (config philosophy).
- `apply_from_config`: read `theme.name` (default: gpui-component's default theme) and `theme.mode` (default dark); unknown name → warning + default. App calls it once after `gpui_component::init` and before opening the window; `theme::toggle_mode` flips light/dark at runtime via the documented `toggle_mode` API.
- Status bar shows `active_name()`.

- [ ] **Step 1:** Inspect the checkout: locate the theme JSONs and the config-parsing type; record findings.
- [ ] **Step 2:** Vendor + implement `theme.rs` with pure tests for: bundled list parses clean (all names load), unknown-name fallback, config-key reading (construct `Config` from builtin docs in the test).
- [ ] **Step 3:** Wire app + toggle action + status bar name.
- [ ] **Step 4:** Manual check: default theme applies (visibly different from Phase 0's look if the default differs); setting `[theme] name = "…"` in a user config file changes startup theme; `theme::toggle_mode` via a temporary binding or the palette (if Task 6 landed) flips modes live. Document.
- [ ] **Step 5:** Full verification; commit `feat: gpui-component bundled themes selectable from config with runtime mode toggle`.

---

### Task 6: Command palette

**Files:**
- Create: `crates/geode-shell/src/palette.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs`, `crates/geode-shell/src/lib.rs`

**Interfaces:**
- Consumes: `ActionRegistry::iter` (actions with title/category), `ThemeService::names`, keymap (to show bindings next to actions: build a reverse index `ActionId -> String` from `Keymap::bindings()` once at palette-open, not per frame).
- Produces: `palette::PaletteState` (pure: `items`, `query`, `selected`, `filter()`) + `palette::render(...)`; palette items: `Action(ActionId, title, category, Option<binding>)` and `Theme(name)`.

**Design (the contract):**
- Pure core, fully unit-tested: `fuzzy_match(query, candidate) -> Option<u32>` — case-insensitive subsequence match, score favors consecutive runs and word starts; `PaletteState::filtered()` returns sorted matches; `move_selection(±1)` clamps; no dependencies.
- Interaction: `palette::toggle` opens/closes; while open, the context stack is `[workspace, palette]` and ShellView routes keys: printable chars append to query, backspace edits, up/down (and ctrl+p/ctrl+n) move selection, Enter dispatches (action → the normal dispatch chain incl. `theme::toggle_mode`; theme item → `ThemeService::apply`), Esc closes. Palette-open swallows all other bindings (matcher not consulted while open, except Esc — simplest: while open, keys go to the palette handler exclusively).
- Render: centered overlay (~560px wide, top-third) on `cx.theme().popover` with border, an input line (rendered text + caret is fine — a full Input entity is acceptable if simpler at rev; either way keyboard behavior above is the contract), and the top ~12 results with the selected row highlighted (`primary` on selection background token per usage guide), binding shown right-aligned in `muted_foreground`.
- Every action title/category from the registry appears; themes appear as `Theme: {name}` under category `Appearance`.

- [ ] **Step 1:** TDD the pure core (fuzzy scoring: prefix beats scattered; case-insensitive; empty query returns all in registry order; selection clamping).
- [ ] **Step 2:** Implement render + ShellView integration.
- [ ] **Step 3:** Manual check: `mod+p` opens; typing filters; Enter on "Split horizontal" splits; Enter on a theme changes theme; Esc closes; all bindings display. Document.
- [ ] **Step 4:** Full verification (`cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`); commit `feat: command palette over actions and themes with fuzzy filtering`.

---

## Self-Review Notes

- **Spec coverage:** §3.2/§3.3 palette (universal, shows bindings, discoverability), §3.1 shell chrome (status bar workspace indicators), theming per user direction (gpui-component themes bundled + config-selected + runtime toggle), key dispatch through the real keymap engine (no gpui keymap parallel system — one keymap, ours). Deliberately out: resize mode, move-tile bindings, hot reload, multi-window, layout persistence, which-key overlay — future 1c/phase work; the tree APIs for move/resize already exist unbound.
- **Consistency:** action ids referenced (`palette::toggle`, `theme::toggle_mode`, workspace verbs) match/extend `defaults` (Task 5 adds `theme::toggle_mode` to `register_builtin_actions` + BUILTIN_KEYMAP binding `mod+shift+t`); status bar's theme-name placeholder is explicitly handed off from Task 4 to Task 5.
- **Honesty about gpui:** tasks carry the drift clause and mandatory skill-reading steps; pure cores (keystroke conversion, fuzzy filter, theme config reading) are TDD'd; gpui behavior is eyeball-verified per task with observations recorded.
