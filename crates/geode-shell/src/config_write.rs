//! The one door for runtime configuration writes.
//!
//! Only the user layer is writable. Each edit parses the current document,
//! changes it with `toml_edit`, writes a uniquely named temporary file beside
//! the target, syncs it, and renames it over the target. A document this
//! process cannot parse is left untouched. Temporary names deliberately do
//! not end in `.toml`, so the reload poll cannot observe a partial write.
//!
//! Submissions targeting the same directory run in acceptance order on the
//! background executor. Parsing and filesystem work happen outside queue
//! locks. Atomic rename prevents a torn file; the directory itself is not
//! fsynced, so persistence across an abrupt system failure remains
//! best-effort.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use geode_core::config::{CONFIG_VERSION, Layer};
use toml_edit::{DocumentMut, value};

// Directory-scoped: all windows writing the same configured directory share
// ordering, while unrelated profiles (and test fixtures) remain independent.
// No filesystem access on submission; callers use their configured directory
// consistently. This does not coordinate other processes or symlink aliases.
#[derive(Default)]
struct Writer {
    pending: Mutex<Pending>,
    transaction: Mutex<()>,
}

#[derive(Default)]
struct Pending {
    jobs: VecDeque<Box<dyn FnOnce() + Send>>,
    running: bool,
}

static WRITERS: LazyLock<Mutex<HashMap<PathBuf, Weak<Writer>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn writer(dir: &Path) -> Arc<Writer> {
    let path = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut writers = WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    writers.retain(|_, writer| writer.strong_count() != 0);
    let entry = writers.entry(path).or_default();
    if let Some(writer) = entry.upgrade() {
        return writer;
    }
    let writer = Arc::new(Writer::default());
    *entry = Arc::downgrade(&writer);
    writer
}

/// Accept a config mutation in calling order, before the executor can reorder
/// tasks. The detached drain owns accepted writes even if the caller closes or
/// drops its result task. Only the result waiter is cancellable. Disk I/O and
/// parsing happen on the background executor; no queue lock covers either.
///
/// Submit a synchronous operation using `edit`/`try_edit`, never a pre-read
/// document or an operation that waits on another submission to this directory.
/// Errors are returned to the caller unchanged; a panicking operation cannot
/// strand later jobs. Process exit remains best-effort, like existing saves.
pub(crate) fn submit<T: Send + 'static>(
    dir: &Path,
    executor: &gpui::BackgroundExecutor,
    operation: impl FnOnce() -> T + Send + 'static,
) -> gpui::Task<T> {
    let writer = writer(dir);
    let (tx, rx) = async_channel::bounded(1);
    let start = {
        let mut pending = writer.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.jobs.push_back(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
            let _ = tx.try_send(result);
        }));
        !std::mem::replace(&mut pending.running, true)
    };
    if start {
        executor
            .spawn(async move {
                loop {
                    let job = {
                        let mut pending = writer.pending.lock().unwrap_or_else(|e| e.into_inner());
                        match pending.jobs.pop_front() {
                            Some(job) => job,
                            None => {
                                pending.running = false;
                                break;
                            }
                        }
                    };
                    job();
                }
            })
            .detach();
    }
    executor.spawn(async move {
        match rx
            .recv()
            .await
            .expect("accepted config write has a completion")
        {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// The path a layered config document lives at, refusing every layer but
/// [`Layer::User`] *before* touching the filesystem.
///
/// The layer guard lives here, on the one function both [`write`] and
/// [`edit`] must call to learn where to write, so there is no way to
/// reach a write without passing it. Only the user layer is ever written
/// from inside the running app (spec §3.1): the builtin layer is
/// compiled in, and the desk layer is shared state a single trader's
/// running app has no business rewriting.
pub(crate) fn doc_path(user_dir: &Path, layer: Layer, doc: &str) -> Result<PathBuf, String> {
    if layer != Layer::User {
        return Err(format!(
            "refusing to write the {} layer's {doc}.toml: only the user layer is writable \
             from inside the app",
            layer.name()
        ));
    }
    Ok(user_dir.join(format!("{doc}.toml")))
}

/// Atomically replace one layered config document with `text`.
///
/// **This is a whole-file replacement and deliberately bypasses
/// [`edit`]'s untouched-on-parse-failure refusal**: nothing here reads,
/// parses, or preserves whatever is on disk, so a user's comments, key
/// order and unrelated tables are gone the moment it succeeds. That is
/// right only when the caller already holds the complete document it
/// means to write as a deliberate replacement. A caller setting one key
/// wants [`edit`] instead; picking this one there silently discards hand-edits.
pub fn write(user_dir: &Path, layer: Layer, doc: &str, text: &str) -> Result<(), String> {
    let path = doc_path(user_dir, layer, doc)?;
    let writer = writer(user_dir);
    let _guard = writer.transaction.lock().unwrap_or_else(|e| e.into_inner());
    write_file(&path, text)
}

/// Read (or create) one layered config document, apply `f` to it, and
/// write it back atomically — the shape every keyed persist in this
/// crate needs. The directory lock covers the read through the final rename;
/// the mutation closure must not recursively write config in that directory.
/// UI callers submit this operation through the FIFO before spawning work.
///
/// `toml_edit`, not the plain `toml` crate: a hand-written config can
/// carry comments and keys this write knows nothing about, and only a
/// format-preserving editor can touch just the one key involved.
///
/// An existing file that fails to parse is an `Err` and is left
/// byte-for-byte untouched — a user's hand-edited file, however broken,
/// is theirs and must never be destroyed by a UI-driven toggle. A
/// missing file is created fresh with `config_version = 1` at the top,
/// matching every other config document this codebase writes.
pub fn edit(
    user_dir: &Path,
    layer: Layer,
    doc: &str,
    f: impl FnOnce(&mut DocumentMut),
) -> Result<(), String> {
    try_edit(user_dir, layer, doc, |document| {
        f(document);
        Ok(())
    })
}

/// A fallible mutation with a result, under the same read–modify–write lock.
/// Validation failure or unwind leaves the original file untouched. The
/// closure must not recursively write config in the same directory.
pub(crate) fn try_edit<T>(
    user_dir: &Path,
    layer: Layer,
    doc: &str,
    f: impl FnOnce(&mut DocumentMut) -> Result<T, String>,
) -> Result<T, String> {
    let path = doc_path(user_dir, layer, doc)?;
    let writer = writer(user_dir);
    let _guard = writer.transaction.lock().unwrap_or_else(|e| e.into_inner());
    let mut document = open_at(&path)?;
    let result = f(&mut document)?;
    write_file(&path, &document.to_string())?;
    Ok(result)
}

/// The read-or-create-and-parse half of [`edit`], by path.
fn open_at(path: &Path) -> Result<DocumentMut, String> {
    let existed = path.exists();

    let mut doc = if existed {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        text.parse::<DocumentMut>().map_err(|e| {
            format!(
                "failed to parse {}: {e} (file left untouched)",
                path.display()
            )
        })?
    } else {
        DocumentMut::new()
    };

    if !existed {
        // The stamp every layered doc carries, from the one constant the
        // loader checks it against (`config::load`) — a literal here
        // would be a second copy to remember when the schema moves, and
        // this door is now the only place a new file gets one at all.
        doc["config_version"] = value(CONFIG_VERSION);
    }

    Ok(doc)
}

/// Process-global counter giving every [`write_file`] call in this
/// process a temp filename distinct from every other *concurrent* call,
/// on top of the pid already distinguishing this process from any other
/// racing on the same file. `Ordering::Relaxed` is enough — this needs
/// distinct values, not a synchronization point with any other memory
/// access.
///
/// The fixed temp name this replaced was a real race: two writers
/// sharing one temp filename could interleave — one call's
/// `File::create` truncating the other's in-progress write, or one
/// call's `rename` consuming the other's temp file out from under it
/// (an `ENOENT` surfaced as a spurious warning even though the first
/// writer's data was fine). A unique name per call removes that
/// interleaving entirely: each writer only ever touches its own file
/// until its own `rename`.
///
/// Layered config mutations are serialized above this primitive. Raw whole-file
/// replacements (session snapshots) and writers in other processes still rely
/// only on atomic rename, not on a cross-process ordering guarantee.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically write `text` to `path`: a unique temp file in the *same*
/// directory (rename is only atomic within a filesystem), `fsync`, then
/// rename over `path` — a crash or a concurrent reader never observes a
/// partial write. Creates the parent directory if it does not exist yet.
///
/// `pub(crate)` rather than private because `session::write_atomic`
/// writes `session.toml`, which is not a *layered* config document — it
/// is per-machine session state, deliberately excluded from
/// `reload::scan` — so it cannot come through [`write`]'s
/// layer-and-doc-name door, but must still be the same one atomic write.
///
/// This is real, potentially-blocking file I/O. Callers driven by UI
/// events must run it on a background executor, never inline on the
/// render thread (spec §7: nothing may stall the render thread).
pub(crate) fn write_file(path: &Path, text: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;

    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(tmp_file_name(path, pid, counter));
    {
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("failed to create {}: {e}", tmp_path.display()))?;
        file.write_all(text.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", tmp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("failed to sync {}: {e}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).map_err(|e| {
        format!(
            "failed to rename {} to {}: {e}",
            tmp_path.display(),
            path.display()
        )
    })
}

/// The temp file's name for one atomic write to `path` (M9, 3b final
/// review): derived from `path`'s own file name rather than hardcoded,
/// because this one writer now stages `app.toml`, `groupings.toml`,
/// `scopes.toml`, `keymap.toml` and `session.toml`, and a name naming
/// the wrong file lies to anyone who finds one after a crash. Kept as a
/// separate pure function so the naming can be tested without touching a
/// filesystem.
fn tmp_file_name(path: &Path, pid: u32, counter: u64) -> String {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("geode-write");
    format!(".{file_name}.{pid}-{counter}.tmp")
}

#[cfg(test)]
mod tmp_file_name_tests {
    use super::*;

    #[test]
    fn the_temp_name_derives_from_the_target_file_not_a_hardcoded_app_toml() {
        // M9: this writer stages more than app.toml
        // (groupings.toml via frame's slot save, keymap.toml, session.toml),
        // so a name hardcoded to `.app.toml.*` lied about what it staged.
        assert_eq!(
            tmp_file_name(Path::new("/x/groupings.toml"), 7, 3),
            ".groupings.toml.7-3.tmp"
        );
        assert_eq!(
            tmp_file_name(Path::new("/x/app.toml"), 7, 4),
            ".app.toml.7-4.tmp"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::Layer;

    #[test]
    fn write_is_atomic_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested");
        write(&sub, Layer::User, "app", "config_version = 1\n").expect("write");
        assert_eq!(
            std::fs::read_to_string(sub.join("app.toml")).unwrap(),
            "config_version = 1\n"
        );
        // No temp file survives a successful write.
        let strays: Vec<_> = std::fs::read_dir(&sub)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy() != "app.toml")
            .collect();
        assert!(strays.is_empty(), "temp files must not survive: {strays:?}");
    }

    /// The whole point of `edit`: a user's comments and unrelated keys are
    /// theirs, and a keyed persist must not eat them.
    #[test]
    fn edit_preserves_comments_and_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.toml"),
            "# mine\nconfig_version = 1\n\n[ui]\nfont_size = \"small\"\n\n[theme]\nname = \"Ayu Dark\"\n",
        ).unwrap();
        edit(dir.path(), Layer::User, "app", |doc| {
            doc["theme"]["name"] = toml_edit::value("Bloomberg");
        })
        .expect("edit");
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains(r#"font_size = "small""#), "{text}");
        assert!(text.contains(r#"name = "Bloomberg""#), "{text}");
    }

    /// A file the user hand-edited into a broken state is theirs. Refuse,
    /// leave it byte-for-byte, and say so — the contract
    /// `fontsize::persist_to_user_config` already keeps.
    #[test]
    fn edit_refuses_an_unparseable_file_without_touching_it() {
        let dir = tempfile::tempdir().unwrap();
        let broken = "config_version = = 1\n";
        std::fs::write(dir.path().join("app.toml"), broken).unwrap();
        let err = edit(dir.path(), Layer::User, "app", |doc| {
            doc["theme"]["name"] = toml_edit::value("x");
        })
        .expect_err("must refuse");
        assert!(err.contains("app.toml"), "the error names the file: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("app.toml")).unwrap(),
            broken,
            "the file must be untouched"
        );
    }

    /// Only the user layer is writable. Desk and builtin are refused rather
    /// than attempted, so a bug cannot write a shared desk file.
    #[test]
    fn only_the_user_layer_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        for layer in [Layer::Builtin, Layer::Desk] {
            assert!(
                write(dir.path(), layer, "app", "x = 1\n").is_err(),
                "{layer:?}"
            );
            assert!(edit(dir.path(), layer, "app", |_| {}).is_err(), "{layer:?}");
        }
        assert!(!dir.path().join("app.toml").exists(), "nothing was written");
    }

    /// `edit` on a doc that does not exist yet creates it — every keyed
    /// persist starts from no file on a fresh install.
    #[test]
    fn edit_creates_a_missing_doc() {
        let dir = tempfile::tempdir().unwrap();
        edit(dir.path(), Layer::User, "app", |doc| {
            doc["ui"]["font_size"] = toml_edit::value("large");
        })
        .expect("edit");
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("large"), "{text}");
    }
    #[gpui::test]
    async fn queued_edits_keep_submission_order_even_when_results_are_dropped(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let executor = cx.update(|cx| cx.background_executor().clone());
        // Queue the complete burst before the test executor runs any job.
        // Dropping a result waiter must not cancel an accepted save.
        for n in 0..32 {
            let path = dir.path().to_path_buf();
            drop(submit(dir.path(), &executor, move || {
                edit(&path, Layer::User, "app", |doc| {
                    doc["last"] = value(n);
                    doc[&format!("key_{n}")] = value(n);
                })
                .unwrap();
            }));
        }
        let path = dir.path().to_path_buf();
        let text = submit(dir.path(), &executor, move || {
            std::fs::read_to_string(path.join("app.toml")).unwrap()
        })
        .await;
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(doc["last"].as_integer(), Some(31));
        for n in 0..32 {
            assert_eq!(doc[&format!("key_{n}")].as_integer(), Some(n));
        }
        // The idle-to-running transition works again after the first drain.
        let path = dir.path().to_path_buf();
        submit(dir.path(), &executor, move || {
            edit(&path, Layer::User, "app", |doc| doc["last"] = value(32))
        })
        .await
        .unwrap();
        let doc: DocumentMut = std::fs::read_to_string(dir.path().join("app.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(doc["last"].as_integer(), Some(32));
    }

    #[gpui::test]
    async fn a_failed_or_panicking_save_does_not_strand_later_writes(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let executor = cx.update(|cx| cx.background_executor().clone());
        let path = dir.path().to_path_buf();
        let failed = submit(dir.path(), &executor, move || {
            try_edit(&path, Layer::User, "app", |doc| {
                doc["uncommitted"] = value(true);
                Err::<(), _>("rejected".to_string())
            })
        });
        let path = dir.path().to_path_buf();
        drop(submit(dir.path(), &executor, move || {
            edit(&path, Layer::User, "app", |doc| {
                doc["uncommitted"] = value(true);
                panic!("injected config mutation panic");
            })
        }));
        let path = dir.path().to_path_buf();
        let saved = submit(dir.path(), &executor, move || {
            edit(&path, Layer::User, "app", |doc| doc["saved"] = value(true))
        });
        assert_eq!(failed.await, Err("rejected".to_string()));
        saved.await.unwrap();
        let doc: DocumentMut = std::fs::read_to_string(dir.path().join("app.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(doc["saved"].as_bool(), Some(true));
        assert!(doc.get("uncommitted").is_none());
    }

    #[test]
    fn concurrent_edits_read_after_the_previous_commit() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let (entered, entry) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (second_entered, second_entry) = mpsc::channel();
        let (starting, started) = mpsc::channel();
        std::thread::scope(|scope| {
            let first = scope.spawn(move || {
                edit(path, Layer::User, "app", |doc| {
                    doc["first"] = value(1);
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(5)).unwrap();
                })
            });
            entry.recv_timeout(Duration::from_secs(5)).unwrap();
            let second = scope.spawn(move || {
                starting.send(()).unwrap();
                edit(path, Layer::User, "app", |doc| {
                    second_entered.send(()).unwrap();
                    doc["second"] = value(2);
                })
            });
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            let early = second_entry.recv_timeout(Duration::from_millis(100));
            // Release even on a regression, so the failure never deadlocks.
            release.send(()).unwrap();
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
            assert!(
                matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
                "second edit entered before the first committed"
            );
        });
        let doc: DocumentMut = std::fs::read_to_string(path.join("app.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(doc["first"].as_integer(), Some(1));
        assert_eq!(doc["second"].as_integer(), Some(2));
    }
}
