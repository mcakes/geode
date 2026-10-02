//! Prepared row lists for the shell's list dialogs.
//!
//! A dialog's rows are derived from their inputs once per input change and ranked
//! once per query change, then read by render and by every handler, so a key or
//! click acts on exactly the rows that were painted. `K` holds exactly the inputs
//! the derivation reads; `Q` holds what the ranking reads. Render never refreshes:
//! the shell refreshes at its event seams (`ShellView::refresh_dialog_rows`), and a
//! debug-build assertion in each dialog's `build` refuses a stale list rather than
//! repairing it.

use std::ops::Range;

use gpui::SharedString;

use crate::listfilter::Ranked;

/// The painted lines of a row. `primary` is the title line; `secondary` the muted
/// line under or beside it (a category, a summary, a value). Shared so render
/// clones a reference count, never the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowText {
    pub primary: SharedString,
    pub secondary: SharedString,
}

impl RowText {
    /// `"{primary} {secondary}"`: what a two-line list (keybindings, object
    /// browse) filters on, and the shape `palette::split_label_indices` splits.
    pub fn two_line(text: &RowText) -> String {
        format!("{} {}", text.primary, text.secondary)
    }

    /// The primary line alone, for a list whose secondary line is a value that
    /// must not match.
    pub fn primary_only(text: &RowText) -> String {
        text.primary.to_string()
    }
}

/// One row the ranking kept: its index into [`Prepared::rows`] and its match
/// highlights as UTF-8 byte ranges into each painted line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub row: usize,
    pub primary: Vec<Range<usize>>,
    pub secondary: Vec<Range<usize>>,
}

/// What a [`Prepared::refresh`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// Key and query unchanged.
    Nothing,
    /// Only the query changed: re-ranked the rows already derived.
    Ranked,
    /// The key changed: re-derived and re-ranked.
    Derived,
}

/// Rows derived for `K` and ranked for `Q`. See the module doc.
#[derive(Debug, Clone)]
pub struct Prepared<K, Q, R> {
    derived_for: Option<K>,
    ranked_for: Option<Q>,
    rows: Vec<R>,
    texts: Vec<RowText>,
    searchable: Vec<String>,
    shown: Vec<Shown>,
    /// Derivations run, for tests that prove a query change does not derive.
    #[cfg(any(test, feature = "test-support"))]
    pub derives: usize,
    /// Rankings run.
    #[cfg(any(test, feature = "test-support"))]
    pub ranks: usize,
}

impl<K, Q, R> Default for Prepared<K, Q, R> {
    fn default() -> Self {
        Prepared {
            derived_for: None,
            ranked_for: None,
            rows: Vec::new(),
            texts: Vec::new(),
            searchable: Vec::new(),
            shown: Vec::new(),
            #[cfg(any(test, feature = "test-support"))]
            derives: 0,
            #[cfg(any(test, feature = "test-support"))]
            ranks: 0,
        }
    }
}

impl<K: PartialEq + Clone, Q: PartialEq + Clone, R> Prepared<K, Q, R> {
    /// Nothing derived: [`Self::is_current`] is false for every key.
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-derive when `key` differs from the last derivation's, re-rank when
    /// either differs. `searchable` turns a row's text into what `rank` matches;
    /// `rank` sees the rows, those texts and the query.
    pub fn refresh(
        &mut self,
        key: &K,
        query: &Q,
        derive: impl FnOnce() -> Vec<(R, RowText)>,
        searchable: fn(&RowText) -> String,
        rank: impl FnOnce(&[R], &[String], &Q) -> Vec<Ranked>,
    ) -> Refreshed {
        let derive_now = self.derived_for.as_ref() != Some(key);
        if derive_now {
            let (rows, texts): (Vec<R>, Vec<RowText>) = derive().into_iter().unzip();
            self.searchable = texts.iter().map(searchable).collect();
            self.rows = rows;
            self.texts = texts;
            self.derived_for = Some(key.clone());
            self.ranked_for = None;
            #[cfg(any(test, feature = "test-support"))]
            {
                self.derives += 1;
            }
        }
        if self.ranked_for.as_ref() == Some(query) {
            return Refreshed::Nothing;
        }
        let ranked = rank(&self.rows, &self.searchable, query);
        let texts = &self.texts;
        self.shown = ranked
            .into_iter()
            .map(|m| shown_of(&texts[m.row], m))
            .collect();
        self.ranked_for = Some(query.clone());
        #[cfg(any(test, feature = "test-support"))]
        {
            self.ranks += 1;
        }
        if derive_now {
            Refreshed::Derived
        } else {
            Refreshed::Ranked
        }
    }

    /// Whether the rows were derived for `key` and ranked for `query`.
    pub fn is_current(&self, key: &K, query: &Q) -> bool {
        self.derived_for.as_ref() == Some(key) && self.ranked_for.as_ref() == Some(query)
    }

    /// Every derived row, in derivation order.
    pub fn rows(&self) -> &[R] {
        &self.rows
    }

    /// Each derived row's painted text, index-aligned with [`Self::rows`].
    pub fn texts(&self) -> &[RowText] {
        &self.texts
    }

    /// The rows the ranking kept, in display order.
    pub fn shown(&self) -> &[Shown] {
        &self.shown
    }

    /// How many rows are shown.
    pub fn len(&self) -> usize {
        self.shown.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// The row at filtered `position`.
    pub fn at(&self, position: usize) -> Option<&R> {
        self.shown.get(position).and_then(|s| self.rows.get(s.row))
    }

    /// The filtered position of the first shown row matching `pred`; `None` when
    /// the filter hides it. Click handlers key rows by identity and turn it back
    /// into a position here.
    pub fn position(&self, pred: impl Fn(&R) -> bool) -> Option<usize> {
        self.shown
            .iter()
            .position(|s| self.rows.get(s.row).is_some_and(&pred))
    }
}

/// Split a match's character indices at the primary line and turn each half into
/// UTF-8 byte ranges over its own line.
fn shown_of(text: &RowText, m: Ranked) -> Shown {
    let primary_len = text.primary.chars().count();
    let (primary, secondary) = crate::palette::split_label_indices(&m.indices, primary_len);
    Shown {
        row: m.row,
        primary: crate::palette::highlight_runs(&text.primary, &primary),
        secondary: crate::palette::highlight_runs(&text.secondary, &secondary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listfilter;

    fn derive(names: &[&'static str]) -> Vec<(&'static str, RowText)> {
        names
            .iter()
            .map(|n| {
                (
                    *n,
                    RowText {
                        primary: SharedString::new_static(n),
                        secondary: SharedString::new_static("Cat"),
                    },
                )
            })
            .collect()
    }

    // `&String` because `Q = String`: `refresh` hands the ranker `&Q`.
    #[allow(clippy::ptr_arg)]
    fn rank(_: &[&'static str], texts: &[String], q: &String) -> Vec<listfilter::Ranked> {
        listfilter::rank(texts, q)
    }

    #[test]
    fn a_query_change_reranks_without_re_deriving() {
        let mut p: Prepared<u64, String, &'static str> = Prepared::new();
        let names = ["Focus left", "Focus right", "Close tile"];
        assert_eq!(
            p.refresh(
                &1,
                &String::new(),
                || derive(&names),
                RowText::two_line,
                rank
            ),
            Refreshed::Derived
        );
        assert_eq!(p.len(), 3);
        assert_eq!(
            p.refresh(
                &1,
                &"clo".to_string(),
                || panic!("a query change must not derive"),
                RowText::two_line,
                rank
            ),
            Refreshed::Ranked
        );
        assert_eq!(p.at(0), Some(&"Close tile"));
        assert_eq!(p.derives, 1);
        assert_eq!(p.ranks, 2);
        assert_eq!(
            p.refresh(
                &1,
                &"clo".to_string(),
                || panic!("unchanged"),
                RowText::two_line,
                |_, _, _| panic!("unchanged")
            ),
            Refreshed::Nothing
        );
    }

    #[test]
    fn a_key_change_re_derives_and_re_ranks() {
        let mut p: Prepared<u64, String, &'static str> = Prepared::new();
        p.refresh(
            &1,
            &"fo".to_string(),
            || derive(&["Focus left"]),
            RowText::two_line,
            rank,
        );
        let refreshed = p.refresh(
            &2,
            &"fo".to_string(),
            || derive(&["Focus left", "Focus up"]),
            RowText::two_line,
            rank,
        );
        assert_eq!(refreshed, Refreshed::Derived);
        assert_eq!(p.len(), 2);
        assert!(p.is_current(&2, &"fo".to_string()));
        assert!(!p.is_current(&1, &"fo".to_string()));
        assert!(!p.is_current(&2, &"f".to_string()));
    }

    #[test]
    fn highlights_split_at_the_primary_line_as_byte_ranges() {
        let mut p: Prepared<u64, String, &'static str> = Prepared::new();
        p.refresh(
            &1,
            &"lc".to_string(),
            || derive(&["Focus left"]),
            RowText::two_line,
            rank,
        );
        let shown = &p.shown()[0];
        // "l" of "left" (byte 6) in the primary line; "C" of "Cat" (byte 0) in the secondary.
        assert_eq!(shown.primary, vec![6..7]);
        assert_eq!(shown.secondary, vec![0..1]);
    }

    #[test]
    fn position_and_at_speak_filtered_positions() {
        let mut p: Prepared<u64, String, &'static str> = Prepared::new();
        p.refresh(
            &1,
            &"right".to_string(),
            || derive(&["Focus left", "Focus right"]),
            RowText::two_line,
            rank,
        );
        assert_eq!(p.position(|r| *r == "Focus right"), Some(0));
        assert_eq!(p.position(|r| *r == "Focus left"), None);
        assert_eq!(p.at(1), None);
    }
}
