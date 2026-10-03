//! One door for every long-lived data thread. A body that unwinds past
//! every containment boundary is declared once, with its payload, as
//! `DataEvent::ThreadStopped`, and nothing restarts it: a panic that repeats
//! on every request would otherwise crash-loop. The body runs without the
//! `contained` marker, so the app's panic hook still writes a crash file; an
//! uncontained thread death is a bug and the file is its report. A body that
//! returns is a deliberate stop and declares nothing.
//!
//! Long-lived service workers go through `spawn_supervised`; new workers
//! must too, or their death is silent. The channel adapter's transport
//! dispatcher has no event sink and is outside this supervision boundary.

use crate::service::{DataEvent, EventSink};
use std::thread::JoinHandle;

/// The request loop's thread name. The shell repeats it (it cannot depend on
/// this crate) to label the loop as the data service.
pub const REQUEST_LOOP: &str = "geode-data";

/// Spawn `body` on a thread called `name`. If `body` unwinds, emit exactly one
/// `ThreadStopped` naming the thread and the payload, then end the thread
/// normally so a later join does not propagate the panic.
pub(crate) fn spawn_supervised(
    name: String,
    sink: EventSink,
    body: impl FnOnce() + Send + 'static,
) -> std::io::Result<JoinHandle<()>> {
    let thread = name.clone();
    std::thread::Builder::new().name(name).spawn(move || {
        // Not `contained`: the panic hook must treat this unwind as a crash.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        if let Err(payload) = outcome {
            let reason = crate::ingest::runner::panic_payload_message(payload.as_ref());
            tracing::error!(target: "geode::ingest", "data thread {thread} stopped: {reason}");
            let _ = sink(DataEvent::ThreadStopped { thread, reason });
        }
    })
}

/// The stop sink for constructors that deliver into a channel (tests,
/// benches): they have no event sink, so a death is still caught and logged
/// by [`spawn_supervised`] but announced to no one.
pub(crate) fn unwatched() -> EventSink {
    std::sync::Arc::new(|_| false)
}

#[cfg(test)]
pub(crate) mod tests_support {
    use crate::service::{DataEvent, EventSink};
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    /// A sink that keeps every event for the test to read.
    pub(crate) fn recording() -> (EventSink, Receiver<DataEvent>) {
        let (tx, rx) = channel();
        let tx = std::sync::Mutex::new(tx);
        (Arc::new(move |e| tx.lock().unwrap().send(e).is_ok()), rx)
    }

    /// The next `ThreadStopped`, skipping everything else.
    pub(crate) fn next_stop(rx: &Receiver<DataEvent>) -> (String, String) {
        loop {
            match rx
                .recv_timeout(Duration::from_secs(30))
                .expect("a ThreadStopped")
            {
                DataEvent::ThreadStopped { thread, reason } => return (thread, reason),
                _ => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::recording;
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_panicking_supervised_body_emits_one_thread_stopped() {
        let (sink, rx) = recording();
        spawn_supervised("geode-test".into(), sink, || panic!("supervised boom"))
            .unwrap()
            .join()
            .expect("the helper catches the unwind");
        let events: Vec<DataEvent> = rx.try_iter().collect();
        assert_eq!(events.len(), 1, "{events:?}");
        match &events[0] {
            DataEvent::ThreadStopped { thread, reason } => {
                assert_eq!(thread, "geode-test");
                assert!(reason.contains("supervised boom"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_returning_supervised_body_declares_nothing() {
        let (sink, rx) = recording();
        spawn_supervised("geode-test".into(), sink, || {})
            .unwrap()
            .join()
            .unwrap();
        assert!(rx.try_iter().next().is_none());
    }

    #[test]
    fn the_supervised_body_is_not_marked_contained() {
        let (sink, _rx) = recording();
        let marked = Arc::new(AtomicBool::new(true));
        let seen = Arc::clone(&marked);
        spawn_supervised("geode-test".into(), sink, move || {
            seen.store(geode_core::panic::is_contained(), Ordering::SeqCst)
        })
        .unwrap()
        .join()
        .unwrap();
        assert!(
            !marked.load(Ordering::SeqCst),
            "the crash hook must see an uncontained thread death as a crash"
        );
    }
}
