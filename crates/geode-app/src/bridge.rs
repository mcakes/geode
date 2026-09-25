//! Application wiring between shell and data. Build service configuration and
//! module factories from shared inputs, route coalesced mailbox events through
//! the window, and forward view reloads to the service. Shell and data remain
//! independent crates. See `docs/current/request-delivery.md`.

use geode_blotter::BlotterFactory;
use geode_core::colour::NamedColours;
use geode_core::config::{Config, Diagnostic, Severity, load_views};
use geode_core::dimensions::DerivedDimensions;
use geode_core::egress_config;
use geode_core::query::{CatalogParams, DistinctOutcome};
use geode_core::schema::SchemaSpec;
use geode_core::source_config::{SourceShape, parse_duration};
use geode_core::view::ViewSpec;
use geode_data::adapter::AdapterRegistry;
use geode_data::documents::DocumentRegistry;
use geode_data::source::SourceSpec;
use geode_data::{
    DataEvent, DataHandle, DataService, DataServiceConfig, EventSink, PricerConfig, PricerRegistry,
};
use geode_marketdata::MarketDataFactory;
use geode_marketdata::core::{CVI, DIVIDEND};
use geode_pricer::content::{PricerFactory, PricerSettings};
use geode_pricer::core::{PRICER_VIEWS_DOC, Views};
use geode_pricer::store::MemorySheetStore;
use geode_shell::diagnostics::{CatalogRequest, Diagnostics, SourceSummary};
use geode_shell::module::{Delivery, UploadDelivery};
use geode_shell::shell::{DIAGNOSTICS_KEY, ShellEvent, ShellView};
use geode_shell::vimfind::FindStyle;
use gpui::{App, AsyncApp, Entity, WindowHandle};
use gpui_component::Root;
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

pub struct DataSetup {
    pub config: DataServiceConfig,
    pub views: Vec<ViewSpec>,
    /// Startup dimensions shared with the service and blotter's filter validation.
    pub dimensions: DerivedDimensions,
    /// Named colours shared by module factories. Parse separately from load_views
    /// to retain colour-definition diagnostics as well as unknown-reference warnings.
    pub colours: NamedColours,
    pub diagnostics: Vec<Diagnostic>,
    /// Datasets declared local. Their publications update diagnostics but skip
    /// frame publication history and revisions, preventing autosave invalidation.
    pub local_datasets: HashSet<String>,
    /// The pricer's views and settings; `stale_after` is filled by `start`.
    pub pricer_views: Views,
    pub pricer_settings: PricerSettings,
    /// What the pricer read out of this config (`pricer_config_key`), so
    /// the reload observer can tell a reload that changed none of it.
    pub pricer_key: PricerConfigKey,
}

/// Build setup when both datasets and views documents are present. Empty
/// parsed definitions still produce Some with any diagnostics; missing either
/// document returns None. Adapters and pricers come from the caller's registry;
/// builtin document kinds are registered here for every setup.
pub fn data_setup(
    config: &Config,
    db_path: PathBuf,
    adapters: AdapterRegistry,
    pricers: PricerRegistry,
) -> Option<DataSetup> {
    let datasets = config.doc("datasets")?;
    // Require a views document, then use load_views for presentation overlays.
    // A direct parse would omit the trader's effective presentation settings.
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
    // `egress.toml` (egress spec §4, §10 amendments 1/2): typed, then
    // resolved against `adapters` — the same registry this function's
    // caller ultimately hands to `DataServiceConfig.adapters` below, read
    // here (by reference) before that move. An unknown adapter or one
    // with no egress side drops the target with a diagnostic; the
    // survivors are what `DataServiceConfig.egress` carries and what
    // `bridge::start` narrows per document for the market-data factories.
    let (egress_specs, d) = config
        .doc("egress")
        .map(|doc| egress_config::from_doc(doc, &schema))
        .unwrap_or_default();
    diagnostics.extend(d);
    let (egress, d) = geode_data::egress::resolve(egress_specs, &adapters);
    diagnostics.extend(d);
    let (colours, colour_diags) = config
        .doc("colours")
        .map(NamedColours::from_doc)
        .unwrap_or_default();
    diagnostics.extend(colour_diags);
    // Resolve the configured pricer, defaulting to mock. An unavailable name
    // warns and installs a missing-pricer implementation that fails each line,
    // allowing the rest of the service to open.
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
    let (pricer_views, view_diags) = pricer_views_from_config(config);
    diagnostics.extend(view_diags);
    let (refresh, refresh_diag) = pricing_refresh_from_config(config);
    diagnostics.extend(refresh_diag);
    let pricer_settings = PricerSettings {
        pricer: pricer_name.clone(),
        pricer_missing: pricer.pricer.is_none(),
        refresh,
        stale_after: Duration::default(),
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
            egress,
        },
        views,
        dimensions,
        colours,
        diagnostics,
        local_datasets,
        pricer_views,
        pricer_settings,
        pricer_key: pricer_config_key(config),
    })
}

/// Choose data.db_path from app.toml, then the demo directory, then the
/// supplied platform data directory/home fallback.
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

/// Read blotter.stale_after from app.toml using the shared duration parser.
/// Missing or invalid values use the blotter's 15-minute default.
pub fn stale_after_from_config(config: &Config) -> Duration {
    config
        .get("app", "blotter.stale_after")
        .and_then(|v| v.as_str())
        .and_then(geode_data::source::parse_duration)
        .unwrap_or(geode_blotter::tile::DEFAULT_STALE_AFTER)
}

/// `[pricing] refresh` (spec §5.5, §9.4): the app default periodic
/// reprice. Absent is 30 s, `"off"` disables it, a duration is that; an
/// unreadable value keeps 30 s and says so (planning decision 21).
pub const DEFAULT_PRICING_REFRESH: Duration = Duration::from_secs(30);

pub fn pricing_refresh_from_config(config: &Config) -> (Option<Duration>, Option<Diagnostic>) {
    let Some(value) = config.get("app", "pricing.refresh") else {
        return (Some(DEFAULT_PRICING_REFRESH), None);
    };
    if value.as_str() == Some("off") {
        return (None, None);
    }
    if let Some(d) = value
        .as_str()
        .and_then(parse_duration)
        .filter(|d| !d.is_zero())
    {
        return (Some(d), None);
    }
    (
        Some(DEFAULT_PRICING_REFRESH),
        Some(Diagnostic {
            severity: Severity::Warning,
            layer: config.explain("app", "pricing.refresh"),
            file: None,
            message: format!("[pricing] refresh = {value} is not a duration or \"off\"; using 30s"),
            path: Some("app.pricing.refresh".to_string()),
        }),
    )
}

/// The `pricer_views` doc, or the bundled two when no layer has one (the
/// builtin layer always does in the app; a test config may not).
pub fn pricer_views_from_config(config: &Config) -> (Views, Vec<Diagnostic>) {
    match config.doc(PRICER_VIEWS_DOC) {
        Some(doc) => Views::from_doc(doc),
        None => (Views::builtin(), Vec::new()),
    }
}

/// Exactly what the pricer reads out of a config: the merged
/// `pricer_views` doc, the raw `[pricing] refresh` value and the resolved
/// stale threshold. Two equal keys resolve to the same views and settings,
/// so the reload observer skips a reload whose key is unchanged — a theme
/// or keymap edit must not restart every tile's refresh timer or repeat a
/// bad value's warning.
#[derive(Debug, Clone, PartialEq)]
pub struct PricerConfigKey {
    views: Option<toml::Table>,
    refresh: Option<toml::Value>,
    stale_after: Duration,
}

pub fn pricer_config_key(config: &Config) -> PricerConfigKey {
    PricerConfigKey {
        views: config.doc(PRICER_VIEWS_DOC).map(|d| d.value.clone()),
        refresh: config.get("app", "pricing.refresh").cloned(),
        stale_after: stale_after_from_config(config),
    }
}

/// Workers offer state without waiting for the UI. Bursts coalesce in the
/// mailbox; only a closed receiver refuses delivery. Count each refusal and
/// log closure once, while allowing producers to continue their work.
fn make_sink(tx: crate::events::Sender, dropped: Arc<AtomicU64>) -> EventSink {
    let warned_closed = Arc::new(AtomicBool::new(false));
    Arc::new(move |e| match tx.try_send(e) {
        Ok(()) => true,
        Err(_) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            if !warned_closed.swap(true, Ordering::Relaxed) {
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
    /// CVI factory sharing this bridge's handle. Retained for reload updates to
    /// the stale threshold.
    pub marketdata: Rc<MarketDataFactory>,
    /// Dividend factory sharing the marketdata context. Suppress its duplicate
    /// keymap fragment while retaining its own actions and stale threshold.
    pub dividend: Rc<MarketDataFactory>,
    /// Timeseries factory sharing the data handle and named colours. Retained
    /// so reload can update the chart palette.
    pub timeseries: Rc<geode_timeseries::content::TimeseriesFactory>,
    /// The line pricer's factory, sharing the handle. Retained so a reload
    /// reaches its views and settings.
    pub pricer: Rc<PricerFactory>,
    events: crate::events::Receiver,
    dropped: Arc<AtomicU64>,
    /// Startup source descriptions paired with their schema-derived pipeline.
    /// Diagnostics describe the running service even when edited source config
    /// awaits restart.
    sources: Vec<(SourceSpec, SourceShape)>,
    /// Local dataset names used to exclude autosave from frame publication updates.
    pub local_datasets: Rc<HashSet<String>>,
    /// The config key the pricer factory was built from; seeds the reload
    /// observer, so a reload that changes nothing the pricer reads is
    /// skipped from the first one. `None` when the factory's config is
    /// unknown: the first reload then always applies.
    pub pricer_key: Option<PricerConfigKey>,
}

/// Pair sources with their pipeline using the service's startup schema.
/// Adapter name alone cannot distinguish subscriptions from fetch sources.
fn source_shapes(sources: &[SourceSpec], schema: &SchemaSpec) -> Vec<(SourceSpec, SourceShape)> {
    sources
        .iter()
        .map(|s| (s.clone(), s.shape(schema)))
        .collect()
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
    let (tx, rx) = crate::events::channel();
    let dropped = Arc::new(AtomicU64::new(0));
    let sink = make_sink(tx, dropped.clone());
    // Share startup schema and dimensions with filter validation before moving
    // service configuration to its worker thread.
    let schema = setup.config.schema.clone();
    let dimensions = setup.dimensions.clone();
    let sources = source_shapes(&setup.config.sources, &schema);
    let local_datasets = Rc::new(setup.local_datasets);
    // Target name → accepted document names, in `egress.toml`
    // order — captured before `DataService::spawn` moves `setup.config`,
    // and shared unmodified between the CVI and dividend factories built
    // below over the SAME resolved list (`MarketDataFactory::create`
    // narrows it per document with its own `targets_for`).
    let egress_targets: Arc<Vec<(String, Vec<String>)>> = Arc::new(
        setup
            .config
            .egress
            .iter()
            .map(|spec| {
                (
                    spec.name.clone(),
                    spec.documents.iter().map(|(doc, _)| doc.clone()).collect(),
                )
            })
            .collect(),
    );
    let handle = DataService::spawn(setup.config, sink);
    // Both factories receive the same startup colours and later reload updates.
    let timeseries = Rc::new(geode_timeseries::content::TimeseriesFactory::new(
        handle.clone(),
        setup.colours.clone(),
    ));
    let factory = Rc::new(BlotterFactory::new(
        handle.clone(),
        setup.views,
        setup.colours,
        schema,
        dimensions,
        find_style,
        stale_after,
    ));
    let mut pricer_settings = setup.pricer_settings.clone();
    let pricer_key = setup.pricer_key.clone();
    pricer_settings.stale_after = stale_after;
    // Part 3's store is in-memory (line-pricer Part 3, planning decision
    // 8): a sheet survives closing and reopening a tile, not a restart.
    let pricer = Rc::new(PricerFactory::new(
        handle.clone(),
        Rc::new(MemorySheetStore::default()),
        setup.pricer_views.clone(),
        pricer_settings,
    ));
    Bridge {
        marketdata: Rc::new(
            MarketDataFactory::new(
                handle.clone(),
                &CVI,
                // One threshold, one config key: a document's own
                // freshness means exactly what a dataset's does to the
                // blotter, and two keys for one idea would be two things
                // to keep in step.
                stale_after,
            )
            .with_egress(egress_targets.clone()),
        ),
        // The second document kind, over the same shared `marketdata`
        // context: `.without_keymap()` is what keeps `keymap_fragments`
        // from splicing a second, identical `<module:{kind}>` layer. The
        // same `egress_targets` `Arc`, narrowed to its own document by
        // `create`.
        dividend: Rc::new(
            MarketDataFactory::new(handle.clone(), &DIVIDEND, stale_after)
                .without_keymap()
                .with_egress(egress_targets),
        ),
        timeseries,
        pricer,
        handle,
        factory,
        events: rx,
        dropped,
        sources,
        local_datasets,
        pricer_key: Some(pricer_key),
    }
}

/// Window-local request lifecycle. The diagnostics entity owns the single
/// pending refresh bit; only the bridge owns submissions and their replies.
#[derive(Default)]
struct CatalogRefresh {
    tag: Cell<u64>,
    in_flight: Cell<Option<(u64, CatalogRequest)>>,
    retry_pending: Cell<bool>,
}

const CATALOG_RETRY_DELAY: Duration = Duration::from_secs(1);

impl CatalogRefresh {
    /// Retain demand after refusal/error without spinning the observer. At most
    /// one timer exists, and it neither retains the window nor the data handle.
    fn retry(
        self: &Rc<Self>,
        diagnostics: &Entity<Diagnostics>,
        request: CatalogRequest,
        window: WindowHandle<Root>,
        cx: &mut App,
    ) {
        let pending = diagnostics.update(cx, |d, _| {
            match request {
                CatalogRequest::Watched => d.request_catalog_refresh(),
                CatalogRequest::Explicit => d.request_catalog(),
            }
            d.pending_catalog_request()
        });
        if !pending || self.retry_pending.replace(true) {
            return;
        }
        let refresh = self.clone();
        let diagnostics = diagnostics.downgrade();
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(CATALOG_RETRY_DELAY).await;
            refresh.retry_pending.set(false);
            let _ = window.update(cx, |_, _, cx| {
                let _ = diagnostics.update(cx, |d, cx| {
                    if d.pending_catalog_request() {
                        cx.notify();
                    }
                });
            });
        })
        .detach();
    }
}

/// Route mailbox events through the window and forward reloads. Awaiting the
/// receiver wakes the foreground task on arrival; scheduling and UI work still
/// determine delivery latency. The task checks window liveness on each event.
pub fn attach(bridge: &Bridge, window: WindowHandle<Root>, cx: &mut App) {
    let rx = bridge.events.clone();
    let dropped = bridge.dropped.clone();
    let handle = bridge.handle.clone();
    let factory = bridge.factory.clone();
    let marketdata = bridge.marketdata.clone();
    let dividend = bridge.dividend.clone();
    let timeseries = bridge.timeseries.clone();

    let shell = window
        .read(cx)
        .ok()
        .and_then(|root| root.view().clone().downcast::<ShellView>().ok())
        .expect("the window's root view is the shell");
    let diagnostics = shell.read(cx).diagnostics().clone();

    // Describe startup sources once using shared summary values, keeping shell
    // independent of geode-data types.
    diagnostics.update(cx, |d, cx| {
        for (source, shape) in &bridge.sources {
            d.describe_source(
                &source.name,
                SourceSummary {
                    paths: source.paths.clone(),
                    priority: format!("{:?}", source.priority),
                    readiness: format!("{:?}", source.readiness),
                    adapter: source.adapter.clone(),
                    // Already empty for a directory source — `from_doc`
                    // reads `topics` only when the source is subscribed —
                    // so it is cloned rather than gated here.
                    topics: source.topics.clone(),
                    // Use the startup schema's classification; downstream diagnostics have only
                    // this summary, not the schema needed to distinguish fetch from subscription.
                    shape: *shape,
                },
            );
        }
        cx.notify();
    });

    // Leave demand in Diagnostics while a request or retry is outstanding.
    // Publications, visibility and as-of changes share that one follow-up bit.
    let catalog_refresh = Rc::new(CatalogRefresh::default());
    cx.observe(&diagnostics, {
        let handle = handle.clone();
        let diagnostics = diagnostics.clone();
        let shell = shell.clone();
        let refresh = catalog_refresh.clone();
        move |_entity, cx| {
            if refresh.in_flight.get().is_some() || refresh.retry_pending.get() {
                return;
            }
            let Some(request) = diagnostics.update(cx, |d, _| d.take_catalog_request()) else {
                return;
            };
            let tag = refresh.tag.get() + 1;
            refresh.tag.set(tag);
            let as_of = shell.read(cx).frame().read(cx).as_of().clone();
            if handle.catalog(CatalogParams {
                key: DIAGNOSTICS_KEY,
                tag,
                as_of,
            }) {
                refresh.in_flight.set(Some((tag, request)));
            } else {
                tracing::warn!(
                    target: "geode::query",
                    "catalog request refused — the data service is busy or gone"
                );
                refresh.retry(&diagnostics, request, window, cx);
            }
        }
    })
    .detach();

    // Reloads: new views to the data thread and to the factory.
    cx.subscribe(&shell, {
        let handle = handle.clone();
        let factory = factory.clone();
        let marketdata = marketdata.clone();
        let dividend = dividend.clone();
        let timeseries = timeseries.clone();
        let diagnostics = diagnostics.clone();
        move |shell, event: &ShellEvent, cx| match event {
            ShellEvent::ConfigReloaded => {
                let config = shell.read(cx).config();
                if config.doc("views").is_none() {
                    return;
                }
                // Use the same presentation-aware view loader as startup.
                let (views, presentation_diags) = load_views(config);
                // Log presentation diagnostics and append/deduplicate them in the data lane.
                // Replacing the config lane here would erase the shell's reload diagnostics.
                // Defer entity mutation until the config borrow is no longer needed.
                for d in &presentation_diags {
                    tracing::warn!(target: "geode::query", "{d}");
                }
                // Parse named colours separately to retain their own validation diagnostics;
                // load_views uses them for reference checks but does not return those errors.
                let (colours, colour_diags) = config
                    .doc("colours")
                    .map(NamedColours::from_doc)
                    .unwrap_or_default();
                for d in &colour_diags {
                    tracing::warn!(target: "geode::query", "{d}");
                }
                // Update both factories from one parsed colour definition set.
                timeseries.set_colours(colours.clone());
                factory.set_colours(colours);
                let (dims, _) = config
                    .doc("dimensions")
                    .map(DerivedDimensions::from_doc)
                    .unwrap_or_default();
                // Refresh factory settings on ConfigReloaded. A stale_after-only edit does
                // not emit this event; it takes effect on a later view/presentation/dimensions/
                // colours reload or restart.
                factory.set_views(views.clone());
                factory.set_find_style(FindStyle::from_config(config));
                let stale_after = stale_after_from_config(config);
                factory.set_stale_after(stale_after);
                // Document panels share the same stale threshold and reload trigger.
                marketdata.set_stale_after(stale_after);
                // Apply the shared threshold to the dividend factory as well.
                dividend.set_stale_after(stale_after);
                // Refresh the factory's validation schema from current config. Dataset-only
                // edits require restart and do not emit ConfigReloaded; a later eligible
                // reload can update this factory before the service's schema is rebuilt.
                if let Some(schema) = config.doc("datasets").map(|d| SchemaSpec::from_doc(d).0) {
                    factory.set_schema(schema);
                }
                factory.set_dims(dims.clone());
                handle.replace_views(views, dims);
                // The config borrow has ended; diagnostics can now be updated through cx.
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
            // The shell requests distinct values through an event because it cannot
            // call geode-data directly. If the service request channel refuses, deliver
            // a synthetic error with the same key/tag/column so the picker stops waiting.
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
            // The shell already logged and installed the rejected reload's diagnostics.
            ShellEvent::ReloadRejected(_) => {}
        }
    })
    .detach();

    // The pricer watches EVERY applied reload (planning decision 20):
    // `ShellEvent::ConfigReloaded` fires only for five named docs, and a
    // `pricer_views` or `[pricing] refresh` edit is neither. The frame's
    // `config` counter is the ungated signal (`main.rs`'s diagnostics
    // factory observes it the same way); `pricer_config_key` then gates
    // it down to the reloads that change what the pricer reads.
    {
        let pricer = bridge.pricer.clone();
        let diagnostics = diagnostics.clone();
        let shell = shell.clone();
        let frame = shell.read(cx).frame().clone();
        let last = Rc::new(Cell::new(frame.read(cx).versions().config));
        // Seeded with the key the factory was built from; `None` (a factory
        // built from an unknown config) lets the first reload through.
        let last_key = Rc::new(std::cell::RefCell::new(bridge.pricer_key.clone()));
        cx.observe(&frame, move |frame, cx| {
            let now = frame.read(cx).versions().config;
            if now == last.get() {
                return;
            }
            last.set(now);
            // Read everything out of the config before the factory takes
            // `cx` mutably.
            let (views, mut diags, refresh, stale_after) = {
                let config = shell.read(cx).config();
                let key = pricer_config_key(config);
                if last_key.borrow().as_ref() == Some(&key) {
                    return;
                }
                *last_key.borrow_mut() = Some(key);
                let (views, diags) = pricer_views_from_config(config);
                let (refresh, refresh_diag) = pricing_refresh_from_config(config);
                let mut diags = diags;
                diags.extend(refresh_diag);
                (views, diags, refresh, stale_after_from_config(config))
            };
            pricer.reload(views, refresh, stale_after, cx);
            for d in &diags {
                tracing::warn!(target: "geode::pricing", "{d}");
            }
            if !diags.is_empty() {
                diagnostics.update(cx, |dg, cx| {
                    let before = dg.version();
                    dg.note_data_diagnostics(std::mem::take(&mut diags), SystemTime::now());
                    if dg.version() != before {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    let diagnostics_for_drain = diagnostics.clone();
    let catalog_refresh_for_drain = catalog_refresh.clone();
    // Retain local dataset names for the drain task after attach's borrow ends.
    let local_datasets = Rc::clone(&bridge.local_datasets);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let diagnostics = diagnostics_for_drain;
        let catalog_refresh = catalog_refresh_for_drain;
        let catalog_window = window;
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);
            // Check window liveness for every event variant. Updating a retained shell
            // entity alone would keep succeeding after the window closes and retain this
            // task's captures. With no new event, the task can remain awaiting the mailbox.
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
                    // Routed by the submitting tile's key, exactly as a
                    // `Query`/`Series` outcome is: the tile that uploaded
                    // is the one whose draft enters `Sent` or shows the
                    // failure. `geode_data::egress` already logs the
                    // outcome under `geode::ingest`.
                    DataEvent::Upload(outcome) => {
                        shell.update(cx, |s, cx| {
                            s.deliver(
                                Delivery::Upload(UploadDelivery {
                                    key: outcome.key,
                                    tag: outcome.tag,
                                    target: outcome.target,
                                    result: outcome.result,
                                }),
                                window,
                                cx,
                            )
                        });
                    }
                    DataEvent::Query(outcome) => {
                        shell.update(cx, |s, cx| {
                            s.deliver(Delivery::Query(outcome), window, cx)
                        });
                    }
                    // Route a series answer by requester key, as for Query.
                    DataEvent::Series(outcome) => {
                        shell.update(cx, |s, cx| {
                            s.deliver(Delivery::Series(outcome), window, cx)
                        });
                    }
                    DataEvent::Published {
                        dataset,
                        batch,
                        books,
                        ..
                    } => {
                        // Record publication in diagnostics and request catalog refresh when watched.
                        // The service owns the detailed publication log; do not duplicate it here.
                        diagnostics.update(cx, |d, cx| {
                            d.note_published(&dataset);
                            cx.notify();
                        });
                        if local_datasets.contains(&dataset) {
                            // Local autosave updates diagnostics without advancing frame revisions or
                            // recent-publication history.
                        } else {
                            let frame = shell.read(cx).frame().clone();
                            // Recent-publication history uses arrival time; this event has no source
                            // timestamp and cannot establish source freshness.
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
                        // Update diagnostics with combined source health. The data service already
                        // logs the transition at its severity; this arm changes retained UI state.
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
                        // Append service diagnostics in their own retained lane. Replacing config
                        // diagnostics here would let service reports and config reloads erase each other.
                        diagnostics.update(cx, |dg, cx| {
                            let before = dg.version();
                            dg.note_data_diagnostics(diags, SystemTime::now());
                            if dg.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    // The picker validates request tag, column, and open state in deliver_distinct.
                    DataEvent::Distinct(outcome) => {
                        shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                    }
                    DataEvent::Catalog(outcome) => {
                        let Some((tag, request)) = catalog_refresh.in_flight.get() else {
                            return;
                        };
                        if outcome.key != DIAGNOSTICS_KEY || tag != outcome.tag {
                            return;
                        }
                        catalog_refresh.in_flight.set(None);
                        match outcome.snapshot {
                            Ok(snapshot) => {
                                let current_as_of = shell.read(cx).frame().read(cx).as_of().clone();
                                diagnostics.update(cx, |d, cx| {
                                    let before = d.version();
                                    // A publication while reading schedules another read, but
                                    // does not starve presentation of consistent snapshots.
                                    // An old as-of, however, must never replace the current one.
                                    if snapshot.as_of == current_as_of {
                                        d.set_catalog(snapshot);
                                    } else {
                                        // Explicit consumers need a current answer even when
                                        // no diagnostics tile observes the frame's as-of.
                                        match request {
                                            CatalogRequest::Watched => d.request_catalog_refresh(),
                                            CatalogRequest::Explicit => d.request_catalog(),
                                        }
                                    }
                                    // Even an unchanged/discarded snapshot releases pending work.
                                    if d.version() != before || d.pending_catalog_request() {
                                        cx.notify();
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(target: "geode::query", "catalog request failed: {e}");
                                catalog_refresh.retry(&diagnostics, request, catalog_window, cx);
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
                    // Record current ingest progress. Mailbox coalescing may omit intermediate
                    // starts or retain only the final LoadEnded.
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
                    // Broadcast fetch completion by identity/source to visible occupants. Each
                    // tile decides whether it watches that pair and whether to requery or show error.
                    DataEvent::SeriesFetched {
                        source,
                        identity,
                        result,
                    } => {
                        shell.update(cx, |s, cx| {
                            s.deliver(
                                Delivery::SeriesFetched {
                                    source,
                                    identity,
                                    result,
                                },
                                window,
                                cx,
                            )
                        });
                    }
                    // Clear the single ingest progress record. The writer runs one job at a time;
                    // extra end events, including queue drain, are harmless when already idle.
                    DataEvent::LoadEnded => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_load_ended();
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    // Route pricing to the keyed occupant; the shell discards absent recipients.
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

    /// Verify source descriptions retain the startup schema's pipeline pairing.
    /// Classification rules themselves are tested in source_config.
    #[test]
    fn source_shapes_names_each_of_the_three_shapes() {
        let (schema, diags) = SchemaSpec::from_doc(&geode_core::config::merge_docs(
            "datasets",
            &[LayerDoc::builtin(
                "datasets",
                r#"
[risk]
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "dimension"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "position"

[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"

[series]
family = "series"
"#,
            )
            .unwrap()],
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let files = SourceSpec::directory("risk_files", "risk", vec!["/x/*.csv".into()]);
        let bus = SourceSpec {
            adapter: "demo_bus".into(),
            document: Some("cvi_params".into()),
            topics: vec!["marketdata/cvi/>".into()],
            ..SourceSpec::directory("cvi", "cvi_params", Vec::new())
        };
        let kdb = SourceSpec {
            adapter: "demo_kdb".into(),
            ..SourceSpec::directory("history", "series", Vec::new())
        };
        let shapes = source_shapes(&[files, bus, kdb], &schema);
        assert_eq!(
            shapes
                .iter()
                .map(|(s, shape)| (s.name.as_str(), *shape))
                .collect::<Vec<_>>(),
            vec![
                ("risk_files", SourceShape::Directory),
                ("cvi", SourceShape::Subscribed),
                ("history", SourceShape::Fetch),
            ]
        );
    }

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
    fn a_burst_is_coalesced_without_a_delivery_refusal() {
        // A busy UI retains the latest state without refusing the producer.
        let (tx, _rx) = crate::events::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = make_sink(tx, dropped.clone());
        let records = logged(|| {
            assert!(sink(DataEvent::Diagnostics(Vec::new())), "the first fits");
            assert!(
                sink(DataEvent::Diagnostics(Vec::new())),
                "the latest state is retained"
            );
        });
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        assert!(
            !records.iter().any(|r| r.message.contains("receiver")),
            "a full channel must not be logged as a gone receiver: {records:?}"
        );
    }

    #[test]
    fn a_closed_channel_is_counted_and_logged_once() {
        let (tx, rx) = crate::events::channel();
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

    fn test_pricer(handle: &DataHandle) -> Rc<PricerFactory> {
        Rc::new(PricerFactory::new(
            handle.clone(),
            Rc::new(MemorySheetStore::default()),
            Views::builtin(),
            PricerSettings::default(),
        ))
    }

    /// A bridge whose every factory is a default; for tests that exercise
    /// one factory's reload path.
    fn test_bridge(handle: DataHandle) -> Bridge {
        let (_tx, rx) = crate::events::channel();
        Bridge {
            factory: Rc::new(BlotterFactory::new(
                handle.clone(),
                Vec::new(),
                NamedColours::default(),
                SchemaSpec::default(),
                DerivedDimensions::default(),
                FindStyle::default(),
                Duration::from_secs(900),
            )),
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
        }
    }

    #[test]
    fn pricing_refresh_reads_off_a_duration_and_defaults_to_thirty_seconds() {
        let config = |text: &str| {
            Config::load(&ConfigSources {
                builtin: vec![LayerDoc::builtin("app", text).unwrap()],
                desk: None,
                user: None,
            })
        };
        assert_eq!(
            pricing_refresh_from_config(&config("")),
            (Some(DEFAULT_PRICING_REFRESH), None)
        );
        assert_eq!(
            pricing_refresh_from_config(&config("[pricing]\nrefresh = \"off\"\n")),
            (None, None)
        );
        assert_eq!(
            pricing_refresh_from_config(&config("[pricing]\nrefresh = \"10s\"\n")),
            (Some(Duration::from_secs(10)), None)
        );
        let (refresh, diag) =
            pricing_refresh_from_config(&config("[pricing]\nrefresh = \"soon\"\n"));
        assert_eq!(refresh, Some(DEFAULT_PRICING_REFRESH));
        let diag = diag.expect("a bad value warns");
        assert_eq!(diag.path.as_deref(), Some("app.pricing.refresh"));
    }

    /// The reload gate's key moves with exactly the three things the
    /// pricer reads and with nothing else.
    #[test]
    fn the_pricer_config_key_changes_only_with_what_the_pricer_reads() {
        let config = |app: &str, views: &str| {
            Config::load(&ConfigSources {
                builtin: vec![
                    LayerDoc::builtin("app", app).unwrap(),
                    LayerDoc::builtin("pricer_views", views).unwrap(),
                ],
                desk: None,
                user: None,
            })
        };
        let app = "[theme]\nname = \"a\"\n[log]\nlevel = \"info\"\n\
                   [pricing]\nrefresh = \"10s\"\n[blotter]\nstale_after = \"5m\"\n";
        let views = "[slim]\ncolumns = [\"qty\", \"price\"]\n";
        let base = pricer_config_key(&config(app, views));
        assert_eq!(
            pricer_config_key(&config(&app.replace("\"a\"", "\"b\""), views)),
            base,
            "a [theme] edit"
        );
        assert_eq!(
            pricer_config_key(&config(&app.replace("\"info\"", "\"debug\""), views)),
            base,
            "a [log] edit"
        );
        assert_ne!(
            pricer_config_key(&config(app, &views.replace("\"qty\", ", ""))),
            base,
            "a pricer_views edit"
        );
        assert_ne!(
            pricer_config_key(&config(&app.replace("\"10s\"", "\"off\""), views)),
            base,
            "a [pricing] refresh edit"
        );
        assert_ne!(
            pricer_config_key(&config(&app.replace("\"5m\"", "\"6m\""), views)),
            base,
            "a stale_after edit"
        );
    }

    #[test]
    fn pricer_views_fall_back_to_the_bundled_two_with_no_doc() {
        let config = Config::load(&ConfigSources {
            builtin: vec![],
            desk: None,
            user: None,
        });
        let (views, diags) = pricer_views_from_config(&config);
        assert!(diags.is_empty());
        assert_eq!(
            views.names().collect::<Vec<_>>(),
            vec!["vanilla", "barrier"]
        );
    }

    /// Planning decision 20: a reload reaches the pricer through the
    /// frame's `config` counter — `ShellEvent::ConfigReloaded` never fires
    /// for a `pricer_views`-only edit.
    #[gpui::test]
    fn a_config_reload_hands_the_pricer_factory_its_views(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("pricer_views", "[slim]\ncolumns = [\"qty\", \"price\"]\n")
                    .unwrap(),
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
        let bridge = test_bridge(handle);
        assert_eq!(
            bridge.pricer.view_names(),
            vec!["vanilla", "barrier"],
            "fixture: built with the bundled views"
        );
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|_, cx| {
            let frame = shell.read(cx).frame().clone();
            frame.update(cx, |f, cx| {
                f.note_config_reloaded();
                cx.notify();
            });
        });
        vcx.run_until_parked();
        assert_eq!(bridge.pricer.view_names(), vec!["slim"]);
    }

    /// A reload that changes nothing the pricer reads (a theme, keymap or
    /// log-level edit) leaves the factory alone: no view re-resolution, no
    /// refresh-timer restart, no repeated warning. The factory's views are
    /// swapped for a sentinel behind the observer's back after the first
    /// reload, so only a second `reload` could put `slim` back.
    #[gpui::test]
    fn a_reload_that_changes_no_pricer_setting_leaves_the_factory_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("pricer_views", "[slim]\ncolumns = [\"qty\", \"price\"]\n")
                    .unwrap(),
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
        let bridge = test_bridge(handle);
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let bump = |vcx: &mut gpui::VisualTestContext| {
            vcx.update(|_, cx| {
                let frame = shell.read(cx).frame().clone();
                frame.update(cx, |f, cx| {
                    f.note_config_reloaded();
                    cx.notify();
                });
            });
            vcx.run_until_parked();
        };
        bump(&mut vcx);
        assert_eq!(
            bridge.pricer.view_names(),
            vec!["slim"],
            "fixture: the first reload hands over the configured views"
        );
        vcx.update(|_, cx| {
            bridge
                .pricer
                .reload(Views::builtin(), None, Duration::from_secs(1), cx)
        });
        bump(&mut vcx);
        assert_eq!(
            bridge.pricer.view_names(),
            vec!["vanilla", "barrier"],
            "an unchanged pricer config reloads nothing"
        );
        assert_eq!(bridge.pricer.settings().refresh, None);
    }

    /// Seeded with the startup key (what `start` carries), the observer
    /// skips even the FIRST reload when nothing the pricer reads changed —
    /// the first theme edit of a session must not restart every tile's
    /// timer either. The factory here holds the bundled views while the
    /// config says `slim`, so any reload at all would be visible.
    #[gpui::test]
    fn a_seeded_key_skips_the_first_reload_that_changes_no_pricer_setting(
        cx: &mut gpui::TestAppContext,
    ) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("pricer_views", "[slim]\ncolumns = [\"qty\", \"price\"]\n")
                    .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let (handle, _rx) = DataHandle::for_tests();
        let mut bridge = test_bridge(handle);
        bridge.pricer_key = Some(vcx.update(|_, cx| pricer_config_key(shell.read(cx).config())));
        cx.update(|cx| attach(&bridge, window, cx));
        vcx.update(|_, cx| {
            let frame = shell.read(cx).frame().clone();
            frame.update(cx, |f, cx| {
                f.note_config_reloaded();
                cx.notify();
            });
        });
        vcx.run_until_parked();
        assert_eq!(
            bridge.pricer.view_names(),
            vec!["vanilla", "barrier"],
            "the first unchanged reload reached nothing"
        );
    }

    /// A shell holding one restored pricer tile, its roster, actions and
    /// keymap fragment wired exactly as `main` wires them — so a typed
    /// key travels the shell's real matcher and insert-focus predicate.
    fn test_shell_services_with_a_pricer_tile() -> ShellServices {
        let mut services = test_shell_services();
        let (handle, _rx) = DataHandle::for_tests();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(PricerFactory::new(
            handle,
            Rc::new(MemorySheetStore::default()),
            Views::builtin(),
            PricerSettings::default(),
        )));
        roster.register_actions(&mut services.registry);
        let (fragments, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        let layered = geode_shell::keymap::fragments::splice(
            &[LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()],
            &fragments,
        );
        let (keymap, diags) = build_keymap(&layered, services.mod_alias, &services.registry);
        assert!(diags.is_empty(), "{diags:?}");
        services.keymap = keymap;
        services.roster = roster;
        let mut table = geode_shell::session::to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &geode_shell::palette_usage::PaletteUsage::new(),
        );
        let ws1: toml::Table = r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [tiles.1]
            module = "pricer"
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let restored = geode_shell::session::from_toml(&table).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        services.workspaces = restored.workspaces;
        services.restored_tiles = restored.tiles;
        services
    }

    /// The pricer's entry field must count as insert focus for the shell:
    /// a shifted letter typed after `o` is text, never a shell binding
    /// (`shift+d` is `workspace::duplicate_horizontal`). The shell treats
    /// keys as typing only while the focused tile's context reads
    /// `mode == insert` AND the tile holds focus
    /// (`ShellView::occupant_insert_stack`).
    #[gpui::test]
    fn typing_into_the_pricer_entry_field_fires_no_shell_binding(cx: &mut gpui::TestAppContext) {
        use geode_shell::diagnostics::fnv1a;
        let services = test_shell_services_with_a_pricer_tile();
        let tail = services.action_tail.clone();
        cx.update(geode_pricer::init);
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let dispatched = |id: &str| {
            let h = fnv1a(id);
            tail.lock().unwrap().recent().any(|x| x == h)
        };
        vcx.simulate_keystrokes("o");
        assert!(
            dispatched("pricer::add_below"),
            "fixture: `o` reached the pricer"
        );
        vcx.simulate_keystrokes("shift-d");
        vcx.simulate_input("ec26");
        assert!(
            !dispatched("workspace::duplicate_horizontal"),
            "a capital typed into the entry field ran a shell binding"
        );
    }

    /// Resolve pricing.adapter through the supplied registry. Unknown names must
    /// warn with the requested and available pricers without preventing setup.
    /// Use both required documents so this reaches registry resolution.
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
        // No dataset is local in this fixture, so the publication exclusion set is empty.
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
        assert_eq!(
            d.message,
            "pricer \"vendor\" ([pricing] adapter) is not built into this binary \
             (have: mock); every priced line will say so"
        );
    }

    /// Verify setup carries every local dataset name and excludes non-local ones.
    /// The routing test supplies its set directly, so it cannot prove this extraction.
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

    /// Local publication must update diagnostics while leaving frame data revisions
    /// unchanged; a normal publication must still advance them.
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
        let (tx, rx) = crate::events::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Rc::new(["pricer_sheets".to_string()].into_iter().collect()),
            pricer_key: None,
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
        // Observe note_published through the dataset entry it creates.
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

    /// A pricing outcome must reach the recording occupant, not merely survive
    /// the drain loop. Inspect its delivery log through the real bridge and shell.
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
        let (tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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

    /// Send a Health event after closing the window and verify the drain exits.
    /// The dropped-counter Arc is captured only by the drain within attach, so
    /// releasing that clone proves exit without adding production test flags.
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
        let (tx, rx) = crate::events::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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

        // A health event must trigger the same closed-window check as query results.
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

    /// [`test_shell_services`] with the shell's own recording module in
    /// the roster and one tile of that kind restored into workspace 1.
    /// `ModuleRoster::default()` holds no factory at all, so the plain
    /// fixture's shell has nothing a broadcast could reach; this one has
    /// exactly one live, visible occupant, and hands back the log it
    /// writes every delivery into.
    fn test_shell_services_with_a_recording_tile() -> (
        ShellServices,
        Rc<std::cell::RefCell<Vec<geode_shell::module::recording::Recorded>>>,
    ) {
        let mut services = test_shell_services();
        let factory = geode_shell::module::recording::RecordingFactory::new("rec");
        let log = factory.log.clone();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(factory));
        roster.register_actions(&mut services.registry);
        services.roster = roster;

        // The one public door onto a workspace holding a tile — the same
        // route `geode-shell`'s own occupant tests restore through.
        let mut table = geode_shell::session::to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &geode_shell::palette_usage::PaletteUsage::new(),
        );
        let ws1: toml::Table = r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [tiles.1]
            module = "rec"
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let restored = geode_shell::session::from_toml(&table).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        services.workspaces = restored.workspaces;
        services.restored_tiles = restored.tiles;
        (services, log)
    }

    /// Fetch outcomes broadcast identity/source to visible occupants, for both
    /// success and error. They are not addressed by the requesting tile's key.
    #[gpui::test]
    fn a_series_fetched_event_is_broadcast_to_the_shell(cx: &mut gpui::TestAppContext) {
        let (services, log) = test_shell_services_with_a_recording_tile();
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
        let (tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
        };
        cx.update(|cx| attach(&bridge, window, cx));

        tx.try_send(DataEvent::SeriesFetched {
            source: "demo_kdb".into(),
            identity: "VIX".into(),
            result: Err("no such symbol".into()),
        })
        .unwrap();
        vcx.run_until_parked();

        assert!(
            log.borrow().iter().any(|r| matches!(
                r,
                geode_shell::module::recording::Recorded::SeriesFetched(_, pair)
                    if pair == "VIX@demo_kdb"
            )),
            "{:?}",
            log.borrow()
        );
    }

    /// A series result must reach only the occupant named by its request key.
    #[gpui::test]
    fn a_series_outcome_is_routed_to_its_tile(cx: &mut gpui::TestAppContext) {
        let (services, log) = test_shell_services_with_a_recording_tile();
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
        let (tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
        };
        cx.update(|cx| attach(&bridge, window, cx));

        // The restored workspace's one tile is id 1, so that is the key
        // this outcome is addressed to.
        tx.try_send(DataEvent::Series(geode_core::series::SeriesOutcome {
            key: geode_core::query::QueryKey(1),
            tag: 11,
            submitted: std::time::Instant::now(),
            result: Ok(geode_core::series::SeriesResult::default()),
        }))
        .unwrap();
        vcx.run_until_parked();

        assert!(
            log.borrow().iter().any(|r| matches!(
                r,
                geode_shell::module::recording::Recorded::Delivered(tile, tag)
                    if tile.0 == 1 && *tag == 11
            )),
            "{:?}",
            log.borrow()
        );
    }

    /// The drain loop routes a `DataEvent::Upload` into
    /// `Delivery::Upload`, addressed to the submitting tile's key exactly
    /// as a `Query`/`Series` outcome is — an arm that only logged would
    /// leave the panel waiting for an outcome forever.
    #[gpui::test]
    fn an_upload_outcome_is_routed_to_its_tile(cx: &mut gpui::TestAppContext) {
        let (services, log) = test_shell_services_with_a_recording_tile();
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
        let (tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        // The restored workspace's one tile is id 1, so that is the key
        // this outcome is addressed to.
        tx.try_send(DataEvent::Upload(geode_data::egress::UploadOutcome {
            key: geode_core::query::QueryKey(1),
            tag: 7,
            target: "sophis".into(),
            result: Ok(()),
        }))
        .unwrap();
        vcx.run_until_parked();

        assert!(
            log.borrow().iter().any(|r| matches!(
                r,
                geode_shell::module::recording::Recorded::Delivered(tile, tag)
                    if tile.0 == 1 && *tag == 7
            )),
            "{:?}",
            log.borrow()
        );
    }

    /// Route Loading and LoadEnded through the bridge into diagnostics progress.
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
        let (tx, rx) = crate::events::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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

    /// After handle shutdown, distinct submission deterministically refuses.
    /// Verify the bridge delivers an error so the picker cannot remain loading.
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
        let (_tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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

    /// Reload presentation diagnostics must reach the retained diagnostics model,
    /// including entries that refer to views no longer present.
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
        let (_tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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

    /// Reload named colours into the factory used by new and existing blotter tiles.
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
        let (_tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory: factory.clone(),
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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

    /// The real observer and drain accept only the active request's tag.
    /// A foreign response must neither update the catalog nor free its slot.
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
        let (tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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
        // A second publish queues a follow-up without replacing the active tag.
        diagnostics.update(&mut vcx, |d, cx| {
            d.note_published("risk");
            cx.notify();
        });
        vcx.run_until_parked();

        // An unsolicited tag must not release the active request.
        tx.try_send(DataEvent::Catalog(CatalogOutcome {
            key: DIAGNOSTICS_KEY,
            tag: 99,
            snapshot: Ok(CatalogSnapshot::default()),
        }))
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.clone()),
            None,
            "a stale (superseded) tag must not be applied"
        );

        // The active tag-1 outcome must be applied, then the follow-up may run.
        let fresh = CatalogSnapshot {
            threads: 4,
            ..CatalogSnapshot::default()
        };
        tx.try_send(DataEvent::Catalog(CatalogOutcome {
            key: DIAGNOSTICS_KEY,
            tag: 1,
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

    /// watch queues demand without notifying by itself. Through the real attach
    /// observer, watch plus notify must produce a Catalog request on the handle.
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
        let (_tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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

    /// Open a real diagnostics tile through its factory and bridge. After the
    /// initial catalog request completes, changing frame as-of must produce a
    /// second request carrying the new value. A pending-bit assertion alone would
    /// not prove that the observer submits the request.
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
        let (tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
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
        for offset in (0..8).rev() {
            frame.update(&mut vcx, |f, cx| {
                f.set_as_of(AsOf::At(at - chrono::Duration::days(offset)));
                cx.notify();
            });
            vcx.run_until_parked();
        }

        assert!(request_rx.try_recv().is_err(), "wait for the first read");
        tx.try_send(DataEvent::Catalog(CatalogOutcome {
            key: DIAGNOSTICS_KEY,
            tag: 1,
            snapshot: Ok(CatalogSnapshot::default()),
        }))
        .unwrap();
        vcx.run_until_parked();
        assert!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.is_none()),
            "an old as-of must not be installed"
        );

        match request_rx.try_recv() {
            Ok(geode_data::Request::Catalog(params)) => {
                assert_eq!(
                    params.as_of,
                    AsOf::At(at),
                    "the second catalog request must carry the new as-of"
                );
                tx.try_send(DataEvent::Catalog(CatalogOutcome {
                    key: params.key,
                    tag: params.tag,
                    snapshot: Ok(CatalogSnapshot {
                        as_of: params.as_of,
                        threads: 8,
                        ..Default::default()
                    }),
                }))
                .unwrap();
            }
            other => {
                panic!("expected a second Request::Catalog carrying the new as-of, got {other:?}")
            }
        }
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().as_of.clone()),
            AsOf::At(at)
        );
        assert!(request_rx.try_recv().is_err());

        // With no read outstanding, only the tile's frame observer can ask.
        // Recovery from a stale in-flight result must not mask that contract.
        let later = at + chrono::Duration::days(1);
        frame.update(&mut vcx, |f, cx| {
            f.set_as_of(AsOf::At(later));
            cx.notify();
        });
        vcx.run_until_parked();
        match request_rx.try_recv() {
            Ok(geode_data::Request::Catalog(params)) => assert_eq!(params.as_of, AsOf::At(later)),
            other => panic!("expected a refresh from an idle as-of change, got {other:?}"),
        }
    }

    /// Describing a source before its first health report must not create a
    /// pending warning. Source metadata alone does not establish unhealthy state.
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
        let (_tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: vec![(
                SourceSpec {
                    pending_timeout: Duration::from_secs(120),
                    ..SourceSpec::directory("risk", "risk", vec!["/data/risk/*.csv".into()])
                },
                SourceShape::Directory,
            )],
            local_datasets: Default::default(),
            pricer_key: None,
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

    /// Setup must give the service and factory presentation-aware views. Direct
    /// view parsing would still return valid definitions while dropping overrides.
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

    /// Dataset presentation supplies defaults beneath view presentation; an
    /// explicit view override wins where both set the same property.
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

    /// Setup carries named colours and their reader diagnostics. load_views checks
    /// colour references but does not retain colour-definition errors.
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

    /// Build the timeseries factory with the shared handle and startup colours.
    /// Its kind must match the roster/session identity used to restore the module.
    #[gpui::test]
    fn start_builds_a_timeseries_factory_beside_the_blotters(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: crate::demo::layer(&dir.path().join("src")),
            ..ConfigSources::default()
        });
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(std::sync::Arc::new(geode_pricing::MockPricer::new()));
        let setup = data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            AdapterRegistry::default(),
            pricers,
        )
        .expect("the demo layer declares datasets and views");
        let bridge =
            cx.update(|cx| start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        assert_eq!(bridge.timeseries.kind(), "timeseries");
        // The neighbours, so a future edit that swaps one factory for
        // another is caught here rather than at a painted tile.
        assert_eq!(bridge.factory.kind(), "blotter");
        assert_eq!(bridge.marketdata.kind(), "cvi");
        assert_eq!(bridge.dividend.kind(), "dividend");
        assert_eq!(
            bridge.pricer_key,
            Some(pricer_config_key(&config)),
            "the reload observer is seeded with the key the factory was built from"
        );
    }

    #[test]
    fn a_closed_receiver_is_counted_as_dropped_rather_than_lost_silently() {
        let (tx, rx) = crate::events::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = make_sink(tx, dropped.clone());
        assert!(sink(DataEvent::Diagnostics(Vec::new())), "the first fits");
        drop(rx);
        assert!(
            !sink(DataEvent::Diagnostics(Vec::new())),
            "the receiver is now closed"
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }
    struct CatalogFixture {
        window: WindowHandle<Root>,
        bridge: Bridge,
        requests: std::sync::mpsc::Receiver<geode_data::Request>,
        events: crate::events::Sender,
    }

    fn catalog_fixture(cx: &mut gpui::TestAppContext) -> CatalogFixture {
        let window = open_test_window(cx, test_shell_services());
        let (handle, requests) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = crate::events::channel();
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(
                handle.clone(),
                &CVI,
                Duration::from_secs(900),
            )),
            dividend: Rc::new(
                MarketDataFactory::new(handle.clone(), &DIVIDEND, Duration::from_secs(900))
                    .without_keymap(),
            ),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
        };
        cx.update(|cx| attach(&bridge, window, cx));

        CatalogFixture {
            window,
            bridge,
            requests,
            events: tx,
        }
    }

    fn next_catalog(f: &CatalogFixture) -> CatalogParams {
        match f.requests.try_recv().expect("catalog request") {
            geode_data::Request::Catalog(params) => params,
            other => panic!("expected catalog, got {other:?}"),
        }
    }

    fn answer_catalog(f: &CatalogFixture, params: &CatalogParams, threads: u64) {
        f.events
            .try_send(DataEvent::Catalog(CatalogOutcome {
                key: params.key,
                tag: params.tag,
                snapshot: Ok(CatalogSnapshot {
                    as_of: params.as_of.clone(),
                    threads,
                    ..Default::default()
                }),
            }))
            .unwrap();
    }

    #[gpui::test]
    fn catalog_bursts_keep_one_read_and_one_follow_up(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        let first = next_catalog(&f);
        for i in 0..128 {
            f.events
                .try_send(DataEvent::Published {
                    dataset: "risk".into(),
                    batch: i.to_string(),
                    gen_id: i,
                    books: vec![None],
                })
                .unwrap();
            vcx.run_until_parked();
        }
        assert!(
            f.requests.try_recv().is_err(),
            "a burst must not queue more reads"
        );
        // Ordinary requests have no catalog backlog to wait behind.
        assert!(f.bridge.handle.cancel(QueryKey(42)));
        assert!(matches!(
            f.requests.try_recv(),
            Ok(geode_data::Request::Cancel { key: QueryKey(42) })
        ));
        answer_catalog(&f, &first, 4);
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().threads),
            4
        );
        let second = next_catalog(&f);
        assert!(f.requests.try_recv().is_err());
        // A duplicate/foreign completion cannot clear the active slot.
        answer_catalog(&f, &first, 99);
        vcx.run_until_parked();
        f.events
            .try_send(DataEvent::Catalog(CatalogOutcome {
                key: QueryKey(123),
                tag: second.tag,
                snapshot: Ok(CatalogSnapshot {
                    threads: 99,
                    ..Default::default()
                }),
            }))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().threads),
            4
        );
        diagnostics.update(&mut vcx, |d, cx| {
            d.note_published("risk");
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(f.requests.try_recv().is_err());
        // Identical results must still release the queued follow-up.
        answer_catalog(&f, &second, 4);
        vcx.run_until_parked();
        let third = next_catalog(&f);
        answer_catalog(&f, &third, 8);
        vcx.run_until_parked();
        assert!(f.requests.try_recv().is_err(), "no demand, no polling");
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().threads),
            8
        );
    }

    #[gpui::test]
    fn catalog_visibility_keeps_the_in_flight_bound(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        let first = next_catalog(&f);
        diagnostics.update(&mut vcx, |d, cx| {
            d.note_published("risk");
            d.unwatch();
            cx.notify();
        });
        answer_catalog(&f, &first, 4);
        vcx.run_until_parked();
        assert!(
            f.requests.try_recv().is_err(),
            "hidden diagnostics needs no follow-up"
        );
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().threads),
            4
        );
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        let second = next_catalog(&f);
        diagnostics.update(&mut vcx, |d, cx| {
            d.unwatch();
            d.watch();
            d.watch();
            d.unwatch();
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(
            f.requests.try_recv().is_err(),
            "showing again must not duplicate an in-flight read"
        );
        answer_catalog(&f, &second, 8);
        vcx.run_until_parked();
        let third = next_catalog(&f);
        answer_catalog(&f, &third, 16);
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().threads),
            16
        );
        assert!(f.requests.try_recv().is_err());
    }

    #[gpui::test]
    fn catalog_refusal_and_failure_retry_without_new_events(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        while f.bridge.handle.cancel(QueryKey(42)) {}
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        // Free the queue, but keep the delay: unrelated notifies cannot hot-loop retries.
        while f.requests.try_recv().is_ok() {}
        for _ in 0..32 {
            diagnostics.update(&mut vcx, |d, cx| {
                d.note_published("risk");
                cx.notify();
            });
            vcx.run_until_parked();
        }
        assert!(f.requests.try_recv().is_err());
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY);
        vcx.run_until_parked();
        let first = next_catalog(&f);
        f.events
            .try_send(DataEvent::Catalog(CatalogOutcome {
                key: first.key,
                tag: first.tag,
                snapshot: Err("transient read error".into()),
            }))
            .unwrap();
        vcx.run_until_parked();
        assert!(f.requests.try_recv().is_err());
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY);
        vcx.run_until_parked();
        let second = next_catalog(&f);
        answer_catalog(&f, &second, 8);
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().threads),
            8
        );
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY);
        vcx.run_until_parked();
        assert!(
            f.requests.try_recv().is_err(),
            "successful completion stops retries"
        );
        // Hide while waiting for an error retry: its old timer must not recreate demand.
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_catalog_refresh();
            cx.notify();
        });
        vcx.run_until_parked();
        let third = next_catalog(&f);
        f.events
            .try_send(DataEvent::Catalog(CatalogOutcome {
                key: third.key,
                tag: third.tag,
                snapshot: Err("retry then hide".into()),
            }))
            .unwrap();
        vcx.run_until_parked();
        diagnostics.update(&mut vcx, |d, cx| {
            d.unwatch();
            cx.notify();
        });
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY);
        vcx.run_until_parked();
        assert!(f.requests.try_recv().is_err());
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        let fourth = next_catalog(&f);
        f.events
            .try_send(DataEvent::Catalog(CatalogOutcome {
                key: fourth.key,
                tag: fourth.tag,
                snapshot: Err("retry then close".into()),
            }))
            .unwrap();
        vcx.run_until_parked();
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY);
        vcx.run_until_parked();
        assert!(
            f.requests.try_recv().is_err(),
            "a retry must not submit after window closure"
        );
    }
    #[gpui::test]
    fn catalog_explicit_requests_survive_without_diagnostics_watchers(
        cx: &mut gpui::TestAppContext,
    ) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
        // This is the identity picker's request door: no diagnostics tile exists.
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_catalog();
            cx.notify();
        });
        vcx.run_until_parked();
        let first = next_catalog(&f);
        let at = chrono::Utc::now();
        frame.update(&mut vcx, |frame, cx| {
            frame.set_as_of(AsOf::At(at));
            cx.notify();
        });
        vcx.run_until_parked();
        answer_catalog(&f, &first, 4);
        vcx.run_until_parked();
        assert!(diagnostics.read_with(&vcx, |d, _| d.catalog.is_none()));
        let second = next_catalog(&f);
        assert_eq!(second.as_of, AsOf::At(at));
        // Mixed demand must retain the explicit request when the last watcher hides.
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            d.request_catalog();
            d.unwatch();
            d.watch(); // both demand kinds are present when the follow-up is submitted
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(f.requests.try_recv().is_err());
        answer_catalog(&f, &second, 8);
        vcx.run_until_parked();
        let third = next_catalog(&f);
        diagnostics.update(&mut vcx, |d, cx| {
            d.unwatch();
            cx.notify();
        });
        vcx.run_until_parked();
        // The combined read must retain the explicit consumer's retry policy.
        f.events
            .try_send(DataEvent::Catalog(CatalogOutcome {
                key: third.key,
                tag: third.tag,
                snapshot: Err("picker retry".into()),
            }))
            .unwrap();
        vcx.run_until_parked();
        assert!(f.requests.try_recv().is_err());
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY);
        vcx.run_until_parked();
        let fourth = next_catalog(&f);
        answer_catalog(&f, &fourth, 16);
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.catalog.as_ref().unwrap().threads),
            16
        );
        diagnostics.update(&mut vcx, |d, cx| {
            d.note_published("risk");
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(
            f.requests.try_recv().is_err(),
            "an explicit read does not subscribe to publications"
        );
    }
}
