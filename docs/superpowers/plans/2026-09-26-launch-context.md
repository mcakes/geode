# Launch Context Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A market-data panel opened without an underlying prompts for one at once, and `g m` in the blotter or pricer opens a panel already set to the underlying at the cursor.

**Architecture:**
- A typed `LaunchContext` in `geode-core` is the shared vocabulary.
- The shell pulls the focused tile's context through a new `TileContent::launch_context` method. It lists the kinds whose `ModuleFactory::accepts` covers that context, in the shared choice dialog. It then creates the chosen kind through the existing `add_tile(kind, Split(None), state)`, using the factory's `launch_state` translation.
- A new `TileContent::launched` hook, deferred after the first render of a tile created by `add_tile`, lets the market-data panel open its underlying picker.

**Tech Stack:** Rust, GPUI + gpui-component (pinned 0.6.2), `toml` tables for tile state.

**Spec:** `docs/superpowers/specs/2026-09-26-launch-context-design.md`

## Global Constraints

- Feature modules never depend on sibling features. `geode-shell` never depends on a feature module.
- `geode-core` stays pure: no I/O, no GPUI in `launch.rs`.
- Every pointer action has a keyboard route. The only new keyboard route is `g m`, and it adds no pointer route.
- No state mutation during render. `launched` is scheduled with `cx.defer_in`, never called inline from `ensure_occupants`.
- A NULL or ambiguous value yields an EMPTY context, never a guessed key.
- Placement for every context launch is `AddPlacement::Split(None)`.
- Keys: `"g m" = "tile::open_with"` in `blotter && mode == normal` and in `pricer && mode == normal`.
- The shell's notice field is `Option<&'static str>`. The no-accepting-kind notice is the constant `"no module opens on the context at the cursor"`. This replaces the spec's `no module opens on {underlying}`; Task 3 amends the spec to match.
- Say "color" in user-facing text (not applicable here; no copy mentions it).
- Work in a git worktree (`worktree-launch-context`). Never `git reset --soft main` to squash.
- Test runs: `cargo test -p <crate> <filter>`. Before merge: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets`, and `zsh scripts/mutation-check.sh --anchors-only`.
- New mutation entries go in `scripts/mutation-check.sh` immediately after the last `run_mutation` entry, which is `"modal back: a click is ignored while a confirm is pending"`, and before the `if [[ -n "$changed_ref" ]]` block. Each entry has this form:
  `run_mutation "<name>" <file> '<anchor, exact, unique>' '<replacement>' <package> <exact test fn name>`.
  Run each new entry with `zsh scripts/mutation-check.sh "<name substring>"`. It must report the mutation as caught. Commit before running mutations: the harness edits files in place.

## Review Focus

1. **Focus race: `launched` vs. modal focus return.** A pick in the tile-kind picker closes the modal, which returns focus, and then adds the tile. The deferred `launched` must run after that return, or the launched tile's input loses the keyboard. Pinned in Task 2 Step 4b, through the picker's real commit route.
2. **`pending_focus_restore` stealing a launched input's focus on the next frame.** It is safe only because the input counts as insert mode (`holds_focus`). Pinned in Task 2 Step 4b, which draws twice after the pick, and in Task 4, which draws after `launched` and checks `holds_focus`.
3. **`g m` while a `g` sequence is pending in a module that also binds `g g`, `g p` or `g u`.** `g m` must not fire `g g` or swallow the `m`. Pinned in Task 5 and Task 6 with fragment tests that check the binding table has `g m` next to the existing `g` chords, and in Task 3 with a real keystroke through the matcher.
4. **Context captured at open vs. read at commit.** Moving the source cursor while the dialog is open must not change what gets opened. Pinned in Task 3.
5. **Restored empty panels at startup.** Several panels saved without an underlying must not each open a picker. Pinned in Task 2 (the shell never calls `launched` on a restore) and Task 4 (a restored panel with no key has no picker until `launched` is called).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-core/src/launch.rs` (new) | `LaunchContext`, `ContextField`: the shared vocabulary |
| `crates/geode-core/src/lib.rs` | `pub mod launch;` |
| `crates/geode-shell/src/module.rs` | New trait methods (`launch_context`, `launched`, `accepts`, `launch_state`); `RecordingFactory` fixture support |
| `crates/geode-shell/src/shell/occupants.rs` | Schedules `launched` for fresh, focused `add_tile` occupants |
| `crates/geode-shell/src/shell/choicedialog.rs` | `Target::TileKindWith`, `Pick::KindWith`, `open_tile_kinds_with`, dynamic title |
| `crates/geode-shell/src/shell/input.rs` | `tile::open_with` dispatch arm, notice constant |
| `crates/geode-shell/src/defaults.rs` | Registers `tile::open_with` |
| `crates/geode-shell/src/shell/tests/mod.rs` | `services_with_recorders` fixture |
| `crates/geode-shell/src/shell/tests/launch.rs` (new) | Shell GPUI tests for `launched` and `tile::open_with` |
| `crates/geode-marketdata/src/content.rs` | `accepts`, `launch_state`, `launched` forwarding |
| `crates/geode-marketdata/src/tile.rs` | `MarketDataTile::launched` |
| `crates/geode-blotter/src/core/launch.rs` (new) | Pure `underlying_at(path, grouping)` |
| `crates/geode-blotter/src/delegate.rs`, `tile.rs`, `content.rs` | `cursor_underlying`, `launch_context`, `g m` binding |
| `crates/geode-pricer/src/core/sheet.rs` | Pure `Sheet::sole_underlying(row)` |
| `crates/geode-pricer/src/tile.rs`, `content.rs` | `launch_context`, `g m` binding |
| `scripts/mutation-check.sh` | Eight new entries |
| Docs | `docs/current/{shell,features,keymaps}.md` and the four crate READMEs |

---

### Task 1: Vocabulary and trait seams

**Files:**
- Create: `crates/geode-core/src/launch.rs`
- Modify: `crates/geode-core/src/lib.rs` (add `pub mod launch;` in alphabetical position, after `pub mod health;`)
- Modify: `crates/geode-shell/src/module.rs`, in the `TileContent` trait (after `holds_focus`), the `ModuleFactory` trait (after `default_keymap`), and the `recording` module

**Interfaces:**
- Produces:
  - `geode_core::launch::{LaunchContext { pub underlying: Option<String> }, ContextField::Underlying}`
  - `LaunchContext::is_empty(&self) -> bool`
  - `LaunchContext::has(&self, ContextField) -> bool`
  - `LaunchContext::covered_by(&self, accepts: &[ContextField]) -> bool`
  - `TileContent::launch_context(&self, cx: &App) -> LaunchContext`
  - `TileContent::launched(&self, window: &mut Window, cx: &mut App)`
  - `ModuleFactory::accepts(&self) -> &'static [ContextField]`
  - `ModuleFactory::launch_state(&self, ctx: &LaunchContext) -> Option<toml::Table>`
  - Recording fixture fields: `RecordingFactory.launch_context: Rc<RefCell<LaunchContext>>`, `RecordingFactory.accepts: &'static [ContextField]`, and `Recorded::Launched(TileId)`

- [ ] **Step 1: Write the failing core tests.** Create `crates/geode-core/src/launch.rs` containing only the tests module and the type stubs needed for the names to resolve. Write the full file now; the implementation is trivial and TDD here is the test-first order within the step.

```rust
//! What a source tile knows at its cursor that another module may open
//! on. The shell pulls it from the focused tile (`TileContent::
//! launch_context`) and lists the kinds whose factory accepts it; the
//! factory translates it into its own restored-state table. Typed, not a
//! table, so a source and a target agree on the words without depending
//! on each other.

/// The context at a source tile's cursor. Every field is `None` when the
/// cursor names no single value: an ambiguous or NULL value is empty, never
/// a guess, because a panel opened on a made-up key is a plausible wrong
/// answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchContext {
    /// The desk's underlying identifier, as the blotter's `underlying_ref`
    /// column and a pricer instrument spell it.
    pub underlying: Option<String>,
}

/// A field of [`LaunchContext`] a target module can open on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextField {
    Underlying,
}

impl LaunchContext {
    pub fn is_empty(&self) -> bool {
        self.underlying.is_none()
    }

    pub fn has(&self, field: ContextField) -> bool {
        match field {
            ContextField::Underlying => self.underlying.is_some(),
        }
    }

    /// Whether a kind accepting `accepts` can open on every field set here.
    /// An empty context is covered by nothing: there is nothing to open on.
    pub fn covered_by(&self, accepts: &[ContextField]) -> bool {
        !self.is_empty() && [ContextField::Underlying]
            .iter()
            .all(|f| !self.has(*f) || accepts.contains(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spx() -> LaunchContext {
        LaunchContext {
            underlying: Some("SPX".into()),
        }
    }

    #[test]
    fn an_empty_context_has_nothing_and_is_covered_by_nothing() {
        let c = LaunchContext::default();
        assert!(c.is_empty());
        assert!(!c.has(ContextField::Underlying));
        assert!(!c.covered_by(&[ContextField::Underlying]));
        assert!(!c.covered_by(&[]));
    }

    #[test]
    fn an_underlying_is_covered_only_by_a_kind_accepting_it() {
        let c = spx();
        assert!(!c.is_empty());
        assert!(c.has(ContextField::Underlying));
        assert!(c.covered_by(&[ContextField::Underlying]));
        assert!(!c.covered_by(&[]), "a kind accepting nothing is not listed");
    }
}
```

Add `pub mod launch;` to `crates/geode-core/src/lib.rs`, and add "launch contexts" to the crate doc's list on line 1 or 2.

- [ ] **Step 2: Run the core tests**

Run: `cargo test -p geode-core launch::`
Expected: 2 passed.

- [ ] **Step 3: Add the trait methods.** In `crates/geode-shell/src/module.rs`, add `use geode_core::launch::{ContextField, LaunchContext};` beside the other `geode_core` imports.

Inside `pub trait TileContent`, directly after `fn holds_focus`, add:

```rust
    /// The context at this tile's cursor, for `tile::open_with`. Pulled by
    /// the shell when the action runs, so a module needs no handle into the
    /// shell. Empty (the default) whenever the cursor names no single
    /// value; the shell then opens the plain tile-kind picker.
    fn launch_context(&self, _cx: &App) -> LaunchContext {
        LaunchContext::default()
    }
    /// Called once, deferred after the first render, for an occupant that
    /// `ShellView::add_tile` created (not a session restore) and that is the
    /// focused tile on that render. A module that is useless without some
    /// state asks for it here; the default does nothing.
    fn launched(&self, _window: &mut Window, _cx: &mut App) {}
```

Inside `pub trait ModuleFactory`, directly after `fn default_keymap`, add:

```rust
    /// Context fields this kind can open on. Empty (the default) keeps the
    /// kind out of `tile::open_with`'s list.
    fn accepts(&self) -> &'static [ContextField] {
        &[]
    }
    /// Translate a launch context into the table [`Self::create`] reads as
    /// its restored record. The factory owns the translation so the shell
    /// never learns a module's state format. `None` (the default) creates
    /// the tile as a plain add would.
    fn launch_state(&self, _ctx: &LaunchContext) -> Option<toml::Table> {
        None
    }
```

- [ ] **Step 4: Extend the recording fixture.** In `mod recording`:
  1. Add the variant `Launched(TileId)` to `pub enum Recorded`, after `Stack(..)`.
  2. Add two fields to `RecordingFactory`, with doc comments:

```rust
        /// What every occupant this factory creates answers from
        /// `launch_context`. Shared and mutable so a test can change the
        /// source's context AFTER `tile::open_with` has opened its dialog,
        /// and so prove the shell captured it at open.
        pub launch_context: Rc<RefCell<LaunchContext>>,
        /// What `accepts` answers. Empty by default, so every existing
        /// fixture stays out of `tile::open_with`'s list.
        pub accepts: &'static [ContextField],
```

  3. Initialise them in `RecordingFactory::new` with `launch_context: Rc::new(RefCell::new(LaunchContext::default()))` and `accepts: &[]`.
  4. Add a third factory field, `pub edit_on_launch: bool`, documented as: "When set, `launched` opens the insert-mode input exactly as `<kind>::edit` does — the stand-in for a panel that opens its own picker when launched, so a shell test can prove the input still holds the keyboard after the modal's focus return and the next frame's focus restore." It defaults to `false`.
  5. Add `launch_context: Rc<RefCell<LaunchContext>>` and `edit_on_launch: bool` to `RecordingContent`. In `create`, pass `launch_context: self.launch_context.clone()` and `edit_on_launch: self.edit_on_launch`.
  6. In `impl TileContent for RecordingContent`, add:

```rust
        fn launch_context(&self, _cx: &App) -> LaunchContext {
            self.launch_context.borrow().clone()
        }
        fn launched(&self, window: &mut Window, cx: &mut App) {
            self.log.borrow_mut().push(Recorded::Launched(self.tile));
            if self.edit_on_launch {
                // The verb is what `dispatch` matches; the kind is irrelevant.
                self.dispatch(&ActionId("launch::edit".into()), None, window, cx);
            }
        }
```

  7. In `impl ModuleFactory for RecordingFactory`, add:

```rust
        fn accepts(&self) -> &'static [ContextField] {
            self.accepts
        }
        /// `{ underlying = ["<u>"] }`, the market-data panel's own shape,
        /// when this fixture accepts the underlying field.
        fn launch_state(&self, ctx: &LaunchContext) -> Option<toml::Table> {
            if !self.accepts.contains(&ContextField::Underlying) {
                return None;
            }
            let u = ctx.underlying.clone()?;
            let mut t = toml::Table::new();
            t.insert(
                "underlying".into(),
                toml::Value::Array(vec![toml::Value::String(u)]),
            );
            Some(t)
        }
```

- [ ] **Step 5: Build everything that implements the traits**

Run: `cargo check --workspace --all-targets && cargo check -p geode-shell --features test-support --all-targets`
Expected: clean. A `match` on `Recorded` without a wildcard fails to compile; add a `Recorded::Launched(_)` arm there, matching the arm style of the surrounding code.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-core/src/launch.rs crates/geode-core/src/lib.rs crates/geode-shell/src/module.rs
git commit -m "feat(core,shell): launch context vocabulary and trait seams"
```

---

### Task 2: The shell calls `launched` for fresh, focused adds

**Files:**
- Modify: `crates/geode-shell/src/shell/occupants.rs`, in `ensure_occupants` (the creation loop around lines 198–270, and the visibility diff that follows it)
- Modify: `crates/geode-shell/src/shell/tests/mod.rs` (add the `services_with_recorders` fixture)
- Create: `crates/geode-shell/src/shell/tests/launch.rs`, and register it in `crates/geode-shell/src/shell/tests/mod.rs`'s module list next to `mod tilepicker;` (as `mod launch;`)

**Interfaces:**
- Consumes: `TileContent::launched`, `Recorded::Launched(TileId)`, and `RecordingFactory.{launch_context, accepts}` (Task 1)
- Consumes: `RecordingFactory.edit_on_launch` (Task 1)
- Produces: the test fixture `pub(super) fn services_with_recorders(recorders: Vec<crate::module::recording::RecordingFactory>) -> ShellServices`, used by Task 3

- [ ] **Step 1: Extract the roster fixture.** In `tests/mod.rs`, split `services_with_rec_roster_shipping` in two. Move its body, from `let (config, builtin) = …` through the `ShellServices { … }` literal, into a new function. Keep every existing comment where it is.

```rust
/// A `ShellServices` whose roster is exactly `recorders`, in order, built
/// in `main.rs`'s startup order (builtin actions, pick and scope actions,
/// add actions for every recorder kind, module actions, fragments, keymap).
pub(super) fn services_with_recorders(
    recorders: Vec<crate::module::recording::RecordingFactory>,
) -> ShellServices {
    use crate::module::ModuleFactory as _;
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources::default());
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    register_pick_actions(&mut registry, &crate::shell::pickable_columns(&config));
    register_scope_actions(&mut registry, &crate::shell::saved_scopes(&config, false));
    let kinds: Vec<&'static str> = recorders.iter().map(|r| r.kind()).collect();
    crate::defaults::register_add_actions(&mut registry, &kinds);
    let mut roster = crate::module::ModuleRoster::new();
    for r in recorders {
        roster.add(Box::new(r));
    }
    roster.register_actions(&mut registry);
    let (keymap_fragments, keymap_fragment_diagnostics) = roster.keymap_fragments();
    let mod_alias = default_mod();
    let keymap = test_keymap_with_fragments(&registry, &keymap_fragments, &[]);
    let (theme, warnings) = crate::theme::load_bundled();
    assert!(warnings.is_empty(), "{warnings:?}");
    ShellServices {
        config,
        builtin,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path: None,
        roster,
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
        restored_palette_usage: crate::palette_usage::PaletteUsage::new(),
        log: None,
        action_tail: std::sync::Arc::new(std::sync::Mutex::new(
            crate::diagnostics::ActionTail::new(),
        )),
        keymap_diagnostics: Vec::new(),
        keymap_fragments,
        keymap_fragment_diagnostics,
    }
}
```

`services_with_rec_roster_shipping` becomes:

```rust
    let mut recorder = crate::module::recording::RecordingFactory::new("rec");
    recorder.fragment = fragment;
    let log = recorder.log.clone();
    let last_focus = recorder.last_focus.clone();
    let input = recorder.input.clone();
    (services_with_recorders(vec![recorder]), log, last_focus, input)
```

The `ShellServices` field list above must match the struct's current field list exactly. If the compiler reports a missing or unknown field, copy the literal verbatim from the old body instead.

Run: `cargo test -p geode-shell tilepicker`
Expected: all pass, unchanged. This proves the extraction.

- [ ] **Step 2: Write the failing `launched` tests.** Create `crates/geode-shell/src/shell/tests/launch.rs`:

```rust
//! Launch context: `TileContent::launched` reaches a tile `add_tile`
//! created (add, duplicate) once it is focused, never a restored one;
//! `tile::open_with` lists the kinds accepting the focused tile's
//! context and creates the pick with the factory's translated state.

use super::*;
use crate::defaults::AddPlacement;
use crate::module::recording::{Recorded, RecordingFactory};
use geode_core::launch::{ContextField, LaunchContext};

type Log = std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>;

fn launched(log: &Log, tile: TileId) -> usize {
    log.borrow()
        .iter()
        .filter(|r| matches!(r, Recorded::Launched(t) if *t == tile))
        .count()
}

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn focused(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> TileId {
    shell.read_with(cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    })
}

/// An add through the real key route: the new tile is focused on its first
/// render, and hears `launched` exactly once, even across further renders.
#[gpui::test]
fn an_added_tile_hears_launched_once(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let tile = focused(&shell, &vcx);
    assert_eq!(launched(&log, tile), 1, "{:?}", log.borrow());
    draw(&mut vcx);
    assert_eq!(launched(&log, tile), 1, "a later render does not repeat it");
}

/// Duplicate goes through `add_tile`, so the copy hears it too.
#[gpui::test]
fn a_duplicated_tile_hears_launched(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let first = focused(&shell, &vcx);
    dispatch_action(&shell, "workspace::duplicate_horizontal", &mut vcx);
    draw(&mut vcx);
    let copy = focused(&shell, &vcx);
    assert_ne!(copy, first);
    assert_eq!(launched(&log, copy), 1, "{:?}", log.borrow());
}

/// An add whose tile is no longer focused on its first render (focus moved
/// back before the frame) is not prompted: `launched` may take the keyboard,
/// and only the focused tile may do that.
#[gpui::test]
fn an_add_that_is_not_focused_on_its_first_render_is_not_launched(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let first = focused(&shell, &vcx);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.add_tile("rec", AddPlacement::Split(None), None, window, cx);
            s.services.workspaces.active_mut().focus_main_tile(first);
        })
    });
    draw(&mut vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2);
    let second = *tiles.iter().find(|t| **t != first).unwrap();
    assert_eq!(launched(&log, second), 0, "{:?}", log.borrow());
}

/// A restored session never hears `launched`: startup takes focus from
/// nothing, however many tiles were saved.
#[gpui::test]
fn a_restored_tile_is_not_launched(cx: &mut gpui::TestAppContext) {
    let mut table = crate::session::to_toml(
        &Workspaces::new(),
        &crate::session::TileRecords::new(),
        None,
        &crate::palette_usage::PaletteUsage::new(),
    );
    let ws1: toml::Table = r#"
        focused = 1
        [node]
        kind = "leaf"
        id = 1
        [tiles.1]
        module = "rec"
    "#
    .parse()
    .unwrap();
    if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
    }
    let restored = crate::session::from_toml(&table).unwrap();
    let (mut services, log) = test_services_with_log();
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    let (_window, mut vcx) = open_shell(cx, services);
    draw(&mut vcx);
    assert!(
        log.borrow().iter().any(|r| matches!(r, Recorded::Created(TileId(1), _))),
        "fixture: the tile was restored: {:?}",
        log.borrow()
    );
    assert_eq!(launched(&log, TileId(1)), 0, "{:?}", log.borrow());
}
```

If `session::from_toml` rejects a `[tiles.1]` table with no `state`, add `[tiles.1.state]` with `x = 1`, as `tests/session.rs` does with `last_command`. If `focus_main_tile` has a different name on `Workspace`, use the method `open_stack_list` calls for `FocusRegion::Main` (`occupants.rs` near line 365).

Run: `cargo test -p geode-shell launch::`
Expected: `an_added_tile_hears_launched_once` and `a_duplicated_tile_hears_launched` FAIL (count 0). The other two pass vacuously.

- [ ] **Step 3: Implement the scheduling.** In `ensure_occupants`:
  1. Before the `for id in &creation_order` loop, add `let mut fresh: Vec<TileId> = Vec::new();`.
  2. Inside the loop, after `let (factory, state) = match (matched, pending_factory) {…};`, record the pending path:

```rust
            // Only an `add_tile` request (no matching restored record) may be
            // told it was launched: a restore must never take focus.
            let from_add = matched.is_none() && pending_factory.is_some();
```

  3. After `self.occupants.insert(*id, occupant);`, add `if from_add { fresh.push(*id); }`.
  4. After the visibility diff loop `for id in active.difference(&self.visible_tiles) {…}`, add:

```rust
        // Tell a fresh `add_tile` occupant it was launched, once, if it is
        // on screen and the focused tile on this frame. Deferred: this runs
        // inside render, and `launched` may move focus (the market-data
        // panel opens its picker), which must follow any modal focus return
        // the add itself came from.
        let focused_tile = self.services.workspaces.active().focused_tile();
        for id in fresh {
            if active.contains(&id) && focused_tile == Some(id) {
                cx.defer_in(window, move |view, window, cx| {
                    if let Some(o) = view.occupants.get(&id) {
                        o.content.launched(window, cx);
                    }
                });
            }
        }
```

  5. Update the module doc comment of `occupants.rs`, or the `ensure_occupants` doc, with one sentence: "A fresh `add_tile` occupant that is on screen and focused hears `TileContent::launched` once, deferred after the render."

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell launch:: && cargo test -p geode-shell occupants`
Expected: all pass.

- [ ] **Step 4b: The focus race (Review Focus 1 and 2).** Append to `tests/launch.rs`. This test goes through the tile picker's real route: the modal closes, which returns focus, then `add_tile` runs, then `launched` is deferred, then the next frame's `pending_focus_restore` runs. The launched tile's own input must hold the keyboard at the end.

```rust
/// Picking a kind in the tile picker: a tile that takes the keyboard in
/// `launched` (as the market-data panel's picker does) still holds it after
/// the modal's focus return and two more frames.
#[gpui::test]
fn a_launched_tile_keeps_the_keyboard_it_takes(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.edit_on_launch = true;
    let input = rec.input.clone();
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    // The first tile took the keyboard in its own `launched`; give it back
    // (the fixture ships no cancel binding) so the picker opens from the
    // shell, as a trader's `mod+n` would.
    vcx.update(|window, cx| window.blur(cx));
    draw(&mut vcx);
    dispatch_action(&shell, "tile::add", &mut vcx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    draw(&mut vcx);
    let new = focused(&shell, &vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2, "the pick split");
    let held = vcx.update(|window, cx| {
        input
            .borrow()
            .as_ref()
            .is_some_and(|i| i.read(cx).focus_handle(cx).is_focused(window))
    });
    assert!(held, "tile {new:?}'s launched input holds the keyboard");
}
```

The `input` cell is shared by every occupant of the factory, so after the pick it holds the new tile's input.

Run: `cargo test -p geode-shell a_launched_tile_keeps_the_keyboard`
Expected: PASS. If it fails, the defer ordering in Step 3 is wrong (focus returned after `launched`). Fix the ordering in the shell; never in the module.

- [ ] **Step 5: Mutation entries.** Append these after the last entry (see Global Constraints):

```zsh
run_mutation "launch: a restored tile is launched" \
  crates/geode-shell/src/shell/occupants.rs \
  '            let from_add = matched.is_none() && pending_factory.is_some();' \
  '            let from_add = true;' \
  geode-shell a_restored_tile_is_not_launched

run_mutation "launch: an unfocused add is launched" \
  crates/geode-shell/src/shell/occupants.rs \
  '            if active.contains(&id) && focused_tile == Some(id) {' \
  '            if active.contains(&id) {' \
  geode-shell an_add_that_is_not_focused_on_its_first_render_is_not_launched
```

Commit first, then run `zsh scripts/mutation-check.sh "launch: a"`, which runs both. Expected: both reported as caught (killed).

- [ ] **Step 6: Commit**

```bash
git add crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): tell a fresh, focused add_tile occupant it was launched"
```

---

### Task 3: `tile::open_with`

**Files:**
- Modify: `crates/geode-shell/src/shell/choicedialog.rs` (the `Target` and `Pick` enums, `pick_at`, `highlighted_slot`, `chrome`, `open`, `commit`, a new `open_tile_kinds_with`, and a new `ChoiceDialogState::tile_kinds_with`)
- Modify: `crates/geode-shell/src/shell/input.rs` (the dispatch arm beside `tile::add`, plus a notice constant)
- Modify: `crates/geode-shell/src/defaults.rs` (register the action beside `tile::add`)
- Modify: `crates/geode-shell/src/shell/tests/launch.rs` (tests)
- Modify: `docs/superpowers/specs/2026-09-26-launch-context-design.md` §3.3 step 3 (the notice wording)

**Interfaces:**
- Consumes: `LaunchContext::covered_by`, `ModuleFactory::{accepts, launch_state}`, `TileContent::launch_context` (Task 1); `services_with_recorders` (Task 2)
- Produces:
  - `Target::TileKindWith { kinds: Vec<String>, context: LaunchContext }`
  - `Pick::KindWith(String, LaunchContext)`
  - `ChoiceDialogState::tile_kinds_with(kinds, context)`
  - `ChoiceDialogState::title(&self) -> SharedString`
  - the action id `"tile::open_with"`
  - `input::NO_MODULE_OPENS`

- [ ] **Step 1: Write the failing pure test.** In `choicedialog.rs`'s own `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn kinds_with_a_context_title_the_dialog_by_it_and_pick_with_it() {
        let ctx = geode_core::launch::LaunchContext {
            underlying: Some("SPX".into()),
        };
        let state = ChoiceDialogState::tile_kinds_with(["cvi", "dividend"], ctx.clone());
        assert_eq!(state.title().as_ref(), "Open SPX in\u{2026}");
        assert_eq!(state.list.options(), &["Cvi".to_string(), "Dividend".to_string()]);
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::KindWith("cvi".into(), ctx))
        );
        assert_eq!(
            ChoiceDialogState::tile_kinds(["rec"]).title().as_ref(),
            "Add a tile"
        );
    }
```

The `Cvi` spelling comes from `capitalize`, which `tile_kinds` also uses. If `tile_kinds` presents a kind through a different helper, match what it does and fix the expected strings to that helper's output.

Run: `cargo test -p geode-shell kinds_with_a_context`
Expected: compile FAIL (`tile_kinds_with` and `title` not found).

- [ ] **Step 2: Implement the dialog target.** In `choicedialog.rs`:
  1. Add `use geode_core::launch::LaunchContext;` and `use gpui::SharedString;` (merge into the existing gpui import).
  2. Add this variant to `Target`:

```rust
    /// `tile::open_with`: the kinds accepting `context`, which was captured
    /// from the focused tile when the dialog opened (moving that tile's
    /// cursor afterwards does not change what a pick opens).
    TileKindWith {
        kinds: Vec<String>,
        context: LaunchContext,
    },
```

  3. Add this variant to `Pick`:

```rust
    /// `ShellView::add_tile` of this kind, with the factory's
    /// `launch_state` of this context.
    KindWith(String, LaunchContext),
```

  4. Add the constructor and the title:

```rust
    /// The rows for `tile::open_with`: `kinds` (already filtered to those
    /// accepting `context`), in roster order, the highlight on the first.
    pub fn tile_kinds_with<'a>(
        kinds: impl IntoIterator<Item = &'a str>,
        context: LaunchContext,
    ) -> Self {
        let Self { list, target } = Self::tile_kinds(kinds);
        let Target::TileKind { kinds } = target else {
            unreachable!("tile_kinds builds a TileKind target")
        };
        Self {
            list,
            target: Target::TileKindWith { kinds, context },
        }
    }

    /// The modal's title: the chrome's fixed words, or `Open {underlying}…`
    /// for a context launch.
    pub fn title(&self) -> SharedString {
        match &self.target {
            Target::TileKindWith { context, .. } => match &context.underlying {
                Some(u) => format!("Open {u} in\u{2026}").into(),
                None => chrome(&self.target).0.into(),
            },
            Target::Grouping { .. } | Target::TileKind { .. } | Target::LogLevel { .. } => {
                chrome(&self.target).0.into()
            }
        }
    }
```

  5. In `pick_at`, add the arm `Target::TileKindWith { kinds, context } => Pick::KindWith(kinds[declared].clone(), context.clone()),`.
  6. In `highlighted_slot`, extend the `None` arm with `| Pick::KindWith(..)`.
  7. In `chrome`, add `Target::TileKindWith { .. } => ("Open in\u{2026}", "tile", "tile-hints", TILE_HINTS),`. The row selectors stay `tile-choice-<Kind>`.
  8. In `fn open`, replace `let (title, ..) = chrome(&state.target);` with `let title = state.title();`.
  9. Add the opener next to `open_tile_kinds`:

```rust
/// Open on the roster kinds accepting `context` — `tile::open_with` with a
/// non-empty context. The caller has checked at least one kind accepts it.
pub fn open_tile_kinds_with(
    view: &mut ShellView,
    kinds: Vec<&'static str>,
    context: LaunchContext,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let state = ChoiceDialogState::tile_kinds_with(kinds, context);
    open(view, state, window, cx);
}
```

  10. In `commit`, add this arm after `Pick::Kind(kind)`:

```rust
        Pick::KindWith(kind, context) => {
            shell.close_modal(window, cx);
            let state = shell
                .services
                .roster
                .factory(&kind)
                .and_then(|f| f.launch_state(&context));
            shell.add_tile(&kind, AddPlacement::Split(None), state, window, cx);
        }
```

  11. Fix any other exhaustive `match` on `Target` or `Pick` that fails to compile (`cargo check -p geode-shell`) by giving `TileKindWith` / `KindWith` the same arm as `TileKind` / `Kind`.
  12. Update the module doc on lines 1–12 with one sentence: "`tile::open_with` uses the same tile rows, filtered to kinds accepting the focused tile's launch context, titled `Open {underlying} in…`; a pick always splits."

Run: `cargo test -p geode-shell kinds_with_a_context`
Expected: PASS.

- [ ] **Step 3: Write the failing GPUI tests.** Append to `tests/launch.rs`:

```rust
/// Binds `g m` in the recorder's own context beside a competing `g g`, as
/// the blotter and pricer fragments do, so the sequence matcher has a real
/// prefix to resolve.
const LAUNCH_FRAGMENT: &str = "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"g g\" = \"rec::noop\"\n\"g m\" = \"tile::open_with\"\n";

/// A roster of "rec" (the source: reports `context`, accepts the
/// underlying, ships `g m`) and "plain" (accepts nothing). Returns rec's
/// log and its shared context cell.
fn launch_services(context: LaunchContext) -> (ShellServices, Log, std::rc::Rc<std::cell::RefCell<LaunchContext>>) {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(LAUNCH_FRAGMENT);
    rec.accepts = &[ContextField::Underlying];
    *rec.launch_context.borrow_mut() = context;
    let log = rec.log.clone();
    let cell = rec.launch_context.clone();
    let plain = RecordingFactory::new("plain");
    let services = services_with_recorders(vec![rec, plain]);
    assert!(
        services.keymap_fragment_diagnostics.is_empty(),
        "{:?}",
        services.keymap_fragment_diagnostics
    );
    (services, log, cell)
}

fn spx() -> LaunchContext {
    LaunchContext {
        underlying: Some("SPX".into()),
    }
}

fn underlying_state(u: &str) -> toml::Table {
    let mut t = toml::Table::new();
    t.insert(
        "underlying".into(),
        toml::Value::Array(vec![toml::Value::String(u.into())]),
    );
    t
}

fn dialog_target(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> Option<crate::shell::choicedialog::Target> {
    shell.read_with(cx, |s, _| s.choice_dialog.as_ref().map(|d| d.target.clone()))
}

/// `g m` on a tile with an underlying opens `Open SPX in…` listing only
/// the accepting kind; `enter` splits a new tile whose `create` received
/// the factory's translated state.
#[gpui::test]
fn g_m_lists_the_accepting_kinds_and_a_pick_creates_with_the_context(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log, _cell) = launch_services(spx());
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert!(
        matches!(
            dialog_target(&shell, &vcx),
            Some(crate::shell::choicedialog::Target::TileKindWith { ref context, .. })
                if *context == spx()
        ),
        "{:?}",
        dialog_target(&shell, &vcx)
    );
    assert!(vcx.debug_bounds("tile-choice-Rec").is_some());
    assert!(
        vcx.debug_bounds("tile-choice-Plain").is_none(),
        "a kind accepting nothing is not listed"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2, "a pick splits");
    let new = focused(&shell, &vcx);
    let expected = underlying_state("SPX");
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(t, Some(s)) if *t == new && *s == expected)),
        "{:?}",
        log.borrow()
    );
}

/// The context is read when the dialog opens: a cursor move on the source
/// while it is open does not change what the pick creates.
#[gpui::test]
fn the_context_is_captured_when_the_dialog_opens(cx: &mut gpui::TestAppContext) {
    let (services, log, cell) = launch_services(spx());
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    *cell.borrow_mut() = LaunchContext {
        underlying: Some("NDX".into()),
    };
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    let new = focused(&shell, &vcx);
    let expected = underlying_state("SPX");
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(t, Some(s)) if *t == new && *s == expected)),
        "{:?}",
        log.borrow()
    );
}

/// An empty context falls back to the plain tile-kind picker.
#[gpui::test]
fn g_m_with_an_empty_context_opens_the_plain_tile_picker(cx: &mut gpui::TestAppContext) {
    let (services, _log, _cell) = launch_services(LaunchContext::default());
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert!(
        matches!(
            dialog_target(&shell, &vcx),
            Some(crate::shell::choicedialog::Target::TileKind { .. })
        ),
        "{:?}",
        dialog_target(&shell, &vcx)
    );
}

/// No accepting kind: no dialog, and the notice names why.
#[gpui::test]
fn g_m_with_no_accepting_kind_shows_the_notice(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(LAUNCH_FRAGMENT);
    *rec.launch_context.borrow_mut() = spx();
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert!(dialog_target(&shell, &vcx).is_none());
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice),
        Some(crate::shell::input::NO_MODULE_OPENS)
    );
}
```

If `ShellView::notice` or `choice_dialog` is not visible to `tests/`, the existing tests read them the same way (`tests/tilepicker.rs` reads `s.choice_dialog`), so visibility is already `pub(super)` or wider. If `input` is private to `shell`, reference the constant as `super::super::input::NO_MODULE_OPENS`, or make it `pub(crate)`.

Run: `cargo test -p geode-shell launch::g_m launch::the_context`
Expected: FAIL. `tile::open_with` is unregistered, so the fragment's binding is dropped at keymap build and the diagnostics assertion fires.

- [ ] **Step 4: Register and dispatch.** In `defaults.rs`, directly after `action(reg, "tile::add", "Add a tile…", "Tiles");`, add:

```rust
    // Pulls the focused tile's launch context and lists the kinds that
    // accept it. Outside the `tile::add_` prefix, like `tile::add`.
    action(reg, "tile::open_with", "Open with context…", "Tiles");
```

In `input.rs`, next to `NOT_IN_A_STACK`, add:

```rust
/// The notice when no registered kind accepts the focused tile's context.
pub(crate) const NO_MODULE_OPENS: &str = "no module opens on the context at the cursor";
```

and add this arm directly after the `"tile::add"` arm:

```rust
        } else if action.0 == "tile::open_with" {
            // Pull the focused tile's context now; the dialog keeps this copy.
            let context = self
                .services
                .workspaces
                .active()
                .focused_tile()
                .and_then(|t| self.occupants.get(&t))
                .map(|o| o.content.launch_context(cx))
                .unwrap_or_default();
            if context.is_empty() {
                choicedialog::open_tile_kinds(self, window, cx);
            } else {
                let kinds: Vec<&'static str> = self
                    .services
                    .roster
                    .kinds()
                    .into_iter()
                    .filter(|k| {
                        self.services
                            .roster
                            .factory(k)
                            .is_some_and(|f| context.covered_by(f.accepts()))
                    })
                    .collect();
                if kinds.is_empty() {
                    self.notice = Some(NO_MODULE_OPENS);
                    cx.notify();
                } else {
                    choicedialog::open_tile_kinds_with(self, kinds, context, window, cx);
                }
            }
```

A placeholder's content returns the default (empty) context, so no special case is needed.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p geode-shell launch:: && cargo test -p geode-shell tilepicker && cargo test -p geode-shell choicedialog`
Expected: all pass.

- [ ] **Step 6: Amend the spec's notice wording.** In `docs/superpowers/specs/2026-09-26-launch-context-design.md` §3.3 step 3, replace "the notice `no module opens on {underlying}`" with "the notice `no module opens on the context at the cursor` (the shell's notice is a static string)". In §7's shell test list, make the same wording change.

- [ ] **Step 7: Mutation entries**

```zsh
run_mutation "launch: open_with lists kinds that do not accept the context" \
  crates/geode-shell/src/shell/input.rs \
  '                            .is_some_and(|f| context.covered_by(f.accepts()))' \
  '                            .is_some()' \
  geode-shell g_m_lists_the_accepting_kinds_and_a_pick_creates_with_the_context

run_mutation "launch: a pick reads the context at commit" \
  crates/geode-shell/src/shell/choicedialog.rs \
  '                .and_then(|f| f.launch_state(&context));' \
  '                .and_then(|f| f.launch_state(&geode_core::launch::LaunchContext { underlying: Some("NDX".into()) }));' \
  geode-shell the_context_is_captured_when_the_dialog_opens
```

The second mutation stands in for "reading a fresh context at commit". It replaces the captured copy with a different value, and the capture test must catch the difference. Commit, then run `zsh scripts/mutation-check.sh "launch: "`. Expected: all four `launch:` entries so far are caught.

- [ ] **Step 8: Docs.**
  - In `docs/current/shell.md`, in the section on tiles and adding tiles, add a short "Launch context" subsection. It should state:
    - A tile reports `launch_context` on demand.
    - `tile::open_with` lists the kinds whose `accepts` covers it, titled `Open {underlying} in…`, and always splits.
    - An empty context falls back to the tile picker.
    - No accepting kind gives the notice.
    - The context is captured when the dialog opens.
    - `launched` runs once, deferred, for an `add_tile` occupant that is focused on its first render, and never for a restore.
  - In `docs/current/input-and-dialogs.md`, add `tile::open_with` to the choice dialog's list of targets.
  - In `crates/geode-shell/README.md`, add a line for each new trait method to the module map entry for `module.rs`, and mention `open_with` in the `choicedialog.rs` entry.

- [ ] **Step 9: Commit**

```bash
git add -A crates/geode-shell scripts/mutation-check.sh docs
git commit -m "feat(shell): tile::open_with opens an accepting kind on the focused tile's context"
```

---

### Task 4: The market-data panel accepts an underlying and prompts when it has none

**Files:**
- Modify: `crates/geode-marketdata/src/content.rs` (`impl ModuleFactory for MarketDataFactory`, `impl TileContent for MarketDataContent`, and its tests)
- Modify: `crates/geode-marketdata/src/tile.rs` (`MarketDataTile::launched`, a harness helper, and tests)
- Modify: `crates/geode-marketdata/README.md`, `docs/current/features.md`

**Interfaces:**
- Consumes: `ContextField`, `LaunchContext`, and the trait methods (Task 1)
- Produces: `MarketDataTile::launched(&mut self, window: &mut Window, cx: &mut Context<Self>)`

- [ ] **Step 1: Write the failing factory test.** In `content.rs`'s `mod tests`, add a test that builds a factory the way the existing tests there do (reuse their `DataHandle::for_tests()` and `MarketDataFactory::new(data, &CVI, Duration::from_secs(900))` construction), then:

```rust
    #[test]
    fn a_panel_accepts_an_underlying_and_translates_it_to_its_restored_key() {
        let (data, _rx) = geode_data::DataHandle::for_tests();
        let f = MarketDataFactory::new(data, &CVI, std::time::Duration::from_secs(900));
        assert_eq!(f.accepts(), &[geode_core::launch::ContextField::Underlying]);
        let state = f
            .launch_state(&geode_core::launch::LaunchContext {
                underlying: Some("SPX".into()),
            })
            .expect("a state for an underlying");
        assert_eq!(
            state.get("underlying"),
            Some(&toml::Value::Array(vec![toml::Value::String("SPX".into())]))
        );
        assert_eq!(
            f.launch_state(&geode_core::launch::LaunchContext::default()),
            None
        );
    }
```

Match the path of `DataHandle` to how this file's existing tests import it. Bring `ModuleFactory` into scope if the tests module doesn't already have it.

Run: `cargo test -p geode-marketdata a_panel_accepts_an_underlying`
Expected: FAIL (`accepts` returns `&[]`).

- [ ] **Step 2: Implement the factory side.** In `impl ModuleFactory for MarketDataFactory`, after `default_keymap`, add:

```rust
    /// Every panel is one document per underlying, so every panel kind
    /// opens on one.
    fn accepts(&self) -> &'static [ContextField] {
        &[ContextField::Underlying]
    }

    /// `{ underlying = ["<u>"] }`: the one-element display key
    /// `MarketDataTile::new` already restores from, so a launched panel
    /// starts exactly as a restored one on that key would.
    fn launch_state(&self, ctx: &LaunchContext) -> Option<toml::Table> {
        let u = ctx.underlying.clone()?;
        let mut t = toml::Table::new();
        t.insert(
            "underlying".into(),
            toml::Value::Array(vec![toml::Value::String(u)]),
        );
        Some(t)
    }
```

Add `use geode_core::launch::{ContextField, LaunchContext};`.

Run: `cargo test -p geode-marketdata a_panel_accepts_an_underlying`
Expected: PASS.

- [ ] **Step 3: Write the failing tile tests.** In `tile.rs`'s tests, add a harness helper next to `fn visible`:

```rust
        /// The shell's `launched` call, through the trait door.
        fn launched(&self, vcx: &mut gpui::VisualTestContext) {
            vcx.update(|window, cx| self.content.launched(window, cx));
            vcx.run_until_parked();
        }
```

Then add the tests:

```rust
    /// A panel launched with no underlying opens its picker at once, the
    /// field holding the keyboard, and keeps it through the next frame (the
    /// shell's deferred focus restore spares an insert-mode input).
    #[gpui::test]
    fn a_launched_panel_with_no_underlying_opens_the_picker(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.launched(&mut vcx);
        assert_eq!(h.mode(&vcx), "insert");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            h.tile.read_with(&vcx, |t, _| matches!(t.popup, Some(Popup::Picker(_)))),
            "the picker is open"
        );
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "its field holds the keyboard"
        );
    }

    /// A panel launched on an underlying shows it and opens no picker.
    #[gpui::test]
    fn a_launched_panel_on_an_underlying_opens_no_picker(cx: &mut gpui::TestAppContext) {
        let mut state = toml::Table::new();
        state.insert(
            "underlying".into(),
            toml::Value::Array(vec![toml::Value::String("SPX.Z".into())]),
        );
        let (h, mut vcx) = open_with(cx, Some(state));
        h.visible(&mut vcx, true);
        h.launched(&mut vcx);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(h.tile.read_with(&vcx, |t, _| t.popup.is_none()));
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts())
                .contains(&"SPX.Z".to_string())
        );
    }

    /// Without `launched` (a restore), an empty panel opens no picker.
    #[gpui::test]
    fn an_empty_panel_that_was_not_launched_opens_no_picker(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(h.tile.read_with(&vcx, |t, _| t.popup.is_none()));
    }
```

If `popup` is not readable from the tests module, use `h.mode(&vcx) == "insert"` plus `holds_focus` as the picker check. `mode` reads insert only while the picker or an editor is open (see `u_opens_the_picker_typing_filters_and_enter_loads`).

Run: `cargo test -p geode-marketdata launched_panel`
Expected: the first test FAILS (mode stays "normal"); the second passes.

- [ ] **Step 4: Implement.** In `tile.rs`, next to `open_picker`, add:

```rust
    /// The shell created this panel through `add_tile` and it is focused: a
    /// panel with no underlying is useless, so ask for one at once. A
    /// launched panel that already has a key (a context launch, a
    /// duplicate) does nothing. Escape leaves the empty panel, as `u` does.
    pub(crate) fn launched(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.key.is_none() && self.popup.is_none() {
            self.open_picker(window, cx);
        }
    }
```

In `content.rs`'s `impl TileContent for MarketDataContent`, add:

```rust
    fn launched(&self, window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.launched(window, cx))
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p geode-marketdata`
Expected: all pass.

- [ ] **Step 6: Mutation entry**

```zsh
run_mutation "launch: a panel with a key is prompted anyway" \
  crates/geode-marketdata/src/tile.rs \
  '        if self.key.is_none() && self.popup.is_none() {' \
  '        if self.popup.is_none() {' \
  geode-marketdata a_launched_panel_on_an_underlying_opens_no_picker
```

Commit, then run `zsh scripts/mutation-check.sh "prompted anyway"`. Expected: caught.

- [ ] **Step 7: Docs.**
  - In `docs/current/features.md`, market-data section: "A panel opened through an add (palette, tile picker, `open_with`, duplicate) with no underlying opens the underlying picker at once; a restored panel does not. Every panel kind accepts an underlying launch context."
  - In `crates/geode-marketdata/README.md`, add the same two facts to the local invariants.

- [ ] **Step 8: Commit**

```bash
git add -A crates/geode-marketdata scripts/mutation-check.sh docs
git commit -m "feat(marketdata): accept an underlying launch context; prompt when launched empty"
```

---

### Task 5: Blotter source

**Files:**
- Create: `crates/geode-blotter/src/core/launch.rs`, and register it in `crates/geode-blotter/src/core/mod.rs` with `pub mod launch;` (alphabetical, after `format`)
- Modify: `crates/geode-blotter/src/delegate.rs` (a `cursor_underlying` method and a test)
- Modify: `crates/geode-blotter/src/tile.rs` (`BlotterTile::launch_context`)
- Modify: `crates/geode-blotter/src/content.rs` (`launch_context` forwarding, the `g m` binding, a fragment test)
- Modify: `crates/geode-blotter/README.md`, `docs/current/features.md`, `docs/current/keymaps.md`

**Interfaces:**
- Consumes: `LaunchContext` (Task 1); `tile::open_with` (Task 3)
- Produces:
  - `crate::core::launch::{UNDERLYING_COLUMN, underlying_at}`
  - `BlotterDelegate::cursor_underlying(&self) -> Option<String>`

- [ ] **Step 1: Write the failing pure tests.** Create `crates/geode-blotter/src/core/launch.rs`:

```rust
//! The blotter's launch context: the underlying of the cursor row, when
//! the grouping carries one and the row is at or below its level.

use super::expansion::Path;

/// The grouping column naming a row's underlying. The desk's underlying
/// identifier; market-data keys use the same one.
pub const UNDERLYING_COLUMN: &str = "underlying_ref";

/// The underlying on `path` (the cursor row's ancestors' tree texts, root
/// excluded) under `grouping`, or `None`:
/// - when the grouping lacks the column;
/// - for a row above that level (a subtotal, or the grand total's empty path);
/// - for a NULL value, which is `None` in the path, never the text "NULL".
pub fn underlying_at(path: &Path, grouping: &[String]) -> Option<String> {
    let level = grouping.iter().position(|g| g == UNDERLYING_COLUMN)?;
    path.get(level)?.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }
    fn p(parts: &[Option<&str>]) -> Path {
        parts.iter().map(|s| s.map(str::to_string)).collect()
    }

    #[test]
    fn an_underlying_row_and_its_descendants_name_it() {
        let grouping = g(&["lhu", "underlying_ref", "position_ref"]);
        assert_eq!(
            underlying_at(&p(&[Some("L1"), Some("SPX")]), &grouping),
            Some("SPX".into())
        );
        assert_eq!(
            underlying_at(&p(&[Some("L1"), Some("SPX"), Some("P7")]), &grouping),
            Some("SPX".into())
        );
    }

    #[test]
    fn rows_above_the_level_and_groupings_without_it_are_empty() {
        let grouping = g(&["lhu", "underlying_ref"]);
        assert_eq!(underlying_at(&p(&[Some("L1")]), &grouping), None, "subtotal");
        assert_eq!(underlying_at(&p(&[]), &grouping), None, "grand total");
        assert_eq!(
            underlying_at(&p(&[Some("L1"), Some("SPX")]), &g(&["lhu", "currency"])),
            None,
            "no underlying column"
        );
    }

    #[test]
    fn a_null_underlying_is_empty_not_a_made_up_key() {
        let grouping = g(&["lhu", "underlying_ref"]);
        assert_eq!(underlying_at(&p(&[Some("L1"), None]), &grouping), None);
    }
}
```

Run: `cargo test -p geode-blotter core::launch`
Expected: 3 passed. The file is written test and implementation together; the next steps wire it.

- [ ] **Step 2: Write the failing delegate test.** In `delegate.rs`'s tests, add a test that uses the existing `snapshot()`, `view()`, `grouping()` fixture (Root; L1, L2; L1/SPX, L1/NDX):

```rust
    #[test]
    fn the_cursor_underlying_follows_the_cursor_row() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.expansion
            .toggle(path_of(&snapshot(), d.plan.as_ref().unwrap(), 1));
        d.reflatten();
        let at = |d: &BlotterDelegate, snap_row: u32| {
            d.shown.iter().position(|r| *r == snap_row).unwrap()
        };
        d.cursor.row = at(&d, 1);
        assert_eq!(d.cursor_underlying(), None, "L1 is above the underlying level");
        d.cursor.row = at(&d, 3);
        assert_eq!(d.cursor_underlying(), Some("SPX".into()));
        d.cursor.row = at(&d, 4);
        assert_eq!(d.cursor_underlying(), Some("NDX".into()));
        d.cursor.row = at(&d, 0);
        assert_eq!(d.cursor_underlying(), None, "the grand total");
    }
```

If the root row is not in `shown` (a hidden grand total), drop the last two lines and note it in the test's doc comment.

Run: `cargo test -p geode-blotter the_cursor_underlying`
Expected: compile FAIL (`cursor_underlying` not found).

- [ ] **Step 3: Implement.** In `delegate.rs`, next to `cursor_path`:

```rust
    /// The cursor row's underlying under the applied grouping, or `None`
    /// (see [`crate::core::launch::underlying_at`]).
    pub fn cursor_underlying(&self) -> Option<String> {
        let plan = self.plan.as_ref()?;
        crate::core::launch::underlying_at(&self.cursor_path()?, &plan.grouping)
    }
```

In `tile.rs`, add a method on `BlotterTile`:

```rust
    /// The launch context at the cursor: its underlying, when it has one.
    /// In visual mode this is the cursor row, not the selection.
    pub fn launch_context(&self, cx: &App) -> geode_core::launch::LaunchContext {
        geode_core::launch::LaunchContext {
            underlying: self.table.read(cx).delegate().cursor_underlying(),
        }
    }
```

In `content.rs`'s `impl TileContent for BlotterContent`, add:

```rust
    fn launch_context(&self, cx: &App) -> geode_core::launch::LaunchContext {
        self.tile.read(cx).launch_context(cx)
    }
```

In `DEFAULT_KEYMAP`, in the `blotter && mode == normal` table, directly after `"g g" = "blotter::top"`, add `"g m" = "tile::open_with"`. Extend the doc comment above `DEFAULT_KEYMAP` with one sentence: "`g m` binds the shell's `tile::open_with`: a fragment may name any action, only its context must be the module's own."

- [ ] **Step 4: Fragment test.** In `content.rs`'s tests (create `#[cfg(test)] mod tests` if there isn't one), add:

```rust
    /// `g m` sits beside `g g` in normal mode and names the shell's action.
    #[test]
    fn g_m_opens_with_context_in_normal_mode() {
        let t: toml::Table = DEFAULT_KEYMAP.parse().unwrap();
        let normal = t["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["context"].as_str() == Some("blotter && mode == normal"))
            .unwrap();
        assert_eq!(normal["keys"]["g m"].as_str(), Some("tile::open_with"));
        assert_eq!(normal["keys"]["g g"].as_str(), Some("blotter::top"));
    }
```

Run: `cargo test -p geode-blotter`
Expected: all pass.

- [ ] **Step 5: Mutation entries**

```zsh
run_mutation "launch: blotter reads a subtotal as its first child's underlying" \
  crates/geode-blotter/src/core/launch.rs \
  '    path.get(level)?.clone()' \
  '    path.get(level).or(path.last())?.clone()' \
  geode-blotter rows_above_the_level_and_groupings_without_it_are_empty

run_mutation "launch: blotter turns a NULL underlying into text" \
  crates/geode-blotter/src/core/launch.rs \
  '    path.get(level)?.clone()' \
  '    Some(path.get(level)?.clone().unwrap_or_else(|| "NULL".into()))' \
  geode-blotter a_null_underlying_is_empty_not_a_made_up_key
```

These two share an anchor. `--anchors-only` reports DUP as a warning, not a failure. That's acceptable because they mutate different behaviours; add a comment line above them saying so. Commit, then run `zsh scripts/mutation-check.sh "launch: blotter"`. Expected: both caught.

- [ ] **Step 6: Docs.**
  - In `docs/current/features.md`, blotter section: "`g m` opens a panel on the cursor row's `underlying_ref` (the column name is fixed); a row above that level, a grouping without it, or a NULL value opens the plain tile picker."
  - In `docs/current/keymaps.md`, add `g m` → `tile::open_with` to the blotter and pricer default binding tables. Task 6 relies on this.
  - In `crates/geode-blotter/README.md`, add `core/launch.rs` to the module map.

- [ ] **Step 7: Commit**

```bash
git add -A crates/geode-blotter scripts/mutation-check.sh docs
git commit -m "feat(blotter): g m opens a panel on the cursor row's underlying"
```

---

### Task 6: Pricer source

**Files:**
- Modify: `crates/geode-pricer/src/core/sheet.rs` (the `Sheet::sole_underlying` method and tests)
- Modify: `crates/geode-pricer/src/tile.rs` (`PricerTile::launch_context` and a tile test)
- Modify: `crates/geode-pricer/src/content.rs` (forwarding, the `g m` binding, a fragment test)
- Modify: `crates/geode-pricer/README.md`, `docs/current/features.md`

**Interfaces:**
- Consumes: `LaunchContext` (Task 1); `tile::open_with` (Task 3)
- Produces: `Sheet::sole_underlying(&self, row: usize) -> Option<String>`

- [ ] **Step 1: Write the failing sheet tests.** In `sheet.rs`'s `mod tests`, add:

```rust
    fn ndx(strike: f64, kind: OptionKind) -> Instrument {
        let Instrument::Vanilla(mut v) = spx(strike, kind) else {
            unreachable!()
        };
        v.underlying = "NDX".into();
        Instrument::Vanilla(v)
    }

    #[test]
    fn a_line_and_a_leg_name_their_own_underlying() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1), callspread(-5)]);
        assert_eq!(s.sole_underlying(0), Some("SPX".into()), "a line");
        let leg = s.children(1).start;
        assert_eq!(s.sole_underlying(leg), Some("SPX".into()), "a leg");
        assert_eq!(s.sole_underlying(1), Some("SPX".into()), "a package on one underlying");
    }

    #[test]
    fn a_package_across_two_underlyings_names_none() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(1.0, OptionKind::Call), 1),
                line(ndx(2.0, OptionKind::Call), 1),
            ],
        );
        s.apply(Edit::Group {
            first: 0,
            count: 2,
            template: Template::Custom,
            id: None,
        })
        .unwrap();
        assert_eq!(s.sole_underlying(0), None);
    }
```

If `Instrument::Vanilla`'s inner type is not directly mutable like this, build the NDX instrument the way `spx` does, with `underlying: "NDX".into()`. If `Template` is not in scope in the tests module, import it from `super`.

Run: `cargo test -p geode-pricer sole_underlying a_line_and_a_leg a_package_across`
Expected: compile FAIL (`sole_underlying` not found).

- [ ] **Step 2: Implement.** In `impl Sheet`, next to `instrument`:

```rust
    /// The one underlying `row` is on: its own instrument's for a line or a
    /// leg, the legs' shared one for a package. `None` for a package across
    /// several underlyings (or none), or a row with no instrument; a launch
    /// context names one underlying or nothing.
    pub fn sole_underlying(&self, row: usize) -> Option<String> {
        if !self.is_package(row) {
            return self.instrument(row).map(|i| i.underlying().to_string());
        }
        let mut found: Option<&str> = None;
        for leg in self.children(row) {
            let u = self.instrument(leg)?.underlying();
            match found {
                None => found = Some(u),
                Some(f) if f == u => {}
                Some(_) => return None,
            }
        }
        found.map(str::to_string)
    }
```

Run: `cargo test -p geode-pricer core::sheet`
Expected: all pass.

- [ ] **Step 3: Wire the tile.** In `tile.rs`, add to `impl PricerTile`:

```rust
    /// The launch context at the cursor: the cursor row's sole underlying.
    pub(crate) fn launch_context(&self) -> geode_core::launch::LaunchContext {
        geode_core::launch::LaunchContext {
            underlying: self
                .cursor_row()
                .and_then(|g| self.model.rows.get(g))
                .and_then(|r| r.row)
                .and_then(|row| self.sheet.sole_underlying(row)),
        }
    }
```

In `content.rs`'s `impl TileContent for PricerContent`, add:

```rust
    fn launch_context(&self, cx: &App) -> geode_core::launch::LaunchContext {
        self.tile.read(cx).launch_context()
    }
```

In `DEFAULT_KEYMAP`, in the `pricer && mode == normal` table, directly after `"g u" = "pricer::ungroup"`, add `"g m" = "tile::open_with"`. Extend the doc comment's `g` sentence to read "(`g g`, `g p`, `g u`, `g m`)".

- [ ] **Step 4: Tile and fragment tests.** In `tile.rs`'s tests, add:

```rust
    /// The cursor line's underlying is the tile's launch context; an empty
    /// sheet has none.
    #[gpui::test]
    fn the_launch_context_is_the_cursor_lines_underlying(cx: &mut gpui::TestAppContext) {
        let (store, mut record) = seeded(&["SPX Z26 5000 C", "NDX Z26 20000 C"]);
        record.insert("cursor".into(), toml::Value::Integer(2));
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        let ctx = vcx.update(|_, cx| h.content.launch_context(cx));
        assert_eq!(ctx.underlying.as_deref(), Some("NDX"));
        let (h2, mut vcx2) = open(cx);
        let empty = vcx2.update(|_, cx| h2.content.launch_context(cx));
        assert!(empty.is_empty());
    }
```

If the shorthand grammar rejects `NDX`, use another underlying it accepts and adjust the expectation. If `open` cannot be called twice with one `cx`, split the second half into its own `#[gpui::test]`.

In `content.rs`'s tests, add a fragment test with the same shape as the blotter's:

```rust
    #[test]
    fn g_m_opens_with_context_in_normal_mode() {
        let t: toml::Table = DEFAULT_KEYMAP.parse().unwrap();
        let normal = t["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["context"].as_str() == Some("pricer && mode == normal"))
            .unwrap();
        assert_eq!(normal["keys"]["g m"].as_str(), Some("tile::open_with"));
        assert_eq!(normal["keys"]["g u"].as_str(), Some("pricer::ungroup"));
    }
```

Run: `cargo test -p geode-pricer`
Expected: all pass.

- [ ] **Step 5: Mutation entry**

```zsh
run_mutation "launch: a mixed package names its first leg's underlying" \
  crates/geode-pricer/src/core/sheet.rs \
  '                Some(_) => return None,' \
  '                Some(_) => {}' \
  geode-pricer a_package_across_two_underlyings_names_none
```

Commit, then run `zsh scripts/mutation-check.sh "mixed package"`. Expected: caught.

- [ ] **Step 6: Docs.**
  - In `docs/current/features.md`, pricer section: "`g m` opens a panel on the cursor row's underlying: a line's or leg's own, a package's when its legs share one; otherwise the plain tile picker."
  - In `crates/geode-pricer/README.md`, add `sole_underlying` to the sheet entry.

- [ ] **Step 7: Commit**

```bash
git add -A crates/geode-pricer scripts/mutation-check.sh docs
git commit -m "feat(pricer): g m opens a panel on the cursor row's underlying"
```

---

### Task 7: Final verification

- [ ] **Step 1: Full gate**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh "launch"
```

Expected:
- Everything is green.
- `--anchors-only` exits 0. The one DUP warning, for the blotter pair, is acceptable.
- All eight `launch` entries are caught, including "prompted anyway" and "mixed package", whose names contain `launch:`.

- [ ] **Step 2: Display check list.** Record the following for Matthew to check on screen with `cargo run -p geode-app -- --demo`:
  1. mod+n → Cvi opens the panel with the underlying picker focused.
  2. In the blotter, with the grouping including underlying, put the cursor on an SPX row. `g m` then shows "Open SPX in…" with Cvi and Dividend, and Enter splits a CVI panel on SPX.
  3. In the pricer, `g m` on an SPX line does the same.
  4. `g m` on a subtotal opens the plain picker.
  5. Restarting with an empty panel saved opens no picker.

- [ ] **Step 3: Commit anything left, and hand off for merge.**
