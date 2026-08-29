# Geode Phase 1c (Shell Polish) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Polish the shell per user direction: config hot reload, vim-prefix focus/move bindings with direct resize keys, session layout persistence, new chrome (top toolbar, left sidebar with workspace indicators and a profile/settings entry, slimmed status bar), a settings dialog on gpui-component's `setting` module, palette refinements (no placeholder text, fuzzy-match highlighting), and the ledgered deferred-minor cleanup. Multi-window explicitly excluded.

**Architecture:** All in `geode-shell` (+ small `geode-app` wiring). Pure cores stay pure and TDD'd: reload-decision logic, layout (de)serialization + `Tree` reconstruction, fuzzy match with indices. gpui code follows the established drift-clause discipline.

**Tech Stack:** existing deps only (no `notify` — mtime polling via gpui background timer).

**Spec:** foundation spec §3.1 (resize, move; resize-as-mode simplified to direct bindings per user direction), §8 (hot reload, layouts-as-config); PHILOSOPHY (honest failure, keyboard-first, per-frame churn). User direction 2026-08-29: toolbar top; sidebar left with workspace indicators + profile icon → settings dialog (gpui-component `setting` module); palette placeholder text removed; fuzzy-matched characters highlighted.

## Global Constraints

- gpui drift rule (unchanged): plan text is the structural contract; vendored skills + pinned checkouts (`~/.cargo/git/checkouts/gpui-component-*/0e2fb7a*/` — note it HAS `setting/` and `avatar/` modules; zed checkout for gpui) are ground truth; record adaptations.
- No new dependencies. No raw colors — `cx.theme()` roles only. Keyboard-first: everything mouse-reachable here (profile icon, settings controls) must also be reachable via actions/palette.
- Render discipline: no I/O on the render path (the watcher runs in a background task; session saves happen on mutation/quit, not per frame); `Tree::layout` once per render.
- Invalid config/session files never panic or take down the app: Diagnostic-style degradation everywhere (bad session file = fresh start + warning).
- Every commit ends with the standard co-author trailer. TDD for all pure cores. CI green on both platforms.
- Established test pattern: `#[gpui::test]` e2e through real key dispatch wherever behavior is keyboard-driven; honest manual-check reporting (display may be unavailable).

## File Structure

```
crates/geode-shell/src/reload.rs           pure reload-decision core + watcher plumbing
crates/geode-shell/src/session.rs          layout (de)serialization, Tree reconstruction, session save/restore
crates/geode-shell/src/tiling/tree.rs      gains validated reconstruction constructor
crates/geode-shell/src/shell/mod.rs        new dispatch arms, chrome composition, settings dialog wiring
crates/geode-shell/src/shell/toolbar.rs    top toolbar (pure element fn)
crates/geode-shell/src/shell/sidebar.rs    left strip: workspace indicators + profile icon (pure element fn)
crates/geode-shell/src/shell/status.rs     slimmed: pending keys, reload indicator, theme name
crates/geode-shell/src/shell/settings_view.rs  settings dialog content on gpui-component setting module
crates/geode-shell/src/palette.rs          fuzzy indices + highlight render + polish
crates/geode-shell/src/defaults.rs         new actions + bindings (resize, move, vim prefixes, settings::open)
```

---

### Task 1: Config hot reload

**Files:** Create `crates/geode-shell/src/reload.rs`; modify `shell/mod.rs`, `shell/status.rs` (indicator input), `lib.rs`, `geode-app/src/main.rs` (pass watched dirs).

**Interfaces:**
- Produces: `reload::Snapshot` (mtimes of every `*.toml` in the desk+user dirs; `scan(dirs) -> Snapshot`; `Snapshot::changed_since(&Snapshot) -> bool` — pure, TDD with tempdirs); `reload::ReloadOutcome { Applied { warnings }, KeptLastGood { errors }, Unchanged }` and `decide(new_config: Config) -> ...` logic: **any error-severity diagnostic ⇒ keep the entire previous Config and report; warnings-only ⇒ apply**; `ShellView::apply_reload(...)` which rebuilds keymap + mod alias, re-applies theme if `[theme]` changed, closes an open palette, and records the outcome for the status bar.
- ShellView holds the watched dirs + last Snapshot + last `ReloadOutcome`; a background task (gpui `cx.spawn` + background timer, ~500ms) polls `scan`, and on change loads a fresh `Config` off the UI thread, then applies on the UI thread. No file I/O ever happens in render.
- Status bar gains a reload indicator input: nothing when healthy; `config: N error(s) — keeping last good` in a danger-toned token when `KeptLastGood`; a brief `reloaded` marker is optional (skip if noisy).

**Steps:** TDD the pure parts (snapshot diffing incl. added/removed/modified files; decide() semantics) → implement watcher task + apply path → `#[gpui::test]` if the test context supports advancing timers (check test refs; else test `apply_reload` directly by calling it with a constructed Config through the entity, which is still a real-entity test) → full verification → commit `feat: config hot reload with last-good semantics and status indicator`.

---

### Task 2: Vim rebinding, direct resize bindings, move-tile bindings

**Files:** Modify `defaults.rs`, `tiling/workspaces.rs`, `shell/mod.rs` (tests only, most likely).

**Interfaces:**
- **Vim-style rebinding (user direction):** focus movement becomes the vim window prefix — `"ctrl+w h/j/k/l"` (two-keystroke sequences; the engine supports them natively) replacing `mod+h/j/k/l`. Move-tile follows the vim analog: `"ctrl+w shift+h/j/k/l"`. The palette's primary binding becomes `ctrl+k` (replacing `mod+p`; keep `ctrl+shift+p` as the discoverable secondary). Update `BUILTIN_KEYMAP` accordingly; the palette-toggle intercept already resolves last-wins over bindings so it follows the new key automatically — verify its single-keystroke assumption still holds for `ctrl+k` (it does; sequences never were toggle candidates).
- **Resize is a direct binding, NOT a mode (user direction, revised twice):** no `ShellMode`, no `resize` context, no mode indicator, no `resize_mode`/`resize_exit` actions. **`shift+h/j/k/l`** bind directly to `workspace::resize_left/down/up/right` in the `workspace` context. Semantics: `shift+h` grows the focused tile's edge toward the left by `RESIZE_STEP` (i.e. `Tree::resize(Direction::Left, RESIZE_STEP)`), symmetrically for j/k/l — "lean the tile in that direction"; shrinking is growing the opposite way. Titles read "Resize: grow left" etc. (Note for the future: bare `shift+letter` in the workspace context will need context-gating care once modules own plain-letter vim bindings — fine today, tiles are placeholders.)
- **Splits and close rebound (user direction), with an action RENAME:** vim semantics chosen — `ctrl+v` opens a side-by-side split (vim `:vsplit`), `ctrl+h` a stacked one (vim `:split`), and `ctrl+shift+w` closes the focused tile. Because our i3-named action ids would read backwards under vim bindings (`ctrl+v` → "split_horizontal"), RENAME the actions to direction-based, ambiguity-free ids: `workspace::split_horizontal` → `workspace::split_right` (Orientation::Horizontal, new tile to the right; title "Split right"), `workspace::split_vertical` → `workspace::split_down` (Orientation::Vertical, new tile below; title "Split down"), `workspace::close_tile` keeps its id but rebinds from `mod+shift+q` to `ctrl+shift+w`. Rename everywhere: defaults registration + keymap, dispatcher arms in `apply_workspace_action`, every test that uses the old ids, and the empty-workspace hint copy in `shell/mod.rs` (becomes exactly `ctrl+h / ctrl+v to open a tile`). No aliases for the old ids — pre-1.0, clean break.
- Freed and left unbound: `mod+h/j/k/l`, `mod+s`, `mod+v`, `mod+shift+q`, `mod+p`, `mod+r`.
- New actions in `defaults`: `workspace::resize_left/down/up/right` (bound `shift+h/j/k/l`) and `workspace::move_left/down/up/right` (bound `ctrl+w shift+h/j/k/l`). Builtin keymap must still build diagnostic-free (guard test counts are minimums). Existing e2e tests that press old bindings must be updated (`ctrl+w` focus sequences, `ctrl+h`/`ctrl+v` splits, `ctrl+shift+w` close, `ctrl+k` palette).
- `apply_workspace_action` gains arms: `move_*` → `Tree::move_direction`; `resize_*` → `Tree::resize(dir, RESIZE_STEP)` with `pub const RESIZE_STEP: f32 = 0.03;`. All pure — no ShellView state involved.

**Steps:** TDD dispatcher arms + renames (pure) → rebind + retitle → `#[gpui::test]`: `ctrl+h` then `ctrl+w l` moves focus; `shift+h` changes ratios; `ctrl+w shift+l` swaps tiles; `ctrl+k` opens the palette; `ctrl+shift+w` closes → full verification → commit `feat: vim-idiom bindings — ctrl+w focus/move, ctrl+h/v splits, shift-hjkl resize`.

---

### Task 3: Session layout persistence

**Files:** Create `crates/geode-shell/src/session.rs`; modify `tiling/tree.rs` (reconstruction), `tiling/mod.rs`, `shell/mod.rs`, `geode-app/src/main.rs`.

**Interfaces:**
- `Tree::from_parts(root: Option<Node>, focused: Option<TileId>, fullscreen: Option<TileId>) -> Result<Tree, String>` — validated: every Split has ≥2 children and ratios of matching length summing to ~1 (renormalize small drift; reject NaN/non-positive), focused/fullscreen must be existing leaves (else cleared with the Result still Ok? No: invalid *references* are healed — cleared — since they're harmless; structural invalidity is Err). TDD.
- `session.rs`: `to_toml(workspaces: &Workspaces, extra: SessionExtra) -> toml::Table` and `from_toml(&toml::Table) -> Result<(Workspaces, SessionExtra), Vec<String>>` (pure, round-trip TDD; `SessionExtra { theme_mode: Option<String> }`); `save(path, ...)` atomic write (write temp + rename); `load(path)` tolerant (missing file = fresh start; parse/validation failure = fresh start + warnings). Format: `config_version = 1`, `[workspaces.N]` tables with a compact node encoding (nested tables mirroring `Node` — spell it out in code, no serde derives needed beyond manual construction since we already hand-build toml elsewhere; serde is acceptable if simpler — implementer's choice, recorded).
- Save triggers: after every successful workspace-mutating dispatch (cheap file, atomic) and on app quit if a quit hook is available at the pinned rev (check; mutation-save already covers crash-robustness). Restore: `main.rs` loads the session before constructing ShellView; `Workspaces::next_tile` must resume past the max restored TileId (add what's needed to `Workspaces` — e.g. `from_parts(spaces, active)` constructor that computes it).
- Session file location: user config dir, `session.toml` (state-as-config is fine — it's declarative and hand-editable). The hot-reload watcher from Task 1 must IGNORE `session.toml` changes we wrote ourselves — simplest: exclude the filename from the snapshot scan (document it).

**Steps:** TDD reconstruction + round-trip (including hostile inputs: bad ratios, dangling focused, unknown keys tolerated) → wire save/restore → `#[gpui::test]` or integration test: build workspaces, save, load, assert layouts equal → full verification → commit `feat: session layout persistence with validated tree reconstruction`.

---

### Task 4: Chrome — toolbar, sidebar, slimmed status bar

**Files:** Create `shell/toolbar.rs`, `shell/sidebar.rs`; modify `shell/mod.rs`, `shell/status.rs`.

**Interfaces (all pure element fns like `status_bar`, except the TitleBar which is gpui-component's):**
- **Toolbar = the native title bar row (user direction: no extra real estate).** Use gpui-component's `TitleBar` (`crates/ui/src/title_bar.rs` at the pinned checkout — it customizes the titlebar and hosts the window controls; `TitleBar::title_bar_options()` feeds `WindowOptions`; the `window_title` example shows the wiring; on macOS the traffic lights overlay it, on Windows it draws caption buttons). Content: the app title `geode` left, and — to prove the row hosts real content (user direction) — a **right-aligned filter text field** (gpui-component `Input` + `InputState` entity owned by ShellView, placeholder `filter`, compact width ~200px). It is deliberately **hooked up to nothing**: no consumer reads its value yet (comment: it becomes the global text filter, spec §4.1, in the data phase). Focus interplay must be handled: clicking the field focuses it and keys route to the input (shell chords won't fire — acceptable while typing a filter); `Esc` in the field returns focus to the shell root. The rest of the row stays reserved for grouping/scope state post-data-phase (comment). `main.rs` gains the `title_bar_options()` in its WindowOptions.
- `sidebar(active: u8, non_empty: &[u8], cx)` — narrow (~40px) left strip, `sidebar` tokens: vertical workspace indicators (same visibility rule as before: active always, non-empty always, others hidden), each clickable → `workspace::switch_N` through the dispatch chain; bottom-anchored profile icon (gpui-component `Avatar` with initials fallback or a user `Icon`) that dispatches `settings::open` (action arrives in Task 5 — for this task register the action in defaults with a no-op arm in ShellView that Task 5 fills; keep the guard test clean).
- `status_bar` slims to: pending keys, reload indicator (Task 1), theme name. Workspace indicators REMOVED (moved to sidebar).
- Composition in ShellView render: toolbar top, then a row of [sidebar | tile area], status bar bottom; tile-area bounds account for toolbar + sidebar + status heights/widths in the `Tree::layout` rect (still called once).

**Steps:** implement → adjust existing gpui::tests for new geometry if any assert on layout → full verification → honest manual check → commit `feat: top toolbar and workspace sidebar; slim the status bar`.

---

### Task 5: Settings dialog

**Files:** Create `shell/settings_view.rs`; modify `shell/mod.rs`, `defaults.rs`.

**Interfaces:**
- Inspect the pinned checkout's `crates/ui/src/setting/` (fields/group/item/page/settings) and any settings example in the repo FIRST; build the dialog content with those primitives (that module exists at this rev — verified). Fallback only if its API demands app-level infrastructure we don't have: plain `Dialog` + `Select` controls, recorded.
- Content v1, two groups: **Appearance** — theme family (dropdown over `ThemeService::names()` or family names), light/dark mode toggle (both apply immediately via `ThemeService`); **Keyboard** — read-only display of the mod key with a note that it's set via `[keymap] mod` in config (no editing UI yet; config editor is a later phase).
- Open paths: `settings::open` action (registered in defaults, palette-visible, category `Appearance`; also bound `mod+,`), the sidebar profile icon, and Esc/close via the dialog's own chrome (`window.open_dialog` — the overlay layers restored in 1b-ui make this work).
- Changes made in the dialog are live (theme applies immediately) but NOT persisted to config files (writing config is the config-editor phase); note this honestly in the dialog (small muted caption: `set [theme] in app.toml to persist`).

**Steps:** checkout inspection recorded → implement → `#[gpui::test]`: dispatch `settings::open`, assert dialog layer rendered (painted quads / root state), change mode via the service path if testable → full verification → commit `feat: settings dialog on gpui-component setting module`.

---

### Task 6: Palette polish — highlighting and cleanup

**Files:** Modify `palette.rs`, `shell/mod.rs`, `theme.rs`.

**Interfaces:**
- `fuzzy_match` returns `Option<(u32, Vec<usize>)>` (score + matched char indices in the candidate, byte-safe via char indexing — TDD: indices verified for prefix/word-start/scattered cases; existing scoring tests keep passing with the tuple).
- Render: matched characters styled (`cx.theme().primary` + bold if cheap) within each row title — build styled runs from the indices (contiguous index runs → styled spans; gpui `StyledText`/highlights or manual span children — check what the pinned rev offers; the `highlighter` module or `Label` masks may help, record choice).
- Placeholder helper text removed entirely; caret renders correctly for empty query (caret first, no text).
- `Matcher::cancel()` called on palette OPEN (ledgered: pending sequences no longer survive a palette session).
- Theme-name lookup gains punctuation normalization (`-`/`_` → space before case-fold) so `"macos-classic"` resolves (extend the existing resolve tests).

**Steps:** TDD indices + normalization → render + wiring → extend the existing palette e2e (assert a highlighted-run structure cheaply if practical; otherwise the pure indices tests + eyeball note) → full verification → commit `feat: fuzzy-match highlighting and palette polish`.

---

### Task 7: Deferred-minor cleanup batch

**Files:** Modify `geode-core/src/config/mod.rs` (Diagnostic Display), `shell/mod.rs` (+test), small touches per ledger.

**Batch (each item small, one commit):**
- `impl fmt::Display for Diagnostic` — `[layer] file: message` shape, unit-tested; `main.rs` stderr printing switches to it.
- Click-to-focus `#[gpui::test]`: simulate mouse-down on a non-focused tile's coordinates, assert focus changed (ledgered from 1b-ui T3).
- `convert_keystroke` double-call on the closed-palette path deduplicated (ledgered).
- Empty-hint render branch: cheap assertion (empty workspace draw → painted quads include text runs; or skip with a recorded reason if the test API can't see text).

**Steps:** implement + tests → verification → commit `chore: deferred-minor cleanup — Diagnostic Display, click-to-focus test, dispatch dedup`.

---

### Task 8: Which-key hint

**Files:** Modify `shell/mod.rs` (+ a small pure helper wherever fits — `shell/whichkey.rs` if it earns a file).

Reinstated (the earlier cut is obsolete): the `ctrl+w` prefix now creates a real sequence family, exactly the case which-key exists for.

**Interfaces:**
- Pure core, TDD: `continuations(keymap: &Keymap, pending: &[Keystroke], stack: &[KeyContext]) -> Vec<(Keystroke, ActionId)>` — for every binding whose sequence strictly extends `pending` and whose predicate passes, yield (next keystroke, action), deduped last-wins, sorted by keystroke text; excludes `none` actions.
- Render: when `matcher.pending()` is non-empty, a small bottom-right overlay (above the status bar, `popover` tokens) listing each continuation as `key → title` (title from the registry; fall back to the action id). Appears immediately (no delay timer — YAGNI until it annoys someone).
- Overlay must not steal focus or affect key routing — display only.

**Steps:** TDD the pure core (extensions, predicate gating, dedup-last-wins, none-exclusion) → render wiring → extend the pending gpui::test to assert the overlay painted while pending → full FINAL verification (`cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`) → commit `feat: which-key continuation hint for pending sequences`.

---

## Self-Review Notes

- **Coverage of user direction:** titlebar-integrated toolbar with no extra row and no content yet (T4), sidebar with workspace indicators + profile icon → settings dialog on the `setting` module (T4+T5), vim-style `ctrl+w` focus/move prefix + `ctrl+k` palette (T2), palette placeholder removed (T6), fuzzy highlighting (T6). Multi-window: excluded per user. Hot reload (T1), direct resize + vim bindings (T2), session persistence (T3), which-key (T8 — reinstated because the `ctrl+w` prefix creates a real sequence family).
- **Consistency:** new action ids (`workspace::resize_*`, `workspace::move_*`, `settings::open`, `workspace::resize_mode/exit`) all registered in defaults with bindings; guard test stays diagnostic-free; dispatcher arms match; watcher ignores `session.toml`; palette-open cancels pending (supersedes the 1b-ui deferred note).
- **Order:** T1 and T2 independent; T3 before T4 only for `next_tile` API stability (not strictly required — briefs are self-contained); T4 before T5 (profile icon hosts the open path).

---

### Task 9: Uniform dialog utility (user direction, added mid-phase)

**Files:** Create `crates/geode-shell/src/shell/dialog.rs`; modify `shell/mod.rs`, `shell/settings_view.rs`, `CLAUDE.md`.

**Interfaces:**
- `dialog::open_shell_dialog(view: &mut ShellView, window, cx, build: impl FnOnce(...))` — the single mandatory door for opening any dialog in Geode. On open it: (1) cancels any pending key sequence (`matcher.cancel()` — consistent with palette-open), (2) closes the palette if open, (3) delegates to gpui-component's dialog layer with our standard conventions. Signature adapts to what `window.open_dialog`'s builder actually needs at the pinned rev — structure is the contract.
- Chord suppression while a dialog is open is already handled centrally by the `has_active_dialog` guard in `handle_key_down` (final-review fix) — this utility does NOT duplicate it; its module doc references the guard so the two halves of the uniform behavior are discoverable together.
- Module doc states the rule: dialogs are opened through this function, never `window.open_dialog` directly. Migrate `settings_view::open` to it (the only current call site — the convention is established while there's exactly one). CLAUDE.md gains a one-line gotcha under the gpui section.
- Tests: a `#[gpui::test]` proving open-through-utility cancels pending (start a `ctrl+w` sequence, open a dialog via the utility, assert pending cleared and which-key overlay gone) and closes an open palette.

**Steps:** implement → migrate settings_view → tests → full verification → commit `feat: uniform shell dialog utility — pending/palette hygiene on open`.

---

### Task 10: Fonts — JetBrains Mono and Inter (user direction, added mid-phase)

**Files:** Create `assets/fonts/` (vendored TTF/OTF + OFL license files); modify `crates/geode-app/src/main.rs` (font registration), `crates/geode-shell` render code where mono applies, possibly theme wiring.

**Interfaces:**
- Vendor both families into `assets/fonts/` (static weights actually used — regular/medium/semibold for Inter, regular/bold for JetBrains Mono — not every weight; include each family's OFL-1.1 license file alongside). Both are SIL OFL — vendoring is fine; record exact upstream versions in a small README in that directory.
- INVENTORY-FIRST: inspect how the pinned revs want fonts wired — gpui's text system accepts embedded font bytes at startup (find the exact API in the zed checkout: `text_system().add_fonts` or equivalent), and gpui-component's `Theme` carries font-family configuration (inspect `crates/ui/src/theme/` for `font_family`/mono equivalents and how `ThemeConfig` interacts — a theme JSON must not override our families unexpectedly; record findings).
- Mapping (user-approved): **Inter** = default UI face (chrome, palette titles, dialogs, hints — becomes the window/theme default). **JetBrains Mono** = data face: keystroke/binding displays (status pending keys, which-key key column, palette binding hints), tile placeholder labels; documented as the face the phase-3 blotter will use for cells. Mono usages reference the family explicitly via a shared constant (e.g. `shell::fonts::MONO`), not string literals scattered.
- Fallback honesty: if a font fails to load, the app must still run on the platform default (warning to stderr) — never panic.
- Tests: font registration smoke (`#[gpui::test]` asserting the families resolve in the text system if the API allows; else document), plus the shared-constant usage compiled everywhere (grep test in report).

**Steps:** inventory inspection (record findings) → vendor fonts + licenses + README → register at startup → apply mapping via theme/window defaults + `fonts::MONO` constant at the named mono sites → tests + honest manual note → full FINAL verification (`cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`) → commit `feat: bundled Inter and JetBrains Mono with UI/data font mapping`.
