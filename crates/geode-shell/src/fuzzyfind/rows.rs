//! The empty query is an implicit range, so opening search never allocates an
//! identity permutation for every loaded row. Only reordered results need storage.
use std::sync::Arc;

#[derive(Clone)]
pub(super) enum Rows {
    All(usize),
    Ranked(Arc<Vec<usize>>),
}

impl Rows {
    pub fn len(&self) -> usize {
        match self {
            Self::All(len) => *len,
            Self::Ranked(rows) => rows.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, position: usize) -> Option<usize> {
        match self {
            Self::All(len) => (position < *len).then_some(position),
            Self::Ranked(rows) => rows.get(position).copied(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).map(|position| self.get(position).unwrap())
    }
}
