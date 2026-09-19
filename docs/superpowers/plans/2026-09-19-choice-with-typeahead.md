# Choice with Typeahead (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One shared "choose one value from a list, with typeahead" core (`geode_shell::choice::ChoiceList`) used by the object dialog's `Choice` rows, every settings-dialog row, and the market-data panel's underlying picker.

**Architecture:** A pure core in `geode-shell` (no gpui, the `listfilter`/`vimnav` mould) owns ranking, the highlight, `complete` and `pick`; each surface keeps its own state and painting. In the two modal dialogs the shared `Input` opens in the filter row's place (the chain field's existing form) and the row list is replaced by the ranked options, painted by one shared `dialog::choice_rows`. The underlying picker's `PickerRows` is re-implemented over the core with its behaviour unchanged.

**Tech Stack:** Rust, gpui / gpui-component 0.6.2 (pinned), `geode_shell::listfilter::rank`, `geode_shell::vimnav`, `TestAppContext` window tests, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-dividend-schedule-and-choice-design.md` §3 (and §2 rulings 4–5, §7 amendments).

## Global Constraints

- Every new pure module has no `gpui` import (spec §3.1: "pure, no gpui, in `listfilter`'s and `vimnav`'s mould").
- The painted cap is 12 rows (`choice::DEFAULT_CAP`), the picker's existing `PICKER_ROWS`.
- `enter` in a choice field picks the **highlighted** option, never the typed text (spec §3.2).
- A transition site only mutates `mode`/`query`/state; `dialog::sync_dialog_text` is the only thing that focuses or writes the shared `Input` (CLAUDE.md, "Dialogs now have two interaction modes").
- `InputState::set_value` emits no `Change` event: any commit path re-reads the field's live text before acting (CLAUDE.md, three traps).
- Read-only domains refuse through `Domain::writable(&stage)` at the existing `i` site; nothing new bypasses it.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `zsh scripts/mutation-check.sh --anchors-only` must all pass before merge. Both macOS and Windows build in CI.
- Commit after every task with the attribution lines from the session's system reminder.

---

### Task 1: `geode_shell::choice` — the pure core

**Files:**
- Create: `crates/geode-shell/src/choice.rs`
- Modify: `crates/geode-shell/src/lib.rs` (add `pub mod choice;` beside `pub mod listfilter;`)

**Interfaces:**
- Consumes: `crate::listfilter::{rank, Ranked, nav_command}`, `crate::vimnav::{apply, apply_clamped, NavCommand}`, `crate::keymap::{Keystroke, Modifiers}`.
- Produces (used by Tasks 2–5):
  ```rust
  pub const DEFAULT_CAP: usize = 12;
  pub struct ChoiceList { /* private */ }
  impl ChoiceList {
      pub fn new(options: Vec<String>, cap: usize) -> Self;
      pub fn options(&self) -> &[String];
      pub fn query(&self) -> &str;
      pub fn ranked(&self) -> &[Ranked];          // whole ranked list, declared-index rows
      pub fn painted(&self) -> &[Ranked];         // first `cap` of `ranked`
      pub fn painted_len(&self) -> usize;
      pub fn highlighted(&self) -> usize;         // index into `painted()`
      pub fn highlighted_option(&self) -> Option<usize>;  // declared index
      pub fn highlighted_text(&self) -> Option<&str>;
      pub fn set_query(&mut self, query: &str) -> bool;   // false = unchanged, nothing moved
      pub fn replace_options(&mut self, options: Vec<String>);
      pub fn place(&mut self, value: Option<&str>);
      pub fn nav(&mut self, cmd: NavCommand);             // vimnav::apply (bare ±1 wraps)
      pub fn nav_clamped(&mut self, cmd: NavCommand);     // vimnav::apply_clamped
      pub fn set_highlighted(&mut self, row: usize) -> bool; // a click; false past painted_len
      pub fn complete(&mut self) -> bool;         // query := highlighted text
      pub fn pick(&self) -> Option<usize>;        // = highlighted_option
  }
  pub enum ChoiceKey { Cancel, Pick, Complete, Nav(NavCommand) }
  pub fn route(ks: &Keystroke) -> Option<ChoiceKey>;
  ```

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-shell/src/choice.rs` with only the tests module and a module doc, so the file compiles once the types exist:

```rust
//! Shared pure core for "choose one value from a list, with typeahead"
//! (spec 2026-09-19 §3.1): the object dialog's `Choice` rows, every
//! settings-dialog row, and the market-data panel's underlying picker
//! all rank, highlight, complete and pick through this one type. No
//! `gpui` here, in the mould of [`crate::listfilter`] and
//! [`crate::vimnav`] — feed it plain strings and shell-native
//! [`Keystroke`]s, unit-test it without a window.
//!
//! Identity is the OPTION TEXT, never a positional index: every re-rank
//! (`set_query`, `replace_options`) captures the highlighted text first
//! and re-finds it afterwards (the underlying picker's own rule, review
//! fix round 2 of the header work), so typing can narrow the list
//! without the highlight silently landing on a different option.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keystroke, Modifiers};

    fn opts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn key(k: &str) -> Keystroke {
        Keystroke { mods: Modifiers::NONE, key: k.to_string() }
    }

    #[test]
    fn an_empty_query_lists_every_option_in_declared_order_capped() {
        let list = ChoiceList::new(opts(&["a", "b", "c"]), 2);
        assert_eq!(list.ranked().iter().map(|r| r.row).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(list.painted_len(), 2);
        assert_eq!(list.highlighted(), 0);
        assert_eq!(list.highlighted_option(), Some(0));
    }

    #[test]
    fn set_query_narrows_and_keeps_the_highlight_by_text() {
        let mut list = ChoiceList::new(opts(&["Gruvbox Dark", "Gruvbox Light", "Nord"]), 12);
        list.place(Some("Nord"));
        assert_eq!(list.highlighted_text(), Some("Nord"));
        assert!(list.set_query("gruv"));
        // Nord no longer matches; the highlight falls back to row 0.
        assert_eq!(list.highlighted_text(), Some("Gruvbox Dark"));
        list.nav(NavCommand::Move(1));
        assert_eq!(list.highlighted_text(), Some("Gruvbox Light"));
        assert!(list.set_query("gruv l"));
        assert_eq!(list.highlighted_text(), Some("Gruvbox Light"), "kept by text across a re-rank");
        assert!(!list.set_query("gruv l"), "an unchanged query moves nothing");
    }

    #[test]
    fn pick_answers_the_declared_index_not_the_ranked_row() {
        let mut list = ChoiceList::new(opts(&["danger", "accent", "chart.1"]), 12);
        list.set_query("acc");
        assert_eq!(list.highlighted(), 0, "ranked row 0");
        assert_eq!(list.pick(), Some(1), "declared index of 'accent'");
    }

    #[test]
    fn nav_wraps_a_bare_step_and_nav_clamped_does_not() {
        let mut list = ChoiceList::new(opts(&["a", "b", "c"]), 12);
        list.nav(NavCommand::Move(-1));
        assert_eq!(list.highlighted(), 2, "a bare -1 wraps (§20.5)");
        list.nav_clamped(NavCommand::Move(5));
        assert_eq!(list.highlighted(), 2, "clamped at the last painted row");
        list.nav_clamped(NavCommand::Move(-5));
        assert_eq!(list.highlighted(), 0);
    }

    #[test]
    fn the_highlight_never_leaves_the_painted_range() {
        let mut list = ChoiceList::new((0..20).map(|i| format!("o{i}")).collect(), 12);
        list.nav_clamped(NavCommand::Move(100));
        assert_eq!(list.highlighted(), 11);
        assert!(!list.set_highlighted(12), "a click past the painted rows is refused");
        assert!(list.set_highlighted(3));
        assert_eq!(list.highlighted(), 3);
        list.nav(NavCommand::Bottom);
        assert_eq!(list.highlighted(), 11, "Bottom is the last PAINTED row");
    }

    #[test]
    fn complete_copies_the_highlighted_text_into_the_query_and_keeps_it_highlighted() {
        let mut list = ChoiceList::new(opts(&["estimated", "declared", "paid"]), 12);
        list.set_query("de");
        assert!(list.complete());
        assert_eq!(list.query(), "declared");
        assert_eq!(list.highlighted_text(), Some("declared"));
        let mut empty = ChoiceList::new(Vec::new(), 12);
        assert!(!empty.complete(), "nothing highlighted, nothing completed");
    }

    #[test]
    fn replace_options_keeps_the_highlight_by_text_and_falls_back_to_zero() {
        let mut list = ChoiceList::new(opts(&["a", "b", "c"]), 12);
        list.place(Some("c"));
        list.replace_options(opts(&["z", "c", "a"]));
        assert_eq!(list.highlighted_text(), Some("c"));
        list.replace_options(opts(&["q"]));
        assert_eq!(list.highlighted(), 0);
        assert_eq!(list.highlighted_text(), Some("q"));
    }

    #[test]
    fn place_on_a_missing_value_lands_on_row_zero() {
        let mut list = ChoiceList::new(opts(&["a", "b"]), 12);
        list.nav(NavCommand::Move(1));
        list.place(Some("nope"));
        assert_eq!(list.highlighted(), 0);
        list.place(None);
        assert_eq!(list.highlighted(), 0);
    }

    #[test]
    fn route_claims_escape_enter_tab_and_the_nav_keys_and_nothing_else() {
        assert!(matches!(route(&key("escape")), Some(ChoiceKey::Cancel)));
        assert!(matches!(route(&key("enter")), Some(ChoiceKey::Pick)));
        assert!(matches!(route(&key("tab")), Some(ChoiceKey::Complete)));
        let shift_tab = Keystroke { mods: Modifiers { shift: true, ..Modifiers::NONE }, key: "tab".into() };
        assert!(matches!(route(&shift_tab), Some(ChoiceKey::Complete)), "tab whatever the modifiers");
        assert!(matches!(route(&key("down")), Some(ChoiceKey::Nav(NavCommand::Move(1)))));
        assert!(matches!(route(&key("up")), Some(ChoiceKey::Nav(NavCommand::Move(-1)))));
        assert!(route(&key("a")).is_none(), "a letter is the field's to type");
        assert!(route(&key("space")).is_none());
        let ctrl_enter = Keystroke { mods: Modifiers::CTRL, key: "enter".into() };
        assert!(route(&ctrl_enter).is_none(), "only a BARE enter picks");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell choice::tests 2>&1 | tail -5`
Expected: compile error — `ChoiceList`, `ChoiceKey`, `route` not found.

- [ ] **Step 3: Implement the core**

Insert above the tests module in `crates/geode-shell/src/choice.rs`:

```rust
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter::{self, Ranked};
use crate::vimnav::{self, NavCommand};

/// How many ranked rows a choice surface PAINTS: the underlying picker's
/// `PICKER_ROWS`, now the one number every choice list shares. A cap
/// rather than a scroll container, because the query narrows the rest
/// and a cap needs no scroll state; the highlight is clamped to it so
/// `enter` can never pick a row the trader cannot see.
pub const DEFAULT_CAP: usize = 12;

/// One list of options, the query it is ranked against, and which
/// painted row is highlighted. See the module doc for the identity rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceList {
    options: Vec<String>,
    query: String,
    ranked: Vec<Ranked>,
    highlighted: usize,
    cap: usize,
}

impl ChoiceList {
    /// Every option ranked in declared order under an empty query, the
    /// highlight on row 0.
    pub fn new(options: Vec<String>, cap: usize) -> Self {
        let ranked = listfilter::rank(&options, "");
        Self { options, query: String::new(), ranked, highlighted: 0, cap }
    }

    pub fn options(&self) -> &[String] { &self.options }
    pub fn query(&self) -> &str { &self.query }
    pub fn ranked(&self) -> &[Ranked] { &self.ranked }

    /// The rows a surface paints — the first `cap` of `ranked`.
    pub fn painted(&self) -> &[Ranked] {
        &self.ranked[..self.painted_len()]
    }

    pub fn painted_len(&self) -> usize {
        self.ranked.len().min(self.cap)
    }

    /// The highlighted PAINTED row.
    pub fn highlighted(&self) -> usize { self.highlighted }

    /// The highlighted option's index in the DECLARED list — what a pick
    /// means. `None` only with nothing ranked.
    pub fn highlighted_option(&self) -> Option<usize> {
        self.painted().get(self.highlighted).map(|r| r.row)
    }

    pub fn highlighted_text(&self) -> Option<&str> {
        self.highlighted_option().map(|i| self.options[i].as_str())
    }

    /// Re-rank against `query`, keeping the highlight by text. `false`
    /// when `query` is what the list was last ranked against — nothing
    /// re-ranks and the highlight stays put, so a defensive re-read of a
    /// field's live text at commit time costs a compare and moves
    /// nothing (`InputState::set_value` emits no `Change` event, so the
    /// commit path cannot trust that every keystroke reached here).
    pub fn set_query(&mut self, query: &str) -> bool {
        if query == self.query {
            return false;
        }
        let keep = self.highlighted_text().map(str::to_string);
        self.query = query.to_string();
        self.place(keep.as_deref());
        true
    }

    /// Swap in a new option list, keeping the highlight by text. The
    /// text is captured BEFORE `options` is overwritten — an index into
    /// the old list means nothing in the new one.
    pub fn replace_options(&mut self, options: Vec<String>) {
        let keep = self.highlighted_text().map(str::to_string);
        self.options = options;
        self.place(keep.as_deref());
    }

    /// Rebuild `ranked` against the current options and query, then put
    /// the highlight on `value`'s row — row 0 when `value` is `None`, not
    /// an option, or ranked past the painted range.
    pub fn place(&mut self, value: Option<&str>) {
        self.ranked = listfilter::rank(&self.options, &self.query);
        let painted = self.painted_len();
        self.highlighted = value
            .and_then(|v| self.options.iter().position(|o| o == v))
            .and_then(|declared| self.ranked.iter().position(|r| r.row == declared))
            .filter(|&row| row < painted)
            .unwrap_or(0);
    }

    /// Move the highlight over the painted rows by [`vimnav::apply`]'s
    /// rule: a bare ±1 wraps, anything larger clamps (§20.5).
    pub fn nav(&mut self, cmd: NavCommand) {
        self.highlighted = vimnav::apply(self.highlighted, self.painted_len(), cmd);
    }

    /// [`Self::nav`] with every step clamped — the underlying picker's
    /// own rule (header spec §7), kept for it.
    pub fn nav_clamped(&mut self, cmd: NavCommand) {
        self.highlighted = vimnav::apply_clamped(self.highlighted, self.painted_len(), cmd);
    }

    /// A click on painted row `row`. Refused (`false`) past the painted
    /// range, which a click cannot reach anyway.
    pub fn set_highlighted(&mut self, row: usize) -> bool {
        if row >= self.painted_len() {
            return false;
        }
        self.highlighted = row;
        true
    }

    /// `tab`: the highlighted option's text becomes the query, and stays
    /// highlighted through the re-rank. `false` with nothing highlighted.
    pub fn complete(&mut self) -> bool {
        let Some(text) = self.highlighted_text().map(str::to_string) else {
            return false;
        };
        self.query = text.clone();
        self.place(Some(&text));
        true
    }

    /// `enter`: the declared index of the highlighted option.
    pub fn pick(&self) -> Option<usize> {
        self.highlighted_option()
    }
}

/// What a keystroke means while a choice field holds the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKey {
    /// `escape`: close with nothing applied.
    Cancel,
    /// A bare `enter`: apply the highlighted option.
    Pick,
    /// `tab`, whatever the modifiers: complete to the highlighted option.
    /// Claimed with `shift` too, so `shift+tab` cannot reach `Root`'s
    /// focus cycling (the reason `dialog::init_reclaimed_keybindings`
    /// exists).
    Complete,
    /// The shared list motions ([`listfilter::nav_command`]).
    Nav(NavCommand),
}

/// The one key table every choice field reads (spec §3.1): the object
/// dialog's, the settings dialog's, and — through its own `up`/`down`
/// arms — the underlying picker's. `None` is "the field's to type".
pub fn route(ks: &Keystroke) -> Option<ChoiceKey> {
    if ks.key == "escape" {
        return Some(ChoiceKey::Cancel);
    }
    if ks.key == "tab" {
        return Some(ChoiceKey::Complete);
    }
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        return Some(ChoiceKey::Pick);
    }
    listfilter::nav_command(ks).map(ChoiceKey::Nav)
}
```

Add to `crates/geode-shell/src/lib.rs`, next to `pub mod listfilter;`: `pub mod choice;`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-shell choice::tests 2>&1 | tail -5`
Expected: `test result: ok. 9 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell/src/choice.rs crates/geode-shell/src/lib.rs
git commit -m "shell: ChoiceList, the shared choose-one-with-typeahead core"
```

---

### Task 2: Object dialog — the draft's choice field (pure)

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`TextEntry`, `Draft`, `visible_rows`, `set_query`, `cancel_text_entry`, `apply_text_entry`, `vocabulary_of`, new methods)
- Modify: `crates/geode-shell/src/shell/objectdialog/groupings.rs` (`begin_chain_entry`, `chain_candidates` — the `completions` field)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (every `entry.completions` / `t.completions` read — mechanical, no behaviour change in this task)
- Test: `crates/geode-shell/src/shell/objectdialog/mod.rs` tests module

**Interfaces:**
- Consumes: `crate::choice::{ChoiceList, DEFAULT_CAP}`, `crate::vimnav::NavCommand`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Completions { None, Chain, Choice }
  pub struct TextEntry { pub row: EditRow, pub completions: Completions }
  // on Draft:
  pub choice: Option<ChoiceList>;            // Some exactly while completions == Choice
  pub fn begin_choice_entry(&mut self) -> Step;
  pub fn choice_entry(&self) -> bool;
  pub fn choice_nav(&mut self, cmd: NavCommand);
  pub fn choice_click(&mut self, row: usize) -> bool;   // set_highlighted + complete
  pub fn complete_choice(&mut self) -> bool;
  pub fn apply_choice(&mut self) -> Step;
  ```

- [ ] **Step 1: Write the failing pure tests**

In `crates/geode-shell/src/shell/objectdialog/mod.rs`'s `mod tests`, beside `a_choice_wraps_backward…` (~line 4284), add:

```rust
    /// §3.2: `i` on a `Choice` row opens the field EMPTY (the current
    /// value is already lit), with the highlight placed on the current
    /// option; `enter` picks the lit row and closes; the cursor stays on
    /// the row throughout.
    #[test]
    fn i_on_a_choice_row_opens_an_empty_field_placed_on_the_current_option() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["none".into(), "danger".into(), "accent".into()],
            selected: 2,
        });
        assert_eq!(draft.begin_choice_entry(), Step::Changed);
        assert!(draft.choice_entry());
        assert_eq!(draft.query, "", "opens empty");
        assert_eq!(draft.choice.as_ref().unwrap().highlighted_text(), Some("accent"));
        assert_eq!(draft.selected_row(), Some(EditRow::Field(0)), "the cursor stays on the row");
        draft.set_query("dan".into());
        assert_eq!(draft.choice.as_ref().unwrap().highlighted_text(), Some("danger"));
        assert_eq!(draft.selected_row(), Some(EditRow::Field(0)), "typing does not move the cursor");
        assert_eq!(draft.apply_choice(), Step::Changed);
        assert!(!draft.choice_entry());
        assert!(draft.choice.is_none());
        assert_eq!(draft.query, "");
        assert!(matches!(&draft.fields[0].kind, FieldKind::Choice { selected: 1, .. }));
        assert_eq!(draft.selected_row(), Some(EditRow::Field(0)));
    }

    #[test]
    fn picking_the_option_already_selected_is_inert_and_still_closes() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["normal".into(), "light".into()],
            selected: 1,
        });
        draft.begin_choice_entry();
        assert_eq!(draft.apply_choice(), Step::Inert);
        assert!(!draft.choice_entry(), "closing is the visible answer");
    }

    #[test]
    fn a_query_matching_nothing_is_refused_with_the_field_open() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["normal".into(), "light".into()],
            selected: 0,
        });
        draft.begin_choice_entry();
        draft.set_query("zzz".into());
        assert!(matches!(draft.apply_choice(), Step::Refused(r) if r.contains("no option matches")));
        assert!(draft.choice_entry(), "the field stays open for a retype");
    }

    #[test]
    fn tab_completes_the_lit_option_and_nav_moves_the_highlight() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["estimated".into(), "declared".into(), "paid".into()],
            selected: 0,
        });
        draft.begin_choice_entry();
        draft.choice_nav(crate::vimnav::NavCommand::Move(1));
        assert_eq!(draft.choice.as_ref().unwrap().highlighted_text(), Some("declared"));
        assert!(draft.complete_choice());
        assert_eq!(draft.query, "declared");
        assert!(draft.choice_click(0));
        assert_eq!(draft.query, "declared", "a click on the only ranked row completes it");
    }

    #[test]
    fn escape_cancels_a_choice_field_with_the_value_untouched() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["a".into(), "b".into()],
            selected: 0,
        });
        draft.begin_choice_entry();
        draft.set_query("b".into());
        draft.cancel_text_entry();
        assert!(!draft.choice_entry());
        assert!(draft.choice.is_none());
        assert!(matches!(&draft.fields[0].kind, FieldKind::Choice { selected: 0, .. }));
        assert_eq!(draft.selected_row(), Some(EditRow::Field(0)));
    }

    #[test]
    fn a_one_option_choice_does_not_open_and_neither_does_a_text_row() {
        let mut one = single_field_draft(FieldKind::Choice {
            options: vec!["only".into()],
            selected: 0,
        });
        assert_eq!(one.begin_choice_entry(), Step::Inert);
        let mut text = single_field_draft(FieldKind::Text("x".into()));
        assert_eq!(text.begin_choice_entry(), Step::Inert);
    }

    /// The footer's `i` chip (§3.2): a two-option `Choice` is
    /// `StepsAndTypes` now, a one-option one still `Inert`.
    #[test]
    fn a_choice_row_steps_and_types() {
        let draft = single_field_draft(FieldKind::Choice {
            options: vec!["a".into(), "b".into()],
            selected: 0,
        });
        assert_eq!(draft.selected_vocabulary(Domain::Colours), RowVocabulary::StepsAndTypes);
    }
```

(`single_field_draft` already exists in that tests module — it is what `a_choice_wraps_backward…` uses; check its signature and match it.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell objectdialog::tests::i_on_a_choice_row 2>&1 | tail -5`
Expected: compile error — `begin_choice_entry` / `choice` not found.

- [ ] **Step 3: Replace the `completions: bool` with `Completions`**

In `mod.rs`, replace the `TextEntry` definition (~line 1148):

```rust
/// What the rows below an open field are (spec 2026-09-19 §3.2). `None`
/// is a plain value field: the rows stay the edit rows, unfiltered, with
/// the edited one highlighted (§19.1). `Chain` is Groupings' chain field
/// (§18.8): the rows are the dimensions that complete the segment being
/// typed. `Choice` is a `Choice` row's typeahead: the rows are the
/// field's own options, ranked by the query, painted from
/// [`Draft::choice`] in the row list's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completions {
    None,
    Chain,
    Choice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEntry {
    pub row: EditRow,
    pub completions: Completions,
}
```

Then, mechanically, every site that read the bool:
- `mod.rs` `visible_rows`: `if entry.completions` → `match entry.completions { Completions::Chain => groupings::chain_candidates(self), Completions::None | Completions::Choice => { /* the unfiltered branch as today */ } }`.
- `mod.rs` `set_query`: the `completions: false` pattern becomes a match — `Completions::None` → `follow(field)`; `Completions::Choice` → `{ if let Some(list) = self.choice.as_mut() { list.set_query(&self.query); } self.follow(field) }`; `Completions::Chain` → `self.selected = 0`. (Bind `row` first; the borrow of `self.query` must be a clone or taken before `choice.as_mut()` — write `let q = self.query.clone();` then `list.set_query(&q)`.)
- `mod.rs` `cancel_text_entry`: pattern `completions: false` → `Completions::None | Completions::Choice`, and add `self.choice = None;` before the match.
- `mod.rs` `apply_text_entry`: pattern `completions: false` → `completions: Completions::None`.
- `mod.rs` `chain_entry()`: `entry.completions == Completions::Chain`.
- `mod.rs` `begin_text_entry`: `completions: Completions::None`.
- `groupings.rs` `begin_chain_entry`: `completions: super::Completions::Chain`.
- `render.rs`: `entry.completions` (footer hints, pill, label), `t.completions` (`open` in the row click), `draft.text_entry.map(|t| t.completions)` → make `open: Option<Completions>` and match `Some(Completions::Chain) => on_completion_clicked(..)`, `Some(_) => {}`, `None => on_edit_row_clicked(..)`. The pill: `Some(entry) if entry.completions == Completions::Chain => chain_pill`, `Some(entry) if entry.completions == Completions::Choice => dialog::choose_pill(cx)` (Task 3 adds it; until then use `edit_pill`), else `edit_pill`. The footer's `if entry.completions {` → `if entry.completions == Completions::Chain {`. The label: `if entry.completions == Completions::Chain {`.
- Any test in `mod.rs`/`groupings.rs`/`tests/objectdialog.rs` constructing `TextEntry { .., completions: true/false }` → `Completions::Chain` / `Completions::None`.

Run `cargo check -p geode-shell --features test-support --all-targets` until it is clean.

- [ ] **Step 4: Add `choice` to `Draft` and the six methods**

In `Draft`'s struct add, beside `text_entry`:

```rust
    /// The typeahead list while a `Choice` row's field is open
    /// (`text_entry.completions == Completions::Choice`), `None`
    /// otherwise. Owns the ranking and the highlight; `selected` stays on
    /// the field's own row throughout, as it does for a plain field.
    pub choice: Option<crate::choice::ChoiceList>,
```

and `choice: None` wherever a `Draft` is constructed (`Draft::new`/`from_fields` — grep `text_entry: None`).

Add the methods beside `begin_text_entry`:

```rust
    /// `i` on a `Choice` row (spec 2026-09-19 §3.2): open the shared
    /// `Input` as a typeahead over the field's options — EMPTY, since the
    /// current value is already the highlighted row and a seed would
    /// have to be deleted before typing — with the highlight placed on
    /// the current option. `Step::Inert` off any other row, and on a
    /// one-option `Choice` (`step_selected`'s own guard, mirrored: there
    /// is nothing to choose between).
    pub fn begin_choice_entry(&mut self) -> Step {
        let Some(row @ EditRow::Field(index)) = self.selected_row() else {
            return Step::Inert;
        };
        let FieldKind::Choice { options, selected } = &self.fields[index].kind else {
            return Step::Inert;
        };
        if options.len() < 2 {
            return Step::Inert;
        }
        let mut list = crate::choice::ChoiceList::new(options.clone(), crate::choice::DEFAULT_CAP);
        list.place(options.get(*selected).map(String::as_str));
        self.choice = Some(list);
        self.query.clear();
        self.text_entry = Some(TextEntry {
            row,
            completions: Completions::Choice,
        });
        self.follow(row);
        Step::Changed
    }

    /// Whether the open field is a `Choice` row's typeahead.
    pub fn choice_entry(&self) -> bool {
        self.text_entry
            .is_some_and(|entry| entry.completions == Completions::Choice)
    }

    /// The nav keys in a choice field move the HIGHLIGHT, never the
    /// cursor (which stays on the field's row).
    pub fn choice_nav(&mut self, cmd: crate::vimnav::NavCommand) {
        if let Some(list) = self.choice.as_mut() {
            list.nav(cmd);
        }
    }

    /// A click on painted row `row` is `tab` on that row (§18.9's rule
    /// for the chain field's completion click).
    pub fn choice_click(&mut self, row: usize) -> bool {
        let Some(list) = self.choice.as_mut() else {
            return false;
        };
        if !list.set_highlighted(row) {
            return false;
        }
        self.complete_choice()
    }

    /// `tab`: the highlighted option's text becomes the query.
    pub fn complete_choice(&mut self) -> bool {
        let Some(list) = self.choice.as_mut() else {
            return false;
        };
        if !list.complete() {
            return false;
        }
        self.query = list.query().to_string();
        true
    }

    /// `enter`: the HIGHLIGHTED option becomes the field's value — never
    /// the typed text (a dropdown commits what is lit) — and the field
    /// closes. Refused with the field open when nothing is highlighted
    /// (the query matched no option). `Step::Inert` when the lit option
    /// is the one already selected: nothing to write, and the field
    /// still closes — closing is the visible answer.
    pub fn apply_choice(&mut self) -> Step {
        let Some(TextEntry {
            row: row @ EditRow::Field(index),
            completions: Completions::Choice,
        }) = self.text_entry
        else {
            return Step::Inert;
        };
        let Some(picked) = self.choice.as_ref().and_then(|l| l.pick()) else {
            return Step::Refused("no option matches — keep typing, or escape".to_string());
        };
        let outcome = match &mut self.fields[index].kind {
            FieldKind::Choice { selected, .. } if *selected == picked => Step::Inert,
            FieldKind::Choice { selected, .. } => {
                *selected = picked;
                Step::Changed
            }
            _ => Step::Inert,
        };
        self.text_entry = None;
        self.choice = None;
        self.query.clear();
        self.follow(row);
        outcome
    }
```

In `vocabulary_of`, change the `Choice` arm: `FieldKind::Choice { .. } | FieldKind::Bool(_) => RowVocabulary::Steps,` becomes two arms — `FieldKind::Choice { .. } => RowVocabulary::StepsAndTypes,` and `FieldKind::Bool(_) => RowVocabulary::Steps,` — and update `RowVocabulary::StepsAndTypes`'s doc: "Both: a `Number` (steps by one, takes a typed value) or a multi-option `Choice` (steps, and `i` opens a typeahead over its options)."

- [ ] **Step 5: Run the tests**

Run: `cargo test -p geode-shell objectdialog 2>&1 | tail -5`
Expected: all pass, including the seven new ones. (The existing footer test `…every_field_on_every_domain…` and the `a_one_option_Choice steps nowhere` test still pass — the one-option arm is unchanged.)

- [ ] **Step 6: Commit**

```bash
git add crates/geode-shell/src/shell/objectdialog/
git commit -m "objectdialog: Completions enum and the draft's choice field (pure)"
```

---

### Task 3: Object dialog — keys, painting, click, footer

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`open_text_field`, `handle_text_key`, `build_edit`, footer hints, pill, new `on_choice_row_clicked`)
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`choose_pill`, `choice_rows`)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh` (three entries)

**Interfaces:**
- Consumes: Task 1's `choice::{route, ChoiceKey, ChoiceList}`, Task 2's `Draft` methods.
- Produces:
  ```rust
  // dialog.rs
  pub(crate) fn choose_pill(cx: &App) -> AnyElement;   // selector dialog-mode-pill-choose
  pub(crate) fn choice_rows(
      list: &ChoiceList,
      selector_prefix: &'static str,   // "objectdialog" | "settings"
      theme: &Theme,
      on_click: impl Fn(usize, &mut Window, &mut App) + 'static + Clone,
  ) -> AnyElement;   // list selector "{prefix}-choice-list", rows "{prefix}-choice-{option text}"
  ```

- [ ] **Step 1: Write the failing window test**

In `crates/geode-shell/src/shell/tests/objectdialog.rs`, after `the_value_chip_steps_a_number_and_is_inert_under_a_confirm`:

```rust
/// Spec 2026-09-19 §3.2: `i` on a `Choice` row opens the shared field as
/// a typeahead over the options, painted in the row list's place;
/// typing narrows, `enter` picks the LIT row (never the typed text),
/// the change rides the tick's own commit path (a builtin colour forks
/// and says so), and the cursor is back on the row.
#[gpui::test]
fn i_on_a_choice_row_opens_a_typeahead_and_enter_picks_the_lit_option(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
    cx.simulate_keystrokes("enter"); // delta
    cx.run_until_parked();
    cx.simulate_keystrokes("j j"); // hue → tone → token
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.choice_entry()));
    assert!(dialog_filter_is_focused(&shell, &mut cx), "the field has the keys");
    assert_eq!(dialog_input_text(&shell, &cx), "", "opens empty");
    assert!(cx.debug_bounds("dialog-name-row").is_some());
    assert!(cx.debug_bounds("dialog-mode-pill-choose").is_some());
    assert!(cx.debug_bounds("objectdialog-choice-list").is_some(), "options in the list's place");
    assert!(cx.debug_bounds("objectdialog-field-hue").is_none(), "no field rows while choosing");
    assert!(cx.debug_bounds("objectdialog-actions").is_none(), "the action bar is withdrawn");

    cx.simulate_input("dan");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-choice-danger").is_some());
    assert!(cx.debug_bounds("objectdialog-choice-accent").is_none(), "narrowed away");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.choice_entry()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(dialog_input_text(&shell, &cx), "");
    assert!(edit_draft(&shell, &cx, |d| matches!(
        &d.fields[2].kind,
        objectdialog::FieldKind::Choice { options, selected } if options[*selected] == "danger"
    )));
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Field(2)),
        "the cursor is back on the token row"
    );
    assert!(cx.debug_bounds("objectdialog-field-hue").is_some(), "the field rows are back");
    assert!(cx.debug_bounds("objectdialog-confirm").is_none(), "a builtin forks without asking");
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("copied 'delta'"), "{notice}");
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("colours.toml")).unwrap();
    assert!(written.contains("token = \"danger\""), "{written}");
}

/// `tab` completes the lit option into the field, `up`/`down` move the
/// highlight, a row click is `tab`, and `escape` cancels with the value
/// untouched and the cursor on the row.
#[gpui::test]
fn tab_completes_and_escape_cancels_a_choice_field(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
    cx.simulate_keystrokes("enter j j i");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    let lit = edit_draft(&shell, &cx, |d| {
        d.choice.as_ref().unwrap().highlighted_text().map(str::to_string)
    });
    assert_eq!(lit.as_deref(), Some("foreground"), "row 1 of the 16 tokens");
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "foreground");
    // A click on the (only) painted row is `tab` too.
    let bounds = cx.debug_bounds("objectdialog-choice-foreground").unwrap();
    cx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.choice_entry()), "a click completes, it does not pick");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.choice_entry()));
    assert!(edit_draft(&shell, &cx, |d| matches!(
        &d.fields[2].kind,
        objectdialog::FieldKind::Choice { options, selected } if options[*selected] == "none"
    )));
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_none()),
        "nothing queued by a cancel"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Field(2))
    );
    assert!(cx.debug_bounds("objectdialog-hint-i").is_some(), "the footer teaches i on a Choice row");
}

/// §19.4: the read-only Schema inspector refuses `i` on its rows
/// through the same gate every other verb uses — nothing in the choice
/// path opens a field there.
#[gpui::test]
fn i_is_refused_on_the_schema_inspector(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_two_datasets_and_a_view(),
        dir.path(),
        "config::schema",
    );
    cx.simulate_keystrokes("enter i");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.text_entry.is_some()));
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("open a column"), "{notice}");
}
```

(If `simulate_mouse_down` is spelled differently in this crate's tests, copy the form `clicking_a_completion_row_completes_the_chain` uses.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support i_on_a_choice_row_opens_a_typeahead 2>&1 | tail -8`
Expected: FAIL at `assert!(edit_draft(.., |d| d.choice_entry()))` — `i` on a `Choice` row currently answers `edit_commit_notice`.

- [ ] **Step 3: `open_text_field` opens the choice field on a `Choice` row**

In `render.rs` `open_text_field`, extend the `editable` match and the open call:

```rust
    let (editable, choice) = match draft.selected_row() {
        Some(EditRow::Field(i)) => match &draft.fields[i].kind {
            FieldKind::Number { .. } => (true, false),
            FieldKind::Text(_) => (domain.text_editable(&draft.fields[i].key), false),
            // Spec 2026-09-19 §3.2: a multi-option `Choice` opens a
            // typeahead; a one-option one has nothing to choose between
            // and falls to the notice below, as stepping it would.
            FieldKind::Choice { options, .. } => (options.len() >= 2, true),
            _ => (false, false),
        },
        _ => (false, false),
    };
    if !editable {
        edit_commit_notice(shell);
        return;
    }
    if let Some(state) = shell.object_dialog.as_mut()
        && let Some(draft) = state.draft.as_mut()
    {
        let step = if choice { draft.begin_choice_entry() } else { draft.begin_text_entry() };
        if step == Step::Changed {
            state.mode = DialogMode::Filter;
        }
    }
```

The `writable()` gate already sits at `handle_edit_key`'s `EditText` arm before `open_field` is reached — confirm by reading it; do not add a second one.

- [ ] **Step 4: `handle_text_key` gets a `Choice` branch, ahead of the chain branch**

At the top of `handle_text_key` (after the `completions` binding), add:

```rust
    // ---- Choice field (spec 2026-09-19 §3.2) ------------------------
    //
    // Dispatched ahead of the chain and plain branches: `enter` here
    // picks the LIT option rather than applying typed text, `tab`
    // completes, and the nav keys move the highlight — one key table,
    // `choice::route`, shared with the settings dialog. Every other key
    // is the focused `Input`'s to type (`false`).
    let choosing = draft_mut(shell).is_some_and(|d| d.choice_entry());
    if choosing {
        let Some(key) = crate::choice::route(ks) else {
            return false;
        };
        match key {
            crate::choice::ChoiceKey::Cancel => {
                if let Some(state) = shell.object_dialog.as_mut()
                    && let Some(draft) = state.draft.as_mut()
                {
                    draft.cancel_text_entry();
                    state.mode = DialogMode::Normal;
                }
                scroll_to_cursor(shell);
            }
            crate::choice::ChoiceKey::Pick => {
                // The field's live text may never have reached the draft
                // through a `Change` event (`set_value` emits none), so
                // the ranking is refreshed from it before the pick.
                let live = shell.dialog_input.read(cx).value().to_string();
                let step = draft_mut(shell).map(|draft| {
                    draft.set_query(live);
                    draft.apply_choice()
                });
                match step {
                    Some(Step::Changed) => {
                        if let Some(state) = shell.object_dialog.as_mut() {
                            state.mode = DialogMode::Normal;
                        }
                        scroll_to_cursor(shell);
                        revalidate(shell);
                        commit_change(shell, cx);
                    }
                    Some(Step::Inert) => {
                        if let Some(state) = shell.object_dialog.as_mut() {
                            state.mode = DialogMode::Normal;
                        }
                        scroll_to_cursor(shell);
                    }
                    Some(Step::Refused(reason)) => set_notice(shell, reason),
                    None => {}
                }
            }
            crate::choice::ChoiceKey::Complete => {
                if !draft_mut(shell).is_some_and(Draft::complete_choice) {
                    set_notice(shell, "nothing to complete here".to_string());
                }
            }
            crate::choice::ChoiceKey::Nav(cmd) => {
                if let Some(draft) = draft_mut(shell) {
                    draft.choice_nav(cmd);
                }
            }
        }
        cx.notify();
        return true;
    }
```

`draft.set_query(live)` inside `Pick` routes to `ChoiceList::set_query`, which is a no-op when the text is unchanged (Task 1's guard), so an ordinary `enter` costs one compare.

- [ ] **Step 5: `dialog::choose_pill` and `dialog::choice_rows`**

In `dialog.rs`, beside `edit_pill`:

```rust
/// The pill while a `Choice` row's typeahead is open (spec 2026-09-19
/// §3.2): `choose`, the `primary` "you are typing" pair. Selector
/// `dialog-mode-pill-choose`.
pub(crate) fn choose_pill(cx: &App) -> AnyElement {
    state_pill("choose", true, cx)
}

/// The ranked options of an open choice field, painted in a dialog's
/// row list's place (spec 2026-09-19 §3.2/§3.3): one row per PAINTED
/// entry of `list`, the highlighted one in the selection colours every
/// list in these dialogs uses, matched characters highlighted through
/// `highlighted_text`. At most `list.painted_len()` rows (12), so no
/// scroll container: the query narrows the rest. `on_click(row)` is the
/// mouse form of `tab` on that row — the caller decides what that means.
/// Selectors: `{prefix}-choice-list` on the list, `{prefix}-choice-{text}`
/// on each row.
pub(crate) fn choice_rows(
    list: &crate::choice::ChoiceList,
    prefix: &'static str,
    theme: &Theme,
    on_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let mut rows = v_flex()
        .id(gpui::SharedString::from(format!("{prefix}-choice-list")))
        .w_full()
        .debug_selector(move || format!("{prefix}-choice-list"));
    for (position, ranked) in list.painted().iter().enumerate() {
        let text = list.options()[ranked.row].clone();
        let selector = format!("{prefix}-choice-{text}");
        let on_click = on_click.clone();
        let mut row = h_flex()
            .w_full()
            .h(px(28.))
            .px_3()
            .items_center()
            .text_sm()
            .debug_selector(move || selector.clone())
            .child(super::keybindings_view::highlighted_text(&text, &ranked.indices, theme.primary))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                on_click(position, window, cx);
            });
        if position == list.highlighted() {
            row = row.bg(theme.selection).text_color(theme.primary);
        }
        rows = rows.child(row);
    }
    rows.into_any_element()
}
```

(Match the imports the file already has: `v_flex`, `h_flex`, `px`, `MouseButton`, `Theme`; `highlighted_text` is `pub(crate)` in `keybindings_view`.)

- [ ] **Step 6: Paint the choice rows in `build_edit`, the pill, the label and the footer**

In `build_edit`, where `let mut list = v_flex().id("objectdialog-list")…` is built and filled, wrap it: if `draft.choice_entry()`, skip the row loop entirely and use

```rust
    let list: AnyElement = if let Some(choice) = draft.choice.as_ref().filter(|_| draft.choice_entry()) {
        let entity_for_click = entity.clone();
        dialog::choice_rows(choice, "objectdialog", theme, move |row, window, cx| {
            entity_for_click.update(cx, |shell, cx| on_choice_row_clicked(shell, row, window, cx));
        })
    } else {
        /* the existing list construction and loop, ending `.into_any_element()` */
    };
```

Add beside `on_completion_clicked`:

```rust
/// A click on a choice-field row is `tab` on it (spec 2026-09-19 §3.2,
/// §18.9's rule for the chain field). Ends in
/// [`dialog::sync_dialog_text`], the row-click seam.
fn on_choice_row_clicked(
    shell: &mut ShellView,
    row: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(draft) = draft_mut(shell) {
        draft.choice_click(row);
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

The pill site (~line 231): `Some(entry) if entry.completions == Completions::Choice => dialog::choose_pill(cx),` ahead of the `edit_pill` arm.

The label site (~line 4064): the `Choice` case falls into the existing `else` (`"{object} · {field label}"`) — no change.

The footer site (~line 3870): make the `text_entry` branch a `match entry.completions`:

```rust
        let mut hints = Vec::new();
        match entry.completions {
            Completions::Chain => {
                hints.push(Hint::prose(HintRow::Move, "type a chain · book / lhu"));
                hints.push(Hint::new(HintRow::Move, &["up", "down"], "move"));
                hints.push(Hint::new(HintRow::Go, &["tab"], "complete"));
                hints.push(Hint::new(HintRow::Go, &["enter"], "apply"));
            }
            Completions::Choice => {
                hints.push(Hint::prose(HintRow::Move, "type to narrow"));
                hints.push(Hint::new(HintRow::Move, &["up", "down"], "move"));
                hints.push(Hint::new(HintRow::Go, &["tab"], "complete"));
                hints.push(Hint::new(HintRow::Go, &["enter"], "choose"));
            }
            Completions::None => {
                hints.push(Hint::prose(HintRow::Move, "type a value"));
                hints.push(Hint::new(HintRow::Go, &["enter"], "apply"));
            }
        }
        hints.push(Hint::new(HintRow::Go, &["escape"], "cancel"));
```

The normal-mode `i` hint (~line 3925) already keys on `types` = `StepsAndTypes | Types`, so a `Choice` row now gets it via Task 2's vocabulary change; change its word from a literal `"type a value"` to `if matches!(selected_row.map(|r| draft.vocabulary_of(Some(r), state.domain)), ..)` — simpler: compute `let chooses = matches!(selected_row, Some(EditRow::Field(i)) if matches!(draft.fields[i].kind, FieldKind::Choice { .. }));` and use `if chooses { "choose a value" } else { "type a value" }`.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p geode-shell --features test-support objectdialog 2>&1 | tail -8`
Expected: all pass, including the three new window tests. Then `cargo test -p geode-shell` for the whole crate (the footer sweep `every_field_on_every_domain_has_help` and the `RowVocabulary` footer tests must still pass).

- [ ] **Step 8: Mutation harness entries**

Append to `scripts/mutation-check.sh`, after the "Mouse parity: the command palette" block:

```bash
# ---- Choice with typeahead (2026-09-19, spec §3.2) --------------------
# `enter` picks the LIT option, never the typed text: mutated to pick
# row 0 of the declared list regardless, `dan`+`enter` writes `none`.
run_mutation "choice: enter picks the highlighted option" \
  crates/geode-shell/src/choice.rs \
  '        self.highlighted_option()
    }' \
  '        Some(0)
    }' \
  geode-shell i_on_a_choice_row_opens_a_typeahead_and_enter_picks_the_lit_option

# The identity rule: a re-rank keeps the highlight by TEXT. Mutated to
# keep the ranked INDEX, typing `gruv l` after `down` lands on a row
# that is no longer the one the trader lit.
run_mutation "choice: a re-rank keeps the highlight by text" \
  crates/geode-shell/src/choice.rs \
  '        let keep = self.highlighted_text().map(str::to_string);
        self.query = query.to_string();' \
  '        let keep: Option<String> = None;
        self.query = query.to_string();' \
  geode-shell set_query_narrows_and_keeps_the_highlight_by_text

# A one-option Choice has nothing to choose between: mutated to open
# anyway, the footer test that says a one-option row is inert fails.
run_mutation "choice: a one-option Choice does not open" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if options.len() < 2 {
            return Step::Inert;
        }
        let mut list = crate::choice::ChoiceList::new' \
  '        let mut list = crate::choice::ChoiceList::new' \
  geode-shell a_one_option_choice_does_not_open_and_neither_does_a_text_row
```

Run: `zsh scripts/mutation-check.sh --anchors-only` — expected: exit 0, no stale or duplicate anchors. Then `zsh scripts/mutation-check.sh "choice:"` — expected: three `KILLED` lines.

- [ ] **Step 9: Commit**

```bash
git add crates/geode-shell/src scripts/mutation-check.sh
git commit -m "objectdialog: i on a Choice row opens a typeahead; enter picks the lit option"
```

---

### Task 4: Settings dialog — `i`/`enter` open a choice field on every row

**Files:**
- Modify: `crates/geode-shell/src/shell/settings_view.rs` (`SettingsState`, `KeyAction`, `route`, `handle_key`, `build`, footer)
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`sync_dialog_text` reads `SettingsState::effective_query`)
- Modify: `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` (§18 amendment)
- Test: `crates/geode-shell/src/shell/tests/chrome_and_dialogs.rs`, `settings_view.rs`'s own tests module
- Modify: `scripts/mutation-check.sh` (one entry)

**Interfaces:**
- Consumes: Task 1's `choice::{ChoiceList, ChoiceKey, route, DEFAULT_CAP}`, Task 3's `dialog::{choice_rows, choose_pill, name_row}`.
- Produces:
  ```rust
  pub struct ChoiceEntry { pub id: SettingId, pub list: ChoiceList }
  // on SettingsState:
  pub choice: Option<ChoiceEntry>;
  pub fn effective_query(&self) -> &str;
  pub fn choosing(&self) -> bool;
  // KeyAction gains:
  OpenChoice,            // `i` or `enter` in normal mode
  Choice(ChoiceKey),     // while a choice field is open
  // route gains a parameter:
  pub fn route(mode: DialogMode, query_is_empty: bool, choosing: bool, ks: &Keystroke) -> KeyAction;
  ```

- [ ] **Step 1: Write the failing tests**

Pure, in `settings_view.rs`'s `mod tests` (beside the existing `route` tests):

```rust
    #[test]
    fn i_and_enter_open_a_choice_in_normal_mode_and_choice_keys_route_while_open() {
        let bare = |k: &str| Keystroke { mods: Modifiers::NONE, key: k.to_string() };
        assert_eq!(route(DialogMode::Normal, true, false, &bare("i")), KeyAction::OpenChoice);
        assert_eq!(route(DialogMode::Normal, true, false, &bare("enter")), KeyAction::OpenChoice);
        assert_eq!(route(DialogMode::Filter, true, false, &bare("enter")), KeyAction::Drop, "filter mode's enter stays inert");
        assert_eq!(route(DialogMode::Filter, false, true, &bare("enter")), KeyAction::Choice(crate::choice::ChoiceKey::Pick));
        assert_eq!(route(DialogMode::Filter, false, true, &bare("escape")), KeyAction::Choice(crate::choice::ChoiceKey::Cancel));
        assert_eq!(route(DialogMode::Filter, false, true, &bare("tab")), KeyAction::Choice(crate::choice::ChoiceKey::Complete));
        assert_eq!(route(DialogMode::Filter, false, true, &bare("x")), KeyAction::PassThrough, "a letter types into the field");
    }
```

(`KeyAction` needs `PartialEq` + `Debug` if it lacks them — check the existing tests' style; they may compare with `matches!`.)

Window, in `chrome_and_dialogs.rs` after `space_and_shift_space_step_the_selected_value_in_normal_mode`:

```rust
/// Spec 2026-09-19 §3.3: `i` on the Theme row opens the shared `Input`
/// as a typeahead over every theme name, painted in the row list's
/// place; `enter` applies the lit theme live (the same `set_theme_on`
/// core a step takes) and closes; the mode is back to normal.
#[gpui::test]
fn i_on_the_theme_row_opens_a_typeahead_and_enter_applies_the_lit_theme(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let before = shell.read_with(&cx, |s, cx| s.services.theme.active_name(cx));
    cx.simulate_keystrokes("i"); // row 0 is Theme
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert!(cx.debug_bounds("dialog-name-row").is_some());
    assert!(cx.debug_bounds("dialog-mode-pill-choose").is_some());
    assert!(cx.debug_bounds("settings-choice-list").is_some());
    assert!(cx.debug_bounds("settings-list").is_none(), "the rows give way to the options");
    cx.simulate_input("gruv d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-choice-Gruvbox Dark").is_some());
    assert!(cx.debug_bounds("settings-choice-Nord").is_none());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().mode),
        DialogMode::Normal
    );
    let after = shell.read_with(&cx, |s, cx| s.services.theme.active_name(cx));
    assert_eq!(after, "Gruvbox Dark");
    assert_ne!(before, after);
    assert!(cx.debug_bounds("settings-list").is_some(), "the rows are back");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().query.clone()),
        "",
        "the FILTER query is untouched by the choice field"
    );
}

/// `escape` cancels the choice field with the setting untouched; `enter`
/// on a filtered-away list is still `Drop` (nothing to open).
#[gpui::test]
fn escape_cancels_a_settings_choice_field_untouched(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let before = shell.read_with(&cx, |s, _| s.font_size);
    cx.simulate_keystrokes("j enter"); // Font size, enter opens too
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    cx.simulate_input("lar");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    assert_eq!(shell.read_with(&cx, |s, _| s.font_size), before);
    assert!(!dialog_filter_is_focused(&shell, &mut cx), "back in normal mode");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        1,
        "the cursor stayed on Font size"
    );
}
```

(Read `ThemeService` for the exact "active theme name" accessor — `services.theme` — and substitute; the existing theme-step tests in this file show how they read it.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support settings 2>&1 | tail -8`
Expected: compile error on `route`'s arity / `choosing`.

- [ ] **Step 3: State and routing**

In `settings_view.rs`:

```rust
/// A row's typeahead while it is open (spec 2026-09-19 §3.3): which
/// setting, and the ranked options. The settings dialog has no draft;
/// every row IS a value list, so this is the whole of its choice state.
pub struct ChoiceEntry {
    pub id: SettingId,
    pub list: crate::choice::ChoiceList,
}
```

Add `pub choice: Option<ChoiceEntry>` to `SettingsState` (`choice: None` in `Default`), and:

```rust
    /// The text the shared `Input` should hold: the choice field's own
    /// query while one is open, the filter query otherwise — what
    /// `dialog::sync_dialog_text` mirrors (the object dialog's
    /// `effective_query`, for the same reason).
    pub fn effective_query(&self) -> &str {
        match self.choice.as_ref() {
            Some(entry) => entry.list.query(),
            None => self.query.as_str(),
        }
    }

    pub fn choosing(&self) -> bool {
        self.choice.is_some()
    }
```

Change `set_query` so a keystroke while choosing feeds the list, not the filter:

```rust
    pub fn set_query(&mut self, query: String) {
        if let Some(entry) = self.choice.as_mut() {
            entry.list.set_query(&query);
            return;
        }
        self.query = query;
        self.selected = 0;
    }
```

`KeyAction` gains `OpenChoice` and `Choice(crate::choice::ChoiceKey)`; derive `Debug, PartialEq, Eq` on it if absent. `route`:

```rust
pub fn route(mode: DialogMode, query_is_empty: bool, choosing: bool, ks: &Keystroke) -> KeyAction {
    // Spec 2026-09-19 §3.3: while a row's typeahead is open the field
    // owns the keys, through the one table every choice field reads.
    if choosing {
        return match crate::choice::route(ks) {
            Some(key) => KeyAction::Choice(key),
            None => KeyAction::PassThrough,
        };
    }
    if ks.key == "escape" { /* unchanged */ }
    if let Some(dir) = tab_step(ks) { /* unchanged */ }
    if let Some(cmd) = listfilter::nav_command(ks) { /* unchanged */ }
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        // §18's "enter inert" is amended (spec 2026-09-19 §7): in
        // normal mode it opens the row's typeahead beside `i`; in filter
        // mode it stays claimed and dropped, since there is no row-level
        // verb the `Input` should lose it to.
        return match mode {
            DialogMode::Normal => KeyAction::OpenChoice,
            DialogMode::Filter => KeyAction::Drop,
        };
    }
    match mode {
        DialogMode::Filter => KeyAction::PassThrough,
        DialogMode::Normal => match dialogmode::normal_command(ks) {
            Some(NormalCommand::Nav(nav)) => KeyAction::Nav(nav),
            Some(NormalCommand::EnterFilter) => KeyAction::EnterFilter,
            Some(NormalCommand::Toggle) => KeyAction::Step(StepDirection::Right),
            Some(NormalCommand::ToggleBack) => KeyAction::Step(StepDirection::Left),
            Some(NormalCommand::EditText) => KeyAction::OpenChoice,
            _ => KeyAction::Drop,
        },
    }
}
```

Update `route`'s doc comment rungs 4 and 6 to say so. Update every existing `route(..)` call and test to the new arity (`state.choosing()` at the call site).

- [ ] **Step 4: `handle_key`'s two new arms**

```rust
        KeyAction::OpenChoice => {
            let selected = state.selected;
            // An empty filtered list has nothing to open: claimed, dropped.
            if let Some(row) = visible.get(selected).and_then(|m| rows.get(m.row)) {
                let mut list = crate::choice::ChoiceList::new(row.values.clone(), crate::choice::DEFAULT_CAP);
                list.place(row.values.get(row.current).map(String::as_str));
                state.choice = Some(ChoiceEntry { id: row.id, list });
                state.mode = DialogMode::Filter;
            }
        }
        KeyAction::Choice(key) => match key {
            crate::choice::ChoiceKey::Cancel => {
                state.choice = None;
                state.mode = DialogMode::Normal;
            }
            crate::choice::ChoiceKey::Pick => {
                // The field's live text may never have reached the list
                // through a `Change` event (`set_value` emits none).
                let live = shell.dialog_input.read(cx).value().to_string();
                let Some(state) = shell.settings.as_mut() else { return true };
                let picked = state.choice.as_mut().and_then(|entry| {
                    entry.list.set_query(&live);
                    entry.list.pick().map(|ix| (entry.id, ix))
                });
                match picked {
                    Some((id, ix)) => {
                        state.choice = None;
                        state.mode = DialogMode::Normal;
                        apply_setting(shell, id, ix, cx);
                    }
                    // Nothing lit: the field stays open. The settings
                    // dialog has no notice slot; the empty list says it.
                    None => {}
                }
            }
            crate::choice::ChoiceKey::Complete => {
                if let Some(entry) = state.choice.as_mut() {
                    entry.list.complete();
                }
            }
            crate::choice::ChoiceKey::Nav(cmd) => {
                if let Some(entry) = state.choice.as_mut() {
                    entry.list.nav(cmd);
                }
            }
        },
```

(`state` is a `&mut` borrowed from `shell.settings` at the top of `handle_key`; the `Pick` arm needs `shell.dialog_input` first, so restructure that arm to read the live text before re-borrowing `state`, as shown.)

- [ ] **Step 5: `sync_dialog_text`, painting, footer, pill**

`dialog.rs` `sync_dialog_text`: the settings arm becomes `(state.mode, false, state.effective_query())`.

`settings_view.rs` `build`: when `state.choice` is `Some(entry)`, replace the filter row with `dialog::name_row(&shell.dialog_input, &format!("{} · choose", rows.iter().find(|r| r.id == entry.id).map(|r| r.title).unwrap_or("")), cx)` and the list with

```rust
        let entity_for_click = entity.clone();
        dialog::choice_rows(&entry.list, "settings", theme, move |row, window, cx| {
            entity_for_click.update(cx, |shell, cx| on_choice_row_clicked(shell, row, window, cx));
        })
```

with

```rust
/// A click on a choice-field row is `tab` on it (spec 2026-09-19 §3.3).
/// Ends in [`dialog::sync_dialog_text`], the row-click seam.
fn on_choice_row_clicked(shell: &mut ShellView, row: usize, window: &mut Window, cx: &mut Context<ShellView>) {
    if let Some(entry) = shell.settings.as_mut().and_then(|s| s.choice.as_mut())
        && entry.list.set_highlighted(row)
    {
        entry.list.complete();
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

Footer: a third arm ahead of the mode match — `if state.choosing() { vec![Hint::prose(Move, "type to narrow"), Hint::new(Move, &["up","down"], "move"), Hint::new(Go, &["tab"], "complete"), Hint::new(Go, &["enter"], "choose"), Hint::new(Go, &["escape"], "cancel")] }`. Normal mode's hints gain `Hint::new(HintRow::Edit, &["i", "enter"], "choose").selector("settings-hint-choose")`.

Pill: the `set_title_extra` closure — `s.choice.is_some()` → `dialog::choose_pill(cx)`, else `dialog::mode_pill(s.mode, cx)`.

`on_row_clicked` / `on_value_chip_clicked`: a click while choosing does nothing (the object dialog's rule) — add `if state.choosing() { return; }` after the `state` borrow in both.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-shell --features test-support 2>&1 | tail -8`
Expected: all pass, including the three new ones and every existing settings test (`the_settings_dialog_opens_in_normal_mode_and_letters_do_not_type` still holds — a bare `enter` now opens rather than drops, so if that test presses `enter` expecting nothing, update it to press a letter instead and add the reason).

- [ ] **Step 7: Spec amendment and harness entry**

Append to `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` §18 (the settings section), a dated paragraph:

> **Amended 2026-09-19** (choice-with-typeahead design §3.3, §7): `enter` is no longer inert in the settings dialog's normal mode — it and `i` open the selected row's typeahead (the shared `Input` in the filter row's place over the row's values, `ChoiceList`), `enter` there applying the lit value through the same `apply_setting` core a step takes. Filter mode's `enter` stays claimed and dropped.

Harness:

```bash
# Settings (spec 2026-09-19 §3.3): the pick applies through the one
# `apply_setting` core. Mutated to close without applying, `gruv d` +
# `enter` leaves the previous theme active.
run_mutation "settings choice: enter applies the lit value" \
  crates/geode-shell/src/shell/settings_view.rs \
  '                        apply_setting(shell, id, ix, cx);' \
  '                        let _ = (id, ix);' \
  geode-shell i_on_the_theme_row_opens_a_typeahead_and_enter_applies_the_lit_theme
```

Run `zsh scripts/mutation-check.sh --anchors-only` then `zsh scripts/mutation-check.sh "settings choice"`.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-shell/src docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md scripts/mutation-check.sh
git commit -m "settings: i/enter open a typeahead over the row's values (spec §18 amended)"
```

---

### Task 5: The underlying picker over `ChoiceList`

**Files:**
- Modify: `crates/geode-marketdata/src/popup.rs` (`PickerRows` internals; `render_picker` reads)
- Modify: `crates/geode-marketdata/src/tile.rs` (every `rows.ranked` / `rows.highlighted` / `rows.all` read — mechanical)
- Test: `popup.rs`'s existing tests (unchanged expectations)

**Interfaces:**
- Consumes: Task 1's `ChoiceList`.
- Produces: `PickerRows`'s existing method set (`with_marks`, `replace_all`, `refilter`, `place`, `step_highlighted`, `painted_len`, `highlighted_key`) with the same behaviour; its `all`/`ranked`/`highlighted`/`query` fields become methods over the inner list.

- [ ] **Step 1: Re-implement `PickerRows`**

```rust
pub(crate) struct PickerRows {
    /// The ranking and the highlight (spec 2026-09-19 §3.1): one core
    /// with the dialogs' choice fields, so the cap, the identity rule
    /// and the re-rank guard are spelled once.
    list: geode_shell::choice::ChoiceList,
    /// The prepared row text, one per option — see the struct's doc.
    pub labels: Vec<SharedString>,
    pub marks: BTreeMap<String, String>,
}

impl PickerRows {
    pub(crate) fn with_marks(all: Vec<String>, marks: BTreeMap<String, String>) -> Self {
        let labels = Self::labels_for(&all, &marks);
        Self {
            list: geode_shell::choice::ChoiceList::new(all, PICKER_ROWS),
            labels,
            marks,
        }
    }
    pub(crate) fn all(&self) -> &[String] { self.list.options() }
    pub(crate) fn query(&self) -> &str { self.list.query() }
    pub(crate) fn highlighted(&self) -> usize { self.list.highlighted() }
    /// The declared indices of the painted rows, in ranked order.
    pub(crate) fn painted(&self) -> impl Iterator<Item = usize> + '_ {
        self.list.painted().iter().map(|r| r.row)
    }
    pub(crate) fn painted_len(&self) -> usize { self.list.painted_len() }
    pub(crate) fn highlighted_key(&self) -> Option<&str> { self.list.highlighted_text() }
    pub(crate) fn refilter(&mut self, new_query: &str) { self.list.set_query(new_query); }
    pub(crate) fn replace_all(&mut self, all: Vec<String>) {
        self.labels = Self::labels_for(&all, &self.marks);
        self.list.replace_options(all);
    }
    pub(crate) fn place(&mut self, key: Option<&str>) { self.list.place(key); }
    /// Clamped, never wrapping — header spec §7's own rule for the
    /// picker, kept (`nav_clamped`).
    pub(crate) fn step_highlighted(&mut self, delta: isize) {
        self.list.nav_clamped(geode_shell::vimnav::NavCommand::Move(delta as i64));
    }
}
```

Keep `PICKER_ROWS` as the picker's cap constant (its doc now says it is `choice::DEFAULT_CAP`'s value — add `const _: () = assert!(PICKER_ROWS == geode_shell::choice::DEFAULT_CAP);`). Update `render_picker` (`rows.ranked.iter().take(PICKER_ROWS)` → `rows.painted()`, `rows.highlighted` → `rows.highlighted()`) and every `tile.rs` read (`grep -n "rows\.\(ranked\|highlighted\|all\|query\)" crates/geode-marketdata/src/tile.rs`).

- [ ] **Step 2: Run the picker tests**

Run: `cargo test -p geode-marketdata popup 2>&1 | tail -5` and `cargo test -p geode-marketdata picker 2>&1 | tail -5`
Expected: every existing test passes unchanged — the tests read through the methods (adjust field reads in the tests to the methods; expectations stay).

- [ ] **Step 3: Full check**

Run: `cargo test --workspace 2>&1 | tail -3 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo fmt --check && zsh scripts/mutation-check.sh --anchors-only`
Expected: green, no warnings, no stale anchors (the harness entries that anchor on `PickerRows` internals — `grep -n "popup.rs" scripts/mutation-check.sh` — must be re-anchored to the new lines if `--anchors-only` reports them).

- [ ] **Step 4: Commit**

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "marketdata: PickerRows rides geode_shell::choice::ChoiceList"
```

---

### Task 6: Docs and hand-off

**Files:**
- Modify: `CLAUDE.md` (one paragraph after "Footer rows by category")
- Modify: `docs/superpowers/specs/2026-09-14-geode-market-data-panel-header-design.md` (§7 note: `PickerRows` is `ChoiceList`)
- Modify: `docs/superpowers/specs/2026-09-19-geode-dividend-schedule-and-choice-design.md` (§3 "As built" line)

- [ ] **Step 1: CLAUDE.md paragraph**

Insert after the "Footer rows by category" paragraph:

> **Choice with typeahead (2026-09-19, spec `2026-09-19-geode-dividend-schedule-and-choice-design.md` §3):** `geode_shell::choice::ChoiceList` is the one "choose one value from a list" core — options, query, a highlight over the first `DEFAULT_CAP` (12) ranked rows, `set_query`/`replace_options` keeping the highlight by TEXT (never index), `complete` (`tab`), `pick` (the declared index of the lit row), `nav` (bare ±1 wraps, §20.5) and `nav_clamped` (the underlying picker's rule) — and `choice::route` is the one key table (`escape` cancel, bare `enter` pick, `tab` complete whatever the modifiers, the `listfilter` nav keys). Three surfaces ride it: the object dialog (`i` on a multi-option `Choice` row, `Completions::Choice` on `TextEntry`, `Draft.choice`, the options painted by `dialog::choice_rows` in the row list's place, pill `choose`), the settings dialog (`i`/`enter` on every row — `enter` is no longer inert there, §18 amended; `SettingsState.choice`, `effective_query`), and the market-data underlying picker (`PickerRows` wraps a `ChoiceList`, behaviour unchanged). **Two things a maintainer must know:** `enter` picks the HIGHLIGHTED option, never the typed text, and every pick path re-feeds the field's live text through `set_query` first because `InputState::set_value` emits no `Change` event (the guard makes an unchanged text a compare); and a row click in a choice list is `tab`, not `enter` — §18.9's rule for the chain field, kept so a mis-click cannot commit.

- [ ] **Step 2: Spec notes**

Header spec §7: add "**2026-09-19:** `PickerRows` is now a thin wrapper over `geode_shell::choice::ChoiceList` (choice-with-typeahead design §3.1/§7); its keys, cap and clamped `up`/`down` are unchanged."

Choice spec §3: add "**As built (2026-09-19):** Tasks 1–5 of `docs/superpowers/plans/2026-09-19-choice-with-typeahead.md`; the object dialog paints the options through `dialog::choice_rows` in place of the row list rather than through `EditRow` rows (no new `EditRow` variant), and the settings dialog's `enter` opens the field in normal mode only."

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/superpowers/specs/
git commit -m "docs: choice with typeahead — CLAUDE.md, header spec and choice spec notes"
```

Then hand off: `superpowers:finishing-a-development-branch`.
