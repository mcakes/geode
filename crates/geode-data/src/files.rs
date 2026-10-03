//! Text file reads and writes a tile asks for (classification CSV import and
//! export). One supervised worker runs them in submission order, off both the
//! UI thread and the request loop, so a slow or network path never stalls a
//! query. A full queue or a stopped worker answers at once with an error
//! rather than waiting. Writes go to a sibling temporary file renamed into
//! place, so a failed write never leaves a half-written file.

use crate::service::{DataEvent, EventSink};
use geode_core::textfile::{TextFileOp, TextFileOutcome, TextFileParams, TextFileResult};
use std::io::Read;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Requests waiting behind the one in flight.
pub const FILES_QUEUE_BOUND: usize = 4;

/// Perform one request. Pure of the worker so tests call it directly.
pub fn run(p: &TextFileParams) -> TextFileResult {
    let shown = p.path.display();
    match &p.op {
        TextFileOp::Read { max_bytes } => TextFileResult::Read((|| {
            let file = std::fs::File::open(&p.path).map_err(|e| format!("{shown}: {e}"))?;
            let len = file.metadata().map_err(|e| format!("{shown}: {e}"))?.len();
            if len > *max_bytes {
                return Err(format!(
                    "{shown} is larger than {} MB",
                    max_bytes / (1024 * 1024)
                ));
            }
            let mut bytes = Vec::with_capacity(len as usize);
            file.take(max_bytes + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("{shown}: {e}"))?;
            String::from_utf8(bytes).map_err(|_| format!("{shown} is not UTF-8 text"))
        })()),
        TextFileOp::Write { text } => TextFileResult::Written((|| {
            let name = p
                .path
                .file_name()
                .ok_or_else(|| format!("{shown}: not a file path"))?;
            let tmp = p
                .path
                .with_file_name(format!(".{}.geode-tmp", name.to_string_lossy()));
            std::fs::write(&tmp, text).map_err(|e| format!("{shown}: {e}"))?;
            std::fs::rename(&tmp, &p.path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{shown}: {e}")
            })
        })()),
    }
}

fn answer(sink: &EventSink, p: &TextFileParams, result: TextFileResult) {
    let _ = sink(DataEvent::TextFile(TextFileOutcome {
        key: p.key,
        tag: p.tag,
        path: p.path.clone(),
        result,
    }));
}

fn refused(p: &TextFileParams, why: &str) -> TextFileResult {
    match p.op {
        TextFileOp::Read { .. } => TextFileResult::Read(Err(why.to_string())),
        TextFileOp::Write { .. } => TextFileResult::Written(Err(why.to_string())),
    }
}

fn work(jobs: Receiver<TextFileParams>, sink: EventSink) {
    while let Ok(job) = jobs.recv() {
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| run(&job))
        })) {
            Ok(result) => result,
            Err(payload) => refused(
                &job,
                &format!(
                    "file worker panicked: {}",
                    crate::ingest::runner::panic_payload_message(&*payload)
                ),
            ),
        };
        answer(&sink, &job, result);
    }
}

/// The one file worker `DataService` owns.
pub struct FileWorker {
    tx: Mutex<Option<SyncSender<TextFileParams>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    sink: EventSink,
}

impl FileWorker {
    pub fn spawn(sink: EventSink) -> Self {
        let (tx, rx) = sync_channel::<TextFileParams>(FILES_QUEUE_BOUND);
        let worker_sink = EventSink::clone(&sink);
        match crate::supervise::spawn_supervised(
            "geode-files".into(),
            EventSink::clone(&sink),
            move || work(rx, worker_sink),
        ) {
            Ok(handle) => FileWorker {
                tx: Mutex::new(Some(tx)),
                thread: Mutex::new(Some(handle)),
                sink,
            },
            Err(e) => {
                tracing::warn!(target: "geode::ingest", "file worker could not start: {e}");
                FileWorker {
                    tx: Mutex::new(None),
                    thread: Mutex::new(None),
                    sink,
                }
            }
        }
    }

    /// Queue a request. Every request is answered exactly once through the
    /// sink: a refusal (queue full, worker stopped) answers here at once.
    pub fn submit(&self, params: TextFileParams) {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            return answer(&self.sink, &params, refused(&params, "file worker stopped"));
        };
        match tx.try_send(params) {
            Ok(()) => {}
            Err(TrySendError::Full(p)) => answer(&self.sink, &p, refused(&p, "file worker busy")),
            Err(TrySendError::Disconnected(p)) => {
                answer(&self.sink, &p, refused(&p, "file worker stopped"))
            }
        }
    }

    /// Close the queue and join the worker; queued requests still run.
    /// Idempotent.
    pub fn shutdown(&self) {
        drop(self.tx.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(h) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }
}

impl Drop for FileWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::query::QueryKey;

    fn params(path: std::path::PathBuf, op: TextFileOp) -> TextFileParams {
        TextFileParams {
            key: QueryKey(9),
            tag: 1,
            path,
            op,
        }
    }

    #[test]
    fn reads_a_utf8_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.csv");
        std::fs::write(&path, "h1,h2\nx,y\n").unwrap();
        assert_eq!(
            run(&params(path, TextFileOp::Read { max_bytes: 1024 })),
            TextFileResult::Read(Ok("h1,h2\nx,y\n".into()))
        );
    }

    #[test]
    fn refuses_a_file_over_the_limit_without_reading_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.csv");
        std::fs::write(&path, vec![b'a'; 2048]).unwrap();
        let TextFileResult::Read(Err(e)) = run(&params(path, TextFileOp::Read { max_bytes: 1024 }))
        else {
            panic!("expected a refusal");
        };
        assert!(e.contains("larger than"), "{e}");
    }

    #[test]
    fn a_non_utf8_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin1.csv");
        std::fs::write(&path, [0x41, 0xE9, 0x0A]).unwrap();
        let TextFileResult::Read(Err(e)) = run(&params(path, TextFileOp::Read { max_bytes: 1024 }))
        else {
            panic!("expected an error");
        };
        assert!(e.contains("UTF-8"), "{e}");
    }

    #[test]
    fn a_missing_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.csv");
        let TextFileResult::Read(Err(e)) = run(&params(path, TextFileOp::Read { max_bytes: 1024 }))
        else {
            panic!("expected an error");
        };
        assert!(e.contains("nope.csv"), "{e}");
    }

    #[test]
    fn a_write_replaces_an_existing_file_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        std::fs::write(&path, "old\n").unwrap();
        assert_eq!(
            run(&params(
                path.clone(),
                TextFileOp::Write {
                    text: "new\n".into()
                }
            )),
            TextFileResult::Written(Ok(()))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn a_write_into_a_missing_directory_is_an_error_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("out.csv");
        let TextFileResult::Written(Err(e)) = run(&params(
            path.clone(),
            TextFileOp::Write { text: "x".into() },
        )) else {
            panic!("expected an error");
        };
        assert!(e.contains("out.csv"), "{e}");
        assert!(!dir.path().join("missing").exists());
    }

    #[test]
    fn the_worker_answers_through_the_sink_with_key_and_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.csv");
        std::fs::write(&path, "x\n").unwrap();
        let (sink, rx) = crate::supervise::tests_support::recording();
        let worker = FileWorker::spawn(sink);
        worker.submit(params(path, TextFileOp::Read { max_bytes: 10 }));
        let outcome = loop {
            match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
                DataEvent::TextFile(o) => break o,
                _ => continue,
            }
        };
        assert_eq!((outcome.key, outcome.tag), (QueryKey(9), 1));
        assert_eq!(outcome.result, TextFileResult::Read(Ok("x\n".into())));
        worker.shutdown();
    }
}
