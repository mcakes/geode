//! One recovery window: the span in which a subscription's receiver judges
//! the transport's replies to a recovery request. Pure: the caller supplies
//! both clocks.
//!
//! A reply loses to a NOTIFY for the same key received at or after the
//! window's `started_at` (rule 1): the subscription was live before the
//! request went out, so such a NOTIFY is at least as new as the reply. The
//! caller passes when that key's newest NOTIFY arrived; the window compares
//! times rather than keeping a per-window set, so a NOTIFY the receiver
//! handled before it noticed a reconnect still counts. Ordering never uses
//! the reply's own receive time: a reply snapshotted before a newer NOTIFY
//! can arrive after it.
//!
//! A reply at or after the deadline is dropped and counted by the caller:
//! the window that could judge it is gone.

use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Added to the source's `recover_timeout` for replies already in flight
/// when the transport's own timeout ends.
pub(crate) const RECOVERY_GRACE: Duration = Duration::from_secs(1);

/// A topic recorded this run is recorded again after this long, so a
/// long-running process keeps its topics' last receive time current and
/// they are never pruned while live.
pub(crate) const RERECORD_AFTER: Duration = Duration::from_secs(24 * 3600);

/// The fallback deadline span when `timeout + RECOVERY_GRACE` overflows
/// `Instant`: far enough to mean "until every topic answers".
const FAR: Duration = Duration::from_secs(365 * 24 * 3600);

/// How many unanswered topics a partial report names.
const SAMPLE: usize = 5;

pub(crate) struct RecoveryWindow {
    /// Replies for a key notified at or after this instant are dropped.
    started_at: DateTime<Utc>,
    deadline: Instant,
    asked: usize,
    answered: HashSet<String>,
}

/// What the receiver does with one recovery reply.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplyVerdict {
    /// Offer it like a NOTIFY, marked recovered.
    Publish,
    /// A NOTIFY for its key arrived since the window started.
    DropNotified,
    /// No window could judge it: the deadline passed, or none was open.
    DropLate,
}

/// A finished window's outcome, as the `<source>:recovery` load lane reads it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RecoveryReport {
    AllAnswered,
    /// Some topics answered. Retired instruments stop answering, so this is
    /// not a failure; `sample` names up to five of the unanswered.
    Partial {
        unanswered: usize,
        sample: Vec<String>,
    },
    /// Not one asked topic answered.
    NoReplies {
        asked: usize,
    },
}

impl RecoveryWindow {
    /// Open a window over `asked`, before the request goes out, so no reply
    /// can arrive with nothing to judge it. Its deadline is
    /// `timeout + RECOVERY_GRACE` after `now`.
    pub(crate) fn start(
        now: Instant,
        started_at: DateTime<Utc>,
        timeout: Duration,
        asked: &[String],
    ) -> Self {
        let deadline = timeout
            .checked_add(RECOVERY_GRACE)
            .and_then(|span| now.checked_add(span))
            .unwrap_or_else(|| now + FAR);
        RecoveryWindow {
            started_at,
            deadline,
            asked: asked.len(),
            answered: HashSet::new(),
        }
    }

    /// Judge one reply on `topic`. `last_notify`: when a NOTIFY for this
    /// reply's key last arrived. Any reply answers its topic, even one
    /// dropped, because the transport did respond.
    pub(crate) fn on_reply(
        &mut self,
        now: Instant,
        topic: &str,
        last_notify: Option<DateTime<Utc>>,
    ) -> ReplyVerdict {
        if !self.answered.contains(topic) {
            self.answered.insert(topic.to_string());
        }
        if now >= self.deadline {
            ReplyVerdict::DropLate
        } else if last_notify.is_some_and(|t| t >= self.started_at) {
            ReplyVerdict::DropNotified
        } else {
            ReplyVerdict::Publish
        }
    }

    /// The deadline passed, or every asked topic answered.
    pub(crate) fn done(&self, now: Instant) -> bool {
        now >= self.deadline || self.answered.len() >= self.asked
    }

    /// When the receiver must next look at this window.
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// The outcome over `asked`, the same topics the window started with.
    pub(crate) fn report(&self, asked: &[String]) -> RecoveryReport {
        let unanswered: Vec<&String> = asked
            .iter()
            .filter(|t| !self.answered.contains(t.as_str()))
            .collect();
        if unanswered.is_empty() {
            RecoveryReport::AllAnswered
        } else if unanswered.len() == asked.len() {
            RecoveryReport::NoReplies { asked: asked.len() }
        } else {
            RecoveryReport::Partial {
                unanswered: unanswered.len(),
                sample: unanswered.into_iter().take(SAMPLE).cloned().collect(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(s: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(s, 0).unwrap()
    }
    fn topics(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_reply_for_a_key_not_notified_since_the_start_publishes() {
        let t0 = Instant::now();
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &topics(&["a"]));
        assert!(matches!(
            w.on_reply(t0, "a", Some(utc(99))),
            ReplyVerdict::Publish
        ));
        assert!(matches!(w.on_reply(t0, "a", None), ReplyVerdict::Publish));
    }

    #[test]
    fn a_reply_for_a_key_notified_since_subscribe_is_dropped() {
        let t0 = Instant::now();
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &topics(&["a"]));
        assert!(matches!(
            w.on_reply(t0, "a", Some(utc(100))),
            ReplyVerdict::DropNotified
        ));
        assert!(matches!(
            w.on_reply(t0, "a", Some(utc(150))),
            ReplyVerdict::DropNotified
        ));
    }

    #[test]
    fn a_reply_after_the_window_is_dropped() {
        let t0 = Instant::now();
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &topics(&["a"]));
        let late = t0 + Duration::from_secs(10) + RECOVERY_GRACE;
        assert!(matches!(
            w.on_reply(late, "a", None),
            ReplyVerdict::DropLate
        ));
    }

    #[test]
    fn the_window_is_done_when_every_topic_answers_or_the_deadline_passes() {
        let t0 = Instant::now();
        let asked = topics(&["a", "b"]);
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &asked);
        w.on_reply(t0, "a", None);
        assert!(!w.done(t0));
        // A dropped-as-notified reply still answered its topic.
        w.on_reply(t0, "b", Some(utc(200)));
        assert!(w.done(t0));
        assert!(matches!(w.report(&asked), RecoveryReport::AllAnswered));
    }

    #[test]
    fn reports_distinguish_partial_from_no_replies() {
        let t0 = Instant::now();
        let asked = topics(&["a", "b", "c"]);
        let end = t0 + Duration::from_secs(12);
        let mut w = RecoveryWindow::start(t0, utc(0), Duration::from_secs(10), &asked);
        w.on_reply(t0, "a", None);
        assert!(w.done(end));
        assert!(matches!(
            w.report(&asked),
            RecoveryReport::Partial { unanswered: 2, .. }
        ));
        let w = RecoveryWindow::start(t0, utc(0), Duration::from_secs(10), &asked);
        assert!(matches!(
            w.report(&asked),
            RecoveryReport::NoReplies { asked: 3 }
        ));
    }
}
