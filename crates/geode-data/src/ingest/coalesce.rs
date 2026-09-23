//! Per-key latest-wins coalescing with a minimum spacing between releases.
//! A new item replaces the pending item for its key without moving the release
//! deadline. The receiver supplies `now` and owns the timer.
//!
//! Only pending items are coalesced; jobs already handed to ingest remain
//! queued. Distinct keys and retained release timestamps have no fixed cap.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// Per-key latest-wins with minimum release spacing. Methods take `now`,
/// so the receiver owns the timer and tests need no clock.
pub struct Coalescer<T> {
    window: Duration,
    /// At most one pending item per key — a later `offer` for a key
    /// already pending replaces it rather than growing a queue.
    pending: HashMap<String, T>,
    last_release: HashMap<String, Instant>,
    /// When each pending key may go. Keyed by `Instant` so `due` can
    /// walk it in order and stop at the first deadline still in the
    /// future, and so `next_deadline` is a single `first_key_value`
    /// lookup rather than a scan of `pending`.
    due_at: BTreeMap<Instant, Vec<String>>,
}

impl<T> Coalescer<T> {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            pending: HashMap::new(),
            last_release: HashMap::new(),
            due_at: BTreeMap::new(),
        }
    }

    /// Offer a document for `key`. Released immediately (returned) when the
    /// window is zero or `now - last_release[key] >= window`; otherwise held,
    /// replacing any pending one, due at `last_release[key] + window`.
    pub fn offer(&mut self, now: Instant, key: String, item: T) -> Option<(String, T)> {
        use std::collections::hash_map::Entry;
        match self.pending.entry(key) {
            Entry::Occupied(mut e) => {
                // Already pending: latest wins, but the due time (set when
                // it first went pending) does not move — re-offering must
                // not let a fast-repeating key push its own release out
                // forever.
                e.insert(item);
                None
            }
            Entry::Vacant(e) => {
                let key = e.key().clone();
                let ready = match self.last_release.get(&key) {
                    None => true,
                    Some(&last) => {
                        self.window.is_zero() || now.saturating_duration_since(last) >= self.window
                    }
                };
                if ready {
                    self.last_release.insert(key.clone(), now);
                    return Some((key, item));
                }
                let due = self.last_release[&key] + self.window;
                e.insert(item);
                self.due_at.entry(due).or_default().push(key);
                None
            }
        }
    }

    /// Everything whose due time is at or before `now`, released.
    pub fn due(&mut self, now: Instant) -> Vec<(String, T)> {
        let mut out = Vec::new();
        let ready_times: Vec<Instant> = self.due_at.range(..=now).map(|(&t, _)| t).collect();
        for t in ready_times {
            let keys = self.due_at.remove(&t).unwrap_or_default();
            for key in keys {
                if let Some(item) = self.pending.remove(&key) {
                    self.last_release.insert(key.clone(), now);
                    out.push((key, item));
                }
            }
        }
        out
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.due_at.keys().next().copied()
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_window_releases_every_offer_immediately() {
        let mut c = Coalescer::new(Duration::ZERO);
        let t0 = Instant::now();
        assert_eq!(c.offer(t0, "SPX".into(), 1), Some(("SPX".into(), 1)));
        assert_eq!(c.offer(t0, "SPX".into(), 2), Some(("SPX".into(), 2)));
        assert_eq!(c.pending(), 0);
        assert_eq!(c.next_deadline(), None);
    }

    #[test]
    fn within_the_window_the_latest_wins_and_is_released_on_the_deadline() {
        let mut c = Coalescer::new(Duration::from_millis(500));
        let t0 = Instant::now();
        assert_eq!(
            c.offer(t0, "SPX".into(), 1),
            Some(("SPX".into(), 1)),
            "first offer for a key goes at once"
        );
        assert_eq!(
            c.offer(t0 + Duration::from_millis(100), "SPX".into(), 2),
            None
        );
        assert_eq!(
            c.offer(t0 + Duration::from_millis(200), "SPX".into(), 3),
            None
        );
        assert_eq!(c.pending(), 1);
        assert_eq!(c.next_deadline(), Some(t0 + Duration::from_millis(500)));
        assert!(c.due(t0 + Duration::from_millis(499)).is_empty());
        assert_eq!(
            c.due(t0 + Duration::from_millis(500)),
            vec![("SPX".into(), 3)]
        );
        assert_eq!(c.pending(), 0);
        // The release restarts the window from the release instant.
        assert_eq!(
            c.offer(t0 + Duration::from_millis(600), "SPX".into(), 4),
            None
        );
        assert_eq!(c.next_deadline(), Some(t0 + Duration::from_millis(1000)));
    }

    #[test]
    fn keys_coalesce_independently() {
        let mut c = Coalescer::new(Duration::from_millis(500));
        let t0 = Instant::now();
        c.offer(t0, "SPX".into(), 1);
        assert_eq!(c.offer(t0, "NDX".into(), 10), Some(("NDX".into(), 10)));
        assert_eq!(
            c.offer(t0 + Duration::from_millis(10), "SPX".into(), 2),
            None
        );
        assert_eq!(
            c.offer(t0 + Duration::from_millis(20), "NDX".into(), 11),
            None
        );
        let mut d = c.due(t0 + Duration::from_millis(500));
        d.sort();
        assert_eq!(d, vec![("NDX".into(), 11), ("SPX".into(), 2)]);
    }

    #[test]
    fn an_offer_after_the_window_elapsed_with_nothing_pending_goes_at_once() {
        let mut c = Coalescer::new(Duration::from_millis(500));
        let t0 = Instant::now();
        c.offer(t0, "SPX".into(), 1);
        assert_eq!(
            c.offer(t0 + Duration::from_secs(2), "SPX".into(), 2),
            Some(("SPX".into(), 2))
        );
    }
}
