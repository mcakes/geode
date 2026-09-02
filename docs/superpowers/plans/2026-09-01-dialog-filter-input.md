# Dialog Filter Input Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the vim/fzf `/` find model in Geode's settings and keybindings dialogs with an always-focused fuzzy filter input plus arrow-key navigation.

**Architecture:** Both dialogs adopt the command palette's existing pattern — a gpui-component `Entity<InputState>` owned by `ShellView`, focused on open, feeding a pure ranking helper that filters and re-orders rows. A new pure module (`dialogfilter`) owns ranking and the nav-key vocabulary; `vimnav::apply` still does the clamped selection arithmetic. Rebind capture keeps working by blurring the input while listening, so raw keystrokes reach the modal key handler exactly as they do today.

**Tech Stack:** Rust, gpui (Zed's UI framework, unpinned git dep), gpui-component (pinned rev `0e2fb7a`), criterion for benches.

**Spec:** `docs/superpowers/specs/2026-09-01-dialog-filter-input-design.md` — read it first; this plan argues from it and cites its sections.

## Global Constraints

- All four CI checks must pass, on **both macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` (warnings are errors), `cargo test --workspace`, `cargo bench --workspace --no-run`.
- **No raw colors.** Every color comes from `cx.theme()` tokens (`theme.muted_foreground`, `theme.border`, `theme.selection`, `theme.primary`, …). Never construct an `Hsla` literal for UI chrome.
- **Nothing may stall the render thread** (`docs/PHILOSOPHY.md`). No file or socket I/O on the UI thread; config writes stay on `cx.background_executor()`.
- **Pure cores are TDD'd and free of `gpui`** — write the failing test first, watch it fail, then implement.
- **Modals open only through `dialog::open_shell_dialog` / `open_shell_dialog_with_key`** — the one mandatory door (`shell::dialog` module doc).
- **Per-frame heap churn is a defect.** Both dialogs already derive rows fresh per render; do not add caching layers, and do not add allocations beyond the one ranking pass those rows already imply.
- `vimnav` and `vimfind` stay in the crate **whole** — `FindStyle`, its config plumbing, and the settings row for it are all untouched (spec §8). Only the two dialogs stop *reading* the find style.
- Commit after each task with the repo's trailers:
  ```
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_013AGrUwbHcA9ef7Mz2gQNrK
  ```

---

### Task 1: The `dialogfilter` pure core

Ranking and the nav-key vocabulary, with no `gpui` anywhere — the piece both dialogs will consume in Tasks 2 and 3.

**Files:**
- Create: `crates/geode-shell/src/dialogfilter.rs`
- Modify: `crates/geode-shell/src/lib.rs:21` (module list, alphabetical — `dialogfilter` sorts between `defaults` and `fonts`)
- Test: `crates/geode-shell/src/dialogfilter.rs` (inline `#[cfg(test)] mod tests`, the house pattern — see `vimnav.rs`)

**Interfaces:**
- Consumes: `palette::fuzzy_match(query: &str, candidate: &str) -> Option<(u32, Vec<usize>)>` (`palette.rs:102`); `vimnav::NavCommand` (`vimnav.rs:69`); `keymap::{Keystroke, Modifiers}`.
- Produces:
  - `pub struct Ranked { pub row: usize, pub indices: Vec<usize> }`
  - `pub fn rank(texts: &[String], query: &str) -> Vec<Ranked>`
  - `pub fn nav_command(ks: &Keystroke) -> Option<NavCommand>`

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-shell/src/dialogfilter.rs` with the module doc, the two public signatures stubbed with `todo!()`, and this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn texts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn rows(r: &[Ranked]) -> Vec<usize> {
        r.iter().map(|m| m.row).collect()
    }

    fn key(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: k.to_string(),
        }
    }

    fn ctrl(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::CTRL,
            key: k.to_string(),
        }
    }

    #[test]
    fn an_empty_query_keeps_every_row_in_natural_order() {
        let t = texts(&["Focus left Workspace", "Split right Workspace"]);
        let ranked = rank(&t, "");
        assert_eq!(rows(&ranked), vec![0, 1]);
        assert!(
            ranked.iter().all(|m| m.indices.is_empty()),
            "an empty query highlights nothing"
        );
    }

    #[test]
    fn a_whitespace_only_query_is_treated_as_empty() {
        let t = texts(&["Focus left Workspace", "Split right Workspace"]);
        assert_eq!(rows(&rank(&t, "   ")), vec![0, 1]);
    }

    #[test]
    fn a_query_drops_rows_that_do_not_match() {
        let t = texts(&["Focus left Workspace", "Toggle theme Appearance"]);
        assert_eq!(rows(&rank(&t, "theme")), vec![1]);
    }

    #[test]
    fn matching_is_case_insensitive_and_subsequence_based() {
        let t = texts(&["Focus left Workspace"]);
        assert_eq!(rows(&rank(&t, "FCSLFT")), vec![0]);
    }

    #[test]
    fn rows_are_ordered_by_score_not_by_position() {
        // "split" is a contiguous prefix-ish run in row 1 and a scattered
        // subsequence in row 0, so row 1 must outrank it despite coming
        // second in natural order.
        let t = texts(&["Set panel list toggle Misc", "Split right Workspace"]);
        assert_eq!(rows(&rank(&t, "split")), vec![1, 0]);
    }

    #[test]
    fn equal_scores_keep_natural_order() {
        let t = texts(&["Focus up Workspace", "Focus up Docks"]);
        let ranked = rank(&t, "focus up");
        assert_eq!(
            rows(&ranked),
            vec![0, 1],
            "identical match shapes must not reorder"
        );
    }

    #[test]
    fn indices_are_char_offsets_into_the_row_text() {
        let t = texts(&["Focus left"]);
        let ranked = rank(&t, "fl");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].indices, vec![0, 6]);
    }

    #[test]
    fn nav_command_maps_the_whole_dialog_vocabulary() {
        let cases: Vec<(Keystroke, NavCommand)> = vec![
            (key("up"), NavCommand::Move(-1)),
            (key("down"), NavCommand::Move(1)),
            (ctrl("p"), NavCommand::Move(-1)),
            (ctrl("n"), NavCommand::Move(1)),
            (ctrl("u"), NavCommand::Move(-5)),
            (ctrl("d"), NavCommand::Move(5)),
            (ctrl("b"), NavCommand::Move(-10)),
            (ctrl("f"), NavCommand::Move(10)),
            (key("pageup"), NavCommand::Move(-10)),
            (key("pagedown"), NavCommand::Move(10)),
        ];
        for (ks, expected) in cases {
            assert_eq!(
                nav_command(&ks),
                Some(expected),
                "wrong command for {:?}",
                ks.key
            );
        }
    }

    #[test]
    fn nav_command_claims_nothing_else() {
        // Keys the dialogs must leave alone: text the input owns, the
        // dialogs' own control keys, and the retired vim motions.
        for ks in [
            key("j"),
            key("k"),
            key("g"),
            key("enter"),
            key("escape"),
            key("tab"),
            key("left"),
            key("right"),
            key("home"),
            key("end"),
            key("/"),
        ] {
            assert_eq!(nav_command(&ks), None, "{} must not be nav", ks.key);
        }
    }

    #[test]
    fn nav_command_requires_exactly_the_named_modifiers() {
        let shift_up = Keystroke {
            mods: Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            key: "up".to_string(),
        };
        assert_eq!(nav_command(&shift_up), None, "shift+up is a selection key");

        let ctrl_shift_d = Keystroke {
            mods: Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            },
            key: "d".to_string(),
        };
        assert_eq!(nav_command(&ctrl_shift_d), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell dialogfilter`
Expected: FAIL — the `todo!()` stubs panic (or the file does not compile until `lib.rs` declares the module; add `pub mod dialogfilter;` to `crates/geode-shell/src/lib.rs` first, between `pub mod defaults;` and `pub mod fonts;`).

- [ ] **Step 3: Write the implementation**

Replace the stubs in `crates/geode-shell/src/dialogfilter.rs`:

```rust
//! Shared pure core for the two list dialogs' filter-first UX
//! (`docs/superpowers/specs/2026-09-01-dialog-filter-input-design.md` §4,
//! §6): fuzzy ranking of row text, and the keystroke vocabulary the
//! dialogs still own once a focused text input has claimed every
//! printable key.
//!
//! No `gpui` here, in the mould of [`crate::vimnav`] and
//! [`crate::vimfind`] — feed it plain strings and shell-native
//! [`Keystroke`]s, unit-test it without a window.
//!
//! ## Why the vocabulary is what it is
//!
//! With a single-line gpui-component `Input` focused, the dialogs can
//! only claim keys that input does not consume first. Verified against
//! the pinned rev (spec §2): `left`/`right`/`home`/`end` are swallowed
//! unconditionally; `up`/`down`/`pageup`/`pagedown` and `tab`/`shift+tab`
//! attach their listeners only for multi-line inputs, so they fall
//! through; `ctrl+d`/`u`/`b`/`n`/`p` are unbound in the `"Input"` context
//! on both platforms. `ctrl+f` is bound to the editor's Search on
//! non-macOS and is reclaimed for us by a `NoAction` binding in
//! `geode-app`'s init (spec §7).
//!
//! `pageup`/`pagedown` are deliberate aliases of `ctrl+b`/`ctrl+f`, not a
//! third step size: they are free, and they are what a hand reaching for
//! "a screenful" finds first on a keyboard that has them.

use crate::keymap::{Keystroke, Modifiers};
use crate::palette::fuzzy_match;
use crate::vimnav::NavCommand;

/// One row that survived the filter: its index into the *unfiltered* row
/// list, plus the char offsets of the query's matched characters within
/// that row's searchable text (what the dialogs paint as highlights).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub row: usize,
    pub indices: Vec<usize>,
}

/// Rank `texts` against `query`.
///
/// An empty (or whitespace-only) query keeps every row, in its natural
/// order, highlighting nothing — the dialogs' resting state. Otherwise
/// only rows whose text fuzzy-matches survive, ordered by
/// [`fuzzy_match`]'s score descending; ties keep natural order, which is
/// what the stable sort below buys and what the dialogs' own
/// `(category, title)` ordering depends on to stay legible.
pub fn rank(texts: &[String], query: &str) -> Vec<Ranked> {
    if query.trim().is_empty() {
        return texts
            .iter()
            .enumerate()
            .map(|(row, _)| Ranked {
                row,
                indices: Vec::new(),
            })
            .collect();
    }

    let mut scored: Vec<(u32, Ranked)> = texts
        .iter()
        .enumerate()
        .filter_map(|(row, text)| {
            fuzzy_match(query, text).map(|(score, indices)| (score, Ranked { row, indices }))
        })
        .collect();
    // `sort_by` is stable, so equal scores keep the natural order the
    // rows arrived in.
    scored.sort_by(|(a, _), (b, _)| b.cmp(a));
    scored.into_iter().map(|(_, ranked)| ranked).collect()
}

/// Map one keystroke onto a list-navigation command, or `None` if the
/// dialogs do not claim it (see the module doc for what they *can*
/// claim). The caller feeds the result to [`crate::vimnav::apply`], which
/// clamps against the current — filtered — row count.
pub fn nav_command(ks: &Keystroke) -> Option<NavCommand> {
    let delta = match (ks.mods, ks.key.as_str()) {
        (Modifiers::NONE, "up") | (Modifiers::CTRL, "p") => -1,
        (Modifiers::NONE, "down") | (Modifiers::CTRL, "n") => 1,
        (Modifiers::CTRL, "u") => -5,
        (Modifiers::CTRL, "d") => 5,
        (Modifiers::CTRL, "b") | (Modifiers::NONE, "pageup") => -10,
        (Modifiers::CTRL, "f") | (Modifiers::NONE, "pagedown") => 10,
        _ => return None,
    };
    Some(NavCommand::Move(delta))
}
```

If `match` on `(ks.mods, ...)` against the `Modifiers::NONE` / `Modifiers::CTRL` constants does not compile as a pattern (associated constants are only usable as patterns for types deriving `PartialEq` + `Eq` and marked `#[derive(PartialEq, Eq)]` — `Modifiers` does, `keystroke.rs:2`), keep it; otherwise fall back to guards of the form `k if ks.mods == Modifiers::CTRL && k == "u" =>`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-shell dialogfilter`
Expected: PASS, 10 tests.

If `rows_are_ordered_by_score_not_by_position` fails, do **not** weaken the assertion — read `palette::fuzzy_match`'s scoring (`palette.rs:102-160`) and pick two fixture strings that genuinely score differently, then update the test's strings and its comment to match what you verified.

- [ ] **Step 5: Verify the workspace is clean and commit**

Run: `cargo fmt --check && cargo clippy -p geode-shell --all-targets -- -D warnings && cargo test -p geode-shell`
Expected: all clean.

```bash
git add crates/geode-shell/src/dialogfilter.rs crates/geode-shell/src/lib.rs
git commit -m "$(cat <<'EOF'
feat: add the dialogfilter pure core

Fuzzy ranking over row text and the keystroke vocabulary the list
dialogs can still claim once a focused single-line Input has taken every
printable key. No gpui, in the mould of vimnav/vimfind.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013AGrUwbHcA9ef7Mz2gQNrK
EOF
)"
```

---

### Task 2: The keybindings dialog, plus the shared input plumbing

Converts the keybindings dialog end to end and introduces the `ShellView` machinery both dialogs will share. Task 3 reuses all of it.

**Files:**
- Modify: `crates/geode-shell/src/shell/mod.rs` — new `dialog_input` field (beside `palette_input`, `mod.rs:373`), built in `new` (near `mod.rs:516`), a new `InputEvent::Change` subscription, a new `close_modal` method, and the three modal-close sites
- Modify: `crates/geode-shell/src/shell/dialog.rs` — `open_shell_dialog_with_key` focuses the dialog input; the two close listeners (`dialog.rs:322`, `dialog.rs:371`) go through `close_modal`; new `filter_row` render helper
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs` — state, `handle_key`, `build`, module doc, tests
- Test: `crates/geode-shell/src/shell/keybindings_view.rs` (inline pure tests) and `crates/geode-shell/src/shell/mod.rs` (`#[gpui::test]`s, alongside the existing dialog tests)

**Interfaces:**
- Consumes: `dialogfilter::{Ranked, rank, nav_command}` (Task 1); `vimnav::apply(selected, len, cmd)`; `palette::fuzzy_match`; `keybindings_view::{press_while_listening, CaptureOutcome, searchable_text, derive_rows, spawn_rebind, is_same_key_recapture}` (all unchanged).
- Produces:
  - `ShellView::dialog_input: Entity<InputState>` — the shared filter field for **both** dialogs
  - `ShellView::close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>)`
  - `dialog::open_shell_dialog_with_key(.., focus_filter: bool)` — one new trailing parameter
  - `dialog::filter_row(input: &Entity<InputState>, frozen: Option<&str>, cx: &App) -> AnyElement`
  - `KeybindingsState { selected, listening, query }` with `pub fn set_query(&mut self, query: String)`
  - `keybindings_view::visible_rows(state: &KeybindingsState, rows: &[KeybindingRow]) -> Vec<Ranked>`
  - `keybindings_view::filtered_position(visible: &[Ranked], rows: &[KeybindingRow], clicked: &ActionId) -> Option<usize>`

- [ ] **Step 1: Write the failing pure tests**

In `keybindings_view.rs`'s `mod tests`, **delete** the vim/fzf find tests (every test naming `find`, `fzf`, `repeat`, or `anchor` — they drive a session this dialog no longer has) and add:

```rust
    #[test]
    fn an_empty_query_shows_every_row_in_derivation_order() {
        let rows = test_rows();
        let state = KeybindingsState::new();
        let visible = visible_rows(&state, &rows);
        assert_eq!(
            visible.iter().map(|m| m.row).collect::<Vec<_>>(),
            (0..rows.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_query_narrows_and_reranks_the_visible_rows() {
        let rows = test_rows();
        let mut state = KeybindingsState::new();
        state.set_query("pal".to_string());
        let visible = visible_rows(&state, &rows);
        assert!(!visible.is_empty(), "'pal' must match the palette row");
        assert!(
            visible.len() < rows.len(),
            "'pal' must not match every row"
        );
        let titles: Vec<&str> = visible
            .iter()
            .map(|m| rows[m.row].title.as_str())
            .collect();
        assert!(
            titles[0].to_lowercase().contains("pal"),
            "best match first, got {titles:?}"
        );
    }

    #[test]
    fn setting_a_query_resets_the_selection_to_the_top_match() {
        let mut state = KeybindingsState::new();
        state.selected = 3;
        state.set_query("z".to_string());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn a_click_resolves_an_action_id_to_its_filtered_position() {
        // The list the user clicks is the filtered one, so a row's click
        // handler (keyed by ActionId, as it always was) must resolve to a
        // position in THAT list, not in the full one.
        let rows = test_rows();
        let mut state = KeybindingsState::new();
        state.set_query(rows[2].title.clone());
        let visible = visible_rows(&state, &rows);
        assert_eq!(
            filtered_position(&visible, &rows, &rows[2].action),
            Some(0),
            "the only match sits at filtered position 0, whatever its \
             position in the full list"
        );
    }

    #[test]
    fn a_click_on_a_row_the_filter_hid_resolves_to_nothing() {
        let rows = test_rows();
        let mut state = KeybindingsState::new();
        state.set_query(rows[2].title.clone());
        let visible = visible_rows(&state, &rows);
        let hidden = rows
            .iter()
            .find(|r| !visible.iter().any(|m| rows[m.row].action == r.action))
            .expect("the query must hide at least one row");
        assert_eq!(filtered_position(&visible, &rows, &hidden.action), None);
    }

    #[test]
    fn setting_a_query_cancels_an_in_progress_capture() {
        // The filter is always live; a query edit is an external
        // interruption to a capture in exactly the way a click is.
        let mut state = KeybindingsState::new();
        state.listening = Some(vec![key("a")]);
        state.set_query("foc".to_string());
        assert!(state.listening.is_none());
    }
```

Add a `test_rows()` helper if one does not already exist, reusing the existing fixture builder in that test module (the rows shaped `title` + `category` — see the current `fn rows(...)` fixture used by the find tests you are deleting; keep it, rename it `test_rows` if it collides).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell keybindings_view`
Expected: FAIL — `visible_rows` and `set_query` do not exist; `KeybindingsState` still has `nav`/`find`/`find_anchor`.

- [ ] **Step 3: Rewrite `KeybindingsState` and add the pure helpers**

In `keybindings_view.rs`, replace the state struct and add the helper:

```rust
/// Persistent state for one open keybinding dialog session — the
/// analogue of `palette::PaletteState`. Holds no `gpui` types (see the
/// module doc's "Architecture" section for why the scroll handle lives
/// beside this instead of inside it), so every transition here is
/// unit-testable without a window.
#[derive(Debug, Default)]
pub struct KeybindingsState {
    /// Index into the **filtered** row list ([`visible_rows`]), not into
    /// the full one — the palette's convention, and what
    /// `vimnav::apply` clamps against. Row identity for clicks and
    /// rebinds is resolved through `visible_rows(..)[selected].row`.
    pub selected: usize,
    /// `Some(pending)` while listening for a new binding — `pending` is
    /// the keystroke sequence captured so far, appended to by
    /// [`press_while_listening`] on every keystroke except a bare
    /// `enter`/`escape`. `None` in ordinary list-navigation mode. While
    /// this is `Some`, `ShellView::dialog_input` is deliberately blurred
    /// so raw keystrokes reach this dialog instead of the filter (spec
    /// §3, "Rebind capture").
    pub listening: Option<Vec<Keystroke>>,
    /// The filter query, mirrored here from `ShellView::dialog_input` by
    /// the `InputEvent::Change` subscription in `ShellView::new`. The
    /// `Input` owns the text; this is the pure copy the row list is
    /// ranked against.
    pub query: String,
}

impl KeybindingsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the query — the pure half of the `InputEvent::Change`
    /// subscription. Resets the selection to the top match (the
    /// palette's `set_query` semantics: after an edit the old index
    /// points at an unrelated row) and cancels any in-progress capture,
    /// since typing is an external interruption to it exactly as a click
    /// is ([`click_selects_or_listens`]).
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
        self.listening = None;
    }
}

/// The rows this dialog currently shows, ranked — [`crate::dialogfilter::rank`]
/// over each row's [`searchable_text`]. Derived fresh at every call site
/// (render, key handling, click resolution), never cached: the same
/// no-caching contract [`derive_rows`] itself has, and the row counts
/// here are small enough that one ranking pass costs nothing measurable.
pub fn visible_rows(state: &KeybindingsState, rows: &[KeybindingRow]) -> Vec<Ranked> {
    let texts: Vec<String> = rows.iter().map(searchable_text).collect();
    dialogfilter::rank(&texts, &state.query)
}

/// Where the row for `clicked` currently sits in the *filtered* list, or
/// `None` if the filter is hiding it. Row click handlers stay keyed by
/// [`ActionId`] — identity, never position, so a row survives the list
/// being reordered under it — and this is the one place that identity is
/// turned back into the index [`KeybindingsState::selected`] speaks.
pub fn filtered_position(
    visible: &[Ranked],
    rows: &[KeybindingRow],
    clicked: &ActionId,
) -> Option<usize> {
    visible
        .iter()
        .position(|m| rows.get(m.row).is_some_and(|r| &r.action == clicked))
}
```

Update the imports at the top of the file: drop `vimfind::{self, FindDirection, FindStyle, FzfOutcome, VimFind, match_range}` and `vimnav::{self, NavResult, VimListNav}`; add `use crate::dialogfilter::{self, Ranked};` and `use crate::vimnav;`. Delete the now-unreferenced wrappers `press_while_finding`, `press_while_finding_fzf`, `highlight_query`, `repeat_find`, and `filter_matches`'s local import.

- [ ] **Step 4: Run the pure tests to verify they pass**

Run: `cargo test -p geode-shell keybindings_view`
Expected: the four new tests PASS. `handle_key`/`build` will not compile yet — that is Step 5; if the crate fails to build, finish Step 5 and re-run.

- [ ] **Step 5: Rewrite `handle_key` and `click_selects_or_listens`**

Replace `keybindings_view::handle_key` (currently `keybindings_view.rs:476`) with:

```rust
/// The [`dialog::ModalKeyHandler`] for this dialog. Priority order:
///
/// 1. while listening, every keystroke is offered to
///    [`press_while_listening`] and swallowed unconditionally (`true`) —
///    even `escape`, which must cancel the capture rather than falling
///    through to `handle_key_down`'s "escape closes the modal";
/// 2. bare `enter` starts listening on the selected row and blurs the
///    filter input, so the capture sees raw keystrokes (spec §3);
/// 3. [`dialogfilter::nav_command`] motions move the selection within the
///    *filtered* list;
/// 4. everything else returns `false`, unhandled — which for a printable
///    key is exactly right: the modal branch in `handle_key_down` does
///    not `stop_propagation`, so the character goes on to the focused
///    `Input`'s own text-insertion phase (the same reasoning
///    `handle_palette_key`'s catch-all arm carries).
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let user_dir = shell.user_dir.clone();
    let input = shell.dialog_input.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return false;
    };
    let visible = visible_rows(state, &rows);

    if let Some(pending) = state.listening.as_mut() {
        match press_while_listening(pending, ks) {
            CaptureOutcome::Continue => {}
            CaptureOutcome::Cancel => {
                state.listening = None;
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            CaptureOutcome::Commit(keystrokes) => {
                state.listening = None;
                let selected = state.selected;
                input.read(cx).focus_handle(cx).focus(window, cx);
                if let Some(row) = visible.get(selected).and_then(|m| rows.get(m.row))
                    && !is_same_key_recapture(row, &keystrokes)
                {
                    spawn_rebind(row, keystrokes, user_dir, cx);
                }
            }
        }
        cx.notify();
        return true;
    }

    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        if visible.is_empty() {
            return true;
        }
        state.listening = Some(Vec::new());
        // Hand focus back to the shell root so the capture sees raw
        // keystrokes: with the filter focused, a bare letter would be
        // consumed as text by gpui-component's `Input` before ever
        // reaching this handler (spec §2a, §3).
        shell.focus_handle.focus(window, cx);
        cx.notify();
        return true;
    }

    if let Some(cmd) = dialogfilter::nav_command(ks) {
        state.selected = vimnav::apply(state.selected, visible.len(), cmd);
        let selected = state.selected;
        shell.keybindings_scroll.scroll_to_item(selected);
        cx.notify();
        return true;
    }

    false
}
```

Note the borrow shape: `state` is a `&mut` borrow of `shell.keybindings`, so `shell.focus_handle` / `shell.keybindings_scroll` cannot be touched while it is live. Where the code above uses both, copy the needed value out first (`let selected = state.selected;`) and let the `state` borrow end — split the function into `{ ... }` blocks if the borrow checker complains, rather than cloning state.

Then update `click_selects_or_listens` to drop the find fields:

```rust
pub fn click_selects_or_listens(state: &mut KeybindingsState, clicked_ix: usize) {
    if state.selected == clicked_ix && state.listening.is_none() {
        state.listening = Some(Vec::new());
    } else {
        state.selected = clicked_ix;
        state.listening = None;
    }
}
```

and `on_row_clicked` to resolve the click through the filtered list, and to blur/refocus the input the same way `handle_key` does:

```rust
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: &ActionId,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let input = shell.dialog_input.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return;
    };
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    click_selects_or_listens(state, ix);
    let listening = state.listening.is_some();
    let selected = state.selected;
    shell.keybindings_scroll.scroll_to_item(selected);
    if listening {
        shell.focus_handle.focus(window, cx);
    } else {
        input.read(cx).focus_handle(cx).focus(window, cx);
    }
    cx.notify();
}
```

`on_row_clicked` now needs a `&mut Window`; its call site is the row's `on_mouse_down` closure in `build`, which already receives `window` — change that closure from `move |_event, _window, cx|` to `move |_event, window, cx|` and pass it through the `entity_for_row.update(cx, ..)` call by using `cx.update_entity` style access or by capturing `window` in the closure body (the closure's `window: &mut Window` is available inside `update`'s body — thread it as an argument).

- [ ] **Step 6: Add `dialog_input`, `close_modal`, and the subscription**

In `crates/geode-shell/src/shell/mod.rs`:

Add the field next to `palette_input` (`mod.rs:373`):

```rust
    /// The two list dialogs' shared filter field (the filter-first dialog
    /// UX, spec §5). One entity, not one per dialog: only one modal is
    /// ever open (`dialog::open_shell_dialog_with_key` refuses to open
    /// over another), so they can never both want it at once. Built once
    /// here and reset by value on every open, exactly like
    /// `palette_input` above and for the same reasons — a stable
    /// `FocusHandle` across close/reopen, and one `InputEvent::Change`
    /// subscription for the life of the window instead of one per open.
    ///
    /// Deliberately blurred while the keybinding dialog is listening for
    /// a new binding: a focused `Input` consumes bare letters as text
    /// before any raw key listener sees them, so capture would be
    /// impossible otherwise (spec §2a, §3).
    dialog_input: Entity<InputState>,
```

Build it in `new`, right after `palette_input`'s subscription:

```rust
        // The dialogs' shared filter field — same lifecycle as
        // `palette_input` above (see that field's doc comment).
        let dialog_input = cx.new(|cx| InputState::new(window, cx).placeholder("filter"));
        cx.subscribe_in(&dialog_input, window, |view, input, event, _window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let query = input.read(cx).value().to_string();
            // Route to whichever dialog is actually open. `close_modal`
            // clears both fields, so at most one is `Some` here — the
            // routing cannot land in a stale state left over from an
            // earlier open.
            if let Some(state) = view.keybindings.as_mut() {
                state.set_query(query);
                view.keybindings_scroll.scroll_to_item(0);
            }
            cx.notify();
        })
        .detach();
```

(Task 3 adds the `settings` arm to this same subscription.)

Add `dialog_input` to the `Self { .. }` construction alongside `palette_input` (`mod.rs:678`).

Add the close method next to `close_palette` (`mod.rs:899`):

```rust
    /// Close whatever modal is open and hand focus back to the shell root
    /// — the modal-side twin of [`close_palette`](Self::close_palette),
    /// added by the filter-first dialog UX because a dialog's filter
    /// field may currently hold focus and nothing else would give it
    /// back. The one standard door for closing a modal: the escape arm in
    /// `handle_key_down`, and `dialog::render_modal`'s close-button and
    /// backdrop listeners, all go through this rather than setting
    /// `self.modal = None` directly.
    ///
    /// Also clears both dialogs' state. That is not tidiness: the shared
    /// `dialog_input` subscription routes by "whichever state is `Some`",
    /// so a stale `settings` left behind by an earlier open would
    /// swallow the *keybinding* dialog's queries.
    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        self.settings = None;
        self.keybindings = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
```

Replace the escape arm at `mod.rs:1343-1346`:

```rust
                if event.keystroke.key == "escape" {
                    self.close_modal(window, cx);
                }
```

- [ ] **Step 7: Focus the filter on open, and render it**

In `crates/geode-shell/src/shell/dialog.rs`, give `open_shell_dialog_with_key` a trailing `focus_filter: bool` parameter, documented as "focus `ShellView::dialog_input` on open — every list dialog passes `true`; `open_shell_dialog` passes `false`", and at the end of its body:

```rust
    if focus_filter {
        view.dialog_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let handle = view.dialog_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }
```

`set_value` does not emit `InputEvent::Change` (verified in the pinned checkout — see `toggle_palette`'s own comment at `mod.rs:849`), so this reset never reaches the subscription; each dialog's fresh state already starts with an empty query.

Update `open_shell_dialog` to pass `false`, and both dialogs' `open` to pass `true`.

Replace both `view.modal = None;` listeners in `render_modal` (`dialog.rs:322` and `dialog.rs:371`) with `view.close_modal(window, cx);`, changing those closures' `_window` bindings to `window`.

Add the shared render helper to `dialog.rs`:

```rust
/// The filter row every list dialog wears at the top: the shared
/// `Input`, chrome stripped (`appearance(false)`) with a bottom border
/// standing in for it — the palette's own `input_row` idiom
/// (`palette::render`), so the three filtering surfaces look alike.
///
/// `frozen` renders a muted, static copy of the query *instead of* the
/// live input: the keybinding dialog passes `Some(query)` while it is
/// listening for a binding, when the input is blurred and a caret would
/// be a lie about where keystrokes are going.
pub fn filter_row(input: &Entity<InputState>, frozen: Option<&str>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let row = div().w_full().border_b_1().border_color(theme.border);
    match frozen {
        Some(query) => row
            .py_1()
            .text_color(theme.muted_foreground)
            .child(query.to_string())
            .into_any_element(),
        None => row
            .child(Input::new(input).appearance(false).w_full())
            .into_any_element(),
    }
}
```

- [ ] **Step 8: Rewrite the keybindings `build`**

In `keybindings_view::build`:

- replace the `fzf_session` / `filter_matches` block with `let visible = visible_rows(state, &rows);`
- size the list from `visible.len()` (unchanged arithmetic, new source)
- iterate `for (position, m) in visible.iter().enumerate()`, using `let row = &rows[m.row];`, `let is_selected = position == state.selected;`, and `debug_selector(move || format!("keybindings-row-{}", m.row))` — the selector keeps naming the row's index in the FULL list, so existing tests that address rows by identity still work
- switch highlighting from substring to fuzzy indices. First widen the palette's pure run-builder — change `fn highlight_runs` (`palette.rs:176`) to `pub(crate) fn highlight_runs`; it already skips out-of-range indices, which is exactly what the title/category split below needs. Then replace `keybindings_view::highlighted_text` (`keybindings_view.rs:668`) with:

```rust
/// Paint `text` with the fuzzy-match `indices` (char offsets into
/// `text`) highlighted — the index-based twin of the palette's own
/// `highlighted_title`, using the same pure
/// [`palette::highlight_runs`] char-index → merged-byte-range
/// conversion. Replaced this helper's old substring-query signature
/// when the dialogs moved from `/` find to fuzzy filtering: a
/// subsequence match has no single contiguous range to hand
/// `match_range`.
pub(crate) fn highlighted_text(text: &str, indices: &[usize], primary: Hsla) -> AnyElement {
    let runs = palette::highlight_runs(text, indices);
    if runs.is_empty() {
        return div().child(text.to_string()).into_any_element();
    }
    let style = HighlightStyle {
        color: Some(primary),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    StyledText::new(text.to_string())
        .with_highlights(runs.into_iter().map(|r| (r, style)))
        .into_any_element()
}
```

  At each call site, split the ranked indices between the two label lines — they are offsets into `searchable_text(row)`, which is `"{title} {category}"`:

```rust
        let title_len = row.title.chars().count();
        let title_ix: Vec<usize> = m.indices.iter().copied().filter(|&i| i < title_len).collect();
        let cat_ix: Vec<usize> = m
            .indices
            .iter()
            .filter(|&&i| i > title_len)
            .map(|&i| i - title_len - 1)
            .collect();
```

  then pass `&title_ix` / `&cat_ix` to `highlighted_text`. Index `title_len` itself is the separating space and belongs to neither.
- prepend the filter row: `.child(dialog::filter_row(&shell.dialog_input, state.listening.as_ref().map(|_| state.query.as_str()), cx))` above the list
- paint "no matches" whenever `visible.is_empty()` (no longer gated on a session being active)
- replace the footer hint rows with the new vocabulary:

```rust
    let hint_line: AnyElement = if state.listening.is_some() {
        h_flex()
            .gap_1()
            .items_center()
            .flex_wrap()
            .children(vec![
                sep("Listening — type keys,"),
                chip("enter"),
                sep("to save,"),
                chip("escape"),
                sep("to cancel"),
            ])
            .into_any_element()
    } else {
        v_flex()
            .gap_0p5()
            .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
                sep("type to filter ·"),
                chip("up"),
                chip("down"),
                sep("move ·"),
                chip("ctrl+d"),
                chip("ctrl+u"),
                sep("±5 ·"),
                chip("ctrl+f"),
                chip("ctrl+b"),
                sep("±10"),
            ]))
            .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
                chip("enter"),
                sep("rebind the selected row ·"),
                chip("escape"),
                sep("close"),
            ]))
            .into_any_element()
    };
```

Rewrite the module doc's opening paragraph and its "Find" material to describe the filter-first model, citing the spec path. Keep the "What this dialog does NOT do" and "Architecture: two-part state" sections, updating the latter to mention `dialog_input` living on `ShellView` for the same reason `keybindings_scroll` does.

- [ ] **Step 9: Write the `#[gpui::test]` wiring tests**

In `crates/geode-shell/src/shell/mod.rs`'s test module, add two helpers and six tests. The helpers factor out the window/draw/downcast/dispatch preamble the existing dialog tests in this module already repeat inline (see `open_shell_dialog_closes_an_open_palette` for the original).

**Typing must go through `cx.simulate_input(..)`, not `cx.simulate_keystrokes(..)`.** `simulate_input` dispatches one keystroke per character through the full path — action dispatch *and* the text-input phase — which is what actually lands text in a focused `Input`; the palette's own caret tests use it that way (`mod.rs:5778`). Use `simulate_keystrokes` only for named/chorded keys.

```rust
    /// Open a real window with a real `ShellView`, draw a frame, dispatch
    /// `action`, draw again — the preamble every dialog test here needs.
    fn dialog_test_shell(
        cx: &mut gpui::TestAppContext,
        action: &str,
    ) -> (Entity<ShellView>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let root = window.root(&mut vcx).unwrap();
        let shell = root.read_with(&vcx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });
        vcx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId(action.to_string()), window, cx);
            });
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (shell, vcx)
    }

    /// Does the shared dialog filter currently hold focus?
    fn filter_is_focused(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> bool {
        cx.update(|window, cx| {
            shell
                .read(cx)
                .dialog_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        })
    }

    /// Opening the dialog focuses the shared filter, so the first
    /// character typed filters instead of falling on the floor.
    #[gpui::test]
    fn opening_the_keybindings_dialog_focuses_the_filter(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        assert!(
            shell.read_with(&cx, |shell, _| shell.keybindings.is_some()),
            "sanity: keybindings::open should have opened the dialog"
        );
        assert!(
            filter_is_focused(&shell, &mut cx),
            "the filter must own focus the moment the dialog opens"
        );
    }

    /// The retired vim motion is now plain text: `j` types a `j` and
    /// leaves the selection where it was.
    #[gpui::test]
    fn typing_j_filters_rather_than_moving_the_selection(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_input("j");
        let (query, selected) = shell.read_with(&cx, |shell, _| {
            let state = shell.keybindings.as_ref().unwrap();
            (state.query.clone(), state.selected)
        });
        assert_eq!(query, "j", "j must reach the filter as text");
        assert_eq!(selected, 0, "j must not move the selection any more");
    }

    /// Arrow and ctrl motions still move the selection, and do it without
    /// disturbing the filter's focus or its text.
    #[gpui::test]
    fn arrows_and_ctrl_motions_move_the_selection(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_keystrokes("down down");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
            2,
            "two downs should land on the third row"
        );
        cx.simulate_keystrokes("ctrl-u");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
            0,
            "ctrl+u steps back 5, clamped at the top of the list"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .query
                .is_empty()),
            "navigation must not put anything in the filter"
        );
        assert!(
            filter_is_focused(&shell, &mut cx),
            "navigation must not steal focus from the filter"
        );
    }

    /// Enter blurs the filter so rebind capture sees raw keys: the letter
    /// lands in the pending binding, NOT in the query.
    #[gpui::test]
    fn enter_starts_listening_and_a_letter_is_captured_not_typed(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_keystrokes("enter");
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .is_some()),
            "enter should start listening on the selected row"
        );
        assert!(
            !filter_is_focused(&shell, &mut cx),
            "listening must blur the filter, or the capture can never see a letter"
        );

        cx.simulate_input("j");
        let (pending, query) = shell.read_with(&cx, |shell, _| {
            let state = shell.keybindings.as_ref().unwrap();
            (state.listening.clone(), state.query.clone())
        });
        assert_eq!(
            pending.as_deref().map(<[_]>::len),
            Some(1),
            "the letter must be captured as the new binding"
        );
        assert!(query.is_empty(), "and must NOT have been typed into the filter");
    }

    /// Escape cancels the capture, refocuses the filter, and leaves both
    /// the query and the dialog itself alone.
    #[gpui::test]
    fn escape_cancels_a_capture_without_closing_the_dialog(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_input("f");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("j");
        cx.simulate_keystrokes("escape");

        let (listening, query, open) = shell.read_with(&cx, |shell, _| {
            let state = shell.keybindings.as_ref().unwrap();
            (state.listening.is_some(), state.query.clone(), shell.modal.is_some())
        });
        assert!(!listening, "escape should cancel the capture");
        assert!(open, "and must not also close the dialog behind it");
        assert_eq!(query, "f", "the filter text survives a cancelled capture");
        assert!(
            filter_is_focused(&shell, &mut cx),
            "cancelling hands focus back to the filter"
        );
    }

    /// Escape from the resting state closes the dialog and returns focus
    /// to the shell root, so shell chords work again immediately.
    #[gpui::test]
    fn escape_closes_the_dialog_and_restores_shell_focus(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_keystrokes("escape");
        shell.read_with(&cx, |shell, _| {
            assert!(shell.modal.is_none(), "escape should close the modal");
            assert!(
                shell.keybindings.is_none(),
                "close_modal must clear the dialog state too, or the shared \
                 input's subscription can route into a stale dialog"
            );
        });
        assert!(
            !filter_is_focused(&shell, &mut cx),
            "focus must leave the filter on close"
        );
        assert!(
            cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
            "and land back on the shell root"
        );
    }
```

If `dialog_test_shell`'s `test_services()` fixture leaves fewer than three rows in the keybindings dialog, `arrows_and_ctrl_motions_move_the_selection`'s expected `2` is wrong — check `derive_rows(&services.registry, &services.keymap).len()` and adjust the expectation to the real clamped value rather than loosening the assertion.

- [ ] **Step 10: Run everything**

Run: `cargo fmt --check && cargo clippy -p geode-shell --all-targets -- -D warnings && cargo test -p geode-shell`
Expected: all clean; the six new `#[gpui::test]`s pass.

Note: `cargo test --workspace` may surface `shell::mod` tests that drove the old dialog with `j`/`k`. Update them to the new vocabulary — do not delete a test that is asserting something real (e.g. "a chord does not leak through the modal"); re-express it with a key the new model actually uses.

- [ ] **Step 11: Commit**

```bash
git add crates/geode-shell/src/shell/
git commit -m "$(cat <<'EOF'
feat: filter-first keybindings dialog

Replaces the dialog's vim motions and `/` find with an always-focused
fuzzy filter and arrow-key navigation, and adds the ShellView plumbing
both list dialogs will share: one `dialog_input` entity, `close_modal`
(which also restores focus and clears dialog state so the shared input's
subscription cannot route into a stale dialog), and a focus-on-open flag
on the modal door.

Rebind capture survives by blurring the filter while listening — a
focused single-line Input consumes bare letters as text before any raw
key listener sees them, so the capture would otherwise be impossible.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013AGrUwbHcA9ef7Mz2gQNrK
EOF
)"
```

---

### Task 3: The settings dialog

Same model, with `tab`/`shift+tab` value stepping in place of rebind capture. All the plumbing already exists from Task 2.

**Files:**
- Modify: `crates/geode-shell/src/shell/settings_view.rs` — state, `handle_key`, `on_row_clicked`, `build`, module doc, tests
- Modify: `crates/geode-shell/src/shell/mod.rs` — add the `settings` arm to the `dialog_input` subscription
- Test: `crates/geode-shell/src/shell/settings_view.rs` (inline pure tests) and `crates/geode-shell/src/shell/mod.rs` (`#[gpui::test]`s)

**Interfaces:**
- Consumes: everything Task 2 produced, plus `settings_view::{step, StepDirection, apply_setting, rows_for, searchable_text, SettingRow, SettingId}` (unchanged).
- Produces: `SettingsState { selected, query }` with `set_query`; `settings_view::visible_rows(state, rows) -> Vec<Ranked>`; `settings_view::filtered_position(visible, rows, clicked: SettingId) -> Option<usize>`.

- [ ] **Step 1: Write the failing pure tests**

In `settings_view.rs`'s `mod tests`, delete the vim/fzf find tests (`vim_find_jumps_...`, `fzf_pick_...`, `fzf_enter_on_zero_matches_...`, `a_click_cancels_an_active_find_session`) and add:

```rust
    #[test]
    fn an_empty_query_shows_every_settings_row() {
        let rows = rows();
        let state = SettingsState::new();
        assert_eq!(
            visible_rows(&state, &rows)
                .iter()
                .map(|m| m.row)
                .collect::<Vec<_>>(),
            (0..rows.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_query_narrows_to_the_matching_rows() {
        let rows = rows();
        let mut state = SettingsState::new();
        state.set_query("font".to_string());
        let visible = visible_rows(&state, &rows);
        assert_eq!(visible.len(), 1);
        assert_eq!(rows[visible[0].row].title, "Font size");
    }

    #[test]
    fn setting_a_query_resets_the_selection_to_the_top_match() {
        let mut state = SettingsState::new();
        state.selected = 2;
        state.set_query("dark".to_string());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn a_click_on_a_different_row_selects_it_without_stepping() {
        let mut state = SettingsState::new();
        assert!(!click_selects_or_steps(&mut state, 2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn a_click_on_the_selected_row_asks_for_a_forward_cycle() {
        let mut state = SettingsState::new();
        state.selected = 1;
        assert!(click_selects_or_steps(&mut state, 1));
    }
```

Keep every existing `step` / `rows_come_in_display_order` / `searchable_text` test as-is — the row model itself is unchanged, Find style row included (spec §8).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell settings_view`
Expected: FAIL — `visible_rows`/`set_query` missing.

- [ ] **Step 3: Rewrite the state and add the helper**

```rust
/// Persistent state for one open settings dialog session — the exact
/// shape of `KeybindingsState` minus the rebind-capture field (there is
/// nothing to "listen" for here; editing is stepping, which is
/// instantaneous). Fresh on every open ([`open`]), holds no gpui types,
/// so every transition is unit-testable without a window.
#[derive(Debug, Default)]
pub struct SettingsState {
    /// Index into the **filtered** row list ([`visible_rows`]), not the
    /// full one — the same convention `KeybindingsState::selected` uses.
    pub selected: usize,
    /// The filter query, mirrored from `ShellView::dialog_input`.
    pub query: String,
}

impl SettingsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the query and reset the selection to the top match — the
    /// pure half of the `InputEvent::Change` subscription in
    /// `ShellView::new`.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
    }
}

/// The rows this dialog currently shows, ranked — the settings twin of
/// `keybindings_view::visible_rows`, over [`searchable_text`] (title and
/// category; the value labels deliberately do not participate — see that
/// function's doc comment).
pub fn visible_rows(state: &SettingsState, rows: &[SettingRow]) -> Vec<Ranked> {
    let texts: Vec<String> = rows.iter().map(searchable_text).collect();
    dialogfilter::rank(&texts, &state.query)
}

/// Where the row for `clicked` currently sits in the *filtered* list, or
/// `None` if the filter is hiding it — the settings twin of
/// `keybindings_view::filtered_position`, keyed by [`SettingId`] rather
/// than `ActionId` for the same identity-not-position reason.
pub fn filtered_position(
    visible: &[Ranked],
    rows: &[SettingRow],
    clicked: SettingId,
) -> Option<usize> {
    visible
        .iter()
        .position(|m| rows.get(m.row).is_some_and(|r| r.id == clicked))
}
```

and simplify the click helper:

```rust
pub fn click_selects_or_steps(state: &mut SettingsState, clicked_ix: usize) -> bool {
    if state.selected == clicked_ix {
        true
    } else {
        state.selected = clicked_ix;
        false
    }
}
```

Update imports: drop `vimfind::{self, FindDirection, FindStyle, VimFind, filter_matches}` and `vimnav::{self, NavResult, VimListNav}`; add `use crate::dialogfilter::{self, Ranked};` and `use crate::vimnav;`. **Keep** `use crate::vimfind::FindStyle;` if `rows_for`/`apply_setting` still name it for the Find style row — they do (spec §8), so keep exactly that import and no more.

- [ ] **Step 4: Rewrite `handle_key`**

```rust
/// The [`dialog::ModalKeyHandler`] for this dialog, mirroring
/// `keybindings_view::handle_key` with value stepping in place of rebind
/// capture:
///
/// 1. [`dialogfilter::nav_command`] motions move the selection within the
///    *filtered* list;
/// 2. `tab` / `shift+tab` step the selected row's value forward / back,
///    wrapping, applied immediately through [`apply_setting`]. `tab` is
///    reachable here only because gpui-component's `Input` gates its own
///    indent listeners on being multi-line (spec §2b) — `left`/`right`,
///    the old stepping keys, are swallowed unconditionally and can never
///    reach this handler again;
/// 3. everything else — bare `enter` and bare `escape` included — returns
///    `false`. `enter` is deliberately inert and reserved: settings apply
///    the instant they are stepped, so there is nothing to confirm.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = rows_for(shell);
    let Some(state) = shell.settings.as_mut() else {
        return false;
    };
    let visible = visible_rows(state, &rows);

    if let Some(cmd) = dialogfilter::nav_command(ks) {
        state.selected = vimnav::apply(state.selected, visible.len(), cmd);
        let selected = state.selected;
        shell.settings_scroll.scroll_to_item(selected);
        cx.notify();
        return true;
    }

    let dir = match (ks.mods, ks.key.as_str()) {
        (Modifiers::NONE, "tab") => StepDirection::Right,
        (m, "tab") if m == (Modifiers { shift: true, ..Modifiers::NONE }) => StepDirection::Left,
        _ => return false,
    };
    let selected = state.selected;
    let Some(row) = visible.get(selected).and_then(|m| rows.get(m.row)) else {
        return false;
    };
    let (id, new_ix) = (row.id, step(row.values.len(), row.current, dir));
    apply_setting(shell, id, new_ix, cx);
    cx.notify();
    true
}
```

Update `on_row_clicked` to resolve the clicked `SettingId` through the filtered list:

```rust
fn on_row_clicked(shell: &mut ShellView, clicked: SettingId, cx: &mut Context<ShellView>) {
    let rows = rows_for(shell);
    let Some(state) = shell.settings.as_mut() else {
        return;
    };
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    let cycle = click_selects_or_steps(state, ix);
    let selected = state.selected;
    shell.settings_scroll.scroll_to_item(selected);
    if cycle && let Some(row) = visible.get(ix).and_then(|m| rows.get(m.row)) {
        let new_ix = step(row.values.len(), row.current, StepDirection::Right);
        apply_setting(shell, row.id, new_ix, cx);
    }
    cx.notify();
}
```

- [ ] **Step 5: Rewrite `build` and add the subscription arm**

In `settings_view::build`, make the same five changes Task 2 made to the keybindings build: `visible_rows` instead of the fzf block; iterate `(position, m)`; index-based highlighting; `dialog::filter_row(&shell.dialog_input, None, cx)` prepended (settings never freezes it — there is no listening state here); `visible.is_empty()` paints "no matches" unconditionally. Replace the three hint rows with:

```rust
    let hint_line: AnyElement = v_flex()
        .gap_0p5()
        .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
            sep("type to filter ·"),
            chip("up"),
            chip("down"),
            sep("move ·"),
            chip("ctrl+d"),
            chip("ctrl+u"),
            sep("±5 ·"),
            chip("ctrl+f"),
            chip("ctrl+b"),
            sep("±10"),
        ]))
        .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
            chip("tab"),
            sep("next value ·"),
            chip("shift+tab"),
            sep("previous value ·"),
            chip("escape"),
            sep("close"),
        ]))
        .into_any_element();
```

Keep the two muted inert footer lines below it exactly as they are.

In `crates/geode-shell/src/shell/mod.rs`, extend the `dialog_input` subscription from Task 2:

```rust
            if let Some(state) = view.keybindings.as_mut() {
                state.set_query(query);
                view.keybindings_scroll.scroll_to_item(0);
            } else if let Some(state) = view.settings.as_mut() {
                state.set_query(query);
                view.settings_scroll.scroll_to_item(0);
            }
```

Rewrite the module doc's "Find (`/`), both styles" section into a "Filter" section describing the new model, and update the "row model" paragraph's stepping sentence from `h`/`l`/`left`/`right`/`enter`/`space` to `tab`/`shift+tab` plus click. Keep the Find style row's description — it is still a row, it just no longer steers these dialogs (say so, citing spec §8).

- [ ] **Step 6: Write the `#[gpui::test]` wiring tests**

Reuse Task 2's `dialog_test_shell` and `filter_is_focused` helpers verbatim — they take the action id, so they already serve both dialogs.

```rust
    #[gpui::test]
    fn opening_the_settings_dialog_focuses_the_filter(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        assert!(shell.read_with(&cx, |shell, _| shell.settings.is_some()));
        assert!(
            filter_is_focused(&shell, &mut cx),
            "the filter must own focus the moment the dialog opens"
        );
    }

    /// Typing filters; the old `h`/`l` stepping keys are now just text,
    /// and must not step anything on their way into the query.
    #[gpui::test]
    fn typing_filters_the_settings_rows(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());
        cx.simulate_input("dark");
        let (query, selected) = shell.read_with(&cx, |shell, _| {
            let state = shell.settings.as_ref().unwrap();
            (state.query.clone(), state.selected)
        });
        assert_eq!(query, "dark");
        assert_eq!(selected, 0, "a query selects the top match");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark()),
            before,
            "typing must never apply a setting — the old h/l/enter stepping \
             keys are plain text now"
        );
    }

    /// tab steps the selected row's value forward and shift+tab back,
    /// through the same apply path a click takes.
    #[gpui::test]
    fn tab_and_shift_tab_step_the_selected_value(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        // Narrow to the Dark mode row so the selection is unambiguous and
        // the applied effect is a single observable boolean.
        cx.simulate_input("dark");
        let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());

        cx.simulate_keystrokes("tab");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark()),
            !before,
            "tab should step the selected row's value forward"
        );

        cx.simulate_keystrokes("shift-tab");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark()),
            before,
            "shift+tab should step it back"
        );
    }

    /// Enter is inert and reserved here (spec §3): it must not step a
    /// value, and must not close the dialog either.
    #[gpui::test]
    fn enter_does_nothing_in_the_settings_dialog(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        cx.simulate_input("dark");
        let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());

        cx.simulate_keystrokes("enter");
        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.theme.active_mode().is_dark(),
                before,
                "enter must not step the value"
            );
            assert!(shell.modal.is_some(), "and must not close the dialog");
        });
    }

    #[gpui::test]
    fn escape_closes_the_settings_dialog_and_restores_shell_focus(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        cx.simulate_keystrokes("escape");
        shell.read_with(&cx, |shell, _| {
            assert!(shell.modal.is_none());
            assert!(shell.settings.is_none(), "close_modal clears dialog state");
        });
        assert!(
            cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
            "focus lands back on the shell root"
        );
    }
```

If `"dark"` does not narrow to exactly the Dark mode row against the real `rows_for` output, pick whatever query does — do not weaken the stepping assertion to accommodate a fuzzy near-miss.

- [ ] **Step 7: Run everything**

Run: `cargo fmt --check && cargo clippy -p geode-shell --all-targets -- -D warnings && cargo test -p geode-shell`
Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-shell/src/shell/
git commit -m "$(cat <<'EOF'
feat: filter-first settings dialog

Same model as the keybindings dialog, with tab/shift+tab stepping the
selected row's value — left/right are swallowed unconditionally by a
focused single-line Input and can never reach the dialog again. Enter is
inert and reserved: settings apply the instant they are stepped.

The `[ui] find_style` setting and its row are untouched; the dialogs
simply stop reading it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013AGrUwbHcA9ef7Mz2gQNrK
EOF
)"
```

---

### Task 4: Reclaim `ctrl+f` on Windows and Linux

Without this, `ctrl+f` paging works on macOS and silently does nothing everywhere else (spec §2c, §7).

**Files:**
- Modify: `crates/geode-app/src/main.rs` (wherever `gpui_component::init(cx)` is called — find it; CLAUDE.md notes it must run before any component use)

**Interfaces:**
- Consumes: `gpui::{KeyBinding, NoAction}`.
- Produces: nothing callable — a window-wide keybinding registration.

- [ ] **Step 1: Add the binding**

Immediately after `gpui_component::init(cx)`:

```rust
    // gpui-component binds ctrl-f to its editor `Search` action in the
    // "Input" key context on non-macOS (crates/base/src/input/base/state.rs
    // in the pinned rev), and that handler returns without `cx.propagate()`
    // when the input isn't searchable — so ctrl+f, which the list dialogs
    // use for "page down", would work on macOS and die silently on Windows
    // and Linux.
    //
    // `NoAction` is gpui's own mechanism for this (gpui/src/keymap.rs's
    // `bindings_for_input`): it suppresses every equal-or-weaker binding it
    // outranks, so no action is dispatched at all and the raw KeyDownEvent
    // reaches our key listeners. A pass-through action of our own would NOT
    // work — gpui dispatches every matched binding in sequence
    // (gpui/src/window.rs's `dispatch_key_event`), so calling
    // `cx.propagate()` would simply hand the key on to `Search`.
    //
    // Later registrations outrank earlier ones, so this must come after
    // `gpui_component::init`. Registered unconditionally rather than behind
    // a `cfg`, so both platforms run one code path.
    cx.bind_keys([KeyBinding::new("ctrl-f", gpui::NoAction, Some("Input"))]);
```

If `NoAction` is not re-exported at `gpui::NoAction`, import it from `gpui::actions`/`gpui::action` as the pinned rev exposes it (`crates/gpui/src/action.rs:4` re-exports `NoAction` from `no_action`); resolve the exact path by compiling, not by guessing.

- [ ] **Step 2: Verify it compiles on this platform**

Run: `cargo build -p geode-app`
Expected: builds clean.

- [ ] **Step 3: Verify the whole workspace**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo bench --workspace --no-run`
Expected: all four clean.

There is no automated test for this binding — the behaviour it fixes only manifests on a non-macOS build with a real focused input, which this environment cannot exercise. Say so plainly in the task report rather than claiming it verified.

- [ ] **Step 4: Commit**

```bash
git add crates/geode-app/src/main.rs
git commit -m "$(cat <<'EOF'
fix: reclaim ctrl+f from gpui-component's Input on Windows and Linux

gpui-component binds ctrl-f to its editor Search action in the "Input"
context off macOS, and swallows the key when the input isn't searchable
— so the dialogs' ctrl+f paging would work on macOS only. A NoAction
binding suppresses it and lets the raw event through; a pass-through
action would not, since gpui dispatches every matched binding in turn.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013AGrUwbHcA9ef7Mz2gQNrK
EOF
)"
```

---

## Notes for the reviewer

Three things in this plan are worth checking against the pinned checkouts rather than taken on trust, because the whole key vocabulary rests on them (spec §2 has the citations):

1. `left`/`right`/`home`/`end` really are unconditional in `Input`'s element, and `tab`/`pageup`/`pagedown`/`up`/`down` really are gated on `is_multi_line`.
2. A `NoAction` binding in the `"Input"` context really does leave `match_result.bindings` empty rather than merely reordering.
3. Focusing `dialog_input`'s handle before the element has ever rendered works — the palette already depends on this (`toggle_palette` focuses before the palette's first frame), so it is precedent, not a new bet.

The one behaviour with no automated coverage is Task 4's binding; it needs a Windows or Linux run to confirm.
