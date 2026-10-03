//! Text file reads and writes a tile asks for (classification CSV import and
//! export). One supervised worker runs them in submission order, off both the
//! UI thread and the request loop, so a slow or network path never stalls a
//! query. A full queue or a stopped worker answers at once with an error
//! rather than waiting. Writes go to a sibling temporary file renamed into
//! place, so a failed write never leaves a half-written file.

use crate::service::{DataEvent, EventSink};
use geode_core::textfile::{TextFileOp, TextFileOutcome, TextFileParams, TextFileResult};
use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Requests waiting behind the one in flight.
pub const FILES_QUEUE_BOUND: usize = 4;

/// Perform one request. Pure of the worker so tests call it directly.
pub fn run(p: &TextFileParams) -> TextFileResult {
    match &p.op {
        TextFileOp::Read { max_bytes } => TextFileResult::Read(read_text(&p.path, *max_bytes)),
        TextFileOp::Write { text } => TextFileResult::Written(write_text(&p.path, text)),
    }
}

/// A byte limit as a person reads it: bytes under 1 MB, one decimal under
/// 10 MB, whole MB above. A truncated "0 MB" would misstate a small limit.
fn limit_text(max_bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if max_bytes < MB {
        format!("{max_bytes} bytes")
    } else if max_bytes < 10 * MB {
        format!("{:.1} MB", max_bytes as f64 / MB as f64)
    } else {
        format!("{} MB", max_bytes / MB)
    }
}

/// Read the whole file as UTF-8, refusing one over `max_bytes`. The size is
/// checked before reading and again after, so a file that grows between the
/// two is still refused rather than read past the limit.
fn read_text(path: &std::path::Path, max_bytes: u64) -> Result<String, String> {
    let shown = path.display();
    let too_large = || format!("{shown} is larger than {}", limit_text(max_bytes));
    let file = std::fs::File::open(path).map_err(|e| format!("{shown}: {e}"))?;
    let len = file.metadata().map_err(|e| format!("{shown}: {e}"))?.len();
    if len > max_bytes {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{shown}: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large());
    }
    String::from_utf8(bytes).map_err(|_| format!("{shown} is not UTF-8 text"))
}

/// Replace `path` with `text` through a sibling temporary renamed into
/// place. Once the temporary exists, any failure (write, sync, rename)
/// removes it, so a failed write leaves neither a partial file nor a
/// leftover temporary.
fn write_text(path: &std::path::Path, text: &str) -> Result<(), String> {
    let shown = path.display();
    let name = path
        .file_name()
        .ok_or_else(|| format!("{shown}: not a file path"))?;
    let tmp = path.with_file_name(format!(".{}.geode-tmp", name.to_string_lossy()));
    let file = std::fs::File::create(&tmp).map_err(|e| format!("{shown}: {e}"))?;
    fill_and_place(file, text, &tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{shown}: {e}")
    })
}

/// Write, flush to disk, close, then rename. The file is closed before the
/// rename so the rename never races an open handle (Windows refuses it).
fn fill_and_place(
    mut file: std::fs::File,
    text: &str,
    tmp: &std::path::Path,
    path: &std::path::Path,
) -> std::io::Result<()> {
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)
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

        // The size is refused before any read: without that check the
        // buffer is sized from the reported length, so a huge file would
        // be allocated for before the post-read check could refuse it. A
        // directory opens on unix and reports a nonzero size, but reading
        // it fails, so only the check before the read answers "larger".
        #[cfg(unix)]
        {
            let TextFileResult::Read(Err(e)) = run(&params(
                dir.path().to_path_buf(),
                TextFileOp::Read { max_bytes: 1 },
            )) else {
                panic!("expected a refusal");
            };
            assert!(e.contains("larger than 1 bytes"), "{e}");
        }
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

    /// A rename that fails after the temporary was written removes the
    /// temporary: the target here is a non-empty directory, which no
    /// platform lets a file replace.
    #[test]
    fn a_failed_rename_removes_the_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), "x").unwrap();
        let TextFileResult::Written(Err(e)) = run(&params(
            path.clone(),
            TextFileOp::Write {
                text: "new\n".into(),
            },
        )) else {
            panic!("expected an error");
        };
        assert!(e.contains("out.csv"), "{e}");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("out.csv")]);
    }

    /// `/dev/zero` reports a length of 0 and then reads without end: the
    /// size check after the read refuses it, as it would a file that grew
    /// between the metadata check and the read.
    #[cfg(unix)]
    #[test]
    fn a_read_past_the_limit_after_the_size_check_is_refused() {
        let TextFileResult::Read(Err(e)) = run(&params(
            "/dev/zero".into(),
            TextFileOp::Read { max_bytes: 16 },
        )) else {
            panic!("expected a refusal");
        };
        assert!(e.contains("larger than 16 bytes"), "{e}");
    }

    #[test]
    fn a_limit_is_stated_in_the_unit_a_person_reads() {
        assert_eq!(limit_text(1024), "1024 bytes");
        assert_eq!(limit_text(5 * 1024 * 1024 / 2), "2.5 MB");
        assert_eq!(limit_text(10 * 1024 * 1024), "10 MB");
    }

    /// A stopped worker still answers: the refusal arrives through the
    /// sink at once, so a tile waiting on its tag is never left waiting.
    #[test]
    fn a_stopped_worker_answers_a_submission_with_an_error() {
        let (sink, rx) = crate::supervise::tests_support::recording();
        let worker = FileWorker::spawn(sink);
        worker.shutdown();
        worker.submit(params("a.csv".into(), TextFileOp::Read { max_bytes: 10 }));
        let outcome = loop {
            match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
                DataEvent::TextFile(o) => break o,
                _ => continue,
            }
        };
        assert_eq!((outcome.key, outcome.tag), (QueryKey(9), 1));
        assert_eq!(
            outcome.result,
            TextFileResult::Read(Err("file worker stopped".into()))
        );
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
