//! Link groups: the vocabulary a tile and the shell share. A group carries a
//! scope and a board of draft documents; a tile may follow one group and
//! emit into one. Pure: the frame holds the state, modules only answer
//! [`Emission`]s.

use std::sync::Arc;

use crate::document::DocumentRows;
use crate::scope::Scope;

/// The column whose single value a link group's scope is named by: the
/// underlying an emitter's cursor rests on. Emitters build their scope with
/// [`underlying_scope`] and readers take the name back with
/// [`underlying_of`], so the two cannot disagree on the column.
pub const UNDERLYING: &str = "underlying_ref";

/// The scope an emitter posts for a cursor on `underlying`: that one value
/// of [`UNDERLYING`] and nothing else.
pub fn underlying_scope(underlying: &str) -> Scope {
    Scope::one(UNDERLYING, underlying)
}

/// The one underlying `scope` names: its sole value for [`UNDERLYING`].
/// `None` when the scope selects no underlying or several, or is
/// impossible.
pub fn underlying_of(scope: &Scope) -> Option<&str> {
    scope.sole(UNDERLYING)
}

/// One of the four fixed link groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    A,
    B,
    C,
    D,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::A, Group::B, Group::C, Group::D];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The letter a trader reads.
    pub fn letter(self) -> &'static str {
        ["A", "B", "C", "D"][self.index()]
    }

    /// The session file's spelling.
    pub fn as_str(self) -> &'static str {
        ["a", "b", "c", "d"][self.index()]
    }

    pub fn parse(s: &str) -> Option<Group> {
        Group::ALL
            .into_iter()
            .find(|g| g.as_str().eq_ignore_ascii_case(s))
    }
}

/// The groups one tile is in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Membership {
    pub follow: Option<Group>,
    pub emit: Option<Group>,
}

impl Membership {
    pub fn is_empty(self) -> bool {
        self.follow.is_none() && self.emit.is_none()
    }
}

/// Where a posted draft stands against what is published. A follower
/// cannot tell these apart from the rows: a `Behind` draft is the OLD
/// base generation plus edits while a newer document is published, and a
/// `Sent` one is what was uploaded until its echo lands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DraftMark {
    #[default]
    Editing,
    Behind,
    Sent,
}

impl DraftMark {
    /// The word a follower shows beside the draft; `None` for a live edit.
    pub fn label(self) -> Option<&'static str> {
        match self {
            DraftMark::Editing => None,
            DraftMark::Behind => Some("behind"),
            DraftMark::Sent => Some("sent"),
        }
    }
}

/// One draft document an emitter posts on its group's board, with where
/// that draft stands.
#[derive(Debug, Clone)]
pub struct BoardEntry {
    pub dataset: String,
    pub key: Vec<String>,
    pub rows: Arc<DocumentRows>,
    pub mark: DraftMark,
}

/// Equal when it is the same document under the same key and mark: the
/// rows compare by allocation. An emitter allocates rows only when its
/// draft changed, so this is exact and costs a pointer compare instead of
/// a document compare on every pull. The mark compares too: the same rows
/// falling behind or being sent is a change a follower must hear.
impl PartialEq for BoardEntry {
    fn eq(&self, other: &Self) -> bool {
        self.dataset == other.dataset
            && self.key == other.key
            && Arc::ptr_eq(&self.rows, &other.rows)
            && self.mark == other.mark
    }
}

/// What a tile posts into the group it emits into. `scope: None` leaves the
/// group's scope as it is (the cursor names no single value).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Emission {
    pub scope: Option<Scope>,
    pub board: Vec<BoardEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentRows;

    fn rows(key: &str) -> Arc<DocumentRows> {
        Arc::new(DocumentRows {
            key: vec![key.to_string()],
            attributes: Vec::new(),
            axes: Vec::new(),
            values: Vec::new(),
        })
    }

    #[test]
    fn a_group_round_trips_its_session_spelling_and_reads_either_case() {
        for g in Group::ALL {
            assert_eq!(Group::parse(g.as_str()), Some(g));
            assert_eq!(Group::parse(g.letter()), Some(g));
            assert_eq!(Group::ALL[g.index()], g);
        }
        assert_eq!(Group::A.as_str(), "a");
        assert_eq!(Group::D.letter(), "D");
        assert_eq!(Group::parse("e"), None);
        assert_eq!(Group::parse(""), None);
        assert_eq!(Group::parse("ab"), None, "a letter, not a prefix");
        assert_eq!(Group::parse(" a"), None, "and not trimmed");
    }

    /// What an emitter posts for an underlying reads back as that
    /// underlying, on the one column both sides name.
    #[test]
    fn an_underlying_scope_names_its_underlying_and_nothing_else_does() {
        use crate::scope::DimensionSelection;
        let scope = underlying_scope("SPX.Z");
        assert_eq!(scope, Scope::one("underlying_ref", "SPX.Z"));
        assert_eq!(UNDERLYING, "underlying_ref");
        assert_eq!(underlying_of(&scope), Some("SPX.Z"));
        assert_eq!(underlying_of(&Scope::default()), None);
        assert_eq!(underlying_of(&Scope::one("book", "SPX.Z")), None);
        let several = Scope {
            dimensions: vec![DimensionSelection {
                column: UNDERLYING.into(),
                values: vec!["SPX.Z".into(), "NDX".into()],
            }],
            ..Scope::default()
        };
        assert_eq!(underlying_of(&several), None, "several name no single one");
        let impossible = Scope {
            impossible: true,
            ..underlying_scope("SPX.Z")
        };
        assert_eq!(underlying_of(&impossible), None);
    }

    #[test]
    fn a_board_entry_is_equal_only_to_the_same_allocation() {
        let shared = rows("SPX.Z");
        let entry = |rows: &Arc<DocumentRows>| BoardEntry {
            dataset: "cvi_params".into(),
            key: vec!["SPX.Z".into()],
            rows: Arc::clone(rows),
            mark: DraftMark::Editing,
        };
        assert_eq!(entry(&shared), entry(&shared));
        // Equal content in a fresh allocation is a new draft: emitters
        // allocate only when the draft changed, so a pointer is the cheap
        // and exact "unchanged" test.
        assert_ne!(entry(&shared), entry(&rows("SPX.Z")));
        let mut other = entry(&shared);
        other.dataset = "dividend_schedule".into();
        assert_ne!(entry(&shared), other);
        // The same rows under another key are another document.
        let mut rekeyed = entry(&shared);
        rekeyed.key = vec!["NDX".into()];
        assert_ne!(entry(&shared), rekeyed);
    }

    #[test]
    fn a_board_entry_differs_by_its_mark() {
        let shared = rows("SPX.Z");
        let entry = |mark: DraftMark| BoardEntry {
            dataset: "cvi_params".into(),
            key: vec!["SPX.Z".into()],
            rows: Arc::clone(&shared),
            mark,
        };
        assert_eq!(entry(DraftMark::Behind), entry(DraftMark::Behind));
        // The same rows held behind a newer document are not the live
        // edit a follower painted: the mark alone is a change.
        assert_ne!(entry(DraftMark::Editing), entry(DraftMark::Behind));
        assert_ne!(entry(DraftMark::Behind), entry(DraftMark::Sent));
        assert_eq!(DraftMark::default(), DraftMark::Editing);
        assert_eq!(DraftMark::Editing.label(), None);
        assert_eq!(DraftMark::Behind.label(), Some("behind"));
        assert_eq!(DraftMark::Sent.label(), Some("sent"));
    }

    #[test]
    fn an_emission_compares_scope_and_board() {
        let r = rows("SPX.Z");
        let e = |u: Option<&str>, board: bool| Emission {
            scope: u.map(|u| Scope::one("underlying_ref", u)),
            board: if board {
                vec![BoardEntry {
                    dataset: "cvi_params".into(),
                    key: vec!["SPX.Z".into()],
                    rows: Arc::clone(&r),
                    mark: DraftMark::Editing,
                }]
            } else {
                Vec::new()
            },
        };
        assert_eq!(e(Some("SPX.Z"), true), e(Some("SPX.Z"), true));
        assert_ne!(e(Some("SPX.Z"), true), e(Some("NDX"), true));
        assert_ne!(e(Some("SPX.Z"), true), e(Some("SPX.Z"), false));
        assert_eq!(Emission::default(), e(None, false));
    }
}
