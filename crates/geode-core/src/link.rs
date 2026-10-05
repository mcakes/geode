//! Link groups: the vocabulary a tile and the shell share. A group carries a
//! scope and a board of draft documents; a tile may follow one group and
//! emit into one. Pure: the frame holds the state, modules only answer
//! [`Emission`]s, which the shell composes with [`compose`] into a
//! [`Posting`].

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

/// Where an emitter's cursor stands, as the shell composes it into a
/// group's scope.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum CursorScope {
    /// No snapshot, or no cursor row: the group's scope is left as it is.
    #[default]
    Nothing,
    /// The cursor row's grouping path, one value per level, plus a leaf
    /// row's own single-valued dimensions. Empty on a total row, which
    /// posts the emitter's base alone.
    Path(Scope),
    /// The path passes through NULL (or an empty value) in this column. A
    /// scope cannot say IS NULL, and leaving the column out would widen
    /// every follower to all its values, so nothing is posted.
    NullIn(String),
}

/// What a tile answers when the shell pulls it: the parts the shell
/// composes into a group's scope, and its board. The tile's own layer and
/// `:unscoped` flag live in the module, so the module reports them; the
/// shell, which knows the tile's lane and groups, picks the base.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Emission {
    pub cursor: CursorScope,
    /// The tile's own `:filter` layer; empty for a tile with none.
    pub layer: Scope,
    /// The tile ignores its frame scope: its base is empty.
    pub unscoped: bool,
    pub board: Vec<BoardEntry>,
}

/// What the shell posts into a group for one emitter. `scope: None`
/// leaves the group's scope as it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Posting {
    pub scope: Option<Scope>,
    pub board: Vec<BoardEntry>,
}

/// A scope selecting exactly these values, one column each, in order: the
/// form a cursor path takes.
pub fn path_scope(pairs: &[(String, String)]) -> Scope {
    Scope {
        dimensions: pairs
            .iter()
            .map(|(column, value)| crate::scope::DimensionSelection {
                column: column.clone(),
                values: vec![value.clone()],
            })
            .collect(),
        ..Scope::default()
    }
}

/// Compose an emitter's parts over `base` (the scope the shell chose for
/// it): base, then its layer when `include_layer`, then its cursor path.
/// An `unscoped` emitter's base is empty, matching what it shows. The
/// second value names the NULL column of a refused path.
pub fn compose(emission: Emission, base: &Scope, include_layer: bool) -> (Posting, Option<String>) {
    let Emission {
        cursor,
        layer,
        unscoped,
        board,
    } = emission;
    let (scope, refused) = match cursor {
        CursorScope::Nothing => (None, None),
        CursorScope::NullIn(column) => (None, Some(column)),
        CursorScope::Path(path) => {
            let empty = Scope::default();
            let base = if unscoped { &empty } else { base };
            let layered = if include_layer {
                base.and_then(&layer)
            } else {
                base.clone()
            };
            (Some(layered.and_then(&path)), None)
        }
    };
    (Posting { scope, board }, refused)
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
    fn a_posting_compares_scope_and_board() {
        let r = rows("SPX.Z");
        let e = |u: Option<&str>, board: bool| Posting {
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
        assert_eq!(Posting::default(), e(None, false));
    }

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(c, v)| (c.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_path_scope_selects_one_value_per_column_in_order() {
        let s = path_scope(&pairs(&[("book", "A"), ("model_code", "ABC")]));
        assert_eq!(s.sole("book"), Some("A"));
        assert_eq!(s.sole("model_code"), Some("ABC"));
        assert_eq!(s.dimensions.len(), 2);
        assert!(path_scope(&[]).is_empty());
    }

    #[test]
    fn compose_puts_base_then_layer_then_path() {
        let base = Scope::one("book", "A");
        let layer = Scope::one("region", "EU");
        let path = path_scope(&pairs(&[("model_code", "ABC")]));
        let e = Emission {
            cursor: CursorScope::Path(path),
            layer,
            ..Default::default()
        };
        let (posting, refused) = compose(e, &base, true);
        let s = posting.scope.expect("a path posts a scope");
        assert_eq!(s.sole("book"), Some("A"));
        assert_eq!(s.sole("region"), Some("EU"));
        assert_eq!(s.sole("model_code"), Some("ABC"));
        assert_eq!(refused, None);
    }

    #[test]
    fn compose_leaves_the_layer_out_when_the_setting_is_off() {
        let e = Emission {
            cursor: CursorScope::Path(Scope::default()),
            layer: Scope::one("region", "EU"),
            ..Default::default()
        };
        let (posting, _) = compose(e, &Scope::one("book", "A"), false);
        let s = posting.scope.unwrap();
        assert_eq!(s.sole("book"), Some("A"));
        assert_eq!(s.sole("region"), None);
    }

    #[test]
    fn an_unscoped_emitter_composes_on_an_empty_base() {
        let e = Emission {
            cursor: CursorScope::Path(Scope::one("model_code", "ABC")),
            unscoped: true,
            ..Default::default()
        };
        let (posting, _) = compose(e, &Scope::one("book", "A"), true);
        let s = posting.scope.unwrap();
        assert_eq!(s.sole("book"), None);
        assert_eq!(s.sole("model_code"), Some("ABC"));
    }

    #[test]
    fn a_total_row_posts_the_base() {
        let e = Emission {
            cursor: CursorScope::Path(Scope::default()),
            ..Default::default()
        };
        let (posting, _) = compose(e, &Scope::one("book", "A"), true);
        assert_eq!(posting.scope, Some(Scope::one("book", "A")));
    }

    #[test]
    fn nothing_and_a_null_path_post_no_scope_but_keep_the_board() {
        let entry = BoardEntry {
            dataset: "d".into(),
            key: vec!["SPX".into()],
            rows: rows("SPX"),
            mark: DraftMark::Editing,
        };
        let e = Emission {
            board: vec![entry.clone()],
            ..Default::default()
        };
        let (posting, refused) = compose(e, &Scope::one("book", "A"), true);
        assert_eq!(posting.scope, None);
        assert_eq!(posting.board, vec![entry.clone()]);
        assert_eq!(refused, None);

        let e = Emission {
            cursor: CursorScope::NullIn("book".into()),
            board: vec![entry.clone()],
            ..Default::default()
        };
        let (posting, refused) = compose(e, &Scope::default(), true);
        assert_eq!(posting.scope, None);
        assert_eq!(posting.board, vec![entry]);
        assert_eq!(refused.as_deref(), Some("book"));
    }
}
