//! `/` under both find styles (Phase 3 spec §6.1, §2.2). The shell owns
//! the input; this decides what typing into it does, over the tree
//! column's text of each visible row. Vim jumps, fzf narrows.

use geode_shell::vimfind::{FindDirection, FindStyle, filter_matches, find_match};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindState {
    pub style: FindStyle,
    /// Where the cursor was when `/` opened; `escape` returns here.
    pub origin: usize,
    pub committed: Option<String>,
    /// fzf: the visible indices that match, in row order.
    pub narrowed: Option<Vec<usize>>,
}

impl FindState {
    pub fn begin(style: FindStyle, cursor_row: usize) -> FindState {
        FindState {
            style,
            origin: cursor_row,
            committed: None,
            narrowed: None,
        }
    }

    /// The query changed. Vim: the visible row to move the cursor to, if
    /// any; fzf: the index *into the narrowed list* (0 when anything
    /// matches), and `narrowed` is updated.
    pub fn changed(&mut self, texts: &[String], query: &str) -> Option<usize> {
        match self.style {
            FindStyle::Vim => find_match(texts, self.origin, FindDirection::Forward, query),
            FindStyle::Fzf => {
                let matches = filter_matches(texts, query);
                let any = !matches.is_empty();
                self.narrowed = Some(matches);
                any.then_some(0)
            }
        }
    }

    pub fn committed(&mut self, query: &str) {
        if !query.is_empty() {
            self.committed = Some(query.to_string());
        }
    }

    /// `escape`: the origin row; fzf's narrowing is dropped.
    pub fn cancelled(&mut self) -> usize {
        self.narrowed = None;
        self.origin
    }

    /// `n`/`N`, counted. Vim-style only; fzf has every visible row
    /// matching already.
    pub fn repeat(
        &self,
        texts: &[String],
        from: usize,
        dir: FindDirection,
        count: Option<u32>,
    ) -> Option<usize> {
        if self.style != FindStyle::Vim || texts.is_empty() {
            return None;
        }
        let query = self.committed.as_deref()?;
        let mut at = from;
        for _ in 0..count.unwrap_or(1).max(1) {
            let start = match dir {
                FindDirection::Forward => (at + 1) % texts.len(),
                FindDirection::Backward => (at + texts.len() - 1) % texts.len(),
            };
            at = find_match(texts, start, dir, query)?;
        }
        Some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::vimfind::{FindDirection, FindStyle};

    fn texts() -> Vec<String> {
        ["Total", "L1", "SPX", "NDX", "SPX"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn vim_style_jumps_as_typed_commits_and_repeats() {
        let mut f = FindState::begin(FindStyle::Vim, 0);
        assert_eq!(
            f.changed(&texts(), "sp"),
            Some(2),
            "first match at or after the origin"
        );
        assert_eq!(
            f.changed(&texts(), "spq"),
            None,
            "no match: the caller keeps the cursor"
        );
        assert_eq!(f.narrowed, None, "vim never narrows");
        f.committed("sp");
        assert_eq!(f.repeat(&texts(), 2, FindDirection::Forward, None), Some(4));
        assert_eq!(
            f.repeat(&texts(), 4, FindDirection::Forward, None),
            Some(2),
            "wraps"
        );
        assert_eq!(
            f.repeat(&texts(), 4, FindDirection::Backward, None),
            Some(2)
        );
        assert_eq!(
            f.repeat(&texts(), 0, FindDirection::Forward, Some(2)),
            Some(4),
            "3n is the third match on: counted"
        );
        assert_eq!(
            FindState::begin(FindStyle::Vim, 0).repeat(&texts(), 0, FindDirection::Forward, None),
            None,
            "nothing committed"
        );
        assert_eq!(f.cancelled(), 0, "escape returns the origin");
    }

    #[test]
    fn fzf_style_narrows_as_typed_and_restores_on_cancel() {
        let mut f = FindState::begin(FindStyle::Fzf, 3);
        assert_eq!(
            f.changed(&texts(), "x"),
            Some(0),
            "the cursor sits on the first narrowed row"
        );
        assert_eq!(f.narrowed, Some(vec![2, 3, 4]));
        f.committed("x");
        assert_eq!(
            f.narrowed,
            Some(vec![2, 3, 4]),
            "Enter keeps the narrowed list"
        );
        assert_eq!(f.cancelled(), 3);
        assert_eq!(f.narrowed, None, "escape restores");
        assert_eq!(
            f.repeat(&texts(), 0, FindDirection::Forward, None),
            None,
            "n is vim-style only"
        );
    }
}
