# Phase 3b — Shell Hosting and Frame Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the shell able to host a module in a tile — keys, commands,
persistence, the shared frame, and the per-tile command line — so the
blotter (Plan 3c) can be dropped in without touching the shell again.

**Architecture:** The keymap engine gains count prefixes as a per-context
feature, so a module gets `5j` without ever reading a digit. `geode-core`
gains `GroupingSlots`; `geode-shell` gains a pure `Frame` held in a gpui
entity every tile observes, the `TileContent`/`ModuleFactory` hosting
contract with a roster the app fills, per-tile occupants painted inside
the existing tile chrome, a `tiles` table in `session.toml`, a
shell-owned command line with ranked completions, `ctrl+1..9`, the
title-bar readout, config reload wiring for the frame, and requery timing
in the perf overlay. Nothing here names `geode-data`; the only data type
that crosses is `geode_core::query::QueryOutcome`.

**Tech Stack:** Rust 2024, gpui (unpinned git dep, resolved by
`Cargo.lock`), gpui-component at rev `0e2fb7a` (`Input`/`InputState`,
`TitleBar`, `StatusBar`, theme tokens), `toml` / `toml_edit`.

**Spec:** `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md`
§2.1–§2.3, §3, §4, §6.8 — read them first. Phase 3a
(`2026-09-03-phase-3a-data-path.md`) must be merged first: this plan
consumes `geode_core::query::{AsOf, QueryOutcome}`.

## Global Constraints

- **Layering:** `geode-shell` never depends on `geode-data` or on any
  module crate. The hosting contract is expressed in `geode-core` and
  gpui types only.
- **CI:** `cargo fmt --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace`, `cargo bench --workspace
  --no-run`, on macOS and Windows. Every task ends green on all four.
- **No raw colours.** Every colour comes from `cx.theme()` tokens.
- **Nothing may stall the render thread.** Config writes go through
  `cx.background_executor()`, as `theme::persist_to_user_config` does.
- **Pure cores are TDD'd and free of `gpui`:** `keymap`, `frame`,
  `groupings`, `commandline`, `session` all test without a window.
- **Modals open only through `dialog::open_shell_dialog`.** Nothing here
  opens one.
- **Per-frame heap churn is a defect.** The occupant map is touched per
  frame only to diff visibility; the readout strings are the sanctioned
  small-String class the status bar already uses.
- **A binding that has shipped is a promise.** No existing binding moves.
  New: `ctrl+0..9`, and `/`, `:` in the `tile` context only.
- **Commit before you mutate**; run `zsh scripts/mutation-check.sh` after
  every task that adds entries and once unfiltered at the end.
- Commit after each task with the repo's trailers:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1
  ```

## What already exists (do not rebuild)

- `keymap::{Matcher, MatchResult, KeyContext, Predicate, Keymap, Binding,
  Keystroke, Modifiers, build_keymap, parse_keystroke, parse_binding}` —
  `Matcher::press(&keymap, keystroke, &stack) -> MatchResult`,
  `pending()`, `cancel()`. `KeyContext::new(name).flag(..).pair(k, v)`,
  `has_flag`, `get`. Predicates already resolve `Eq` innermost-wins.
- `ShellView` (`shell/mod.rs`): one root `FocusHandle`; `context_stack()`
  returns `["workspace"]` plus `"palette"`; `handle_key_down` routes
  modal → filter input → palette toggle → palette → drag escape → matcher
  → `dispatch(&ActionId, window, cx)`; `pending_focus_restore` is
  consumed at the top of `render`; `tile_cell` paints the placeholder
  label; `apply_reload` diffs keymap docs and theme.
- `ShellServices { config, registry, keymap, mod_alias, workspaces, theme,
  session_path }`, built in `geode-app/src/main.rs::build_shell_services`.
- `session::{to_toml, from_toml, save, load, write_atomic, to_string_pretty}`
  over `Workspaces`; per-workspace `node`/`focused`/`fullscreen`/`region`/
  `docks`.
- `status::status_bar(pending, reload_message, theme_name, cx)`;
  `toolbar::toolbar(filter_input, cx)` with a reserved `flex_1` middle;
  `whichkey::{continuations, render}`; `perf::{FrameHistogram, format_ms}`;
  `perf_overlay::render(hist, toolbar_height, cx)`.
- `palette::{fuzzy_match, highlighted_title (private), render}`,
  `listfilter::{rank, Ranked, nav_command}`, `vimnav::{NavCommand, apply}`,
  `vimfind::{FindStyle, find_match, repeat_find, filter_matches}`.
- `theme::{persist_to_user_config, write_atomic}` — the `toml_edit`
  read-modify-write pattern for `app.toml`.
- `tiling::{TileId, Workspaces, Workspace, Tree, Docks, Dock, FocusRegion,
  DockSide}` — `Workspaces::{spaces, active, active_mut, alloc_tile,
  split_active, switch_epoch}`, `Workspace::{tree, docks, region,
  focused_tile, focus_main_tile}`, `Tree::tiles()`, `Docks::iter()`,
  `Dock::tree()`.
- `defaults::{BUILTIN_KEYMAP, register_builtin_actions}`.
- Tests: `shell::tests::test_services()` builds a `ShellServices` with
  the builtin keymap; `#[gpui::test]` tests open a window with
  `Root::new(view)`, draw with `VisualTestContext`, and drive keys with
  `cx.simulate_keystrokes("ctrl-v")`; `cx.debug_bounds("selector")`
  reads a `debug_selector`'d element's painted bounds.

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-shell/src/keymap/matcher.rs`, `context.rs` | Count prefixes (§3.3). |
| `crates/geode-core/src/groupings.rs` (new) | `GroupingSlots` from `groupings.toml` (§4.2). |
| `crates/geode-shell/src/frame.rs` (new) | Pure `Frame`, `FrameVersions`, `FrameReadout`, slot persistence (§4). |
| `crates/geode-shell/src/module.rs` (new) | `TileContent`, `FindEvent`, `TileOccupant`, `ModuleFactory`, `ModuleRoster`, the placeholder module, a recording test module (§3.1–§3.2). |
| `crates/geode-shell/src/session.rs` | `tiles` table, `TileRecord`, `Restored` (§3.5). |
| `crates/geode-shell/src/commandline.rs` (new) | Pure command-line state: word at cursor, ranked candidates, accept, key vocabulary (§3.4). |
| `crates/geode-shell/src/shell/commandline_view.rs` (new) | Paints the strip and the completion popup. |
| `crates/geode-shell/src/shell/mod.rs` | Occupants, context stack, dispatch fall-through, command line routing, frame entity, reload wiring, `ShellEvent`. |
| `crates/geode-shell/src/shell/{status,toolbar,whichkey,perf_overlay}.rs` | Count, readout, requery rows. |
| `crates/geode-shell/src/perf.rs` | `RequeryStats` (§6.8). |
| `crates/geode-shell/src/defaults.rs` | New actions and bindings. |
| `crates/geode-app/src/main.rs` | Roster construction, restored tiles into services. |
| `scripts/mutation-check.sh` | Entries per task. |

---

### Task 1: Count prefixes in the keymap engine

Spec §2.3, §3.3. A bare digit under a context that declares `counts`
accumulates; the resolved action carries the count.

**Files:**
- Modify: `crates/geode-shell/src/keymap/context.rs` (a `COUNTS` flag
  and a builder)
- Modify: `crates/geode-shell/src/keymap/matcher.rs`
- Modify: `crates/geode-shell/src/keymap/mod.rs` (re-export `MAX_COUNT`)
- Modify: `crates/geode-shell/src/shell/mod.rs:1572` (the `Matched` arm)
  and `dispatch`'s signature
- Modify: `crates/geode-shell/tests/keymap_integration.rs:47-72`,
  `crates/geode-shell/tests/tiling_integration.rs:28,112` (pattern
  shape)
- Modify: `crates/geode-shell/src/shell/status.rs`,
  `crates/geode-shell/src/shell/whichkey.rs` (show the count)
- Test: `matcher.rs` inline

**Interfaces:**
- Produces:
  ```rust
  pub const COUNTS: &str = "counts";            // keymap::context
  impl KeyContext { pub fn counts(self) -> Self } // adds the flag
  pub const MAX_COUNT: u32 = 9999;
  pub enum MatchResult { Matched { action: ActionId, count: Option<u32> }, Pending, NoMatch }
  impl Matcher { pub fn count(&self) -> Option<u32>; }
  fn ShellView::dispatch(&mut self, action: &ActionId, count: Option<u32>, window, cx)
  status::status_bar(pending, count: Option<u32>, reload_message, theme_name, cx)
  whichkey::render(continuations, count: Option<u32>, registry, viewport_width, status_bar_height, cx)
  ```

- [ ] **Step 1: Write the failing tests**

Add to `matcher.rs`'s test module:

```rust
    fn counting() -> Vec<KeyContext> {
        vec![
            KeyContext::new("workspace"),
            KeyContext::new("blotter").pair("mode", "normal").counts(),
        ]
    }

    fn km_counts() -> Keymap {
        keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"j\" = \"b::down\"\n\"g g\" = \"b::top\"\n\"0\" = \"b::first_col\"\n",
            )],
            &["b::down", "b::top", "b::first_col"],
        );
    }

    #[test]
    fn digits_accumulate_under_a_counting_context_and_ride_the_action() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("1"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(1));
        assert_eq!(m.press(&km, ks("2"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(12));
        assert_eq!(
            m.press(&km, ks("j"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::down".into()),
                count: Some(12)
            }
        );
        assert_eq!(m.count(), None, "consumed by the action");
        assert!(m.pending().is_empty());
    }

    #[test]
    fn digits_are_ordinary_keys_outside_a_counting_context() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("5"), &ws()), MatchResult::NoMatch);
        assert_eq!(m.count(), None);
        // The innermost context decides: a counting frame below a
        // non-counting one does not count.
        let stack = vec![
            KeyContext::new("blotter").counts(),
            KeyContext::new("palette"),
        ];
        assert_eq!(m.press(&km, ks("5"), &stack), MatchResult::NoMatch);
    }

    #[test]
    fn a_leading_zero_is_a_key_and_a_later_zero_is_a_digit() {
        // vim: `0` is a motion unless a count has begun.
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(
            m.press(&km, ks("0"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::first_col".into()),
                count: None
            }
        );
        assert_eq!(m.press(&km, ks("1"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("0"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(10));
        assert_eq!(
            m.press(&km, ks("j"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::down".into()),
                count: Some(10)
            }
        );
    }

    #[test]
    fn a_count_survives_a_pending_sequence_and_dies_with_a_dead_end() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("3"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("g"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(3), "still counting through the sequence");
        assert_eq!(
            m.press(&km, ks("g"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::top".into()),
                count: Some(3)
            }
        );

        assert_eq!(m.press(&km, ks("4"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("x"), &counting()), MatchResult::NoMatch);
        assert_eq!(m.count(), None, "a dead end clears the count");
    }

    #[test]
    fn a_digit_inside_a_pending_sequence_is_a_key_not_a_count() {
        // `g 1` is not `1g`: once a sequence has begun, digits are keys.
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("1"), &counting()), MatchResult::NoMatch);
        assert_eq!(m.count(), None);
    }

    #[test]
    fn escape_cancel_and_a_modified_digit() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("7"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("escape"), &counting()), MatchResult::NoMatch);
        assert_eq!(m.count(), None);

        assert_eq!(m.press(&km, ks("7"), &counting()), MatchResult::Pending);
        m.cancel();
        assert_eq!(m.count(), None);

        // ctrl+1 is a chord, never a count digit.
        assert_eq!(m.press(&km, ks("ctrl+1"), &counting()), MatchResult::NoMatch);
        assert_eq!(m.count(), None);
    }

    #[test]
    fn the_count_is_capped() {
        let km = km_counts();
        let mut m = Matcher::default();
        for _ in 0..8 {
            assert_eq!(m.press(&km, ks("9"), &counting()), MatchResult::Pending);
        }
        assert_eq!(m.count(), Some(MAX_COUNT));
    }
```

Change every existing `MatchResult::Matched(ActionId(..))` assertion in
this file to `MatchResult::Matched { action: ActionId(..), count: None }`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell matcher:: 2>&1 | grep -E "^error" | head -5`
Expected: `counts()` and the struct variant do not exist.

- [ ] **Step 3: Implement**

`context.rs`, after `KeyContext::pair`:

```rust
/// The flag a context sets to opt into count prefixes (Phase 3 §3.3):
/// while the innermost context on the stack carries it, bare digits
/// accumulate in the matcher instead of being matched.
pub const COUNTS: &str = "counts";

impl KeyContext {
    /// Opt this context into count prefixes.
    pub fn counts(self) -> Self {
        self.flag(COUNTS)
    }
}
```

`matcher.rs`:

```rust
use super::{KeyContext, Keymap, Keystroke, Modifiers, UNBOUND_ACTION};
use super::context::COUNTS;
use crate::actions::ActionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    Matched {
        action: ActionId,
        /// The count prefix typed before the binding, if any (§3.3).
        count: Option<u32>,
    },
    /// The keystrokes so far are a prefix of at least one binding — or a
    /// count is being typed — awaiting more.
    Pending,
    NoMatch,
}

/// Four digits: more than any tree needs, and a held key cannot
/// overflow anything.
pub const MAX_COUNT: u32 = 9999;

/// Sequence-aware key matcher. One per focus target is unnecessary — the
/// shell holds one and feeds it the active context stack per press.
///
/// Counts are the engine's, not a module's (Phase 3 §2.3): `ActionId`
/// carries no argument, so `5j` has to be assembled here and handed to
/// the action, or every module would end up reading raw digits — which
/// is binding keys by another name.
#[derive(Debug, Default)]
pub struct Matcher {
    pending: Vec<Keystroke>,
    count: Option<u32>,
}

impl Matcher {
    pub fn press(
        &mut self,
        keymap: &Keymap,
        keystroke: Keystroke,
        stack: &[KeyContext],
    ) -> MatchResult {
        // A bare digit with nothing pending, under a counting context,
        // is a count digit — except a leading `0`, which vim keeps as a
        // motion. Once a sequence has begun, digits are keys again.
        if self.pending.is_empty()
            && keystroke.mods == Modifiers::NONE
            && stack.last().is_some_and(|c| c.has_flag(COUNTS))
            && let Some(digit) = count_digit(&keystroke.key)
            && (digit != 0 || self.count.is_some())
        {
            let so_far = self.count.unwrap_or(0);
            self.count = Some(so_far.saturating_mul(10).saturating_add(digit).min(MAX_COUNT));
            return MatchResult::Pending;
        }

        self.pending.push(keystroke);
        let mut exact: Option<&super::Binding> = None;
        let mut has_longer_candidate = false;
        for binding in keymap.bindings() {
            if binding.predicate.as_ref().is_some_and(|p| !p.eval(stack)) {
                continue;
            }
            if binding.keystrokes == self.pending {
                // Bindings are in layer-then-definition order; keep the last.
                exact = Some(binding);
            } else if binding.keystrokes.len() > self.pending.len()
                && binding.keystrokes.starts_with(&self.pending)
            {
                has_longer_candidate = true;
            }
        }
        if let Some(binding) = exact {
            self.pending.clear();
            let count = self.count.take();
            if binding.action.0 == UNBOUND_ACTION {
                return MatchResult::NoMatch;
            }
            return MatchResult::Matched {
                action: binding.action.clone(),
                count,
            };
        }
        if has_longer_candidate {
            return MatchResult::Pending;
        }
        self.pending.clear();
        self.count = None;
        MatchResult::NoMatch
    }

    pub fn pending(&self) -> &[Keystroke] {
        &self.pending
    }

    /// The count typed so far, while one is in flight.
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
        self.count = None;
    }
}

fn count_digit(key: &str) -> Option<u32> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.to_digit(10),
        _ => None,
    }
}
```

`keymap/mod.rs`: re-export `MAX_COUNT` and `COUNTS` beside `Matcher`
and `MatchResult`.

`shell/mod.rs`: the arm at line 1572 becomes

```rust
            MatchResult::Matched { action, count } => {
                self.dispatch(&action, count, window, cx);
                cx.notify();
            }
```

and `dispatch` takes `count: Option<u32>` after `action` — every
existing arm ignores it (`let _ = count;` is not needed; it is simply
unused until Task 3's fall-through). `dispatch_palette_item` passes
`None`.

`status_bar` gains `count: Option<u32>` after `pending`; the left region
shows `format!("{count}")` in the mono face ahead of the pending text
when `Some`. `whichkey::render` gains `count: Option<u32>` after
`continuations`; when `Some` the panel's first row is `count N` in
`muted_foreground`. Update both call sites in `render`.

Update `tests/keymap_integration.rs` assertions to the struct variant
with `count: None`, and `tests/tiling_integration.rs`'s two `Matched(action)`
patterns to `Matched { action, .. }`.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-shell`
Expected: green.

- [ ] **Step 5: Full check and commit**

```bash
git add crates/geode-shell
git commit -m "feat(shell): count prefixes in the keymap engine

Bare digits under a context that declares counts accumulate in the
matcher with vim's leading-zero rule, survive a pending sequence, and
ride the resolved action as Option<u32>. The status bar and which-key
show the count (Phase 3 §2.3, §3.3).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entries** (package `geode-shell`)

```sh
# ---- keymap counts (Phase 3 §3.3)

run_mutation "matcher: digits count only under a counting context" \
  crates/geode-shell/src/keymap/matcher.rs \
  '            && stack.last().is_some_and(|c| c.has_flag(COUNTS))' \
  '            && true' \
  geode-shell

run_mutation "matcher: a leading zero is a key" \
  crates/geode-shell/src/keymap/matcher.rs \
  '            && (digit != 0 || self.count.is_some())' \
  '            && true' \
  geode-shell

run_mutation "matcher: a dead end clears the count" \
  crates/geode-shell/src/keymap/matcher.rs \
  '        self.pending.clear();
        self.count = None;
        MatchResult::NoMatch' \
  '        self.pending.clear();
        MatchResult::NoMatch' \
  geode-shell

run_mutation "matcher: the count is capped" \
  crates/geode-shell/src/keymap/matcher.rs \
  '.min(MAX_COUNT));' \
  ');' \
  geode-shell
```

Run: `zsh scripts/mutation-check.sh "matcher:"` — all `caught`. Commit.

---

### Task 2: `GroupingSlots` and the pure `Frame`

Spec §4.1, §4.2. Slots are numbered, validated against the schema, and
persisted by number; the frame is a pure struct with one counter per
field.

**Files:**
- Create: `crates/geode-core/src/groupings.rs`
- Modify: `crates/geode-core/src/lib.rs` (`pub mod groupings;`)
- Create: `crates/geode-shell/src/frame.rs`
- Modify: `crates/geode-shell/src/lib.rs` (`pub mod frame;`)
- Test: both files inline

**Interfaces:**
- Produces (core):
  ```rust
  pub struct GroupingSlots { .. }   // Debug, Clone, Default, PartialEq
  impl GroupingSlots {
      pub fn from_doc(doc: &MergedDoc, schema: &SchemaSpec, dims: &DerivedDimensions) -> (GroupingSlots, Vec<Diagnostic>);
      pub fn get(&self, slot: u8) -> Option<&[String]>;      // 1..=9
      pub fn set(&mut self, slot: u8, grouping: Vec<String>) -> bool;
      pub fn label(&self, slot: u8) -> Option<String>;        // "lhu / underlying_ref"
      pub fn label_of(grouping: &[String]) -> String;
      pub fn is_empty(&self) -> bool;
  }
  ```
- Produces (shell):
  ```rust
  pub struct FrameVersions { pub scope: u64, pub grouping: u64, pub as_of: u64, pub data: u64, pub config: u64 }  // Copy, Default, PartialEq
  pub struct FrameReadout { pub slot: Option<(u8, String)>, pub scope: String, pub as_of: Option<String> }
  pub struct Frame { .. }
  impl Frame {
      pub fn new(slots: GroupingSlots, user_dir: Option<PathBuf>) -> Frame;
      pub fn versions(&self) -> FrameVersions;
      pub fn scope(&self) -> &Scope;  pub fn set_scope(&mut self, scope: Scope) -> bool;
      pub fn clear_scope(&mut self) -> bool;  pub fn undo_scope(&mut self) -> bool;
      pub fn slots(&self) -> &GroupingSlots;  pub fn active_slot(&self) -> Option<u8>;
      pub fn active_grouping(&self) -> Option<&[String]>;
      pub fn set_active_slot(&mut self, slot: Option<u8>) -> bool;
      pub fn replace_slots(&mut self, slots: GroupingSlots) -> bool;
      pub fn save_slot(&mut self, slot: u8, grouping: Vec<String>) -> Result<(), String>;
      pub fn as_of(&self) -> &AsOf;  pub fn set_as_of(&mut self, as_of: AsOf) -> bool;
      pub fn note_published(&mut self);  pub fn note_config_reloaded(&mut self);
      pub fn effective_scope(&self, tile: &Scope) -> Scope;
      pub fn readout(&self) -> FrameReadout;
      pub fn user_dir(&self) -> Option<&Path>;
  }
  pub fn persist_slot_to_user_config(user_dir: &Path, slot: u8, grouping: &[String]) -> Result<(), String>;
  ```
  `save_slot` sets the slot in memory and returns the persistence
  *request*; the caller (ShellView, Task 6) does the background write via
  `persist_slot_to_user_config`, since a pure struct must not do I/O.

- [ ] **Step 1: Write the failing core tests**

Create `crates/geode-core/src/groupings.rs` with the API stubbed and:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Layer, LayerDoc, Severity, merge_docs};
    use crate::dimensions::DerivedDimensions;
    use crate::schema::SchemaSpec;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn dims() -> DerivedDimensions {
        let text = "[desk]\nfrom = \"book\"\n[desk.values]\nEU = [\"BK000\"]\n";
        let doc = merge_docs("dimensions", &[LayerDoc::builtin("dimensions", text).unwrap()]);
        DerivedDimensions::from_doc(&doc).0
    }

    fn layered(desk: &str, user: &str) -> MergedDoc {
        merge_docs(
            "groupings",
            &[
                LayerDoc {
                    layer: Layer::Desk,
                    name: "groupings".into(),
                    file: "desk/groupings.toml".into(),
                    table: desk.parse().unwrap(),
                },
                LayerDoc {
                    layer: Layer::User,
                    name: "groupings".into(),
                    file: "user/groupings.toml".into(),
                    table: user.parse().unwrap(),
                },
            ],
        )
    }

    #[test]
    fn slots_are_numbered_and_labelled_by_their_grouping_string() {
        let doc = merge_docs(
            "groupings",
            &[LayerDoc::builtin(
                "groupings",
                "config_version = 1\n1 = [\"desk\", \"book\", \"lhu\"]\n2 = [\"lhu\", \"position_ref\"]\n",
            )
            .unwrap()],
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(slots.get(1), Some(&["desk".to_string(), "book".into(), "lhu".into()][..]));
        assert_eq!(slots.label(1).as_deref(), Some("desk / book / lhu"));
        assert_eq!(slots.label(2).as_deref(), Some("lhu / position_ref"));
        assert_eq!(slots.get(3), None);
        assert_eq!(slots.get(0), None);
        assert_eq!(slots.get(10), None);
        assert!(!slots.is_empty());
    }

    #[test]
    fn a_user_layer_overrides_one_slot_and_inherits_the_rest() {
        let doc = layered(
            "1 = [\"book\"]\n2 = [\"lhu\"]\n",
            "2 = [\"position_ref\"]\n",
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(slots.label(1).as_deref(), Some("book"));
        assert_eq!(slots.label(2).as_deref(), Some("position_ref"));
    }

    #[test]
    fn an_unknown_column_is_an_error_for_that_slot_only() {
        let doc = merge_docs(
            "groupings",
            &[LayerDoc::builtin("groupings", "1 = [\"book\", \"nonesuch\"]\n2 = [\"lhu\"]\n").unwrap()],
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert_eq!(slots.get(1), None);
        assert!(slots.get(2).is_some());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("slot 1") && diags[0].message.contains("nonesuch"));
    }

    #[test]
    fn bad_keys_and_shapes_warn_and_are_ignored() {
        let doc = merge_docs(
            "groupings",
            &[LayerDoc::builtin(
                "groupings",
                "config_version = 1\nfoo = [\"book\"]\n0 = [\"book\"]\n10 = [\"book\"]\n3 = \"book\"\n4 = []\n",
            )
            .unwrap()],
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert!(slots.is_empty(), "{slots:?}");
        assert_eq!(diags.len(), 5, "{diags:?}");
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }

    #[test]
    fn set_replaces_a_slot_in_memory() {
        let mut slots = GroupingSlots::default();
        assert!(slots.set(3, vec!["book".into()]));
        assert!(!slots.set(0, vec!["book".into()]));
        assert!(!slots.set(3, Vec::new()), "an empty grouping is not a slot");
        assert_eq!(slots.label(3).as_deref(), Some("book"));
        assert_eq!(GroupingSlots::label_of(&["a".to_string(), "b".into()]), "a / b");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core groupings:: 2>&1 | tail -3`

- [ ] **Step 3: Implement `GroupingSlots`**

```rust
//! The nine grouping slots (foundation §4.4, Phase 3 §4.2). Numbered,
//! not named: users rebind them often and a slot is referred to by its
//! number and its grouping string — `lhu / underlying_ref / position_ref`.
//!
//! `groupings.toml` is atomic at depth one, so a user layer overriding
//! slot `3` replaces only slot 3 and inherits the rest.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::SchemaSpec;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupingSlots {
    slots: [Option<Vec<String>>; 9],
}

fn index(slot: u8) -> Option<usize> {
    (1..=9).contains(&slot).then(|| slot as usize - 1)
}

impl GroupingSlots {
    pub fn from_doc(
        doc: &MergedDoc,
        schema: &SchemaSpec,
        dims: &DerivedDimensions,
    ) -> (GroupingSlots, Vec<Diagnostic>) {
        let mut out = GroupingSlots::default();
        let mut diags = Vec::new();
        let known = |column: &str| {
            dims.get(column).is_some()
                || schema
                    .datasets
                    .iter()
                    .any(|ds| ds.column(column).is_some())
        };
        for (key, value) in &doc.value {
            if key == "config_version" {
                continue;
            }
            let slot = match key.parse::<u8>().ok().filter(|s| (1..=9).contains(s)) {
                Some(s) => s,
                None => {
                    diags.push(Diagnostic {
                        severity: Severity::Warning,
                        layer: None,
                        file: None,
                        message: format!(
                            "groupings: key '{key}' is not a slot number 1–9; ignored"
                        ),
                    });
                    continue;
                }
            };
            let grouping: Vec<String> = match value.as_array() {
                Some(a) => a
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect(),
                None => {
                    diags.push(Diagnostic {
                        severity: Severity::Warning,
                        layer: None,
                        file: None,
                        message: format!("groupings: slot {slot} must be an array of column names"),
                    });
                    continue;
                }
            };
            if grouping.is_empty() {
                diags.push(Diagnostic {
                    severity: Severity::Warning,
                    layer: None,
                    file: None,
                    message: format!("groupings: slot {slot} is empty; ignored"),
                });
                continue;
            }
            if let Some(unknown) = grouping.iter().find(|c| !known(c)) {
                diags.push(Diagnostic {
                    severity: Severity::Error,
                    layer: None,
                    file: None,
                    message: format!(
                        "groupings: slot {slot} names '{unknown}', which no dataset or \
                         derived dimension declares; slot dropped"
                    ),
                });
                continue;
            }
            out.set(slot, grouping);
        }
        (out, diags)
    }

    pub fn get(&self, slot: u8) -> Option<&[String]> {
        self.slots.get(index(slot)?)?.as_deref()
    }

    /// `false` for a slot outside 1–9 or an empty grouping.
    pub fn set(&mut self, slot: u8, grouping: Vec<String>) -> bool {
        let Some(i) = index(slot) else {
            return false;
        };
        if grouping.is_empty() {
            return false;
        }
        self.slots[i] = Some(grouping);
        true
    }

    pub fn label(&self, slot: u8) -> Option<String> {
        self.get(slot).map(Self::label_of)
    }

    /// The grouping string a slot is known by everywhere.
    pub fn label_of(grouping: &[String]) -> String {
        grouping.join(" / ")
    }

    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
}
```

`lib.rs`: `pub mod groupings;` after `pub mod dimensions;`.

- [ ] **Step 4: Run the core tests**

Run: `cargo test -p geode-core groupings::`
Expected: 5 passed.

- [ ] **Step 5: Write the failing frame tests**

Create `crates/geode-shell/src/frame.rs` with the API stubbed and:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::{DimensionSelection, Scope};

    fn slots() -> GroupingSlots {
        let mut s = GroupingSlots::default();
        s.set(1, vec!["book".into(), "lhu".into()]);
        s.set(2, vec!["underlying_ref".into()]);
        s
    }

    fn book_scope(book: &str) -> Scope {
        Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![book.into()],
            }],
            ..Scope::default()
        }
    }

    #[test]
    fn each_mutation_bumps_exactly_its_own_counter() {
        let mut f = Frame::new(slots(), None);
        let v0 = f.versions();

        assert!(f.set_scope(book_scope("BK000")));
        let v1 = f.versions();
        assert_eq!(v1.scope, v0.scope + 1);
        assert_eq!((v1.grouping, v1.as_of, v1.data, v1.config), (v0.grouping, v0.as_of, v0.data, v0.config));

        assert!(f.set_active_slot(Some(2)));
        let v2 = f.versions();
        assert_eq!(v2.grouping, v1.grouping + 1);
        assert_eq!(v2.scope, v1.scope);

        assert!(f.set_as_of(AsOf::At(chrono::DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z").unwrap().with_timezone(&chrono::Utc))));
        assert_eq!(f.versions().as_of, v2.as_of + 1);

        f.note_published();
        assert_eq!(f.versions().data, v2.data + 1);
        f.note_config_reloaded();
        assert_eq!(f.versions().config, v2.config + 1);
    }

    #[test]
    fn an_unchanged_value_bumps_nothing() {
        let mut f = Frame::new(slots(), None);
        let v0 = f.versions();
        assert!(!f.set_scope(Scope::default()));
        assert!(!f.set_active_slot(None));
        assert!(!f.set_as_of(AsOf::Live));
        assert_eq!(f.versions(), v0);
    }

    #[test]
    fn an_empty_slot_cannot_be_activated() {
        let mut f = Frame::new(slots(), None);
        assert!(!f.set_active_slot(Some(5)));
        assert_eq!(f.active_slot(), None);
        assert!(f.set_active_slot(Some(1)));
        assert_eq!(f.active_grouping(), Some(&["book".to_string(), "lhu".into()][..]));
        assert!(f.set_active_slot(None));
        assert_eq!(f.active_grouping(), None);
    }

    #[test]
    fn scope_clear_remembers_one_level_and_undo_restores_it() {
        let mut f = Frame::new(slots(), None);
        f.set_scope(book_scope("BK000"));
        assert!(f.clear_scope());
        assert!(f.scope().is_empty());
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &book_scope("BK000"));
        assert!(!f.undo_scope(), "one level only");
        // Setting a new scope also remembers the previous one.
        f.set_scope(book_scope("BK001"));
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &book_scope("BK000"));
    }

    #[test]
    fn effective_scope_composes_global_and_tile() {
        let mut f = Frame::new(slots(), None);
        f.set_scope(book_scope("BK000"));
        let tile = Scope {
            text: Some("spx".into()),
            ..Scope::default()
        };
        let eff = f.effective_scope(&tile);
        assert_eq!(eff.dimensions, book_scope("BK000").dimensions);
        assert_eq!(eff.text.as_deref(), Some("spx"));
        assert_eq!(f.effective_scope(&Scope::default()), book_scope("BK000"));
    }

    #[test]
    fn replacing_slots_bumps_config_and_grouping_and_drops_a_vanished_active_slot() {
        let mut f = Frame::new(slots(), None);
        f.set_active_slot(Some(2));
        let v = f.versions();
        let mut fewer = GroupingSlots::default();
        fewer.set(1, vec!["book".into()]);
        assert!(f.replace_slots(fewer));
        assert_eq!(f.active_slot(), None, "slot 2 no longer exists");
        assert_eq!(f.versions().config, v.config + 1);
        assert_eq!(f.versions().grouping, v.grouping + 1);
    }

    #[test]
    fn saving_a_slot_updates_memory_and_bumps_grouping_only_when_active() {
        let mut f = Frame::new(slots(), None);
        let v = f.versions();
        assert!(f.save_slot(3, vec!["lhu".into()]).is_ok());
        assert_eq!(f.slots().label(3).as_deref(), Some("lhu"));
        assert_eq!(f.versions().grouping, v.grouping, "not the active slot");
        f.set_active_slot(Some(3));
        let v = f.versions();
        assert!(f.save_slot(3, vec!["book".into()]).is_ok());
        assert_eq!(f.versions().grouping, v.grouping + 1, "the active slot changed");
        assert!(f.save_slot(0, vec!["book".into()]).is_err());
        assert!(f.save_slot(3, Vec::new()).is_err());
    }

    #[test]
    fn the_readout_names_the_slot_scope_and_as_of() {
        let mut f = Frame::new(slots(), None);
        let r = f.readout();
        assert_eq!(r.slot, None);
        assert_eq!(r.scope, "");
        assert_eq!(r.as_of, None);

        f.set_active_slot(Some(1));
        f.set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into(), "BK001".into(), "BK002".into()],
            }],
            text: Some("spx".into()),
            expression: geode_core::scope::parse_expr("delta01 > 5").ok(),
            ..Scope::default()
        });
        f.set_as_of(AsOf::At(
            chrono::DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        ));
        let r = f.readout();
        assert_eq!(r.slot, Some((1, "book / lhu".to_string())));
        assert_eq!(r.scope, "book ∈ {3} · text \"spx\" · expr");
        assert_eq!(r.as_of.as_deref(), Some("2026-09-03 14:05"));
    }

    #[test]
    fn a_slot_is_persisted_as_a_bare_numeric_key() {
        let dir = tempfile::tempdir().unwrap();
        persist_slot_to_user_config(dir.path(), 3, &["lhu".into(), "position_ref".into()]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(table["config_version"].as_integer(), Some(1));
        assert_eq!(
            table["3"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>(),
            vec!["lhu", "position_ref"]
        );
        // A second save keeps the first slot.
        persist_slot_to_user_config(dir.path(), 5, &["book".into()]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert!(table.contains_key("3") && table.contains_key("5"));
    }
}
```

- [ ] **Step 6: Implement `Frame`**

```rust
//! The shared frame (foundation §4, Phase 3 §4): global scope, the active
//! grouping slot, as-of, and the data and config generations, as one
//! value every tile observes. Pure: `ShellView` holds it in a gpui
//! entity and notifies; a module reads it through that entity.
//!
//! Every mutation bumps exactly the counters it affects, so a tile can
//! compare the fields it follows against the ones it last acted on with
//! one integer compare each — a pinned tile ignores `grouping`, an
//! unscoped tile ignores `scope`, every tile follows `as_of`, `data` and
//! `config` (§4.1).

use crate::perf::RequeryStats;
use geode_core::groupings::GroupingSlots;
use geode_core::query::AsOf;
use geode_core::scope::Scope;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, value};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameVersions {
    pub scope: u64,
    pub grouping: u64,
    pub as_of: u64,
    pub data: u64,
    pub config: u64,
}

/// What the title bar shows (§4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameReadout {
    pub slot: Option<(u8, String)>,
    pub scope: String,
    pub as_of: Option<String>,
}

#[derive(Debug)]
pub struct Frame {
    scope: Scope,
    previous_scope: Option<Scope>,
    slots: GroupingSlots,
    active_slot: Option<u8>,
    as_of: AsOf,
    versions: FrameVersions,
    /// Requery timing the blotter records (Phase 3 §6.8). Here because
    /// the frame is the one shell-side handle every module holds.
    pub requery: RequeryStats,
    user_dir: Option<PathBuf>,
}

impl Frame {
    pub fn new(slots: GroupingSlots, user_dir: Option<PathBuf>) -> Frame {
        Frame {
            scope: Scope::default(),
            previous_scope: None,
            slots,
            active_slot: None,
            as_of: AsOf::Live,
            versions: FrameVersions::default(),
            requery: RequeryStats::new(),
            user_dir,
        }
    }

    pub fn versions(&self) -> FrameVersions {
        self.versions
    }

    pub fn user_dir(&self) -> Option<&Path> {
        self.user_dir.as_deref()
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Replace the global scope, remembering the previous one for
    /// `undo_scope`. `false` when nothing changed.
    pub fn set_scope(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        self.previous_scope = Some(std::mem::replace(&mut self.scope, scope));
        self.versions.scope += 1;
        true
    }

    pub fn clear_scope(&mut self) -> bool {
        self.set_scope(Scope::default())
    }

    /// One level (§4.3). Swaps rather than pops, so undo-undo returns.
    pub fn undo_scope(&mut self) -> bool {
        let Some(previous) = self.previous_scope.take() else {
            return false;
        };
        if previous == self.scope {
            return false;
        }
        self.previous_scope = Some(std::mem::replace(&mut self.scope, previous));
        self.versions.scope += 1;
        true
    }

    pub fn slots(&self) -> &GroupingSlots {
        &self.slots
    }

    pub fn active_slot(&self) -> Option<u8> {
        self.active_slot
    }

    pub fn active_grouping(&self) -> Option<&[String]> {
        self.slots.get(self.active_slot?)
    }

    /// `Some(n)` activates a filled slot; `None` returns following tiles
    /// to their views' own grouping. `false` when nothing changed or the
    /// slot is empty.
    pub fn set_active_slot(&mut self, slot: Option<u8>) -> bool {
        if let Some(n) = slot
            && self.slots.get(n).is_none()
        {
            return false;
        }
        if self.active_slot == slot {
            return false;
        }
        self.active_slot = slot;
        self.versions.grouping += 1;
        true
    }

    /// A reloaded `groupings.toml` (§4.5). Bumps `config`, and `grouping`
    /// too because the active slot's contents may have changed; an active
    /// slot that no longer exists is cleared.
    pub fn replace_slots(&mut self, slots: GroupingSlots) -> bool {
        if self.slots == slots {
            return false;
        }
        self.slots = slots;
        if self.active_slot.is_some_and(|n| self.slots.get(n).is_none()) {
            self.active_slot = None;
        }
        self.versions.config += 1;
        self.versions.grouping += 1;
        true
    }

    /// `:group save N`: set the slot in memory. The caller persists with
    /// [`persist_slot_to_user_config`] off the UI thread.
    pub fn save_slot(&mut self, slot: u8, grouping: Vec<String>) -> Result<(), String> {
        if !self.slots.set(slot, grouping) {
            return Err(format!("slot must be 1–9 and the grouping non-empty (got {slot})"));
        }
        if self.active_slot == Some(slot) {
            self.versions.grouping += 1;
        }
        Ok(())
    }

    pub fn as_of(&self) -> &AsOf {
        &self.as_of
    }

    pub fn set_as_of(&mut self, as_of: AsOf) -> bool {
        if self.as_of == as_of {
            return false;
        }
        self.as_of = as_of;
        self.versions.as_of += 1;
        true
    }

    pub fn note_published(&mut self) {
        self.versions.data += 1;
    }

    pub fn note_config_reloaded(&mut self) {
        self.versions.config += 1;
    }

    /// Global AND tile (foundation §4.2). Phase 3 passes an empty tile
    /// layer; `:filter` will fill it.
    pub fn effective_scope(&self, tile: &Scope) -> Scope {
        self.scope.and_then(tile)
    }

    pub fn readout(&self) -> FrameReadout {
        let slot = self
            .active_slot
            .and_then(|n| self.slots.label(n).map(|l| (n, l)));
        let mut parts: Vec<String> = Vec::new();
        for d in &self.scope.dimensions {
            if !d.values.is_empty() {
                parts.push(format!("{} ∈ {{{}}}", d.column, d.values.len()));
            }
        }
        if let Some(t) = &self.scope.text {
            parts.push(format!("text \"{t}\""));
        }
        if self.scope.expression.is_some() {
            parts.push("expr".into());
        }
        if self.scope.impossible {
            parts.push("∅".into());
        }
        let as_of = match &self.as_of {
            AsOf::Live => None,
            AsOf::At(t) => Some(t.format("%Y-%m-%d %H:%M").to_string()),
        };
        FrameReadout {
            slot,
            scope: parts.join(" · "),
            as_of,
        }
    }
}

/// Write one slot into the user layer's `groupings.toml` as a bare
/// numeric key, keeping every other key (Phase 3 §4.2). The same
/// `toml_edit` read-modify-write and atomic rename `theme::
/// persist_to_user_config` uses.
pub fn persist_slot_to_user_config(
    user_dir: &Path,
    slot: u8,
    grouping: &[String],
) -> Result<(), String> {
    if !(1..=9).contains(&slot) || grouping.is_empty() {
        return Err(format!("slot {slot} out of range or empty grouping"));
    }
    let path = user_dir.join("groupings.toml");
    let existed = path.exists();
    let mut doc = if existed {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        text.parse::<DocumentMut>().map_err(|e| {
            format!("failed to parse {}: {e} (file left untouched)", path.display())
        })?
    } else {
        DocumentMut::new()
    };
    if !existed {
        doc["config_version"] = value(1_i64);
    }
    let mut array = toml_edit::Array::new();
    for g in grouping {
        array.push(g.as_str());
    }
    doc[slot.to_string().as_str()] = value(array);
    crate::theme::write_atomic(user_dir, &path, &doc.to_string())
}
```

`RequeryStats::new()` is Task 7's; for this task add a stub in
`perf.rs`:

```rust
/// Requery timing (Phase 3 §6.8). Filled in by Task 7; the frame carries
/// it from the start so its shape does not change.
#[derive(Debug, Default)]
pub struct RequeryStats {}
impl RequeryStats {
    pub const fn new() -> Self {
        RequeryStats {}
    }
}
```

`geode-shell` needs `chrono` for the readout's `format`: add
`chrono = "0.4.42"` to its `[dependencies]` (already in the tree through
`geode-core`). `lib.rs`: `pub mod frame;` between `fonts` and
`fontsize`.

- [ ] **Step 7: Run**

Run: `cargo test -p geode-shell frame::`
Expected: 9 passed.

- [ ] **Step 8: Full check and commit**

```bash
git add crates/geode-core crates/geode-shell
git commit -m "feat: GroupingSlots and the pure Frame

Numbered slots from groupings.toml, validated per slot and layered per
slot; a pure Frame with one version counter per field, one level of
scope undo, and a readout for the title bar (Phase 3 §4.1, §4.2).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 9: Harness entries**

```sh
# ---- grouping slots and the frame (Phase 3 §4)

run_mutation "groupings: an unknown column drops the slot" \
  crates/geode-core/src/groupings.rs \
  '            if let Some(unknown) = grouping.iter().find(|c| !known(c)) {' \
  '            if let Some(unknown) = grouping.iter().find(|c| !known(c) && false) {' \
  geode-core

run_mutation "frame: an empty slot cannot be activated" \
  crates/geode-shell/src/frame.rs \
  '            && self.slots.get(n).is_none()' \
  '            && false' \
  geode-shell

run_mutation "frame: set_scope bumps only the scope counter" \
  crates/geode-shell/src/frame.rs \
  '        self.previous_scope = Some(std::mem::replace(&mut self.scope, scope));
        self.versions.scope += 1;' \
  '        self.previous_scope = Some(std::mem::replace(&mut self.scope, scope));
        self.versions.scope += 1;
        self.versions.grouping += 1;' \
  geode-shell

run_mutation "frame: a vanished active slot is cleared on reload" \
  crates/geode-shell/src/frame.rs \
  '        if self.active_slot.is_some_and(|n| self.slots.get(n).is_none()) {' \
  '        if false {' \
  geode-shell
```

Run: `zsh scripts/mutation-check.sh "groupings:"` and `"frame:"` — all
`caught`. Commit.

---

### Task 3: The hosting contract — occupants in tiles

Spec §2.1, §3.1–§3.3. A tile is still a `TileId`; the shell keeps a map
from id to occupant, paints the occupant's view inside the existing tile
chrome, pushes its key context, and hands it unknown actions.

**Files:**
- Create: `crates/geode-shell/src/module.rs`
- Modify: `crates/geode-shell/src/lib.rs` (`pub mod module;`)
- Modify: `crates/geode-shell/src/shell/mod.rs` — `ShellServices`
  (`roster`), `ShellView` (`frame`, `occupants`, `visible_tiles`),
  `new`, `context_stack`, `dispatch`, `render` (`ensure_occupants`,
  `tile_cell`, tile click), `tests`
- Modify: `crates/geode-app/src/main.rs` (`build_shell_services` builds a
  roster and registers its actions before `build_keymap`)
- Test: `module.rs` inline; `shell/mod.rs` tests (`TestAppContext`)

**Interfaces:**
- Produces:
  ```rust
  pub enum FindEvent { Changed(String), Committed(String), Cancelled }
  pub trait TileContent {
      fn key_context(&self, cx: &App) -> KeyContext;
      fn dispatch(&self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut App) -> bool;
      fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String>;
      fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String>;
      fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App);
      fn deliver(&self, outcome: QueryOutcome, window: &mut Window, cx: &mut App);
      fn set_visible(&self, visible: bool, cx: &mut App);
      fn serialize(&self, cx: &App) -> toml::Table;
  }
  pub struct TileOccupant { pub kind: &'static str, pub view: AnyView, pub content: Box<dyn TileContent> }
  pub trait ModuleFactory {
      fn kind(&self) -> &'static str;
      fn register_actions(&self, registry: &mut ActionRegistry);
      fn create(&self, tile: TileId, restored: Option<&toml::Table>, frame: Entity<Frame>,
                window: &mut Window, cx: &mut App) -> TileOccupant;
  }
  pub struct ModuleRoster { .. }
  impl ModuleRoster {
      pub fn new(default_kind: impl Into<String>) -> ModuleRoster;
      pub fn add(&mut self, factory: Box<dyn ModuleFactory>);
      pub fn register_actions(&self, registry: &mut ActionRegistry);
      pub fn factory(&self, kind: &str) -> Option<&dyn ModuleFactory>;
      pub fn default_factory(&self) -> Option<&dyn ModuleFactory>;
      pub fn kinds(&self) -> Vec<&'static str>;
  }
  pub mod placeholder { pub struct PlaceholderFactory; /* kind "placeholder" */ }
  #[cfg(any(test, feature = "test-support"))]
  pub mod recording { pub struct RecordingFactory { pub log: Rc<RefCell<Vec<Recorded>>>, pub completions: Vec<String>, pub command_result: Result<(), String> }
                      pub enum Recorded { Created(TileId, Option<toml::Table>), Dispatch(TileId, ActionId, Option<u32>), Command(TileId, String), Find(TileId, FindEvent), Visible(TileId, bool), Delivered(TileId, u64) } }
  impl ShellView { pub fn frame(&self) -> &Entity<Frame>; pub fn deliver(&mut self, outcome: QueryOutcome, window, cx); pub fn occupant_kind(&self, tile: TileId) -> Option<&'static str>; }
  ShellServices { …, pub roster: ModuleRoster, pub restored_tiles: BTreeMap<u64, TileRecord> }
  ```
  `TileRecord` is defined in Task 4; for this task it is
  `pub struct TileRecord { pub kind: String, pub state: toml::Table }` in
  `session.rs`, and `restored_tiles` is empty everywhere until Task 4.
  `geode-shell` gains a `test-support` feature (`[features] test-support
  = []`) so Plan 3c's blotter tests can use the recording module.

- [ ] **Step 1: Write the failing pure tests**

`module.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_roster_finds_factories_by_kind_and_names_the_default() {
        let mut roster = ModuleRoster::new("rec");
        assert!(roster.default_factory().is_none(), "nothing added yet");
        roster.add(Box::new(recording::RecordingFactory::new("rec")));
        roster.add(Box::new(placeholder::PlaceholderFactory));
        assert_eq!(roster.kinds(), vec!["rec", "placeholder"]);
        assert_eq!(roster.factory("rec").map(|f| f.kind()), Some("rec"));
        assert_eq!(roster.default_factory().map(|f| f.kind()), Some("rec"));
        assert!(roster.factory("nonesuch").is_none());
    }

    #[test]
    fn registering_actions_delegates_to_every_factory_once() {
        let mut roster = ModuleRoster::new("rec");
        roster.add(Box::new(recording::RecordingFactory::new("rec")));
        let mut registry = ActionRegistry::default();
        roster.register_actions(&mut registry);
        assert!(registry.contains(&ActionId("rec::noop".into())));
        // Registering twice is the roster's caller's mistake, and the
        // registry says so rather than silently duplicating.
        let mut again = ActionRegistry::default();
        roster.register_actions(&mut again);
        assert_eq!(again.iter().count(), 1);
    }
}
```

- [ ] **Step 2: Write the failing `TestAppContext` tests**

Add to `shell/mod.rs`'s test module a services builder with a roster and
four tests:

```rust
    /// `test_services` with a recording module as the default occupant.
    fn services_with_recorder() -> (ShellServices, std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>) {
        let recorder = crate::module::recording::RecordingFactory::new("rec");
        let log = recorder.log.clone();
        let mut services = test_services();
        let mut roster = crate::module::ModuleRoster::new("rec");
        roster.add(Box::new(recorder));
        roster.register_actions(&mut services.registry);
        // The keymap must be rebuilt after the module's actions exist,
        // exactly as `main.rs` orders it, plus a binding into the
        // module's own context so a key can be seen to reach it.
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let module_doc = LayerDoc::builtin(
            "keymap",
            "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"j\" = \"rec::noop\"\n",
        )
        .unwrap();
        let (keymap, diags) = build_keymap(&[doc, module_doc], default_mod(), &services.registry);
        assert!(diags.is_empty(), "{diags:?}");
        services.keymap = keymap;
        services.roster = roster;
        (services, log)
    }

    fn open_shell(cx: &mut gpui::TestAppContext, services: ShellServices) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::shell::dialog::init_reclaimed_keybindings);
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (window, vcx)
    }

    fn shell_of(window: &gpui::WindowHandle<Root>, cx: &gpui::VisualTestContext) -> Entity<ShellView> {
        window
            .root(cx)
            .unwrap()
            .read_with(cx, |root, _| root.view().clone().downcast::<ShellView>().unwrap())
    }

    #[gpui::test]
    fn a_split_creates_an_occupant_of_the_default_kind_and_paints_it(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &cx);
        let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());
        assert_eq!(shell.read_with(&cx, |s, _| s.occupant_kind(tile)), Some("rec"));
        assert!(
            log.borrow().iter().any(|r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)),
            "{:?}",
            log.borrow()
        );
        let bounds = cx.debug_bounds(&format!("tile-content-{}", tile.0));
        assert!(bounds.is_some_and(|b| b.size.width > px(0.0)), "the occupant's view painted: {bounds:?}");
    }

    #[gpui::test]
    fn a_key_in_the_occupants_context_reaches_its_dispatch_with_the_count(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("4 j");
        let shell = shell_of(&window, &cx);
        let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());
        assert!(
            log.borrow().iter().any(|r| matches!(
                r,
                crate::module::recording::Recorded::Dispatch(t, a, Some(4)) if *t == tile && a.0 == "rec::noop"
            )),
            "{:?}",
            log.borrow()
        );
    }

    #[gpui::test]
    fn closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        let shell = shell_of(&window, &cx);
        let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());

        cx.simulate_keystrokes("alt-2");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            log.borrow().iter().any(|r| matches!(r, crate::module::recording::Recorded::Visible(t, false) if *t == tile)),
            "hidden on switch: {:?}",
            log.borrow()
        );
        cx.simulate_keystrokes("alt-1");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            log.borrow().iter().any(|r| matches!(r, crate::module::recording::Recorded::Visible(t, true) if *t == tile)),
            "shown on return: {:?}",
            log.borrow()
        );

        cx.simulate_keystrokes("ctrl-w");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(shell.read_with(&cx, |s, _| s.occupant_kind(tile)), None, "occupant dropped with its tile");
    }

    #[gpui::test]
    fn a_click_on_a_tile_leaves_the_shell_focused_on_the_next_frame(cx: &mut gpui::TestAppContext) {
        // gpui focuses a tracked element on mouse down; an occupant that
        // tracks its own handle (DataTable does) would take focus with it
        // and every shell chord would go dead. The tile's click handler
        // arms the same restore `apply_reload` uses (§3.3).
        let (services, _log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &cx);
        let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());
        let bounds = cx.debug_bounds(&format!("tile-content-{}", tile.0)).unwrap();
        cx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
        assert!(cx.update(|window, _| focused.is_focused(window)), "the shell root has focus again");
    }
```

(The recording module's view tracks its own focus handle on purpose, so
the last test exercises the real hazard. `simulate_mouse_down`/`_up`
exist on `VisualTestContext` at the pinned gpui rev; the existing
`mouse_down_on_a_tile_focuses_it` test uses them — copy its call shape
if the signatures differ.)

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p geode-shell module:: 2>&1 | grep -E "^error" | head -3`

- [ ] **Step 4: Implement `module.rs`**

```rust
//! The module-hosting contract (foundation §9.1, Phase 3 §3). A tile is
//! still a `TileId`; what lives in it is a [`TileOccupant`] the shell
//! created through a [`ModuleFactory`] from the app's [`ModuleRoster`].
//!
//! Nothing here names `geode-data`. The factory gets shell-side handles
//! only — the tile id and the frame entity — and a module that needs
//! data carries its own handle as a field of its factory, built in
//! `geode-app` where both sides meet (§2.1). The one data type that
//! crosses is `geode_core::query::QueryOutcome`, which the shell routes
//! to the tile whose id is the outcome's key.

use crate::actions::{ActionId, ActionRegistry};
use crate::frame::Frame;
use crate::keymap::KeyContext;
use crate::tiling::TileId;
use geode_core::query::QueryOutcome;
use gpui::{AnyView, App, Entity, Window};

/// What the `/` line tells the occupant (§3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindEvent {
    Changed(String),
    Committed(String),
    Cancelled,
}

pub trait TileContent {
    /// Pushed onto the keymap context stack while this tile is focused,
    /// e.g. `blotter` with `mode = normal`, opted into counts.
    fn key_context(&self, cx: &App) -> KeyContext;
    /// An action the shell did not recognise. `true` if handled.
    fn dispatch(&self, action: &ActionId, count: Option<u32>, window: &mut Window, cx: &mut App) -> bool;
    /// A `:` line, without the colon. `Err` is shown inline on the line.
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String>;
    /// Candidates for the word under `cursor` on a `:` line. The shell
    /// ranks and shows them; the occupant only knows its vocabulary.
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String>;
    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App);
    /// A query result addressed to this tile (§5.1).
    fn deliver(&self, outcome: QueryOutcome, window: &mut Window, cx: &mut App);
    /// Hidden tiles may drop subscriptions; shown tiles requery if stale.
    fn set_visible(&self, visible: bool, cx: &mut App);
    /// State for `session.toml` (§3.5); stored opaquely by the shell.
    fn serialize(&self, cx: &App) -> toml::Table;
}

pub struct TileOccupant {
    pub kind: &'static str,
    /// What the shell paints inside the tile chrome.
    pub view: AnyView,
    pub content: Box<dyn TileContent>,
}

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

/// The only place the app knows which modules exist (§9.1).
pub struct ModuleRoster {
    factories: Vec<Box<dyn ModuleFactory>>,
    default_kind: String,
}

impl ModuleRoster {
    pub fn new(default_kind: impl Into<String>) -> ModuleRoster {
        ModuleRoster {
            factories: Vec::new(),
            default_kind: default_kind.into(),
        }
    }

    pub fn add(&mut self, factory: Box<dyn ModuleFactory>) {
        self.factories.push(factory);
    }

    pub fn register_actions(&self, registry: &mut ActionRegistry) {
        for f in &self.factories {
            f.register_actions(registry);
        }
    }

    pub fn factory(&self, kind: &str) -> Option<&dyn ModuleFactory> {
        self.factories.iter().find(|f| f.kind() == kind).map(|f| f.as_ref())
    }

    pub fn default_factory(&self) -> Option<&dyn ModuleFactory> {
        self.factory(&self.default_kind)
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.factories.iter().map(|f| f.kind()).collect()
    }
}

impl Default for ModuleRoster {
    fn default() -> Self {
        ModuleRoster::new("placeholder")
    }
}

/// The occupant of a tile nothing else claims: an unknown session kind,
/// or a roster with no default. Paints a hint naming the palette; never
/// a blank, never a panic.
pub mod placeholder {
    use super::*;
    use gpui::prelude::*;
    use gpui::{Context, Render, div};
    use gpui_component::ActiveTheme as _;

    pub struct PlaceholderFactory;

    struct PlaceholderView {
        tile: TileId,
    }

    impl Render for PlaceholderView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .debug_selector(|| format!("tile-content-{}", self.tile.0))
                .child("ctrl+k → open a view")
        }
    }

    struct PlaceholderContent;

    impl TileContent for PlaceholderContent {
        fn key_context(&self, _cx: &App) -> KeyContext {
            KeyContext::new("placeholder")
        }
        fn dispatch(&self, _: &ActionId, _: Option<u32>, _: &mut Window, _: &mut App) -> bool {
            false
        }
        fn command(&self, _: &str, _: &mut Window, _: &mut App) -> Result<(), String> {
            Err("this tile has no module; open one from the palette".into())
        }
        fn completions(&self, _: &str, _: usize, _: &App) -> Vec<String> {
            Vec::new()
        }
        fn find(&self, _: FindEvent, _: &mut Window, _: &mut App) {}
        fn deliver(&self, _: QueryOutcome, _: &mut Window, _: &mut App) {}
        fn set_visible(&self, _: bool, _: &mut App) {}
        fn serialize(&self, _: &App) -> toml::Table {
            toml::Table::new()
        }
    }

    impl ModuleFactory for PlaceholderFactory {
        fn kind(&self) -> &'static str {
            "placeholder"
        }
        fn register_actions(&self, _: &mut ActionRegistry) {}
        fn create(&self, tile: TileId, _: Option<&toml::Table>, _: Entity<Frame>, _: &mut Window, cx: &mut App) -> TileOccupant {
            let view = cx.new(|_| PlaceholderView { tile });
            TileOccupant {
                kind: "placeholder",
                view: view.into(),
                content: Box::new(PlaceholderContent),
            }
        }
    }
}

/// A module that records everything the shell does to it, for the
/// shell's own hosting tests and for module tests that need a neighbour.
#[cfg(any(test, feature = "test-support"))]
pub mod recording {
    use super::*;
    use gpui::prelude::*;
    use gpui::{Context, FocusHandle, Render, div};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Recorded {
        Created(TileId, Option<toml::Table>),
        Dispatch(TileId, ActionId, Option<u32>),
        Command(TileId, String),
        Find(TileId, FindEvent),
        Visible(TileId, bool),
        Delivered(TileId, u64),
    }

    pub struct RecordingFactory {
        kind: &'static str,
        pub log: Rc<RefCell<Vec<Recorded>>>,
        /// What `completions` answers, regardless of the line.
        pub completions: Vec<String>,
        /// What `command` answers.
        pub command_result: Result<(), String>,
    }

    impl RecordingFactory {
        pub fn new(kind: &'static str) -> RecordingFactory {
            RecordingFactory {
                kind,
                log: Rc::new(RefCell::new(Vec::new())),
                completions: vec!["delta01".into(), "gamma01".into()],
                command_result: Ok(()),
            }
        }
    }

    /// Tracks its own focus handle on purpose: that is what `DataTable`
    /// does, and the shell's click-to-focus restore is tested against it.
    struct RecordingView {
        tile: TileId,
        focus: FocusHandle,
    }

    impl Render for RecordingView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .debug_selector(|| format!("tile-content-{}", self.tile.0))
                .child(format!("rec {}", self.tile.0))
        }
    }

    struct RecordingContent {
        tile: TileId,
        log: Rc<RefCell<Vec<Recorded>>>,
        completions: Vec<String>,
        command_result: Result<(), String>,
        pub state: RefCell<toml::Table>,
    }

    impl TileContent for RecordingContent {
        fn key_context(&self, _cx: &App) -> KeyContext {
            KeyContext::new("rec").pair("mode", "normal").counts()
        }
        fn dispatch(&self, action: &ActionId, count: Option<u32>, _: &mut Window, _: &mut App) -> bool {
            self.log.borrow_mut().push(Recorded::Dispatch(self.tile, action.clone(), count));
            action.0.starts_with("rec::")
        }
        fn command(&self, line: &str, _: &mut Window, _: &mut App) -> Result<(), String> {
            self.log.borrow_mut().push(Recorded::Command(self.tile, line.to_string()));
            self.state.borrow_mut().insert("last_command".into(), toml::Value::String(line.to_string()));
            self.command_result.clone()
        }
        fn completions(&self, _: &str, _: usize, _: &App) -> Vec<String> {
            self.completions.clone()
        }
        fn find(&self, event: FindEvent, _: &mut Window, _: &mut App) {
            self.log.borrow_mut().push(Recorded::Find(self.tile, event));
        }
        fn deliver(&self, outcome: QueryOutcome, _: &mut Window, _: &mut App) {
            self.log.borrow_mut().push(Recorded::Delivered(self.tile, outcome.tag));
        }
        fn set_visible(&self, visible: bool, _: &mut App) {
            self.log.borrow_mut().push(Recorded::Visible(self.tile, visible));
        }
        fn serialize(&self, _: &App) -> toml::Table {
            self.state.borrow().clone()
        }
    }

    impl ModuleFactory for RecordingFactory {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn register_actions(&self, registry: &mut ActionRegistry) {
            let _ = registry.register(crate::actions::ActionDef {
                id: ActionId(format!("{}::noop", self.kind)),
                title: "Recording no-op".into(),
                category: "Test".into(),
            });
        }
        fn create(&self, tile: TileId, restored: Option<&toml::Table>, _: Entity<Frame>, _: &mut Window, cx: &mut App) -> TileOccupant {
            self.log.borrow_mut().push(Recorded::Created(tile, restored.cloned()));
            let focus = cx.focus_handle();
            let view = cx.new(|_| RecordingView { tile, focus });
            TileOccupant {
                kind: self.kind,
                view: view.into(),
                content: Box::new(RecordingContent {
                    tile,
                    log: self.log.clone(),
                    completions: self.completions.clone(),
                    command_result: self.command_result.clone(),
                    state: RefCell::new(restored.cloned().unwrap_or_default()),
                }),
            }
        }
    }
}
```

`Cargo.toml` of `geode-shell`: add `[features] test-support = []` next
to `profiling`. `lib.rs`: `pub mod module;` between `listfilter` and
`palette`.

- [ ] **Step 5: Wire `ShellView`**

`ShellServices` gains:

```rust
    /// The modules the app compiled in (§9.1); the shell creates tile
    /// occupants through it and never names a module crate.
    pub roster: ModuleRoster,
    /// Per-tile module kind and state restored from `session.toml`
    /// (Task 4); consumed as occupants are created.
    pub restored_tiles: BTreeMap<u64, crate::session::TileRecord>,
```

(`test_services()` sets `roster: ModuleRoster::default()` and
`restored_tiles: BTreeMap::new()`.)

`ShellView` gains:

```rust
    /// The shared frame (§4), created here so every occupant can hold it.
    frame: Entity<Frame>,
    /// Who lives in each tile. Created lazily in `ensure_occupants` and
    /// dropped when the tile is gone from every workspace.
    occupants: HashMap<TileId, TileOccupant>,
    /// The tiles painted last frame, to diff visibility without touching
    /// every occupant every frame.
    visible_tiles: HashSet<TileId>,
```

In `new`, before the struct literal:

```rust
        let frame = {
            let (schema, _) = services
                .config
                .doc("datasets")
                .map(SchemaSpec::from_doc)
                .unwrap_or_default();
            let (dims, _) = services
                .config
                .doc("dimensions")
                .map(DerivedDimensions::from_doc)
                .unwrap_or_default();
            let (slots, diags) = services
                .config
                .doc("groupings")
                .map(|d| GroupingSlots::from_doc(d, &schema, &dims))
                .unwrap_or_default();
            for d in &diags {
                eprintln!("[groupings] {d}");
            }
            cx.new(|_| Frame::new(slots, user_dir.clone()))
        };
```

(`SchemaSpec::from_doc` returns a tuple; `unwrap_or_default` needs
`(SchemaSpec, Vec<Diagnostic>)` to be `Default`, which it is.)

New methods:

```rust
    pub fn frame(&self) -> &Entity<Frame> {
        &self.frame
    }

    pub fn occupant_kind(&self, tile: TileId) -> Option<&'static str> {
        self.occupants.get(&tile).map(|o| o.kind)
    }

    /// Route a query outcome to the tile whose id is its key (§5.1). The
    /// app bridge calls this; an outcome for a tile that no longer exists
    /// is dropped.
    pub fn deliver(&mut self, outcome: QueryOutcome, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(o) = self.occupants.get(&TileId(outcome.key.0)) {
            o.content.deliver(outcome, window, cx);
        }
    }

    /// Every tile id in every workspace, main trees and docks.
    fn all_tiles(&self) -> HashSet<TileId> {
        let mut out = HashSet::new();
        for (_, ws) in self.services.workspaces.spaces() {
            out.extend(ws.tree().tiles());
            for (_, dock) in ws.docks().iter() {
                out.extend(dock.tree().tiles());
            }
        }
        out
    }

    /// The tiles of the active workspace: what is on screen.
    fn active_tiles(&self) -> HashSet<TileId> {
        let ws = self.services.workspaces.active();
        let mut out: HashSet<TileId> = ws.tree().tiles().into_iter().collect();
        for (_, dock) in ws.docks().iter() {
            if dock.visible() {
                out.extend(dock.tree().tiles());
            }
        }
        out
    }

    /// Create occupants for tiles that lack one, drop occupants whose tile
    /// is gone, and tell occupants when they enter or leave the screen.
    /// Runs at the top of `render`, the one place with a `Window` on every
    /// path that can change the tile set (a split, a close, a restore, a
    /// workspace switch).
    fn ensure_occupants(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let all = self.all_tiles();
        self.occupants.retain(|id, _| all.contains(id));
        for id in &all {
            if self.occupants.contains_key(id) {
                continue;
            }
            let restored = self.services.restored_tiles.remove(&id.0);
            let factory = restored
                .as_ref()
                .and_then(|r| self.services.roster.factory(&r.kind))
                .or_else(|| self.services.roster.default_factory());
            let occupant = match factory {
                Some(f) => f.create(*id, restored.as_ref().map(|r| &r.state), self.frame.clone(), window, cx),
                None => crate::module::placeholder::PlaceholderFactory.create(*id, None, self.frame.clone(), window, cx),
            };
            self.occupants.insert(*id, occupant);
        }
        let active = self.active_tiles();
        for id in self.visible_tiles.difference(&active) {
            if let Some(o) = self.occupants.get(id) {
                o.content.set_visible(false, cx);
            }
        }
        for id in active.difference(&self.visible_tiles) {
            if let Some(o) = self.occupants.get(id) {
                o.content.set_visible(true, cx);
            }
        }
        self.visible_tiles = active;
    }
```

`context_stack`:

```rust
    fn context_stack(&self) -> Vec<KeyContext> {
        let mut stack = vec![KeyContext::new("workspace")];
        if let Some(tile) = self.services.workspaces.active().focused_tile()
            && let Some(o) = self.occupants.get(&tile)
        {
            // `tile` is the shell's own frame for "some occupant has
            // focus" (Task 5 binds `/` and `:` on it); the occupant's own
            // context sits above it, innermost.
            stack.push(KeyContext::new("tile"));
            stack.push(o.content.key_context(cx));
        }
        if self.palette.is_some() {
            stack.push(KeyContext::new("palette"));
        }
        stack
    }
```

`key_context` needs `&App`; `context_stack` has none today. Change its
signature to `fn context_stack(&self, cx: &App) -> Vec<KeyContext>` and
pass `cx` at its three call sites (`is_palette_toggle`, the matcher
press in `handle_key_down`, and the which-key computation in `render`).

`dispatch`'s final `else`:

```rust
        } else {
            #[cfg(feature = "profiling")]
            if profiling_hook::dispatch(self, action, window, cx) {
                return;
            }
            // A module's own action (§3.3): hand it to the focused
            // occupant. Unhandled ids fall off the end silently, as they
            // always did.
            if let Some(tile) = self.services.workspaces.active().focused_tile()
                && let Some(o) = self.occupants.get(&tile)
            {
                o.content.dispatch(action, count, window, cx);
            }
        }
```

(`profiling_hook::dispatch` returns `()` today; change it to return
`bool` — `true` when it recognised the id — so the fall-through is
reachable with the feature on.)

In `render`, right after the `pending_focus_restore` block:

```rust
        self.ensure_occupants(window, cx);
```

`tile_cell` takes the occupant's view: change its signature to
`|id, r, is_focused, view: Option<AnyView>, cx|`, drop
`.flex().items_center().justify_center()`, `.font_family(fonts::MONO)`
and the label, and end with:

```rust
                .overflow_hidden()
                .map(|el| match view {
                    Some(view) => el.child(view),
                    None => el
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family(fonts::MONO)
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("tile {}", id.0)),
                })
```

Both call sites pass `self.occupants.get(&id).map(|o| o.view.clone())`
(an `AnyView` clone is an `Entity` clone). The tree-tile
`on_mouse_down` listener adds `view.pending_focus_restore = true;`
before `cx.notify()`, with the comment from the test above.

`main.rs`'s `build_shell_services`, after `register_builtin_actions`:

```rust
    // Modules register their actions before the keymap builds (§3.2);
    // Plan 3c fills the roster with the blotter. The default kind is
    // read from `[app] modules.default`, "blotter" when unset.
    let default_kind = config
        .get("app", "modules.default")
        .and_then(|v| v.as_str())
        .unwrap_or("blotter")
        .to_string();
    let roster = ModuleRoster::new(default_kind);
    roster.register_actions(&mut registry);
```

and `ShellServices { …, roster, restored_tiles: BTreeMap::new() }`.

- [ ] **Step 6: Run**

Run: `cargo test -p geode-shell && cargo build -p geode-app`
Expected: green, including the four new `TestAppContext` tests.

- [ ] **Step 7: Full check and commit**

```bash
git add crates/geode-shell crates/geode-app/src/main.rs
git commit -m "feat(shell): the module-hosting contract and tile occupants

TileContent, ModuleFactory and ModuleRoster; occupants created lazily
per tile, painted inside the tile chrome, pushed onto the key context
stack, handed unknown actions with their count, told when they leave
the screen, and dropped with their tile. A tile click re-arms the
shell's focus restore. Placeholder and recording modules (Phase 3 §3).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 8: Harness entries** (package `geode-shell`)

```sh
# ---- module hosting (Phase 3 §3)

run_mutation "hosting: an unknown action reaches the focused occupant with its count" \
  crates/geode-shell/src/shell/mod.rs \
  '                o.content.dispatch(action, count, window, cx);' \
  '                o.content.dispatch(action, None, window, cx);' \
  geode-shell

run_mutation "hosting: a closed tile drops its occupant" \
  crates/geode-shell/src/shell/mod.rs \
  '        self.occupants.retain(|id, _| all.contains(id));' \
  '        let _ = &all;' \
  geode-shell

run_mutation "hosting: leaving the screen is announced" \
  crates/geode-shell/src/shell/mod.rs \
  '                o.content.set_visible(false, cx);' \
  '                let _ = o;' \
  geode-shell
```

Run: `zsh scripts/mutation-check.sh "hosting:"` — all `caught`. Commit.

---

### Task 4: `tiles` in `session.toml`

Spec §3.5. Module kind and opaque state per tile, restored into
`ShellServices::restored_tiles`, written whenever the layout or any
occupant's state changes.

**Files:**
- Modify: `crates/geode-shell/src/session.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (`take_dirty_session_write`,
  `save_session`, `last_tiles_written`)
- Modify: `crates/geode-app/src/main.rs` (`session::load` → `Restored`)
- Test: `session.rs` inline; one `TestAppContext` test

**Interfaces:**
- Produces:
  ```rust
  pub struct TileRecord { pub kind: String, pub state: toml::Table }   // Debug, Clone, PartialEq
  pub type TileRecords = BTreeMap<u64, TileRecord>;
  pub struct Restored { pub workspaces: Workspaces, pub tiles: TileRecords, pub warnings: Vec<String> }
  pub fn to_toml(workspaces: &Workspaces, tiles: &TileRecords) -> toml::Table;
  pub fn from_toml(table: &toml::Table) -> Result<Restored, Vec<String>>;
  pub fn to_string_pretty(workspaces: &Workspaces, tiles: &TileRecords) -> Result<String, String>;
  pub fn save(path: &Path, workspaces: &Workspaces, tiles: &TileRecords) -> io::Result<()>;
  pub fn load(path: &Path) -> Restored;
  ```

- [ ] **Step 1: Write the failing tests**

Add to `session.rs`'s tests (it has helpers building a `Workspaces` with
tiles; use them — `two_tile_workspaces()` or the nearest existing one):

```rust
    #[test]
    fn tiles_round_trip_with_their_kind_and_opaque_state() {
        let ws = two_tile_workspaces();
        let ids = ws.active().tree().tiles();
        let mut tiles = TileRecords::new();
        let mut state = toml::Table::new();
        state.insert("view".into(), toml::Value::String("tree".into()));
        state.insert(
            "pinned".into(),
            toml::Value::Array(vec![toml::Value::String("lhu".into())]),
        );
        tiles.insert(ids[0].0, TileRecord { kind: "blotter".into(), state });
        tiles.insert(ids[1].0, TileRecord { kind: "blotter".into(), state: toml::Table::new() });

        let text = to_string_pretty(&ws, &tiles).unwrap();
        assert!(text.contains(&format!("[workspaces.1.tiles.{}]", ids[0].0)), "{text}");
        assert!(text.contains("module = \"blotter\""), "{text}");
        assert!(text.contains("view = \"tree\""), "{text}");

        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(restored.tiles, tiles);
        assert_eq!(restored.workspaces.active().tree().tiles(), ids);
    }

    #[test]
    fn a_tile_record_for_an_id_not_in_that_workspace_is_dropped_with_a_warning() {
        let ws = two_tile_workspaces();
        let mut tiles = TileRecords::new();
        tiles.insert(999, TileRecord { kind: "blotter".into(), state: toml::Table::new() });
        let mut table = to_toml(&ws, &tiles);
        // Force the stray record in under workspace 1 regardless of what
        // `to_toml` filtered.
        let ws_table = table["workspaces"]["1"].as_table_mut().unwrap();
        let mut stray = toml::Table::new();
        stray.insert("module".into(), toml::Value::String("blotter".into()));
        let mut tiles_table = ws_table
            .get("tiles")
            .and_then(|t| t.as_table())
            .cloned()
            .unwrap_or_default();
        tiles_table.insert("999".into(), toml::Value::Table(stray));
        ws_table.insert("tiles".into(), toml::Value::Table(tiles_table));

        let restored = from_toml(&table).unwrap();
        assert!(!restored.tiles.contains_key(&999));
        assert!(
            restored.warnings.iter().any(|w| w.contains("999")),
            "{:?}",
            restored.warnings
        );
    }

    #[test]
    fn a_tile_record_without_a_module_or_with_a_bad_state_is_dropped_with_a_warning() {
        let ws = two_tile_workspaces();
        let id = ws.active().tree().tiles()[0].0;
        let mut table = to_toml(&ws, &TileRecords::new());
        let ws_table = table["workspaces"]["1"].as_table_mut().unwrap();
        let mut tiles_table = toml::Table::new();
        let mut no_module = toml::Table::new();
        no_module.insert("state".into(), toml::Value::Table(toml::Table::new()));
        tiles_table.insert(id.to_string(), toml::Value::Table(no_module));
        ws_table.insert("tiles".into(), toml::Value::Table(tiles_table));
        let restored = from_toml(&table).unwrap();
        assert!(restored.tiles.is_empty());
        assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
    }

    #[test]
    fn a_session_without_tiles_still_loads_and_writes_no_tiles_table() {
        // Every pre-Phase-3 session file.
        let ws = two_tile_workspaces();
        let text = to_string_pretty(&ws, &TileRecords::new()).unwrap();
        assert!(!text.contains("tiles"), "{text}");
        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.tiles.is_empty());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell session:: 2>&1 | grep -E "^error" | head -3`

- [ ] **Step 3: Implement**

In `session.rs`:

```rust
/// One tile's occupant, as persisted (Phase 3 §3.5). `state` is whatever
/// the module's `serialize` returned; the shell never reads inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct TileRecord {
    pub kind: String,
    pub state: toml::Table,
}

pub type TileRecords = BTreeMap<u64, TileRecord>;

/// What `load`/`from_toml` hand back.
#[derive(Debug)]
pub struct Restored {
    pub workspaces: Workspaces,
    pub tiles: TileRecords,
    pub warnings: Vec<String>,
}
```

`to_toml(workspaces, tiles)`: inside the per-workspace loop, after the
docks table, collect the ids that belong to this workspace (`tree.tiles()`
plus every dock's tiles) and write:

```rust
        let mut tiles_table = toml::Table::new();
        let mut here: Vec<TileId> = tree.tiles();
        for (_, dock) in workspace.docks().iter() {
            here.extend(dock.tree().tiles());
        }
        for id in here {
            let Some(record) = tiles.get(&id.0) else {
                continue;
            };
            let mut t = toml::Table::new();
            t.insert("module".to_string(), toml::Value::String(record.kind.clone()));
            if !record.state.is_empty() {
                t.insert("state".to_string(), toml::Value::Table(record.state.clone()));
            }
            tiles_table.insert(id.0.to_string(), toml::Value::Table(t));
        }
        if !tiles_table.is_empty() {
            ws_table.insert("tiles".to_string(), toml::Value::Table(tiles_table));
        }
```

`from_toml` returns `Result<Restored, Vec<String>>`. `parse_workspace`
gains an `out_tiles: &mut TileRecords` parameter and, after building the
workspace, reads `ws_table.get("tiles")`:

```rust
    if let Some(tiles_value) = ws_table.get("tiles") {
        match tiles_value.as_table() {
            None => warnings.push(format!("workspace {ix}: tiles is not a table; ignored")),
            Some(tiles_table) => {
                let mut here: Vec<u64> = workspace.tree().tiles().iter().map(|t| t.0).collect();
                for (_, dock) in workspace.docks().iter() {
                    here.extend(dock.tree().tiles().iter().map(|t| t.0));
                }
                for (key, value) in tiles_table {
                    let Ok(id) = key.parse::<u64>() else {
                        warnings.push(format!("workspace {ix}: tile key '{key}' is not an id; ignored"));
                        continue;
                    };
                    if !here.contains(&id) {
                        warnings.push(format!("workspace {ix}: tile {id} has a record but is not in the layout; ignored"));
                        continue;
                    }
                    let Some(t) = value.as_table() else {
                        warnings.push(format!("workspace {ix}: tile {id} is not a table; ignored"));
                        continue;
                    };
                    let Some(kind) = t.get("module").and_then(|v| v.as_str()) else {
                        warnings.push(format!("workspace {ix}: tile {id} has no module; ignored"));
                        continue;
                    };
                    let state = match t.get("state") {
                        None => toml::Table::new(),
                        Some(s) => match s.as_table() {
                            Some(s) => s.clone(),
                            None => {
                                warnings.push(format!("workspace {ix}: tile {id} state is not a table; ignored"));
                                continue;
                            }
                        },
                    };
                    out_tiles.insert(id, TileRecord { kind: kind.to_string(), state });
                }
            }
        }
    }
```

`to_string_pretty(workspaces, tiles)`, `save(path, workspaces, tiles)`,
and `load(path) -> Restored` (a missing or bad file is `Restored {
workspaces: Workspaces::new(), tiles: BTreeMap::new(), warnings }`).

`ShellView`:

```rust
    /// The tile records written last, so a module state change (which
    /// never dirties the layout flag) is still noticed by the watcher.
    last_tiles_written: TileRecords,

    fn current_tiles(&self, cx: &App) -> TileRecords {
        self.occupants
            .iter()
            .filter(|(_, o)| o.kind != "placeholder")
            .map(|(id, o)| (id.0, TileRecord { kind: o.kind.to_string(), state: o.content.serialize(cx) }))
            .collect()
    }

    fn take_dirty_session_write(&mut self, cx: &App) -> Option<(PathBuf, String)> {
        let tiles = self.current_tiles(cx);
        if !self.session_dirty && tiles == self.last_tiles_written {
            return None;
        }
        self.session_dirty = false;
        let path = self.services.session_path.clone()?;
        match session::to_string_pretty(&self.services.workspaces, &tiles) {
            Ok(text) => {
                self.last_tiles_written = tiles;
                Some((path, text))
            }
            Err(e) => {
                eprintln!("[session] warning: failed to serialize session: {e}");
                None
            }
        }
    }

    pub fn save_session(&self, cx: &App) {
        let Some(path) = self.services.session_path.as_ref() else {
            return;
        };
        if let Err(e) = session::save(path, &self.services.workspaces, &self.current_tiles(cx)) {
            eprintln!("[session] warning: failed to save session: {e}");
        }
    }
```

The watcher loop in `new` calls `take_dirty_session_write(cx)` inside
its `update`; `main.rs`'s quit hook passes `cx`. `main.rs`'s restore:

```rust
            if let Some(path) = &services.session_path {
                let restored = session::load(path);
                for warning in &restored.warnings {
                    eprintln!("[session] warning: {warning}");
                }
                services.workspaces = restored.workspaces;
                services.restored_tiles = restored.tiles;
            }
```

Add one `TestAppContext` test in `shell/mod.rs`: with the recorder
roster, `ctrl-v`, then read the shell and assert `current_tiles(cx)`
holds the focused tile with kind `"rec"`; then, using
`services.restored_tiles` pre-seeded with a record for a tile id the
test creates through a restored `Workspaces` (build one with
`session::from_toml` on a hand-written table containing a `tiles`
entry), assert the log holds `Created(tile, Some(state))` with that
state.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-shell && cargo build -p geode-app`

- [ ] **Step 5: Full check and commit**

```bash
git add crates/geode-shell crates/geode-app/src/main.rs
git commit -m "feat(shell): session.toml carries each tile's module and state

A tiles table per workspace, keyed by tile id, with the module kind and
the opaque state its serialize returned; dangling or malformed records
warn and are dropped; a state change is written on the watcher tick
even when the layout is clean (Phase 3 §3.5).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entries** (package `geode-shell`)

```sh
# ---- session tiles (Phase 3 §3.5)

run_mutation "session: a record for a tile not in the layout is dropped" \
  crates/geode-shell/src/session.rs \
  '                    if !here.contains(&id) {' \
  '                    if false {' \
  geode-shell

run_mutation "session: tile state round-trips" \
  crates/geode-shell/src/session.rs \
  '            if !record.state.is_empty() {
                t.insert("state".to_string(), toml::Value::Table(record.state.clone()));
            }' \
  '' \
  geode-shell
```

Run: `zsh scripts/mutation-check.sh "session:"` — both `caught`. Commit.

---

### Task 5: The command line and completions

Spec §3.4. One shell-owned input at the bottom of the focused tile, a
prompt of `/` or `:`, ranked completions from the occupant, and a
routing rule for every key while it is open.

**Files:**
- Create: `crates/geode-shell/src/commandline.rs` (pure)
- Create: `crates/geode-shell/src/shell/commandline_view.rs`
- Modify: `crates/geode-shell/src/lib.rs`, `crates/geode-shell/src/shell/mod.rs`
  (fields, `new`, `handle_key_down`, `dispatch`, `render`)
- Modify: `crates/geode-shell/src/palette.rs` (`highlighted_title` →
  `pub(crate)`)
- Modify: `crates/geode-shell/src/defaults.rs` (`tile::command_line`,
  `tile::find`; bindings in context `tile`)
- Test: `commandline.rs` inline; `shell/mod.rs` `TestAppContext` tests

**Interfaces:**
- Produces (pure):
  ```rust
  pub enum Prompt { Find, Command }
  pub struct CommandLine { pub prompt: Prompt, pub tile: TileId, pub error: Option<String>,
                           pub candidates: Vec<Ranked>, pub words: Vec<String>, pub highlighted: usize,
                           pub word: Range<usize> }
  pub fn word_at(line: &str, cursor: usize) -> Range<usize>;      // byte range of the word under the cursor
  pub fn rank_candidates(words: &[String], word: &str) -> Vec<Ranked>;
  pub fn accept(line: &str, word: Range<usize>, candidate: &str) -> (String, usize);  // new line, new cursor
  pub enum CompletionKey { Next, Prev, Accept }
  pub fn completion_key(ks: &Keystroke) -> Option<CompletionKey>;
  pub enum Submit { Run(String), Accepted(String, usize), Ambiguous(Vec<String>) }
  pub fn resolve_submit(line: &str, cursor: usize, candidates: &[Ranked], words: &[String]) -> Submit;
  ```
- Produces (shell): `tile::command_line`, `tile::find` actions; the
  strip's `debug_selector` is `"command-line"`, its popup rows
  `"completion-row-{i}"`.

- [ ] **Step 1: Write the failing pure tests**

`commandline.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Modifiers;

    fn key(k: &str) -> Keystroke {
        Keystroke { mods: Modifiers::NONE, key: k.into() }
    }
    fn ctrl(k: &str) -> Keystroke {
        Keystroke { mods: Modifiers::CTRL, key: k.into() }
    }
    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_word_under_the_cursor_is_delimited_by_spaces_and_commas() {
        assert_eq!(word_at("sort del", 8), 5..8);
        assert_eq!(word_at("sort del", 5), 5..5, "at the start of a word, the word is empty so far");
        assert_eq!(word_at("sort delta01 desc", 9), 5..12, "cursor inside a word");
        assert_eq!(word_at("group lhu,und", 13), 10..13, "commas split");
        assert_eq!(word_at("", 0), 0..0);
        assert_eq!(word_at("sort ", 5), 5..5);
    }

    #[test]
    fn candidates_are_ranked_by_the_shared_fuzzy_matcher() {
        let ranked = rank_candidates(&words(&["delta01", "gamma01", "daily_trading_pnl"]), "del");
        assert_eq!(ranked[0].row, 0);
        assert_eq!(ranked.len(), 2, "gamma01 has no d-e-l subsequence: {ranked:?}");
        assert!(rank_candidates(&words(&["a"]), "").len() == 1, "an empty word keeps everything");
    }

    #[test]
    fn accepting_replaces_the_word_and_puts_the_cursor_after_it() {
        assert_eq!(accept("sort del", 5..8, "delta01"), ("sort delta01".into(), 12));
        assert_eq!(accept("sort d desc", 5..6, "delta01"), ("sort delta01 desc".into(), 12));
        assert_eq!(accept("sort ", 5..5, "npv"), ("sort npv".into(), 8));
    }

    #[test]
    fn the_key_vocabulary() {
        assert_eq!(completion_key(&key("tab")), Some(CompletionKey::Accept));
        assert_eq!(completion_key(&ctrl("n")), Some(CompletionKey::Next));
        assert_eq!(completion_key(&key("down")), Some(CompletionKey::Next));
        assert_eq!(completion_key(&ctrl("p")), Some(CompletionKey::Prev));
        assert_eq!(completion_key(&key("up")), Some(CompletionKey::Prev));
        assert_eq!(completion_key(&key("a")), None);
    }

    #[test]
    fn submit_runs_accepts_or_refuses() {
        let w = words(&["delta01", "gamma01", "npv"]);
        let one = rank_candidates(&w, "np");
        assert_eq!(resolve_submit("sort np", 7, &one, &w), Submit::Accepted("sort npv".into(), 8));
        let many = rank_candidates(&w, "a01");
        assert_eq!(
            resolve_submit("sort a01", 8, &many, &w),
            Submit::Ambiguous(words(&["delta01", "gamma01"]))
        );
        let exact = rank_candidates(&w, "npv");
        assert_eq!(resolve_submit("sort npv", 8, &exact, &w), Submit::Run("sort npv".into()), "an exact word runs");
        assert_eq!(resolve_submit("sort npv desc", 13, &[], &w), Submit::Run("sort npv desc".into()), "no candidates: run as typed");
        assert_eq!(resolve_submit("unpin", 5, &[], &[]), Submit::Run("unpin".into()));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell commandline:: 2>&1 | tail -3`

- [ ] **Step 3: Implement the pure core**

```rust
//! The per-tile command line's pure state (Phase 3 §3.4): the word under
//! the cursor, ranked completions through the palette's fuzzy matcher,
//! accepting one, and the key vocabulary. `shell::commandline_view`
//! paints it; `ShellView` routes keys to it. No `gpui` here.

use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter::{Ranked, rank};
use crate::tiling::TileId;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Find,
    Command,
}

impl Prompt {
    pub fn glyph(self) -> &'static str {
        match self {
            Prompt::Find => "/",
            Prompt::Command => ":",
        }
    }
}

#[derive(Debug)]
pub struct CommandLine {
    pub prompt: Prompt,
    pub tile: TileId,
    pub error: Option<String>,
    /// The occupant's vocabulary for the word under the cursor, as last
    /// asked.
    pub words: Vec<String>,
    /// `words` ranked against that word; empty when there is no popup.
    pub candidates: Vec<Ranked>,
    pub highlighted: usize,
    pub word: Range<usize>,
}

impl CommandLine {
    pub fn new(prompt: Prompt, tile: TileId) -> CommandLine {
        CommandLine {
            prompt,
            tile,
            error: None,
            words: Vec::new(),
            candidates: Vec::new(),
            highlighted: 0,
            word: 0..0,
        }
    }

    /// Re-rank for a new line and vocabulary.
    pub fn refresh(&mut self, line: &str, cursor: usize, words: Vec<String>) {
        self.word = word_at(line, cursor);
        self.words = words;
        self.candidates = rank_candidates(&self.words, &line[self.word.clone()]);
        self.highlighted = 0;
        self.error = None;
    }

    pub fn highlighted_word(&self) -> Option<&str> {
        self.candidates
            .get(self.highlighted)
            .map(|r| self.words[r.row].as_str())
    }

    pub fn step(&mut self, delta: i64) {
        if self.candidates.is_empty() {
            return;
        }
        let len = self.candidates.len() as i64;
        self.highlighted = ((self.highlighted as i64 + delta).rem_euclid(len)) as usize;
    }
}

fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || c == ','
}

/// The byte range of the word under `cursor` (a byte offset, as
/// `InputState::cursor` reports). Words are delimited by whitespace and
/// commas, so `:group lhu,und` completes `und`.
pub fn word_at(line: &str, cursor: usize) -> Range<usize> {
    let cursor = cursor.min(line.len());
    let start = line[..cursor]
        .char_indices()
        .rev()
        .find(|(_, c)| is_delimiter(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let end = line[cursor..]
        .char_indices()
        .find(|(_, c)| is_delimiter(*c))
        .map(|(i, _)| cursor + i)
        .unwrap_or(line.len());
    start..end
}

pub fn rank_candidates(words: &[String], word: &str) -> Vec<Ranked> {
    rank(words, word)
}

/// Replace `word` in `line` with `candidate`; the new cursor sits after it.
pub fn accept(line: &str, word: Range<usize>, candidate: &str) -> (String, usize) {
    let mut out = String::with_capacity(line.len() + candidate.len());
    out.push_str(&line[..word.start]);
    out.push_str(candidate);
    let cursor = out.len();
    out.push_str(&line[word.end..]);
    (out, cursor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKey {
    Next,
    Prev,
    Accept,
}

pub fn completion_key(ks: &Keystroke) -> Option<CompletionKey> {
    let plain = ks.mods == Modifiers::NONE;
    let ctrl = ks.mods == Modifiers::CTRL;
    match ks.key.as_str() {
        "tab" if plain => Some(CompletionKey::Accept),
        "down" if plain => Some(CompletionKey::Next),
        "up" if plain => Some(CompletionKey::Prev),
        "n" if ctrl => Some(CompletionKey::Next),
        "p" if ctrl => Some(CompletionKey::Prev),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submit {
    /// Run the line as typed.
    Run(String),
    /// One candidate matched a word that was not yet exact: the line
    /// with it accepted, and the new cursor. Run that.
    Accepted(String, usize),
    /// Several candidates and none exact: refuse, naming them.
    Ambiguous(Vec<String>),
}

/// Enter's decision (§3.4): an exact word or no candidates runs as typed;
/// exactly one candidate is accepted and run; more is refused. The line
/// never guesses.
pub fn resolve_submit(line: &str, cursor: usize, candidates: &[Ranked], words: &[String]) -> Submit {
    let word = word_at(line, cursor);
    let typed = &line[word.clone()];
    if typed.is_empty() || candidates.is_empty() || words.iter().any(|w| w == typed) {
        return Submit::Run(line.to_string());
    }
    if candidates.len() == 1 {
        let (line, cursor) = accept(line, word, &words[candidates[0].row]);
        return Submit::Accepted(line, cursor);
    }
    Submit::Ambiguous(candidates.iter().map(|r| words[r.row].clone()).collect())
}
```

`lib.rs`: `pub mod commandline;` between `actions` and `dataprobe`.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-shell commandline::`
Expected: 5 passed.

- [ ] **Step 5: Write the failing `TestAppContext` tests**

In `shell/mod.rs` tests, with `services_with_recorder()` and `open_shell`
from Task 3:

```rust
    #[gpui::test]
    fn colon_opens_the_command_line_and_enter_runs_the_line_on_the_occupant(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("command-line").is_some(), "the strip painted");
        cx.simulate_input("unpin");
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("command-line").is_none(), "closed after a successful command");
        let shell = shell_of(&window, &cx);
        let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());
        assert!(log.borrow().contains(&crate::module::recording::Recorded::Command(tile, "unpin".into())), "{:?}", log.borrow());
        let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
        assert!(cx.update(|window, _| focused.is_focused(window)), "focus back on the shell");
    }

    #[gpui::test]
    fn completions_rank_accept_on_tab_and_submit_on_a_unique_enter(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.simulate_input("sort g");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("completion-row-0").is_some(), "gamma01 is offered");
        assert!(cx.debug_bounds("completion-row-1").is_none(), "delta01 has no g");
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &cx);
        let line = shell.read_with(&cx, |s, cx| s.command_input.read(cx).value(cx).to_string());
        assert_eq!(line, "sort gamma01");

        cx.simulate_keystrokes("enter");
        let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());
        assert!(log.borrow().contains(&crate::module::recording::Recorded::Command(tile, "sort gamma01".into())));

        // A unique match submits without tab.
        cx.simulate_keystrokes(":");
        cx.simulate_input("sort del");
        cx.simulate_keystrokes("enter");
        assert!(log.borrow().contains(&crate::module::recording::Recorded::Command(tile, "sort delta01".into())), "{:?}", log.borrow());
    }

    #[gpui::test]
    fn an_ambiguous_enter_and_a_failing_command_show_inline_and_stay_open(cx: &mut gpui::TestAppContext) {
        let (mut services, log) = services_with_recorder();
        // Make the recorder's `command` fail.
        let mut roster = crate::module::ModuleRoster::new("rec");
        let mut rec = crate::module::recording::RecordingFactory::new("rec");
        rec.command_result = Err("no such column".into());
        let log2 = rec.log.clone();
        roster.add(Box::new(rec));
        services.roster = roster;
        let _ = log;
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.simulate_input("sort a01");
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &cx);
        let error = shell.read_with(&cx, |s, _| s.command_line.as_ref().and_then(|c| c.error.clone()));
        assert!(error.as_deref().is_some_and(|e| e.contains("delta01") && e.contains("gamma01")), "{error:?}");
        assert!(log2.borrow().iter().all(|r| !matches!(r, crate::module::recording::Recorded::Command(..))), "nothing ran");

        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let error = shell.read_with(&cx, |s, _| s.command_line.as_ref().and_then(|c| c.error.clone()));
        assert_eq!(error.as_deref(), Some("no such column"), "the occupant's error, inline, line still open");
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("command-line").is_none());
    }

    #[gpui::test]
    fn slash_streams_find_events_and_escape_cancels(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("/");
        cx.simulate_input("sp");
        let shell = shell_of(&window, &cx);
        let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());
        use crate::module::{FindEvent, recording::Recorded};
        assert!(log.borrow().contains(&Recorded::Find(tile, FindEvent::Changed("sp".into()))), "{:?}", log.borrow());
        cx.simulate_keystrokes("escape");
        assert!(log.borrow().contains(&Recorded::Find(tile, FindEvent::Cancelled)));
        cx.simulate_keystrokes("/");
        cx.simulate_input("x");
        cx.simulate_keystrokes("enter");
        assert!(log.borrow().contains(&Recorded::Find(tile, FindEvent::Committed("x".into()))));
    }
```

(`simulate_input` types text through gpui's real input path so
`InputEvent::Change` fires; the palette tests in this file already rely
on it.)

- [ ] **Step 6: Implement the shell side**

`defaults.rs`: register

```rust
    action(reg, "tile::command_line", "Open the tile command line", "Tile");
    action(reg, "tile::find", "Find in tile", "Tile");
```

and add to `BUILTIN_KEYMAP`:

```toml
[[bindings]]
context = "tile"
[bindings.keys]
":" = "tile::command_line"
"/" = "tile::find"
```

(`:` arrives as the shifted character with shift cleared — see the doc
comment on `BUILTIN_KEYMAP` about `ctrl+{`; `/` is unshifted.)

`ShellView` fields:

```rust
    /// The per-tile command line's input (§3.4), built once like
    /// `palette_input`.
    command_input: Entity<InputState>,
    /// The open command line, or `None`.
    command_line: Option<CommandLine>,
```

In `new`: create `command_input` beside `palette_input` and subscribe:

```rust
        cx.subscribe_in(&command_input, window, |view, input, event, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            view.on_command_line_changed(window, cx);
        });
```

Methods:

```rust
    fn open_command_line(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tile) = self.services.workspaces.active().focused_tile() else {
            return;
        };
        if !self.occupants.contains_key(&tile) {
            return;
        }
        self.matcher.cancel();
        self.command_line = Some(CommandLine::new(prompt, tile));
        self.command_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.command_input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn close_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_line = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn on_command_line_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.command_line.as_ref() else {
            return;
        };
        let (prompt, tile) = (line.prompt, line.tile);
        let text = self.command_input.read(cx).value(cx).to_string();
        let cursor = self.command_input.read(cx).cursor();
        let Some(o) = self.occupants.get(&tile) else {
            return;
        };
        match prompt {
            Prompt::Find => o.content.find(FindEvent::Changed(text), window, cx),
            Prompt::Command => {
                let words = o.content.completions(&text, cursor, cx);
                if let Some(line) = self.command_line.as_mut() {
                    line.refresh(&text, cursor, words);
                }
            }
        }
        cx.notify();
    }

    /// Keys while the command line's input has focus. `true` if consumed.
    fn handle_command_line_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(prompt) = self.command_line.as_ref().map(|c| c.prompt) else {
            return false;
        };
        let tile = self.command_line.as_ref().map(|c| c.tile).unwrap();
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            if prompt == Prompt::Find
                && let Some(o) = self.occupants.get(&tile)
            {
                o.content.find(FindEvent::Cancelled, window, cx);
            }
            self.close_command_line(window, cx);
            return true;
        }
        if key == "enter" {
            let text = self.command_input.read(cx).value(cx).to_string();
            let cursor = self.command_input.read(cx).cursor();
            match prompt {
                Prompt::Find => {
                    if let Some(o) = self.occupants.get(&tile) {
                        o.content.find(FindEvent::Committed(text), window, cx);
                    }
                    self.close_command_line(window, cx);
                }
                Prompt::Command => {
                    let (candidates, words) = {
                        let c = self.command_line.as_ref().unwrap();
                        (c.candidates.clone(), c.words.clone())
                    };
                    let to_run = match commandline::resolve_submit(&text, cursor, &candidates, &words) {
                        commandline::Submit::Run(line) => line,
                        commandline::Submit::Accepted(line, _) => {
                            self.command_input.update(cx, |input, cx| input.set_value(line.clone(), window, cx));
                            line
                        }
                        commandline::Submit::Ambiguous(names) => {
                            if let Some(c) = self.command_line.as_mut() {
                                c.error = Some(format!("ambiguous: {}", names.join(", ")));
                            }
                            cx.notify();
                            return true;
                        }
                    };
                    let result = match self.occupants.get(&tile) {
                        Some(o) => o.content.command(&to_run, window, cx),
                        None => Err("the tile is gone".into()),
                    };
                    match result {
                        Ok(()) => self.close_command_line(window, cx),
                        Err(e) => {
                            if let Some(c) = self.command_line.as_mut() {
                                c.error = Some(e);
                            }
                            cx.notify();
                        }
                    }
                }
            }
            return true;
        }
        if prompt == Prompt::Command
            && let Some(ks) = convert_keystroke(&event.keystroke)
            && let Some(ck) = commandline::completion_key(&ks)
        {
            let c = self.command_line.as_mut().unwrap();
            match ck {
                commandline::CompletionKey::Next => c.step(1),
                commandline::CompletionKey::Prev => c.step(-1),
                commandline::CompletionKey::Accept => {
                    if let Some(word) = c.highlighted_word().map(str::to_string) {
                        let text = self.command_input.read(cx).value(cx).to_string();
                        let (line, _) = commandline::accept(&text, c.word.clone(), &word);
                        // Cycle on repeat: the next tab highlights the next
                        // candidate over the same typed word.
                        c.step(1);
                        let keep = std::mem::take(&mut c.candidates);
                        let keep_words = std::mem::take(&mut c.words);
                        let highlighted = c.highlighted;
                        self.command_input.update(cx, |input, cx| input.set_value(line, window, cx));
                        if let Some(c) = self.command_line.as_mut() {
                            c.candidates = keep;
                            c.words = keep_words;
                            c.highlighted = highlighted;
                        }
                    }
                }
            }
            cx.notify();
            return true;
        }
        false
    }
```

In `handle_key_down`, directly after the modal branch and before the
filter-input branch:

```rust
        if self.command_line.is_some()
            && self.command_input.read(cx).focus_handle(cx).is_focused(window)
        {
            if self.handle_command_line_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
```

(`set_value` does not emit `Change`, so an accepted completion does not
re-rank and wipe the cycle; a typed character does, which is right.)

`dispatch` arms:

```rust
        } else if action.0 == "tile::command_line" {
            self.open_command_line(Prompt::Command, window, cx);
        } else if action.0 == "tile::find" {
            self.open_command_line(Prompt::Find, window, cx);
```

`commandline_view.rs`:

```rust
//! Paints the per-tile command line (Phase 3 §3.4): a one-line strip
//! along the bottom edge of the focused tile with the prompt glyph, the
//! shared `Input`, and an inline error; above it, when there are
//! candidates, a popup of ranked rows with match highlighting, reusing
//! the palette's row look.

use crate::commandline::{CommandLine, Prompt};
use crate::fonts;
use crate::tiling::Rect;
use gpui::prelude::*;
use gpui::{App, Entity, IntoElement, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

pub const HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 24.0;
const MAX_ROWS: usize = 8;

pub fn render(line: &CommandLine, input: &Entity<InputState>, tile: Rect, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let strip_top = tile.y + tile.h - HEIGHT - 1.0;
    let mut strip = h_flex()
        .absolute()
        .left(px(tile.x + 1.0))
        .top(px(strip_top))
        .w(px((tile.w - 2.0).max(0.0)))
        .h(px(HEIGHT))
        .items_center()
        .gap_2()
        .px_2()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_t_1()
        .border_color(theme.border)
        .debug_selector(|| "command-line".to_string())
        .child(
            div()
                .font_family(fonts::MONO)
                .text_color(theme.muted_foreground)
                .child(line.prompt.glyph()),
        )
        .child(Input::new(input).appearance(false).w_full());
    if let Some(error) = &line.error {
        strip = strip.child(div().text_color(theme.danger).child(error.clone()));
    }

    let mut layer = div().absolute().left_0().top_0().size_full().child(strip);
    if line.prompt == Prompt::Command && !line.candidates.is_empty() {
        let rows = line.candidates.len().min(MAX_ROWS);
        let mut list = v_flex()
            .absolute()
            .left(px(tile.x + 1.0))
            .top(px(strip_top - rows as f32 * ROW_HEIGHT - 2.0))
            .w(px(((tile.w - 2.0) * 0.5).clamp(160.0, 420.0)))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded(px(4.))
            .p_1();
        for (i, r) in line.candidates.iter().take(MAX_ROWS).enumerate() {
            let text = &line.words[r.row];
            let mut row = div()
                .h(px(ROW_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(4.))
                .font_family(fonts::MONO)
                .debug_selector(move || format!("completion-row-{i}"))
                .child(crate::palette::highlighted_title(text, &r.indices, theme.primary));
            if i == line.highlighted {
                row = row.bg(theme.selection);
            }
            list = list.child(row);
        }
        layer = layer.child(list);
    }
    layer
}
```

In `render`, after the data-probe layer and before the palette overlay,
find the focused tile's rect from the layout pass already done for the
tiles (the `rects` vector; docked tiles likewise) and:

```rust
            .when_some(
                self.command_line.as_ref().zip(focused_rect),
                |el, (line, rect)| el.child(commandline_view::render(line, &self.command_input, rect, cx)),
            )
```

where `focused_rect: Option<Rect>` is captured during the tile loop when
`is_focused`. `palette::highlighted_title` becomes `pub(crate)`.
Register `pub mod commandline_view;` in `shell/mod.rs`'s module list.

- [ ] **Step 7: Run**

Run: `cargo test -p geode-shell`
Expected: green, all four command-line tests included.

- [ ] **Step 8: Full check and commit**

```bash
git add crates/geode-shell
git commit -m "feat(shell): the per-tile command line with ranked completions

One shell-owned input at the bottom of the focused tile for / and :;
the occupant supplies words, the shell ranks them with listfilter,
accepts on tab or a unique Enter, refuses an ambiguous Enter inline,
and shows a command's error without closing (Phase 3 §3.4).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 9: Harness entries** (package `geode-shell`)

```sh
# ---- command line (Phase 3 §3.4)

run_mutation "commandline: an ambiguous word is refused, never guessed" \
  crates/geode-shell/src/commandline.rs \
  '    if candidates.len() == 1 {' \
  '    if !candidates.is_empty() {' \
  geode-shell

run_mutation "commandline: an exact word runs as typed" \
  crates/geode-shell/src/commandline.rs \
  '    if typed.is_empty() || candidates.is_empty() || words.iter().any(|w| w == typed) {' \
  '    if typed.is_empty() || candidates.is_empty() {' \
  geode-shell

run_mutation "commandline: escape on a find is a cancel" \
  crates/geode-shell/src/shell/mod.rs \
  '                o.content.find(FindEvent::Cancelled, window, cx);' \
  '                let _ = o;' \
  geode-shell
```

Run: `zsh scripts/mutation-check.sh "commandline:"` — all `caught`. Commit.

---

### Task 6: Frame keys, the readout, and config reload

Spec §4.2–§4.5. `ctrl+1..9`/`ctrl+0` switch slots; the title bar shows
the frame; `groupings`, `views`, `dimensions` reload into the frame;
`sources`/`datasets` changes say "restart".

**Files:**
- Modify: `crates/geode-shell/src/defaults.rs` (actions, bindings)
- Modify: `crates/geode-shell/src/shell/toolbar.rs` (readout)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`dispatch`, `render`,
  `apply_reload`, `ShellEvent`, `save_slot`)
- Modify: `crates/geode-shell/src/shell/status.rs` (restart message)
- Test: `shell/mod.rs` tests; `toolbar.rs` has none (paint check only)

**Interfaces:**
- Produces:
  ```rust
  pub enum ShellEvent { ConfigReloaded, RestartRequired(String) }
  impl EventEmitter<ShellEvent> for ShellView {}
  impl ShellView {
      pub fn save_slot(&mut self, slot: u8, grouping: Vec<String>, cx: &mut Context<Self>) -> Result<(), String>;
      pub fn config(&self) -> &Config;
  }
  toolbar::toolbar(filter_input, readout: &FrameReadout, cx)
  status::status_bar(pending, count, reload_message, restart_message: Option<&str>, theme_name, cx)
  ```
  Actions `frame::slot_1..9`, `frame::slot_clear` (category "Frame").

- [ ] **Step 1: Write the failing tests**

```rust
    #[gpui::test]
    fn ctrl_digits_switch_the_frame_slot_and_ctrl_0_clears_it(cx: &mut gpui::TestAppContext) {
        let mut services = test_services();
        // Two slots through config, the way `new` reads them.
        let groupings = LayerDoc::builtin("groupings", "1 = [\"book\"]\n2 = [\"lhu\"]\n").unwrap();
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
        )
        .unwrap();
        services.config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), groupings, datasets],
            ..ConfigSources::default()
        });
        let (window, mut cx) = open_shell(cx, services);
        let shell = shell_of(&window, &cx);
        cx.simulate_keystrokes("ctrl-2");
        assert_eq!(shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()), Some(2));
        let v = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
        cx.simulate_keystrokes("ctrl-5");
        assert_eq!(shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()), Some(2), "an empty slot is ignored");
        assert_eq!(shell.read_with(&cx, |s, cx| s.frame.read(cx).versions()), v);
        cx.simulate_keystrokes("ctrl-0");
        assert_eq!(shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()), None);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("frame-readout").is_some(), "the readout painted");
    }

    #[gpui::test]
    fn a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart(cx: &mut gpui::TestAppContext) {
        let (services, _log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        let shell = shell_of(&window, &cx);
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&shell, move |_, event: &ShellEvent, _| sink.borrow_mut().push(event.clone())).detach();
        });

        let mut new_config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
                LayerDoc::builtin("groupings", "3 = [\"book\"]\n").unwrap(),
                LayerDoc::builtin("datasets", "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n").unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let v0 = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
        shell.update(&mut cx, |s, cx| s.apply_reload(std::mem::take(&mut new_config), cx));
        let (slots, versions) = shell.read_with(&cx, |s, cx| (s.frame.read(cx).slots().clone(), s.frame.read(cx).versions()));
        assert_eq!(slots.label(3).as_deref(), Some("book"));
        assert!(versions.config > v0.config);
        assert!(events.borrow().contains(&ShellEvent::ConfigReloaded), "{:?}", events.borrow());

        // Now a sources change.
        let mut with_sources = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
                LayerDoc::builtin("sources", "[s]\ndataset = \"risk\"\npaths = [\"/x/*.csv\"]\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        shell.update(&mut cx, |s, cx| s.apply_reload(std::mem::take(&mut with_sources), cx));
        assert!(
            events.borrow().iter().any(|e| matches!(e, ShellEvent::RestartRequired(m) if m.contains("sources"))),
            "{:?}",
            events.borrow()
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("restart-required").is_some(), "the status bar says so");
    }
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement**

`defaults.rs`: register

```rust
    for i in 1..=9 {
        action(reg, &format!("frame::slot_{i}"), &format!("Grouping slot {i}"), "Frame");
    }
    action(reg, "frame::slot_clear", "Clear grouping slot (views' own grouping)", "Frame");
```

and in the context-less `[[bindings]]` table:

```toml
"ctrl+1" = "frame::slot_1"
"ctrl+2" = "frame::slot_2"
"ctrl+3" = "frame::slot_3"
"ctrl+4" = "frame::slot_4"
"ctrl+5" = "frame::slot_5"
"ctrl+6" = "frame::slot_6"
"ctrl+7" = "frame::slot_7"
"ctrl+8" = "frame::slot_8"
"ctrl+9" = "frame::slot_9"
"ctrl+0" = "frame::slot_clear"
```

`ShellView`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// Views, dimensions or groupings changed and were applied; the app
    /// bridge forwards the new views to the data thread.
    ConfigReloaded,
    /// Sources or datasets changed; nothing was applied.
    RestartRequired(String),
}

impl EventEmitter<ShellEvent> for ShellView {}
```

field `restart_required: Option<String>`; `dispatch` arms:

```rust
        } else if let Some(n) = action.0.strip_prefix("frame::slot_").and_then(|s| s.parse::<u8>().ok()) {
            self.frame.update(cx, |f, cx| {
                if f.set_active_slot(Some(n)) {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::slot_clear" {
            self.frame.update(cx, |f, cx| {
                if f.set_active_slot(None) {
                    cx.notify();
                }
            });
```

`save_slot` (called by a module's `:group save N` through the shell?
No — modules hold the frame, not the shell. So this is a `Frame`-level
concern: give `Frame` a `pending_persist: Option<(u8, Vec<String>)>`
set by `save_slot`, and have `ShellView` observe the frame entity and
drain it):

```rust
        // In `new`:
        cx.observe(&frame, |view, frame, cx| view.on_frame_changed(frame, cx)).detach();

    fn on_frame_changed(&mut self, frame: Entity<Frame>, cx: &mut Context<Self>) {
        // A slot saved by a module (`:group save N`) is persisted here,
        // off the UI thread, because the frame is pure and the module
        // has no file access (§4.2).
        if let Some((slot, grouping)) = frame.update(cx, |f, _| f.take_pending_persist())
            && let Some(dir) = self.user_dir.clone()
        {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = crate::frame::persist_slot_to_user_config(&dir, slot, &grouping) {
                        eprintln!("[groupings] warning: {e}");
                    }
                })
                .detach();
        }
        cx.notify();
    }
```

Add to `Frame`: `pending_persist: Option<(u8, Vec<String>)>`, set in
`save_slot` on success, and `pub fn take_pending_persist(&mut self) ->
Option<(u8, Vec<String>)>`. (Add a line to Task 2's
`saving_a_slot…` test: `assert_eq!(f.take_pending_persist(), Some((3,
vec!["book".into()])));` after the second save.)

`apply_reload`, inside the `Applied` branch after the keymap/theme
handling:

```rust
            let changed = |name: &str| {
                !docs_equal(
                    self.services.config.layered_docs(name),
                    new_config.layered_docs(name),
                )
            };
            let groupings_changed = changed("groupings") || changed("datasets") || changed("dimensions");
            let views_changed = changed("views") || changed("dimensions");
            let restart = ["sources", "datasets"]
                .into_iter()
                .filter(|d| changed(d))
                .collect::<Vec<_>>();
```

(rename the existing `keymap_docs_equal` to `docs_equal`; it already
compares by table.) After `self.services.config = new_config;`:

```rust
            if groupings_changed {
                let (schema, _) = self.services.config.doc("datasets").map(SchemaSpec::from_doc).unwrap_or_default();
                let (dims, _) = self.services.config.doc("dimensions").map(DerivedDimensions::from_doc).unwrap_or_default();
                let (slots, diags) = self.services.config.doc("groupings").map(|d| GroupingSlots::from_doc(d, &schema, &dims)).unwrap_or_default();
                for d in &diags {
                    eprintln!("[groupings] {d}");
                }
                self.frame.update(cx, |f, cx| {
                    if f.replace_slots(slots) {
                        cx.notify();
                    }
                });
            }
            if views_changed {
                self.frame.update(cx, |f, cx| {
                    f.note_config_reloaded();
                    cx.notify();
                });
                cx.emit(ShellEvent::ConfigReloaded);
            }
            if !restart.is_empty() {
                let message = format!("{} changed — restart to apply", restart.join(" and "));
                self.restart_required = Some(message.clone());
                cx.emit(ShellEvent::RestartRequired(message));
            }
```

`pub fn config(&self) -> &Config { &self.services.config }` for the
bridge to read the new views.

`toolbar::toolbar(filter_input, readout, cx)`: the reserved `flex_1`
middle becomes

```rust
            .child(
                h_flex()
                    .flex_1()
                    .justify_center()
                    .gap_3()
                    .font_family(fonts::MONO)
                    .text_sm()
                    .debug_selector(|| "frame-readout".to_string())
                    .when_some(readout.as_of.as_ref(), |el, t| {
                        el.bg(theme.warning.opacity(0.25)).px_2().rounded(px(4.)).child(
                            div().text_color(theme.warning_foreground).child(format!("AS OF {t}")),
                        )
                    })
                    .child(div().text_color(theme.muted_foreground).child(match &readout.slot {
                        Some((n, label)) => format!("{n} · {label}"),
                        None => "view default".to_string(),
                    }))
                    .when(!readout.scope.is_empty(), |el| {
                        el.child(div().text_color(theme.foreground).child(readout.scope.clone()))
                    }),
            )
```

`render` computes `let readout = self.frame.read(cx).readout();` once
per frame and passes it. `status_bar` gains `restart_message:
Option<&str>` rendered in the `warning` token with
`debug_selector("restart-required")`.

- [ ] **Step 4: Run, check, commit**

Run: `cargo test -p geode-shell && cargo build -p geode-app`

```bash
git add crates/geode-shell
git commit -m "feat(shell): frame slots on ctrl+1..9, the title-bar readout, frame reload

ctrl+1..9 and ctrl+0 switch the active grouping slot; the title bar's
reserved middle shows slot, scope summary and an unmissable AS OF; a
reloaded groupings/views/dimensions doc reaches the frame and the app
bridge, a sources/datasets change asks for a restart (Phase 3 §4.2–§4.5).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 5: Harness entry** (package `geode-shell`)

```sh
run_mutation "frame: a sources change is a restart, not a silent apply" \
  crates/geode-shell/src/shell/mod.rs \
  '            let restart = ["sources", "datasets"]' \
  '            let restart = ["nonesuch"]' \
  geode-shell
```

Run: `zsh scripts/mutation-check.sh "frame:"` — all `caught`. Commit.

---

### Task 7: `RequeryStats` in the perf overlay

Spec §6.8. Two always-compiled histograms the blotter records into
through the frame; the overlay shows them.

**Files:**
- Modify: `crates/geode-shell/src/perf.rs` (replace Task 2's stub)
- Modify: `crates/geode-shell/src/shell/perf_overlay.rs` (signature,
  rows)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`perf::reset` arm,
  overlay call)
- Test: `perf.rs` inline

**Interfaces:**
- Produces:
  ```rust
  pub struct RequeryStats { .. }
  impl RequeryStats {
      pub const fn new() -> Self;
      pub fn record_submit_to_snapshot(&mut self, micros: u64);
      pub fn record_snapshot_to_paint(&mut self, micros: u64);
      pub fn last(&self) -> Option<(u64, u64)>;      // (submit→snapshot, snapshot→paint), the last completed pair
      pub fn submit_to_snapshot(&self) -> &FrameHistogram;
      pub fn snapshot_to_paint(&self) -> &FrameHistogram;
      pub fn reset(&mut self);
  }
  perf_overlay::render(hist: &FrameHistogram, requery: &RequeryStats, toolbar_height, cx)
  ```

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn requery_stats_pair_the_two_halves_and_summarise_each() {
        let mut s = RequeryStats::new();
        assert_eq!(s.last(), None);
        s.record_submit_to_snapshot(12_000);
        assert_eq!(s.last(), None, "half a pair is not a pair");
        s.record_snapshot_to_paint(3_000);
        assert_eq!(s.last(), Some((12_000, 3_000)));
        s.record_submit_to_snapshot(20_000);
        s.record_snapshot_to_paint(4_000);
        assert_eq!(s.last(), Some((20_000, 4_000)));
        assert_eq!(s.submit_to_snapshot().count(), 2);
        assert_eq!(s.snapshot_to_paint().count(), 2);
        assert!(s.submit_to_snapshot().percentile_micros(50.0).unwrap() >= 12_000);
        s.reset();
        assert_eq!(s.last(), None);
        assert_eq!(s.submit_to_snapshot().count(), 0);
    }
```

- [ ] **Step 2: Implement**

```rust
/// Requery timing (Phase 3 §6.8): the two halves of §7.1's "query +
/// snapshot handoff + first painted frame" that the headless benchmarks
/// cannot see. The blotter records submit→snapshot on `deliver` and
/// snapshot→paint on the first render after it. Fixed-size, allocation-
/// free, never notifies — the same discipline as `FrameHistogram`.
#[derive(Debug)]
pub struct RequeryStats {
    submit_to_snapshot: FrameHistogram,
    snapshot_to_paint: FrameHistogram,
    pending_snapshot: Option<u64>,
    last: Option<(u64, u64)>,
}

impl Default for RequeryStats {
    fn default() -> Self {
        Self::new()
    }
}

impl RequeryStats {
    pub const fn new() -> Self {
        RequeryStats {
            submit_to_snapshot: FrameHistogram::new(),
            snapshot_to_paint: FrameHistogram::new(),
            pending_snapshot: None,
            last: None,
        }
    }

    pub fn record_submit_to_snapshot(&mut self, micros: u64) {
        self.submit_to_snapshot.record_micros(micros);
        self.pending_snapshot = Some(micros);
    }

    pub fn record_snapshot_to_paint(&mut self, micros: u64) {
        self.snapshot_to_paint.record_micros(micros);
        if let Some(first) = self.pending_snapshot.take() {
            self.last = Some((first, micros));
        }
    }

    pub fn last(&self) -> Option<(u64, u64)> {
        self.last
    }

    pub fn submit_to_snapshot(&self) -> &FrameHistogram {
        &self.submit_to_snapshot
    }

    pub fn snapshot_to_paint(&self) -> &FrameHistogram {
        &self.snapshot_to_paint
    }

    pub fn reset(&mut self) {
        self.submit_to_snapshot.reset();
        self.snapshot_to_paint.reset();
        self.pending_snapshot = None;
        self.last = None;
    }
}
```

`perf_overlay::render` gains `requery: &RequeryStats` and three rows
after `max`:

```rust
                .child(row("requery", match requery.last() {
                    Some((q, p)) => format!("{} + {}", format_ms(q), format_ms(p)),
                    None => dash(),
                }, cx))
                .child(row("q p50", requery.submit_to_snapshot().percentile_micros(50.0).map_or_else(dash, format_ms), cx))
                .child(row("paint p50", requery.snapshot_to_paint().percentile_micros(50.0).map_or_else(dash, format_ms), cx))
```

In `render`, pass `&self.frame.read(cx).requery`. In the `perf::reset`
arm, also `self.frame.update(cx, |f, _| f.requery.reset())`.

- [ ] **Step 3: Run, check, commit**

Run: `cargo test -p geode-shell`

```bash
git add crates/geode-shell
git commit -m "feat(shell): requery timing in the perf overlay

RequeryStats pairs submit→snapshot with snapshot→first-paint, the half
of §7.1 the benchmarks stop short of; shown as three rows in the
overlay and zeroed by perf::reset (Phase 3 §6.8).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

---

### Task 8: Unfiltered harness run and docs

- [ ] **Step 1: Run the whole harness**

Run: `zsh scripts/mutation-check.sh`
Expected: every line `caught`. Fix any `SURVIVED` in the owning task.

- [ ] **Step 2: Docs**

- `CLAUDE.md`: the harness entry count; in the Phase 1 paragraph's list
  of what the shell has, add "count prefixes in the keymap engine
  (`KeyContext::counts`), the module-hosting contract
  (`shell::module`), the shared frame (`shell::frame`, `ctrl+1..9`),
  and the per-tile command line (`/`, `:` in the `tile` context)". Add
  a line to the gotchas: **"A tile occupant that tracks its own focus
  handle takes focus on click; the tile's mouse-down handler re-arms
  `pending_focus_restore` so the shell's chords survive. Keep it."**
- `docs/perf.md`: under the overlay section, the three new rows and
  what they measure.

- [ ] **Step 3: Final check and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`

```bash
git add CLAUDE.md docs/perf.md scripts/mutation-check.sh
git commit -m "docs: Phase 3b landed — hosting contract, frame, counts, command line

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

---

## Self-Review

**Spec coverage:**

| Spec | Task |
|---|---|
| §2.1 factory carries no service handles | 3 |
| §2.2 shell-owned input; `find_style` semantics | 5 (input and events); the vim/fzf behaviour is the blotter's, Plan 3c |
| §2.3, §3.3 counts in the engine | 1 |
| §3.1 `TileContent`, `FindEvent`, `TileOccupant`; `QueryOutcome` routing | 3 |
| §3.2 factory, roster, default kind, placeholder for unknown | 3 |
| §3.3 context stack, dispatch fall-through, sequences, focus restore, reclaimed `DataTable` keys | 3 (all but the reclaim — the `DataTable` `NoAction` bindings are added in Plan 3c where the table first appears) |
| §3.4 command line, completions, prompts, errors | 5 |
| §3.5 session `tiles` | 4 |
| §4.1 `Frame`, versions, `effective_scope` | 2 |
| §4.2 numbered slots, `:group save` persistence | 2, 6 |
| §4.3 `:` vocabulary | the frame API (2) supports every row; the parsing is the blotter's (Plan 3c) |
| §4.4 readout, AS OF, pinned/unscoped tile markers | 6 (readout); tile markers are the blotter's header strip (3c) |
| §4.5 reload wiring, restart diagnostic | 6 |
| §6.8 `RequeryStats` | 7 |
| §7.2 shell tests listed | 1, 2, 3, 4, 5, 6 |

**Placeholder scan:** none. Every step carries code.

**Type consistency:**
- `TileContent::dispatch(action, count, window, cx)` (3) matches
  `ShellView::dispatch`'s new `count` (1).
- `FindEvent` (3) is what Task 5 sends.
- `TileRecord`/`TileRecords`/`Restored` (4) match `ShellServices::
  restored_tiles` (3) — Task 3 defines the struct in `session.rs` so
  the field compiles before Task 4 fills it in.
- `Frame::take_pending_persist` is added in Task 6 with a test line
  appended to Task 2's test.
- `RequeryStats` stub (2) is replaced whole in Task 7 with the same
  `new()`.
- `context_stack(&self, cx: &App)` (3) is called with `cx` from
  `render`, `handle_key_down` and `is_palette_toggle`.

## Execution Handoff

Plan complete. Plan 3c (the blotter, `--demo`, probe deletion) follows
and depends on both 3a and 3b.
