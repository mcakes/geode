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
        Self {
            options,
            query: String::new(),
            ranked,
            highlighted: 0,
            cap,
        }
    }

    pub fn options(&self) -> &[String] {
        &self.options
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn ranked(&self) -> &[Ranked] {
        &self.ranked
    }

    /// The rows a surface paints — the first `cap` of `ranked`.
    pub fn painted(&self) -> &[Ranked] {
        &self.ranked[..self.painted_len()]
    }

    pub fn painted_len(&self) -> usize {
        self.ranked.len().min(self.cap)
    }

    /// The highlighted PAINTED row.
    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keystroke, Modifiers};

    fn opts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn key(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: k.to_string(),
        }
    }

    #[test]
    fn an_empty_query_lists_every_option_in_declared_order_capped() {
        let list = ChoiceList::new(opts(&["a", "b", "c"]), 2);
        assert_eq!(
            list.ranked().iter().map(|r| r.row).collect::<Vec<_>>(),
            [0, 1, 2]
        );
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
        assert_eq!(
            list.highlighted_text(),
            Some("Gruvbox Light"),
            "kept by text across a re-rank"
        );
        assert!(
            !list.set_query("gruv l"),
            "an unchanged query moves nothing"
        );

        // The case above never actually distinguishes "kept by text"
        // from "kept by index" — every re-rank there leaves exactly one
        // surviving candidate, so falling back to row 0 lands on the
        // same option either way. Here "Bamboo" is placed at row 0 (its
        // DECLARED index), and the re-rank leaves TWO candidates: "am"
        // scores "Ambrose" higher (it matches at the very start of the
        // word) than "Bamboo" (a mid-word match), so after the re-rank
        // "Ambrose" is ranked row 0 and "Bamboo" row 1 — a kept-by-INDEX
        // identity (or a dropped one, which falls back to row 0) would
        // both land the highlight on "Ambrose"; only kept-by-TEXT still
        // finds "Bamboo".
        let mut rivals = ChoiceList::new(opts(&["Bamboo", "Ambrose"]), 12);
        rivals.place(Some("Bamboo"));
        assert_eq!(
            rivals.highlighted_text(),
            Some("Bamboo"),
            "placed at its declared row 0"
        );
        assert!(rivals.set_query("am"));
        assert_eq!(
            rivals.highlighted_text(),
            Some("Bamboo"),
            "kept by text even though it no longer ranks first"
        );
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
        assert!(
            !list.set_highlighted(12),
            "a click past the painted rows is refused"
        );
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
        let shift_tab = Keystroke {
            mods: Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            key: "tab".into(),
        };
        assert!(
            matches!(route(&shift_tab), Some(ChoiceKey::Complete)),
            "tab whatever the modifiers"
        );
        assert!(matches!(
            route(&key("down")),
            Some(ChoiceKey::Nav(NavCommand::Move(1)))
        ));
        assert!(matches!(
            route(&key("up")),
            Some(ChoiceKey::Nav(NavCommand::Move(-1)))
        ));
        assert!(
            route(&key("a")).is_none(),
            "a letter is the field's to type"
        );
        assert!(route(&key("space")).is_none());
        let ctrl_enter = Keystroke {
            mods: Modifiers::CTRL,
            key: "enter".into(),
        };
        assert!(route(&ctrl_enter).is_none(), "only a BARE enter picks");
    }
}
