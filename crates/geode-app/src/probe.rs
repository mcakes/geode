//! Wiring for the throwaway data probe (spec §7's vertical slice).
//!
//! `geode-app` is the only crate that sees both the shell and the data
//! layer (CLAUDE.md), so the service lives here and the tile
//! (`geode_shell::dataprobe`) only renders what this pushes into it.
//! Deleted along with the tile when the blotter lands.
//!
//! **Opt-in, and silent when off.** The probe runs only when
//! `GEODE_PROBE_DIR` names a directory of source files *and* the layered
//! config declares at least one dataset and one view. Absent any of those
//! this returns without opening a database, because the shell must start
//! normally for someone who has no data configured — invalid or missing
//! config never stops the app (spec §10.1).
//!
//! An environment variable rather than a config key on purpose: a config
//! key is a promise to keep, and this whole file is scheduled for
//! deletion. Reading `[sources]` properly is the ingest scheduler's job,
//! not the probe's.
//!
//! **Why a thread rather than a background task.** `DataService` owns a
//! DuckDB connection, which is not `Sync`, so it cannot live inside a
//! gpui future. One dedicated thread owns the service for its whole life
//! and publishes readings over a channel; the foreground task only drains
//! that channel. That is also the threading model the spec describes
//! (§2): the UI thread reads prepared snapshots and never touches a
//! connection.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant, SystemTime};

use geode_core::config::Config;
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::snapshot::Snapshot;
use geode_core::view::ViewSpec;
use geode_data::ingest::{IngestEvent, IngestRunner, build_plan};
use geode_data::query::AsOf;
use geode_data::service::{DataService, DataServiceConfig};
use geode_data::source::{Priority, Readiness, SourceSpec, discover};
use geode_data::store::{Catalog, Store};
use geode_shell::dataprobe::ProbeState;
use geode_shell::shell::ShellView;
use gpui::{App, AsyncApp, Entity};

/// How deep the probe materializes. The tile shows a flat first
/// screenful, so two levels is what it can actually display — and passing
/// `usize::MAX` here would quietly reintroduce the unbounded query the
/// depth bound exists to prevent (`docs/perf.md`).
const MAX_DEPTH: usize = 2;

/// How often the probe re-queries. Slow on purpose: this is a diagnostic,
/// and a fast timer would turn an idle app into a busy one — the same
/// discipline the perf overlay follows.
const REQUERY_INTERVAL: Duration = Duration::from_secs(5);

/// How often the foreground task looks for a new reading. Cheap: a
/// `try_recv` on an empty channel, and no `cx.notify()` unless one
/// arrived, so an idle probe costs no frames.
const DRAIN_INTERVAL: Duration = Duration::from_millis(250);

/// What one probe cycle produced.
struct Reading {
    snapshot: Option<Snapshot>,
    freshness: Vec<(String, String, i64)>,
    query_micros: u64,
    error: Option<String>,
}

/// Everything the probe needs, resolved from config and the environment.
#[derive(Clone)]
struct Setup {
    schema: SchemaSpec,
    views: Vec<ViewSpec>,
    dimensions: DerivedDimensions,
    dataset: String,
    view: String,
    db_path: PathBuf,
    source_dir: PathBuf,
}

fn setup(config: &Config) -> Option<Setup> {
    let source_dir = PathBuf::from(std::env::var_os("GEODE_PROBE_DIR")?);
    if !source_dir.is_dir() {
        eprintln!("[probe] GEODE_PROBE_DIR is not a directory: {source_dir:?}");
        return None;
    }
    // `datasets` and `views` are what the probe is for, so their absence
    // means there is nothing to probe. `dimensions` is optional — a
    // config with no derived dimensions is an ordinary config, not a
    // reason to disable the probe.
    //
    // Said out loud, because the silence was the whole problem: with
    // GEODE_PROBE_DIR set and no datasets.toml, this returned `None`,
    // `start` was never called, and the tile sat on "no query has returned
    // yet" for the life of the session with nothing anywhere saying why.
    let missing: Vec<&str> = ["datasets", "views"]
        .into_iter()
        .filter(|doc| config.doc(doc).is_none())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "[probe] GEODE_PROBE_DIR is set but the config has no {}. \
             The probe needs {} in GEODE_DESK_CONFIG (or the user config \
             directory) and will stay idle without them.",
            missing
                .iter()
                .map(|d| format!("`{d}`"))
                .collect::<Vec<_>>()
                .join(" or "),
            missing
                .iter()
                .map(|d| format!("{d}.toml"))
                .collect::<Vec<_>>()
                .join(" and "),
        );
        return None;
    }
    let (schema, schema_warnings) = SchemaSpec::from_doc(config.doc("datasets")?);
    let (views, view_warnings) = ViewSpec::from_doc(config.doc("views")?);
    let (dimensions, dimension_warnings) = match config.doc("dimensions") {
        Some(doc) => DerivedDimensions::from_doc(doc),
        None => (DerivedDimensions::default(), Vec::new()),
    };
    for warning in schema_warnings
        .iter()
        .chain(view_warnings.iter())
        .chain(dimension_warnings.iter())
    {
        eprintln!("[probe] warning: {warning}");
    }

    let Some(view) = views.first() else {
        eprintln!("[probe] views.toml declares no views, so there is nothing to query");
        return None;
    };
    let dataset = view.dataset.clone();
    if schema.dataset(&dataset).is_none() {
        eprintln!(
            "[probe] view '{}' names undeclared dataset '{dataset}'",
            view.name
        );
        return None;
    }
    let view_name = view.name.clone();
    Some(Setup {
        schema,
        views,
        dimensions,
        dataset,
        view: view_name,
        // The temp dir, not the config dir: a database is not config,
        // and inventing a state-directory convention for a file that gets
        // deleted with the probe would leave phase 3 honouring it.
        db_path: std::env::temp_dir().join("geode-probe.duckdb"),
        source_dir,
    })
}

/// Ingest whatever is ready in the source directory, blocking until the
/// queue drains. Runs once, before the service opens: DuckDB gives one
/// writer per file, so an ingest `Store` held open alongside the
/// service's would contend for it.
fn ingest(setup: &Setup) -> Result<(), String> {
    let ds = setup
        .schema
        .dataset(&setup.dataset)
        .expect("checked in setup")
        .clone();
    let store = Store::open(&setup.db_path).map_err(|e| format!("{e:?}"))?;
    store.apply_schema(&ds).map_err(|e| format!("{e:?}"))?;
    let candidates = {
        let catalog = Catalog::new(store.writer());
        catalog.ensure_tables().map_err(|e| format!("{e:?}"))?;
        let spec = source_spec(setup);
        discover(&spec, &catalog, SystemTime::now()).map_err(|e| format!("{e:?}"))?
    };
    let plan = build_plan(&[(source_spec(setup), candidates)]);
    if plan.items.is_empty() {
        return Ok(());
    }

    let (handle, events) = IngestRunner::spawn(store, ds, setup.dataset.clone());
    handle.submit(plan);
    // Drain to the first PlanComplete, then stop: the probe ingests once.
    while let Ok(event) = events.recv() {
        match event {
            IngestEvent::Failed { batch, reason } => {
                eprintln!("[probe] ingest failed for {batch}: {reason}");
            }
            IngestEvent::PlanComplete => break,
            IngestEvent::Published { .. } => {}
        }
    }
    handle.shutdown();
    Ok(())
}

fn source_spec(setup: &Setup) -> SourceSpec {
    SourceSpec {
        name: "probe".into(),
        dataset: setup.dataset.clone(),
        paths: vec![format!("{}/*.csv", setup.source_dir.display())],
        readiness: Readiness::Sentinel,
        priority: Priority::LatestRisk,
        poll_interval: REQUERY_INTERVAL,
        pending_timeout: Duration::from_secs(60),
        batch_pattern: None,
    }
}

/// One submit-and-wait cycle, timed end to end — the §7.1 path minus the
/// paint, which is the half the benchmarks already cover.
fn query_once(service: &DataService, setup: &Setup) -> Reading {
    let freshness = || {
        service
            .freshness(&setup.dataset)
            .unwrap_or_default()
            .into_iter()
            .map(|(book, as_of)| (book, as_of.to_rfc3339(), 0))
            .collect::<Vec<_>>()
    };

    let started = Instant::now();
    if let Err(e) = service.query(&setup.view, &Scope::default(), AsOf::Live, MAX_DEPTH) {
        return Reading {
            snapshot: None,
            freshness: freshness(),
            query_micros: 0,
            error: Some(format!("{e:?}")),
        };
    }
    let result = service
        .query_results()
        .recv_timeout(Duration::from_secs(30))
        .ok();
    let query_micros = started.elapsed().as_micros() as u64;

    let (snapshot, error) = match result {
        None => (None, Some("no result within 30s".to_string())),
        Some(result) => match result.snapshot {
            Ok(snapshot) => (Some(snapshot), None),
            Err(reason) => (None, Some(reason)),
        },
    };
    Reading {
        snapshot,
        freshness: freshness(),
        query_micros,
        error,
    }
}

/// Own the service for its whole life and publish readings. Everything
/// that touches a connection happens here, off the UI thread.
fn run(setup: Setup, out: std::sync::mpsc::Sender<Reading>) {
    if let Err(reason) = ingest(&setup) {
        let _ = out.send(Reading {
            snapshot: None,
            freshness: Vec::new(),
            query_micros: 0,
            error: Some(format!("ingest: {reason}")),
        });
        return;
    }
    let service = match DataService::open(DataServiceConfig {
        db_path: setup.db_path.clone(),
        schema: setup.schema.clone(),
        views: setup.views.clone(),
        dimensions: setup.dimensions.clone(),
        query_workers: 2,
    }) {
        Ok(service) => service,
        Err(e) => {
            let _ = out.send(Reading {
                snapshot: None,
                freshness: Vec::new(),
                query_micros: 0,
                error: Some(format!("open: {e:?}")),
            });
            return;
        }
    };

    // Stops when the receiver drops, which is when the window closes.
    // One stderr line per reading: the probe is opt-in and this is the
    // only way to confirm the whole path ran without watching the tile.
    while {
        let reading = query_once(&service, &setup);
        match (&reading.snapshot, &reading.error) {
            (_, Some(error)) => eprintln!("[probe] {}: {error}", setup.view),
            (Some(snapshot), None) => eprintln!(
                "[probe] {}: {} rows in {:.1} ms",
                setup.view,
                snapshot.rows(),
                reading.query_micros as f64 / 1000.0
            ),
            (None, None) => {}
        }
        out.send(reading).is_ok()
    } {
        std::thread::sleep(REQUERY_INTERVAL);
    }
    service.shutdown();
}

/// The probe's resolved inputs. Opaque: the binary only carries one from
/// [`prepare`] to [`start`].
pub struct Prepared(Setup);

/// Resolve the probe's inputs while the config is still in hand. `None`
/// means the probe is not configured, which is the normal case.
pub fn prepare(config: &Config) -> Option<Prepared> {
    setup(config).map(Prepared)
}

/// Start the probe against an open shell. Returns immediately.
pub fn start(prepared: Prepared, shell: Entity<ShellView>, cx: &mut App) {
    let setup = prepared.0;
    let (tx, rx) = channel();
    if std::thread::Builder::new()
        .name("geode-probe".into())
        .spawn(move || run(setup, tx))
        .is_err()
    {
        eprintln!("[probe] could not start its thread");
        return;
    }
    drain(rx, shell, cx);
}

/// Move readings onto the UI thread. Nothing here blocks and nothing
/// notifies unless a reading actually arrived.
///
/// Holds the shell weakly: a diagnostic must not be what keeps the window
/// alive, and the failed update is how the loop learns to stop.
fn drain(rx: Receiver<Reading>, shell: Entity<ShellView>, cx: &mut App) {
    let shell = shell.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            match rx.try_recv() {
                Ok(reading) => {
                    let pushed = shell.update(cx, |shell, cx| {
                        shell.set_probe(
                            ProbeState {
                                snapshot: reading.snapshot.map(std::sync::Arc::new),
                                freshness: reading.freshness,
                                query_micros: reading.query_micros,
                                error: reading.error,
                            },
                            cx,
                        );
                    });
                    if pushed.is_err() {
                        return; // the window is gone
                    }
                }
                // The probe thread ended; nothing more will arrive.
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {}
            }
            cx.background_executor().timer(DRAIN_INTERVAL).await;
        }
    })
    .detach();
}
