//! Wiring for the throwaway data probe (spec §7's vertical slice).
//!
//! `geode-app` is the only crate that sees both the shell and the data
//! layer (CLAUDE.md), so the service lives here and the tile
//! (`geode_shell::dataprobe`) only renders what this pushes into it.
//! Deleted along with the tile when the blotter lands.
//!
//! **Opt-in, and silent when off.** The probe runs when a layered
//! `sources.toml` is present, or `GEODE_PROBE_DIR` names a directory of
//! source files — either way, only if the config also declares at least
//! one dataset and one view. Absent all of that this returns without
//! opening a database, because the shell must start normally for someone
//! who has no data configured — invalid or missing config never stops
//! the app (spec §10.1).
//!
//! `sources.toml`, when present, is read properly through
//! `SourceSpec::from_doc` — that is the ingest scheduler's real
//! configuration surface, and reading it here is this file previewing
//! what phase 3 makes permanent. `GEODE_PROBE_DIR` stays only as the
//! throwaway shortcut it always was: a one-source-over-a-directory
//! fallback for running the probe without writing a `sources.toml`,
//! since this whole file is scheduled for deletion. Either way, once a
//! source is built, the service's own scheduler does the ingesting —
//! this file never does.
//!
//! **The probe is `DataHandle`'s first consumer** (Phase 3 §9 step 1).
//! The service lives on its own thread behind the handle; this file only
//! submits queries, drains the event sink on the UI side, and pushes
//! readings into the tile. Ingest is the service's scheduler's job now —
//! the probe no longer ingests anything itself.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use geode_core::config::Config;
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{AsOf, QueryKey};
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::snapshot::Snapshot;
use geode_core::view::ViewSpec;
use geode_data::source::{Priority, Readiness, SourceSpec};
use geode_data::{DataEvent, DataService, DataServiceConfig, EventSink, QueryParams};
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
/// arrived, so an idle probe costs no frames. Still the 250 ms poll the
/// probe always had — the blotter's bridge wakes on the channel instead
/// (Phase 3 §5.1) and this file is deleted with the probe.
const DRAIN_INTERVAL: Duration = Duration::from_millis(250);

/// What one probe cycle produced.
struct Reading {
    snapshot: Option<std::sync::Arc<Snapshot>>,
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
    sources: Vec<SourceSpec>,
    view: String,
    db_path: PathBuf,
}

fn setup(config: &Config) -> Option<Setup> {
    let source_dir = std::env::var_os("GEODE_PROBE_DIR").map(PathBuf::from);
    let has_sources_doc = config.doc("sources").is_some();
    if source_dir.is_none() && !has_sources_doc {
        return None;
    }
    if let Some(dir) = &source_dir
        && !dir.is_dir()
    {
        eprintln!("[probe] GEODE_PROBE_DIR is not a directory: {dir:?}");
        return None;
    }
    // `datasets` and `views` are what the probe is for, so their absence
    // means there is nothing to probe. `dimensions` is optional — a
    // config with no derived dimensions is an ordinary config, not a
    // reason to disable the probe.
    let missing: Vec<&str> = ["datasets", "views"]
        .into_iter()
        .filter(|doc| config.doc(doc).is_none())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "[probe] the probe is enabled but the config has no {}; it will stay idle",
            missing.join(" or ")
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
    let view_name = view.name.clone();
    if schema.dataset(&dataset).is_none() {
        eprintln!("[probe] view '{view_name}' names undeclared dataset '{dataset}'");
        return None;
    }

    // sources.toml when present; otherwise the environment variable
    // builds one source over the directory, as it always did.
    let sources = match config.doc("sources") {
        Some(doc) => {
            let (sources, diags) = SourceSpec::from_doc(doc, &schema);
            for d in &diags {
                eprintln!("[probe] sources: {d}");
            }
            sources
        }
        None => vec![SourceSpec {
            name: "probe".into(),
            dataset: dataset.clone(),
            paths: vec![format!("{}/*.csv", source_dir?.display())],
            readiness: Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: REQUERY_INTERVAL,
            pending_timeout: Duration::from_secs(60),
            batch_pattern: None,
        }],
    };

    Some(Setup {
        schema,
        views,
        dimensions,
        sources,
        view: view_name,
        // The temp dir, not the config dir: a database is not config,
        // and inventing a state-directory convention for a file that gets
        // deleted with the probe would leave phase 3 honouring it.
        db_path: std::env::temp_dir().join("geode-probe.duckdb"),
    })
}

/// The probe's resolved inputs. Opaque: the binary only carries one from
/// [`prepare`] to [`start`].
pub struct Prepared(Setup);

/// Resolve the probe's inputs while the config is still in hand. `None`
/// means the probe is not configured, which is the normal case.
pub fn prepare(config: &Config) -> Option<Prepared> {
    setup(config).map(Prepared)
}

/// Start the probe against an open shell. Returns immediately: the
/// service opens on its own thread behind the handle.
pub fn start(prepared: Prepared, shell: Entity<ShellView>, cx: &mut App) {
    let setup = prepared.0;
    let (tx, rx) = channel::<DataEvent>();
    let sink: EventSink = std::sync::Arc::new(move |e| tx.send(e).is_ok());
    let handle = DataService::spawn(
        DataServiceConfig {
            db_path: setup.db_path.clone(),
            schema: setup.schema.clone(),
            views: setup.views.clone(),
            dimensions: setup.dimensions.clone(),
            query_workers: 2,
            sources: setup.sources.clone(),
        },
        sink,
    );
    drain(rx, handle, setup.view, shell, cx);
}

/// Move events onto the UI thread and requery on a timer and on every
/// publish. Nothing here blocks; `cx.notify()` only when something
/// arrived. Still the 250 ms poll the probe always had — the blotter's
/// bridge wakes on the channel instead (Phase 3 §5.1) and this file is
/// deleted with the probe.
fn drain(
    rx: Receiver<DataEvent>,
    handle: geode_data::DataHandle,
    view: String,
    shell: Entity<ShellView>,
    cx: &mut App,
) {
    let shell = shell.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut tag = 0u64;
        let mut last_query = Instant::now() - REQUERY_INTERVAL;
        let mut submitted_at: Option<Instant> = None;
        loop {
            let mut requery = last_query.elapsed() >= REQUERY_INTERVAL;
            let mut reading: Option<Reading> = None;
            loop {
                match rx.try_recv() {
                    Ok(DataEvent::Query(o)) => {
                        if o.tag != tag {
                            continue; // stale
                        }
                        let query_micros = submitted_at
                            .map(|t| t.elapsed().as_micros() as u64)
                            .unwrap_or(0);
                        reading = Some(match o.snapshot {
                            Ok(snapshot) => Reading {
                                freshness: snapshot
                                    .provenance()
                                    .datasets
                                    .iter()
                                    .map(|f| {
                                        (
                                            f.dataset.clone(),
                                            f.as_of.clone().unwrap_or_else(|| "—".into()),
                                            f.generation,
                                        )
                                    })
                                    .collect(),
                                snapshot: Some(snapshot),
                                query_micros,
                                error: None,
                            },
                            Err(e) => Reading {
                                snapshot: None,
                                freshness: Vec::new(),
                                query_micros,
                                error: Some(e),
                            },
                        });
                    }
                    Ok(DataEvent::Published {
                        dataset,
                        batch,
                        gen_id,
                        ..
                    }) => {
                        eprintln!("[probe] published {dataset}/{batch} gen {gen_id}");
                        requery = true;
                    }
                    Ok(DataEvent::Health {
                        source,
                        worst,
                        detail,
                    }) => {
                        eprintln!("[probe] health {source}: {} — {detail}", worst.label());
                    }
                    Ok(DataEvent::Diagnostics(diags)) => {
                        for d in diags {
                            eprintln!("[probe] {d}");
                        }
                    }
                    Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => break,
                }
            }
            if requery {
                tag += 1;
                last_query = Instant::now();
                submitted_at = Some(last_query);
                if !handle.query(QueryParams {
                    key: QueryKey(1),
                    tag,
                    submitted: last_query,
                    view: view.clone(),
                    grouping: None,
                    scope: Scope::default(),
                    as_of: AsOf::Live,
                    max_depth: MAX_DEPTH,
                }) {
                    eprintln!(
                        "[probe] query refused ({} dropped so far)",
                        handle.dropped_requests()
                    );
                }
            }
            if let Some(reading) = reading {
                match (&reading.snapshot, &reading.error) {
                    (_, Some(error)) => eprintln!("[probe] {view}: {error}"),
                    (Some(snapshot), None) => eprintln!(
                        "[probe] {view}: {} rows in {:.1} ms",
                        snapshot.rows(),
                        reading.query_micros as f64 / 1000.0
                    ),
                    (None, None) => {}
                }
                let pushed = shell.update(cx, |shell, cx| {
                    shell.set_probe(
                        ProbeState {
                            snapshot: reading.snapshot,
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
            cx.background_executor().timer(DRAIN_INTERVAL).await;
        }
    })
    .detach();
}
