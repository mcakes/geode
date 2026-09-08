//! Crash and log-file housekeeping (Phase 4b). Task 2 added
//! [`trim_log_files`], the pure startup trim for the daily rolling log
//! directory (`main.rs::install_logging`). Task 6 adds the panic hook
//! itself: [`install_panic_hook`] replaces the default hook with one
//! that writes a crash file — the log ring's contents plus the last 32
//! actions dispatched, both otherwise lost the moment the process exits
//! — beside the daily logs, then defers to whatever hook was installed
//! before it (so a debug build's default "note: run with `RUST_BACKTRACE`"
//! output, or any hook a future feature adds, still runs).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use geode_core::log::{Record, Ring};
use geode_shell::diagnostics::ActionTail;

/// Replaces the process panic hook (spec §4.7, the plan's ruling: "the
/// panic in ingest marks the source `Failed`... Task 6 adds the `error`
/// event with file and payload" — this is the render-thread-panic half
/// of that same story). Installed once, after the `tracing` subscriber
/// (`main.rs::install_logging`), with everything the crash file needs
/// already captured as `'static` handles: `ring` (the log tail),
/// `tail` (the last 32 dispatched actions, as hashes — see
/// `ActionTail`'s own doc comment), and `names`, which resolves those
/// hashes back to action ids through `ActionRegistry::hash_names`'s
/// shared snapshot (so a hash recorded before this hook was installed,
/// or before some later `register` call, still resolves).
///
/// `dir` is the directory crash files are written into — `None` (no
/// writable user config dir) means the hook only logs; a missing home
/// directory must never be the reason a panic itself panics.
///
/// The previous hook (`std::panic::take_hook`) always runs, last: this
/// hook adds a crash file, it does not replace whatever handling was
/// already installed (the default hook's stderr backtrace, or a future
/// feature's own hook).
pub fn install_panic_hook(
    dir: Option<PathBuf>,
    ring: Arc<Ring>,
    tail: Arc<Mutex<ActionTail>>,
    names: Arc<dyn Fn(u64) -> Option<String> + Send + Sync>,
) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_payload_message(info.payload());
        let location = info.location().map(|l| l.to_string());
        let at = SystemTime::now();

        // Allocation is fine on the panic path (spec: "nothing may stall
        // the render thread" governs the *hot* path, not the one time
        // the process is already going down) — `Ring::drain_since`'s own
        // doc comment already allocates one clone per matching record.
        let mut records = Vec::with_capacity(ring.capacity());
        ring.drain_since(0, &mut records);

        let actions: Vec<String> = tail
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recent()
            .map(|h| names(h).unwrap_or_else(|| format!("<unknown action {h:#x}>")))
            .collect();

        match &dir {
            Some(dir) => {
                match write_crash_file(dir, at, &message, location.as_deref(), &records, &actions) {
                    Ok(path) => {
                        tracing::error!(
                            target: "geode::shell",
                            "crash file written to {}",
                            path.display()
                        );
                    }
                    Err(e) => {
                        tracing::error!(target: "geode::shell", "failed to write crash file: {e}");
                    }
                }
            }
            None => {
                tracing::error!(target: "geode::shell", "panic (no crash directory configured): {message}");
            }
        }

        previous(info);
    }));
}

/// A panic payload as text: `panic!`/`unwrap`/`expect` payloads are
/// always `&'static str` or `String`; anything else (a custom
/// `panic_any` payload) falls back to a named placeholder rather than
/// losing the crash file entirely. Same shape as
/// `geode_data::ingest::runner`'s own copy — different crates, no
/// shared dependency to hang a single one off.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}

/// Writes `crash-<YYYYMMDD-HHMMSS>.log` under `dir`: the panic message
/// and location, the log ring's contents (oldest first, as already
/// ordered by `Ring::drain_since`), and the last actions dispatched
/// (oldest first, per `ActionTail::recent`). The timestamp is UTC, not
/// local — the same clock `tracing-appender`'s daily log files use
/// (`main.rs::install_logging`'s own MIN-7 note), so a crash file's name
/// sorts and reads consistently against the log file it landed beside,
/// even though every *displayed* time elsewhere in this app is the
/// trader's local clock (Phase 4a's ruling).
pub fn write_crash_file(
    dir: &Path,
    at: SystemTime,
    message: &str,
    location: Option<&str>,
    records: &[Record],
    actions: &[String],
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = crash_timestamp(at);
    let path = dir.join(format!("crash-{stamp}.log"));

    let mut out = String::new();
    out.push_str("Geode crash report\n");
    out.push_str(&format!("at: {stamp} UTC\n"));
    out.push_str(&format!("message: {message}\n"));
    out.push_str(&format!("location: {}\n", location.unwrap_or("<unknown>")));

    out.push_str("\n-- log tail --\n");
    for r in records {
        out.push_str(&format!("[{}] {} {}\n", r.level, r.target, r.message));
    }

    out.push_str("\n-- last actions --\n");
    for a in actions {
        out.push_str(a);
        out.push('\n');
    }

    std::fs::write(&path, out)?;
    Ok(path)
}

fn crash_timestamp(at: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = at.into();
    dt.format("%Y%m%d-%H%M%S").to_string()
}

/// Deletes the oldest `geode.*.log` files in `dir` beyond `keep`, applied
/// once at startup (`tracing_appender::rolling::daily` itself never
/// prunes past files it didn't create this run). Oldest-first by file
/// name — `RollingFileAppender`'s daily names sort lexicographically by
/// date (`geode.YYYY-MM-DD.log`), so a plain sort is a correct age order
/// with no filesystem metadata read. A pure function of a directory
/// listing: never called with a `dir` that doesn't exist, but a missing
/// or unreadable directory is simply a no-op, not a panic (this runs
/// ahead of the subscriber being fully up).
pub fn trim_log_files(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            name.starts_with("geode.") && name.ends_with(".log")
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for old in &files[..files.len() - keep] {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::log::Level;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    fn record(seq: u64, target: &'static str, message: &str) -> Record {
        Record {
            at: SystemTime::UNIX_EPOCH,
            level: Level::INFO,
            target,
            message: message.to_string(),
            seq,
        }
    }

    #[test]
    fn write_crash_file_contains_the_message_location_records_and_actions_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let records = vec![
            record(1, "geode::ingest", "first record message"),
            record(2, "geode::shell", "second record message"),
        ];
        let actions = vec![
            "workspace::focus_left".to_string(),
            "palette::toggle".to_string(),
        ];

        let path = write_crash_file(
            dir.path(),
            SystemTime::UNIX_EPOCH,
            "index out of bounds: the len is 3 but the index is 5",
            Some("crates/geode-shell/src/frame.rs:120:9"),
            &records,
            &actions,
        )
        .unwrap();

        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("crash-"), "{name}");
        assert!(name.ends_with(".log"), "{name}");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("index out of bounds: the len is 3 but the index is 5"));
        assert!(text.contains("crates/geode-shell/src/frame.rs:120:9"));

        let first = text.find("first record message").expect("first record");
        let second = text.find("second record message").expect("second record");
        assert!(first < second, "records must appear oldest-first: {text}");

        let a1 = text.find("workspace::focus_left").expect("first action");
        let a2 = text.find("palette::toggle").expect("second action");
        assert!(a1 < a2, "actions must appear oldest-first: {text}");
    }

    #[test]
    fn write_crash_file_without_records_or_actions_still_writes_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let path =
            write_crash_file(dir.path(), SystemTime::UNIX_EPOCH, "boom", None, &[], &[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("boom"));
        assert!(
            text.contains("<unknown>"),
            "a missing location is named: {text}"
        );
    }

    #[test]
    fn trim_deletes_the_oldest_files_beyond_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=9 {
            touch(dir.path(), &format!("geode.2026-09-{day:02}.log"));
        }
        trim_log_files(dir.path(), 7);
        let mut remaining: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                "geode.2026-09-03.log",
                "geode.2026-09-04.log",
                "geode.2026-09-05.log",
                "geode.2026-09-06.log",
                "geode.2026-09-07.log",
                "geode.2026-09-08.log",
                "geode.2026-09-09.log",
            ]
        );
    }

    #[test]
    fn trim_is_a_no_op_under_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "geode.2026-09-01.log");
        trim_log_files(dir.path(), 7);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn trim_ignores_files_that_are_not_named_like_a_geode_log() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=9 {
            touch(dir.path(), &format!("geode.2026-09-{day:02}.log"));
        }
        touch(dir.path(), "other.txt");
        trim_log_files(dir.path(), 7);
        assert!(dir.path().join("other.txt").exists(), "not ours to delete");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 8); // 7 kept + other.txt
    }

    #[test]
    fn trim_on_a_missing_directory_is_a_no_op_not_a_panic() {
        trim_log_files(Path::new("/does/not/exist"), 7);
    }
}
