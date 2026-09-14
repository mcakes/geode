//! The footer hints every modal dialog paints, organised by what a key
//! *does* rather than by which dialog wrote the line (user ruling
//! 2026-09-14, interaction-model spec §19).
//!
//! Each dialog used to hand-assemble two rows, "motion" and "action",
//! and the same key landed on different rows depending on who wrote it:
//! the object dialog's edit stage put `space`/`shift+space` beside `j`/`k`,
//! the settings dialog put them on the action row. A trader reading a
//! footer has to know *where to look*, and that only works if the row a
//! key sits on is decided once, by its category, everywhere.
//!
//! So a footer is a flat list of [`Hint`]s, each tagged with the
//! [`HintRow`] it belongs to, and [`rows`] lays them out in a fixed
//! order — move, edit, go — dropping a row with nothing in it. The
//! dialogs only decide *which* hints are live (the mode-honesty rule:
//! name only keys that act right now); the row is not their call.
//!
//! No `gpui` here, in the mould of [`crate::dialogmode`]: the rendering
//! lives in `shell::dialog::hint_rows`, and this module is what a unit
//! test can pin without a window.

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
    /// paint (`escape`'s rung, "back to <object>").
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

/// Lay the hints out as rows, in [`HintRow::ALL`] order, each row
/// keeping its hints in the order they were given and a row with no
/// hints not appearing at all.
pub fn rows(hints: &[Hint]) -> Vec<(HintRow, Vec<&Hint>)> {
    HintRow::ALL
        .into_iter()
        .filter_map(|row| {
            let members: Vec<&Hint> = hints.iter().filter(|h| h.row == row).collect();
            (!members.is_empty()).then_some((row, members))
        })
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

    /// A row nobody put a hint on is not painted — a read-only surface
    /// has no edit row, the naming stage has only a go row — and the
    /// rows that remain keep their fixed order.
    #[test]
    fn an_empty_row_is_dropped_and_the_order_holds() {
        let hints = vec![
            Hint::new(HintRow::Go, &["enter"], "create"),
            Hint::new(HintRow::Go, &["escape"], "cancel"),
        ];
        let laid = rows(&hints);
        assert_eq!(laid.len(), 1);
        assert_eq!(laid[0].0, HintRow::Go);
        assert_eq!(laid[0].1.len(), 2);

        let hints = vec![
            Hint::new(HintRow::Go, &["escape"], "close"),
            Hint::prose(HintRow::Move, "type to filter"),
        ];
        let laid = rows(&hints);
        assert_eq!(
            laid.iter().map(|(row, _)| *row).collect::<Vec<_>>(),
            vec![HintRow::Move, HintRow::Go]
        );
        assert!(laid[0].1[0].keys.is_empty(), "prose carries no chips");
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
