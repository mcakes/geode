//! Topic pattern matching — Solace's syntax, in the one place the whole
//! crate (and, later, the real vendor adapter) shares it.
//!
//! `geode-data` never opens a socket, so nothing here talks to a broker:
//! this is the *grammar* a `[sources.<name>] topics` entry is written in,
//! matched locally against the topic a `Message` arrived on. It is
//! Solace's because the adapter this contract is designed for is
//! Solace's, and a pattern a trader writes in `sources.toml` has to mean
//! the same thing whether the messages came from the broker or from
//! `ChannelAdapter` in a test or the demo (global constraint: one place,
//! tested).

/// Whether `topic` matches `pattern`.
///
/// Levels are separated by `/`. A `*` level matches exactly one level. A
/// `>` level, and only as the FINAL level, matches one or more trailing
/// levels — never zero, which is why `marketdata/cvi/>` does not match
/// the parent topic `marketdata/cvi`. Every other level is a literal.
///
/// A `>` anywhere but the last level is not wildcard syntax at all, so it
/// is treated as the literal level `>` — which no ordinary topic carries,
/// so such a pattern matches nothing rather than silently behaving like
/// some other wildcard. Refusing it at config-load time would be the
/// alternative; it is not done here because this function is the pure
/// matcher and has nowhere to put a diagnostic.
///
/// Allocation-free: both sides are walked as `split` iterators, so a
/// per-message match over a handful of patterns costs no heap at all
/// (PHILOSOPHY §6 — this runs on the dispatcher for every message).
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    let mut patterns = pattern.split('/').peekable();
    let mut levels = topic.split('/');
    while let Some(pat) = patterns.next() {
        let last = patterns.peek().is_none();
        let Some(level) = levels.next() else {
            // The topic ran out of levels while the pattern has one left.
            // A trailing `>` is NOT satisfied by zero levels, so there is
            // no pattern left that could match here.
            return false;
        };
        let matched = match pat {
            // One level consumed above, and `>` is last, so every
            // remaining level of the topic is swallowed with it.
            ">" if last => return true,
            "*" => true,
            literal => literal == level,
        };
        if !matched {
            return false;
        }
    }
    // The pattern is exhausted: a match only if the topic is too, since a
    // literal pattern must not match a deeper topic (that is what `>` is
    // for).
    levels.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_patterns_follow_solace_rules() {
        assert!(topic_matches("marketdata/cvi/>", "marketdata/cvi/SPX.Z"));
        assert!(topic_matches(
            "marketdata/cvi/>",
            "marketdata/cvi/SPX.Z/extra"
        ));
        assert!(
            !topic_matches("marketdata/cvi/>", "marketdata/cvi"),
            "> needs at least one level"
        );
        assert!(topic_matches("marketdata/*/SPX.Z", "marketdata/cvi/SPX.Z"));
        assert!(!topic_matches(
            "marketdata/*/SPX.Z",
            "marketdata/cvi/x/SPX.Z"
        ));
        assert!(topic_matches(
            "marketdata/cvi/SPX.Z",
            "marketdata/cvi/SPX.Z"
        ));
        assert!(!topic_matches(
            "marketdata/cvi/SPX.Z",
            "marketdata/cvi/NDX.Z"
        ));
        assert!(
            !topic_matches("marketdata/>/cvi", "marketdata/x/cvi"),
            "> is only legal as the last level"
        );
    }
}
