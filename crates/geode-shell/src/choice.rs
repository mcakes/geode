//! Pure typeahead choice state: options, fuzzy ranking, selection, and a
//! sliding display window. Callers retain window focus and apply the picked value.
//!
//! Reranking preserves the selected option by its text, falling back to the first
//! ranked row when that text no longer matches. Duplicate option texts resolve to
//! the first declared occurrence. The cap limits the painted window, not the
//! ranked list; callers must supply a positive cap for a visible selection.

use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter::{self, Ranked};
use crate::vimnav::{self, NavCommand};

/// Default number of visible choice rows. Ranking and navigation retain all matches.
pub const DEFAULT_CAP: usize = 12;

/// One list of options, the query it is ranked against, and which
/// painted row is highlighted. See the module doc for the identity rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceList {
    options: Vec<String>,
    query: String,
    ranked: Vec<Ranked>,
    /// Index into `ranked` (the FULL list, not the painted window).
    highlighted: usize,
    /// The start of the painted slice into `ranked`. Always kept, by
    /// `follow`, so that `window <= highlighted < window + cap` (or
    /// `ranked` is empty) — see `follow`'s own doc comment.
    window: usize,
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
            window: 0,
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

    /// The rows a surface paints — the `cap`-wide slice of `ranked`
    /// starting at `window`, which `follow` keeps around the highlight.
    pub fn painted(&self) -> &[Ranked] {
        &self.ranked[self.window..self.window + self.painted_len()]
    }

    pub fn painted_len(&self) -> usize {
        (self.ranked.len() - self.window).min(self.cap)
    }

    /// The highlighted row, WINDOW-relative — what a painter of the
    /// window alone (the market-data picker) compares row positions
    /// against; a click's `row` argument to `set_highlighted` lives in
    /// this same space.
    pub fn highlighted(&self) -> usize {
        self.highlighted - self.window
    }

    /// Selected index in the full ranked list, for callers rendering every row
    /// inside a scroll container. Window-relative painters use [`Self::highlighted`].
    pub fn ranked_highlighted(&self) -> usize {
        self.highlighted
    }

    /// A click on ranked row `row` — [`Self::set_highlighted`]'s twin for
    /// a painter of every ranked row. Refused (`false`) past the list.
    pub fn set_ranked_highlighted(&mut self, row: usize) -> bool {
        if row >= self.ranked.len() {
            return false;
        }
        self.highlighted = row;
        self.follow();
        true
    }

    /// The highlighted option's index in the DECLARED list — what a pick
    /// means. `None` only with nothing ranked.
    pub fn highlighted_option(&self) -> Option<usize> {
        self.ranked.get(self.highlighted).map(|r| r.row)
    }

    pub fn highlighted_text(&self) -> Option<&str> {
        self.highlighted_option().map(|i| self.options[i].as_str())
    }

    /// Rerank a changed query, preserving selection by option text. Return false
    /// without reranking when the text is unchanged, so rereading Input at commit
    /// does not disturb the current selection.
    pub fn set_query(&mut self, query: &str) -> bool {
        if query == self.query {
            return false;
        }
        let keep = self.highlighted_text().map(str::to_string);
        self.query = query.to_string();
        self.place(keep.as_deref());
        true
    }

    /// Rerank a changed query and put the highlight on `value`, or on the
    /// top-ranked row when `value` is `None` or the query filters it out.
    /// Unlike [`Self::set_query`] the row lit before is not kept: for a
    /// list whose opening row is a default the user did not choose, a row
    /// that merely survives the query must not stay lit over the one the
    /// query ranks first. An unchanged query returns false and moves
    /// nothing, so a highlight moved since the last change is kept.
    pub fn set_query_placing(&mut self, query: &str, value: Option<&str>) -> bool {
        if query == self.query {
            return false;
        }
        self.query = query.to_string();
        self.place(value);
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

    /// Rerank and select the first occurrence of `value` when it survives the
    /// filter, otherwise row zero. Reset the window and bring selection into view.
    pub fn place(&mut self, value: Option<&str>) {
        self.ranked = listfilter::rank(&self.options, &self.query);
        self.highlighted = value
            .and_then(|v| self.options.iter().position(|o| o == v))
            .and_then(|declared| self.ranked.iter().position(|r| r.row == declared))
            .unwrap_or(0);
        self.window = 0;
        self.follow();
    }

    /// Slide the window to contain selection, then clamp its start so the
    /// last visible window remains full when enough results exist. Requires cap > 0.
    fn follow(&mut self) {
        if self.highlighted < self.window {
            self.window = self.highlighted;
        } else if self.highlighted >= self.window + self.cap {
            self.window = self.highlighted + 1 - self.cap;
        }
        self.window = self.window.min(self.ranked.len().saturating_sub(self.cap));
    }

    /// Navigate the full ranked list: deltas ±1 wrap and other moves clamp.
    /// Then bring the selected row into the painted window.
    pub fn nav(&mut self, cmd: NavCommand) {
        self.highlighted = vimnav::apply(self.highlighted, self.ranked.len(), cmd);
        self.follow();
    }

    /// Navigate with every move clamped, then bring selection into view.
    pub fn nav_clamped(&mut self, cmd: NavCommand) {
        self.highlighted = vimnav::apply_clamped(self.highlighted, self.ranked.len(), cmd);
        self.follow();
    }

    /// Select a window-relative painted row, matching [`Self::highlighted`].
    /// Return false outside the painted slice. Full-list painters instead use
    /// [`Self::set_ranked_highlighted`].
    pub fn set_highlighted(&mut self, row: usize) -> bool {
        if row >= self.painted_len() {
            return false;
        }
        self.highlighted = self.window + row;
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

/// Map Escape and Tab with any modifiers, bare Enter, and shared list
/// navigation to choice commands. Return `None` for the caller to route as text
/// or another operation. This helper does not change focus or apply a value.
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

        // Reordering options changes the selected index. Retaining the same
        // text proves selection follows option identity, not the old index.
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

    /// A list opened on a row that is the default, not the user's choice,
    /// must not keep that row lit through a query it merely survives:
    /// Enter would then commit a row the user did not type towards.
    #[test]
    fn set_query_placing_lights_the_named_row_or_the_top_rank() {
        let mut list = ChoiceList::new(opts(&["Bamboo", "Ambrose", "Nord"]), 12);
        assert_eq!(list.highlighted_text(), Some("Bamboo"));
        assert!(list.set_query_placing("am", None));
        assert_eq!(
            list.highlighted_text(),
            list.ranked()
                .first()
                .map(|r| list.options()[r.row].as_str()),
            "the top-ranked row, not the one lit before"
        );
        assert_eq!(list.highlighted_text(), Some("Ambrose"));
        assert_eq!(list.query(), "am");

        list.nav(NavCommand::Move(1));
        assert_eq!(list.highlighted_text(), Some("Bamboo"));
        assert!(
            !list.set_query_placing("am", None),
            "an unchanged query moves nothing"
        );
        assert_eq!(
            list.highlighted_text(),
            Some("Bamboo"),
            "a highlight moved since the last query change is kept"
        );

        assert!(list.set_query_placing("", Some("Nord")));
        assert_eq!(list.highlighted_text(), Some("Nord"), "the named row");
        assert!(list.set_query_placing("b", Some("Nord")));
        assert_eq!(
            list.highlighted_text(),
            Some("Bamboo"),
            "a named row the query filters out falls back to the top rank"
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
        assert_eq!(list.highlighted(), 2, "a bare -1 wraps");
        list.nav_clamped(NavCommand::Move(5));
        assert_eq!(list.highlighted(), 2, "clamped at the last painted row");
        list.nav_clamped(NavCommand::Move(-5));
        assert_eq!(list.highlighted(), 0);
    }

    #[test]
    fn the_highlight_never_leaves_the_painted_range() {
        // 20 options, cap 12 (window < list): a jump to the very bottom
        // has to drag the window along rather than clamping the
        // highlight to whatever the window last showed.
        let mut list = ChoiceList::new((0..20).map(|i| format!("o{i}")).collect(), 12);
        list.nav_clamped(NavCommand::Move(100));
        assert_eq!(
            list.highlighted_option(),
            Some(19),
            "clamped at the last DECLARED row, not the last painted one"
        );
        assert_eq!(list.highlighted(), 11, "still the last row OF the window");
        assert_eq!(
            list.painted()[0].row,
            8,
            "the window followed the highlight into view"
        );
        assert!(
            !list.set_highlighted(12),
            "a click past the painted rows is refused"
        );
        assert!(list.set_highlighted(3));
        assert_eq!(
            list.highlighted_option(),
            Some(11),
            "row 3 of a window that starts at 8"
        );
        list.nav(NavCommand::Bottom);
        assert_eq!(
            list.highlighted_option(),
            Some(19),
            "Bottom is the last DECLARED row"
        );
        list.nav(NavCommand::Top);
        assert_eq!(list.highlighted_option(), Some(0), "Top is the first row");
        assert_eq!(
            list.painted()[0].row,
            0,
            "the window followed the highlight back to the start"
        );
        list.nav(NavCommand::Move(-1));
        assert_eq!(
            list.highlighted_option(),
            Some(19),
            "a bare -1 wraps over the WHOLE ranked list, not just the window"
        );
        assert!(
            list.painted().iter().any(|r| r.row == 19),
            "the window followed the wrap into view"
        );
    }

    #[test]
    fn place_brings_a_value_past_the_cap_into_view() {
        // A selected value beyond the first window must remain both visible
        // and pickable after reranking.
        let options: Vec<String> = (0..44).map(|i| format!("t{i:02}")).collect();
        let mut list = ChoiceList::new(options, 12);
        list.place(Some("t30"));
        assert_eq!(list.highlighted_text(), Some("t30"));
        assert_eq!(list.highlighted(), 11, "the last row of its window");
        let painted: Vec<&str> = list
            .painted()
            .iter()
            .map(|r| list.options()[r.row].as_str())
            .collect();
        assert_eq!(
            painted,
            (19..=30).map(|i| format!("t{i:02}")).collect::<Vec<_>>()
        );

        // Narrowing the query re-finds it by text and keeps it in view.
        assert!(list.set_query("t3"));
        assert_eq!(list.highlighted_text(), Some("t30"));
        assert!(
            list.painted()
                .iter()
                .any(|r| list.options()[r.row] == "t30"),
            "still in view after the re-rank"
        );
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
