//! Coalesced state for UI delivery. Pending entries are keyed by event kind and
//! recipient, source, or dataset/batch. Tagged outcomes retain the highest tag;
//! publications union affected books and keep the greatest generation ID.
//! Replacing an entry preserves its position among other pending keys, so this
//! is not a chronological event log. See `docs/current/request-delivery.md`.
//!
//! An upload outcome keys on `(tile, tag)` rather than the tile alone, so two
//! distinct uploads from the same tile remain separate. Coalescing by tile
//! alone could hide an earlier upload's failure behind a later success.
//!
//! A one-slot channel carries only wakeups. Full wakeup capacity does not refuse
//! state, but pending entries have no fixed key-count cap. Sender acceptance
//! does not acknowledge that the window has applied the event.

use geode_core::query::QueryKey;
use geode_data::service::DataEvent;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
enum Key {
    Query(QueryKey),
    Series(QueryKey),
    Distinct(QueryKey),
    Catalog(QueryKey),
    Price(QueryKey),
    /// Keyed on `(tile, tag)`, not on the tile alone: uploads are separate
    /// user actions whose outcomes must remain distinct. Keying on the tile would
    /// let `Sender::try_send`'s highest-tag-wins coalescing drop an earlier
    /// still-undelivered outcome (e.g. a failure) when a later upload from
    /// the same tile answers before the first is read.
    Upload(QueryKey, u64),
    Published(String, String),
    Fetched(String, String, bool),
    Load,
    Health(String),
    Polled(String),
    Diagnostics,
}

fn key(event: &DataEvent) -> Key {
    match event {
        DataEvent::Query(o) => Key::Query(o.key),
        DataEvent::Series(o) => Key::Series(o.key),
        DataEvent::Distinct(o) => Key::Distinct(o.key),
        DataEvent::Catalog(o) => Key::Catalog(o.key),
        DataEvent::Price(o) => Key::Price(o.key),
        DataEvent::Upload(o) => Key::Upload(o.key, o.tag),
        DataEvent::Published { dataset, batch, .. } => {
            Key::Published(dataset.clone(), batch.clone())
        }
        DataEvent::SeriesFetched {
            source,
            identity,
            result,
        } => Key::Fetched(source.clone(), identity.clone(), result.is_ok()),
        DataEvent::Loading { .. } | DataEvent::LoadEnded => Key::Load,
        DataEvent::Health { source, .. } => Key::Health(source.clone()),
        DataEvent::Polled { source, .. } => Key::Polled(source.clone()),
        DataEvent::Diagnostics(_) => Key::Diagnostics,
    }
}

fn tag(event: &DataEvent) -> Option<u64> {
    match event {
        DataEvent::Query(o) => Some(o.tag),
        DataEvent::Series(o) => Some(o.tag),
        DataEvent::Distinct(o) => Some(o.tag),
        DataEvent::Catalog(o) => Some(o.tag),
        DataEvent::Price(o) => Some(o.tag),
        DataEvent::Upload(o) => Some(o.tag),
        _ => None,
    }
}

#[derive(Default)]
struct Pending {
    events: HashMap<Key, DataEvent>,
    order: VecDeque<Key>,
}

#[derive(Debug)]
pub(crate) struct Closed;

pub(crate) struct Sender {
    pending: Arc<Mutex<Pending>>,
    wake: async_channel::Sender<()>,
}

#[derive(Clone)]
pub(crate) struct Receiver {
    pending: Arc<Mutex<Pending>>,
    wake: async_channel::Receiver<()>,
}

pub(crate) fn channel() -> (Sender, Receiver) {
    let pending = Arc::new(Mutex::new(Pending::default()));
    let (tx, rx) = async_channel::bounded(1);
    (
        Sender {
            pending: Arc::clone(&pending),
            wake: tx,
        },
        Receiver { pending, wake: rx },
    )
}

impl Sender {
    pub(crate) fn try_send(&self, mut event: DataEvent) -> Result<(), Closed> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if self.wake.is_closed() {
            return Err(Closed);
        }
        let key = key(&event);
        if let Key::Fetched(source, identity, true) = &key {
            // Success clears an earlier failure. A failure AFTER a success must
            // retain both: the success triggers a requery for the appended span.
            let failed = Key::Fetched(source.clone(), identity.clone(), false);
            if pending.events.remove(&failed).is_some() {
                pending.order.retain(|key| key != &failed);
            }
        }
        if let Some(old) = pending.events.get(&key) {
            if tag(old) > tag(&event) {
                return Ok(());
            }
            if let (DataEvent::Diagnostics(previous), DataEvent::Diagnostics(diags)) =
                (old, &mut event)
            {
                let mut merged = previous.clone();
                for diagnostic in diags.drain(..) {
                    if !merged.contains(&diagnostic) {
                        merged.push(diagnostic);
                    }
                }
                *diags = merged;
            }
            // A newer publication supersedes the generation, but must still
            // invalidate books changed by any earlier undelivered publication.
            if let (
                DataEvent::Published {
                    books: previous,
                    gen_id: previous_gen,
                    ..
                },
                DataEvent::Published { books, gen_id, .. },
            ) = (old, &mut event)
            {
                books.extend(previous.iter().cloned());
                books.sort();
                books.dedup();
                *gen_id = (*gen_id).max(*previous_gen);
            }
        } else {
            pending.order.push_back(key.clone());
        }
        if let DataEvent::Diagnostics(diags) = &mut event {
            // Match the shell's diagnostic history bound and oldest-first order.
            let excess = diags
                .len()
                .saturating_sub(geode_shell::diagnostics::DATA_DIAGNOSTICS_CAP);
            diags.drain(..excess);
        }
        let replaced = pending.events.insert(key, event);
        // A full channel already promises a wakeup. The pending state is
        // installed before signalling, so the reader cannot miss an update.
        let _ = self.wake.try_send(());
        drop(pending);
        // Releasing a displaced Arrow snapshot can be expensive. Do it after
        // unlocking so the UI can take the next event immediately.
        drop(replaced);
        Ok(())
    }
}

impl Receiver {
    pub(crate) async fn recv(&self) -> Result<DataEvent, async_channel::RecvError> {
        loop {
            {
                let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(key) = pending.order.pop_front() {
                    return Ok(pending.events.remove(&key).expect("queued event"));
                }
            }
            self.wake.recv().await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::query::QueryOutcome;
    use std::time::Instant;

    #[test]
    fn an_idle_receiver_is_woken_by_the_final_event() {
        use std::future::Future;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::task::{Context, Poll, Wake, Waker};
        struct Flag(AtomicBool);
        impl Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let (tx, rx) = channel();
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));
        let mut context = Context::from_waker(&waker);
        let mut receiving = std::pin::pin!(rx.recv());
        assert!(receiving.as_mut().poll(&mut context).is_pending());
        tx.try_send(DataEvent::LoadEnded).unwrap();
        assert!(flag.0.load(Ordering::Relaxed));
        assert!(matches!(
            receiving.as_mut().poll(&mut context),
            Poll::Ready(Ok(DataEvent::LoadEnded))
        ));
    }

    #[gpui::test]
    async fn diagnostics_coalesce_in_history_order_with_the_shells_bound() {
        let (tx, rx) = channel();
        for n in 0..300 {
            tx.try_send(DataEvent::Diagnostics(vec![
                geode_core::config::Diagnostic {
                    severity: geode_core::config::Severity::Error,
                    layer: None,
                    file: None,
                    message: format!("error {n}"),
                    path: None,
                },
            ]))
            .unwrap();
        }
        let DataEvent::Diagnostics(diags) = rx.recv().await.unwrap() else {
            panic!("diagnostics expected")
        };
        assert_eq!(diags.len(), geode_shell::diagnostics::DATA_DIAGNOSTICS_CAP);
        assert_eq!(diags.first().unwrap().message, "error 44");
        assert_eq!(diags.last().unwrap().message, "error 299");
    }

    #[gpui::test]
    async fn a_failed_fetch_cannot_erase_a_successful_spans_invalidation() {
        let (tx, rx) = channel();
        let fetched = |result| DataEvent::SeriesFetched {
            source: "history".into(),
            identity: "SPX".into(),
            result,
        };
        tx.try_send(fetched(Err("old failure".into()))).unwrap();
        tx.try_send(fetched(Ok(10))).unwrap();
        tx.try_send(fetched(Err("new failure".into()))).unwrap();
        assert!(matches!(
            rx.recv().await.unwrap(),
            DataEvent::SeriesFetched { result: Ok(10), .. }
        ));
        assert!(
            matches!(rx.recv().await.unwrap(), DataEvent::SeriesFetched { result: Err(e), .. } if e == "new failure")
        );
        tx.try_send(fetched(Err("cleared".into()))).unwrap();
        tx.try_send(fetched(Ok(0))).unwrap();
        drop(tx);
        assert!(matches!(
            rx.recv().await.unwrap(),
            DataEvent::SeriesFetched { result: Ok(0), .. }
        ));
        assert!(rx.recv().await.is_err());
    }

    #[gpui::test]
    async fn two_upload_outcomes_for_the_same_tile_are_both_delivered() {
        // Uploads are separate user actions, not a retriable request: a
        // failed upload followed by a successful one must not coalesce into
        // only the latest, the way a tagged query answer would.
        let (tx, rx) = channel();
        let outcome = |tag, result: Result<(), &str>| {
            DataEvent::Upload(geode_data::egress::UploadOutcome {
                key: QueryKey(1),
                tag,
                target: "sophis".into(),
                result: result.map_err(str::to_string),
            })
        };
        tx.try_send(outcome(1, Err("write error"))).unwrap();
        tx.try_send(outcome(2, Ok(()))).unwrap();
        assert!(matches!(
            rx.recv().await.unwrap(),
            DataEvent::Upload(o) if o.tag == 1 && o.result == Err("write error".into())
        ));
        assert!(matches!(
            rx.recv().await.unwrap(),
            DataEvent::Upload(o) if o.tag == 2 && o.result == Ok(())
        ));
    }

    #[gpui::test]
    async fn a_burst_retains_terminal_results_and_all_publication_books() {
        let (tx, rx) = channel();
        for n in 0..4096 {
            tx.try_send(DataEvent::Published {
                dataset: "risk".into(),
                batch: "batch".into(),
                gen_id: n,
                books: vec![Some(format!("book{}", n % 2))],
            })
            .unwrap();
            tx.try_send(DataEvent::Query(QueryOutcome {
                key: QueryKey(1),
                tag: n as u64,
                snapshot: Err(format!("result {n}")),
                submitted: Instant::now(),
            }))
            .unwrap();
        }
        tx.try_send(DataEvent::LoadEnded).unwrap();
        // Late completion of an older request cannot replace the latest answer.
        tx.try_send(DataEvent::Query(QueryOutcome {
            key: QueryKey(1),
            tag: 0,
            snapshot: Err("stale".into()),
            submitted: Instant::now(),
        }))
        .unwrap();
        assert_eq!(rx.pending.lock().unwrap().events.len(), 3);
        drop(tx); // The final burst must drain even when no further event arrives.
        assert!(
            matches!(rx.recv().await.unwrap(), DataEvent::Published { gen_id: 4095, books, .. } if books.len() == 2)
        );
        assert!(matches!(rx.recv().await.unwrap(), DataEvent::Query(o) if o.tag == 4095));
        assert!(matches!(rx.recv().await.unwrap(), DataEvent::LoadEnded));
        assert!(rx.recv().await.is_err());
    }
}
