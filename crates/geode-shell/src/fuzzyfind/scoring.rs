//! Reusable score-only matching for a single ASCII word, with the palette's
//! exact scoring rules. Alignment/backtracking is deferred to visible rows.
use crate::palette;

pub(super) struct Scorer<'a> {
    query: &'a str,
    words: Vec<&'a str>,
    ends: Vec<u32>,
    within: Vec<u32>,
}

impl<'a> Scorer<'a> {
    pub(super) fn new(query: &'a str) -> Self {
        Self {
            query,
            words: query.split_whitespace().collect(),
            ends: Vec::new(),
            within: Vec::new(),
        }
    }

    pub(super) fn score(&mut self, candidate: &str) -> Option<u32> {
        if self.words.len() == 1 && self.query.is_ascii() && candidate.starts_with(self.query) {
            // A consecutive prefix attains the maximum score: a gap loses a
            // run bonus, which exceeds any new word-boundary bonus it can gain.
            return Some(
                self.query.len() as u32
                    + palette::PREFIX_BONUS
                    + (self.query.len() as u32 - 1) * palette::RUN_BONUS
                    + self.query.as_bytes()[..self.query.len() - 1]
                        .iter()
                        .filter(|&&ch| matches!(ch, b' ' | b':' | b'_' | b'-'))
                        .count() as u32
                        * palette::WORD_START_BONUS,
            );
        }
        // Cheap necessary condition, including for unordered multi-word queries.
        // Matching bytes is safe as a rejection filter for Unicode too.
        for word in &self.words {
            let mut remaining = word.as_bytes();
            for &byte in candidate.as_bytes() {
                if remaining.first() == Some(&byte) {
                    remaining = &remaining[1..];
                    if remaining.is_empty() {
                        break;
                    }
                }
            }
            if !remaining.is_empty() {
                return None;
            }
        }
        if self.words.len() != 1 || !self.query.is_ascii() {
            return palette::fuzzy_match_lowered(self.query, candidate, usize::MAX)
                .map(|(score, _)| score);
        }

        // An ASCII query can match only ASCII bytes. Unicode bytes in the
        // candidate act as gaps; they cannot change a boundary or run bonus.
        let c = candidate.as_bytes();
        self.ends.resize(c.len(), 0);
        self.within.resize(c.len(), 0);
        let mut score = 0;
        for (i, &q) in self.query.as_bytes().iter().enumerate() {
            let (mut prev_end, mut prev_within, mut best) = (0, 0, 0);
            for (j, &ch) in c.iter().enumerate() {
                let base = 1
                    + if j == 0 { palette::PREFIX_BONUS } else { 0 }
                    + if j > 0 && matches!(c[j - 1], b' ' | b':' | b'_' | b'-') {
                        palette::WORD_START_BONUS
                    } else {
                        0
                    };
                let cell = if ch != q || (i > 0 && prev_within == 0) {
                    0
                } else if i == 0 {
                    base
                } else {
                    base + prev_within.max(if prev_end > 0 {
                        prev_end + palette::RUN_BONUS
                    } else {
                        0
                    })
                };
                prev_end = self.ends[j];
                prev_within = self.within[j];
                self.ends[j] = cell;
                best = best.max(cell);
                self.within[j] = best;
            }
            score = best;
        }
        (score > 0).then_some(score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scorer_preserves_palette_scores_with_reused_storage() {
        let mut candidates = vec![String::new()];
        let mut level = vec![String::new()];
        for _ in 0..5 {
            level = level
                .iter()
                .flat_map(|prefix| ['a', 'b', '_', 'é'].map(|ch| format!("{prefix}{ch}")))
                .collect();
            candidates.extend(level.clone());
        }
        candidates.extend([
            "equities › us › book a".into(),
            "contract 1499999 equities › us › book a".into(),
            "a-b:a_b ab a_b".into(),
            "i\u{307}stanbul".into(),
        ]);
        let mut queries = candidates
            .iter()
            .filter(|s| s.chars().count() <= 3)
            .cloned()
            .collect::<Vec<_>>();
        queries.extend(
            [
                "a b",
                "b a",
                "a a",
                "a_b",
                "a-b",
                "b:a",
                "contract",
                "contract 1499",
                "book equities",
                "i\u{307}",
            ]
            .map(str::to_string),
        );
        for query in queries {
            let mut scorer = Scorer::new(&query);
            // Reverse order also exercises scratch buffers shrinking.
            for candidate in candidates.iter().chain(candidates.iter().rev()) {
                assert_eq!(
                    scorer.score(candidate),
                    palette::fuzzy_match_lowered(&query, candidate, usize::MAX)
                        .map(|(score, _)| score),
                    "query {query:?}, candidate {candidate:?}"
                );
            }
        }
    }
}
