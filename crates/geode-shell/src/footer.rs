//! Shared dialog footer hints, grouped by what a key does.
//!
//! Dialogs supply the currently active [`Hint`]s, each tagged with a
//! [`HintRow`]. [`rows`] returns move, edit, and go in fixed order, including
//! empty rows. The painter reserves their height so selection changes do
//! not move the footer or the content below it.
//!
//! This module contains the model and grouping rules; rendering lives in
//! `shell::dialog::hint_rows`.

use std::borrow::Cow;

/// Which footer row a hint belongs to, in the order the rows paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintRow {
    /// The cursor and the viewport: `j`/`k`, `up`/`down`, the scroll
    /// chords, and "type to filter" (the filter narrows what the cursor
    /// moves over).
    Move,
    /// Anything that changes a value or an object: stepping, toggling,
    /// adding, reordering, removing, typing a value, creating, deleting,
    /// reverting, overwriting, rebinding.
    Edit,
    /// Stage and mode changes: opening a row, jumping to a slot, entering
    /// or leaving filter mode, applying or cancelling a field, answering
    /// a confirm, and `escape`'s honest next rung.
    Go,
}

impl HintRow {
    /// Paint order — the one place it is spelled.
    pub const ALL: [HintRow; 3] = [HintRow::Move, HintRow::Edit, HintRow::Go];

    /// The dim leading label a row carries so the eye can find it
    /// without reading it.
    pub fn label(self) -> &'static str {
        match self {
            HintRow::Move => "move",
            HintRow::Edit => "edit",
            HintRow::Go => "go",
        }
    }
}

/// One hint: the keys it names (chip specs, `parse_keystroke`'s
/// grammar), the word that says what they do, and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub row: HintRow,
    /// Zero or more keystroke specs. Empty for a prose-only hint such as
    /// "type to filter".
    pub keys: Vec<&'static str>,
    /// Painted between the first two chips when set — `1 – 9` — so a
    /// range reads as one, not as two keys.
    pub between: Option<&'static str>,
    /// What the keys do. Owned, because a few hints are decided per
    /// paint (`escape`'s rung, `back to <object>`).
    pub word: Cow<'static, str>,
    /// A `debug_selector` for the first chip, so a window test can ask
    /// whether this hint was painted at all.
    pub selector: Option<&'static str>,
}

impl Hint {
    /// A hint with chips.
    pub fn new(row: HintRow, keys: &[&'static str], word: impl Into<Cow<'static, str>>) -> Self {
        Hint {
            row,
            keys: keys.to_vec(),
            between: None,
            word: word.into(),
            selector: None,
        }
    }

    /// A prose-only hint — no chips, just the words.
    pub fn prose(row: HintRow, word: impl Into<Cow<'static, str>>) -> Self {
        Hint::new(row, &[], word)
    }

    /// Two chips painted as a range: `1 – 9`.
    pub fn range(
        row: HintRow,
        from: &'static str,
        to: &'static str,
        word: impl Into<Cow<'static, str>>,
    ) -> Self {
        let mut hint = Hint::new(row, &[from, to], word);
        hint.between = Some("–");
        hint
    }

    /// Tag the first chip with a selector.
    pub fn selector(mut self, selector: &'static str) -> Self {
        self.selector = Some(selector);
        self
    }
}

/// Group hints in [`HintRow::ALL`] order, preserving their order within
/// each row. Return all three rows even when empty: the painter reserves
/// a full line for each so the footer's height stays constant as the
/// selection or available actions change.
pub fn rows(hints: &[Hint]) -> Vec<(HintRow, Vec<&Hint>)> {
    HintRow::ALL
        .into_iter()
        .map(|row| (row, hints.iter().filter(|h| h.row == row).collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row a hint lands on is decided by its category, never by the
    /// order the dialog listed it in: `space` given before `j` still
    /// paints under `j`, on the edit row.
    #[test]
    fn a_hints_category_decides_its_row_not_its_position() {
        let hints = vec![
            Hint::new(HintRow::Edit, &["space"], "change"),
            Hint::new(HintRow::Go, &["escape"], "close"),
            Hint::new(HintRow::Move, &["j", "k"], "move"),
            Hint::new(HintRow::Edit, &["shift+j", "shift+k"], "reorder"),
        ];
        let laid = rows(&hints);
        let shape: Vec<(HintRow, Vec<&str>)> = laid
            .iter()
            .map(|(row, members)| (*row, members.iter().map(|h| h.word.as_ref()).collect()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (HintRow::Move, vec!["move"]),
                (HintRow::Edit, vec!["change", "reorder"]),
                (HintRow::Go, vec!["close"]),
            ]
        );
    }

    /// Empty hint categories retain their fixed row positions. The painter
    /// can reserve those lines even when only the go row contains hints.
    #[test]
    fn an_empty_row_is_kept_empty_and_the_order_holds() {
        let hints = vec![
            Hint::new(HintRow::Go, &["enter"], "create"),
            Hint::new(HintRow::Go, &["escape"], "cancel"),
        ];
        let laid = rows(&hints);
        assert_eq!(
            laid.iter().map(|(row, _)| *row).collect::<Vec<_>>(),
            HintRow::ALL.to_vec(),
            "every row, every time"
        );
        assert!(laid[0].1.is_empty() && laid[1].1.is_empty());
        assert_eq!(laid[2].1.len(), 2);

        let hints = vec![
            Hint::new(HintRow::Go, &["escape"], "close"),
            Hint::prose(HintRow::Move, "type to filter"),
        ];
        let laid = rows(&hints);
        assert_eq!(laid.len(), 3);
        assert!(laid[0].1[0].keys.is_empty(), "prose carries no chips");
        assert!(laid[1].1.is_empty(), "no edit hints, but the row is there");
    }

    /// The constructors spell the three shapes a hint takes.
    #[test]
    fn constructors_shape_the_hint() {
        let range = Hint::range(HintRow::Go, "1", "9", "open slot").selector("x");
        assert_eq!(range.keys, vec!["1", "9"]);
        assert_eq!(range.between, Some("–"));
        assert_eq!(range.selector, Some("x"));
        let plain = Hint::new(HintRow::Edit, &["i"], String::from("type a value"));
        assert_eq!(plain.between, None);
        assert_eq!(plain.word, "type a value");
        assert_eq!(HintRow::Move.label(), "move");
        assert_eq!(HintRow::Edit.label(), "edit");
        assert_eq!(HintRow::Go.label(), "go");
    }
}
