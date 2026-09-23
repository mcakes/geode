//! Allocation-free slash-separated topic matching for the local adapter.
//! Configuration validation lives in geode-core; this matcher interprets
//! patterns without returning diagnostics.

/// Match slash-separated levels. A whole `*` level matches one level; a final
/// `>` consumes one or more levels. Other text, including a non-final `>`, is
/// literal. No normalization or validation is performed here: split preserves
/// empty levels, while configuration rejects empty pattern levels.
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
