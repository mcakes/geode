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

/// Replaces the process panic hook (spec §4.7). Installed once, after
/// the `tracing` subscriber (`main.rs::install_logging`), with
/// everything the crash file needs already captured as `'static`
/// handles: `ring` (the log tail), `tail` (the last 32 dispatched
/// actions, as hashes — see `ActionTail`'s own doc comment), and
/// `names`, which resolves those hashes back to action ids through
/// `ActionRegistry::hash_names`'s shared snapshot.
///
/// **This hook runs for every panic on any thread, contained or not**
/// (fix round 1, MAJ-1) — a process panic hook fires before
/// `catch_unwind` ever gets a chance to catch anything, so the four
/// deliberate containment boundaries this codebase has (the ingest
/// load, its pop-time catalog recheck, a discovery poll, a query pool
/// worker — each wrapped in `geode_core::panic::contained`) do not
/// suppress it. `geode_core::panic::is_contained()` is how this hook
/// tells the two apart: a *contained* panic is one of those boundaries
/// doing exactly what it exists for — the app keeps running — so it
/// gets an `error` log line and no file; only an *uncontained* panic,
/// one nothing caught, is an actual crash and gets a
/// `write_crash_file` call. Without this, forty contained panics in a
/// row (the `file_generations`-row scenario `runner.rs`'s pop-time
/// recheck documents) would produce forty "Geode crash report" files
/// for an application that never crashed.
///
/// `dir` is the directory crash files are written into — `None` (no
/// writable user config dir) means the hook only logs; a missing home
/// directory must never be the reason a panic itself panics.
///
/// The previous hook (`std::panic::take_hook`) always runs, last: this
/// hook adds a crash file, it does not replace whatever handling was
/// already installed (the default hook's stderr backtrace, or a future
/// feature's own hook). The crash file is written — and, for a
/// contained panic, the `error!` line is emitted — *before* that final
/// call, so the artifact lands even if `previous` never returns.
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

        // MAJ-1: a contained panic (one of this codebase's own
        // `catch_unwind` boundaries doing its job) is not a crash — log
        // it and stop. `geode_core::panic::is_contained` reads a
        // thread-local set for the duration of the `contained` call the
        // panic is unwinding out of; the hook always runs on the
        // panicking thread, so this reads the right thread's marker.
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

        // From here down: nothing caught this — an uncontained panic is
        // the process actually going down.
        //
        // Allocation is fine on this path (spec: "nothing may stall the
        // render thread" governs the *hot* path, not the one time the
        // process is already going down) — `Ring::drain_since`'s own doc
        // comment already allocates one clone per matching record.
        let mut records = Vec::with_capacity(ring.capacity());
        ring.drain_since(0, &mut records);

        // MIN-2: `try_lock`, not `lock().unwrap_or_else(into_inner)` —
        // poison recovery is the wrong defense here. A hook runs
        // *before* unwinding, so a mutex the panicking thread already
        // holds is *held*, not poisoned; blocking `lock()` on it from
        // the same thread would deadlock the hook rather than recover
        // from anything. Unreachable today (`dispatch`'s guard is a
        // statement temporary, and `ActionTail::record`/`fnv1a` panic on
        // nothing), but the crash file is the artifact that must
        // survive — a missing tail is a smaller loss than no file at
        // all.
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
                        // MIN-3: this — and the `error!` calls in every
                        // other arm of this hook — re-enters the
                        // `tracing` subscriber the hook is itself
                        // reporting on. That's why the file is written
                        // first: if a panic inside the subscriber (e.g.
                        // the rolling appender's writer lock already
                        // held by this same thread) deadlocks here, the
                        // artifact is already on disk. This crash file
                        // itself does not depend on the daily log file's
                        // own state either way — `write_crash_file`
                        // reads the ring's in-memory tail directly, never
                        // the file layer.
                        //
                        // MIN-8 (final review), superseding the earlier
                        // note here: `install_logging` now uses
                        // `tracing_appender::non_blocking`, not a plain
                        // `RollingFileAppender` — writes to the daily log
                        // file go through a bounded channel to a
                        // background thread, so an EARLIER log line is no
                        // longer guaranteed to already be on disk by the
                        // time a panic reaches this hook (a buffered
                        // batch can still be in flight). Accepted: the
                        // file layer is UI-thread `warn`+ only (the
                        // Global Constraint), so the window is small, and
                        // the crash file — the artifact this hook exists
                        // to guarantee — is unaffected either way.
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

/// How many `crash-*.log` files `write_crash_file` keeps, applied after
/// every write (fix round 1, MAJ-1) — the same shape as the daily logs'
/// own cap (`trim_log_files`, `keep = 7`), just pruned continuously
/// instead of once at startup, since crash files aren't rotated by
/// anything else the way `tracing_appender` rotates the daily logs.
const CRASH_FILES_KEPT: usize = 10;

/// Writes `crash-<YYYYMMDD-HHMMSS-mmm>.log` under `dir`: the panic
/// message and location, the log ring's contents (oldest first, as
/// already ordered by `Ring::drain_since`, each line carrying its own
/// timestamp and sequence number so it can be aligned against
/// `logs/geode.YYYY-MM-DD.log`), and the last actions dispatched
/// (oldest first, per `ActionTail::recent`). The timestamp is UTC, not
/// local — the same clock `tracing-appender`'s daily log files use
/// (`main.rs::install_logging`'s own MIN-7 note), so a crash file's name
/// sorts and reads consistently against the log file it landed beside,
/// even though every *displayed* time elsewhere in this app is the
/// trader's configured clock (Phase 4a's ruling; as-of dialog Part 2).
///
/// Millisecond resolution, opened with `create_new` rather than
/// `std::fs::write` (fix round 1, MAJ-1): two panics inside one second
/// — the exact shape of a run of *contained* panics before this fix
/// round, and still possible for two genuinely uncontained ones close
/// together — used to collide on a second-resolution name and the
/// second write silently truncated the first file. `create_new` turns
/// that into a detected collision instead: on `AlreadyExists`, retry
/// with a `-1`, `-2`, … suffix until a name is free, so a near-
/// simultaneous second crash gets its own file rather than erasing the
/// first one's. Every write also prunes `dir` to the newest
/// [`CRASH_FILES_KEPT`] `crash-*.log` files, since nothing else ever
/// rotates them.
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
        // Always suffixed, zero-padded: `-00` for the first file at this
        // millisecond, `-01` for a collision, so a plain name sort is a
        // true age order (`-` sorts before `.`, so an unsuffixed name
        // would sort *after* its own later collision — re-review of Task
        // 6's fix round).
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

/// One log-tail line: level, target, message, plus (fix round 1, MIN-4)
/// the record's own UTC timestamp and ring sequence number — without
/// them, a crash file's tail cannot be lined up against
/// `logs/geode-YYYY-MM-DD.log`, which is the first thing anyone reading
/// one will try to do.
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

/// Deletes the oldest files matching `<prefix>*<suffix>` in `dir` beyond
/// `keep`. Oldest-first by file name: shared by [`trim_log_files`]
/// (`geode.YYYY-MM-DD.log`, sorts lexicographically by date) and
/// [`write_crash_file`]'s own pruning (`crash-YYYYMMDD-HHMMSS-mmm.log`,
/// sorts lexicographically the same way) — both name formats are
/// deliberately built so a plain sort is a correct age order with no
/// filesystem metadata read. A pure function of a directory listing:
/// never called with a `dir` that doesn't exist, but a missing or
/// unreadable directory is simply a no-op, not a panic.
fn prune_files(dir: &Path, prefix: &str, suffix: &str, keep: usize) {
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
            name.starts_with(prefix) && name.ends_with(suffix)
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

/// Deletes the oldest `geode.*.log` files in `dir` beyond `keep`, applied
/// once at startup (`tracing_appender::rolling::daily` itself never
/// prunes past files it didn't create this run). A pure function of a
/// directory listing: never called with a `dir` that doesn't exist, but
/// a missing or unreadable directory is simply a no-op, not a panic
/// (this runs ahead of the subscriber being fully up). See
/// [`prune_files`] for the shared oldest-first mechanics.
pub fn trim_log_files(dir: &Path, keep: usize) {
    prune_files(dir, "geode.", ".log", keep);
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::log::Level;
    use std::time::Duration;

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

    // Fix round 1, MIN-5: the exact name, not just a prefix/suffix check —
    // an implementation that ignored `at` (used `SystemTime::now()`
    // instead) or changed the format would still pass the looser
    // assertions above. This pins the format, the UTC choice, the
    // millisecond field, and the `at` parameter itself in one line.
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

    // Fix round 1, MAJ-1: two panics in the same millisecond must not
    // silently overwrite each other — `create_new` turns the collision
    // into a detected error the write retries past with a `-1` suffix,
    // rather than `std::fs::write`'s truncate-on-collision.
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

    // Fix round 1, MAJ-1: nothing else ever rotates crash files, so
    // `write_crash_file` prunes on every call — without this, a run of
    // contained-panics-turned-uncontained (or just an old checkout) would
    // accumulate `crash-*.log` files forever.
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
