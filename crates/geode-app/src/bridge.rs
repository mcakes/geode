//! Where shell and data meet (Phase 3 spec §5.1, §9.1): builds the
//! service config from the layered docs, spawns the service behind a
//! `DataHandle`, drains its event channel into the shell on a task that
//! wakes on arrival, and forwards config reloads back to the data thread.

use geode_blotter::BlotterFactory;
use geode_core::colour::NamedColours;
use geode_core::config::{Config, Diagnostic, Severity, load_views};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{CatalogParams, DistinctOutcome};
use geode_core::schema::SchemaSpec;
use geode_core::view::ViewSpec;
use geode_data::adapter::AdapterRegistry;
use geode_data::documents::DocumentRegistry;
use geode_data::source::SourceSpec;
use geode_data::{
    DataEvent, DataHandle, DataService, DataServiceConfig, EventSink, PricerConfig, PricerRegistry,
};
use geode_marketdata::MarketDataFactory;
use geode_marketdata::core::CVI;
use geode_shell::diagnostics::SourceSummary;
use geode_shell::module::Delivery;
use geode_shell::shell::{DIAGNOSTICS_KEY, ShellEvent, ShellView};
use geode_shell::vimfind::FindStyle;
use gpui::{App, AsyncApp, WindowHandle};
use gpui_component::Root;
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Outbound events queued before the sink refuses (§7.3). Tiles × a
/// small burst; a full channel is counted by the bridge's own
/// `dropped` counter. The sink handed to `DataService::spawn` must
/// `try_send`, never block, and a refusal is counted rather than
/// silently lost — surfaced through `Diagnostics::note_dropped` (Phase
/// 4b §4.4; was `ShellView::set_data_status` before that entity existed).
const EVENT_BOUND: usize = 256;

pub struct DataSetup {
    pub config: DataServiceConfig,
    pub views: Vec<ViewSpec>,
    /// The same `DerivedDimensions` already folded into `config.dimensions`
    /// for the service, carried alongside it too so a caller with a
    /// `DataSetup` in hand (rather than reaching into `config`) has it
    /// directly — mirrors `views` being both `config.views` and its own
    /// field for the same reason. `start` now reads this too (Phase 4a
    /// §3.7): the blotter factory validates `:filter`/`:scope` against
    /// the same schema and dimensions the service itself runs on.
    pub dimensions: DerivedDimensions,
    /// `colours.toml` (Part 2c §6.2): the definitions a view column's
    /// `colour = "<name>"` resolves against. Read here rather than left
    /// to `load_views` — that function reads the doc too (to warn about
    /// a column naming a colour nothing defines) but throws
    /// `NamedColours::from_doc`'s own diagnostics away, so this is the
    /// only place a malformed `colours.toml` is ever reported.
    pub colours: NamedColours,
    pub diagnostics: Vec<Diagnostic>,
    /// Every dataset whose spec is `local` (line-pricer spec §7.2): a
    /// sheet autosave publish for one of these must not bump the
    /// frame's `data` version — a tile reading it follows through its
    /// own document request instead. `start` copies this into
    /// `Bridge.local_datasets` for the drain task to read.
    pub local_datasets: HashSet<String>,
}

/// `None` when there is nothing to serve: no datasets or no views.
///
/// `adapters` is the caller's own roster (Task 10, the demo bus):
/// `main.rs` passes an `AdapterRegistry` holding the `ChannelAdapter` it
/// registered under `--demo` and `AdapterRegistry::default()` otherwise,
/// so a non-demo build serves every `csv_dir` source and reports each
/// subscribed one as unservable — the honest answer, never a silent
/// no-op. `documents` is never a caller's choice: every build folds in
/// `geode_documents::builtin_kinds()`, since a document kind carries no
/// state and there is nothing a caller could sensibly leave out.
pub fn data_setup(
    config: &Config,
    db_path: PathBuf,
    adapters: AdapterRegistry,
    pricers: PricerRegistry,
) -> Option<DataSetup> {
    let datasets = config.doc("datasets")?;
    // Presence only: the views themselves come from `load_views`, which
    // applies `view_presentation` over them. Nothing here may read the
    // `views` doc directly — a module must never see a view the
    // trader's presentation has not been merged into (spec §5.6).
    config.doc("views")?;
    let mut diagnostics = Vec::new();
    let (schema, d) = SchemaSpec::from_doc(datasets);
    diagnostics.extend(d);
    let (views, d) = load_views(config);
    diagnostics.extend(d);
    let (dimensions, d) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    diagnostics.extend(d);
    let (sources, d) = config
        .doc("sources")
        .map(|doc| SourceSpec::from_doc(doc, &schema))
        .unwrap_or_default();
    diagnostics.extend(d);
    let (colours, colour_diags) = config
        .doc("colours")
        .map(NamedColours::from_doc)
        .unwrap_or_default();
    diagnostics.extend(colour_diags);
    // `[pricing] adapter` (line-pricer spec §5.5): defaults to the mock
    // every build registers; an unknown name never fails startup — it
    // becomes a `PricerConfig::missing` (every priced line says so) plus
    // a warning naming what this binary actually has.
    let pricer_name = config
        .get("app", "pricing.adapter")
        .and_then(|v| v.as_str())
        .unwrap_or(geode_pricing::MOCK_PRICER)
        .to_string();
    let pricer = match pricers.get(&pricer_name) {
        Some(p) => PricerConfig::with(p),
        None => {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                layer: config.explain("app", "pricing.adapter"),
                file: None,
                message: format!(
                    "pricer \"{pricer_name}\" ([pricing] adapter) is not built into this binary (have: {}); every priced line will say so",
                    pricers.names().join(", ")
                ),
                path: Some("app.pricing.adapter".to_string()),
            });
            PricerConfig::missing(&pricer_name)
        }
    };
    let local_datasets: HashSet<String> = schema
        .datasets
        .iter()
        .filter(|d| d.local)
        .map(|d| d.name.clone())
        .collect();
    Some(DataSetup {
        config: DataServiceConfig {
            db_path,
            schema,
            views: views.clone(),
            dimensions: dimensions.clone(),
            query_workers: 4,
            sources,
            adapters,
            documents: {
                let mut documents = DocumentRegistry::default();
                for kind in geode_documents::builtin_kinds() {
                    documents.register(kind);
                }
                documents
            },
            pricer,
        },
        views,
        dimensions,
        colours,
        diagnostics,
        local_datasets,
    })
}

/// `[app] data.db_path` wins; then the demo directory; then the platform
/// application-data directory (spec §5.4).
pub fn db_path(
    config: &Config,
    demo: Option<&Path>,
    local_app_data: Option<String>,
    home: Option<String>,
) -> PathBuf {
    if let Some(p) = config.get("app", "data.db_path").and_then(|v| v.as_str()) {
        return PathBuf::from(p);
    }
    if let Some(dir) = demo {
        return dir.join("geode.duckdb");
    }
    if let Some(lad) = local_app_data {
        return PathBuf::from(lad).join("Geode").join("geode.duckdb");
    }
    let home = home.unwrap_or_else(|| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/Geode/geode.duckdb")
    } else {
        PathBuf::from(home).join(".local/share/geode/geode.duckdb")
    }
}

/// `[app] blotter.stale_after` (spec §6.5), a duration string in the
/// same `"30s"`/`"10m"`/`"2h"` vocabulary `sources.toml` already uses —
/// reused here (`geode_data::source::parse_duration`) rather than
/// inventing a second duration grammar. Missing or unparsable falls
/// back to the blotter's own default (`geode_blotter::tile::
/// DEFAULT_STALE_AFTER`, 15 minutes).
pub fn stale_after_from_config(config: &Config) -> Duration {
    config
        .get("app", "blotter.stale_after")
        .and_then(|v| v.as_str())
        .and_then(geode_data::source::parse_duration)
        .unwrap_or(geode_blotter::tile::DEFAULT_STALE_AFTER)
}

/// The sink `DataService::spawn` is given: `try_send` onto `tx`, never
/// blocking the caller (query-pool worker or ingest thread), and a
/// refusal bumps `dropped` rather than being lost silently (§7.3).
/// Factored out of `start` so it's unit-testable without a real service
/// thread. `false` means "this event was not delivered", never "stop
/// producing" — no producer inside the service exits on one (Phase 4b
/// follow-up, Task 1).
///
/// Both refusals are counted, because both lose an event, but they are
/// different facts: a FULL channel means the UI is momentarily behind a
/// burst and is already surfaced through `Diagnostics::note_dropped`,
/// while a CLOSED one means the receiver really is gone — worth one
/// line in the log, latched so a busy producer cannot fill the ring
/// with it.
fn make_sink(tx: async_channel::Sender<DataEvent>, dropped: Arc<AtomicU64>) -> EventSink {
    let warned_closed = Arc::new(AtomicBool::new(false));
    Arc::new(move |e| match tx.try_send(e) {
        Ok(()) => true,
        Err(err) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            if err.is_closed() && !warned_closed.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    target: "geode::shell",
                    "the data event receiver is gone; further events are dropped",
                );
            }
            false
        }
    })
}

pub struct Bridge {
    pub handle: DataHandle,
    pub factory: Rc<BlotterFactory>,
    /// The CVI panel's factory (market-data spec §8.1), built here for
    /// the same reason the blotter's is: it needs this bridge's
    /// `DataHandle`, and `attach`'s reload handler needs a clone of it to
    /// refresh `stale_after` on. One factory per panel spec — a second
    /// document kind's panel is a second field here, not a second crate.
    pub marketdata: Rc<MarketDataFactory>,
    events: async_channel::Receiver<DataEvent>,
    dropped: Arc<AtomicU64>,
    /// The sources the running service was actually built from (Phase 4b
    /// §4.4) — `attach` describes each one to the `Diagnostics` entity
    /// once. Cloned out of `setup.config.sources` before that config
    /// moves into `DataService::spawn` below, same reasoning as `schema`/
    /// `dimensions` two lines up.
    sources: Vec<SourceSpec>,
    /// Copied from `DataSetup.local_datasets` (line-pricer spec §7.2):
    /// the `Published` arm in `attach`'s drain task reads this to skip
    /// the frame's `data` version bump for a local dataset's publish.
    pub local_datasets: Rc<HashSet<String>>,
}

pub fn start(
    setup: DataSetup,
    find_style: FindStyle,
    stale_after: Duration,
    _cx: &mut App,
) -> Bridge {
    for d in &setup.diagnostics {
        tracing::warn!(target: "geode::query", "{d}");
    }
    let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
    let dropped = Arc::new(AtomicU64::new(0));
    let sink = make_sink(tx, dropped.clone());
    // Cloned before the move into `DataService::spawn` below — the
    // factory validates `:filter`/`:scope` against the same schema and
    // dimensions the service itself was built from (spec §3.7).
    let schema = setup.config.schema.clone();
    let dimensions = setup.dimensions.clone();
    let sources = setup.config.sources.clone();
    let local_datasets = Rc::new(setup.local_datasets);
    let handle = DataService::spawn(setup.config, sink);
    let factory = Rc::new(BlotterFactory::new(
        handle.clone(),
        setup.views,
        setup.colours,
        schema,
        dimensions,
        find_style,
        stale_after,
    ));
    Bridge {
        marketdata: Rc::new(MarketDataFactory::new(
            handle.clone(),
            &CVI,
            // One threshold, one config key: a document's own freshness
            // means exactly what a dataset's does to the blotter, and two
            // keys for one idea would be two things to keep in step.
            stale_after,
        )),
        handle,
        factory,
        events: rx,
        dropped,
        sources,
        local_datasets,
    }
}

/// Route events into the shell and forward reloads. Wakes on arrival:
/// `async_channel::Receiver::recv` is a future gpui's executor polls, so
/// delivery latency is a frame, not a poll interval.
pub fn attach(bridge: &Bridge, window: WindowHandle<Root>, cx: &mut App) {
    let rx = bridge.events.clone();
    let dropped = bridge.dropped.clone();
    let handle = bridge.handle.clone();
    let factory = bridge.factory.clone();
    let marketdata = bridge.marketdata.clone();

    let shell = window
        .read(cx)
        .ok()
        .and_then(|root| root.view().clone().downcast::<ShellView>().ok())
        .expect("the window's root view is the shell");
    let diagnostics = shell.read(cx).diagnostics().clone();

    // Every configured source's static description (Phase 4b §4.4),
    // once — plain strings so `geode_shell::diagnostics` never names
    // `geode_data::source::{Priority, Readiness}` themselves (CLAUDE.md:
    // shell and data never depend on each other).
    diagnostics.update(cx, |d, cx| {
        for source in &bridge.sources {
            d.describe_source(
                &source.name,
                SourceSummary {
                    paths: source.paths.clone(),
                    priority: format!("{:?}", source.priority),
                    readiness: format!("{:?}", source.readiness),
                    adapter: source.adapter.clone(),
                    // Already empty for a directory source — `from_doc`
                    // reads `topics` only when the source is subscribed —
                    // and the tile reads that emptiness as "a directory
                    // source", so it is cloned rather than gated here.
                    topics: source.topics.clone(),
                },
            );
        }
        cx.notify();
    });

    // The diagnostics tile's `Request::Catalog` drain (Phase 4b §4.5):
    // fires on every notify from the entity, regardless of what queued
    // the request — `Diagnostics::note_published` (below, on a fresh
    // publish while watched) and the diagnostics tile's own
    // `set_visible(true)` (Task 5, `Diagnostics::watch`) both go through
    // this one door, so a tile becoming visible gets its first catalog
    // the same way a publish refreshes an already-visible one.
    // `catalog_tag` is a plain `Rc<Cell<u64>>`, not `Arc<AtomicU64>`:
    // both this observer and the drain loop below run on the UI thread's
    // single-threaded async executor (the loop already captures a
    // non-`Send` `Rc<BlotterFactory>`), so there is no real concurrency
    // to guard against.
    let catalog_tag = Rc::new(Cell::new(0u64));
    cx.observe(&diagnostics, {
        let handle = handle.clone();
        let diagnostics = diagnostics.clone();
        let shell = shell.clone();
        let catalog_tag = catalog_tag.clone();
        move |_entity, cx| {
            let requested = diagnostics.update(cx, |d, _cx| d.take_pending_catalog_request());
            if !requested {
                return;
            }
            let tag = catalog_tag.get() + 1;
            catalog_tag.set(tag);
            let as_of = shell.read(cx).frame().read(cx).as_of().clone();
            // MIN-7 (Phase 4b Task 4 fix round 1): a refused request
            // (the service thread's queue is full or it's gone) used to
            // vanish silently — unlike `ShellEvent::DistinctRequested`
            // just above, which synthesises an error outcome so the
            // picker never sits on `loading…` forever. There is no
            // equivalent "loading" UI state for the catalog yet (Task 5
            // hasn't built the tile), so this surfaces it the same way
            // every other refusal in this file does: a `geode::query`
            // warning, not a panic and not a swallow.
            if !handle.catalog(CatalogParams {
                key: DIAGNOSTICS_KEY,
                tag,
                as_of,
            }) {
                tracing::warn!(
                    target: "geode::query",
                    "catalog request refused — the data service is busy or gone"
                );
            }
        }
    })
    .detach();

    // Reloads: new views to the data thread and to the factory.
    cx.subscribe(&shell, {
        let handle = handle.clone();
        let factory = factory.clone();
        let marketdata = marketdata.clone();
        let diagnostics = diagnostics.clone();
        move |shell, event: &ShellEvent, cx| match event {
            ShellEvent::ConfigReloaded => {
                let config = shell.read(cx).config();
                if config.doc("views").is_none() {
                    return;
                }
                // Same door as `data_setup`: a reload that read the
                // `views` doc directly would silently drop the trader's
                // `view_presentation.toml` the first time anything
                // reloaded, which is precisely what the Views dialog's
                // write relies on picking up (spec §5.6, §7.1). Its
                // diagnostics are no longer discarded either (§19.6,
                // below) — a stale `view_presentation.toml` name used to
                // warn once at startup (`data_setup`) and go silent on
                // every reload after.
                let (views, presentation_diags) = load_views(config);
                // §19.6: the reload path used to discard these, so a
                // `view_presentation.toml` entry naming a view that no
                // longer exists warned once at startup and was silent
                // through every reload after — the trader renames a view
                // and their column order quietly stops applying. Reported
                // the way `data_setup`'s are at startup, and noted in the
                // entity through the data-batch door (append + dedupe),
                // never `note_config`, whose replace semantics belong to
                // `apply_reload` alone (Phase 4b MAJ-5). Logged and
                // queued here, before `config` (borrowed from `cx`
                // through `shell.read`) is used again below — the actual
                // `diagnostics.update` call, which needs `cx` mutably,
                // waits until `config`'s last use, further down.
                for d in &presentation_diags {
                    tracing::warn!(target: "geode::query", "{d}");
                }
                // 2c §6.2: `colours` is one of `ConfigReloaded`'s own
                // triggers (`shell::hot_reload`), so this really is the
                // handler a colour edit wakes. Its reader's diagnostics
                // join the presentation ones — `load_views` above read
                // the same doc and discarded them, so without this a
                // malformed colour would be reported at startup
                // (`data_setup`) and never again, which is precisely
                // when a trader is editing the file.
                let (colours, colour_diags) = config
                    .doc("colours")
                    .map(NamedColours::from_doc)
                    .unwrap_or_default();
                for d in &colour_diags {
                    tracing::warn!(target: "geode::query", "{d}");
                }
                factory.set_colours(colours);
                let (dims, _) = config
                    .doc("dimensions")
                    .map(DerivedDimensions::from_doc)
                    .unwrap_or_default();
                // `stale_after` is not among `ConfigReloaded`'s own
                // triggers (that event fires for the docs a tile runs and
                // paints on — `views`, `view_presentation`, `dimensions`,
                // `colours`; `shell::hot_reload::apply_reload`) — an edit to
                // `[app] blotter.stale_after` alone does not itself wake
                // this handler. It is re-read and re-applied here anyway,
                // piggybacking on whatever reload did fire, so it never
                // drifts further than one views/dimensions reload behind
                // what's on disk; a `stale_after`-only edit needs a
                // restart, same as `sources`/`datasets`.
                factory.set_views(views.clone());
                factory.set_find_style(FindStyle::from_config(config));
                let stale_after = stale_after_from_config(config);
                factory.set_stale_after(stale_after);
                // The panel reads the same key, and piggybacks on the
                // same reload for the same reason (the paragraph above):
                // `stale_after` is not itself a `ConfigReloaded` trigger,
                // so this can only ever drift as far as the next
                // views/dimensions reload.
                marketdata.set_stale_after(stale_after);
                // `:filter`/`:scope` validation (Phase 4a §3.7): the
                // `datasets` doc is re-read here too — `ConfigReloaded`
                // doesn't fire for a `datasets`-only edit (that instead
                // sets `restart_required`, since the data engine itself
                // needs a restart to pick up new source paths or column
                // definitions), so this can only ever drift as far as a
                // `views`/`dimensions` reload that happened to arrive
                // alongside a stale `datasets` doc — the same bound
                // `stale_after` above accepts.
                if let Some(schema) = config.doc("datasets").map(|d| SchemaSpec::from_doc(d).0) {
                    factory.set_schema(schema);
                }
                factory.set_dims(dims.clone());
                handle.replace_views(views, dims);
                // §19.6: `config`'s last use was just above — free to
                // borrow `cx` mutably now.
                let reload_diags: Vec<Diagnostic> =
                    presentation_diags.into_iter().chain(colour_diags).collect();
                if !reload_diags.is_empty() {
                    diagnostics.update(cx, |dg, cx| {
                        let before = dg.version();
                        dg.note_data_diagnostics(reload_diags, SystemTime::now());
                        if dg.version() != before {
                            cx.notify();
                        }
                    });
                }
            }
            // The dimension pickers (Phase 4a §3.3/§3.4): `geode-shell`
            // cannot depend on `geode-data` (CLAUDE.md), so a picker's
            // request leaves the shell as this event instead of a direct
            // `DataHandle::distinct` call — this is the one place that
            // call actually happens. The outcome comes back on the
            // `DataEvent` drain loop below, routed to `deliver_distinct`.
            // A refused request (`false`: the query pool's queue is full
            // or the service thread is gone) gets no reply from that
            // drain loop — nothing else would ever arrive to move the
            // picker off `loading…` — so a synthetic error outcome is
            // delivered right here instead, echoing the request's own
            // key/tag/column exactly as a real reply would.
            ShellEvent::DistinctRequested(params) => {
                let queued = handle.distinct(params.clone());
                if !queued {
                    let outcome = DistinctOutcome {
                        key: params.key,
                        tag: params.tag,
                        column: params.column.clone(),
                        values: Err("the data service is busy or gone — try again".into()),
                    };
                    shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                }
            }
            ShellEvent::RestartRequired(_) => {}
            // §19.6: the shell already logged each diagnostic
            // (`geode::config` error) and noted them in the entity
            // (`apply_reload`'s own `note_config` call, which runs
            // regardless of the outcome) — there is nothing left for the
            // bridge to forward.
            ShellEvent::ReloadRejected(_) => {}
        }
    })
    .detach();

    let diagnostics_for_drain = diagnostics.clone();
    let catalog_tag_for_drain = catalog_tag.clone();
    // Line-pricer spec §7.2: read once, up front, so the `Published` arm
    // below can decide without touching `bridge` — `attach` only borrows
    // it (`&Bridge`), and that borrow ends when `attach` returns, well
    // before the drain task below ever runs, so the `Rc` is cloned here
    // rather than captured by reference.
    let local_datasets = Rc::clone(&bridge.local_datasets);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let diagnostics = diagnostics_for_drain;
        let catalog_tag = catalog_tag_for_drain;
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);
            // Every branch below reaches the shell through `window.update`
            // rather than a standalone `Entity<ShellView>` clone updated
            // via plain `cx.update`: an entity update succeeds forever,
            // window or no, so a bare `cx.update` never notices the
            // window is gone and this task (with its `DataHandle` clone,
            // `Rc<BlotterFactory>` and `Arc<AtomicU64>`) would outlive the
            // window until a `Query` outcome happened to arrive. Routing
            // every branch through the window handle makes the very next
            // event — of any kind — the one that ends the task.
            let handled = window.update(cx, |root, window, cx| {
                let Ok(shell) = root.view().clone().downcast::<ShellView>() else {
                    return;
                };
                if now_dropped != last_dropped {
                    diagnostics.update(cx, |d, cx| {
                        let before = d.version();
                        d.note_dropped(now_dropped);
                        if d.version() != before {
                            cx.notify();
                        }
                    });
                }
                match event {
                    DataEvent::Query(outcome) => {
                        shell.update(cx, |s, cx| {
                            s.deliver(Delivery::Query(outcome), window, cx)
                        });
                    }
                    DataEvent::Published {
                        dataset,
                        batch,
                        books,
                        ..
                    } => {
                        // Not logged here (Phase 4b Task 2 fix round 1,
                        // MAJ-2): `geode-data`'s own `service.rs` already
                        // logs every publish at `info` — with more detail
                        // (book/row counts) than this arm has — the
                        // moment the event is constructed, so a second
                        // line here would be a strictly less informative
                        // duplicate, and it would run on the UI thread
                        // (this whole match is inside `window.update`),
                        // against the Global Constraint that UI-thread
                        // code emits at `warn` or above only. Deleting
                        // beats dropping to `debug`: a `debug!` call site
                        // here would cost a (statically-disabled, but
                        // real) max-level check on every publish for no
                        // reason to exist at all — there's nothing this
                        // arm could say that the engine-side line
                        // doesn't already say better.
                        //
                        // `Diagnostics::note_published` (Phase 4b §4.4)
                        // borrows `dataset` ahead of the move into
                        // `Publish` below — it also sets
                        // `pending_catalog_request` while a diagnostics
                        // tile is watching, which the `cx.observe`
                        // registered in `attach` drains.
                        diagnostics.update(cx, |d, cx| {
                            d.note_published(&dataset);
                            cx.notify();
                        });
                        if local_datasets.contains(&dataset) {
                            // Line-pricer spec §7.2: a sheet autosave is
                            // not a data change for the workspace; a
                            // tile reading sheets follows the dataset
                            // through its own document request.
                        } else {
                            let frame = shell.read(cx).frame().clone();
                            // The event carries no timestamp of its own;
                            // the arrival instant is what a "recent
                            // publishes" preset needs (Phase 4a §3.12).
                            let publish = geode_shell::frame::Publish {
                                dataset,
                                batch,
                                books: books.len(),
                                at: chrono::Utc::now(),
                            };
                            frame.update(cx, |f, cx| {
                                f.note_published(publish);
                                cx.notify();
                            });
                        }
                    }
                    DataEvent::Health {
                        source,
                        worst,
                        detail,
                    } => {
                        // Not logged here either, same reasoning as
                        // `Published` just above (MAJ-2): `geode-data`'s
                        // `log_health_event` already logs this at the
                        // level the outcome deserves (`error` for
                        // `Failed`, `warn` for `Degraded`/
                        // `PendingTooLong`, `info`/`debug` otherwise) the
                        // moment the event is constructed. This arm keeps
                        // only the real state change — the `Diagnostics`
                        // entity, which the status bar's summary reads —
                        // which is not logging and so isn't subject to
                        // the UI-thread level constraint at all.
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_health(&source, worst, detail, SystemTime::now());
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    DataEvent::Diagnostics(diags) => {
                        for d in &diags {
                            tracing::warn!(target: "geode::query", "{d}");
                        }
                        // NEW-1 (Phase 4b Task 4 fix round 2): the data
                        // layer's own diagnostics — schema/dataset/view
                        // validation errors, a failed open, or a
                        // `Request::ReplaceViews` outcome — go through
                        // `note_data_diagnostics`, never `note_config`.
                        // Round 1 routed both through `note_config`,
                        // whose replace semantics (MAJ-5) meant this and
                        // a config load/reload silently erased each
                        // other's diagnostics from the summary.
                        diagnostics.update(cx, |dg, cx| {
                            let before = dg.version();
                            dg.note_data_diagnostics(diags, SystemTime::now());
                            if dg.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    // The dimension picker's own outcome (spec §3.4),
                    // paired with the `DistinctRequested` submission
                    // above. `deliver_distinct` carries its own
                    // stale-tag/stale-column/no-picker-open guard, so
                    // nothing here needs to check the key or tag itself.
                    DataEvent::Distinct(outcome) => {
                        shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                    }
                    // The diagnostics tile's "what does the database
                    // hold" result (Phase 4b §4.5), requested by the
                    // `cx.observe` registered in `attach`. Tag-checked
                    // against `catalog_tag` — that observer is the only
                    // submitter, so an outcome whose tag doesn't match
                    // the latest one it handed out is answering a
                    // request a newer one has already superseded, and is
                    // dropped rather than applied (spec §7.3: a stale
                    // result is never rendered).
                    DataEvent::Catalog(outcome) => {
                        if outcome.tag != catalog_tag.get() {
                            return;
                        }
                        match outcome.snapshot {
                            Ok(snapshot) => {
                                diagnostics.update(cx, |d, cx| {
                                    let before = d.version();
                                    d.set_catalog(snapshot);
                                    if d.version() != before {
                                        cx.notify();
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(target: "geode::query", "catalog request failed: {e}")
                            }
                        }
                    }
                    DataEvent::Polled {
                        source,
                        ready,
                        at,
                        next,
                    } => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_polled(&source, ready, at, next);
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    // Task 3 (ingest progress, spec 2026-09-17 §5.3): a
                    // load began — always bumps, since a new `Loading`
                    // is a new record even for the same source (its
                    // path or depth moved).
                    DataEvent::Loading {
                        source,
                        path,
                        queued,
                    } => {
                        diagnostics.update(cx, |d, cx| {
                            d.note_loading(&source, &path, queued, SystemTime::now());
                            cx.notify();
                        });
                    }
                    // Carries no `source` (finding 2, 2026-09-19 final
                    // review): one ingest runner draining one FIFO queue
                    // means loads are strictly sequential, so the load
                    // that just ended is always the one `note_loading`
                    // last recorded, and `note_load_ended` is a no-op
                    // when nothing is — including the extra copy the
                    // queue drain now sends after every `Published`/
                    // `Failed`'s own.
                    DataEvent::LoadEnded => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_load_ended();
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    // Line-pricer spec §5.4: routed to the shell exactly
                    // like `Query` — `ShellView::deliver`'s own router
                    // (Task 7) drops it when the key names no live
                    // occupant.
                    DataEvent::Price(outcome) => {
                        shell.update(cx, |s, cx| {
                            s.deliver(Delivery::Price(outcome), window, cx)
                        });
                    }
                }
            });
            if handled.is_err() {
                return; // the window is gone
            }
            last_dropped = now_dropped;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};
    use geode_core::log::Ring;
    use geode_core::query::{AsOf, CatalogOutcome, CatalogSnapshot, QueryKey};
    use geode_data::source::SourceSpec;
    use geode_diagnostics::DiagnosticsFactory;
    use geode_shell::actions::ActionRegistry;
    use geode_shell::defaults::{BUILTIN_KEYMAP, default_mod, register_builtin_actions};
    use geode_shell::keymap::build_keymap;
    use geode_shell::module::recording::{Recorded, RecordingFactory};
    use geode_shell::module::{ModuleFactory, ModuleRoster};
    use geode_shell::session::TileRecords;
    use geode_shell::shell::ShellServices;
    use geode_shell::tiling::{TileId, Workspaces};
    use geode_shell::{theme, vimfind::FindStyle};
    use gpui::AppContext as _;
    use std::cell::RefCell;

    /// Records logged while `f` runs, on this thread only — the same
    /// scoped-subscriber pattern `geode-data`'s own test modules use.
    fn logged(f: impl FnOnce()) -> Vec<geode_core::log::Record> {
        use tracing_subscriber::layer::SubscriberExt;
        let ring = Arc::new(Ring::new(16));
        let sub =
            tracing_subscriber::registry().with(geode_core::log::RingLayer::new(ring.clone()));
        tracing::subscriber::with_default(sub, f);
        let mut out = Vec::new();
        ring.drain_since(0, &mut out);
        out
    }

    #[test]
    fn a_full_channel_is_counted_but_not_reported_as_a_gone_receiver() {
        // A momentarily full channel and a closed one both refuse, but
        // only one of them means the receiver is gone (Phase 4b
        // follow-up, Task 1). A full one is already surfaced through
        // `Diagnostics::note_dropped`; claiming the receiver had gone
        // would be a lie in the log.
        let (tx, _rx) = async_channel::bounded::<DataEvent>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = make_sink(tx, dropped.clone());
        let records = logged(|| {
            assert!(sink(DataEvent::Diagnostics(Vec::new())), "the first fits");
            assert!(
                !sink(DataEvent::Diagnostics(Vec::new())),
                "the second finds it full"
            );
        });
        assert_eq!(dropped.load(Ordering::Relaxed), 1, "the refusal is counted");
        assert!(
            !records.iter().any(|r| r.message.contains("receiver")),
            "a full channel must not be logged as a gone receiver: {records:?}"
        );
    }

    #[test]
    fn a_closed_channel_is_counted_and_logged_once() {
        let (tx, rx) = async_channel::bounded::<DataEvent>(4);
        drop(rx);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = make_sink(tx, dropped.clone());
        let records = logged(|| {
            assert!(!sink(DataEvent::Diagnostics(Vec::new())));
            assert!(!sink(DataEvent::Diagnostics(Vec::new())));
        });
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            2,
            "every refusal is counted, closed or full"
        );
        let gone: Vec<_> = records
            .iter()
            .filter(|r| r.message.contains("receiver"))
            .collect();
        assert_eq!(
            gone.len(),
            1,
            "a gone receiver is worth one line, not one per event: {records:?}"
        );
        assert_eq!(gone[0].target, "geode::shell");
        assert_eq!(gone[0].level, tracing::Level::WARN);
    }

    /// The minimal real `ShellServices` a window needs to open — same
    /// shape as `geode-shell`'s own `test_services()` (not reachable
    /// from here: it is `pub(super)` inside that crate's test module),
    /// built from public items only.
    fn test_shell_services() -> ShellServices {
        test_shell_services_with_sources(ConfigSources::default())
    }

    /// A trimmed `datasets` doc making `book` pickable (categorical
    /// dimension) — same shape as `geode-shell::shell::tests::picker`'s
    /// own `DATASETS_DOC`, reproduced here since that module is private
    /// to its crate.
    const PICKABLE_DATASETS_DOC: &str = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#;

    /// `test_shell_services()` with `book` pickable, for the picker test
    /// below — `picker::open` only reaches the `Values` stage (and so
    /// only emits `DistinctRequested`) for a column `ShellView.pickable`
    /// actually names.
    fn test_shell_services_with_pickable_book() -> ShellServices {
        test_shell_services_with_sources(ConfigSources {
            builtin: vec![LayerDoc::builtin("datasets", PICKABLE_DATASETS_DOC).unwrap()],
            desk: None,
            user: None,
        })
    }

    /// Builds `config` and `builtin` from one `ConfigSources` via
    /// `ShellServices::config_and_builtin`, so the two cannot disagree —
    /// a config hot reload re-merges exactly the docs `builtin` holds
    /// (see that constructor's doc comment).
    fn test_shell_services_with_sources(sources: ConfigSources) -> ShellServices {
        let (config, builtin) = ShellServices::config_and_builtin(sources);
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let mod_alias = default_mod();
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            builtin,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
            session_path: None,
            roster: ModuleRoster::default(),
            restored_tiles: TileRecords::new(),
            restored_frame: None,
            restored_palette_usage: geode_shell::palette_usage::PaletteUsage::new(),
            log: None,
            action_tail: std::sync::Arc::new(std::sync::Mutex::new(
                geode_shell::diagnostics::ActionTail::new(),
            )),
            keymap_diagnostics: Vec::new(),
            keymap_fragments: Vec::new(),
            keymap_fragment_diagnostics: Vec::new(),
        }
    }

    /// [`test_shell_services`] with one `RecordingFactory` of kind "rec"
    /// in the roster (`geode_shell::module::recording`, `test-support`
    /// feature) — the neighbour the `Price` delivery test below opens
    /// through `ShellView::open_module` so it has a real, focused,
    /// non-placeholder tile whose id it can read back and whose log it
    /// can inspect (the view type behind a roster's `&dyn ModuleFactory`
    /// is private, so the recording factory's own `log` is the only
    /// window into what a delivery actually did). No add actions are
    /// registered for "rec" — `open_module` calls `ShellView::add_tile`
    /// directly rather than through action dispatch, so nothing here
    /// needs a keymap binding or a registry entry for the kind.
    fn test_shell_services_with_rec_roster() -> (ShellServices, Rc<RefCell<Vec<Recorded>>>) {
        let (config, builtin) = ShellServices::config_and_builtin(ConfigSources::default());
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let mod_alias = default_mod();
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        let recorder = RecordingFactory::new("rec");
        let log = recorder.log.clone();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(recorder));
        let services = ShellServices {
            config,
            builtin,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
            session_path: None,
            roster,
            restored_tiles: TileRecords::new(),
            restored_frame: None,
            restored_palette_usage: geode_shell::palette_usage::PaletteUsage::new(),
            log: None,
            action_tail: std::sync::Arc::new(std::sync::Mutex::new(
                geode_shell::diagnostics::ActionTail::new(),
            )),
            keymap_diagnostics: Vec::new(),
            keymap_fragments: Vec::new(),
            keymap_fragment_diagnostics: Vec::new(),
        };
        (services, log)
    }

    fn open_test_window(
        cx: &mut gpui::TestAppContext,
        services: ShellServices,
    ) -> WindowHandle<Root> {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap()
    }

    /// Line-pricer spec §5.5: `[pricing] adapter` (default `"mock"`)
    /// resolves through the caller's `PricerRegistry`, and an unknown
    /// name is a warning diagnostic naming both the requested and the
    /// registered pricers — never a startup failure.
    ///
    /// `data_setup` also requires a `views` doc (`config.doc("views")?`)
    /// to return `Some` at all, so — unlike the brief's first sketch,
    /// which built the config from a bare, empty `datasets` doc via
    /// `config_from` and got `None` back — this builds a minimal but
    /// real one-dataset schema plus one view, the same shape
    /// `data_setup_needs_datasets_and_views_and_carries_sources` (below)
    /// already proves produces no diagnostics of its own.
    #[test]
    fn pricing_adapter_resolves_through_the_registry_and_an_unknown_name_is_a_diagnostic() {
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let dir = tempfile::tempdir().unwrap();

        let config_with_app = |app_text: &str| {
            let mut builtin = vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n")
                    .unwrap(),
            ];
            if !app_text.is_empty() {
                builtin.push(LayerDoc::builtin("app", app_text).unwrap());
            }
            Config::load(&ConfigSources {
                builtin,
                ..ConfigSources::default()
            })
        };

        // The default: no [pricing] section at all.
        let config = config_with_app("");
        let setup = data_setup(
            &config,
            dir.path().join("a.duckdb"),
            AdapterRegistry::default(),
            pricers.clone(),
        )
        .unwrap();
        assert_eq!(setup.config.pricer.name, "mock");
        assert!(setup.config.pricer.pricer.is_some());
        assert!(
            !setup
                .diagnostics
                .iter()
                .any(|d| d.message.contains("pricer"))
        );
        // Review finding, Important 2: this config declares no `local`
        // dataset, so `DataSetup.local_datasets` must come back empty —
        // `data_setup_names_every_local_dataset_and_only_those` (below)
        // is where the non-empty case is pinned.
        assert!(
            setup.local_datasets.is_empty(),
            "{:?}",
            setup.local_datasets
        );

        let config = config_with_app("[pricing]\nadapter = \"vendor\"\n");
        let setup = data_setup(
            &config,
            dir.path().join("b.duckdb"),
            AdapterRegistry::default(),
            pricers,
        )
        .unwrap();
        assert_eq!(setup.config.pricer.name, "vendor");
        assert!(setup.config.pricer.pricer.is_none());
        let d = setup
            .diagnostics
            .iter()
            .find(|d| d.message.contains("pricer"))
            .unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert!(
            d.message.contains("vendor") && d.message.contains("mock"),
            "{}",
            d.message
        );
    }

    /// Line-pricer spec §7.2 (review finding, Important 2):
    /// `DataSetup.local_datasets` must name every dataset whose spec is
    /// `local` — and only those, so a non-local dataset (`risk`, here)
    /// beside it isn't swept in by accident. `sheets` is the exact
    /// document-family shape `local_is_read_on_a_document_dataset_and_
    /// defaults_to_false` in `geode-core/src/schema/mod.rs` already
    /// proves `SchemaSpec::from_doc` reads `local` correctly for; this
    /// pins that `data_setup` carries that flag through to the field the
    /// bridge's local-publish gate actually reads
    /// (`a_local_publish_does_not_bump_the_frames_data_version_but_a_
    /// normal_one_does`, below, which hand-picks its own dataset name
    /// and so would not have caught a `local_datasets` that came back
    /// empty, wrong, or as every dataset regardless of `local`).
    #[test]
    fn data_setup_names_every_local_dataset_and_only_those() {
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                     [sheets]\n\
                     family = \"document\"\n\
                     local = true\n\
                     key = [\"sheet\"]\n\
                     axes = [\"line\"]\n\
                     [sheets.columns.sheet]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [sheets.columns.line]\ntype = \"i64\"\nrole = \"axis\"\n\
                     [sheets.columns.qty]\ntype = \"i64\"\nrole = \"value\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n")
                    .unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(
            &config,
            dir.path().join("c.duckdb"),
            AdapterRegistry::default(),
            pricers,
        )
        .unwrap();
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        assert_eq!(
            setup.local_datasets,
            ["sheets".to_string()].into_iter().collect()
        );
        assert!(setup.config.schema.dataset("sheets").unwrap().local);
        assert!(!setup.config.schema.dataset("risk").unwrap().local);
    }

    /// Line-pricer spec §7.2: a `local` dataset's publish must not bump
    /// the frame's `data` version (a tile reading it follows through its
    /// own document request instead), but it must still reach
    /// `Diagnostics::note_published` — modelled line for line on
    /// `loading_and_load_ended_reach_the_diagnostics_entity`'s
    /// `Bridge`/`attach` setup, above.
    #[gpui::test]
    fn a_local_publish_does_not_bump_the_frames_data_version_but_a_normal_one_does(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = open_test_window(cx, test_shell_services());
        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Rc::new(["pricer_sheets".to_string()].into_iter().collect()),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let before = frame.read_with(&vcx, |f, _| f.versions().data);

        tx.try_send(DataEvent::Published {
            dataset: "pricer_sheets".into(),
            batch: "untitled-1".into(),
            gen_id: 1,
            books: vec![None],
        })
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            frame.read_with(&vcx, |f, _| f.versions().data),
            before,
            "a local publish is not a data change"
        );
        // `Diagnostics::last_published` doesn't exist; `note_published`
        // is observable through the `datasets` map it inserts into.
        assert!(
            diagnostics.read_with(&vcx, |d, _| d.datasets.contains_key("pricer_sheets")),
            "diagnostics still saw it"
        );

        tx.try_send(DataEvent::Published {
            dataset: "risk_snapshot".into(),
            batch: "EOD".into(),
            gen_id: 2,
            books: vec![Some("BK1".into())],
        })
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(frame.read_with(&vcx, |f, _| f.versions().data), before + 1);
    }

    /// Line-pricer spec §5.4: the bridge's `DataEvent::Price` arm must
    /// actually reach the occupant, not merely fail to panic — a
    /// reverted arm (the Task 6 placeholder `DataEvent::Price(_outcome)
    /// => {}`) would leave a test that only sends-and-parks green too
    /// (review finding, Important 1). A `RecordingFactory` tile is the
    /// route: its `TileContent::deliver` pushes `Recorded::Priced(tile,
    /// tag)` on a `Delivery::Price` (`crates/geode-shell/src/module.rs`),
    /// and the factory's `log` is the one window a test outside
    /// `geode-shell` has onto what a delivery actually did (the view
    /// type behind a roster's `&dyn ModuleFactory` is private).
    #[gpui::test]
    fn a_price_event_is_delivered_to_the_shell_as_delivery_price(cx: &mut gpui::TestAppContext) {
        let (services, log) = test_shell_services_with_rec_roster();
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });

        // A fresh `Workspaces::new()` has no tiles at all yet (its tree
        // is empty, not a lone placeholder); `open_module` finds nothing
        // of kind "rec" and falls through to `add_tile`'s "empty region"
        // branch, which splits the empty tree and hands the new root
        // tile straight to the factory — no add action or keymap
        // binding needed, since this calls the method directly rather
        // than dispatching. `Workspaces::split_active`'s `next_tile`
        // counter starts at 0 and is pre-incremented, so the very first
        // tile a fresh workspace ever creates is deterministically
        // `TileId(1)` (the same assumption every low-level tiling-tree
        // test in `geode-shell` already makes); `occupant_kind` — the
        // one tile accessor this crate can actually reach (`current_
        // tiles` is `pub(super)`) — confirms it rather than trusting it
        // blindly.
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.open_module("rec", window, cx);
            });
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = TileId(1);
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.occupant_kind(tile)),
            Some("rec"),
            "open_module(\"rec\", ..) must have created a tile at TileId(1)"
        );

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        tx.try_send(DataEvent::Price(geode_core::pricing::PriceOutcome {
            key: QueryKey(tile.0),
            tag: 5,
            submitted: std::time::Instant::now(),
            results: Vec::new(),
        }))
        .unwrap();
        vcx.run_until_parked();
        assert!(
            log.borrow().contains(&Recorded::Priced(tile, 5)),
            "the Price delivery must reach the tile's occupant: {:?}",
            log.borrow()
        );
    }

    /// Finding 1 (fix round 1): every branch of the drain loop must
    /// detect the closed window and end the task, not just `Query`.
    /// Before the fix, `Published`/`Health`/the dropped-events branch
    /// reached the shell through a standalone `Entity<ShellView>` clone
    /// via plain `cx.update`, which succeeds forever regardless of the
    /// window — so the task (and its `dropped` counter, `DataHandle` and
    /// factory clones) outlived the window until a `Query` outcome
    /// happened to arrive.
    ///
    /// `dropped: Arc<AtomicU64>` is the cleanest observable proxy for
    /// "the task has ended": it is captured directly by the drain task's
    /// `async move` block (every branch reads it) and by nothing else in
    /// `attach` — unlike `factory`/`handle`, which the `ConfigReloaded`
    /// subscription also clones for its own, unrelated, longer lifetime.
    /// A `Health` event — one of the branches that used to bypass the
    /// window check — is sent after the window closes; if the task is
    /// still alive it will have observed the event and still hold its
    /// clone, so the strong count would not move. No completion flag
    /// needed in production code.
    #[gpui::test]
    fn the_drain_task_ends_on_the_first_event_after_the_window_closes(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = open_test_window(cx, test_shell_services());

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };

        cx.update(|cx| attach(&bridge, window, cx));

        // This test's own `dropped`, `bridge.dropped`, and the drain
        // task's own clone (captured at `attach` time) — three, while
        // the task is alive.
        let alive = Arc::strong_count(&dropped);
        assert_eq!(
            alive, 3,
            "the drain task holds its own clone of `dropped` while it runs"
        );

        cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();

        // A `Health` event — one of the branches that used to bypass the
        // window check entirely — must be what ends the task now that
        // the window is gone.
        tx.try_send(DataEvent::Health {
            source: "s".into(),
            worst: geode_data::health::Health::Ok,
            detail: String::new(),
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(
            Arc::strong_count(&dropped),
            alive - 1,
            "the drain task released its clone of `dropped` once the window was gone"
        );
    }

    /// Task 3 (ingest progress, spec 2026-09-17 §5.3): the bridge routes
    /// `DataEvent::Loading`/`LoadEnded` into the diagnostics entity's
    /// `ingest` field.
    #[gpui::test]
    fn loading_and_load_ended_reach_the_diagnostics_entity(cx: &mut gpui::TestAppContext) {
        let window = open_test_window(cx, test_shell_services());

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };

        cx.update(|cx| attach(&bridge, window, cx));

        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

        tx.try_send(DataEvent::Loading {
            source: "risk".into(),
            path: "/data/risk/EOD.csv".into(),
            queued: 4,
        })
        .unwrap();
        vcx.run_until_parked();
        let recorded = diagnostics.read_with(&vcx, |d, _| d.ingest.clone());
        let a = recorded.expect("Loading reached the entity");
        assert_eq!(
            (a.source.as_str(), a.path.as_str(), a.queued),
            ("risk", "/data/risk/EOD.csv", 4)
        );

        tx.try_send(DataEvent::LoadEnded).unwrap();
        vcx.run_until_parked();
        assert!(
            diagnostics.read_with(&vcx, |d, _| d.ingest.is_none()),
            "LoadEnded cleared it"
        );
    }

    /// F3 (final fix wave, whole-branch review): `DataHandle::distinct`
    /// discarding its `bool` used to leave the picker on "loading…"
    /// forever once the request was refused, since nothing else would
    /// ever reply. `DataHandle::shutdown` drops the request sender, so
    /// every later `distinct` call refuses (`Inner::send` sees `None`)
    /// without needing a real service thread to exercise the refusal.
    #[gpui::test]
    fn a_refused_distinct_request_errors_the_picker(cx: &mut gpui::TestAppContext) {
        let window = open_test_window(cx, test_shell_services_with_pickable_book());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, _rx) = DataHandle::for_tests();
        handle.shutdown();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                geode_shell::shell::picker::open(view, Some("book".into()), window, cx);
            });
        });
        vcx.run_until_parked();

        let values = shell.read_with(&vcx, |s, _| {
            s.picker()
                .expect("the picker is still open — nothing here closes it")
                .values
                .clone()
        });
        assert_eq!(
            values,
            Some(Err(
                "the data service is busy or gone — try again".to_string()
            )),
            "a refused request must error the picker, not leave it loading forever"
        );
    }

    /// §19.6: a reload no longer drops `load_views`'s presentation
    /// diagnostics — a stale `view_presentation.toml` name reaches the
    /// diagnostics entity on every reload, not only at startup.
    #[gpui::test]
    fn a_reload_reports_a_stale_presentation_name(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "views",
                    "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("view_presentation", "[gone]\nhidden = [\"npv\"]\n").unwrap(),
            ],
            desk: None,
            user: None,
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let reported = diagnostics.read_with(&vcx, |d, _| {
            d.data_diagnostics
                .iter()
                .any(|(_, d)| d.message.contains("no view of that name"))
        });
        assert!(
            reported,
            "the reload path must report the stale presentation name"
        );
    }

    /// 2c §6.2: a reloaded `colours.toml` reaches the factory, so the
    /// next tile — and, through `BlotterTile::apply`, every open one —
    /// paints the new definitions. Without this the doc would be read
    /// once at startup and a trader's colour edit would need a restart.
    #[gpui::test]
    fn a_reload_hands_the_factory_the_new_colours(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("colours", "[delta]\nhue = 240\n").unwrap(),
            ],
            desk: None,
            user: None,
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory: factory.clone(),
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        assert!(
            factory.colours().get("delta").is_none(),
            "fixture: the factory starts with no colours at all"
        );
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        assert!(
            factory.colours().get("delta").is_some(),
            "the reload must hand the factory the config's colours"
        );
    }

    /// Phase 4b §4.5: `attach`'s `cx.observe(&diagnostics, ..)` submits
    /// one `Request::Catalog` per drained `pending_catalog_request`, each
    /// with a fresh, higher tag. Two publishes while a diagnostics tile
    /// is watching submit tags 1 then 2 — an outcome answering the
    /// superseded tag 1 must be dropped (never applied to
    /// `Diagnostics.catalog`), and one answering the latest tag 2 must
    /// be applied. Exercises the real `attach`-installed observer and
    /// drain loop end to end, not a unit of either in isolation.
    #[gpui::test]
    fn a_stale_catalog_outcome_is_dropped_and_the_latest_is_applied(cx: &mut gpui::TestAppContext) {
        let window = open_test_window(cx, test_shell_services());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

        // Tag 1: watch, then publish once.
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            d.note_published("risk");
            cx.notify();
        });
        vcx.run_until_parked();
        // Tag 2: a second publish supersedes it.
        diagnostics.update(&mut vcx, |d, cx| {
            d.note_published("risk");
            cx.notify();
        });
        vcx.run_until_parked();

        // The stale tag-1 outcome must not be applied.
        tx.try_send(DataEvent::Catalog(CatalogOutcome {
            key: DIAGNOSTICS_KEY,
            tag: 1,
            snapshot: Ok(CatalogSnapshot::default()),
        }))
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.clone()),
            None,
            "a stale (superseded) tag must not be applied"
        );

        // The fresh tag-2 outcome must be applied.
        let fresh = CatalogSnapshot {
            threads: 4,
            ..CatalogSnapshot::default()
        };
        tx.try_send(DataEvent::Catalog(CatalogOutcome {
            key: DIAGNOSTICS_KEY,
            tag: 2,
            snapshot: Ok(fresh.clone()),
        }))
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.clone()),
            Some(fresh),
            "the latest tag must be applied"
        );
    }

    /// MIN-6 (Phase 4b Task 4 fix round 2): `watch()`'s own doc comment
    /// states the contract — it queues a request but does not (cannot)
    /// notify by itself, so the caller must `cx.notify()` in the same
    /// update for the bridge's drain to see it. Exercised end to end
    /// through the real `attach()`-installed observer and a real
    /// `DataHandle::for_tests()` receiver: `watch()` + `cx.notify()`
    /// must produce a `Request::Catalog` on the wire. This is the
    /// contract Task 5's `set_visible(true)` has to follow.
    #[gpui::test]
    fn watch_reaching_the_bridge_drain_requires_the_callers_own_notify(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = open_test_window(cx, test_shell_services());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, request_rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();

        match request_rx.try_recv() {
            Ok(geode_data::Request::Catalog(_)) => {}
            other => panic!("expected a Request::Catalog on the wire, got {other:?}"),
        }
    }

    /// MAJ-7 (Phase 4b Task 5, re-reviewed fix round 2: "tested through
    /// the bridge drain" — the entity- and tile-level tests added in fix
    /// round 1 were not enough on their own). The data thread computes
    /// the data section's resolved-generation marker under the as-of
    /// carried on the `CatalogParams` of the request that produced the
    /// held `CatalogSnapshot`; nothing re-requested one when the frame's
    /// as-of changed until `DiagnosticsTile`'s own frame observer started
    /// calling `Diagnostics::request_catalog()`. Exercised end to end
    /// through the real `geode_diagnostics::DiagnosticsFactory`, a real
    /// visible tile, and the real `attach()`-installed observer + drain:
    /// opening the tile watches (the first `Request::Catalog`, drained
    /// here), then changing the frame's as-of must produce a SECOND
    /// `Request::Catalog` carrying the new as-of — not the entity/tile
    /// unit tests' proxy of "the pending flag got set", but the real
    /// request landing on the wire with the right value.
    #[gpui::test]
    fn an_as_of_change_on_a_visible_diagnostics_tile_requests_a_second_catalog_with_the_new_as_of(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = open_test_window(cx, test_shell_services());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, request_rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

        // A real diagnostics tile, created through the real factory, the
        // same door `main.rs`'s roster and `ensure_occupants` use.
        let diagnostics_factory =
            DiagnosticsFactory::new(Arc::new(Ring::new(64)), Config::default());
        let occupant = vcx.update(|window, cx| {
            diagnostics_factory.create(
                TileId(999),
                None,
                frame.clone(),
                diagnostics.clone(),
                window,
                cx,
            )
        });
        // Becoming visible watches — the same first `Request::Catalog`
        // `watch_reaching_the_bridge_drain_requires_the_callers_own_notify`
        // proves above; drained here so only the as-of-driven second
        // request is left to observe.
        vcx.update(|_window, cx| {
            occupant.content.set_visible(true, cx);
        });
        vcx.run_until_parked();
        match request_rx.try_recv() {
            Ok(geode_data::Request::Catalog(_)) => {}
            other => panic!("expected the first Request::Catalog on visibility, got {other:?}"),
        }

        let at = chrono::Utc::now();
        frame.update(&mut vcx, |f, cx| {
            f.set_as_of(AsOf::At(at));
            cx.notify();
        });
        vcx.run_until_parked();

        match request_rx.try_recv() {
            Ok(geode_data::Request::Catalog(params)) => {
                assert_eq!(
                    params.as_of,
                    AsOf::At(at),
                    "the second catalog request must carry the new as-of"
                );
            }
            other => {
                panic!("expected a second Request::Catalog carrying the new as-of, got {other:?}")
            }
        }
    }

    /// CRIT-1: a healthy desk — one configured source, no `Health`
    /// event ever emitted for it (the honest steady state: `geode-data`
    /// only sends `Health` when discovery has something worth reporting,
    /// never a routine "still fine") — must not show anything in the
    /// summary. Before the fix, `attach`'s `describe_source` alone
    /// created a `SourceState` whose default health counted as
    /// `pending`, so a perfectly healthy start showed a permanent,
    /// warning-toned `sources 1 pending`.
    #[gpui::test]
    fn a_configured_source_with_no_health_event_reports_nothing(cx: &mut gpui::TestAppContext) {
        let window = open_test_window(cx, test_shell_services());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: vec![SourceSpec {
                pending_timeout: Duration::from_secs(120),
                ..SourceSpec::directory("risk", "risk", vec!["/data/risk/*.csv".into()])
            }],
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

        // `attach` already ran `describe_source` synchronously (it's not
        // behind the async drain loop); no event was ever sent.
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.summary()).as_ref(),
            "",
            "a configured-but-unreported source must not appear in the summary"
        );
        assert!(
            diagnostics
                .read_with(&vcx, |d, _| d.sources["risk"].health.clone())
                .is_none(),
            "no report yet — not Health::Pending, not anything"
        );
    }

    #[test]
    fn the_database_path_prefers_config_then_demo_then_the_platform_dir() {
        let empty = Config::load(&ConfigSources::default());
        assert_eq!(
            db_path(
                &empty,
                None,
                Some("C:\\Users\\me\\AppData\\Local".into()),
                Some("/home/me".into())
            ),
            PathBuf::from("C:\\Users\\me\\AppData\\Local")
                .join("Geode")
                .join("geode.duckdb")
        );
        let unix = db_path(&empty, None, None, Some("/home/me".into()));
        assert!(
            unix.ends_with("Geode/geode.duckdb") || unix.ends_with("geode/geode.duckdb"),
            "{unix:?}"
        );
        assert_eq!(
            db_path(
                &empty,
                Some(std::path::Path::new("/tmp/geode-demo/100000-42")),
                None,
                None
            ),
            PathBuf::from("/tmp/geode-demo/100000-42/geode.duckdb")
        );
        let configured = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "[data]\ndb_path = \"/var/geode/x.duckdb\"\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        assert_eq!(
            db_path(
                &configured,
                Some(std::path::Path::new("/tmp/d")),
                None,
                None
            ),
            PathBuf::from("/var/geode/x.duckdb"),
            "config wins even over demo"
        );
    }

    #[test]
    fn data_setup_needs_datasets_and_views_and_carries_sources() {
        let none = Config::load(&ConfigSources::default());
        assert!(
            data_setup(
                &none,
                "/tmp/x.duckdb".into(),
                AdapterRegistry::default(),
                PricerRegistry::default()
            )
            .is_none()
        );
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
                LayerDoc::builtin("sources", "[s]\ndataset = \"risk\"\npaths = [\"/x/*.csv\"]\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(
            &config,
            "/tmp/x.duckdb".into(),
            AdapterRegistry::default(),
            PricerRegistry::default(),
        )
        .unwrap();
        assert_eq!(setup.config.sources.len(), 1);
        assert_eq!(setup.views.len(), 1);
        assert_eq!(setup.config.query_workers, 4);
    }

    /// The views handed to the data service and the blotter factory are
    /// the *merged* ones: `data_setup` must go through
    /// `geode_core::config::load_views`, never read the `views` doc
    /// itself. Reading it directly still compiles and still produces
    /// views — it just silently discards the trader's
    /// `view_presentation.toml`, which is exactly the failure the
    /// presentation split exists to prevent (spec §5.6), and nothing
    /// downstream can tell the difference.
    #[test]
    fn data_setup_hands_out_views_with_the_users_presentation_already_merged() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
                     [[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                     [[v.columns]]\nname = \"npv\"\nkind = \"measure\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "view_presentation",
                    "[v]\norder = [\"npv\", \"book\"]\n[v.width]\nnpv = 140\n",
                )
                .unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(
            &config,
            "/tmp/x.duckdb".into(),
            AdapterRegistry::default(),
            PricerRegistry::default(),
        )
        .unwrap();
        let view = &setup.views[0];
        let names: Vec<&str> = view.columns.iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["npv", "book"], "presentation order applied");
        assert_eq!(view.presentation_of("npv").width, Some(140.0));
        // The service is built from the same list, not a second read.
        let served: Vec<&str> = setup.config.views[0]
            .columns
            .iter()
            .map(|c| c.name())
            .collect();
        assert_eq!(served, names);
    }

    /// dataset-presentation spec §6: the dataset-level overlay is merged
    /// *under* the view-level one, so a view's own `view_presentation.toml`
    /// entry wins where both set the same key, and a view that never
    /// touched a key still gets the dataset's value.
    #[test]
    fn data_setup_hands_out_views_with_the_dataset_level_merged_under_the_view_level() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n[[v.columns]]\nname = \"npv\"\nkind = \"measure\"\n[w]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[w.columns]]\nname = \"npv\"\nkind = \"measure\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("dataset_presentation", "[risk.columns.npv]\nlabel = \"NPV k\"\nscale = \"k\"\n").unwrap(),
                LayerDoc::builtin("view_presentation", "[v.columns.npv]\nlabel = \"NPV\"\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(
            &config,
            "/tmp/x.duckdb".into(),
            AdapterRegistry::default(),
            PricerRegistry::default(),
        )
        .unwrap();
        let v = setup.views.iter().find(|v| v.name == "v").unwrap();
        let w = setup.views.iter().find(|v| v.name == "w").unwrap();
        assert_eq!(v.presentation_of("npv").label.as_deref(), Some("NPV"));
        assert_eq!(w.presentation_of("npv").label.as_deref(), Some("NPV k"));
        assert_eq!(
            v.presentation_of("npv").scale,
            Some(geode_core::view::Scale::Thousands),
            "an unset view key keeps the dataset's"
        );
    }

    /// 2c §6.2: the `colours` doc travels to the blotter through
    /// `DataSetup` like the views do, and its reader's own diagnostics
    /// travel with it — `load_views` (the only other place the doc is
    /// read) discards them, so without this every malformed colour in
    /// `colours.toml` would be dropped in total silence.
    #[test]
    fn data_setup_carries_the_colours_and_reports_their_diagnostics() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n")
                    .unwrap(),
                LayerDoc::builtin(
                    "colours",
                    "[delta]\nhue = 240\n[broken]\nhue = 240\ntoken = \"danger\"\n",
                )
                .unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(
            &config,
            "/tmp/x.duckdb".into(),
            AdapterRegistry::default(),
            PricerRegistry::default(),
        )
        .unwrap();
        assert!(
            setup.colours.get("delta").is_some(),
            "the good definition must reach the factory"
        );
        assert!(
            setup.colours.get("broken").is_none(),
            "the refused one must not"
        );
        assert!(
            setup
                .diagnostics
                .iter()
                .any(|d| d.message.contains("colour 'broken'")),
            "a malformed colour must be reported, not dropped in silence: {:?}",
            setup.diagnostics
        );
    }

    #[test]
    fn stale_after_reads_the_configured_duration_and_falls_back_to_the_default() {
        let none = Config::load(&ConfigSources::default());
        assert_eq!(
            stale_after_from_config(&none),
            geode_blotter::tile::DEFAULT_STALE_AFTER
        );
        let configured = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[blotter]\nstale_after = \"90s\"\n").unwrap()],
            ..ConfigSources::default()
        });
        assert_eq!(
            stale_after_from_config(&configured),
            Duration::from_secs(90)
        );
        let junk = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "[blotter]\nstale_after = \"not-a-duration\"\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        assert_eq!(
            stale_after_from_config(&junk),
            geode_blotter::tile::DEFAULT_STALE_AFTER,
            "an unparsable value falls back rather than panicking"
        );
    }

    #[test]
    fn a_refused_event_is_counted_as_dropped_rather_than_lost_silently() {
        let (tx, rx) = async_channel::bounded::<DataEvent>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = make_sink(tx, dropped.clone());
        assert!(sink(DataEvent::Diagnostics(Vec::new())), "the first fits");
        assert!(
            !sink(DataEvent::Diagnostics(Vec::new())),
            "the channel is now full"
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        drop(rx);
    }
}
