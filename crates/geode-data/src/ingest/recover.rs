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
//!
//! The window closes early on replies only. A NOTIFY on an asked topic
//! received at or after `started_at` covers that topic in the report, so a
//! feed whose GET side never answers but whose NOTIFYs show it live is not
//! reported as "no replies".

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
    /// The topics the request named. Only these count as answered, so a
    /// reply on any other topic can neither close the window early nor
    /// change its report.
    asked: HashSet<String>,
    /// The subset of `asked` some reply has arrived on.
    answered: HashSet<String>,
    /// The subset of `asked` a NOTIFY received at or after `started_at`
    /// arrived on. Covers a topic in the report; never closes the window.
    notified: HashSet<String>,
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
    /// Every asked topic answered or was notified since the start.
    AllAnswered,
    /// Some topics answered. Retired instruments stop answering, so this is
    /// not a failure; `unanswered` counts the topics neither answered nor
    /// notified, and `sample` names up to five of them in sorted order.
    Partial {
        unanswered: usize,
        sample: Vec<String>,
    },
    /// Not one asked topic answered, and `asked` of them were not notified
    /// since the start either.
    NoReplies { asked: usize },
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
            asked: asked.iter().cloned().collect(),
            answered: HashSet::new(),
            notified: HashSet::new(),
        }
    }

    /// Judge one reply on `topic`. `last_notify`: when a NOTIFY for this
    /// reply's key last arrived. A reply on an asked topic answers it, even
    /// one dropped, because the transport did respond.
    pub(crate) fn on_reply(
        &mut self,
        now: Instant,
        topic: &str,
        last_notify: Option<DateTime<Utc>>,
    ) -> ReplyVerdict {
        if self.asked.contains(topic) && !self.answered.contains(topic) {
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
        now >= self.deadline || self.answered.len() >= self.asked.len()
    }

    /// When the receiver must next look at this window.
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// A NOTIFY on `topic` received at `received`. An asked topic notified
    /// at or after the window's start is live, so it counts as covered in
    /// the report. A NOTIFY from before the start (one queued across the
    /// outage) proves nothing about the reconnected feed and does not
    /// count. Report only: a NOTIFY never closes the window, because a
    /// topic carrying several keys can be notified for one while another's
    /// reply is still on its way.
    pub(crate) fn on_notify(&mut self, topic: &str, received: DateTime<Utc>) {
        if received >= self.started_at
            && self.asked.contains(topic)
            && !self.notified.contains(topic)
        {
            self.notified.insert(topic.to_string());
        }
    }

    /// How many topics the request named.
    pub(crate) fn asked(&self) -> usize {
        self.asked.len()
    }

    /// The outcome over the topics the window started with. A topic is
    /// covered when it answered or was notified since the start; the rest
    /// are sorted so the report's sample is stable. "No replies" needs both
    /// no reply at all and an uncovered topic: a feed whose NOTIFYs cover
    /// every asked topic is live whether or not its GET side answered.
    pub(crate) fn report(&self) -> RecoveryReport {
        let mut uncovered: Vec<&String> = self
            .asked
            .iter()
            .filter(|t| !self.answered.contains(t.as_str()) && !self.notified.contains(t.as_str()))
            .collect();
        uncovered.sort();
        if uncovered.is_empty() {
            RecoveryReport::AllAnswered
        } else if self.answered.is_empty() {
            RecoveryReport::NoReplies {
                asked: uncovered.len(),
            }
        } else {
            RecoveryReport::Partial {
                unanswered: uncovered.len(),
                sample: uncovered.into_iter().take(SAMPLE).cloned().collect(),
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
        assert!(matches!(w.report(), RecoveryReport::AllAnswered));
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
            w.report(),
            RecoveryReport::Partial { unanswered: 2, .. }
        ));
        let w = RecoveryWindow::start(t0, utc(0), Duration::from_secs(10), &asked);
        assert!(matches!(w.report(), RecoveryReport::NoReplies { asked: 3 }));
    }

    #[test]
    fn a_reply_on_a_topic_never_asked_answers_nothing() {
        let t0 = Instant::now();
        let asked = topics(&["a", "b"]);
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &asked);
        w.on_reply(t0, "z", None);
        w.on_reply(t0, "y", None);
        assert!(
            !w.done(t0),
            "two unasked replies do not close a two-topic window"
        );
        assert!(matches!(w.report(), RecoveryReport::NoReplies { asked: 2 }));
        w.on_reply(t0, "a", None);
        assert!(!w.done(t0));
        assert!(matches!(
            w.report(),
            RecoveryReport::Partial { unanswered: 1, .. }
        ));
    }

    #[test]
    fn a_notify_since_the_start_covers_an_asked_topic_in_the_report() {
        let t0 = Instant::now();
        let asked = topics(&["a", "b"]);
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &asked);
        w.on_notify("a", utc(100));
        w.on_notify("b", utc(150));
        assert!(
            matches!(w.report(), RecoveryReport::AllAnswered),
            "both topics notified since the start: the source is live"
        );
    }

    #[test]
    fn a_notify_before_the_start_covers_nothing() {
        let t0 = Instant::now();
        let asked = topics(&["a"]);
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &asked);
        // Received before the outage and still queued when the window opened.
        w.on_notify("a", utc(99));
        assert!(matches!(w.report(), RecoveryReport::NoReplies { asked: 1 }));
    }

    #[test]
    fn a_notify_never_closes_the_window() {
        let t0 = Instant::now();
        let asked = topics(&["a", "b"]);
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &asked);
        w.on_notify("a", utc(200));
        w.on_notify("b", utc(200));
        assert!(
            !w.done(t0),
            "a multi-key topic notified for one key still waits for its reply"
        );
    }

    #[test]
    fn partial_notify_cover_with_no_reply_reports_the_uncovered_as_no_replies() {
        let t0 = Instant::now();
        let asked = topics(&["a", "b", "c"]);
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &asked);
        w.on_notify("b", utc(200));
        w.on_notify("z", utc(200));
        assert!(matches!(w.report(), RecoveryReport::NoReplies { asked: 2 }));
        w.on_reply(t0, "a", None);
        assert_eq!(
            w.report(),
            RecoveryReport::Partial {
                unanswered: 1,
                sample: vec!["c".to_string()],
            }
        );
    }

    #[test]
    fn a_partial_report_samples_the_uncovered_in_sorted_order() {
        let t0 = Instant::now();
        let asked = topics(&["g", "f", "e", "d", "c", "b", "a"]);
        let mut w = RecoveryWindow::start(t0, utc(100), Duration::from_secs(10), &asked);
        w.on_reply(t0, "a", None);
        assert_eq!(
            w.report(),
            RecoveryReport::Partial {
                unanswered: 6,
                sample: topics(&["b", "c", "d", "e", "f"]),
            }
        );
    }
}
