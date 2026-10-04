//! Panic reports and crash-file retention.
//!
//! [`install_panic_hook`] logs panics marked by Geode's containment boundaries
//! and writes reports for unmarked panics. Reports snapshot the in-memory log
//! ring and recent dispatched actions before invoking the previous panic hook.
//! The app stores reports in the user config directory and daily logs in its
//! `logs` subdirectory; `geode_compose::logging` installs and trims those.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use geode_compose::logging::prune_files;
use geode_core::log::{Record, Ring};
use geode_shell::diagnostics::ActionTail;

/// Installs panic reporting after logging and the action registry are ready.
/// `ring` supplies retained log records, `tail` supplies the last 32 dispatched
/// action hashes, and `names` resolves hashes through the registry's shared map.
///
/// A process panic hook runs before unwinding, even when `catch_unwind` will
/// catch the panic. [`geode_core::panic::is_contained`] identifies calls marked
/// by Geode's containment helper: these emit an error log without a report.
/// Unmarked panics attempt a report. The marker does not determine whether the
/// process will exit; an unmarked panic can still be caught or end only its
/// thread.
///
/// `dir` selects the report directory; `None` logs without writing a report.
/// The action tail uses a nonblocking lock and degrades to a placeholder if it
/// is locked or poisoned. The `names` callback must also avoid blocking on locks
/// that the panicking thread might hold.
///
/// The previous hook is invoked after reporting on either path, preserving its
/// stderr output or other handling. Report writing precedes the result log so
/// subscriber failures cannot prevent an already completed file write. Reporting
/// is best-effort: it still allocates and performs I/O on the panicking thread.
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

        // The hook runs on the panicking thread before unwinding clears the
        // thread-local containment marker. Marked panics log without a report;
        // the previous hook still runs.
        if geode_core::panic::is_contained() {
            tracing::error!(
                target: "geode::shell",
                "contained panic{}: {message}",
                location
                    .as_deref()
                    .map(|l| format!(" at {l}"))
                    .unwrap_or_default()
            );
            previous(info);
            return;
        }

        // An unmarked panic gets a snapshot of the retained ring records.
        // This path allocates to preserve diagnostic context before unwinding.
        let mut records = Vec::with_capacity(ring.capacity());
        ring.drain_since(0, &mut records);

        // The hook runs before unwinding releases locks. A blocking lock
        // could deadlock on a guard held by this same thread; a placeholder
        // preserves the rest of the report when the tail is unavailable.
        let actions: Vec<String> = match tail.try_lock() {
            Ok(guard) => guard
                .recent()
                .map(|h| names(h).unwrap_or_else(|| format!("<unknown action {h:#x}>")))
                .collect(),
            Err(_) => vec!["<action tail unavailable>".to_string()],
        };

        match &dir {
            Some(dir) => {
                match write_crash_file(dir, at, &message, location.as_deref(), &records, &actions) {
                    Ok(path) => {
                        // Write the report before re-entering the tracing subscriber,
                        // which may itself be involved in the panic. The report uses
                        // in-memory records directly; daily log writes are buffered on
                        // a background thread and may still be in flight.
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

/// Extracts string panic payloads, with a placeholder for custom payload types.
/// This keeps non-string `panic_any` values representable in a report.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}

/// Maximum matching report files targeted by best-effort pruning after each
/// successful write. Reports have no separate rotation task.
const CRASH_FILES_KEPT: usize = 10;

/// Writes `crash-<YYYYMMDD-HHMMSS-mmm>-<suffix>.log` under `dir`.
/// The report contains the panic message and location, log records, and action
/// names in their supplied order. The hook supplies records and actions oldest
/// first. Each record includes its UTC timestamp and ring sequence number for
/// correlation with daily logs.
///
/// File timestamps use UTC, matching the daily logs regardless of the configured
/// display clock. The collision suffix starts at `00`; `create_new` reserves a
/// name without truncating an existing report and retries with the next suffix
/// on `AlreadyExists`.
///
/// Directory creation, file creation, and write failures propagate to the caller;
/// a failed write can leave a partial file. After a successful write, pruning
/// keeps the last [`CRASH_FILES_KEPT`] matching names in lexicographic order.
/// Pruning failures are ignored.
pub fn write_crash_file(
    dir: &Path,
    at: SystemTime,
    message: &str,
    location: Option<&str>,
    records: &[Record],
    actions: &[String],
) -> std::io::Result<PathBuf> {
    use std::io::Write;

    std::fs::create_dir_all(dir)?;
    let stamp = crash_timestamp(at);

    let mut out = String::new();
    out.push_str("Geode crash report\n");
    out.push_str(&format!("at: {stamp} UTC\n"));
    out.push_str(&format!("message: {message}\n"));
    out.push_str(&format!("location: {}\n", location.unwrap_or("<unknown>")));

    out.push_str("\n-- log tail --\n");
    for r in records {
        out.push_str(&format_record(r));
        out.push('\n');
    }

    out.push_str("\n-- last actions --\n");
    for a in actions {
        out.push_str(a);
        out.push('\n');
    }

    let mut suffix = 0u32;
    let path = loop {
        // Every name has a suffix, including the first (`-00`), so an
        // unsuffixed file cannot sort after its later collision. Padding
        // keeps suffixes below 100 in numeric order for filename pruning.
        let name = format!("crash-{stamp}-{suffix:02}.log");
        let candidate = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(out.as_bytes())?;
                break candidate;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                suffix += 1;
            }
            Err(e) => return Err(e),
        }
    };

    prune_files(dir, "crash-", ".log", CRASH_FILES_KEPT);
    Ok(path)
}

/// Formats a retained record with its UTC timestamp, ring sequence, level,
/// target, and message so the report can be correlated with daily logs.
fn format_record(r: &Record) -> String {
    let dt: chrono::DateTime<chrono::Utc> = r.at.into();
    format!(
        "[{}] seq={} {} {} {}",
        dt.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        r.seq,
        r.level,
        r.target,
        r.message
    )
}

fn crash_timestamp(at: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = at.into();
    dt.format("%Y%m%d-%H%M%S-%3f").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::log::Level;
    use std::time::Duration;

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

    // The exact name verifies UTC formatting, millisecond resolution, and use
    // of the supplied timestamp rather than the wall clock.
    #[test]
    fn write_crash_file_names_the_file_from_at_at_millisecond_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path =
            write_crash_file(dir.path(), SystemTime::UNIX_EPOCH, "boom", None, &[], &[]).unwrap();
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "crash-19700101-000000-000-00.log"
        );
    }

    // Two reports at the same millisecond must keep both contents.
    // `create_new` detects the collision and retries with the next suffix.
    #[test]
    fn a_second_write_at_the_same_instant_gets_a_suffixed_name_not_a_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_crash_file(
            dir.path(),
            SystemTime::UNIX_EPOCH,
            "first panic",
            None,
            &[],
            &[],
        )
        .unwrap();
        let second = write_crash_file(
            dir.path(),
            SystemTime::UNIX_EPOCH,
            "second panic",
            None,
            &[],
            &[],
        )
        .unwrap();

        assert_ne!(first, second, "the two crash files must not collide");
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "crash-19700101-000000-000-01.log"
        );

        let first_text = std::fs::read_to_string(&first).unwrap();
        let second_text = std::fs::read_to_string(&second).unwrap();
        assert!(
            first_text.contains("first panic"),
            "the first file must survive the second write: {first_text}"
        );
        assert!(second_text.contains("second panic"));
    }

    // Each successful write prunes report files; there is no separate
    // rotation task to enforce retention.
    #[test]
    fn write_crash_file_prunes_to_the_newest_ten() {
        let dir = tempfile::tempdir().unwrap();
        for secs in 0..11u64 {
            write_crash_file(
                dir.path(),
                SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
                "boom",
                None,
                &[],
                &[],
            )
            .unwrap();
        }
        let remaining: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining.len(), 10, "{remaining:?}");
        assert!(
            !remaining.contains(&"crash-19700101-000000-000-00.log".to_string()),
            "the oldest file must have been pruned: {remaining:?}"
        );
        assert!(
            remaining.contains(&"crash-19700101-000010-000-00.log".to_string()),
            "the newest file must survive: {remaining:?}"
        );
    }
}
