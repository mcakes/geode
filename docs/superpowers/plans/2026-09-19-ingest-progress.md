# Ingest Progress Implementation Plan (Phase 3, slice 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** While the ingest runner is loading a file or publishing a document, the status bar shows a 2 px indeterminate strip along its top edge and a `loading <source> · <n> queued` segment; both vanish the moment the load ends, so an idle window pays nothing.

**Architecture:** The runner emits `IngestEvent::Started { source, path, queued }` after it pops a job (outside the queue lock); the service forwards it as `DataEvent::Loading` and, on `Published` or `Failed`, sends `DataEvent::LoadEnded { source }` — an explicit end event, because a failed load's `Health` may be deduplicated away and the strip must never stick. The bridge writes both into the shell's `Diagnostics` entity (`ingest: Option<IngestActivity>`, its label prepared once per event, never per frame); `status::status_bar` paints gpui-kit `Progress::loading(true)` as an absolutely-positioned 2 px overlay on the bar's top edge (no layout moves when a load starts or ends) plus a text segment; the diagnostics tile's sources section shows the same record. The runner is one thread fed by one FIFO channel, so loads are strictly sequential and a `LoadEnded` always ends the current `Loading` — no path matching is needed.

**Tech Stack:** Rust; gpui-pre 0.3.5; gpui-component 0.6.2 (`gpui_component::progress::Progress`, `Sizable`/`Size::Size(px)`; `Progress::loading(true)` honours `reduce_motion` itself); `std::time::SystemTime`.

**Spec:** `docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md` §5.3. Two amendments recorded in Task 4: the end signal is an explicit `DataEvent::LoadEnded` rather than "the matching `Published`/`Failed` by path" (a failed load's `Health` event is deduplicated by the health tracker and may not arrive at all; and one runner on one FIFO channel makes loads sequential, so path matching guards nothing); and the strip is an absolute overlay on the bar's top edge rather than a sibling above it (so the tile area does not shrink by 2 px when a load starts).

## Global Constraints

- Charter: nothing may stall the render thread; per-frame heap churn is a defect — the strip's label is prepared in `note_loading` (once per event), the status bar clones an `Rc<str>`/`SharedString`, and the `Progress` animation runs only while the strip is painted (gpui redraws only while an animation is live; an idle window draws no frame). The sink is never called while the queue mutex is held (`runner.rs`'s own rule).
- Spec §5.3: `Spinner` is ruled out (user, 2026-09-17); the indicator is gpui-kit `Progress::new(..).loading(true)` at 2 px, full width, on the bar's top edge, themed `primary` by the component's default; the segment reads `loading <source> · <n> queued`; nothing paints when idle.
- Data layering: `geode-shell` and `geode-data` never depend on each other; the bridge (`geode-app`) is where the event meets the entity. `Diagnostics` mutators bump `versions.sources` (the sources section paints the record) and `version` (the summary cache key) — the same pattern `note_health`/`note_polled` follow.
- TDD: runner and service tests in `geode-data`; `Diagnostics` unit tests without a window; `sources_rows` (pure) in `geode-diagnostics`; the status bar with `#[gpui::test]` driving the entity; the bridge with the existing `tx.try_send(DataEvent::…)` fixture. No existing test's assertions change.
- The four CI checks plus the `test-support` check stay green; `zsh scripts/mutation-check.sh --anchors-only` exits 0; every behaviour this plan adds gets a harness entry (Task 4) and CLAUDE.md line 85's count is bumped to `grep -c "^run_mutation " scripts/mutation-check.sh`.
- Commits small and per task, each with the harness-supplied `Co-Authored-By` trailer. Never commit `TODO.md` or `docs/modules.md`.
- Worktree branch off `main`: `worktree-ingest-progress`.
- Out of scope: a determinate `Progress` (a CSV load has no known fraction), a per-tile skeleton or requery spinner (offered, not taken), discovery-poll progress, the query pool.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-data/src/ingest/runner.rs` | `IngestEvent::Started`; emitted after `take_work` with the lock released (Task 1) |
| `crates/geode-data/src/service.rs` | `DataEvent::Loading`, `DataEvent::LoadEnded`; the sink's `Started`/`Published`/`Failed` arms (Task 1) |
| `crates/geode-shell/src/diagnostics.rs` | `IngestActivity`, `Diagnostics.ingest`, `note_loading`, `note_load_ended` (Task 2) |
| `crates/geode-shell/src/shell/status.rs`, `shell/render.rs` | the strip overlay + segment; the call site (Task 2) |
| `crates/geode-diagnostics/src/sections.rs` | the `loading …` row under a source (Task 2) |
| `crates/geode-app/src/bridge.rs` | the two new arms (Task 3) |
| `scripts/mutation-check.sh`, `CLAUDE.md`, the spec | harness, count, §5.3 as-built (Task 4) |

---

### Task 1: `IngestEvent::Started` → `DataEvent::Loading` / `DataEvent::LoadEnded`

**Files:**
- Modify: `crates/geode-data/src/ingest/runner.rs:60-85` (the enum), `:504-560` (the pop site), its `mod tests`
- Modify: `crates/geode-data/src/service.rs:66-100` (`DataEvent`), `:753-890` (the ingest sink), its `mod tests`
- Test: those two files' `mod tests`

**Interfaces:**
- Consumes: `take_work(&mut Queue) -> Option<Work>` (documents first), `Work::{Document(DocumentJob), File(WorkItem)}`, `WorkItem { source, dataset, batch, candidate: Candidate { csv_path: PathBuf, .. }, .. }`, `DocumentJob { source, dataset, .. }`, `IngestSink`.
- Produces:
  - `IngestEvent::Started { source: String, path: String, queued: usize }` — `path` is the file's `csv_path` (lossy string) or `document://{source}/{dataset}` for a document (the batch is not known until the rows are read; the display never needs it); `queued` is the number of items still waiting behind it (files + documents) at the instant it was popped.
  - `DataEvent::Loading { source: String, path: String, queued: usize }`; `DataEvent::LoadEnded { source: String }` — sent after the `Published` send and after the `Failed` arm's health send, unconditionally (not gated on the health dedupe).

- [ ] **Step 1: Failing runner test**

In `runner.rs`'s `mod tests`, beside the test that drives two files through `spawn_channel` (grep `spawn_channel` there and copy its store/schema/submit setup):

```rust
    #[test]
    fn started_precedes_each_publish_and_counts_what_is_still_queued() {
        // Two files submitted back to back: the first pops with one item
        // still behind it, the second with none. Every Started precedes
        // its own Published, and the path is the file's own.
        let (store, ds, dir) = /* the fixture the neighbouring two-file test builds */;
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        let a = /* the neighbouring test's first WorkItem */;
        let b = /* its second */;
        let a_path = a.candidate.csv_path.to_string_lossy().to_string();
        let b_path = b.candidate.csv_path.to_string_lossy().to_string();
        handle.submit(vec![a, b]);
        let events = drain(&rx, 2);
        let started: Vec<(String, usize)> = events
            .iter()
            .filter_map(|e| match e {
                IngestEvent::Started { path, queued, .. } => Some((path.clone(), *queued)),
                _ => None,
            })
            .collect();
        assert_eq!(started, vec![(a_path, 1), (b_path, 0)]);
        // Ordering: Started(a) < Published(a) < Started(b) < Published(b).
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                IngestEvent::Started { .. } => "started",
                IngestEvent::Published { .. } => "published",
                IngestEvent::Failed { .. } => "failed",
                IngestEvent::PlanComplete => "drained",
            })
            .filter(|k| *k != "drained")
            .collect();
        assert_eq!(kinds, ["started", "published", "started", "published"]);
        handle.shutdown();
    }

    #[test]
    fn a_document_job_starts_with_its_synthetic_path() {
        let (store, ds, _dir) = /* the fixture the document-publish runner test builds (grep `submit_document`) */;
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit_document(/* the neighbouring document test's DocumentJob for source "cvi", dataset "cvi_params" */);
        let events = drain(&rx, 1);
        let started = events.iter().find_map(|e| match e {
            IngestEvent::Started { source, path, queued } => Some((source.clone(), path.clone(), *queued)),
            _ => None,
        });
        assert_eq!(started, Some(("cvi".to_string(), "document://cvi/cvi_params".to_string(), 0)));
        handle.shutdown();
    }
```

Replace each `/* … */` with the neighbouring tests' real fixture code — copy, do not invent (the file has both a two-file test and a document test). If `handle.submit` takes a `WorkPlan` rather than a `Vec`, use that shape. The `drain` helper stops after N `Published`/`Failed`, so `Started`s before them are collected.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data started_precedes a_document_job_starts 2>&1 | grep -E "error\[|no variant|panicked" | head -3`
Expected: compile error — `IngestEvent::Started` does not exist.

- [ ] **Step 3: Implement the event**

Add to `IngestEvent` (before `Published`):

```rust
    /// The runner popped a job and is about to load it (spec 2026-09-17
    /// §5.3): `path` is the file's own path, or `document://{source}/
    /// {dataset}` for a document (its batch is not known until the rows
    /// are read, and the strip never needs it); `queued` is how many
    /// items — files and documents — still waited behind it at the
    /// instant it was popped. Always followed by exactly one `Published`
    /// or `Failed` for the same job: one runner, one FIFO queue.
    Started {
        source: String,
        path: String,
        queued: usize,
    },
```

In `run`'s loop, capture the depth while the lock is held and emit after it is released. Change the `let work = { … }` block so it yields `(work, queued)`: inside the inner `loop`, at `if let Some(work) = take_work(&mut q) { announced_idle = false; break (work, q.items.len() + q.documents.len()); }`; then, right after the block (lock released, before the `match work`):

```rust
        let (work, queued) = work;
        let (started_source, started_path) = match &work {
            Work::Document(job) => (
                job.source.clone(),
                format!("document://{}/{}", job.source, job.dataset),
            ),
            Work::File(item) => (
                item.source.clone(),
                item.candidate.csv_path.to_string_lossy().into_owned(),
            ),
        };
        if !sink(IngestEvent::Started {
            source: started_source,
            path: started_path,
            queued,
        }) {
            log_refused_event(&refusal_logged, "a load-started announcement");
        }
```

(`Queue.items`/`documents` are the two queues `take_work` reads — confirm the field names at `runner.rs:127`.) A refused send is logged once and ignored, the same rule every other emit here follows. Update every `match` over `IngestEvent` the compiler names (the test helper `drain`'s own match, the health tracker's, and the service's — the service arm is Step 5).

- [ ] **Step 4: Run the runner tests**

Run: `cargo test -p geode-data started_precedes a_document_job_starts 2>&1 | grep -E "^test |panicked"` → both `ok`. Then `cargo test -p geode-data ingest::runner 2>&1 | grep -E "^test result"` → 0 failed (the existing tests must tolerate the extra event — `drain` counts only `Published`/`Failed`; a test that asserts an exact event list will need the `Started`s added to its expectation, which is a legitimate update of an exact-sequence assertion, not a weakening — name each in the report).

- [ ] **Step 5: Failing service test, then the `DataEvent`s**

In `service.rs`'s `mod tests`, beside a test that opens a `DataService` with a collecting sink and drives one publish (grep `DataEvent::Published` in the tests; copy its fixture):

```rust
    #[test]
    fn a_load_is_bracketed_by_loading_and_load_ended() {
        let (service, rx, _dir) = /* the neighbouring publish test's fixture: a service whose sink is a channel */;
        /* drop the neighbouring test's one CSV into the source dir and wait for its Published, as that test does */
        let mut kinds = Vec::new();
        while let Ok(e) = rx.recv_timeout(std::time::Duration::from_secs(30)) {
            match e {
                DataEvent::Loading { ref source, ref path, queued } => {
                    assert_eq!(source, "risk");
                    assert!(path.ends_with(".csv"), "{path}");
                    assert_eq!(queued, 0);
                    kinds.push("loading");
                }
                DataEvent::Published { .. } => kinds.push("published"),
                DataEvent::LoadEnded { ref source } => {
                    assert_eq!(source, "risk");
                    kinds.push("ended");
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(kinds, ["loading", "published", "ended"]);
        drop(service);
    }
```

Add to `DataEvent`:

```rust
    /// The ingest runner popped a job (spec 2026-09-17 §5.3): the status
    /// bar's progress strip starts here. Ended by [`DataEvent::LoadEnded`].
    Loading {
        source: String,
        path: String,
        queued: usize,
    },
    /// The job announced by the last `Loading` finished — published or
    /// failed — sent unconditionally, because a failed load's `Health`
    /// is deduplicated by the tracker and may never reach the shell,
    /// and the strip must not stick. One runner on one FIFO channel
    /// makes loads sequential, so this always ends the current one.
    LoadEnded { source: String },
```

In the ingest sink: a new arm `IngestEvent::Started { source, path, queued } => sink(DataEvent::Loading { source, path, queued }),`; in the `Published` arm, after the existing `Published`/health sends (keep its `delivered` logic intact), add `let _ = sink(DataEvent::LoadEnded { source: source.clone() });` — before `source` is moved into the health call, or clone it first; in the `Failed` arm, after its health send, `let _ = sink(DataEvent::LoadEnded { source: source.clone() });`. A refused `LoadEnded` is a dropped event like any other (the bridge counts drops); it must not change the arm's existing return value.

- [ ] **Step 6: Run the service tests; fmt; clippy; commit**

Run: `cargo test -p geode-data 2>&1 | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print "passed="p" failed="f}'` → `failed=0`. Then `cargo fmt --check && cargo clippy -p geode-data --all-targets -- -D warnings 2>&1 | tail -1`. Also `cargo check --workspace --all-targets` — every consumer of `DataEvent` (the bridge, the diagnostics tests) must still match exhaustively; if the bridge has a wildcard arm nothing breaks, otherwise add `DataEvent::Loading { .. } | DataEvent::LoadEnded { .. } => {}` there for now (Task 3 replaces it).

```bash
git add crates/geode-data crates/geode-app/src/bridge.rs
git commit -m "data: IngestEvent::Started and DataEvent::Loading / LoadEnded bracket every load"
```

---

### Task 2: The entity, the strip, the segment, the sources row

**Files:**
- Modify: `crates/geode-shell/src/diagnostics.rs` (`IngestActivity`, `Diagnostics.ingest`, `note_loading`, `note_load_ended`, tests)
- Modify: `crates/geode-shell/src/shell/status.rs` (`status_bar` gains `ingest: Option<&IngestActivity>`; the overlay + segment), `crates/geode-shell/src/shell/render.rs:883-905` (pass it)
- Modify: `crates/geode-diagnostics/src/sections.rs:74-130` (`sources_rows`)
- Test: `diagnostics.rs` unit tests; `crates/geode-shell/src/shell/tests/diagnostics.rs` (window); `sections.rs` unit tests

**Interfaces:**
- Consumes: `Diagnostics { version, versions: DiagVersions, sources, .. }`, `note_health`'s bump pattern; `status::status_bar`'s existing parameters; `sections::row(text, indent, Tone)`; `gpui_component::progress::Progress`, `gpui_component::{Sizable, Size}`.
- Produces:
  - `pub struct IngestActivity { pub source: String, pub path: String, pub queued: usize, pub since: SystemTime, pub label: SharedString }` on `Diagnostics.ingest: Option<IngestActivity>` (`gpui::SharedString` — `geode-shell` already depends on gpui; a clone is a refcount bump).
  - `pub fn Diagnostics::note_loading(&mut self, source: &str, path: &str, queued: usize, at: SystemTime)` — sets `ingest` (label `"loading {source} · {queued} queued"`, or `"loading {source}"` when `queued == 0`), bumps `version` and `versions.sources`.
  - `pub fn Diagnostics::note_load_ended(&mut self)` — clears `ingest` if `Some` and bumps both; a no-op when already `None`.
  - `status_bar(.., ingest: Option<&IngestActivity>, ..)` painting `debug_selector` `ingest-strip` (the 2 px overlay) and `ingest-loading` (the segment) iff `Some`.
  - A sources-section row `loading {path} since HH:MM:SS` (indent 1, `Tone::Muted`) under the loading source.

- [ ] **Step 1: Failing unit tests for the entity**

In `diagnostics.rs`'s `mod tests`:

```rust
    #[test]
    fn note_loading_records_the_activity_and_bumps_the_sources_version() {
        let mut d = Diagnostics::default();
        let v = d.versions().sources;
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        d.note_loading("risk", "/data/risk/EOD.csv", 3, at);
        let a = d.ingest.as_ref().expect("recorded");
        assert_eq!(a.source, "risk");
        assert_eq!(a.path, "/data/risk/EOD.csv");
        assert_eq!(a.queued, 3);
        assert_eq!(a.since, at);
        assert_eq!(&*a.label, "loading risk · 3 queued");
        assert_eq!(d.versions().sources, v + 1);
        d.note_loading("cvi", "document://cvi/cvi_params", 0, at);
        assert_eq!(&*d.ingest.as_ref().unwrap().label, "loading cvi");
    }

    #[test]
    fn note_load_ended_clears_and_is_a_no_op_when_idle() {
        let mut d = Diagnostics::default();
        let v0 = d.versions().sources;
        d.note_load_ended();
        assert!(d.ingest.is_none());
        assert_eq!(d.versions().sources, v0, "nothing to clear, nothing bumps");
        d.note_loading("risk", "/x.csv", 0, SystemTime::UNIX_EPOCH);
        let v1 = d.versions().sources;
        d.note_load_ended();
        assert!(d.ingest.is_none());
        assert_eq!(d.versions().sources, v1 + 1);
    }
```

(`Diagnostics::default()` — use whatever the file's tests construct with; grep `Diagnostics::` in its tests.)

- [ ] **Step 2: Implement the entity half**

In `diagnostics.rs`, beside `SourceState`:

```rust
/// What the ingest runner is loading right now (spec 2026-09-17 §5.3),
/// set by `DataEvent::Loading` and cleared by `DataEvent::LoadEnded`.
/// `label` is prepared here, once per event, so the status bar clones a
/// `SharedString` per paint and formats nothing per frame.
#[derive(Debug, Clone)]
pub struct IngestActivity {
    pub source: String,
    pub path: String,
    pub queued: usize,
    pub since: SystemTime,
    pub label: gpui::SharedString,
}
```

field `pub ingest: Option<IngestActivity>,` on `Diagnostics` (and in its constructor/`Default`), and the two mutators beside `note_polled`:

```rust
    /// A load began (`DataEvent::Loading`). Always bumps: a new `Started`
    /// is a new record even for the same source (its path or depth moved).
    pub fn note_loading(&mut self, source: &str, path: &str, queued: usize, at: SystemTime) {
        let label: gpui::SharedString = if queued == 0 {
            format!("loading {source}").into()
        } else {
            format!("loading {source} · {queued} queued").into()
        };
        self.ingest = Some(IngestActivity {
            source: source.to_string(),
            path: path.to_string(),
            queued,
            since: at,
            label,
        });
        self.version += 1;
        self.versions.sources += 1;
    }

    /// The load ended (`DataEvent::LoadEnded`), published or failed. A
    /// no-op when nothing was recorded — an end with no start bumps
    /// nothing.
    pub fn note_load_ended(&mut self) {
        if self.ingest.take().is_some() {
            self.version += 1;
            self.versions.sources += 1;
        }
    }
```

Run: `cargo test -p geode-shell diagnostics::tests 2>&1 | grep -E "^test result"` → 0 failed.

- [ ] **Step 3: Failing window test for the strip and segment**

In `crates/geode-shell/src/shell/tests/diagnostics.rs` (the file that already asserts `diagnostics-summary`; copy its fixture — `open_shell(cx, test_services())`, `shell_of`):

```rust
#[gpui::test]
fn the_ingest_strip_and_segment_paint_only_while_a_load_is_running(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("ingest-strip").is_none(), "idle: no strip");
    assert!(vcx.debug_bounds("ingest-loading").is_none(), "idle: no segment");
    let bar_before = vcx.debug_bounds("shell-status-bar").expect("status bar painted");

    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut vcx, |d, cx| {
        d.note_loading("risk", "/data/risk/EOD.csv", 2, std::time::SystemTime::now());
        cx.notify();
    });
    vcx.run_until_parked();
    let strip = vcx.debug_bounds("ingest-strip").expect("loading: the strip paints");
    let seg = vcx.debug_bounds("ingest-loading").expect("loading: the segment paints");
    let bar = vcx.debug_bounds("shell-status-bar").unwrap();
    assert_eq!(bar.origin.y, bar_before.origin.y, "the bar did not move");
    assert_eq!(bar.size.height, bar_before.size.height, "the bar did not grow");
    assert_eq!(strip.origin.y, bar.origin.y, "the strip sits on the bar's top edge");
    assert_eq!(strip.size.height, gpui::px(2.));
    assert!(strip.size.width >= bar.size.width - gpui::px(1.), "full width");
    assert!(seg.size.width > gpui::px(0.));

    diagnostics.update(&mut vcx, |d, cx| {
        d.note_load_ended();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("ingest-strip").is_none(), "ended: strip gone");
    assert!(vcx.debug_bounds("ingest-loading").is_none(), "ended: segment gone");
}
```

`shell-status-bar`: if the bar has no such selector today, add `.debug_selector(|| "shell-status-bar".to_string())` to the wrapper this task introduces (below). The shell re-renders on the entity's notify because `ShellView` observes `Diagnostics` (`on_diagnostics_changed`) — confirm with `grep -n "observe(&diagnostics\|observe(&self.diagnostics" crates/geode-shell/src/shell/mod.rs`; if the observer only fires on `version` changes, `note_loading` bumps it.

- [ ] **Step 4: Implement the strip and segment**

In `status.rs`, `status_bar` gains a parameter `ingest: Option<&crate::diagnostics::IngestActivity>` (add it after `diagnostics_summary`/`on_diagnostics_click`; the fn is already `#[allow(clippy::too_many_arguments)]`). Add the segment beside the diagnostics summary:

```rust
    if let Some(activity) = ingest {
        bar = bar.left(
            div()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "ingest-loading".to_string())
                .child(activity.label.clone()),
        );
    }
```

(`label` is a `SharedString`; the clone is a refcount bump.) Then wrap the bar so the strip is an overlay that moves nothing:

```rust
    let bar = bar.right(div().text_color(theme.muted_foreground).child(theme_name.to_string()));
    div()
        .relative()
        .flex_none()
        .w_full()
        .h(px(HEIGHT))
        .debug_selector(|| "shell-status-bar".to_string())
        .child(bar)
        .when(ingest.is_some(), |el| {
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(2.))
                    .debug_selector(|| "ingest-strip".to_string())
                    .child(
                        Progress::new("ingest-strip")
                            .loading(true)
                            .with_size(Size::Size(px(2.)))
                            .w_full(),
                    ),
            )
        })
```

with `use gpui_component::{progress::Progress, Sizable as _, Size};` (check the exact re-export: `grep -n "pub use progress\|pub mod progress" ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-component-0.6.2/src/lib.rs`). The function's return type stays `impl IntoElement`. In `render.rs`, read the activity once with the summary: `let diagnostics_read = self.diagnostics.read(cx); let diagnostics_summary = diagnostics_read.summary(); let ingest = diagnostics_read.ingest.clone();` (an `Option<IngestActivity>` clone: two `String`s and an `Rc` per render — avoid it: pass `ingest.as_ref()` from the borrowed read if the borrow checker allows it across the `status_bar` call; if not, clone only the `label` `SharedString` and pass `Option<SharedString>`, changing the parameter to that and the `.when(ingest.is_some(), …)` gate accordingly — say which in the report). Update the call site.

- [ ] **Step 5: The sources-section row and its unit test**

In `sections.rs`'s tests (they build a `Diagnostics` by hand — copy the fixture of the test that asserts a source's `since` row):

```rust
    #[test]
    fn a_loading_source_shows_what_it_is_loading_under_its_health_row() {
        let mut d = /* the fixture with one reported source "risk" */;
        let at = std::time::SystemTime::now();
        d.note_loading("risk", "/data/risk/EOD.csv", 1, at);
        let rows = sources_rows(&d, at);
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert!(
            text.iter().any(|t| t.starts_with("loading /data/risk/EOD.csv since ")),
            "{text:?}"
        );
        d.note_load_ended();
        let rows = sources_rows(&d, at);
        assert!(!rows.iter().any(|r| r.text.starts_with("loading ")));
    }
```

Implement in `sources_rows`, right after the health row for `name` is pushed: `if let Some(a) = &d.ingest && a.source == name { out.push(row(format!("loading {} since {}", a.path, local_hms(a.since)), 1, Tone::Muted)); }` (`local_hms` is the file's existing helper; `Row.text` may be a `SharedString` — adjust the test's accessor).

- [ ] **Step 6: Run everything touched; fmt; clippy; commit**

Run: `cargo test -p geode-shell -p geode-diagnostics 2>&1 | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print "passed="p" failed="f}'` → `failed=0`; `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -1`.

```bash
git add crates/geode-shell crates/geode-diagnostics
git commit -m "shell: ingest activity on Diagnostics; the status bar's 2 px loading strip and segment; the sources row"
```

---

### Task 3: The bridge

**Files:**
- Modify: `crates/geode-app/src/bridge.rs` (the drain's `match` — the arms beside `DataEvent::Health`; the placeholder arm Task 1 may have added)
- Test: `bridge.rs` `mod tests` (the `tx.try_send(DataEvent::…)` fixture at ~867–935)

**Interfaces:**
- Consumes: `DataEvent::Loading`/`LoadEnded` (Task 1), `Diagnostics::note_loading`/`note_load_ended` (Task 2).
- Produces: nothing new.

- [ ] **Step 1: Failing bridge test**

Beside `the_drain_task_ends_on_the_first_event_after_the_window_closes`, reusing its setup up to `bridge.attach(...)` (whatever attaches the drain to the window — copy exactly):

```rust
    #[gpui::test]
    fn loading_and_load_ended_reach_the_diagnostics_entity(cx: &mut gpui::TestAppContext) {
        let window = open_test_window(cx, test_shell_services());
        /* the neighbouring test's handle/factory/channel/bridge/attach lines, verbatim */
        let shell = /* the window's ShellView entity, as the neighbouring tests read it */;
        let diagnostics = shell.read_with(cx, |s, _| s.diagnostics().clone());

        tx.try_send(DataEvent::Loading {
            source: "risk".into(),
            path: "/data/risk/EOD.csv".into(),
            queued: 4,
        })
        .unwrap();
        cx.run_until_parked();
        let recorded = diagnostics.read_with(cx, |d, _| d.ingest.clone());
        let a = recorded.expect("Loading reached the entity");
        assert_eq!((a.source.as_str(), a.path.as_str(), a.queued), ("risk", "/data/risk/EOD.csv", 4));

        tx.try_send(DataEvent::LoadEnded { source: "risk".into() }).unwrap();
        cx.run_until_parked();
        assert!(diagnostics.read_with(cx, |d, _| d.ingest.is_none()), "LoadEnded cleared it");
    }
```

- [ ] **Step 2: Implement the two arms**

In the drain's `match`, beside `DataEvent::Health { .. } => { … }`:

```rust
                    DataEvent::Loading { source, path, queued } => {
                        diagnostics.update(cx, |d, cx| {
                            d.note_loading(&source, &path, queued, SystemTime::now());
                            cx.notify();
                        });
                    }
                    DataEvent::LoadEnded { .. } => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_load_ended();
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
```

Remove any placeholder wildcard arm Task 1 added. The `source` on `LoadEnded` is ignored on purpose (sequential loads — the as-built records why).

- [ ] **Step 3: Run, fmt, clippy, commit**

Run: `cargo test -p geode-app loading_and_load_ended 2>&1 | grep -E "^test |panicked"` → `ok`; `cargo test -p geode-app 2>&1 | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print "passed="p" failed="f}'` → `failed=0`; fmt; clippy.

```bash
git add crates/geode-app/src/bridge.rs
git commit -m "app: the bridge routes Loading / LoadEnded into the diagnostics entity"
```

---

### Task 4: Harness, count, spec as-built, workspace verification

**Files:**
- Modify: `scripts/mutation-check.sh` (three entries, after the last `asof:` entry), `CLAUDE.md:85`, the spec §5.3

- [ ] **Step 1: Harness entries** (copy each `from` anchor from the committed file byte for byte; **commit first**)

```zsh
# Ingest progress (2026-09-19): `queued` is what still WAITS behind the
# popped job. Counting the job itself (+1) would paint "1 queued" for a
# lone file and never reach 0 while anything loads.
run_mutation "ingest: queued counts what waits behind the popped job" \
  crates/geode-data/src/ingest/runner.rs \
  '                    break (work, q.items.len() + q.documents.len());' \
  '                    break (work, q.items.len() + q.documents.len() + 1);' \
  geode-data \
  started_precedes_each_publish_and_counts_what_is_still_queued

# LoadEnded must be sent from the Failed arm too: a failed load's Health
# is deduplicated by the tracker, so without it the strip sticks on a
# second identical failure.
run_mutation "ingest: a failed load still ends the strip" \
  crates/geode-data/src/service.rs \
  '<the LoadEnded send line inside the Failed arm, copied exactly>' \
  '<that line commented out: prefix with // >' \
  geode-data \
  a_load_is_bracketed_by_loading_and_load_ended

# Idle costs nothing: the strip and segment exist only while `ingest` is
# Some. Painting them unconditionally survives every entity test.
run_mutation "status: the strip paints only while loading" \
  crates/geode-shell/src/shell/status.rs \
  '        .when(ingest.is_some(), |el| {' \
  '        .when(true, |el| {' \
  geode-shell \
  the_ingest_strip_and_segment_paint_only_while_a_load_is_running
```

The second entry's test must actually exercise the Failed arm — if `a_load_is_bracketed_by_loading_and_load_ended` only drives a successful publish, add a sibling service test `a_failed_load_still_ends` that drops a malformed CSV (copy the neighbouring failed-load service test's fixture) and asserts `["loading", "ended"]` (the `Failed` becomes a `Health` event the test ignores), and name THAT test in the entry. Run `--anchors-only` (exit 0), then each entry by substring → `caught`. Update CLAUDE.md line 85's count.

- [ ] **Step 2: Spec §5.3 as-built**

Append to §5.3:

```markdown
**As built (2026-09-19):** `IngestEvent::Started { source, path, queued }`
is emitted by the runner after `take_work`, with the queue lock released
(`queued` = files + documents still waiting; a document's path is
`document://{source}/{dataset}`, its batch unknown until the rows are
read); the service forwards it as `DataEvent::Loading` and sends
`DataEvent::LoadEnded { source }` after every `Published` AND every
`Failed` — an explicit end event rather than the paragraph's "the
matching `Published`/`Failed` by path", because a failed load's `Health`
is deduplicated by the tracker and may never arrive, and one runner on
one FIFO channel makes loads strictly sequential, so path matching
guards nothing (the bridge ignores `LoadEnded.source`). The shell's
`Diagnostics.ingest: Option<IngestActivity>` carries the record with its
label prepared once (`note_loading`/`note_load_ended`, both bumping
`versions.sources`); `status::status_bar` paints gpui-kit
`Progress::loading(true)` at 2 px as an ABSOLUTE overlay on the bar's top
edge — not a sibling above it, so the tile area never shrinks when a load
starts — plus a muted `loading <source> · <n> queued` segment, both only
while `ingest` is `Some`; the diagnostics tile's sources section adds
`loading <path> since HH:MM:SS` under that source. Display check pending:
the strip's colour and motion on a real window.
```

- [ ] **Step 3: Whole-workspace verification and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -1 && cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | awk '/^test result/ {p+=$4; f+=$6} /FAILED/ {print} END {print "passed="p" failed="f}' && cargo bench --workspace --no-run 2>&1 | tail -1 && cargo check -p geode-shell --features test-support --all-targets 2>&1 | tail -1 && zsh scripts/mutation-check.sh --anchors-only | tail -1`
Expected: clean / `Finished` / `failed=0` / `Finished` / `Finished` / `0 stale, 0 ambiguous`. Also a smoke run: `cargo build -p geode-app && (./target/debug/geode --demo 1000 > /tmp/geode-smoke.log 2>&1 & pid=$!; sleep 20; kill $pid; grep -iE "panic" /tmp/geode-smoke.log | head)` → no panic (the demo publishes CVI documents every ~5 s, so the strip path runs for real).

```bash
git add scripts/mutation-check.sh CLAUDE.md docs/superpowers/specs/2026-09-17-geode-gpui-kit-upgrade-and-adoption-design.md
git commit -m "docs + harness: ingest progress as built; three harness entries"
```
