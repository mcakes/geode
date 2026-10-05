//! Application wiring between shell and data. Build service configuration and
//! module factories from shared inputs, route coalesced mailbox events through
//! the window, and forward view reloads to the service. Shell and data remain
//! independent crates. See `docs/current/request-delivery.md`.

use geode_blotter::BlotterFactory;
use geode_core::colour::NamedColours;
use geode_core::config::{Config, DIMENSIONS_DOC, Diagnostic, Layer, Severity, load_views};
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::DocumentKind;
use geode_core::egress_config;
use geode_core::panel::{KindActionRegistry, PANELS_DOC, PanelSpec, load_panels, refusal};
use geode_core::query::{AsOf, CatalogParams, DistinctOutcome, ReferenceOutcome, ReferenceParams};
use geode_core::schema::{ColumnType, SchemaSpec};
use geode_core::source_config::{SourceShape, parse_duration};
use geode_core::view::ViewSpec;
use geode_data::adapter::AdapterRegistry;
use geode_data::source::SourceSpec;
use geode_data::{
    DEFAULT_STORE_DEADLINE, DataEvent, DataHandle, DataService, DataServiceConfig, EventSink,
    PricerConfig, PricerRegistry, Refusal, StoreRole, VolConfig, VolModelRegistry,
};
use geode_marketdata::MarketDataFactory;
use geode_pricer::content::{PayoutSource, PricerFactory, PricerSettings, UnderlyingList};
use geode_pricer::core::{
    PRICER_SHEETS_DATASET, PRICER_TEMPLATES_DOC, PRICER_VIEWS_DOC, TemplateSet, Views,
};
use geode_pricer::store::DuckSheetStore;
use geode_shell::diagnostics::{CatalogRequest, Diagnostics, ReferenceLane, SourceSummary};
use geode_shell::module::placeholder::PLACEHOLDER_KIND;
use geode_shell::module::{Delivery, UploadDelivery};
use geode_shell::reference::ReferenceGlobal;
use geode_shell::shell::objectdialog::shadow_of;
use geode_shell::shell::{DIAGNOSTICS_KEY, REFERENCE_KEY, ShellEvent, ShellView, is_shell_key};
use geode_shell::vimfind::FindStyle;
use gpui::{App, AsyncApp, Entity, WindowHandle};
use gpui_component::Root;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

pub struct DataSetup {
    pub config: DataServiceConfig,
    pub views: Vec<ViewSpec>,
    /// Startup dimensions shared with the service and blotter's filter validation.
    pub dimensions: DerivedDimensions,
    /// Named colors shared by module factories. Parse separately from load_views
    /// to retain color-definition diagnostics as well as unknown-reference warnings.
    pub colours: NamedColours,
    pub diagnostics: Vec<Diagnostic>,
    /// Datasets declared local. Their publications update diagnostics but skip
    /// frame publication history and revisions, preventing autosave invalidation.
    pub local_datasets: HashSet<String>,
    /// The pricer's views, template tables and settings; `stale_after` is
    /// filled by `start`.
    pub pricer_views: Views,
    pub pricer_templates: TemplateSet,
    pub pricer_settings: PricerSettings,
    /// `[pricing] underlyings` as written (`UnderlyingList::set`
    /// normalises it).
    pub pricer_underlyings: Vec<String>,
    /// What the pricer read out of this config (`pricer_config_key`), so
    /// the reload observer can tell a reload that changed none of it.
    pub pricer_key: PricerConfigKey,
    /// The accepted market-data panels in `panels` document order, and an
    /// Error per refused panel for the shell's config section.
    pub panels: Vec<PanelSpec>,
    pub panel_diagnostics: Vec<Diagnostic>,
    /// Each classification's winning layer and, for the ones a user copy
    /// shadows, the layer of the copy beneath it, for the classifications
    /// factory's startup snapshot.
    pub classification_layers: BTreeMap<String, Layer>,
    pub classification_shadowed: BTreeMap<String, Layer>,
}

/// Tile and page kinds other modules own. A panel of one of these names
/// would put two factories behind one kind, and a saved blotter could
/// restore as a panel. `diagnostics` is a page kind; it also names the
/// page's keymap context and session table, so a panel may not take it.
pub(crate) const MODULE_KINDS: &[&str] = &[
    "blotter",
    "timeseries",
    "volslice",
    "classifications",
    "pricer",
    "diagnostics",
    "guide",
    PLACEHOLDER_KIND,
];

/// Every kind action this build registers. Adding a verb is code: its
/// handler lives in the module that dispatches it.
fn kind_actions() -> KindActionRegistry {
    geode_marketdata::core::builtin_kind_actions()
}

/// The accepted panels, in `panels` document order, and an Error per refused
/// one. `panels` is restart-required, so this runs once per launch.
fn load_panels_from_config(
    config: &Config,
    schema: &SchemaSpec,
    documents: &[Arc<dyn DocumentKind>],
) -> (Vec<PanelSpec>, Vec<Diagnostic>) {
    let Some(doc) = config.doc(PANELS_DOC) else {
        return (Vec::new(), Vec::new());
    };
    let (panels, mut diags) = load_panels(doc, &kind_actions(), schema, documents);
    let (panels, taken): (Vec<PanelSpec>, Vec<PanelSpec>) = panels
        .into_iter()
        .partition(|p| !MODULE_KINDS.contains(&p.kind.as_str()));
    diags.extend(taken.iter().map(|p| {
        refusal(
            doc,
            &p.kind,
            "",
            &format!("'{}' is another module's tile kind", p.kind),
        )
    }));
    (panels, diags)
}

/// Build setup when both datasets and views documents are present. Empty
/// parsed definitions still produce Some with any diagnostics; missing either
/// document returns None. Adapters, pricers and vol models come from the
/// caller's registries; the engine half (schema with the app datasets
/// pinned, dimensions, sources, document kinds, clock) comes from
/// `geode_compose::engine_setup`.
pub fn data_setup(
    config: &Config,
    db_path: PathBuf,
    adapters: AdapterRegistry,
    pricers: PricerRegistry,
    vol_models: VolModelRegistry,
) -> Option<DataSetup> {
    config.doc("datasets")?;
    // Require a views document, then use load_views for presentation overlays.
    // A direct parse would omit the trader's effective presentation settings.
    config.doc("views")?;
    let geode_compose::EngineSetup {
        config: mut service,
        document_kinds,
        mut diagnostics,
    } = geode_compose::engine_setup(config, db_path, adapters);
    let (views, d) = load_views(config);
    diagnostics.extend(d);
    // Resolve typed egress targets against the service's adapter registry.
    // An unknown adapter or one without egress support drops the target with
    // a diagnostic. The resolved list feeds both the service and each
    // market-data factory's document-specific target choices.
    let (egress_specs, d) = config
        .doc("egress")
        .map(|doc| egress_config::from_doc(doc, &service.schema))
        .unwrap_or_default();
    diagnostics.extend(d);
    let (egress, d) = geode_data::egress::resolve(egress_specs, &service.adapters);
    diagnostics.extend(d);
    // Resolve the one position service the same way: an unknown adapter, or
    // one without a position side, configures none, with a diagnostic.
    let (positions_spec, d) = config
        .doc("positions")
        .map(geode_core::positions::from_doc)
        .unwrap_or_default();
    diagnostics.extend(d);
    let (positions, d) = geode_data::positions::resolve(positions_spec, &service.adapters);
    diagnostics.extend(d);
    // Definitions and the value mapping checked against them, as one.
    let (colours, colour_diags) = NamedColours::from_config(config);
    diagnostics.extend(colour_diags);
    // Resolve the configured pricer, defaulting to mock. An unavailable name
    // warns and leaves the implementation absent; the pricing worker returns
    // an error for each line while the rest of the service can still open.
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
    // Resolve the configured vol model, defaulting to the demo stand-in. An
    // unavailable name warns and leaves the model absent; the vol worker
    // answers every job with that reason while the service still opens.
    let vol_name = config
        .get("app", "vol.model")
        .and_then(|v| v.as_str())
        .unwrap_or(geode_pricing::DEMO_VOL_MODEL)
        .to_string();
    let vol = match vol_models.get(&vol_name) {
        Some(m) => VolConfig::with(m),
        None => {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                layer: config.explain("app", "vol.model"),
                file: None,
                message: format!(
                    "vol model \"{vol_name}\" ([vol] model) is not built into this binary (have: {}); every vol slice will say so",
                    vol_models.names().join(", ")
                ),
                path: Some("app.vol.model".to_string()),
            });
            VolConfig::missing(&vol_name)
        }
    };
    let (pricer_views, view_diags) = pricer_views_from_specs(config, &views);
    diagnostics.extend(view_diags);
    let (pricer_templates, template_diags) =
        pricer_templates_from_config(config, &TemplateSet::builtin(), "built-in");
    diagnostics.extend(template_diags);
    let (refresh, refresh_diag) = pricing_refresh_from_config(config);
    diagnostics.extend(refresh_diag);
    let (pricer_underlyings, underlying_diags) = pricing_underlyings_from_config(config);
    let pricer_underlyings = pricer_underlyings.unwrap_or_default();
    diagnostics.extend(underlying_diags);
    let (payout, payout_diags) = pricing_payout_currency_from_config(config, &service.schema);
    diagnostics.extend(payout_diags);
    let pricer_settings = PricerSettings {
        pricer: pricer_name.clone(),
        pricer_missing: pricer.pricer.is_none(),
        refresh,
        stale_after: Duration::default(),
        payout,
    };
    // One set of document kinds: the panels are checked against exactly
    // the kinds the service registers.
    let (panels, panel_diagnostics) =
        load_panels_from_config(config, &service.schema, &document_kinds);
    let local_datasets: HashSet<String> = service
        .schema
        .datasets
        .iter()
        .filter(|d| d.local)
        .map(|d| d.name.clone())
        .collect();
    let dimensions = service.dimensions.clone();
    let (classification_layers, classification_shadowed) =
        classification_provenance(config, &dimensions);
    service.views = views.clone();
    service.pricer = pricer;
    service.vol = vol;
    service.egress = egress;
    service.positions = positions;
    Some(DataSetup {
        config: service,
        views,
        dimensions,
        colours,
        diagnostics,
        local_datasets,
        pricer_views,
        pricer_templates,
        pricer_settings,
        pricer_underlyings,
        pricer_key: pricer_config_key(config),
        panels,
        panel_diagnostics,
        classification_layers,
        classification_shadowed,
    })
}

/// Each classification's winning layer, and the classifications whose user
/// copy shadows a definition in a lower layer, with that layer: the ones a
/// revert would restore, and what it restores. Read from the layered `dimensions` documents, the same
/// provenance the object dialog badges.
fn classification_provenance(
    config: &Config,
    dims: &DerivedDimensions,
) -> (BTreeMap<String, Layer>, BTreeMap<String, Layer>) {
    let layers: BTreeMap<String, Layer> = dims
        .all()
        .filter_map(|d| {
            config
                .explain(DIMENSIONS_DOC, &d.name)
                .map(|l| (d.name.clone(), l))
        })
        .collect();
    let shadowed = layers
        .iter()
        .filter(|(_, layer)| **layer == Layer::User)
        .filter_map(|(name, _)| {
            shadow_of(config, DIMENSIONS_DOC, name).map(|(lower, _)| (name.clone(), lower))
        })
        .collect();
    (layers, shadowed)
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

/// Default periodic repricing interval when `app.pricing.refresh` is absent
/// or invalid.
pub const DEFAULT_PRICING_REFRESH: Duration = Duration::from_secs(30);

/// Read `app.pricing.refresh`: `"off"` disables periodic repricing; a nonzero
/// duration sets the interval. Invalid values, including zero, warn and use
/// the 30-second default. An absent value uses the default without a warning.
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

/// `[pricing] underlyings`: the names the pricer's entry bar suggests,
/// as written and in order (`UnderlyingList::set` upper-cases and drops
/// repeats). An absent setting clears the list. A non-array value returns `None`
/// with a warning, preserving the running list on reload; startup uses an empty
/// list. Non-string elements warn and are omitted from an otherwise valid array.
pub fn pricing_underlyings_from_config(config: &Config) -> (Option<Vec<String>>, Vec<Diagnostic>) {
    let Some(value) = config.get("app", "pricing.underlyings") else {
        return (Some(Vec::new()), Vec::new());
    };
    let warn = |message: String| Diagnostic {
        severity: Severity::Warning,
        layer: config.explain("app", "pricing.underlyings"),
        file: None,
        message,
        path: Some("app.pricing.underlyings".to_string()),
    };
    // A value that is not an array answers `None`: a reload keeps the list
    // it had (hot reload keeps the last valid state), startup has none.
    let Some(items) = value.as_array() else {
        return (
            None,
            vec![warn(format!(
                "[pricing] underlyings = {value} is not an array of names; ignored"
            ))],
        );
    };
    let mut names = Vec::with_capacity(items.len());
    let mut diags = Vec::new();
    for item in items {
        match item.as_str() {
            Some(name) => names.push(name.to_string()),
            None => diags.push(warn(format!(
                "[pricing] underlyings: {item} is not a name; skipped"
            ))),
        }
    }
    (Some(names), diags)
}

/// The `[pricing] payout_currency` a key-absent config falls back to.
const DEFAULT_PAYOUT_CURRENCY: (&str, &str) = ("underlyings", "currency");

/// `[pricing] payout_currency = "<dataset>.<column>"`: the reference column
/// a new line's payout currency defaults from. `schema` is the startup
/// schema: datasets are restart-required, so it is the one the service
/// serves on reload too. An absent key means `underlyings.currency` when
/// that column exists, and quietly nothing when it does not (a desk without
/// that dataset has not asked for it, nor when it is not a text column of
/// a single-key dataset). A value naming anything but a non-key text column
/// of a declared reference dataset keyed by one column is an error and
/// resolves to nothing rather than to a guessed column: new lines get no
/// currency and say so, where a wrong column would price in a plausible
/// wrong one.
pub fn pricing_payout_currency_from_config(
    config: &Config,
    schema: &SchemaSpec,
) -> (Option<PayoutSource>, Vec<Diagnostic>) {
    let resolve = |dataset: &str, column: &str| {
        schema
            .datasets
            .iter()
            .find(|d| d.name == dataset && d.is_reference())
            // A lookup joins a multi-column key with `/` and the pricer
            // looks up by underlying alone, so only a single-key dataset
            // ever matches; a column that is not text never parses as a
            // code. Either would leave every line blank without saying why.
            .filter(|d| d.key.len() == 1 && d.key[0] != column)
            .filter(|d| d.column(column).is_some_and(|c| c.ty == ColumnType::Utf8))
            .map(|_| PayoutSource {
                dataset: dataset.to_string(),
                column: column.to_string(),
            })
    };
    let Some(value) = config.get("app", "pricing.payout_currency") else {
        let (dataset, column) = DEFAULT_PAYOUT_CURRENCY;
        return (resolve(dataset, column), Vec::new());
    };
    let resolved = value
        .as_str()
        .and_then(|v| v.split_once('.'))
        .filter(|(_, column)| !column.contains('.'))
        .and_then(|(dataset, column)| resolve(dataset, column));
    if resolved.is_some() {
        return (resolved, Vec::new());
    }
    let shown = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string);
    (
        None,
        vec![Diagnostic {
            severity: Severity::Error,
            layer: config.explain("app", "pricing.payout_currency"),
            file: None,
            message: format!(
                "[pricing] payout_currency = \"{shown}\" must name a text column of a single-key reference dataset, e.g. \"underlyings.currency\"; new lines get no currency"
            ),
            path: Some("app.pricing.payout_currency".to_string()),
        }],
    )
}

/// The pricer's views out of `specs`, the `load_views` result for `config`
/// (the caller has already collected `load_views`'s own diagnostics, so
/// only the pricer's are returned here). A `pricer_views` document is no
/// longer read: its presence is an error naming where the views now live,
/// so a desk or user layer still carrying one is told rather than silently
/// ignored.
pub fn pricer_views_from_specs(config: &Config, specs: &[ViewSpec]) -> (Views, Vec<Diagnostic>) {
    let (views, mut diags) = Views::from_specs(specs);
    if config.doc(PRICER_VIEWS_DOC).is_some() {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: "pricer_views is no longer read; declare views over dataset \"pricer\" in views.toml".into(),
            path: Some(PRICER_VIEWS_DOC.into()),
        });
    }
    (views, diags)
}

/// The `pricer_templates` doc, or the built-in set when no layer has one
/// (the builtin layer always does in the app; a test config may not).
/// A bad entry keeps `previous`'s definition of its name (keep-last-valid,
/// per name): the builtin set at startup, the running set on a reload.
/// `previous_is` names that set in the warning: "built-in" or "previous".
pub fn pricer_templates_from_config(
    config: &Config,
    previous: &TemplateSet,
    previous_is: &str,
) -> (TemplateSet, Vec<Diagnostic>) {
    match config.doc(PRICER_TEMPLATES_DOC) {
        Some(doc) => TemplateSet::from_doc_over(doc, previous, previous_is),
        None => (TemplateSet::builtin(), Vec::new()),
    }
}

/// Inputs to the pricer's live reload: the merged `views` doc with both
/// presentation overlays (the pricer's column plan is built from all three),
/// the colors they may name (a named column's cells and header are painted
/// from them, so a `colors.toml` edit alone must reach open tiles), the
/// `value_colors` mapping those colors carry (likewise), the retired
/// `pricer_views` doc (so one added at runtime raises its retirement
/// diagnostic without a restart), merged `pricer_templates`, the
/// `dimensions` doc (a frame scope over the pricer may name a derived
/// dimension, so an edit to it must re-apply open tiles' scopes), raw
/// `app.pricing.refresh`, `app.pricing.underlyings` and
/// `app.pricing.payout_currency`, and the resolved
/// stale threshold. Equal keys leave factory views, templates, suggestions,
/// and timers alone and avoid repeating invalid-value warnings. The selected
/// pricing adapter is fixed at service startup and excluded here.
#[derive(Debug, Clone, PartialEq)]
pub struct PricerConfigKey {
    views: Option<toml::Table>,
    view_presentation: Option<toml::Table>,
    dataset_presentation: Option<toml::Table>,
    colors: Option<toml::Table>,
    /// The value mapping the pricer's colors carry.
    value_colors: Option<toml::Table>,
    pricer_views: Option<toml::Table>,
    templates: Option<toml::Table>,
    /// The derived dimensions a frame scope over the pricer may name.
    dimensions: Option<toml::Table>,
    refresh: Option<toml::Value>,
    underlyings: Option<toml::Value>,
    payout_currency: Option<toml::Value>,
    stale_after: Duration,
}

pub fn pricer_config_key(config: &Config) -> PricerConfigKey {
    PricerConfigKey {
        views: config.doc("views").map(|d| d.value.clone()),
        view_presentation: config.doc("view_presentation").map(|d| d.value.clone()),
        dataset_presentation: config.doc("dataset_presentation").map(|d| d.value.clone()),
        colors: config
            .doc(geode_core::config::COLORS_DOC)
            .map(|d| d.value.clone()),
        value_colors: config
            .doc(geode_core::config::VALUE_COLORS_DOC)
            .map(|d| d.value.clone()),
        pricer_views: config.doc(PRICER_VIEWS_DOC).map(|d| d.value.clone()),
        templates: config.doc(PRICER_TEMPLATES_DOC).map(|d| d.value.clone()),
        dimensions: config.doc(DIMENSIONS_DOC).map(|d| d.value.clone()),
        refresh: config.get("app", "pricing.refresh").cloned(),
        underlyings: config.get("app", "pricing.underlyings").cloned(),
        payout_currency: config.get("app", "pricing.payout_currency").cloned(),
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
    /// One factory per accepted panel, in `panels` document order. Retained
    /// for reload updates to the shared stale threshold.
    pub panels: Vec<Rc<MarketDataFactory>>,
    /// Timeseries factory sharing the data handle and named colors. Retained
    /// so reload can update the chart palette.
    pub timeseries: Rc<geode_timeseries::content::TimeseriesFactory>,
    /// The vol slice viewer's factory, sharing the data handle. Its expiry
    /// colors come from the theme, so no reload reaches it.
    pub volslice: Rc<geode_volslice::VolsliceFactory>,
    /// The classifications factory, sharing the handle. Retained so every
    /// reload pushes it the dimensions, schema, views and layers it reads.
    pub classifications: Rc<geode_classifications::ClassificationsFactory>,
    /// The line pricer's factory, sharing the handle. Retained so a reload
    /// reaches its views and settings.
    pub pricer: Rc<PricerFactory>,
    /// The list behind the pricer's entry-bar underlyings; the reload
    /// observer sets it from `[pricing] underlyings`.
    pub underlyings: Rc<UnderlyingList>,
    events: crate::events::Receiver,
    dropped: Arc<AtomicU64>,
    /// Startup source descriptions paired with their schema-derived pipeline.
    /// Diagnostics describe the running service even when edited source config
    /// awaits restart.
    sources: Vec<(SourceSpec, SourceShape)>,
    /// Reference-family dataset names in startup-schema declaration order,
    /// each with its key's column count, handed to diagnostics and the live
    /// reference cache at attach. Fixed for the run like `sources`: the
    /// service serves the schema it started with.
    reference_datasets: Vec<(String, usize)>,
    /// The startup schema, fixed for the run like `reference_datasets`:
    /// the pricer reload resolves `[pricing] payout_currency` against the
    /// reference datasets the service actually serves, not an edited
    /// `datasets` doc awaiting restart.
    schema: Rc<SchemaSpec>,
    /// Local dataset names used to exclude autosave from frame publication updates.
    pub local_datasets: Rc<HashSet<String>>,
    /// The config key the pricer factory was built from; seeds the reload
    /// observer, so a reload that changes nothing the pricer reads is
    /// skipped from the first one. `None` when the factory's config is
    /// unknown: the first reload then always applies.
    pub pricer_key: Option<PricerConfigKey>,
    /// Whether the service started with a position service: `positions.toml`
    /// named one and its adapter resolved. Fixed for the run, since
    /// `positions.toml` is restart-required. Gates the Move LHU row action.
    pub positions_configured: bool,
}

/// One factory per accepted panel. Only the first ships the shared
/// `marketdata` keymap fragment: every panel declares the same context and
/// bindings, and a second copy would splice a duplicate layer. Each factory
/// narrows the shared egress targets to its own document.
fn panel_factories(
    handle: &DataHandle,
    panels: Vec<PanelSpec>,
    stale_after: Duration,
    egress: &Arc<Vec<(String, Vec<String>)>>,
) -> Vec<Rc<MarketDataFactory>> {
    panels
        .into_iter()
        .enumerate()
        .map(|(i, spec)| {
            let ships_keymap = i == 0;
            let factory = MarketDataFactory::new(handle.clone(), Arc::new(spec), stale_after)
                .with_egress(egress.clone());
            Rc::new(if ships_keymap {
                factory
            } else {
                factory.without_keymap()
            })
        })
        .collect()
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
    cx: &mut App,
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
    let pricer_dims = setup.dimensions.clone();
    let sources = source_shapes(&setup.config.sources, &schema);
    let reference_datasets = schema
        .datasets
        .iter()
        .filter(|ds| ds.is_reference())
        .map(|ds| (ds.name.clone(), ds.key.len()))
        .collect();
    let startup_schema = Rc::new(schema.clone());
    let local_datasets = Rc::new(setup.local_datasets);
    let panels = setup.panels;
    // Target names and accepted documents in `egress.toml` order. Every panel
    // factory shares the resolved list; each narrows it to its document kind
    // when creating a tile.
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
    let positions_configured = setup.config.positions.is_some();
    // Through the lease: while the background collector holds the store the
    // open waits up to `DEFAULT_STORE_DEADLINE` for its handoff (the
    // `store-waiting` status segment), and a second window on the same store
    // is refused instead of sharing the writer.
    let handle = DataService::spawn_as(
        setup.config,
        sink,
        StoreRole::App {
            deadline: DEFAULT_STORE_DEADLINE,
        },
    );
    // Both factories receive the same startup colors and later reload updates.
    let timeseries = Rc::new(geode_timeseries::content::TimeseriesFactory::new(
        handle.clone(),
        setup.colours.clone(),
    ));
    let volslice = Rc::new(geode_volslice::VolsliceFactory::new(handle.clone()));
    // Pushed its startup snapshot now, before any tile is restored; every
    // reload pushes the next one.
    let classifications = Rc::new(geode_classifications::ClassificationsFactory::new(
        handle.clone(),
    ));
    classifications.set_config(
        geode_classifications::ClassificationsConfig {
            dims: setup.dimensions.clone(),
            schema: startup_schema.clone(),
            views: setup.views.clone(),
            layers: setup.classification_layers,
            shadowed: setup.classification_shadowed,
        },
        cx,
    );
    let factory = Rc::new(BlotterFactory::new(
        handle.clone(),
        setup.views,
        setup.colours.clone(),
        schema,
        dimensions,
        find_style,
        stale_after,
    ));
    let mut pricer_settings = setup.pricer_settings.clone();
    let pricer_key = setup.pricer_key.clone();
    pricer_settings.stale_after = stale_after;
    let underlyings = Rc::new(UnderlyingList::default());
    underlyings.set(&setup.pricer_underlyings);
    // Sheets live in the local `pricer_sheets` dataset the builtin layer
    // declares. The store only queues reads and writes; their answers come
    // back through the drain (a load's as the tile's `Delivery::Query`, a
    // save's or forget's to the factory by sheet name).
    let pricer = Rc::new(
        PricerFactory::new(
            handle.clone(),
            Rc::new(DuckSheetStore::new(handle.clone())),
            setup.pricer_views.clone(),
            setup.pricer_templates.clone(),
            pricer_settings,
        )
        .with_underlyings(underlyings.clone())
        // The same startup colors as the blotter: a named `color` on a
        // pricer view column resolves against them.
        .with_colours(setup.colours)
        // The derived dimensions a frame scope may name over `pricer`
        // (`region` over `underlying_ref`), as the blotter's.
        .with_dims(pricer_dims),
    );
    // Panels and blotters use the same configured stale threshold.
    let panels = panel_factories(&handle, panels, stale_after, &egress_targets);
    Bridge {
        panels,
        timeseries,
        volslice,
        classifications,
        pricer,
        underlyings,
        handle,
        factory,
        events: rx,
        dropped,
        sources,
        reference_datasets,
        schema: startup_schema,
        local_datasets,
        pricer_key: Some(pricer_key),
        positions_configured,
    }
}

/// The sheet a local-write outcome names, when it is one of the pricer's:
/// `pricer_sheets` is keyed by the sheet name alone, so the outcome's batch
/// (the joined document key) is that name.
fn pricer_sheet<'a>(dataset: &str, batch: &'a str) -> Option<&'a str> {
    (dataset == PRICER_SHEETS_DATASET).then_some(batch)
}

/// Flush unsaved pricer sheets before requesting data-service shutdown.
/// Accepted saves enter the queue ahead of `Shutdown`, and the ingest runner
/// processes queued local writes before stopping. Submission or write failures
/// can still leave a sheet unsaved.
///
/// Joining the service may wait for an in-flight load, so it runs off the UI
/// thread. GPUI waits only until its quit timeout; completion is not guaranteed
/// before process exit.
pub fn stop_at_quit(bridge: &Bridge, cx: &mut App) {
    let handle = bridge.handle.clone();
    let pricer = Rc::clone(&bridge.pricer);
    cx.on_app_quit(move |cx| {
        pricer.flush_all(cx);
        let handle = handle.clone();
        cx.background_executor().spawn(async move {
            handle.shutdown();
        })
    })
    .detach();
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

/// The diagnostics page's reference reads. Only the latest submission's
/// answer is stored, so a slow answer for an older as-of cannot replace a
/// newer one. No retry: the page re-asks on as-of change or publication, and
/// a refusal is shown with its reason.
#[derive(Default)]
struct ReferenceRefresh {
    tag: Cell<u64>,
}

/// What the page says when a reference or poll submission was refused.
fn reference_refusal_reason(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Busy => "the data service is busy — press r to retry",
        Refusal::Stopped => "the data service has stopped",
    }
}

/// The live reference tables behind `ReferenceGlobal`, one lane per
/// reference dataset under `REFERENCE_KEY`. Only a dataset's latest tag is
/// applied, so a slow answer cannot replace a newer one. A refused read keeps
/// its demand on a timer; a failed read keeps the last table, since an empty
/// one would turn every lookup into a missing value.
struct ReferenceCache {
    handle: DataHandle,
    tags: RefCell<HashMap<String, u64>>,
    /// Datasets with a retry timer armed: at most one each.
    retry: RefCell<HashSet<String>>,
    /// Datasets whose last answer was an error, so a run of failures warns once.
    failing: RefCell<HashSet<String>>,
    /// Each reference dataset's key column count, from the startup schema.
    key_columns: HashMap<String, usize>,
}

const REFERENCE_RETRY_DELAY: Duration = Duration::from_secs(1);

impl ReferenceCache {
    fn new(handle: DataHandle, datasets: &[(String, usize)]) -> ReferenceCache {
        ReferenceCache {
            handle,
            tags: RefCell::default(),
            retry: RefCell::default(),
            failing: RefCell::default(),
            key_columns: datasets.iter().cloned().collect(),
        }
    }

    fn is_reference(&self, dataset: &str) -> bool {
        self.key_columns.contains_key(dataset)
    }

    /// Read `dataset`'s live table under a new tag, superseding any read in
    /// flight. `Busy` arms a retry; `Stopped` drops the demand, since nothing
    /// would ever serve it. `window` is the window the cache serves; its
    /// closure ends a retry.
    fn refresh(self: &Rc<Self>, dataset: &str, window: WindowHandle<Root>, cx: &mut App) {
        // The tag becomes the latest only once submitted: a refused read
        // sends nothing, and advancing the tag anyway would drop the answer
        // to the read still in flight.
        let tag = self.tags.borrow().get(dataset).copied().unwrap_or(0) + 1;
        match self.handle.reference(ReferenceParams {
            key: REFERENCE_KEY,
            tag,
            dataset: dataset.to_string(),
            as_of: AsOf::Live,
        }) {
            Ok(()) => {
                self.tags.borrow_mut().insert(dataset.to_string(), tag);
            }
            Err(Refusal::Busy) => self.retry(dataset, window, cx),
            Err(Refusal::Stopped) => {}
        }
    }

    /// Reread after the delay. A dataset already waiting keeps its one timer,
    /// so a burst of refused publishes cannot pile up reads. The timer holds
    /// the cache weakly and checks the window: the drain keeps the cache
    /// alive until its next event, so only the window's closure reliably
    /// ends the lane.
    fn retry(self: &Rc<Self>, dataset: &str, window: WindowHandle<Root>, cx: &mut App) {
        if !self.retry.borrow_mut().insert(dataset.to_string()) {
            return;
        }
        let cache = Rc::downgrade(self);
        let dataset = dataset.to_string();
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(REFERENCE_RETRY_DELAY).await;
            cx.update(|cx| {
                if let Some(cache) = cache.upgrade() {
                    cache.retry.borrow_mut().remove(&dataset);
                    if window.read(cx).is_ok() {
                        cache.refresh(&dataset, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Apply a live answer, republishing the global only when a table
    /// changed. Answers under another key or a superseded tag are dropped.
    fn answer(&self, outcome: ReferenceOutcome, cx: &mut App) {
        if outcome.key != REFERENCE_KEY
            || self.tags.borrow().get(&outcome.dataset) != Some(&outcome.tag)
        {
            return;
        }
        let dataset = outcome.dataset;
        // Unreachable in practice: only a refreshed dataset has a tag.
        let Some(&key_columns) = self.key_columns.get(&dataset) else {
            return;
        };
        let current = cx.global::<ReferenceGlobal>().0.clone();
        let next = match outcome.table {
            Ok(Some(table)) => {
                self.note_succeeded(&dataset);
                current.with_table(&dataset, &table, key_columns)
            }
            Ok(None) => {
                self.note_succeeded(&dataset);
                current.without(&dataset)
            }
            Err(e) => {
                if self.note_failed(&dataset) {
                    tracing::warn!(
                        target: "geode::reference",
                        "reference '{dataset}' read failed: {e}"
                    );
                }
                None
            }
        };
        if let Some(next) = next {
            cx.set_global(ReferenceGlobal(Arc::new(next)));
        }
    }

    /// Record a failed read; true only when the dataset was not already
    /// failing, the one transition worth a warning.
    fn note_failed(&self, dataset: &str) -> bool {
        self.failing.borrow_mut().insert(dataset.to_string())
    }

    fn note_succeeded(&self, dataset: &str) {
        self.failing.borrow_mut().remove(dataset);
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
    let panels = bridge.panels.clone();
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
                    dataset: source.dataset.clone(),
                    paths: source.paths.clone(),
                    // A snapshot's priority is an unread default: snapshots
                    // are taken ahead of every file.
                    priority: if *shape == SourceShape::Snapshot {
                        String::new()
                    } else {
                        format!("{:?}", source.priority)
                    },
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
        // Fixed for the run: the service serves its startup schema.
        d.set_reference_datasets(
            bridge
                .reference_datasets
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
        );
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
            // This observer and the drain keep the diagnostics entity alive
            // past the window, and a late notify must not turn queued demand
            // into a read for a window nobody sees.
            if window.read(cx).is_err() {
                return;
            }
            if refresh.in_flight.get().is_some() || refresh.retry_pending.get() {
                return;
            }
            let Some(request) = diagnostics.update(cx, |d, _| d.take_catalog_request()) else {
                return;
            };
            let tag = refresh.tag.get() + 1;
            refresh.tag.set(tag);
            let as_of = shell.read(cx).active_frame().read(cx).as_of().clone();
            match handle.catalog(CatalogParams {
                key: DIAGNOSTICS_KEY,
                tag,
                as_of,
            }) {
                Ok(()) => refresh.in_flight.set(Some((tag, request))),
                Err(Refusal::Busy) => {
                    tracing::warn!(
                        target: "geode::query",
                        "catalog request refused: the data service is busy; retrying"
                    );
                    refresh.retry(&diagnostics, request, window, cx);
                }
                // Nothing will serve a retry, and the stopped segment already
                // says why: keeping the demand would re-ask on every notify.
                Err(Refusal::Stopped) => {}
            }
        }
    })
    .detach();

    // Reference reads and poll-now requests drain independently of the
    // catalog lane: a catalog read in flight must not hold a page's table.
    let reference_refresh = Rc::new(ReferenceRefresh::default());
    cx.observe(&diagnostics, {
        let handle = handle.clone();
        let diagnostics = diagnostics.clone();
        let shell = shell.clone();
        let refresh = reference_refresh.clone();
        move |_entity, cx| {
            // As the catalog lane: no submission for a closed window.
            if window.read(cx).is_err() {
                return;
            }
            if let Some(dataset) = diagnostics.update(cx, |d, _| d.take_reference_request()) {
                let tag = refresh.tag.get() + 1;
                refresh.tag.set(tag);
                let as_of = shell.read(cx).active_frame().read(cx).as_of().clone();
                if let Err(refusal) = handle.reference(ReferenceParams {
                    key: DIAGNOSTICS_KEY,
                    tag,
                    dataset: dataset.clone(),
                    as_of,
                }) {
                    diagnostics.update(cx, |d, cx| {
                        d.note_reference_refused(
                            &dataset,
                            ReferenceLane::Read,
                            reference_refusal_reason(refusal),
                        );
                        cx.notify();
                    });
                }
            }
            if let Some(dataset) = diagnostics.update(cx, |d, _| d.take_poll_request()) {
                // A poll has no answer of its own, so its refusal clears on
                // the next submission that is accepted.
                let result = handle.poll(dataset.clone());
                diagnostics.update(cx, |d, cx| {
                    let changed = match result {
                        Ok(()) => d.note_poll_submitted(&dataset),
                        Err(refusal) => {
                            d.note_reference_refused(
                                &dataset,
                                ReferenceLane::Poll,
                                reference_refusal_reason(refusal),
                            );
                            true
                        }
                    };
                    if changed {
                        cx.notify();
                    }
                });
            }
        }
    })
    .detach();

    // Live reference tables for `ReferenceGlobal`: read every reference
    // dataset now, then again on each of its publishes.
    let reference_cache = Rc::new(ReferenceCache::new(
        handle.clone(),
        &bridge.reference_datasets,
    ));
    for (dataset, _) in &bridge.reference_datasets {
        reference_cache.refresh(dataset, window, cx);
    }

    // Reloads: new views to the data thread and to the factory.
    cx.subscribe(&shell, {
        let handle = handle.clone();
        let factory = factory.clone();
        let panels = panels.clone();
        let timeseries = timeseries.clone();
        let classifications = bridge.classifications.clone();
        let startup_schema = bridge.schema.clone();
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
                // Parse named colors separately to retain their own validation diagnostics;
                // load_views uses them for reference checks but does not return those errors.
                // Definitions and the value mapping checked against them, as one.
                let (colours, colour_diags) = NamedColours::from_config(config);
                for d in &colour_diags {
                    tracing::warn!(target: "geode::query", "{d}");
                }
                // Update both factories from one parsed color definition set.
                timeseries.set_colours(colours.clone());
                factory.set_colours(colours);
                let (dims, _) = config
                    .doc(DIMENSIONS_DOC)
                    .map(DerivedDimensions::from_doc)
                    .unwrap_or_default();
                // Refresh factory settings on ConfigReloaded. The stale
                // threshold lives in `app` and arrives with
                // `AppSettingsReloaded` instead.
                factory.set_views(views.clone());
                factory.set_find_style(FindStyle::from_config(config));
                // Refresh the factory's validation schema from current config. Dataset-only
                // edits require restart and do not emit ConfigReloaded; a later eligible
                // reload can update this factory before the service's schema is rebuilt.
                let mut pin_diags = Vec::new();
                let mut pinned = None;
                if let Some(mut schema) = config.doc("datasets").map(|d| SchemaSpec::from_doc(d).0)
                {
                    pin_diags.extend(geode_compose::pin_app_datasets(&mut schema, config));
                    pinned = Some(Rc::new(schema.clone()));
                    factory.set_schema(schema);
                }
                factory.set_dims(dims.clone());
                // The classifications snapshot, from the same dims, pinned
                // schema (the startup one when no datasets doc loaded) and
                // views; pushed once the config borrow has ended.
                let (layers, shadowed) = classification_provenance(config, &dims);
                let classification_config = geode_classifications::ClassificationsConfig {
                    dims: dims.clone(),
                    schema: pinned.unwrap_or_else(|| startup_schema.clone()),
                    views: views.clone(),
                    layers,
                    shadowed,
                };
                // A refused hand-off leaves the service on the old views while
                // the factory builds tiles against the new ones: say so.
                let handoff = match handle.replace_views(views, dims) {
                    Ok(()) => None,
                    Err(refusal) => Some(Diagnostic {
                        severity: match refusal {
                            Refusal::Busy => Severity::Warning,
                            Refusal::Stopped => Severity::Error,
                        },
                        layer: None,
                        file: None,
                        message: format!(
                            "the reloaded views did not reach the data service: {refusal}"
                        ),
                        path: None,
                    }),
                };
                // The config borrow has ended; the factory's tiles and the
                // diagnostics can now be updated through cx.
                classifications.set_config(classification_config, cx);
                let reload_diags: Vec<Diagnostic> = presentation_diags
                    .into_iter()
                    .chain(colour_diags)
                    .chain(pin_diags)
                    .chain(handoff)
                    .collect();
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
                if let Err(refusal) = queued {
                    let outcome = DistinctOutcome {
                        key: params.key,
                        tag: params.tag,
                        column: params.column.clone(),
                        values: Err(match refusal {
                            Refusal::Busy => "the data service is busy — try again".into(),
                            Refusal::Stopped => "the data service has stopped".into(),
                        }),
                    };
                    shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                }
            }
            // Queued before the frame's revision notification, so the new
            // threshold is in the shared cell when each tile's flip re-arms
            // its stale wake-up.
            ShellEvent::AppSettingsReloaded => {
                let stale_after = stale_after_from_config(shell.read(cx).config());
                factory.set_stale_after(stale_after);
                // Every panel shares the one stale threshold.
                for panel in &panels {
                    panel.set_stale_after(stale_after);
                }
            }
            ShellEvent::RestartRequired(_) => {}
            // The shell already logged and installed the rejected reload's diagnostics.
            ShellEvent::ReloadRejected(_) => {}
        }
    })
    .detach();

    // The frame's config revision advances on every applied reload, including
    // pricer views and pricing settings that do not emit ConfigReloaded.
    // Compare pricer_config_key before updating the factory so unrelated edits
    // leave its views and refresh timers alone.
    {
        let pricer = bridge.pricer.clone();
        let underlyings = bridge.underlyings.clone();
        let schema = bridge.schema.clone();
        let diagnostics = diagnostics.clone();
        let shell = shell.clone();
        let frame = shell.read(cx).frame().clone();
        let last = Rc::new(Cell::new(frame.read(cx).config_version()));
        // Seeded with the key the factory was built from; `None` (a factory
        // built from an unknown config) lets the first reload through.
        let last_key = Rc::new(std::cell::RefCell::new(bridge.pricer_key.clone()));
        cx.observe(&frame, move |frame, cx| {
            let now = frame.read(cx).config_version();
            if now == last.get() {
                return;
            }
            last.set(now);
            // Read everything out of the config before the factory takes
            // `cx` mutably.
            let (views, templates, colours, dims, mut diags, refresh, stale_after, payout) = {
                let config = shell.read(cx).config();
                let key = pricer_config_key(config);
                if last_key.borrow().as_ref() == Some(&key) {
                    return;
                }
                *last_key.borrow_mut() = Some(key);
                // `load_views`'s own diagnostics are the ConfigReloaded
                // observer's to report, as are the colors doc's; only the
                // pricer's are collected here.
                let (specs, _) = load_views(config);
                let (views, diags) = pricer_views_from_specs(config, &specs);
                // Definitions and the value mapping checked against them, as one.
                let (colours, _) = NamedColours::from_config(config);
                // The dimensions doc's own diagnostics are the
                // ConfigReloaded observer's to report.
                let (dims, _) = config
                    .doc(DIMENSIONS_DOC)
                    .map(DerivedDimensions::from_doc)
                    .unwrap_or_default();
                let (refresh, refresh_diag) = pricing_refresh_from_config(config);
                // A bad entry keeps the running definition of its name.
                let (templates, template_diags) =
                    pricer_templates_from_config(config, &pricer.templates(), "previous");
                let (names, underlying_diags) = pricing_underlyings_from_config(config);
                if let Some(names) = names {
                    underlyings.set(&names);
                }
                let (payout, payout_diags) = pricing_payout_currency_from_config(config, &schema);
                let mut diags = diags;
                diags.extend(template_diags);
                diags.extend(refresh_diag);
                diags.extend(underlying_diags);
                diags.extend(payout_diags);
                (
                    views,
                    templates,
                    colours,
                    dims,
                    diags,
                    refresh,
                    stale_after_from_config(config),
                    payout,
                )
            };
            // Before `reload`: its rebuild re-applies every tile's scope
            // against the new dimensions.
            pricer.set_dims(dims);
            pricer.reload(views, templates, colours, refresh, stale_after, payout, cx);
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
    // The handle's `Busy` refusal total, read once per drained event.
    let refused_handle = handle.clone();
    let catalog_refresh_for_drain = catalog_refresh.clone();
    let reference_refresh_for_drain = reference_refresh.clone();
    // Retain local dataset names for the drain task after attach's borrow ends.
    let local_datasets = Rc::clone(&bridge.local_datasets);
    // The pricer's sheet writes are answered through the drain.
    let pricer = Rc::clone(&bridge.pricer);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let diagnostics = diagnostics_for_drain;
        let catalog_refresh = catalog_refresh_for_drain;
        let reference_refresh = reference_refresh_for_drain;
        // The handle for the retry lanes (`window` below is the `&mut Window`).
        let retry_window = window;
        let mut last_dropped = 0u64;
        let mut last_refused = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);
            let now_refused = refused_handle.dropped_requests();
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
                if now_refused != last_refused {
                    diagnostics.update(cx, |d, cx| {
                        let before = d.version();
                        d.note_refused(now_refused);
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
                    // Routed by the requesting tile's key, like an upload.
                    DataEvent::TextFile(outcome) => {
                        shell.update(cx, |s, cx| s.deliver(Delivery::TextFile(outcome), window, cx));
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
                        if reference_cache.is_reference(&dataset) {
                            reference_cache.refresh(&dataset, retry_window, cx);
                        }
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
                    // Shell keys go to the shell's own consumers (picker,
                    // scopes, expression suggestions, action values), which
                    // validate tag and column themselves; any other key is a
                    // tile's, delivered like a query.
                    DataEvent::Distinct(outcome) => {
                        if is_shell_key(outcome.key) {
                            shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                        } else {
                            shell.update(cx, |s, cx| {
                                s.deliver(Delivery::Distinct(outcome), window, cx)
                            });
                        }
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
                                let current_as_of = shell.read(cx).active_frame().read(cx).as_of().clone();
                                diagnostics.update(cx, |d, cx| {
                                    let before = d.version();
                                    // A publication while reading schedules another read, but
                                    // does not starve presentation of consistent snapshots.
                                    // An old as-of, however, must never replace the current one.
                                    if snapshot.as_of == current_as_of {
                                        d.set_catalog(snapshot, SystemTime::now());
                                    } else {
                                        // Explicit consumers need a current answer even when
                                        // no diagnostics page observes the frame's as-of.
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
                                catalog_refresh.retry(&diagnostics, request, retry_window, cx);
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
                    // Route local-write outcomes by dataset and document key. The pricer
                    // factory uses the sheet name to find the originating tile, including
                    // loads deferred until a queued save settles. Every such outcome must
                    // survive mailbox coalescing. Other datasets have no local-write recipient
                    // here; failures also reach diagnostics as separate error events.
                    DataEvent::LocalPublished { dataset, batch, .. } => {
                        if let Some(sheet) = pricer_sheet(&dataset, &batch) {
                            pricer.save_answered(sheet, Ok(()), cx);
                        }
                    }
                    DataEvent::LocalPublishFailed {
                        dataset,
                        batch,
                        reason,
                    } => {
                        if let Some(sheet) = pricer_sheet(&dataset, &batch) {
                            pricer.save_answered(sheet, Err(reason), cx);
                        }
                    }
                    DataEvent::ForgetFailed {
                        dataset,
                        batch,
                        reason,
                    } => {
                        if let Some(sheet) = pricer_sheet(&dataset, &batch) {
                            pricer.forget_answered(sheet, Err(reason), cx);
                        }
                    }
                    // A forget changes what the database holds without a
                    // `Published`, so a watched catalog is refreshed here or it
                    // would keep listing the forgotten document.
                    DataEvent::Forgotten { dataset, batch } => {
                        if let Some(sheet) = pricer_sheet(&dataset, &batch) {
                            pricer.forget_answered(sheet, Ok(()), cx);
                        }
                        diagnostics.update(cx, |d, cx| {
                            d.request_catalog_refresh();
                            if d.pending_catalog_request() {
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
                    // Route vol slices to the keyed occupant; the shell discards absent recipients.
                    DataEvent::VolSlices(outcome) => {
                        shell.update(cx, |s, cx| {
                            s.deliver(Delivery::VolSlices(outcome), window, cx)
                        });
                    }
                    // The live lane keeps `ReferenceGlobal`; see `ReferenceCache`.
                    DataEvent::Reference(outcome) if outcome.key == REFERENCE_KEY => {
                        reference_cache.answer(outcome, cx);
                    }
                    // Only the latest submission's answer counts. The page
                    // compares its as-of with the frame and re-asks itself;
                    // the bridge does not.
                    DataEvent::Reference(outcome) => {
                        if outcome.key != DIAGNOSTICS_KEY
                            || outcome.tag != reference_refresh.tag.get()
                        {
                            return;
                        }
                        diagnostics.update(cx, |d, cx| {
                            d.set_reference(outcome);
                            cx.notify();
                        });
                    }
                    // A position command's answer becomes the status notice;
                    // `geode_data::positions` already logs it under
                    // `geode::ingest`.
                    DataEvent::Command(outcome) => {
                        shell.update(cx, |s, cx| s.note_command(&outcome, cx));
                    }
                    // A data thread died despite containment, or the request
                    // loop never opened. Its segment and the diagnostics row
                    // stay until restart. Logging is not repeated here: the
                    // supervisor logs a death, and an open failure also
                    // arrives as an error Diagnostic.
                    DataEvent::ThreadStopped { thread, reason } => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_thread_stopped(&thread, reason, SystemTime::now());
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    // The app's open is waiting for the collector to hand the
                    // store over, and then has it; shown by the status segment.
                    DataEvent::StoreWaiting { holder } => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_store_waiting(holder);
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
                    DataEvent::StoreOpened => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_store_opened();
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
                }
            });
            if handled.is_err() {
                return; // the window is gone
            }
            last_dropped = now_dropped;
            last_refused = now_refused;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};
    use geode_core::log::Ring;
    use geode_core::query::{CatalogOutcome, CatalogSnapshot, QueryKey, ReferenceTable};
    use geode_data::source::SourceSpec;
    use geode_diagnostics::DiagnosticsPageFactory;
    use geode_marketdata::core::builtin_panel;
    use geode_pricer::store::MemorySheetStore;
    use geode_shell::actions::ActionRegistry;
    use geode_shell::defaults::{
        BUILTIN_KEYMAP, default_mod, register_builtin_actions, register_page_actions,
    };
    use geode_shell::keymap::build_keymap;
    use geode_shell::module::recording::{Recorded, RecordingFactory};
    use geode_shell::module::{ModuleFactory, ModuleRoster, PageFactory};
    use geode_shell::session::TileRecords;
    use geode_shell::shell::ShellServices;
    use geode_shell::tiling::{TileId, Workspaces};
    use geode_shell::{theme, vimfind::FindStyle};
    use gpui::AppContext as _;
    use std::cell::RefCell;
    use std::path::Path;

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

    /// Capture logs emitted by `f` on this thread with a scoped subscriber.
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

    /// Minimal shell services built through public APIs, with no module roster
    /// or persisted session.
    fn test_shell_services() -> ShellServices {
        test_shell_services_with_sources(ConfigSources::default())
    }

    /// A categorical book dimension that the shell can offer in its picker.
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
            restored_pinned: Default::default(),
            restored_links: Default::default(),
            restored_palette_usage: geode_shell::palette_usage::PaletteUsage::new(),
            log: None,
            action_tail: std::sync::Arc::new(std::sync::Mutex::new(
                geode_shell::diagnostics::ActionTail::new(),
            )),
            keymap_diagnostics: Vec::new(),
            keymap_fragments: Vec::new(),
            keymap_fragment_diagnostics: Vec::new(),
            composition_diagnostics: Vec::new(),
            pages: geode_shell::module::PageRoster::new(),
            restored_pages: std::collections::BTreeMap::new(),
        }
    }

    /// Shell services with a recording factory and its delivery log. Opening
    /// "rec" through ShellView::open_module creates a real occupant without an
    /// add action or key binding; the log exposes deliveries to that occupant.
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
            restored_pinned: Default::default(),
            restored_links: Default::default(),
            restored_palette_usage: geode_shell::palette_usage::PaletteUsage::new(),
            log: None,
            action_tail: std::sync::Arc::new(std::sync::Mutex::new(
                geode_shell::diagnostics::ActionTail::new(),
            )),
            keymap_diagnostics: Vec::new(),
            keymap_fragments: Vec::new(),
            keymap_fragment_diagnostics: Vec::new(),
            composition_diagnostics: Vec::new(),
            pages: geode_shell::module::PageRoster::new(),
            restored_pages: std::collections::BTreeMap::new(),
        };
        (services, log)
    }

    fn open_test_window(
        cx: &mut gpui::TestAppContext,
        services: ShellServices,
    ) -> WindowHandle<Root> {
        cx.update(gpui_component::init);
        open_shell_window(cx, services)
    }

    /// [`open_test_window`] for a window hosting a pricer tile: the
    /// pricer's `DataTable` key overrides are installed after
    /// `gpui_component::init`, as `main` installs them — gpui gives the
    /// later binding precedence, so the reverse order would let the
    /// table's own `escape`/arrow bindings beat the tile's.
    fn open_pricer_test_window(
        cx: &mut gpui::TestAppContext,
        services: ShellServices,
    ) -> WindowHandle<Root> {
        cx.update(gpui_component::init);
        cx.update(geode_pricer::init);
        open_shell_window(cx, services)
    }

    fn open_shell_window(
        cx: &mut gpui::TestAppContext,
        services: ShellServices,
    ) -> WindowHandle<Root> {
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
            TemplateSet::builtin(),
            PricerSettings::default(),
        ))
    }

    /// A bridge whose every factory is a default; for tests that exercise
    /// one factory's reload path.
    fn test_bridge(handle: DataHandle) -> Bridge {
        let (_tx, rx) = crate::events::channel();
        // The factory reads the same list the reload observer sets, as
        // `start` wires them.
        let underlyings = Rc::new(UnderlyingList::default());
        let pricer = Rc::new(
            PricerFactory::new(
                handle.clone(),
                Rc::new(MemorySheetStore::default()),
                Views::builtin(),
                TemplateSet::builtin(),
                PricerSettings::default(),
            )
            .with_underlyings(underlyings.clone()),
        );
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer,
            handle,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings,
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

    #[test]
    fn pricing_underlyings_reads_an_array_and_warns_on_bad_values() {
        let config = |text: &str| {
            Config::load(&ConfigSources {
                builtin: vec![LayerDoc::builtin("app", text).unwrap()],
                desk: None,
                user: None,
            })
        };
        let (names, diags) = pricing_underlyings_from_config(&config(
            "[pricing]\nunderlyings = [\"spx\", 3, \"SX5E\"]\n",
        ));
        assert_eq!(
            names.as_deref(),
            Some(&["spx".to_string(), "SX5E".to_string()][..])
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].path.as_deref(), Some("app.pricing.underlyings"));
        assert_eq!(diags[0].severity, Severity::Warning);

        let (names, diags) =
            pricing_underlyings_from_config(&config("[pricing]\nunderlyings = \"SPX\"\n"));
        assert_eq!(
            names, None,
            "not an array: ignored, so a reload keeps its list"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].path.as_deref(), Some("app.pricing.underlyings"));

        let (names, diags) =
            pricing_underlyings_from_config(&config("[pricing]\nrefresh = \"10s\"\n"));
        assert_eq!(names, Some(Vec::new()), "absent: an empty list");
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// An `app` doc alone, for the `[pricing]` readers.
    fn app_config(text: &str) -> Config {
        Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", text).unwrap()],
            desk: None,
            user: None,
        })
    }

    /// A schema whose `underlyings` reference dataset is keyed by
    /// `underlying_ref` and carries `currency` and `name`.
    fn underlyings_schema() -> SchemaSpec {
        let (schema, diags) = SchemaSpec::from_doc(&geode_core::config::merge_docs(
            "datasets",
            &[LayerDoc::builtin(
                "datasets",
                r#"
[underlyings]
family = "reference"
key = ["underlying_ref"]
[underlyings.columns.underlying_ref]
type = "utf8"
role = "dimension"
[underlyings.columns.name]
type = "utf8"
role = "attribute"
[underlyings.columns.currency]
type = "utf8"
role = "attribute"
"#,
            )
            .unwrap()],
        ));
        assert!(diags.is_empty(), "fixture: {diags:?}");
        schema
    }

    fn payout(dataset: &str, column: &str) -> PayoutSource {
        PayoutSource {
            dataset: dataset.into(),
            column: column.into(),
        }
    }

    #[test]
    fn payout_currency_defaults_to_underlyings_currency() {
        let (source, diags) = pricing_payout_currency_from_config(
            &app_config("[pricing]\nrefresh = \"10s\"\n"),
            &underlyings_schema(),
        );
        assert_eq!(source, Some(payout("underlyings", "currency")));
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn payout_currency_without_the_default_dataset_is_none_quietly() {
        let config = app_config("[pricing]\nrefresh = \"10s\"\n");
        let (source, diags) = pricing_payout_currency_from_config(&config, &SchemaSpec::default());
        assert_eq!(source, None, "no underlyings dataset");
        assert!(diags.is_empty(), "{diags:?}");

        let mut schema = underlyings_schema();
        schema.datasets[0].columns.retain(|c| c.name != "currency");
        let (source, diags) = pricing_payout_currency_from_config(&config, &schema);
        assert_eq!(source, None, "underlyings without a currency column");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn an_explicit_payout_currency_resolves() {
        let (source, diags) = pricing_payout_currency_from_config(
            &app_config("[pricing]\npayout_currency = \"underlyings.name\"\n"),
            &underlyings_schema(),
        );
        assert_eq!(source, Some(payout("underlyings", "name")));
        assert!(diags.is_empty(), "{diags:?}");
    }

    fn assert_payout_error(text: &str, shown: &str) {
        let (source, diags) =
            pricing_payout_currency_from_config(&app_config(text), &underlyings_schema());
        assert_eq!(source, None, "{text}");
        assert_eq!(diags.len(), 1, "{text}: {diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(
            diags[0].path.as_deref(),
            Some("app.pricing.payout_currency")
        );
        assert!(diags[0].layer.is_some(), "names its layer: {diags:?}");
        assert_eq!(
            diags[0].message,
            format!(
                "[pricing] payout_currency = \"{shown}\" must name a text column of a \
                 single-key reference dataset, e.g. \"underlyings.currency\"; new lines get no currency"
            )
        );
    }

    #[test]
    fn a_payout_currency_naming_a_missing_column_is_an_error() {
        assert_payout_error(
            "[pricing]\npayout_currency = \"underlyings.ccy\"\n",
            "underlyings.ccy",
        );
        assert_payout_error(
            "[pricing]\npayout_currency = \"listings.currency\"\n",
            "listings.currency",
        );
        assert_payout_error("[pricing]\npayout_currency = \"currency\"\n", "currency");
        assert_payout_error(
            "[pricing]\npayout_currency = \"underlyings.currency.code\"\n",
            "underlyings.currency.code",
        );
        assert_payout_error("[pricing]\npayout_currency = 3\n", "3");

        // A dataset of another family is not looked up by underlying.
        let mut schema = underlyings_schema();
        schema.datasets[0].family = geode_core::schema::Family::Measures;
        let (source, diags) = pricing_payout_currency_from_config(
            &app_config("[pricing]\npayout_currency = \"underlyings.currency\"\n"),
            &schema,
        );
        assert_eq!(source, None);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn a_payout_currency_naming_the_key_column_is_an_error() {
        assert_payout_error(
            "[pricing]\npayout_currency = \"underlyings.underlying_ref\"\n",
            "underlyings.underlying_ref",
        );
    }

    /// A lookup joins a multi-column key with `/` and the pricer looks up
    /// by `underlying_ref` alone, so a column of a two-key dataset never
    /// matches a line: refused, and quietly nothing for the absent key.
    #[test]
    fn a_payout_currency_in_a_multi_key_dataset_is_an_error() {
        let mut schema = underlyings_schema();
        schema.datasets[0].key.push("name".into());
        let (source, diags) = pricing_payout_currency_from_config(
            &app_config("[pricing]\npayout_currency = \"underlyings.currency\"\n"),
            &schema,
        );
        assert_eq!(source, None);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(
            diags[0].path.as_deref(),
            Some("app.pricing.payout_currency")
        );

        let (source, diags) = pricing_payout_currency_from_config(
            &app_config("[pricing]\nrefresh = \"10s\"\n"),
            &schema,
        );
        assert_eq!(source, None, "the default needs a single key too");
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// A column that is not text never parses as a currency code: refused,
    /// and quietly nothing for the absent key.
    #[test]
    fn a_payout_currency_naming_a_non_text_column_is_an_error() {
        let mut schema = underlyings_schema();
        for c in &mut schema.datasets[0].columns {
            if c.name == "currency" {
                c.ty = geode_core::schema::ColumnType::I64;
            }
        }
        let (source, diags) = pricing_payout_currency_from_config(
            &app_config("[pricing]\npayout_currency = \"underlyings.currency\"\n"),
            &schema,
        );
        assert_eq!(source, None);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(
            diags[0].path.as_deref(),
            Some("app.pricing.payout_currency")
        );

        let (source, diags) = pricing_payout_currency_from_config(
            &app_config("[pricing]\nrefresh = \"10s\"\n"),
            &schema,
        );
        assert_eq!(source, None, "the default needs a text column too");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_payout_currency_change_alone_passes_the_reload_gate() {
        let base = pricer_config_key(&app_config("[pricing]\nrefresh = \"10s\"\n"));
        assert_ne!(
            pricer_config_key(&app_config(
                "[pricing]\nrefresh = \"10s\"\npayout_currency = \"underlyings.name\"\n"
            )),
            base
        );
    }

    /// The reload key changes with live pricer settings; unrelated application
    /// settings leave it unchanged.
    #[test]
    fn the_pricer_config_key_changes_only_with_what_the_pricer_reads() {
        let config = |app: &str, views: &str, templates: &str| {
            Config::load(&ConfigSources {
                builtin: vec![
                    LayerDoc::builtin("app", app).unwrap(),
                    LayerDoc::builtin("views", views).unwrap(),
                    LayerDoc::builtin(PRICER_TEMPLATES_DOC, templates).unwrap(),
                ],
                desk: None,
                user: None,
            })
        };
        let app = "[theme]\nname = \"a\"\n[log]\nlevel = \"info\"\n\
                   [pricing]\nrefresh = \"10s\"\n[blotter]\nstale_after = \"5m\"\n";
        let views = SLIM_VIEW;
        let templates = "[RR]\nlegs = [ { weight = -1, strike = 1, kind = \"P\" }, \
                         { weight = 1, strike = 2, kind = \"C\" } ]\n";
        let base = pricer_config_key(&config(app, views, templates));
        assert_eq!(
            pricer_config_key(&config(&app.replace("\"a\"", "\"b\""), views, templates)),
            base,
            "a [theme] edit"
        );
        assert_eq!(
            pricer_config_key(&config(
                &app.replace("\"info\"", "\"debug\""),
                views,
                templates
            )),
            base,
            "a [log] edit"
        );
        assert_ne!(
            pricer_config_key(&config(
                app,
                &views.replace("name = \"qty\"\nkind = \"dimension\"\n", ""),
                templates
            )),
            base,
            "a views edit"
        );
        assert_ne!(
            pricer_config_key(&config(app, views, &templates.replace("-1", "-2"))),
            base,
            "a pricer_templates edit"
        );
        assert_ne!(
            pricer_config_key(&config(
                &app.replace("\"10s\"", "\"off\""),
                views,
                templates
            )),
            base,
            "a [pricing] refresh edit"
        );
        assert_ne!(
            pricer_config_key(&config(&app.replace("\"5m\"", "\"6m\""), views, templates)),
            base,
            "a stale_after edit"
        );
        assert_ne!(
            pricer_config_key(&config(
                &app.replace("[blotter]", "underlyings = [\"NDX\"]\n[blotter]"),
                views,
                templates
            )),
            base,
            "a [pricing] underlyings edit"
        );
    }

    #[test]
    fn pricer_templates_from_config_falls_back_to_the_builtin_set_without_a_doc() {
        let config = Config::load(&ConfigSources {
            builtin: vec![],
            desk: None,
            user: None,
        });
        let (set, diags) =
            pricer_templates_from_config(&config, &TemplateSet::builtin(), "built-in");
        assert!(diags.is_empty());
        assert_eq!(set, TemplateSet::builtin());
        assert_eq!(set.iter().count(), 7, "the seven built-ins");
    }

    /// With a doc, the merged layers decide: a user `CONDOR` joins the
    /// built-ins, and a bad entry is dropped with a diagnostic.
    #[test]
    fn pricer_templates_from_config_reads_the_merged_doc() {
        let config = Config::from_docs(vec![
            LayerDoc::builtin(PRICER_TEMPLATES_DOC, geode_pricer::core::BUILTIN_TEMPLATES).unwrap(),
            LayerDoc::builtin(
                PRICER_TEMPLATES_DOC,
                "[CONDOR]\nlegs = [ { weight = 1, strike = 1, kind = \"C\" }, \
                 { weight = -1, strike = 2, kind = \"C\" }, \
                 { weight = -1, strike = 3, kind = \"C\" }, \
                 { weight = 1, strike = 4, kind = \"C\" } ]\n\
                 [BAD]\nlegs = []\n",
            )
            .unwrap(),
        ]);
        let (set, diags) =
            pricer_templates_from_config(&config, &TemplateSet::builtin(), "built-in");
        assert!(set.resolve("CONDOR").is_some());
        assert!(set.resolve("RR").is_some(), "the built-ins stay");
        assert!(set.resolve("BAD").is_none());
        assert!(!diags.is_empty(), "the bad entry is reported");
    }

    /// A `views` doc with a blotter view and one pricer view (`slim`, over
    /// the computed `pricer` dataset).
    const SLIM_VIEW: &str = "[tree]\ndataset = \"risk\"\n[slim]\ndataset = \"pricer\"\n\
                             [[slim.columns]]\nname = \"qty\"\nkind = \"dimension\"\n\
                             [[slim.columns]]\nname = \"npv\"\n";

    /// The pricer's views are the `views` doc's entries over `pricer`: the
    /// app's builtin layer carries the bundled two, and a view over another
    /// dataset beside them is not the pricer's.
    #[test]
    fn pricer_views_come_from_the_views_doc_over_the_pricer_dataset() {
        let config = Config::load(&ConfigSources {
            builtin: crate::builtin_layer(None),
            desk: None,
            user: None,
        });
        let (specs, _) = load_views(&config);
        let (views, diags) = pricer_views_from_specs(&config, &specs);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            views.names().collect::<Vec<_>>(),
            vec!["barrier", "vanilla"]
        );

        let mut builtin = crate::builtin_layer(None);
        builtin.push(
            LayerDoc::builtin(
                "views",
                "[tree]\ndataset = \"risk_snapshot\"\n[[tree.columns]]\nname = \"npv\"\n",
            )
            .unwrap(),
        );
        let config = Config::load(&ConfigSources {
            builtin,
            desk: None,
            user: None,
        });
        let (specs, _) = load_views(&config);
        assert!(specs.iter().any(|s| s.name == "tree"), "fixture: merged in");
        let (views, diags) = pricer_views_from_specs(&config, &specs);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            views.names().collect::<Vec<_>>(),
            vec!["barrier", "vanilla"],
            "a view over another dataset is not a pricer view"
        );
    }

    /// A layer still carrying the retired document is told where the
    /// views now live, once, as an error on that document.
    #[test]
    fn a_pricer_views_doc_is_an_error_naming_views_toml() {
        let mut builtin = crate::builtin_layer(None);
        builtin.push(
            LayerDoc::builtin(PRICER_VIEWS_DOC, "[slim]\ncolumns = [\"qty\", \"npv\"]\n").unwrap(),
        );
        let config = Config::load(&ConfigSources {
            builtin,
            desk: None,
            user: None,
        });
        let (specs, _) = load_views(&config);
        let (views, diags) = pricer_views_from_specs(&config, &specs);
        assert_eq!(
            views.names().collect::<Vec<_>>(),
            vec!["barrier", "vanilla"],
            "the retired doc adds no view"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(
            diags[0].message.contains("views.toml"),
            "{}",
            diags[0].message
        );
        assert_eq!(diags[0].path.as_deref(), Some(PRICER_VIEWS_DOC));
    }

    /// The pricer's column plan is built from the merged presentation, so
    /// an edit to either overlay alone must reach open pricer tiles.
    #[test]
    fn a_presentation_only_edit_changes_the_pricer_key() {
        let config = |extra: Option<LayerDoc>| {
            let mut builtin = vec![LayerDoc::builtin("views", SLIM_VIEW).unwrap()];
            builtin.extend(extra);
            Config::load(&ConfigSources {
                builtin,
                desk: None,
                user: None,
            })
        };
        let base = pricer_config_key(&config(None));
        assert_ne!(
            pricer_config_key(&config(Some(
                LayerDoc::builtin("view_presentation", "[slim]\nhidden = [\"npv\"]\n").unwrap()
            ))),
            base,
            "a view_presentation edit"
        );
        assert_ne!(
            pricer_config_key(&config(Some(
                LayerDoc::builtin(
                    "dataset_presentation",
                    "[pricer.columns.npv]\nlabel = \"PX\"\n"
                )
                .unwrap()
            ))),
            base,
            "a dataset_presentation edit"
        );
        assert_ne!(
            pricer_config_key(&config(Some(
                LayerDoc::builtin(geode_core::config::COLORS_DOC, "[warm]\nhue = 30\n").unwrap()
            ))),
            base,
            "a colors edit"
        );
    }

    /// The pricer's colors carry the value mapping, so a `value_colors`
    /// edit alone must change the key and re-run the pricer's reload.
    #[test]
    fn the_pricer_reload_key_changes_with_value_colors() {
        let with = |text: &str| {
            Config::from_docs(vec![
                LayerDoc::builtin(geode_core::config::VALUE_COLORS_DOC, text).unwrap(),
            ])
        };
        assert_ne!(
            pricer_config_key(&with("[underlying_ref]\nSPX = \"blue\"\n")),
            pricer_config_key(&with("[underlying_ref]\nSPX = \"teal\"\n")),
            "a value-color edit alone must re-run the pricer's reload"
        );
    }

    /// The retired doc is part of the key: one added at runtime, with no
    /// other pricer-relevant edit, must still reach the reload path that
    /// raises its retirement diagnostic.
    #[test]
    fn a_runtime_added_pricer_views_doc_changes_the_pricer_key() {
        let config = |extra: Option<LayerDoc>| {
            let mut builtin = vec![LayerDoc::builtin("views", SLIM_VIEW).unwrap()];
            builtin.extend(extra);
            Config::load(&ConfigSources {
                builtin,
                desk: None,
                user: None,
            })
        };
        let base = pricer_config_key(&config(None));
        assert_ne!(
            pricer_config_key(&config(Some(
                LayerDoc::builtin(PRICER_VIEWS_DOC, "[slim]\ncolumns = [\"qty\", \"npv\"]\n")
                    .unwrap()
            ))),
            base,
            "a pricer_views doc appearing"
        );
    }

    /// A frame scope over the pricer may name a derived dimension, so a
    /// `dimensions` edit alone must change the key, and the reload observer
    /// hands the factory the new dimensions before its tiles rebuild.
    #[gpui::test]
    fn a_dimensions_reload_reaches_the_pricer(cx: &mut gpui::TestAppContext) {
        const REGION: &str = "[region]\nfrom = \"underlying_ref\"\n\
                              [region.values]\nUS = [\"SPX\", \"NDX\"]\n";
        let config = |extra: Option<LayerDoc>| {
            let mut builtin = vec![LayerDoc::builtin("views", SLIM_VIEW).unwrap()];
            builtin.extend(extra);
            Config::load(&ConfigSources {
                builtin,
                desk: None,
                user: None,
            })
        };
        let base = pricer_config_key(&config(None));
        assert_ne!(
            pricer_config_key(&config(Some(
                LayerDoc::builtin("dimensions", REGION).unwrap()
            ))),
            base,
            "a dimensions edit"
        );

        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", SLIM_VIEW).unwrap(),
                LayerDoc::builtin("dimensions", REGION).unwrap(),
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
        assert!(
            bridge.pricer.dims().get("region").is_none(),
            "fixture: built with no dimensions"
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
        let dims = bridge.pricer.dims();
        let region = dims.get("region").expect("the reload's dimensions");
        assert_eq!(region.from, "underlying_ref");
    }

    /// Pricer view reloads follow the frame's config revision: the observer
    /// re-reads the `views` doc and hands the factory its `pricer` views.
    #[gpui::test]
    fn a_config_reload_hands_the_pricer_factory_its_views(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![LayerDoc::builtin("views", SLIM_VIEW).unwrap()],
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
            vec!["barrier", "vanilla"],
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

    /// The `ConfigReloaded` observer hands the blotter factory colors that
    /// carry the checked value mapping, not the definitions alone.
    #[gpui::test]
    fn a_config_reload_hands_the_blotter_factory_the_value_colors(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            // The observer refreshes factories only when a `views` doc exists.
            builtin: vec![
                LayerDoc::builtin("views", SLIM_VIEW).unwrap(),
                LayerDoc::builtin(geode_core::config::COLORS_DOC, "[blue]\nhue = 240\n").unwrap(),
                LayerDoc::builtin(
                    "dimensions",
                    "[region]\nfrom = \"underlying_ref\"\n[region.values]\nUS = [\"SPX\"]\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    geode_core::config::VALUE_COLORS_DOC,
                    "[region]\nUS = \"blue\"\n",
                )
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
        assert!(
            bridge.factory.colours().values().is_empty(),
            "fixture: built with no value mapping"
        );
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        assert_eq!(
            bridge
                .factory
                .colours()
                .values()
                .get("region", "US")
                .map(|c| &**c),
            Some("blue")
        );
    }

    /// Builtin `desk` and `region`; the user layer redefines `desk` and adds
    /// `sector`.
    const BUILTIN_DIMS: &str = "[desk]\nfrom = \"book\"\n[region]\nfrom = \"underlying_ref\"\n";
    const USER_DIMS: &str = "[desk]\nfrom = \"book\"\n[sector]\nfrom = \"book\"\n";

    fn assert_classifications_snapshot(config: &geode_classifications::ClassificationsConfig) {
        let names: Vec<&str> = config.dims.all().map(|d| d.name.as_str()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        for name in ["desk", "region", "sector"] {
            assert!(names.contains(&name), "{names:?}");
        }
        assert_eq!(
            config.layers,
            BTreeMap::from([
                ("desk".to_string(), Layer::User),
                ("region".to_string(), Layer::Builtin),
                ("sector".to_string(), Layer::User),
            ])
        );
        // Only the user copy with a lower definition beneath it shadows one,
        // and the snapshot names that lower layer.
        assert_eq!(
            config.shadowed,
            BTreeMap::from([("desk".to_string(), Layer::Builtin)])
        );
    }

    /// The classifications factory holds its snapshot from startup, before
    /// any tile is restored and before any reload.
    #[gpui::test]
    fn startup_pushes_the_classifications_snapshot(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("dimensions.toml"), USER_DIMS).unwrap();
        let mut builtin = crate::builtin_layer(Some(dir.path()));
        builtin.push(LayerDoc::builtin("dimensions", BUILTIN_DIMS).unwrap());
        let config = Config::load(&ConfigSources {
            builtin,
            desk: None,
            user: Some(user.path().to_path_buf()),
        });
        let setup = data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        let views = setup.views.len();
        let bridge =
            cx.update(|cx| start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let snapshot = bridge.classifications.config().expect("pushed at startup");
        assert_classifications_snapshot(&snapshot);
        assert_eq!(snapshot.views.len(), views);
        assert!(
            Rc::ptr_eq(&snapshot.schema, &bridge.schema),
            "the startup schema"
        );
        bridge.handle.shutdown();
    }

    /// The `ConfigReloaded` observer pushes the classifications factory the
    /// reloaded dimensions with their layers.
    #[gpui::test]
    fn a_config_reload_pushes_the_classifications_snapshot(cx: &mut gpui::TestAppContext) {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("dimensions.toml"), USER_DIMS).unwrap();
        let services = test_shell_services_with_sources(ConfigSources {
            // The observer refreshes factories only when a `views` doc exists.
            builtin: vec![
                LayerDoc::builtin("views", SLIM_VIEW).unwrap(),
                LayerDoc::builtin("dimensions", BUILTIN_DIMS).unwrap(),
            ],
            desk: None,
            user: Some(user.path().to_path_buf()),
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (handle, _rx) = DataHandle::for_tests();
        let bridge = test_bridge(handle);
        assert!(
            bridge.classifications.config().is_none(),
            "fixture: built with no snapshot"
        );
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        let snapshot = bridge.classifications.config().expect("pushed on reload");
        assert_classifications_snapshot(&snapshot);
        assert!(
            snapshot.views.iter().any(|v| v.name == "slim"),
            "the reloaded views"
        );
    }

    // --- Classifications through the composition root -----------------

    /// The dataset the end-to-end classifications are checked against, as
    /// the module's own fixtures declare it: `underlying_ref` is a
    /// groupable text column there, so a classification over it may be
    /// written (with no measure at a grain it is not groupable).
    const CLASS_DATASETS: &str = r#"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.delta]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
    const CLASS_DIMS: &str =
        "[region]\nfrom = \"underlying_ref\"\n[region.values]\nEurope = [\"SX5E\", \"DAX\"]\n";

    /// A shell assembled as startup assembles it (`add_bridge_modules`, the
    /// roster's actions and keymap fragments, the bridge's drain) with two
    /// classifications tiles showing `region` (1, focused, and 2) and a
    /// recording tile (3), over a writable user directory. The
    /// classifications factory holds its snapshot before any tile is
    /// restored, as `start` pushes it.
    struct ClassificationsShell {
        vcx: gpui::VisualTestContext,
        shell: Entity<ShellView>,
        handle: DataHandle,
        requests: std::sync::mpsc::Receiver<geode_data::Request>,
        events: crate::events::Sender,
        tail: Arc<std::sync::Mutex<geode_shell::diagnostics::ActionTail>>,
        _user: tempfile::TempDir,
    }

    impl ClassificationsShell {
        fn open(cx: &mut gpui::TestAppContext) -> ClassificationsShell {
            let user = tempfile::tempdir().unwrap();
            let (handle, requests) = DataHandle::for_tests();
            let (tx, rx) = crate::events::channel();
            let mut bridge = test_bridge(handle.clone());
            bridge.events = rx;
            let sources = ConfigSources {
                builtin: vec![
                    LayerDoc::builtin("datasets", CLASS_DATASETS).unwrap(),
                    LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                    LayerDoc::builtin("dimensions", CLASS_DIMS).unwrap(),
                ],
                desk: None,
                user: Some(user.path().to_path_buf()),
            };
            let mut services = test_shell_services_with_sources(sources);
            let config = services.config.clone();
            let dims = DerivedDimensions::from_doc(config.doc(DIMENSIONS_DOC).unwrap()).0;
            let schema = SchemaSpec::from_doc(config.doc("datasets").unwrap()).0;
            let (layers, shadowed) = classification_provenance(&config, &dims);
            cx.update(|cx| {
                bridge.classifications.set_config(
                    geode_classifications::ClassificationsConfig {
                        dims,
                        schema: Rc::new(schema),
                        views: Vec::new(),
                        layers,
                        shadowed,
                    },
                    cx,
                )
            });

            let mut roster = ModuleRoster::new();
            crate::add_bridge_modules(&mut roster, &bridge);
            roster.add(Box::new(RecordingFactory::new("rec")));
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
                &geode_shell::session::PinnedRecords::new(),
                &geode_shell::palette_usage::PaletteUsage::new(),
                &geode_shell::session::PageRecords::new(),
            );
            let ws1: toml::Table = r#"
                focused = 1
                [node]
                kind = "split"
                orientation = "horizontal"
                ratios = [0.34, 0.33, 0.33]
                [[node.children]]
                kind = "leaf"
                id = 1
                [[node.children]]
                kind = "leaf"
                id = 2
                [[node.children]]
                kind = "leaf"
                id = 3
                [tiles.1]
                module = "classifications"
                [tiles.1.state]
                version = 1
                name = "region"
                [tiles.2]
                module = "classifications"
                [tiles.2.state]
                version = 1
                name = "region"
                [tiles.3]
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
            let tail = services.action_tail.clone();

            cx.update(gpui_component::init);
            cx.update(geode_classifications::init);
            let user_dir = user.path().to_path_buf();
            let window = cx
                .update(|cx| {
                    cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                        let view =
                            cx.new(|cx| ShellView::new(services, None, Some(user_dir), window, cx));
                        cx.new(|cx| Root::new(view, window, cx))
                    })
                })
                .unwrap();
            cx.update(|cx| attach(&bridge, window, cx));
            let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
            let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
                root.view().clone().downcast::<ShellView>().unwrap()
            });
            let mut s = ClassificationsShell {
                vcx,
                shell,
                handle,
                requests,
                events: tx,
                tail,
                _user: user,
            };
            s.draw();
            assert_eq!(
                s.shell.read_with(&s.vcx, |sh, _| (
                    sh.occupant_kind(TileId(1)),
                    sh.occupant_kind(TileId(2)),
                    sh.occupant_kind(TileId(3))
                )),
                (
                    Some("classifications"),
                    Some("classifications"),
                    Some("rec")
                )
            );
            s
        }

        fn draw(&mut self) {
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            self.vcx.run_until_parked();
        }

        /// The values reads the tiles asked since the last call, in order.
        fn distinct_requests(&self) -> Vec<geode_core::query::DistinctParams> {
            self.requests
                .try_iter()
                .filter_map(|r| match r {
                    geode_data::Request::Distinct(p) => Some(p),
                    _ => None,
                })
                .collect()
        }

        /// Post a values answer to the bridge's real drain.
        fn answer(&mut self, key: QueryKey, tag: u64, values: &[(&str, u64)]) {
            self.events
                .try_send(DataEvent::Distinct(geode_core::query::DistinctOutcome {
                    key,
                    tag,
                    column: "underlying_ref".into(),
                    values: Ok(values.iter().map(|(s, n)| (s.to_string(), *n)).collect()),
                }))
                .unwrap();
            self.draw();
        }

        fn focused(&self) -> Option<TileId> {
            self.shell.read_with(&self.vcx, |s, _| {
                s.services().workspaces.active().focused_tile()
            })
        }

        fn dispatched(&self, id: &str) -> usize {
            let h = geode_shell::diagnostics::fnv1a(id);
            self.tail
                .lock()
                .unwrap()
                .recent()
                .filter(|x| *x == h)
                .count()
        }

        /// Run `title` from the shell's palette.
        fn palette(&mut self, title: &str) {
            self.vcx.simulate_keystrokes("ctrl-k");
            self.draw();
            self.vcx.simulate_input(title);
            self.vcx.simulate_keystrokes("enter");
            self.draw();
        }
    }

    /// A tile-keyed values answer through the bridge's drain reaches the
    /// classifications tile that asked; a shell-keyed one (the picker's,
    /// over the same column) reaches the open picker and no tile.
    #[gpui::test]
    fn a_tile_keyed_distinct_reaches_a_classifications_tile_and_a_picker_one_does_not(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut s = ClassificationsShell::open(cx);
        let asked = s.distinct_requests();
        let first = asked
            .iter()
            .find(|p| p.key == QueryKey(1))
            .expect("tile 1 asks for its values")
            .clone();
        assert_eq!(first.column, "underlying_ref");
        assert!(
            asked.iter().any(|p| p.key == QueryKey(2)),
            "tile 2 asks under its own key"
        );
        assert!(
            s.vcx.debug_bounds("classifications-row-SMI").is_none(),
            "fixture: SMI is not in the map"
        );

        // The picker asks for the same column under the shell's key.
        let shell = s.shell.clone();
        s.vcx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                geode_shell::shell::picker::open(view, Some("underlying_ref".into()), window, cx);
            });
        });
        s.draw();
        let picked = s
            .distinct_requests()
            .into_iter()
            .find(|p| p.key == geode_shell::shell::PICKER_KEY)
            .expect("the picker asks for its values");
        s.answer(picked.key, picked.tag, &[("SMI", 4), ("DAX", 2)]);
        assert_eq!(
            s.shell
                .read_with(&s.vcx, |sh, _| sh.picker().and_then(|p| p.values.clone())),
            Some(Ok(vec![("SMI".to_string(), 4), ("DAX".to_string(), 2)])),
            "the shell's key reaches the picker"
        );
        assert!(
            s.vcx.debug_bounds("classifications-row-SMI").is_none(),
            "and no tile"
        );
        s.vcx.simulate_keystrokes("escape");
        s.draw();

        s.answer(QueryKey(1), first.tag, &[("SMI", 4), ("DAX", 2)]);
        assert!(
            s.vcx.debug_bounds("classifications-row-SMI").is_some(),
            "the tile's own answer fills its grid"
        );
    }

    /// A classifications action run from the palette reaches the focused
    /// classifications tile only: the refresh asks for that tile's values
    /// alone, and with another kind focused it asks for none.
    #[gpui::test]
    fn a_palette_classifications_action_reaches_only_the_focused_classifications_tile(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut s = ClassificationsShell::open(cx);
        s.distinct_requests();
        assert_eq!(s.focused(), Some(TileId(1)), "fixture");

        s.palette("Classification: Refresh values");
        assert_eq!(
            s.dispatched("classifications::refresh"),
            1,
            "fixture: the palette dispatched the refresh"
        );
        let keys: Vec<QueryKey> = s.distinct_requests().iter().map(|p| p.key).collect();
        assert_eq!(keys, [QueryKey(1)], "only the focused tile refreshed");

        s.vcx.simulate_keystrokes("alt-l alt-l");
        s.draw();
        assert_eq!(
            s.focused(),
            Some(TileId(3)),
            "fixture: the recorder focused"
        );
        s.palette("Classification: Refresh values");
        assert_eq!(
            s.dispatched("classifications::refresh"),
            2,
            "fixture: the palette dispatched it again"
        );
        assert!(
            s.distinct_requests().is_empty(),
            "no classifications tile acted for another kind's focus"
        );
    }

    /// A label set in a classifications tile goes through the config door:
    /// past the debounce the shell's `dimensions` document carries it, and
    /// the reload hands the data service the relabelled dimension.
    #[gpui::test]
    fn a_label_edit_reaches_the_dimensions_doc_and_the_data_service(cx: &mut gpui::TestAppContext) {
        let mut s = ClassificationsShell::open(cx);
        let tag = s
            .distinct_requests()
            .iter()
            .find(|p| p.key == QueryKey(1))
            .expect("tile 1 asks for its values")
            .tag;
        // SMI, unclassified, leads the default order under the cursor.
        s.answer(QueryKey(1), tag, &[("SMI", 4), ("DAX", 2), ("SX5E", 1)]);
        assert!(
            s.handle.pending_dimensions_for_tests().is_none(),
            "fixture: no views handed over yet"
        );

        s.vcx.simulate_keystrokes("c");
        s.draw();
        s.vcx.simulate_input("Alpine");
        s.vcx.simulate_keystrokes("enter");
        s.draw();
        s.vcx.executor().advance_clock(Duration::from_millis(300));
        s.draw();

        let label = |dims: &DerivedDimensions, source: &str| {
            dims.get("region")
                .and_then(|d| d.values.get(source).cloned())
        };
        let shell_dims = s.shell.read_with(&s.vcx, |sh, _| {
            DerivedDimensions::from_doc(sh.config().doc(DIMENSIONS_DOC).unwrap()).0
        });
        assert_eq!(label(&shell_dims, "SMI").as_deref(), Some("Alpine"));
        assert_eq!(
            label(&shell_dims, "DAX").as_deref(),
            Some("Europe"),
            "the whole object, the other labels kept"
        );
        let handed = s
            .handle
            .pending_dimensions_for_tests()
            .expect("the reload replaced the service's views");
        assert_eq!(label(&handed, "SMI").as_deref(), Some("Alpine"));
    }

    impl ClassificationsShell {
        /// Run every file request the tiles asked since the last call
        /// through the data tier's file operation, on the real disk, and
        /// post each answer to the bridge's drain as the file worker does.
        /// The `geode-files` worker's queue and thread are bypassed:
        /// `geode_data::files::run` is called directly, on this thread.
        fn run_file_requests(&mut self) -> usize {
            let asked: Vec<_> = self
                .requests
                .try_iter()
                .filter_map(|r| match r {
                    geode_data::Request::TextFile(p) => Some(p),
                    _ => None,
                })
                .collect();
            for p in &asked {
                let result = geode_data::files::run(p);
                self.events
                    .try_send(DataEvent::TextFile(geode_core::textfile::TextFileOutcome {
                        key: p.key,
                        tag: p.tag,
                        path: p.path.clone(),
                        result,
                    }))
                    .unwrap();
            }
            self.draw();
            asked.len()
        }
    }

    /// A CSV round trip through the composition root: Export writes the
    /// shown classification to the file the save dialog names, on disk;
    /// the file edited there and imported through the open dialog asks
    /// y/n, and past the debounce the shell's `dimensions` document
    /// carries the file's labels, the ones it does not name kept.
    #[gpui::test]
    fn a_classification_exported_edited_and_imported_reaches_the_dimensions_doc(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut s = ClassificationsShell::open(cx);
        let tag = s
            .distinct_requests()
            .iter()
            .find(|p| p.key == QueryKey(1))
            .expect("tile 1 asks for its values")
            .tag;
        s.answer(QueryKey(1), tag, &[("SMI", 4), ("DAX", 2), ("SX5E", 1)]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("region.csv");

        s.palette("Classification: Export CSV");
        assert_eq!(s.dispatched("classifications::export"), 1, "fixture");
        let target = path.clone();
        s.vcx.simulate_new_path_selection(|_| Some(target));
        s.draw();
        assert_eq!(s.run_file_requests(), 1, "one write");
        let region = s.shell.read_with(&s.vcx, |sh, _| {
            DerivedDimensions::from_doc(sh.config().doc(DIMENSIONS_DOC).unwrap()).0
        });
        let region = region.get("region").expect("fixture").clone();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, geode_core::classification::export(&region, None));
        assert!(written.contains("DAX,Europe"), "{written}");

        // DAX moves, SMI gains a label; SX5E is not named and stays.
        std::fs::write(&path, "underlying_ref,region\nDAX,DACH\nSMI,Alpine\n").unwrap();
        s.palette("Classification: Import CSV");
        assert_eq!(s.dispatched("classifications::import"), 1, "fixture");
        let chosen = path.clone();
        s.vcx.simulate_path_prompt_response(|_| Some(vec![chosen]));
        s.draw();
        assert_eq!(s.run_file_requests(), 1, "one read");
        s.vcx.simulate_keystrokes("y");
        s.draw();
        s.vcx.executor().advance_clock(Duration::from_millis(300));
        s.draw();

        let shell_dims = s.shell.read_with(&s.vcx, |sh, _| {
            DerivedDimensions::from_doc(sh.config().doc(DIMENSIONS_DOC).unwrap()).0
        });
        let label = |source: &str| {
            shell_dims
                .get("region")
                .and_then(|d| d.values.get(source).cloned())
        };
        assert_eq!(label("DAX").as_deref(), Some("DACH"));
        assert_eq!(label("SMI").as_deref(), Some("Alpine"));
        assert_eq!(
            label("SX5E").as_deref(),
            Some("Europe"),
            "merged, not replaced"
        );
    }

    /// A desk `pricer_templates` layer: a `CONDOR` and a broken `RR`
    /// (`weight = 0`), over the builtin seven.
    fn desk_templates() -> Vec<LayerDoc> {
        vec![
            LayerDoc::builtin(PRICER_TEMPLATES_DOC, geode_pricer::core::BUILTIN_TEMPLATES).unwrap(),
            LayerDoc::builtin(
                PRICER_TEMPLATES_DOC,
                "[RR]\nlegs = [ { weight = 0, strike = 1, kind = \"P\" }, \
                 { weight = 1, strike = 2, kind = \"C\" } ]\n\
                 [CONDOR]\nlegs = [ { weight = 1, strike = 1, kind = \"C\" }, \
                 { weight = -1, strike = 2, kind = \"C\" }, \
                 { weight = -1, strike = 3, kind = \"C\" }, \
                 { weight = 1, strike = 4, kind = \"C\" } ]\n",
            )
            .unwrap(),
        ]
    }

    /// The reload observer hands the factory the configured templates,
    /// and a broken entry keeps the running definition of its name.
    #[gpui::test]
    fn a_config_reload_hands_the_pricer_factory_its_templates(cx: &mut gpui::TestAppContext) {
        let mut builtin = vec![LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap()];
        builtin.extend(desk_templates());
        let services = test_shell_services_with_sources(ConfigSources {
            builtin,
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
        assert!(
            !bridge.pricer.template_names().contains(&"CONDOR".into()),
            "fixture: built with the builtin set"
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
        let names = bridge.pricer.template_names();
        assert!(names.contains(&"CONDOR".into()), "{names:?}");
        assert!(
            names.contains(&"RR".into()),
            "the broken RR keeps the running one: {names:?}"
        );
    }

    /// A reload hands the entry bar's underlying list its new
    /// `[pricing] underlyings`, and the list's revision moves so open
    /// tiles re-read it.
    #[gpui::test]
    fn a_config_reload_hands_the_pricer_factory_its_underlyings(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("app", "[pricing]\nunderlyings = [\"ndx\"]\n").unwrap(),
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
        let source = bridge.pricer.underlying_source();
        let before = vcx.update(|_, cx| {
            assert!(
                source.underlyings(cx).is_empty(),
                "fixture: built with no underlyings"
            );
            source.revision(cx)
        });
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
        vcx.update(|_, cx| {
            let list = source.underlyings(cx);
            assert_eq!(list.iter().map(|s| s.as_ref()).collect::<Vec<_>>(), ["NDX"]);
            assert_ne!(source.revision(cx), before, "the revision moved");
        });
    }

    /// A reload resolves `[pricing] payout_currency` against the bridge's
    /// startup schema and hands the result to the factory's settings.
    #[gpui::test]
    fn a_config_reload_hands_the_pricer_factory_its_payout_source(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("app", "[pricing]\npayout_currency = \"underlyings.name\"\n")
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
        let mut bridge = test_bridge(handle);
        bridge.schema = Rc::new(underlyings_schema());
        assert_eq!(bridge.pricer.settings().payout, None, "fixture");
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
        assert_eq!(
            bridge.pricer.settings().payout,
            Some(payout("underlyings", "name"))
        );
    }

    /// At startup `[pricing] payout_currency` reaches the factory's
    /// settings, and an unresolvable one is reported with the other
    /// startup diagnostics.
    #[gpui::test]
    fn startup_hands_the_pricer_factory_its_payout_source(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let datasets = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                        [underlyings]\nfamily = \"reference\"\nkey = [\"underlying_ref\"]\n\
                        [underlyings.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                        [underlyings.columns.currency]\ntype = \"utf8\"\nrole = \"attribute\"\n";
        let setup_with = |app: &str, dir: &std::path::Path| {
            let config = Config::load(&ConfigSources {
                builtin: vec![
                    LayerDoc::builtin("datasets", datasets).unwrap(),
                    LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n")
                        .unwrap(),
                    LayerDoc::builtin("app", app).unwrap(),
                ],
                ..ConfigSources::default()
            });
            data_setup(
                &config,
                dir.join("t.duckdb"),
                AdapterRegistry::default(),
                geode_data::PricerRegistry::default(),
                geode_data::VolModelRegistry::default(),
            )
            .unwrap()
        };
        let dir = tempfile::tempdir().unwrap();
        let bad = setup_with(
            "[pricing]\npayout_currency = \"underlyings.ccy\"\n",
            dir.path(),
        );
        assert_eq!(bad.pricer_settings.payout, None);
        assert!(
            bad.diagnostics
                .iter()
                .any(|d| d.path.as_deref() == Some("app.pricing.payout_currency")),
            "{:?}",
            bad.diagnostics
        );
        let setup = setup_with("[pricing]\nrefresh = \"10s\"\n", dir.path());
        let bridge =
            cx.update(|cx| start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let payout_source = bridge.pricer.settings().payout;
        bridge.handle.shutdown();
        assert_eq!(payout_source, Some(payout("underlyings", "currency")));
    }

    /// A reload whose `[pricing] underlyings` is not an array keeps the
    /// list the bar had (hot reload keeps the last valid state) rather
    /// than emptying it.
    #[gpui::test]
    fn a_malformed_underlyings_reload_keeps_the_last_list(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("app", "[pricing]\nunderlyings = \"NDX\"\n").unwrap(),
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
        bridge.underlyings.set(&["SPX".to_string()]);
        let source = bridge.pricer.underlying_source();
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
        vcx.update(|_, cx| {
            let list = source.underlyings(cx);
            assert_eq!(
                list.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
                ["SPX"],
                "the malformed value was ignored"
            );
        });
    }

    /// At startup the configured templates reach the factory, and a broken
    /// entry falls back to the builtin definition of its name.
    #[gpui::test]
    fn startup_hands_the_pricer_factory_its_templates(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let mut builtin = vec![
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            )
            .unwrap(),
            LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
        ];
        builtin.extend(desk_templates());
        builtin.push(LayerDoc::builtin("app", "[pricing]\nunderlyings = [\"spx\"]\n").unwrap());
        let config = Config::load(&ConfigSources {
            builtin,
            ..ConfigSources::default()
        });
        let setup = data_setup(
            &config,
            dir.path().join("t.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        assert!(
            setup
                .diagnostics
                .iter()
                .any(|d| d.message.contains("keeping the built-in definition")),
            "{:?}",
            setup.diagnostics
        );
        let bridge =
            cx.update(|cx| start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let names = bridge.pricer.template_names();
        let underlyings = cx.update(|cx| bridge.pricer.underlying_source().underlyings(cx));
        bridge.handle.shutdown();
        assert_eq!(
            underlyings.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            ["SPX"],
            "[pricing] underlyings reaches the factory at startup"
        );
        assert!(names.contains(&"CONDOR".into()), "{names:?}");
        assert_eq!(
            bridge.pricer.templates().resolve("RR"),
            TemplateSet::builtin().resolve("RR"),
            "the broken RR falls back to the builtin one"
        );
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
            builtin: vec![LayerDoc::builtin("views", SLIM_VIEW).unwrap()],
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
            bridge.pricer.reload(
                Views::builtin(),
                TemplateSet::builtin(),
                NamedColours::default(),
                None,
                Duration::from_secs(1),
                None,
                cx,
            )
        });
        bump(&mut vcx);
        assert_eq!(
            bridge.pricer.view_names(),
            vec!["barrier", "vanilla"],
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
            builtin: vec![LayerDoc::builtin("views", SLIM_VIEW).unwrap()],
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
            vec!["barrier", "vanilla"],
            "the first unchanged reload reached nothing"
        );
    }

    /// A shell holding one restored pricer tile, its roster, actions and
    /// keymap fragment wired exactly as `main` wires them — so a typed
    /// key travels the shell's real matcher and insert-focus predicate.
    fn test_shell_services_with_a_pricer_tile() -> ShellServices {
        test_shell_services_with_a_pricer_tile_and(|_| {})
    }

    /// [`test_shell_services_with_a_pricer_tile`], with `roster_hook`
    /// adding what the test hosts beside the pricer before the keymap is
    /// built.
    fn test_shell_services_with_a_pricer_tile_and(
        roster_hook: impl FnOnce(&mut ModuleRoster),
    ) -> ShellServices {
        let mut services = test_shell_services();
        let (handle, _rx) = DataHandle::for_tests();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(PricerFactory::new(
            handle,
            Rc::new(MemorySheetStore::default()),
            Views::builtin(),
            TemplateSet::builtin(),
            PricerSettings::default(),
        )));
        roster_hook(&mut roster);
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
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
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
        let window = open_pricer_test_window(cx, services);
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

    /// End to end: a real pricer in the shell, a real right press on a
    /// line's cell. gpui-component's selectable table stops a cell's right
    /// press, so only the shell's captured listener sees it; the pricer
    /// records the row and answers `press_context` for it, and the row
    /// menu opens at the pointer on the PRESSED line (SPX), not the cursor
    /// line (NDX, the last typed). `enter` (a mouse-opened surface takes
    /// typed keys) picks Open Rec with that underlying.
    #[gpui::test]
    fn a_right_press_on_a_pricer_line_opens_its_row_menu(cx: &mut gpui::TestAppContext) {
        let mut rec = RecordingFactory::new("rec");
        rec.accepts = &["underlying_ref"];
        let log = rec.log.clone();
        let services = test_shell_services_with_a_pricer_tile_and(|roster| {
            roster.add(Box::new(rec));
        });
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let draw = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            vcx.run_until_parked();
        };
        draw(&mut vcx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.simulate_keystrokes("o");
        vcx.simulate_input("-5 SPX Z26 5000 C");
        vcx.simulate_keystrokes("enter");
        vcx.simulate_input("-5 NDX Z26 20000 C");
        vcx.simulate_keystrokes("enter escape");
        draw(&mut vcx);

        let at = vcx
            .debug_bounds("pricer-cell-0-1")
            .expect("the SPX line's first value cell is painted")
            .center();
        vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
        vcx.simulate_mouse_up(at, gpui::MouseButton::Right, gpui::Modifiers::none());
        draw(&mut vcx);
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.notice_for_test()),
            None,
            "the press found a row to offer"
        );

        vcx.simulate_keystrokes("enter");
        draw(&mut vcx);
        let launched: Vec<_> = log
            .borrow()
            .iter()
            .filter_map(|r| match r {
                Recorded::Created(_, state) => Some(state.clone()),
                _ => None,
            })
            .collect();
        let expected: toml::Table = r#"underlying = ["SPX"]"#.parse().unwrap();
        assert_eq!(
            launched,
            vec![Some(expected)],
            "the press opened the pressed line's menu (SPX), not the cursor line's (NDX)"
        );

        // gpui-component's table holds the menu it builds on every right
        // press in a cycle only its next right press breaks: press once
        // more and close the window in the same update.
        vcx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    button: gpui::MouseButton::Right,
                    position: at,
                    modifiers: gpui::Modifiers::none(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.remove_window();
        });
        vcx.run_until_parked();
    }

    /// The whole route: a real health report on the shell's `Diagnostics`
    /// reaches a hosted pricer tile's header, and a real click on the chip
    /// opens the diagnostics page through the shell's drain.
    #[gpui::test]
    fn a_health_chip_click_opens_the_diagnostics_page(cx: &mut gpui::TestAppContext) {
        use geode_shell::module::recording::{PageRecorded, RecordingPageFactory};
        let mut services = test_shell_services_with_a_pricer_tile();
        let page = RecordingPageFactory::new("diagnostics");
        let log = page.log();
        let page_view = page.view();
        services.pages.add(Box::new(page));
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.open_page_kind_for_test()),
            None,
            "fixture: no page is open before the click"
        );
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        diagnostics.update(&mut vcx, |d, cx| {
            d.describe_source(
                "sheets_src",
                geode_shell::diagnostics::SourceSummary::for_dataset("pricer_sheets"),
            );
            d.note_health(
                "sheets_src",
                geode_shell::diagnostics::Health::Failed {
                    reason: "disk full".into(),
                },
                "disk full".into(),
                std::time::SystemTime::UNIX_EPOCH,
            );
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let at = vcx
            .debug_bounds("tile-health-1")
            .expect("the pricer tile paints its chip")
            .center();
        vcx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count: 1,
        });
        vcx.run_until_parked();
        // Right after the click, parked: the page is open AND holds focus.
        // `open_page`'s `prevent_default` does nothing on this route (the
        // chip's own mouse-down already ran), so focus is the fact to pin.
        assert!(log.borrow().contains(&PageRecorded::Visible(true)));
        assert!(
            log.borrow()
                .contains(&PageRecorded::Reveal("sheets_src".into())),
            "the page reveals the chip's source"
        );
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.open_page_kind_for_test()),
            Some("diagnostics"),
            "the page is open"
        );
        assert!(
            vcx.update(|window, cx| {
                page_view
                    .borrow()
                    .as_ref()
                    .expect("the page was created")
                    .read(cx)
                    .is_focused(window)
            }),
            "the page holds focus after a chip click"
        );
    }

    /// The palette's `Edit column in view…` reads the focused pricer tile's
    /// columns and opens the Views column picker over the pricer's view.
    #[gpui::test]
    fn edit_column_in_view_opens_over_the_pricer_view(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_a_pricer_tile();
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.simulate_keystrokes("ctrl-k");
        vcx.simulate_input("Edit column in view");
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let target = shell.read_with(&vcx, |s, _| s.choice_dialog_target());
        assert!(
            matches!(
                target,
                Some(geode_shell::shell::choicedialog::Target::Column { ref view, .. }) if view == "vanilla"
            ),
            "{target:?}"
        );
    }

    /// The sheet picker's filter and the rename field, both opened by the
    /// pointer on the header's sheet name, are insert focus for the shell
    /// too: a shifted letter typed after the click is text, never a shell
    /// binding (`shift+d` is `workspace::duplicate_horizontal`). The keys
    /// are typed AFTER the click, the way a mouse-opened field is used.
    #[gpui::test]
    fn typing_into_the_pricer_sheet_picker_and_rename_field_fires_no_shell_binding(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::diagnostics::fnv1a;
        let (handle, _rx) = DataHandle::for_tests();
        let services = test_shell_services();
        let tail = services.action_tail.clone();
        let (services, tiles) = with_a_pricer_tile_on(services, test_pricer(&handle), "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let draw = |vcx: &mut gpui::VisualTestContext| {
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            })
        };
        draw(&mut vcx);
        let tile = tiles.borrow()[0].clone();
        let dispatched = |id: &str| {
            let h = fnv1a(id);
            tail.lock().unwrap().recent().any(|x| x == h)
        };
        let field =
            |vcx: &mut gpui::VisualTestContext| tile.read_with(vcx, |t, cx| t.sheet_field_text(cx));
        let name = vcx
            .debug_bounds("pricer-sheet-name")
            .expect("the sheet name is painted")
            .center();
        let press = |vcx: &mut gpui::VisualTestContext, click_count: usize| {
            vcx.simulate_event(gpui::MouseDownEvent {
                position: name,
                modifiers: gpui::Modifiers::default(),
                button: gpui::MouseButton::Left,
                click_count,
                first_mouse: false,
            });
            vcx.simulate_event(gpui::MouseUpEvent {
                position: name,
                modifiers: gpui::Modifiers::default(),
                button: gpui::MouseButton::Left,
                click_count,
            });
            vcx.run_until_parked();
            draw(vcx);
        };

        press(&mut vcx, 1);
        assert_eq!(
            field(&mut vcx).as_deref(),
            Some(""),
            "fixture: the picker opened"
        );
        vcx.simulate_keystrokes("shift-d");
        vcx.simulate_input("ec");
        vcx.run_until_parked();
        assert!(
            !dispatched("workspace::duplicate_horizontal"),
            "a capital typed into the sheet picker ran a shell binding"
        );
        assert_eq!(
            field(&mut vcx).as_deref(),
            Some("Dec"),
            "the picker took the text"
        );
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        draw(&mut vcx);
        assert!(dispatched("pricer::cancel"), "escape reached the pricer");
        assert_eq!(field(&mut vcx), None, "fixture: escape closed the picker");

        press(&mut vcx, 1);
        press(&mut vcx, 2);
        assert_eq!(
            field(&mut vcx).as_deref(),
            Some("a"),
            "fixture: the rename field opened"
        );
        vcx.simulate_keystrokes("shift-d");
        vcx.simulate_input("ay");
        vcx.run_until_parked();
        assert!(
            !dispatched("workspace::duplicate_horizontal"),
            "a capital typed into the rename field ran a shell binding"
        );
        assert_eq!(
            field(&mut vcx).as_deref(),
            Some("Day"),
            "the rename field took the text over its selected name"
        );
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert!(dispatched("pricer::commit"), "enter reached the pricer");
        assert_eq!(field(&mut vcx), None, "enter renamed and closed the field");
        assert_eq!(
            tile.read_with(&vcx, |t, _| t.title().to_string()),
            "Pricer · Day"
        );
    }

    /// A mod+double-click on the pricer's sheet name is the shell's
    /// fullscreen gesture and nothing else: no picker, no rename field.
    #[gpui::test]
    fn a_mod_double_click_on_the_pricer_sheet_name_fullscreens_and_opens_no_field(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::diagnostics::fnv1a;
        let (handle, _rx) = DataHandle::for_tests();
        let services = test_shell_services();
        let tail = services.action_tail.clone();
        let alias = services.mod_alias;
        let modifiers = gpui::Modifiers {
            control: alias.ctrl,
            alt: alias.alt,
            platform: alias.cmd,
            ..Default::default()
        };
        let (services, tiles) = with_a_pricer_tile_on(services, test_pricer(&handle), "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        let name = vcx
            .debug_bounds("pricer-sheet-name")
            .expect("the sheet name is painted")
            .center();
        for click_count in [1, 2] {
            vcx.simulate_event(gpui::MouseDownEvent {
                position: name,
                modifiers,
                button: gpui::MouseButton::Left,
                click_count,
                first_mouse: false,
            });
            vcx.simulate_event(gpui::MouseUpEvent {
                position: name,
                modifiers,
                button: gpui::MouseButton::Left,
                click_count,
            });
            vcx.run_until_parked();
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        let h = fnv1a("workspace::fullscreen_tile");
        assert!(
            tail.lock().unwrap().recent().any(|x| x == h),
            "the shell's mod+double-click fullscreened the tile"
        );
        assert_eq!(
            tile.read_with(&vcx, |t, cx| t.sheet_field_text(cx)),
            None,
            "the pricer opened no picker or rename field"
        );
    }

    /// "Add lines below…" committed from the shell palette while the bar is open
    /// leaves the bar, its text, and focus in its field. The palette's
    /// commit returns focus to the shell root before it dispatches, so an
    /// open bar that kept only its text would read `mode == insert`
    /// without holding focus, and a shifted letter would reach a shell
    /// binding (`shift+d` is `workspace::duplicate_horizontal`).
    #[gpui::test]
    fn a_palette_add_on_an_open_bar_keeps_typing_in_its_field(cx: &mut gpui::TestAppContext) {
        use geode_shell::diagnostics::fnv1a;
        let (handle, _rx) = DataHandle::for_tests();
        let services = test_shell_services();
        let tail = services.action_tail.clone();
        let (services, tiles) = with_a_pricer_tile_on(services, test_pricer(&handle), "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        let count = |id: &str| {
            let h = fnv1a(id);
            tail.lock().unwrap().recent().filter(|x| *x == h).count()
        };
        vcx.simulate_keystrokes("o");
        vcx.simulate_input("SPX ");
        vcx.run_until_parked();
        assert_eq!(count("pricer::add_below"), 1, "fixture: `o` opened the bar");
        vcx.simulate_keystrokes("ctrl-k");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(count("palette::toggle"), 1, "fixture: the palette opened");
        vcx.simulate_input("Add lines below");
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            count("pricer::add_below"),
            2,
            "fixture: the palette dispatched the add"
        );
        vcx.simulate_keystrokes("shift-d");
        vcx.run_until_parked();
        assert_eq!(
            count("workspace::duplicate_horizontal"),
            0,
            "a capital typed after the palette add ran a shell binding"
        );
        assert_eq!(
            tile.read_with(&vcx, |t, cx| t.entry_text(cx)).as_deref(),
            Some("SPX D"),
            "the text kept and the capital appended"
        );
    }

    /// The expiry's date field is insert focus for the shell too, though
    /// it is not a text input: a shifted letter typed into it is no shell
    /// binding (`shift+d` is `workspace::duplicate_horizontal`), digits
    /// reach the field, `up` steps it (the fragment's `insert_up`), and
    /// `enter` commits a date expiry — every key through the shell's real
    /// matcher and insert-focus predicate.
    #[gpui::test]
    fn typing_into_the_pricer_date_field_fires_no_shell_binding(cx: &mut gpui::TestAppContext) {
        use geode_shell::diagnostics::fnv1a;
        let (handle, _rx) = DataHandle::for_tests();
        let services = test_shell_services();
        let tail = services.action_tail.clone();
        let (services, tiles) = with_a_pricer_tile_on(services, test_pricer(&handle), "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        let mode = |vcx: &mut gpui::VisualTestContext| {
            tile.read_with(vcx, |t, _| {
                t.key_context().get("mode").unwrap_or("").to_string()
            })
        };
        let dispatched = |id: &str| {
            let h = fnv1a(id);
            tail.lock().unwrap().recent().any(|x| x == h)
        };
        type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert_eq!(mode(&mut vcx), "normal", "fixture: the entry field closed");
        // qty → underlying → expiry, then edit.
        vcx.simulate_keystrokes("l l i");
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(mode(&mut vcx), "insert", "the date field is insert mode");
        vcx.simulate_keystrokes("shift-d");
        vcx.run_until_parked();
        assert!(
            !dispatched("workspace::duplicate_horizontal"),
            "a capital typed into the date field ran a shell binding"
        );
        assert_eq!(mode(&mut vcx), "insert", "the field is still open");
        vcx.simulate_keystrokes("2 5 up enter");
        vcx.run_until_parked();
        assert_eq!(mode(&mut vcx), "normal", "enter committed and closed it");
        let expiry = tile.read_with(&vcx, |t, _| {
            t.sheet().instrument(0).map(|i| i.expiry().clone())
        });
        assert_eq!(
            expiry,
            Some(geode_core::pricing::Expiry::Date(
                chrono::NaiveDate::from_ymd_opt(2026, 12, 26).unwrap()
            )),
            "the digits typed the day and up stepped it"
        );
    }

    /// `escape` after committing a line closes the entry field that
    /// `enter` left open on the next line. The committed line is the
    /// table's selection, and `DataTable`'s own `escape` → `Cancel` would
    /// clear that selection and stop the key; the pricer's init rebinds it
    /// to `NoAction`, which wins only when installed after
    /// `gpui_component::init` — the order `main` uses.
    #[gpui::test]
    fn escape_after_a_committed_line_closes_the_entry_field(cx: &mut gpui::TestAppContext) {
        let (handle, _rx) = DataHandle::for_tests();
        let (services, tiles) =
            with_a_pricer_tile_on(test_shell_services(), test_pricer(&handle), "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        let mode = |vcx: &mut gpui::VisualTestContext| {
            tile.read_with(vcx, |t, _| {
                t.key_context().get("mode").unwrap_or("").to_string()
            })
        };
        type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
        assert_eq!(
            mode(&mut vcx),
            "insert",
            "fixture: enter leaves the next line's entry field open"
        );
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert_eq!(
            mode(&mut vcx),
            "normal",
            "the first escape after a committed line left the entry field open"
        );
    }

    /// The cell editor lives inside the table, so `DataTable`'s own
    /// `escape` (clear the selection, stop the key) must not beat the
    /// tile's cancel: the first escape closes the editor.
    #[gpui::test]
    fn escape_closes_the_cell_editor_inside_the_table(cx: &mut gpui::TestAppContext) {
        let (handle, _rx) = DataHandle::for_tests();
        let (services, tiles) =
            with_a_pricer_tile_on(test_shell_services(), test_pricer(&handle), "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        let mode = |vcx: &mut gpui::VisualTestContext| {
            tile.read_with(vcx, |t, _| {
                t.key_context().get("mode").unwrap_or("").to_string()
            })
        };
        type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert_eq!(mode(&mut vcx), "normal", "fixture: the bar closed");
        vcx.simulate_keystrokes("i");
        vcx.run_until_parked();
        assert_eq!(
            mode(&mut vcx),
            "insert",
            "fixture: `i` opened the cell editor"
        );
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert_eq!(
            mode(&mut vcx),
            "normal",
            "the first escape left the cell editor open"
        );
    }

    /// A key answering an armed `:rm` confirm is the confirm's alone: `j`
    /// cancels it and does not then reach the shell as a cursor move (the
    /// confirm has just given up the keyboard, so the shell would read the
    /// tile as in normal mode).
    #[gpui::test]
    fn a_key_answering_the_rm_confirm_reaches_nothing_else(cx: &mut gpui::TestAppContext) {
        use geode_pricer::store::SheetStore as _;
        let (handle, _rx) = DataHandle::for_tests();
        let store = MemorySheetStore::default();
        store.set_known(vec!["x".into()]);
        let pricer = Rc::new(PricerFactory::new(
            handle,
            Rc::new(store.clone()),
            Views::builtin(),
            TemplateSet::builtin(),
            PricerSettings::default(),
        ));
        let (services, tiles) = with_a_pricer_tile_on(test_shell_services(), pricer, "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
        // `enter` left the next line's entry field open.
        vcx.simulate_input("-3 SPX Z26 5100 C");
        vcx.simulate_keystrokes("enter");
        vcx.simulate_keystrokes("escape");
        vcx.simulate_keystrokes("k");
        vcx.run_until_parked();
        let cursor = |vcx: &gpui::VisualTestContext| {
            tile.read_with(vcx, |t, cx| t.serialize(cx).get("cursor").cloned())
        };
        let before = cursor(&vcx);
        vcx.simulate_keystrokes("j");
        vcx.run_until_parked();
        assert_ne!(cursor(&vcx), before, "fixture: `j` moves the cursor");
        vcx.simulate_keystrokes("k");
        vcx.run_until_parked();
        assert_eq!(cursor(&vcx), before, "fixture: `k` moves it back");

        run_command(&mut vcx, &tile, "rm x");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.simulate_keystrokes("j");
        vcx.run_until_parked();
        assert!(store.forgets().is_empty(), "`j` is not `y`");
        assert_eq!(
            tile.read_with(&vcx, |t, _| t
                .key_context()
                .get("mode")
                .unwrap_or("")
                .to_string()),
            "normal",
            "`j` cancelled the confirm"
        );
        assert_eq!(
            cursor(&vcx),
            before,
            "the confirm's `j` also moved the cursor"
        );
    }

    /// A press on the `:rm` prompt itself answers "no" and takes no focus
    /// (the prompt's own press-to-focus would hand the keyboard back to a
    /// question that is gone). The keyboard returns to the tile through the
    /// shell's restoration path: the very next `j` moves the cursor.
    #[gpui::test]
    fn the_tile_answers_keys_after_a_press_on_the_rm_prompt(cx: &mut gpui::TestAppContext) {
        use geode_pricer::store::SheetStore as _;
        let (handle, _rx) = DataHandle::for_tests();
        let store = MemorySheetStore::default();
        store.set_known(vec!["x".into()]);
        let pricer = Rc::new(PricerFactory::new(
            handle,
            Rc::new(store.clone()),
            Views::builtin(),
            TemplateSet::builtin(),
            PricerSettings::default(),
        ));
        let (services, tiles) = with_a_pricer_tile_on(test_shell_services(), pricer, "a");
        let window = open_pricer_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, _| window.activate_window());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
        vcx.simulate_input("-3 SPX Z26 5100 C");
        vcx.simulate_keystrokes("enter");
        vcx.simulate_keystrokes("escape");
        vcx.simulate_keystrokes("k");
        vcx.run_until_parked();
        let cursor = |vcx: &gpui::VisualTestContext| {
            tile.read_with(vcx, |t, cx| t.serialize(cx).get("cursor").cloned())
        };
        let mode = |vcx: &gpui::VisualTestContext| {
            tile.read_with(vcx, |t, _| {
                t.key_context().get("mode").unwrap_or("").to_string()
            })
        };

        type_command(&mut vcx, "rm x");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(mode(&vcx), "insert", "fixture: `:rm x` armed the confirm");
        let at = vcx
            .debug_bounds("pricer-remove-confirm-1")
            .expect("the prompt is painted")
            .center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(mode(&vcx), "normal", "the press cancelled the confirm");
        assert!(store.forgets().is_empty(), "a press is not `y`");

        let before = cursor(&vcx);
        vcx.simulate_keystrokes("j");
        vcx.run_until_parked();
        assert_ne!(
            cursor(&vcx),
            before,
            "`j` reached the tile with no other click after the prompt press"
        );
    }

    type PricerTiles = Rc<RefCell<Vec<Entity<geode_pricer::tile::PricerTile>>>>;

    /// Forwards to the pricer factory exactly as `main`'s handle does and
    /// keeps every tile it builds, so a test can read the tile the shell
    /// hosts (the shell exposes no occupant's view).
    struct KeepingPricer {
        factory: Rc<PricerFactory>,
        tiles: PricerTiles,
    }

    impl ModuleFactory for KeepingPricer {
        fn kind(&self) -> &'static str {
            self.factory.kind()
        }
        fn register_actions(&self, registry: &mut ActionRegistry) {
            self.factory.register_actions(registry)
        }
        fn contexts(&self) -> Vec<&'static str> {
            self.factory.contexts()
        }
        fn default_keymap(&self) -> Option<&'static str> {
            self.factory.default_keymap()
        }
        fn create(
            &self,
            tile: TileId,
            restored: Option<&toml::Table>,
            frame: geode_shell::frame::FrameRef,
            diagnostics: Entity<Diagnostics>,
            window: &mut gpui::Window,
            cx: &mut App,
        ) -> geode_shell::module::TileOccupant {
            let o = self
                .factory
                .create(tile, restored, frame, diagnostics, window, cx);
            let view = o.view.clone().downcast().expect("a pricer tile");
            self.tiles.borrow_mut().push(view);
            o
        }
    }

    /// `services` holding one restored pricer tile on `sheet`, built by
    /// `factory`, with the roster, actions and keymap fragment wired as
    /// `main` wires them.
    fn with_a_pricer_tile_on(
        mut services: ShellServices,
        factory: Rc<PricerFactory>,
        sheet: &str,
    ) -> (ShellServices, PricerTiles) {
        let tiles = PricerTiles::default();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(KeepingPricer {
            factory,
            tiles: tiles.clone(),
        }));
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
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
        );
        let ws1: toml::Table = format!(
            r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [tiles.1]
            module = "pricer"
            [tiles.1.state]
            sheet = "{sheet}"
        "#
        )
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let restored = geode_shell::session::from_toml(&table).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        services.workspaces = restored.workspaces;
        services.restored_tiles = restored.tiles;
        (services, tiles)
    }

    type BlotterTiles = Rc<RefCell<Vec<Entity<geode_blotter::tile::BlotterTile>>>>;

    /// Forwards to a blotter factory exactly as `main`'s handle does and
    /// keeps every tile it builds, as [`KeepingPricer`] does for the
    /// pricer.
    struct KeepingBlotter {
        factory: BlotterFactory,
        tiles: BlotterTiles,
    }

    impl ModuleFactory for KeepingBlotter {
        fn kind(&self) -> &'static str {
            self.factory.kind()
        }
        fn register_actions(&self, registry: &mut ActionRegistry) {
            self.factory.register_actions(registry)
        }
        fn contexts(&self) -> Vec<&'static str> {
            self.factory.contexts()
        }
        fn default_keymap(&self) -> Option<&'static str> {
            self.factory.default_keymap()
        }
        fn create(
            &self,
            tile: TileId,
            restored: Option<&toml::Table>,
            frame: geode_shell::frame::FrameRef,
            diagnostics: Entity<Diagnostics>,
            window: &mut gpui::Window,
            cx: &mut App,
        ) -> geode_shell::module::TileOccupant {
            let o = self
                .factory
                .create(tile, restored, frame, diagnostics, window, cx);
            let view = o.view.clone().downcast().expect("a blotter tile");
            self.tiles.borrow_mut().push(view);
            o
        }
    }

    /// A real blotter hosted in the shell, restored from a session whose
    /// one tile shows view `view` (declared by `views_toml`), with its
    /// first query answered by a snapshot of `columns`. `roster` adds what
    /// the test hosts beside the blotter before the keymap is built.
    struct ShellBlotter {
        vcx: gpui::VisualTestContext,
        tile: Entity<geode_blotter::tile::BlotterTile>,
        shell: Entity<ShellView>,
        window: WindowHandle<Root>,
        /// The handle the tile and the roster hook were given.
        handle: DataHandle,
        /// What the tile and the shell's actions asked the data service
        /// for after the first query.
        requests: std::sync::mpsc::Receiver<geode_data::Request>,
    }

    /// A [`ShellBlotter`] snapshot column's metadata: additive at both
    /// depths, direct scope, summable only for `delta01`.
    fn shell_blotter_meta(n: &str) -> geode_core::snapshot::ColumnMeta {
        use geode_core::attribution::{Attribution, ScopeSemantics};
        geode_core::snapshot::ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
            summable: n == "delta01",
            mixed_flag: None,
        }
    }

    fn shell_blotter(
        cx: &mut gpui::TestAppContext,
        views_toml: &str,
        view: &str,
        roster_hook: impl FnOnce(&mut ModuleRoster, &DataHandle),
        columns: Vec<(
            geode_core::snapshot::ColumnMeta,
            geode_core::snapshot::TestColumn,
        )>,
    ) -> ShellBlotter {
        let (handle, rx) = DataHandle::for_tests();
        let views = geode_core::view::ViewSpec::from_doc(&geode_core::config::merge_docs(
            "views",
            &[LayerDoc::builtin("views", views_toml).unwrap()],
        ))
        .0;
        let tiles = BlotterTiles::default();
        let mut services = test_shell_services();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(KeepingBlotter {
            factory: BlotterFactory::new(
                handle.clone(),
                views,
                NamedColours::default(),
                SchemaSpec::default(),
                DerivedDimensions::default(),
                FindStyle::default(),
                Duration::from_secs(900),
            ),
            tiles: tiles.clone(),
        }));
        roster_hook(&mut roster, &handle);
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
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
        );
        let ws1: toml::Table = format!(
            r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [tiles.1]
            module = "blotter"
            [tiles.1.state]
            view = "{view}"
        "#
        )
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let restored = geode_shell::session::from_toml(&table).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        services.workspaces = restored.workspaces;
        services.restored_tiles = restored.tiles;

        cx.update(gpui_component::init);
        cx.update(geode_blotter::init);
        let window = open_shell_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let tile = tiles.borrow()[0].clone();

        let tag = loop {
            match rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the tile asks for its rows")
            {
                geode_data::Request::Query(p) => break p.tag,
                _ => continue,
            }
        };
        let snap = Arc::new(geode_core::snapshot::Snapshot::for_tests(columns, 1));
        tile.update(&mut vcx, |t, cx| {
            t.deliver(
                geode_core::query::QueryOutcome {
                    key: QueryKey(1),
                    tag,
                    snapshot: Ok(snap),
                    submitted: std::time::Instant::now(),
                },
                cx,
            )
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        ShellBlotter {
            vcx,
            tile,
            shell,
            window,
            handle,
            requests: rx,
        }
    }

    impl ShellBlotter {
        /// A right press (down and up) on the painted cell `selector`.
        fn right_press(&mut self, selector: &'static str) {
            let at = self
                .vcx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is painted"))
                .center();
            self.vcx
                .simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
            self.vcx
                .simulate_mouse_up(at, gpui::MouseButton::Right, gpui::Modifiers::none());
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        /// Type `keys` (a mouse-opened surface takes typed keys), then draw.
        fn type_keys(&mut self, keys: &str) {
            self.vcx.simulate_keystrokes(keys);
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        /// The tile's selection kind and rows, and its cursor row.
        fn selection_and_cursor(
            &self,
        ) -> (
            geode_core::grid::selection::SelectKind,
            std::ops::Range<usize>,
            usize,
        ) {
            self.tile.read_with(&self.vcx, |t, cx| {
                let d = t.table().read(cx).delegate();
                let r = d.resolved.clone().expect("a V selection");
                (r.kind, r.rows, d.cursor.row)
            })
        }

        /// gpui-component's table holds the menu it builds on every right
        /// press in a cycle only its next right press breaks: press once
        /// more and close the window in the same update, so the deferred
        /// rebuild never runs (as the blotter's `release_the_table_menu`).
        fn close(mut self) {
            let at = self
                .vcx
                .debug_bounds("blotter-cell-0-0")
                .expect("the root row is painted")
                .center();
            self.vcx.update(|window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        button: gpui::MouseButton::Right,
                        position: at,
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.remove_window();
            });
            self.vcx.run_until_parked();
        }
    }

    /// A pointer-started selection must retain tile focus. Click a painted
    /// cell, then type `j` through the shell's keymap and focus routing;
    /// the resulting extension proves the next key reaches the blotter.
    #[gpui::test]
    fn a_shift_click_selection_keeps_focus_so_a_typed_key_extends_it(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_core::attribution::{Attribution, ScopeSemantics};
        use geode_core::grid::selection::SelectKind;
        use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
        let (handle, rx) = DataHandle::for_tests();
        let views = geode_core::view::ViewSpec::from_doc(&geode_core::config::merge_docs(
            "views",
            &[LayerDoc::builtin(
                "views",
                "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                 [[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
            )
            .unwrap()],
        ))
        .0;
        let tiles = BlotterTiles::default();
        let mut services = test_shell_services();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(KeepingBlotter {
            factory: BlotterFactory::new(
                handle,
                views,
                NamedColours::default(),
                SchemaSpec::default(),
                DerivedDimensions::default(),
                FindStyle::default(),
                Duration::from_secs(900),
            ),
            tiles: tiles.clone(),
        }));
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
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
        );
        let ws1: toml::Table = r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [tiles.1]
            module = "blotter"
            [tiles.1.state]
            view = "tree"
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

        cx.update(gpui_component::init);
        cx.update(geode_blotter::init);
        let window = open_shell_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();

        // Answer the tile's first query with three rows: root, L1, L2.
        let tag = loop {
            match rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the tile asks for its rows")
            {
                geode_data::Request::Query(p) => break p.tag,
                _ => continue,
            }
        };
        let meta = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
            summable: n == "delta01",
            mixed_flag: None,
        };
        let snap = Arc::new(Snapshot::for_tests(
            vec![
                (
                    meta("lhu"),
                    TestColumn::Dict(vec![None, Some("L1".into()), Some("L2".into())]),
                ),
                (meta("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    meta("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
            ],
            1,
        ));
        tile.update(&mut vcx, |t, cx| {
            t.deliver(
                geode_core::query::QueryOutcome {
                    key: QueryKey(1),
                    tag,
                    snapshot: Ok(snap),
                    submitted: std::time::Instant::now(),
                },
                cx,
            )
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // Shift+click the delta01 cell on L1: a block from the cursor.
        let at = vcx
            .debug_bounds("blotter-cell-1-1")
            .expect("the L1 delta01 cell is painted")
            .center();
        vcx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::shift());
        vcx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::shift());
        vcx.run_until_parked();
        let resolved = |vcx: &mut gpui::VisualTestContext| {
            tile.read_with(vcx, |t, cx| t.table().read(cx).delegate().resolved.clone())
        };
        let r = resolved(&mut vcx).expect("the shift+click started a selection");
        assert_eq!((r.kind, r.rows.clone()), (SelectKind::Block, 0..2));

        vcx.simulate_keystrokes("j");
        vcx.run_until_parked();
        let r = resolved(&mut vcx).expect("the selection is still live");
        assert_eq!(
            r.rows,
            0..3,
            "`j` typed after the click reached the blotter and extended the block"
        );
    }

    /// The row menu's production path end to end: a real blotter hosted in
    /// the shell, a right press on one of its cells, the shell reading the
    /// occupant's `press_context` through `TileContent` (the blotter's
    /// forwarder, not a test double), and `enter` typed after the press
    /// (the mouse-opened rule). The press lands on SPX inside a `V`
    /// selection whose cursor sits on NDX: the menu opens SPX, the pressed
    /// row (spec ruling 6), never the cursor row.
    #[gpui::test]
    fn a_right_press_on_a_blotter_cell_opens_the_pressed_rows_menu(cx: &mut gpui::TestAppContext) {
        use geode_core::grid::selection::SelectKind;
        use geode_core::snapshot::TestColumn;
        let mut rec = RecordingFactory::new("rec");
        rec.accepts = &["underlying_ref"];
        let log = rec.log.clone();
        let dict = |a: &str, b: &str| TestColumn::Dict(vec![None, Some(a.into()), Some(b.into())]);
        // The tile's first query is answered: root; L1 (SPX); L2 (NDX).
        let mut f = shell_blotter(
            cx,
            FLAT_VIEW,
            "flat",
            |roster, _| roster.add(Box::new(rec)),
            vec![
                (shell_blotter_meta("lhu"), dict("L1", "L2")),
                (shell_blotter_meta("underlying_ref"), dict("SPX", "NDX")),
                (
                    shell_blotter_meta("row_depth"),
                    TestColumn::I32(vec![0, 1, 1]),
                ),
                (
                    shell_blotter_meta("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
            ],
        );

        // `V` on SPX, then `j` to NDX: rows 1..3 selected, cursor on NDX.
        f.vcx.simulate_keystrokes("j shift-v j");
        f.vcx.run_until_parked();
        assert_eq!(f.selection_and_cursor(), (SelectKind::Rows, 1..3, 2));

        // Right-press SPX's underlying_ref cell, inside the selection.
        f.right_press("blotter-cell-1-1");

        // A mouse-opened surface takes typed keys: `enter` picks Open Rec.
        f.vcx.simulate_keystrokes("enter");
        f.vcx.run_until_parked();
        let launched: Vec<_> = log
            .borrow()
            .iter()
            .filter_map(|r| match r {
                Recorded::Created(_, state) => Some(state.clone()),
                _ => None,
            })
            .collect();
        let expected: toml::Table = r#"underlying = ["SPX"]"#.parse().unwrap();
        assert_eq!(
            launched,
            vec![Some(expected)],
            "the press opened the pressed row's menu (SPX), not the cursor row's (NDX)"
        );
        f.close();
    }

    /// The view both row-menu end-to-end fixtures restore: grouped by
    /// `lhu`, showing `underlying_ref` and `delta01`.
    const FLAT_VIEW: &str = "[flat]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                             [[flat.columns]]\nname = \"underlying_ref\"\nkind = \"dimension\"\n\
                             [[flat.columns]]\nname = \"delta01\"\n";

    /// A [`ShellBlotter`] with the row actions startup registers
    /// (`add_dimension_actions`: Open in Nemo, then Move LHU with a
    /// position service configured), a recording
    /// [`geode_shell::dimension::UrlOpener`] (the returned list), and one
    /// delivered snapshot grouped by `lhu`: root; L1 over P7 and P8; L2 over
    /// P9; L3 over P6. `position_ref` is a HIDDEN unanimity column (the view
    /// shows only `underlying_ref` and `delta01`) with its
    /// `position_ref#mixed` flag, as the compiler emits a context column:
    /// mixed on the root and L1, single on L2 and L3.
    fn nemo_blotter(cx: &mut gpui::TestAppContext) -> (ShellBlotter, Rc<RefCell<Vec<String>>>) {
        use geode_core::snapshot::{ColumnMeta, TestColumn};
        let opened: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = opened.clone();
        cx.update(|cx| {
            cx.set_global(geode_shell::dimension::UrlOpener(Rc::new(move |url, _| {
                sink.borrow_mut().push(url.to_string())
            })))
        });
        let meta = shell_blotter_meta;
        let dict = |v: [Option<&str>; 4]| {
            TestColumn::Dict(v.iter().map(|s| s.map(String::from)).collect())
        };
        let f = shell_blotter(
            cx,
            FLAT_VIEW,
            "flat",
            // The same call `add_bridge_modules` makes.
            |roster, data| crate::add_dimension_actions(roster, data, true),
            vec![
                (
                    meta("lhu"),
                    dict([None, Some("L1"), Some("L2"), Some("L3")]),
                ),
                (
                    meta("underlying_ref"),
                    dict([None, Some("SPX"), Some("SPX"), Some("SPX")]),
                ),
                (meta("row_depth"), TestColumn::I32(vec![0, 1, 1, 1])),
                (
                    meta("delta01"),
                    TestColumn::F64(vec![Some(12.0), Some(5.0), Some(4.0), Some(3.0)]),
                ),
                // Hidden: no view column names it. Index 5 is its flag.
                (
                    ColumnMeta {
                        mixed_flag: Some(5),
                        ..meta("position_ref")
                    },
                    dict([None, None, Some("P9"), Some("P6")]),
                ),
                (
                    meta("position_ref#mixed"),
                    TestColumn::Bool(vec![Some(true), Some(true), Some(false), Some(false)]),
                ),
            ],
        );
        (f, opened)
    }

    /// Right press on L2's `delta01` (a measure, so no leading column):
    /// the menu's first row is Open in Nemo under `position_ref · P9`, read
    /// from a column the view never shows. `enter` opens the URL through
    /// the production `ActionCx::open_url` and the shell paints the
    /// action's own notice.
    #[gpui::test]
    fn a_right_press_opens_the_rows_position_in_nemo(cx: &mut gpui::TestAppContext) {
        let (mut f, opened) = nemo_blotter(cx);
        f.right_press("blotter-cell-2-2");
        f.type_keys("enter");
        assert_eq!(*opened.borrow(), vec!["nemo://position/P9".to_string()]);
        assert!(
            f.vcx.debug_bounds("shell-notice").is_some(),
            "the notice is painted"
        );
        assert_eq!(
            f.shell
                .read_with(&f.vcx, |s, _| s.notice_for_test())
                .as_deref(),
            Some("opened nemo://position/P9")
        );
        f.close();
    }

    /// L1 sums two positions: `position_ref` is mixed there, so no Nemo
    /// row is offered, and nothing else has a row, so no menu opens; the
    /// `enter` that would pick a row opens nothing.
    #[gpui::test]
    fn a_subtotal_over_two_positions_offers_no_nemo_row(cx: &mut gpui::TestAppContext) {
        let (mut f, opened) = nemo_blotter(cx);
        f.right_press("blotter-cell-1-2");
        assert_eq!(
            f.shell
                .read_with(&f.vcx, |s, _| s.notice_for_test())
                .as_deref(),
            Some(geode_shell::shell::row_menu::NO_ROW_ACTIONS),
            "the press reached the shell, which found no row to offer"
        );
        f.type_keys("enter");
        assert_eq!(*opened.borrow(), Vec::<String>::new());
        f.close();
    }

    /// A right press inside a `V` selection opens the PRESSED row's
    /// position: L3 (P6), not the cursor row's (L2, P9) and not the
    /// selection's first row (L2 again), so both slips are caught.
    #[gpui::test]
    fn nemo_opens_the_pressed_row_not_the_selection(cx: &mut gpui::TestAppContext) {
        use geode_core::grid::selection::SelectKind;
        let (mut f, opened) = nemo_blotter(cx);
        // `V` on L3, then `k` to L2: rows 2..4 selected, cursor on L2.
        f.type_keys("j j j shift-v k");
        assert_eq!(f.selection_and_cursor(), (SelectKind::Rows, 2..4, 2));
        f.right_press("blotter-cell-3-2");
        assert_eq!(
            f.selection_and_cursor(),
            (SelectKind::Rows, 2..4, 2),
            "a press inside the selection keeps it and leaves the cursor on L2"
        );
        f.type_keys("enter");
        assert_eq!(*opened.borrow(), vec!["nemo://position/P6".to_string()]);
        f.close();
    }

    /// Link groups through the composition root: the roster startup builds
    /// (`add_bridge_modules` over the bridge's shared factories), the real
    /// blotter and pricer contents, the real drain, and the chooser's keys.
    /// A factory registered directly in a test proves only that a module
    /// can emit; this proves the shell startup assembles reaches each
    /// module's `emits`, `emission` and `watch_emission`, and that the
    /// frame handle it gives a tile reads the group that tile follows.
    ///
    /// The blotter (tile 1) emits into A; the pricer (tile 2) follows A and
    /// emits into B; a second blotter (tile 3) follows A. Moving the first
    /// blotter's cursor moves A's scope: the second blotter queries under
    /// it, the pricer hides the line A no longer selects, and the pricer's
    /// own cursor line names B's scope.
    #[gpui::test]
    fn the_production_blotter_emits_its_cursor_underlying_into_a_group_a_pricer_follows(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_core::link::{Group, Membership, underlying_of};
        use geode_core::snapshot::TestColumn;
        use geode_shell::tiling::WorkspaceIx;

        let (handle, rx) = DataHandle::for_tests();
        let (mut bridge, tx) = test_bridge_with_pricer(handle.clone(), test_pricer(&handle));
        let views = geode_core::view::ViewSpec::from_doc(&geode_core::config::merge_docs(
            "views",
            &[LayerDoc::builtin("views", FLAT_VIEW).unwrap()],
        ))
        .0;
        bridge.factory = Rc::new(BlotterFactory::new(
            handle,
            views,
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));

        // The roster, actions and keymap as `build_shell_services` wires
        // them.
        let mut services = test_shell_services();
        let mut roster = ModuleRoster::new();
        crate::add_bridge_modules(&mut roster, &bridge);
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

        // A session with a blotter on the left, the pricer, focused, in the
        // middle and a second blotter on the right; none is in a group.
        let mut table = geode_shell::session::to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
        );
        let ws1: toml::Table = r#"
            focused = 2
            [node]
            kind = "split"
            orientation = "horizontal"
            ratios = [0.34, 0.33, 0.33]
            [[node.children]]
            kind = "leaf"
            id = 1
            [[node.children]]
            kind = "leaf"
            id = 2
            [[node.children]]
            kind = "leaf"
            id = 3
            [tiles.1]
            module = "blotter"
            [tiles.1.state]
            view = "flat"
            [tiles.2]
            module = "pricer"
            [tiles.2.state]
            sheet = "book"
            [tiles.3]
            module = "blotter"
            [tiles.3.state]
            view = "flat"
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

        cx.update(gpui_component::init);
        cx.update(geode_blotter::init);
        cx.update(geode_pricer::init);
        let window = open_shell_window(cx, services);
        cx.update(|cx| attach(&bridge, window, cx));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let draw = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            vcx.run_until_parked();
        };
        draw(&mut vcx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let (blotter, pricer, follower) = (TileId(1), TileId(2), TileId(3));
        assert_eq!(
            shell.read_with(&vcx, |s, _| (
                s.occupant_kind(blotter),
                s.occupant_kind(pricer),
                s.occupant_kind(follower)
            )),
            (Some("blotter"), Some("pricer"), Some("blotter"))
        );
        let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

        // The next query `tile` sent. Another tile's queries met on the way
        // are kept for that tile's own call.
        let kept: RefCell<Vec<geode_data::QueryParams>> = RefCell::new(Vec::new());
        let next_query = |tile: TileId| -> geode_data::QueryParams {
            let held = kept.borrow().iter().position(|p| p.key == QueryKey(tile.0));
            if let Some(ix) = held {
                return kept.borrow_mut().remove(ix);
            }
            loop {
                match rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the tile asks for its rows")
                {
                    geode_data::Request::Query(p) if p.key == QueryKey(tile.0) => return p,
                    geode_data::Request::Query(p) => kept.borrow_mut().push(p),
                    _ => continue,
                }
            }
        };

        // Each blotter's first query, answered through the drain: the root,
        // then L1 on SPX and L2 on NDX.
        let dict = |a: &str, b: &str| TestColumn::Dict(vec![None, Some(a.into()), Some(b.into())]);
        let snapshot = Arc::new(geode_core::snapshot::Snapshot::for_tests(
            vec![
                (shell_blotter_meta("lhu"), dict("L1", "L2")),
                (shell_blotter_meta("underlying_ref"), dict("SPX", "NDX")),
                (
                    shell_blotter_meta("row_depth"),
                    TestColumn::I32(vec![0, 1, 1]),
                ),
                (
                    shell_blotter_meta("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
            ],
            1,
        ));
        let answer = |vcx: &mut gpui::VisualTestContext, query: &geode_data::QueryParams| {
            tx.try_send(DataEvent::Query(geode_core::query::QueryOutcome {
                key: query.key,
                tag: query.tag,
                snapshot: Ok(snapshot.clone()),
                submitted: std::time::Instant::now(),
            }))
            .unwrap();
            draw(vcx);
        };
        answer(&mut vcx, &next_query(blotter));
        let unlinked = next_query(follower);
        assert!(
            unlinked.scope.is_empty(),
            "fixture: in no group, the second blotter queries the whole book"
        );
        answer(&mut vcx, &unlinked);

        // Two lines in the pricer, one per underlying; the cursor rests on
        // the NDX line, the last typed.
        vcx.simulate_keystrokes("o");
        vcx.simulate_input("-5 SPX Z26 5000 C");
        vcx.simulate_keystrokes("enter");
        vcx.simulate_input("-5 NDX Z26 20000 C");
        vcx.simulate_keystrokes("enter escape");
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("pricer-hidden").is_none(),
            "fixture: the workspace's empty scope hides no line"
        );

        // The chooser, by its key, on each tile in turn.
        let link = |vcx: &mut gpui::VisualTestContext, row: &str| {
            vcx.simulate_keystrokes("alt-u");
            draw(vcx);
            vcx.simulate_input(row);
            vcx.simulate_keystrokes("enter");
            draw(vcx);
        };
        let membership =
            |vcx: &gpui::VisualTestContext, tile| frame.read_with(vcx, |f, _| f.membership(tile));
        let group_underlying = |vcx: &gpui::VisualTestContext, g| {
            frame.read_with(vcx, |f, _| {
                f.group_scope(g).sole("underlying_ref").map(str::to_owned)
            })
        };
        link(&mut vcx, "follow a");
        link(&mut vcx, "emit b");
        assert_eq!(
            membership(&vcx, pricer),
            Membership {
                follow: Some(Group::A),
                emit: Some(Group::B),
            },
            "the pricer's content answers `emits`, or it is offered no emit row"
        );
        assert_eq!(
            group_underlying(&vcx, Group::B).as_deref(),
            Some("NDX"),
            "joining pulls the pricer's emission: its cursor line's underlying"
        );
        vcx.simulate_keystrokes("alt-l");
        draw(&mut vcx);
        link(&mut vcx, "follow a");
        assert_eq!(
            membership(&vcx, follower),
            Membership {
                follow: Some(Group::A),
                emit: None,
            },
            "the blotter's content answers `follows`, or it is offered no follow row"
        );
        vcx.simulate_keystrokes("alt-h alt-h");
        draw(&mut vcx);
        link(&mut vcx, "emit a");
        assert_eq!(
            membership(&vcx, blotter),
            Membership {
                follow: None,
                emit: Some(Group::A),
            },
            "the blotter's content answers `emits`"
        );
        assert_eq!(
            group_underlying(&vcx, Group::A),
            None,
            "the cursor is on the root row, which names no underlying"
        );

        // Each module's header reads its chips through the frame handle
        // the shell gave its tile: a handle bound to the workspace alone
        // shows none.
        for chip in [
            "tile-link-1-A-emit",
            "tile-link-2-A-follow",
            "tile-link-2-B-emit",
            "tile-link-3-A-follow",
        ] {
            assert!(vcx.debug_bounds(chip).is_some(), "{chip} is painted");
        }

        // The first blotter's cursor moves to L1: the shell pulls its
        // emission and A's scope names SPX. The second blotter, reading A
        // through its own handle, sends a query scoped to SPX; answering it
        // releases the flip the group's change opened over A's followers.
        vcx.simulate_keystrokes("j");
        draw(&mut vcx);
        assert_eq!(group_underlying(&vcx, Group::A).as_deref(), Some("SPX"));
        let on_spx = next_query(follower);
        assert_eq!(
            underlying_of(&on_spx.scope),
            Some("SPX"),
            "the production follower queries under its group's scope"
        );
        answer(&mut vcx, &on_spx);
        // The pricer hides its NDX line. Its cursor rested there and can
        // rest only on a shown line, so it moves to the SPX line, which is
        // what the pricer now emits into B: the SPX line is the one shown.
        assert!(
            vcx.debug_bounds("pricer-hidden").is_some(),
            "the pricer applies the scope of the group it follows"
        );
        assert_eq!(
            group_underlying(&vcx, Group::B).as_deref(),
            Some("SPX"),
            "the NDX line is the hidden one"
        );

        // And on to L2: the lines change places.
        vcx.simulate_keystrokes("j");
        draw(&mut vcx);
        assert_eq!(group_underlying(&vcx, Group::A).as_deref(), Some("NDX"));
        let on_ndx = next_query(follower);
        assert_eq!(underlying_of(&on_ndx.scope), Some("NDX"));
        answer(&mut vcx, &on_ndx);
        assert!(vcx.debug_bounds("pricer-hidden").is_some());
        assert_eq!(
            group_underlying(&vcx, Group::B).as_deref(),
            Some("NDX"),
            "the SPX line is the hidden one now"
        );
        assert!(
            frame.read_with(&vcx, |f, _| f.view(WorkspaceIx::FIRST).scope().is_empty()),
            "the workspace's own scope is untouched"
        );
    }

    /// The next request on `f`'s handle that `pick` takes, skipping the
    /// rest (refreshes, catalog reads).
    fn next_request<T>(
        f: &ShellBlotter,
        mut pick: impl FnMut(geode_data::Request) -> Option<T>,
    ) -> T {
        loop {
            let r = f
                .requests
                .recv_timeout(Duration::from_secs(5))
                .expect("the request arrives");
            if let Some(t) = pick(r) {
                return t;
            }
        }
    }

    /// [`nemo_blotter`] with the bridge attached (so the shell's distinct
    /// requests reach the handle, as at startup), `V` over L2 (P9) and L3
    /// (P6), a right press on L3 inside the selection, `j enter` on
    /// "Move LHU…", and the `lhu` values `L4`, `L5` answered through the
    /// shell's `deliver_distinct` as the drain does, then `enter` on L4.
    /// Returns the fixture with the confirm open, and the bridge.
    fn move_lhu_to_the_confirm(cx: &mut gpui::TestAppContext) -> (ShellBlotter, Bridge) {
        let (mut f, _) = nemo_blotter(cx);
        let bridge = test_bridge(f.handle.clone());
        let window = f.window;
        // Outside the window's update: `attach` reads the window's root.
        gpui::TestAppContext::update(&f.vcx, |cx| attach(&bridge, window, cx));
        f.type_keys("j j j shift-v k");
        f.right_press("blotter-cell-3-2");
        f.type_keys("j enter");
        let (key, tag) = next_request(&f, |r| match r {
            geode_data::Request::Distinct(p) if p.column == "lhu" => Some((p.key, p.tag)),
            _ => None,
        });
        f.shell.update(&mut f.vcx, |s, cx| {
            s.deliver_distinct(
                DistinctOutcome {
                    key,
                    tag,
                    column: "lhu".into(),
                    values: Ok(vec![("L4".into(), 1), ("L5".into(), 2)]),
                },
                cx,
            )
        });
        f.type_keys("enter");
        assert!(
            f.vcx
                .debug_bounds("action-question-Move 2 positions to LHU L4?")
                .is_some(),
            "the confirm names both positions and the picked LHU"
        );
        (f, bridge)
    }

    /// Move LHU end to end through the production registration: the two
    /// selected positions, in selection order, go out as one command to the
    /// picked LHU after `y`, and the notice says sent, not done.
    #[gpui::test]
    fn move_lhu_sends_the_selected_positions_after_confirm(cx: &mut gpui::TestAppContext) {
        let (mut f, _bridge) = move_lhu_to_the_confirm(cx);
        f.type_keys("y");
        let params = next_request(&f, |r| match r {
            geode_data::Request::MoveLhu(p) => Some(p),
            _ => None,
        });
        assert_eq!(params.positions, vec!["P9".to_string(), "P6".to_string()]);
        assert_eq!(params.lhu, "L4");
        assert_eq!(
            f.shell
                .read_with(&f.vcx, |s, _| s.notice_for_test())
                .as_deref(),
            Some("moving 2 positions to LHU L4 \u{b7} sent")
        );
        f.close();
    }

    /// A yes the data handle cannot admit (here: shut down between the
    /// confirm opening and `y`) says so at once, with the handle's refusal;
    /// nothing else will answer it.
    #[gpui::test]
    fn a_move_the_data_handle_refuses_says_refused(cx: &mut gpui::TestAppContext) {
        let (mut f, _bridge) = move_lhu_to_the_confirm(cx);
        f.handle.shutdown();
        f.type_keys("y");
        assert_eq!(
            f.shell
                .read_with(&f.vcx, |s, _| s.notice_for_test())
                .as_deref(),
            Some("move to LHU L4 refused: the data service has stopped")
        );
        f.close();
    }

    /// `n` at the confirm sends no command and says nothing.
    #[gpui::test]
    fn no_to_the_confirm_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (mut f, _bridge) = move_lhu_to_the_confirm(cx);
        f.type_keys("n");
        let sent: Vec<_> = f
            .requests
            .try_iter()
            .filter(|r| matches!(r, geode_data::Request::MoveLhu(_)))
            .collect();
        assert!(sent.is_empty(), "{sent:?}");
        assert_eq!(f.shell.read_with(&f.vcx, |s, _| s.notice_for_test()), None);
        f.close();
    }

    /// [`test_bridge`] with `pricer` as its pricer factory, and the sender
    /// of its mailbox so a test can post data events to the real drain.
    fn test_bridge_with_pricer(
        handle: DataHandle,
        pricer: Rc<PricerFactory>,
    ) -> (Bridge, crate::events::Sender) {
        let (tx, rx) = crate::events::channel();
        let mut bridge = test_bridge(handle);
        bridge.pricer = pricer;
        bridge.events = rx;
        (bridge, tx)
    }

    /// A position-service answer posted to the real drain becomes the
    /// shell's status notice, in the shared wording.
    #[gpui::test]
    fn a_command_answer_reaches_the_status_notice(cx: &mut gpui::TestAppContext) {
        let (handle, _rx) = DataHandle::for_tests();
        let window = open_test_window(cx, test_shell_services());
        let (tx, rx) = crate::events::channel();
        let mut bridge = test_bridge(handle);
        bridge.events = rx;
        cx.update(|cx| attach(&bridge, window, cx));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let outcome = geode_core::positions::CommandOutcome {
            tag: 4,
            count: 3,
            lhu: "BK003_LHU2".into(),
            result: Ok(()),
        };
        let expected = geode_core::positions::outcome_notice(&outcome);
        tx.try_send(DataEvent::Command(outcome)).unwrap();
        vcx.run_until_parked();
        assert_eq!(
            shell
                .read_with(&vcx, |s, _| s.notice_for_test())
                .map(|n| n.to_string()),
            Some(expected)
        );
    }

    /// `o`, a line, `enter` typed through the shell into its focused tile
    /// (which leaves the next line's entry field open).
    fn type_a_line(vcx: &mut gpui::VisualTestContext, line: &str) {
        vcx.simulate_keystrokes("o");
        vcx.simulate_input(line);
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
    }

    /// Type `:line⏎` through the shell's command line into the focused
    /// tile.
    fn type_command(vcx: &mut gpui::VisualTestContext, line: &str) {
        vcx.simulate_keystrokes(":");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.simulate_input(line);
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
    }

    /// Run `line` as the tile's `:` command, the route the shell's command
    /// line takes (`TileContent::command`).
    fn run_command(
        vcx: &mut gpui::VisualTestContext,
        tile: &Entity<geode_pricer::tile::PricerTile>,
        line: &str,
    ) {
        let result = vcx.update(|window, cx| tile.update(cx, |t, cx| t.command(line, window, cx)));
        assert_eq!(result, Ok(()), ":{line}");
        vcx.run_until_parked();
    }

    /// The pricer's sheets are written through local publishes and
    /// forgets, and a tile may be waiting on any one of their outcomes (a
    /// load deferred behind a queued save has no timeout). The drain hands
    /// every `pricer_sheets` outcome — stored, failed, forgotten, forget
    /// failed — to the pricer factory, and no other dataset's.
    #[gpui::test]
    fn every_pricer_sheets_write_outcome_reaches_the_pricer_and_no_other_datasets_does(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_pricer::store::SheetStore as _;
        const SHEETS: &str = geode_pricer::core::PRICER_SHEETS_DATASET;
        let (handle, _rx) = DataHandle::for_tests();
        let store = MemorySheetStore::default();
        // Names are known only once an outcome confirms them, as in the
        // app's DuckDB store.
        store.set_confirming(true);
        let pricer = Rc::new(PricerFactory::new(
            handle.clone(),
            Rc::new(store.clone()),
            Views::builtin(),
            TemplateSet::builtin(),
            PricerSettings::default(),
        ));
        let (services, tiles) = with_a_pricer_tile_on(test_shell_services(), pricer.clone(), "a");
        let window = open_pricer_test_window(cx, services);
        let (bridge, tx) = test_bridge_with_pricer(handle, pricer);
        cx.update(|cx| attach(&bridge, window, cx));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tile = tiles.borrow()[0].clone();
        let post = |event: DataEvent, vcx: &mut gpui::VisualTestContext| {
            tx.try_send(event).unwrap();
            vcx.run_until_parked();
        };

        // A stored save makes the name known; another dataset's does not.
        post(
            DataEvent::LocalPublished {
                dataset: "sheets".into(),
                batch: "s1".into(),
                gen_id: 1,
            },
            &mut vcx,
        );
        assert!(
            !store.contains("s1"),
            "another dataset's save reached the pricer"
        );
        post(
            DataEvent::LocalPublished {
                dataset: SHEETS.into(),
                batch: "s1".into(),
                gen_id: 2,
            },
            &mut vcx,
        );
        assert!(
            store.contains("s1"),
            "the stored save did not reach the pricer"
        );

        // A forgotten sheet stops being known; another dataset's forget
        // leaves it.
        store.set_known(vec!["s2".into()]);
        post(
            DataEvent::Forgotten {
                dataset: "sheets".into(),
                batch: "s2".into(),
            },
            &mut vcx,
        );
        assert!(
            store.contains("s2"),
            "another dataset's forget reached the pricer"
        );
        post(
            DataEvent::Forgotten {
                dataset: SHEETS.into(),
                batch: "s2".into(),
            },
            &mut vcx,
        );
        assert!(!store.contains("s2"), "the forget did not reach the pricer");

        // A failed save resumes the load deferred behind it: the tile saves
        // `a`, moves off it, and asks for it back while the save is queued.
        type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
        assert_eq!(
            tile.read_with(&vcx, |t, _| t.sheet().len()),
            1,
            "fixture: typed"
        );
        vcx.executor().advance_clock(Duration::from_secs(2));
        vcx.run_until_parked();
        assert!(
            store.get("a").is_some(),
            "fixture: the idle save was queued"
        );
        run_command(&mut vcx, &tile, "new");
        run_command(&mut vcx, &tile, "e a");
        let loads = || store.loads().iter().filter(|(n, _, _)| n == "a").count();
        let before = loads();
        assert!(
            tile.read_with(&vcx, |t, _| t.is_loading()),
            "fixture: the load waits on the queued save"
        );
        post(
            DataEvent::LocalPublishFailed {
                dataset: "sheets".into(),
                batch: "a".into(),
                reason: "disk full".into(),
            },
            &mut vcx,
        );
        assert_eq!(
            loads(),
            before,
            "another dataset's failure resumed the load"
        );
        post(
            DataEvent::LocalPublishFailed {
                dataset: SHEETS.into(),
                batch: "a".into(),
                reason: "disk full".into(),
            },
            &mut vcx,
        );
        assert_eq!(
            loads(),
            before + 1,
            "the failed save did not resume the load"
        );
        assert!(!tile.read_with(&vcx, |t, _| t.is_loading()));

        // A failed forget gives the name back: `:rm` withholds it until the
        // forget is answered.
        store.set_known(vec!["x".into()]);
        let offered = |vcx: &gpui::VisualTestContext| {
            tile.read_with(vcx, |t, _| t.completions("e ", 2))
                .contains(&"x".to_string())
        };
        assert!(offered(&vcx), "fixture: a known sheet is offered");
        run_command(&mut vcx, &tile, "rm x");
        vcx.simulate_keystrokes("y");
        vcx.run_until_parked();
        assert_eq!(
            store.forgets(),
            vec!["x".to_string()],
            "fixture: `y` forgets"
        );
        assert!(!offered(&vcx), "fixture: a sheet being removed is withheld");
        post(
            DataEvent::ForgetFailed {
                dataset: "sheets".into(),
                batch: "x".into(),
                reason: "locked".into(),
            },
            &mut vcx,
        );
        assert!(
            !offered(&vcx),
            "another dataset's forget failure reached the pricer"
        );
        post(
            DataEvent::ForgetFailed {
                dataset: SHEETS.into(),
                batch: "x".into(),
                reason: "locked".into(),
            },
            &mut vcx,
        );
        assert!(offered(&vcx), "the failed forget did not reach the pricer");
    }

    /// Drive the test app until `done` holds, letting the data service's
    /// own threads run in real time between turns.
    fn wait_until(
        vcx: &mut gpui::VisualTestContext,
        what: &str,
        mut done: impl FnMut(&mut gpui::VisualTestContext) -> bool,
    ) {
        for _ in 0..2000 {
            vcx.run_until_parked();
            if done(vcx) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting until {what}");
    }

    /// The app as `main` builds it over `config` — the real `data_setup`,
    /// `start` and a DuckDB file at `db` — with one restored pricer tile
    /// on `sheet`, attached to its window.
    fn open_app_with_a_pricer_tile(
        cx: &mut gpui::TestAppContext,
        sources: ConfigSources,
        db: PathBuf,
        sheet: &str,
    ) -> (Bridge, WindowHandle<Root>, PricerTiles) {
        let config = Config::load(&sources);
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(std::sync::Arc::new(geode_pricing::MockPricer::new()));
        let setup = data_setup(
            &config,
            db,
            AdapterRegistry::default(),
            pricers,
            geode_data::VolModelRegistry::default(),
        )
        .expect("the demo layer declares datasets and views");
        let bridge =
            cx.update(|cx| start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let (services, tiles) = with_a_pricer_tile_on(
            test_shell_services_with_sources(sources),
            bridge.pricer.clone(),
            sheet,
        );
        let window = open_pricer_test_window(cx, services);
        cx.update(|cx| attach(&bridge, window, cx));
        (bridge, window, tiles)
    }

    /// A pending sheet save reaches the database before service shutdown when
    /// submission and storage succeed. Holding the tile past window teardown
    /// isolates the quit hook from the tile's own close-time flush.
    #[gpui::test]
    fn quitting_saves_every_unsaved_sheet_before_the_data_service_stops(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let sources = || ConfigSources {
            builtin: crate::builtin_layer(Some(dir.path())),
            desk: None,
            user: None,
        };
        let db = dir.path().join("geode.duckdb");
        let (bridge, window, tiles) =
            open_app_with_a_pricer_tile(cx, sources(), db.clone(), "book");
        cx.update(|cx| stop_at_quit(&bridge, cx));
        {
            let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let tile = tiles.borrow()[0].clone();
            wait_until(&mut vcx, "the empty sheet has loaded", |vcx| {
                tile.read_with(vcx, |t, _| !t.is_loading())
            });
            type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
        }
        cx.quit();
        // The quit hook's shutdown may still be finishing; this joins it.
        bridge.handle.shutdown();
        drop(tiles);
        drop(bridge);

        let config = Config::load(&sources());
        let setup = data_setup(
            &config,
            db,
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = DataService::spawn(setup.config, Arc::new(move |e| tx.send(e).is_ok()));
        assert!(
            handle
                .document(geode_core::query::DocumentParams {
                    key: QueryKey(7),
                    tag: 1,
                    submitted: std::time::Instant::now(),
                    dataset: PRICER_SHEETS_DATASET.into(),
                    document_key: vec!["book".into()],
                    as_of: AsOf::Live,
                })
                .is_ok()
        );
        let rows = loop {
            if let DataEvent::Query(o) = rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                break o.snapshot.unwrap().rows();
            }
        };
        assert_eq!(rows, 1, "the line typed before quit was stored");
        handle.shutdown();
    }

    /// Sheets persist in DuckDB: a line typed into one tile is saved by
    /// the idle write-behind, a second tile's `:e` reads it back, and after
    /// a restart over the same database a tile restoring the sheet loads
    /// the same line — with the sheet's name known from the catalog. The
    /// app's real wiring end to end: `builtin_layer`'s `pricer_sheets`,
    /// `start`'s store, the data service, and the drain's routing of the
    /// save's outcome and the load's answer.
    #[gpui::test]
    fn a_typed_sheet_is_stored_in_duckdb_and_loads_back_in_a_new_tile_and_after_a_restart(
        cx: &mut gpui::TestAppContext,
    ) {
        // Allow the test scheduler to wait for mailbox wakeups from the data
        // service's worker threads.
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let sources = || ConfigSources {
            builtin: crate::builtin_layer(Some(dir.path())),
            desk: None,
            user: None,
        };
        let db = dir.path().join("geode.duckdb");
        let loaded = |tile: &Entity<geode_pricer::tile::PricerTile>| {
            let tile = tile.clone();
            move |vcx: &mut gpui::VisualTestContext| {
                tile.read_with(vcx, |t, _| !t.is_loading() && t.sheet().len() == 1)
            }
        };

        let typed = {
            let (bridge, window, tiles) =
                open_app_with_a_pricer_tile(cx, sources(), db.clone(), "book");
            let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let first = tiles.borrow()[0].clone();
            wait_until(&mut vcx, "the empty sheet has loaded", |vcx| {
                first.read_with(vcx, |t, _| !t.is_loading())
            });
            type_a_line(&mut vcx, "-5 SPX Z26 5000 C");
            let typed = first.read_with(&vcx, |t, _| t.sheet().shorthand(0));
            assert_eq!(
                first.read_with(&vcx, |t, _| t.sheet().len()),
                1,
                "fixture: typed"
            );
            // The idle save publishes; the first tile then leaves the sheet
            // so a second one may open it.
            vcx.executor().advance_clock(Duration::from_secs(2));
            vcx.run_until_parked();
            // `escape` closes the next line's entry field `enter` left
            // open; `:new` is then typed through the shell's command line.
            vcx.simulate_keystrokes("escape");
            vcx.run_until_parked();
            type_command(&mut vcx, "new");
            assert_eq!(
                first.read_with(&vcx, |t, _| t.sheet().name.clone()),
                "untitled-1",
                "the typed :new reached the first tile"
            );
            let shell = window
                .read_with(&vcx, |root, _| root.view().clone().downcast::<ShellView>())
                .unwrap()
                .unwrap();
            vcx.update(|window, cx| {
                shell.update(cx, |s, cx| {
                    s.add_tile(
                        "pricer",
                        geode_shell::defaults::AddPlacement::Split(None),
                        None,
                        window,
                        cx,
                    )
                })
            });
            vcx.run_until_parked();
            let second = tiles.borrow().last().unwrap().clone();
            assert_ne!(second, first, "fixture: a second tile");
            type_command(&mut vcx, "e book");
            assert_eq!(
                second.read_with(&vcx, |t, _| t.sheet().name.clone()),
                "book",
                "fixture: `:e book` reached the second tile"
            );
            wait_until(
                &mut vcx,
                "the second tile loaded the sheet",
                loaded(&second),
            );
            assert_eq!(second.read_with(&vcx, |t, _| t.sheet().shorthand(0)), typed);
            // A restart: the window goes and the service stops, releasing
            // the database file.
            vcx.update(|window, _| window.remove_window());
            vcx.run_until_parked();
            bridge.handle.shutdown();
            vcx.run_until_parked();
            typed
        };

        let (bridge, window, tiles) = open_app_with_a_pricer_tile(cx, sources(), db, "book");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let restored = tiles.borrow()[0].clone();
        wait_until(
            &mut vcx,
            "the restored tile loaded the sheet",
            loaded(&restored),
        );
        assert_eq!(
            restored.read_with(&vcx, |t, _| t.sheet().shorthand(0)),
            typed
        );
        wait_until(&mut vcx, "the catalog names the stored sheet", |vcx| {
            restored
                .read_with(vcx, |t, _| t.completions("e ", 2))
                .contains(&"book".to_string())
        });
        // Stop the service's threads before the app is torn down: an event
        // they send later would wake the drain and hold its entities.
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        bridge.handle.shutdown();
        vcx.run_until_parked();
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
            geode_data::VolModelRegistry::default(),
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
            geode_data::VolModelRegistry::default(),
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

    /// Resolve `[vol] model` through the supplied registry the way the pricer
    /// resolves. An unknown name warns, names what the binary has, and leaves
    /// the model absent without preventing setup; no `[vol]` table means the
    /// demo model.
    #[test]
    fn an_unknown_vol_model_warns_and_leaves_the_model_absent() {
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

        let config = config_with_app("[vol]\nmodel = \"vendor\"\n");
        let setup = data_setup(
            &config,
            dir.path().join("a.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        assert_eq!(setup.config.vol.name, "vendor");
        assert!(setup.config.vol.model.is_none());
        let d = setup
            .diagnostics
            .iter()
            .find(|d| d.message.contains("vol model"))
            .unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(
            d.message,
            "vol model \"vendor\" ([vol] model) is not built into this binary \
             (have: ); every vol slice will say so"
        );
        assert_eq!(d.path.as_deref(), Some("app.vol.model"));

        // The default: no [vol] table at all resolves to the registered demo model.
        let mut vol_models = geode_data::VolModelRegistry::default();
        vol_models.register(Arc::new(geode_pricing::DemoVolModel));
        let config = config_with_app("");
        let setup = data_setup(
            &config,
            dir.path().join("b.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            vol_models,
        )
        .unwrap();
        assert_eq!(setup.config.vol.name, "demo");
        assert!(setup.config.vol.model.is_some());
        assert!(
            !setup
                .diagnostics
                .iter()
                .any(|d| d.message.contains("vol model")),
            "{:?}",
            setup.diagnostics
        );
    }

    /// Verify setup carries every local dataset name and excludes non-local ones.
    /// The routing test supplies its set directly, so it cannot prove this extraction.
    #[test]
    fn data_setup_names_every_local_dataset_and_only_those() {
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let mut vol_models = geode_data::VolModelRegistry::default();
        vol_models.register(Arc::new(geode_pricing::DemoVolModel));
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
            vol_models,
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Rc::new(["pricer_sheets".to_string()].into_iter().collect()),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let before = frame.read_with(&vcx, |f, _| f.data_version());

        tx.try_send(DataEvent::Published {
            dataset: "pricer_sheets".into(),
            batch: "untitled-1".into(),
            gen_id: 1,
            books: vec![None],
        })
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            frame.read_with(&vcx, |f, _| f.data_version()),
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
        assert_eq!(frame.read_with(&vcx, |f, _| f.data_version()), before + 1);
    }

    /// A shell holding one recording tile with a real bridge attached, so a
    /// keyed delivery test can send an event and read the occupant's log.
    struct RecordingTile {
        vcx: gpui::VisualTestContext,
        log: Rc<RefCell<Vec<Recorded>>>,
        events: crate::events::Sender,
        tile: TileId,
        /// Kept alive so the factories outlive the attached drain.
        _bridge: Bridge,
        /// Kept alive so the handle's channel stays open.
        _rx: std::sync::mpsc::Receiver<geode_data::Request>,
    }

    fn recording_tile_with_bridge(cx: &mut gpui::TestAppContext) -> RecordingTile {
        let (services, log) = test_shell_services_with_rec_roster();
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });

        // Opening the recording module creates the first tile in the empty
        // workspace. Its allocated id is 1; verify the occupant before using that
        // id as an outcome's destination.
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

        let (handle, rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, events) = crate::events::channel();
        let bridge = Bridge {
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        RecordingTile {
            vcx,
            log,
            events: tx,
            tile,
            _bridge: bridge,
            _rx: rx,
        }
    }

    /// A pricing outcome must reach the recording occupant, not merely survive
    /// the drain loop. Inspect its delivery log through the real bridge and shell.
    #[gpui::test]
    fn a_price_event_is_delivered_to_the_shell_as_delivery_price(cx: &mut gpui::TestAppContext) {
        let fixture = recording_tile_with_bridge(cx);
        let tile = fixture.tile;
        fixture
            .events
            .try_send(DataEvent::Price(geode_core::pricing::PriceOutcome {
                key: QueryKey(tile.0),
                tag: 5,
                submitted: std::time::Instant::now(),
                results: Vec::new(),
            }))
            .unwrap();
        fixture.vcx.run_until_parked();
        assert!(
            fixture.log.borrow().contains(&Recorded::Priced(tile, 5)),
            "the Price delivery must reach the tile's occupant: {:?}",
            fixture.log.borrow()
        );
    }

    /// A vol-slice outcome takes the same keyed route as a price: the drain
    /// must hand it to the shell as `Delivery::VolSlices` and the tile's
    /// occupant must see it.
    #[gpui::test]
    fn a_vol_slices_event_is_delivered_to_the_shell_as_delivery_vol_slices(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = recording_tile_with_bridge(cx);
        let tile = fixture.tile;
        fixture
            .events
            .try_send(DataEvent::VolSlices(geode_core::vol::VolSliceOutcome {
                key: QueryKey(tile.0),
                tag: 6,
                submitted: std::time::Instant::now(),
                results: Vec::new(),
            }))
            .unwrap();
        fixture.vcx.run_until_parked();
        assert!(
            fixture.log.borrow().contains(&Recorded::VolSliced(tile, 6)),
            "the VolSlices delivery must reach the tile's occupant: {:?}",
            fixture.log.borrow()
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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

    /// Shell services with one visible recording occupant restored into
    /// workspace 1, plus its delivery log for checking broadcast routing.
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

        // Restore the recording occupant through the public session format.
        let mut table = geode_shell::session::to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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
        assert_eq!(
            distinct_refused(cx, DataHandle::shutdown),
            Some(Err("the data service has stopped".to_string())),
            "a refused request must error the picker, not leave it loading forever"
        );
    }

    #[gpui::test]
    fn a_busy_distinct_request_says_try_again(cx: &mut gpui::TestAppContext) {
        assert_eq!(
            distinct_refused(cx, DataHandle::fill_for_tests),
            Some(Err("the data service is busy — try again".to_string()))
        );
    }

    /// The picker's values after a distinct request against a handle that
    /// `refuse` has made refuse.
    fn distinct_refused(
        cx: &mut gpui::TestAppContext,
        refuse: fn(&DataHandle),
    ) -> Option<Result<Vec<(String, u64)>, String>> {
        let window = open_test_window(cx, test_shell_services_with_pickable_book());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, _rx) = DataHandle::for_tests();
        refuse(&handle);
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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

        shell.read_with(&vcx, |s, _| {
            s.picker()
                .expect("the picker is still open — nothing here closes it")
                .values
                .clone()
        })
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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

    /// An edit that changes only `[blotter] stale_after` reaches the blotter
    /// and panel factories through the real reload route, whose shared cell
    /// every open tile reads — not only at the next views reload or restart.
    #[gpui::test]
    fn a_stale_after_only_reload_reaches_the_tiles(cx: &mut gpui::TestAppContext) {
        let builtin = vec![
            LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
            LayerDoc::builtin("app", "[blotter]\nstale_after = \"15m\"\n").unwrap(),
        ];
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: builtin.clone(),
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
        let panel = Rc::new(MarketDataFactory::new(
            handle.clone(),
            builtin_panel("cvi"),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = crate::events::channel();
        let bridge = Bridge {
            panels: vec![panel.clone()],
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory: factory.clone(),
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });

        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("app.toml"),
            "[blotter]\nstale_after = \"2m\"\n",
        )
        .unwrap();
        let candidate = Config::load(&ConfigSources {
            builtin,
            desk: None,
            user: Some(user.path().to_path_buf()),
        });
        shell.update(&mut vcx, |s, cx| s.apply_reload_for_test(candidate, cx));
        vcx.run_until_parked();
        assert_eq!(
            factory.stale_after(),
            Duration::from_secs(120),
            "the blotter tiles' shared threshold follows the reload"
        );
        assert_eq!(
            panel.stale_after(),
            Duration::from_secs(120),
            "every panel's shared threshold follows the reload"
        );
    }

    /// Reload named colors into the factory used by new and existing blotter tiles.
    #[gpui::test]
    fn a_reload_hands_the_factory_the_new_colours(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap(),
                LayerDoc::builtin("colors", "[delta]\nhue = 240\n").unwrap(),
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory: factory.clone(),
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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

    /// The pricer's reload observer hands the factory the config's named
    /// colors with the views, so a `colors.toml` edit repaints an open
    /// pricer tile's named columns.
    #[gpui::test]
    fn a_reload_hands_the_pricer_the_new_colours(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![
                LayerDoc::builtin("views", SLIM_VIEW).unwrap(),
                LayerDoc::builtin("colors", "[delta]\nhue = 240\n").unwrap(),
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
        assert!(
            bridge.pricer.colours().get("delta").is_none(),
            "fixture: the factory starts with no colours at all"
        );
        vcx.update(|_, cx| {
            let frame = shell.read(cx).frame().clone();
            frame.update(cx, |f, cx| {
                f.note_config_reloaded();
                cx.notify();
            });
        });
        vcx.run_until_parked();
        assert!(
            bridge.pricer.colours().get("delta").is_some(),
            "the reload must hand the pricer the config's colours"
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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

    /// Open the real diagnostics page through its factory and bridge. After
    /// the initial catalog request completes, changing frame as-of must
    /// produce a second request carrying the new value. A pending-bit assertion alone would
    /// not prove that the observer submits the request.
    #[gpui::test]
    fn an_as_of_change_on_the_visible_diagnostics_page_requests_a_second_catalog_with_the_new_as_of(
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

        // The real diagnostics page, created through the real factory, the
        // same door `main.rs`'s page roster and `open_page` use.
        let diagnostics_factory =
            DiagnosticsPageFactory::new(Arc::new(Ring::new(64)), Config::default());
        let occupant = vcx.update(|window, cx| {
            diagnostics_factory.create(
                None,
                geode_shell::frame::FrameRef::new(
                    frame.clone(),
                    geode_shell::tiling::WorkspaceIx::FIRST,
                ),
                diagnostics.clone(),
                Rc::new(|_, _, _| {}),
                window,
                cx,
            )
        });
        // Visibility starts the initial catalog request. Drain it before checking
        // that subsequent as-of changes submit the current frame value.
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
                f.shared_mut()
                    .set_as_of(AsOf::At(at - chrono::Duration::days(offset)));
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
            f.shared_mut().set_as_of(AsOf::At(later));
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
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
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
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

    /// `attach` describes each source with its dataset, so a tile asking
    /// about the dataset hears about the source. The name differs from the
    /// dataset here so a link built from the name would fail.
    #[gpui::test]
    fn describing_a_source_carries_its_dataset(cx: &mut gpui::TestAppContext) {
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: vec![(
                SourceSpec::directory("risk_src", "risk", vec!["/data/risk/*.csv".into()]),
                SourceShape::Directory,
            )],
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: Vec::new(),
            schema: Default::default(),
            underlyings: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        diagnostics.update(&mut vcx, |d, cx| {
            d.note_health(
                "risk_src",
                geode_shell::diagnostics::Health::Failed {
                    reason: "torn".into(),
                },
                "torn".into(),
                std::time::SystemTime::UNIX_EPOCH,
            );
            cx.notify();
        });
        let asked = diagnostics.read_with(&vcx, |d, _| d.health_for_datasets(&["risk"]));
        assert_eq!(asked.map(|h| h.source), Some("risk_src".to_string()));
    }

    /// A subscription's drops are filed on the load lane under
    /// `<source>:queue`, but the service emits them as the SOURCE's health
    /// (pinned in `geode-data` by
    /// `a_flooded_subscription_reports_its_drops_as_degraded_source_health`).
    /// That event, through the bridge's drain, reaches the tile chip's
    /// question: the source's dataset reads Degraded with the drop reason,
    /// and no source named `cvi:queue` appears.
    #[gpui::test]
    fn a_subscription_drop_report_reaches_its_datasets_tile_health(cx: &mut gpui::TestAppContext) {
        let window = open_test_window(cx, test_shell_services());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (handle, _rx) = DataHandle::for_tests();
        let (tx, rx) = crate::events::channel();
        let mut bridge = test_bridge(handle);
        bridge.events = rx;
        bridge.sources = vec![(
            SourceSpec {
                adapter: "demo_bus".into(),
                document: Some("cvi".into()),
                topics: vec!["cvi/>".into()],
                ..SourceSpec::directory("cvi", "cvi_params", Vec::new())
            },
            SourceShape::Subscribed,
        )];
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let reason = "12 messages dropped since 09:30:05";
        tx.try_send(DataEvent::Health {
            source: "cvi".into(),
            worst: geode_data::health::Health::Degraded {
                reason: reason.into(),
            },
            detail: format!(
                "{}: {reason}",
                geode_data::health::condition_key("cvi", geode_data::health::QUEUE)
            ),
        })
        .unwrap();
        vcx.run_until_parked();
        let asked = diagnostics
            .read_with(&vcx, |d, _| d.health_for_datasets(&["cvi_params"]))
            .expect("the dataset's tiles see the drop");
        assert_eq!(asked.source, "cvi");
        assert_eq!(
            asked.worst,
            geode_shell::diagnostics::Health::Degraded {
                reason: reason.into()
            }
        );
        assert_eq!(asked.reason, reason);
        assert!(
            diagnostics.read_with(&vcx, |d, _| !d.sources.contains_key("cvi:queue")),
            "no source is named after the condition key"
        );
    }

    /// A desk layer's `datasets.toml` whose body is `declaration` (any
    /// datasets text: a `pricer_sheets` or `pricer` redeclaration), and
    /// the sources loading it over the builtin layer.
    fn with_desk_pricer_sheets(dir: &Path, declaration: &str) -> (ConfigSources, PathBuf) {
        let desk = dir.join("desk");
        std::fs::create_dir_all(&desk).unwrap();
        let file = desk.join("datasets.toml");
        std::fs::write(&file, format!("config_version = 1\n{declaration}")).unwrap();
        let sources = ConfigSources {
            builtin: crate::builtin_layer(Some(dir)),
            desk: Some(desk),
            user: None,
        };
        (sources, file)
    }

    /// `pricer_sheets` with two same-typed columns swapped: reads by name
    /// would decode it, but positional inserts into an existing table would
    /// put each value in the other's column.
    fn reordered_pricer_sheets() -> String {
        geode_pricer::core::PRICER_SHEETS_DECLARATION
            .replace("columns.kind]", "columns.SWAP]")
            .replace("columns.template]", "columns.kind]")
            .replace("columns.SWAP]", "columns.template]")
    }

    fn pricer_sheets_pin_diagnostic(diags: &[Diagnostic]) -> Option<&Diagnostic> {
        diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.message.contains("pricer_sheets"))
    }

    /// The app owns `pricer_sheets`: its tables are created once and
    /// written positionally, so a desk or user redeclaration that differs
    /// is ignored, with an error naming the layer, its file and why.
    #[test]
    fn a_layer_redeclaring_pricer_sheets_differently_is_ignored_with_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, file) = with_desk_pricer_sheets(dir.path(), &reordered_pricer_sheets());
        let config = Config::load(&sources);
        let setup = data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        let builtin = crate::builtin_layer(None);
        let (alone, _) = SchemaSpec::from_doc(&geode_core::config::merge_docs(
            "datasets",
            &builtin
                .into_iter()
                .filter(|d| d.name == "datasets")
                .collect::<Vec<_>>(),
        ));
        assert!(
            setup.config.schema.dataset(PRICER_SHEETS_DATASET)
                == alone.dataset(PRICER_SHEETS_DATASET),
            "the service runs the app's declaration"
        );
        let d = pricer_sheets_pin_diagnostic(&setup.diagnostics).expect("an error diagnostic");
        assert_eq!(d.layer, Some(geode_core::config::Layer::Desk));
        assert_eq!(d.file.as_deref(), Some(file.as_path()));
        assert!(d.message.contains("ignored"), "{}", d.message);
        assert!(d.message.contains("wrong columns"), "{}", d.message);
    }

    #[test]
    fn a_layer_redeclaring_pricer_sheets_identically_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, _) =
            with_desk_pricer_sheets(dir.path(), geode_pricer::core::PRICER_SHEETS_DECLARATION);
        let config = Config::load(&sources);
        let setup = data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        assert!(
            pricer_sheets_pin_diagnostic(&setup.diagnostics).is_none(),
            "{:?}",
            setup.diagnostics
        );
    }

    /// The builtin layer declares the pricer's vocabulary as a computed
    /// dataset, so views, scopes and groupings can name its columns.
    #[test]
    fn the_pricer_dataset_is_declared_computed_in_every_build() {
        let config = Config::load(&ConfigSources {
            builtin: crate::builtin_layer(None),
            desk: None,
            user: None,
        });
        let (schema, _) = SchemaSpec::from_doc(config.doc("datasets").unwrap());
        let ds = schema
            .dataset(geode_pricer::core::PRICER_DATASET)
            .expect("declared by the builtin layer");
        assert!(ds.computed);
        assert_eq!(ds.columns.len(), 44);
    }

    /// The app owns `pricer` too: a differing redeclaration would change
    /// what a view, scope or grouping over the pricer means, so it is
    /// ignored with an error naming the layer and its file.
    #[test]
    fn a_layer_redeclaring_pricer_differently_is_ignored_with_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (sources, file) = with_desk_pricer_sheets(
            dir.path(),
            "[pricer]\ncomputed = false\n[pricer.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
        );
        let config = Config::load(&sources);
        let setup = data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        let ds = setup
            .config
            .schema
            .dataset(geode_pricer::core::PRICER_DATASET)
            .expect("the app's declaration");
        assert!(
            ds.computed && ds.columns.len() == 44,
            "the service runs the app's declaration"
        );
        let d = setup
            .diagnostics
            .iter()
            .find(|d| d.path.as_deref() == Some("datasets.pricer") && d.severity == Severity::Error)
            .expect("an error diagnostic");
        assert_eq!(d.layer, Some(geode_core::config::Layer::Desk));
        assert_eq!(d.file.as_deref(), Some(file.as_path()));
        assert!(d.message.contains("ignored"), "{}", d.message);
        assert!(
            d.message.contains("view, scope or grouping"),
            "{}",
            d.message
        );
    }

    /// A reload re-reads `datasets` for the blotter's validation schema:
    /// the redeclaration is ignored there too, and reported.
    #[gpui::test]
    fn a_reload_ignores_and_reports_a_redeclared_pricer_sheets(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (sources, _) = with_desk_pricer_sheets(dir.path(), &reordered_pricer_sheets());
        let window = open_test_window(cx, test_shell_services_with_sources(sources));
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
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let reported = diagnostics.read_with(&vcx, |d, _| {
            let diags: Vec<Diagnostic> =
                d.data_diagnostics.iter().map(|(_, d)| d.clone()).collect();
            pricer_sheets_pin_diagnostic(&diags).is_some()
        });
        assert!(
            reported,
            "the reload path must report the ignored redeclaration"
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
                PricerRegistry::default(),
                geode_data::VolModelRegistry::default()
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
            geode_data::VolModelRegistry::default(),
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
            geode_data::VolModelRegistry::default(),
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
            geode_data::VolModelRegistry::default(),
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

    /// Setup carries named colors and their reader diagnostics. load_views checks
    /// color references but does not retain color-definition errors.
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
                    "colors",
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
            geode_data::VolModelRegistry::default(),
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
                .any(|d| d.message.contains("color 'broken'")),
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

    /// Build the timeseries factory with the shared handle and startup colors.
    /// Its kind must match the roster/session identity used to restore the module.
    #[gpui::test]
    fn start_builds_a_timeseries_factory_beside_the_blotters(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: crate::builtin_layer(Some(dir.path())),
            ..ConfigSources::default()
        });
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(std::sync::Arc::new(geode_pricing::MockPricer::new()));
        let setup = data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            AdapterRegistry::default(),
            pricers,
            geode_data::VolModelRegistry::default(),
        )
        .expect("the demo layer declares datasets and views");
        let bridge =
            cx.update(|cx| start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        assert_eq!(bridge.timeseries.kind(), "timeseries");
        // Each factory must retain the kind used for roster and session lookup.
        assert_eq!(bridge.factory.kind(), "blotter");
        assert_eq!(
            bridge.panels.iter().map(|f| f.kind()).collect::<Vec<_>>(),
            ["cvi", "dividend"]
        );
        assert_eq!(
            bridge.pricer_key,
            Some(pricer_config_key(&config)),
            "the reload observer is seeded with the key the factory was built from"
        );
    }

    /// The demo composition over the builtin layer `main` assembles.
    fn demo_setup(dir: &std::path::Path, user: Option<&std::path::Path>) -> DataSetup {
        let config = Config::load(&ConfigSources {
            builtin: crate::builtin_layer(Some(dir)),
            desk: None,
            user: user.map(std::path::Path::to_path_buf),
        });
        data_setup(
            &config,
            dir.join("geode.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .expect("the demo layer declares datasets and views")
    }

    #[test]
    fn the_builtin_panels_load_clean_over_the_demo_datasets() {
        let dir = tempfile::tempdir().unwrap();
        let setup = demo_setup(dir.path(), None);
        assert!(
            setup.panel_diagnostics.is_empty(),
            "{:?}",
            setup.panel_diagnostics
        );
        assert_eq!(
            setup
                .panels
                .iter()
                .map(|p| p.kind.as_str())
                .collect::<Vec<_>>(),
            ["cvi", "dividend"]
        );
        assert_eq!(setup.panels[0], *builtin_panel("cvi"));
    }

    /// A saved blotter must never restore as a panel: a panel named after
    /// another module's kind is refused by name and the rest load.
    #[test]
    fn a_panel_named_after_another_module_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let blotter = geode_marketdata::core::BUILTIN_PANELS
            .split("\n[dividend]")
            .next()
            .unwrap()
            .replace("[cvi]", "[blotter]")
            .replace("cvi.", "blotter.");
        std::fs::write(
            user.path().join("panels.toml"),
            format!("config_version = 1\n{blotter}"),
        )
        .unwrap();
        let setup = demo_setup(dir.path(), Some(user.path()));
        assert_eq!(
            setup
                .panels
                .iter()
                .map(|p| p.kind.as_str())
                .collect::<Vec<_>>(),
            ["cvi", "dividend"]
        );
        assert_eq!(
            setup.panel_diagnostics.len(),
            1,
            "{:?}",
            setup.panel_diagnostics
        );
        assert_eq!(
            setup.panel_diagnostics[0].path.as_deref(),
            Some("panels.blotter")
        );
    }

    /// A desk config with views but without the market-data datasets (they
    /// are declared only by `--demo`): the builtin panels are refused by
    /// name and no market-data kind exists. With no `views` doc at all,
    /// `data_setup` is `None` and no data module starts, panels included.
    #[test]
    fn without_market_data_datasets_the_builtin_panels_are_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut builtin = crate::builtin_layer(None);
        builtin.push(LayerDoc::builtin("views", "").unwrap());
        let config = Config::load(&ConfigSources {
            builtin,
            ..ConfigSources::default()
        });
        let setup = data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .expect("the pricer's builtin datasets and an empty views doc");
        assert!(setup.panels.is_empty());
        assert_eq!(
            setup
                .panel_diagnostics
                .iter()
                .map(|d| d.path.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["panels.cvi.dataset", "panels.dividend.dataset"]
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
        fixture_with_reference(cx, Vec::new())
    }

    /// The catalog fixture over a startup schema declaring these reference
    /// datasets.
    fn fixture_with_reference(
        cx: &mut gpui::TestAppContext,
        reference_datasets: Vec<String>,
    ) -> CatalogFixture {
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
            panels: Vec::new(),
            timeseries: Rc::new(geode_timeseries::content::TimeseriesFactory::new(
                handle.clone(),
                NamedColours::default(),
            )),
            volslice: Rc::new(geode_volslice::VolsliceFactory::new(handle.clone())),
            classifications: Rc::new(geode_classifications::ClassificationsFactory::new(
                handle.clone(),
            )),
            pricer: test_pricer(&handle),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
            pricer_key: None,
            positions_configured: false,
            reference_datasets: reference_datasets.into_iter().map(|n| (n, 1)).collect(),
            schema: Default::default(),
            underlyings: Default::default(),
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

    /// The page's next reference read on the wire, skipping the catalog
    /// reads a watch also queues and the live cache's reads at attach.
    fn next_reference(f: &CatalogFixture) -> ReferenceParams {
        loop {
            match f.requests.try_recv().expect("reference request") {
                geode_data::Request::Reference(params) if params.key == REFERENCE_KEY => continue,
                geode_data::Request::Reference(params) => return params,
                geode_data::Request::Catalog(_) => continue,
                other => panic!("expected reference, got {other:?}"),
            }
        }
    }

    fn reference_answer(params: &ReferenceParams) -> DataEvent {
        DataEvent::Reference(ReferenceOutcome {
            key: params.key,
            tag: params.tag,
            dataset: params.dataset.clone(),
            as_of: params.as_of.clone(),
            table: Ok(None),
        })
    }

    fn fixture_diagnostics(
        f: &CatalogFixture,
        vcx: &mut gpui::VisualTestContext,
    ) -> (Entity<ShellView>, Entity<Diagnostics>) {
        let shell = f.window.root(vcx).unwrap().read_with(vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(vcx, |s, _| s.diagnostics().clone());
        (shell, diagnostics)
    }

    #[gpui::test]
    fn attach_hands_diagnostics_the_reference_datasets(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let (_, diagnostics) = fixture_diagnostics(&f, &mut vcx);
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.reference_datasets.clone()),
            vec!["underlyings".to_string()]
        );
    }

    /// The page's demand reaches the handle through the real observer,
    /// carrying the frame's as-of; only the latest tag's answer is stored.
    #[gpui::test]
    fn a_watched_reference_request_reaches_the_handle_and_its_answer_is_stored(
        cx: &mut gpui::TestAppContext,
    ) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let (shell, diagnostics) = fixture_diagnostics(&f, &mut vcx);
        let at = chrono::Utc::now() - chrono::Duration::days(3);
        let frame = shell.read_with(&vcx, |s, _| s.active_frame().clone());
        frame.update(&mut vcx, |fr, cx| {
            fr.shared_mut().set_as_of(AsOf::At(at));
            cx.notify();
        });
        vcx.run_until_parked();

        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            d.request_reference("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        let first = next_reference(&f);
        assert_eq!(first.key, DIAGNOSTICS_KEY);
        assert_eq!(first.dataset, "underlyings");
        assert_eq!(first.as_of, AsOf::At(at), "the frame's as-of is carried");

        diagnostics.update(&mut vcx, |d, cx| {
            d.request_reference("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        let second = next_reference(&f);
        assert!(second.tag > first.tag);

        // The first answer is superseded; only the latest request's counts.
        f.events.try_send(reference_answer(&first)).unwrap();
        vcx.run_until_parked();
        assert!(diagnostics.read_with(&vcx, |d, _| d.reference.is_none()));

        f.events.try_send(reference_answer(&second)).unwrap();
        vcx.run_until_parked();
        let stored = diagnostics.read_with(&vcx, |d, _| d.reference.clone());
        assert_eq!(stored.map(|o| o.tag), Some(second.tag));
    }

    #[gpui::test]
    fn a_busy_handle_refuses_a_reference_read_on_the_page(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let (_, diagnostics) = fixture_diagnostics(&f, &mut vcx);
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            d.request_reference("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        next_reference(&f);

        f.bridge.handle.fill_for_tests();
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_reference("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.reference_refusal.clone()),
            Some(geode_shell::diagnostics::ReferenceRefusal {
                dataset: "underlyings".to_string(),
                reason: "the data service is busy — press r to retry".to_string(),
                lane: ReferenceLane::Read,
            })
        );
    }

    /// A poll the handle refuses is recorded on the poll lane; the next
    /// poll that is submitted clears it.
    #[gpui::test]
    fn a_busy_handle_refuses_a_poll_on_the_page(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let (_, diagnostics) = fixture_diagnostics(&f, &mut vcx);
        f.bridge.handle.fill_for_tests();
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_poll("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.reference_refusal.clone()),
            Some(geode_shell::diagnostics::ReferenceRefusal {
                dataset: "underlyings".to_string(),
                reason: "the data service is busy — press r to retry".to_string(),
                lane: ReferenceLane::Poll,
            })
        );
        // Drain the filled queue so the retry is accepted.
        while f.requests.try_recv().is_ok() {}
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_poll("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(
            f.requests
                .try_iter()
                .any(|r| matches!(r, geode_data::Request::Poll { .. }))
        );
        assert!(
            diagnostics.read_with(&vcx, |d, _| d.reference_refusal.is_none()),
            "a submitted poll clears its refusal"
        );
    }

    /// The diagnostics entity outlives its window, so demand queued after the
    /// window closed must not reach the handle: neither a poll-now nor a
    /// page's reference read.
    #[gpui::test]
    fn a_closed_window_submits_no_poll_or_reference_read(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let (_, diagnostics) = fixture_diagnostics(&f, &mut vcx);
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        while f.requests.try_recv().is_ok() {}
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_poll("underlyings");
            d.request_reference("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        let late = f.requests.try_recv();
        assert!(
            late.is_err(),
            "nothing may be submitted for a closed window: {late:?}"
        );
    }

    /// Poll-now is explicit: it reaches the handle with no page watching.
    #[gpui::test]
    fn a_poll_request_reaches_the_handle(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let (_, diagnostics) = fixture_diagnostics(&f, &mut vcx);
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_poll("underlyings");
            cx.notify();
        });
        vcx.run_until_parked();
        // Skip the live cache's read at attach.
        assert!(next_live_reference(&f).is_some());
        match f.requests.try_recv() {
            Ok(geode_data::Request::Poll { dataset }) => assert_eq!(dataset, "underlyings"),
            other => panic!("expected a poll, got {other:?}"),
        }
    }

    /// The next live read the reference cache put on the wire, skipping
    /// catalog reads; `None` when the queue holds no such read.
    fn next_live_reference(f: &CatalogFixture) -> Option<ReferenceParams> {
        while let Ok(request) = f.requests.try_recv() {
            match request {
                geode_data::Request::Reference(params) if params.key == REFERENCE_KEY => {
                    return Some(params);
                }
                geode_data::Request::Catalog(_) => continue,
                other => panic!("expected a live reference read, got {other:?}"),
            }
        }
        None
    }

    /// An `underlyings` table keyed by its first column.
    fn underlyings_table(currency: &str) -> ReferenceTable {
        ReferenceTable {
            columns: vec!["name".into(), "currency".into()],
            rows: vec![vec![Some("SPX".into()), Some(currency.into())]],
            gen_id: 1,
            source_time: chrono::Utc::now(),
        }
    }

    fn live_answer(
        params: &ReferenceParams,
        table: Result<Option<ReferenceTable>, String>,
    ) -> DataEvent {
        DataEvent::Reference(ReferenceOutcome {
            key: params.key,
            tag: params.tag,
            dataset: params.dataset.clone(),
            as_of: params.as_of.clone(),
            table,
        })
    }

    fn published(dataset: &str) -> DataEvent {
        DataEvent::Published {
            dataset: dataset.into(),
            batch: "b".into(),
            gen_id: 2,
            books: vec![None],
        }
    }

    fn live_currency(vcx: &mut gpui::VisualTestContext) -> Option<String> {
        vcx.update(|_, cx| {
            cx.global::<ReferenceGlobal>()
                .0
                .lookup("underlyings", "SPX", "currency")
                .map(str::to_string)
        })
    }

    /// Counts every republish of the global an observing module would see.
    fn count_publishes(vcx: &mut gpui::VisualTestContext) -> Rc<Cell<usize>> {
        let count = Rc::new(Cell::new(0));
        let counter = count.clone();
        vcx.update(|_, cx| {
            cx.observe_global::<ReferenceGlobal>(move |_| counter.set(counter.get() + 1))
                .detach()
        });
        count
    }

    #[gpui::test]
    fn attach_reads_each_reference_dataset_live(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let read = next_live_reference(&f).expect("attach reads the dataset");
        assert_eq!(read.key, REFERENCE_KEY);
        assert_eq!(read.dataset, "underlyings");
        assert_eq!(read.as_of, AsOf::Live, "the global is live only");
        assert!(next_live_reference(&f).is_none(), "one read per dataset");
    }

    #[gpui::test]
    fn an_answer_publishes_the_global_and_a_repeat_does_not_notify(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let count = count_publishes(&mut vcx);
        let first = next_live_reference(&f).unwrap();
        f.events
            .try_send(live_answer(&first, Ok(Some(underlyings_table("USD")))))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(live_currency(&mut vcx).as_deref(), Some("USD"));
        assert_eq!(count.get(), 1);

        // A republish of the same rows is read again but changes nothing.
        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        let second = next_live_reference(&f).unwrap();
        f.events
            .try_send(live_answer(&second, Ok(Some(underlyings_table("USD")))))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(count.get(), 1, "an unchanged table wakes no observer");

        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        let third = next_live_reference(&f).unwrap();
        f.events
            .try_send(live_answer(&third, Ok(Some(underlyings_table("EUR")))))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(live_currency(&mut vcx).as_deref(), Some("EUR"));
        assert_eq!(count.get(), 2);
    }

    #[gpui::test]
    fn a_publish_of_a_reference_dataset_rereads_it(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let first = next_live_reference(&f).unwrap();
        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        let second = next_live_reference(&f).expect("a publish rereads");
        assert_eq!(second.dataset, "underlyings");
        assert_eq!(second.as_of, AsOf::Live);
        assert!(second.tag > first.tag);

        f.events.try_send(published("risk_snapshot")).unwrap();
        vcx.run_until_parked();
        assert!(
            next_live_reference(&f).is_none(),
            "a non-reference publish reads nothing"
        );
    }

    #[gpui::test]
    fn a_stale_tag_answer_is_ignored(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let count = count_publishes(&mut vcx);
        let first = next_live_reference(&f).unwrap();
        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        let second = next_live_reference(&f).unwrap();

        f.events
            .try_send(live_answer(&first, Ok(Some(underlyings_table("USD")))))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            live_currency(&mut vcx),
            None,
            "a superseded answer is dropped"
        );
        assert_eq!(count.get(), 0);

        f.events
            .try_send(live_answer(&second, Ok(Some(underlyings_table("EUR")))))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(live_currency(&mut vcx).as_deref(), Some("EUR"));
    }

    /// A refused reread keeps the last table and retries once, after the
    /// delay, however many refusals arrived meanwhile.
    #[gpui::test]
    fn a_busy_refusal_retries_and_keeps_the_cache(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let first = next_live_reference(&f).unwrap();
        f.events
            .try_send(live_answer(&first, Ok(Some(underlyings_table("USD")))))
            .unwrap();
        vcx.run_until_parked();

        // Two refused rereads: distinct batches, since the mailbox coalesces
        // a repeated (dataset, batch) publish into one event.
        f.bridge.handle.fill_for_tests();
        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        f.events
            .try_send(DataEvent::Published {
                dataset: "underlyings".into(),
                batch: "c".into(),
                gen_id: 3,
                books: vec![None],
            })
            .unwrap();
        vcx.run_until_parked();
        while f.requests.try_recv().is_ok() {}
        assert_eq!(live_currency(&mut vcx).as_deref(), Some("USD"));
        assert!(
            next_live_reference(&f).is_none(),
            "no retry before the delay"
        );

        vcx.executor().advance_clock(REFERENCE_RETRY_DELAY);
        vcx.run_until_parked();
        let retried = next_live_reference(&f).expect("the retry reads again");
        assert!(retried.tag > first.tag);
        assert!(next_live_reference(&f).is_none(), "one retry per dataset");
        f.events
            .try_send(live_answer(&retried, Ok(Some(underlyings_table("EUR")))))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(live_currency(&mut vcx).as_deref(), Some("EUR"));
    }

    /// A refused reread sends nothing, so the read already in flight stays
    /// the latest and its answer applies at once, without waiting for the retry.
    #[gpui::test]
    fn a_refused_reread_keeps_the_in_flight_answer_current(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let first = next_live_reference(&f).unwrap();
        f.bridge.handle.fill_for_tests();
        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        f.events
            .try_send(live_answer(&first, Ok(Some(underlyings_table("USD")))))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(live_currency(&mut vcx).as_deref(), Some("USD"));
    }

    /// A refused reread's timer must not submit once its window has closed:
    /// the drain still holds the cache until its next event, so the weak
    /// handle alone does not end the lane.
    #[gpui::test]
    fn a_refused_reread_does_not_retry_after_window_closure(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        assert!(next_live_reference(&f).is_some());
        f.bridge.handle.fill_for_tests();
        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        while f.requests.try_recv().is_ok() {}
        vcx.executor().advance_clock(REFERENCE_RETRY_DELAY);
        vcx.run_until_parked();
        let late = f.requests.try_recv();
        assert!(
            late.is_err(),
            "a reference retry must not submit after window closure: {late:?}"
        );
    }

    #[gpui::test]
    fn a_failed_read_keeps_the_last_table(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let count = count_publishes(&mut vcx);
        let first = next_live_reference(&f).unwrap();
        f.events
            .try_send(live_answer(&first, Ok(Some(underlyings_table("USD")))))
            .unwrap();
        vcx.run_until_parked();
        f.events.try_send(published("underlyings")).unwrap();
        vcx.run_until_parked();
        let second = next_live_reference(&f).unwrap();
        f.events
            .try_send(live_answer(&second, Err("disk I/O error".into())))
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(live_currency(&mut vcx).as_deref(), Some("USD"));
        assert_eq!(count.get(), 1, "a failure republishes nothing");
    }

    /// No generation at all removes the dataset's table; a second empty
    /// answer changes nothing and wakes no observer.
    #[gpui::test]
    fn an_empty_answer_removes_the_table(cx: &mut gpui::TestAppContext) {
        let f = fixture_with_reference(cx, vec!["underlyings".into()]);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let count = count_publishes(&mut vcx);
        let first = next_live_reference(&f).unwrap();
        f.events
            .try_send(live_answer(&first, Ok(Some(underlyings_table("USD")))))
            .unwrap();
        vcx.run_until_parked();
        for _ in 0..2 {
            f.events.try_send(published("underlyings")).unwrap();
            vcx.run_until_parked();
            let read = next_live_reference(&f).unwrap();
            f.events.try_send(live_answer(&read, Ok(None))).unwrap();
            vcx.run_until_parked();
            assert_eq!(live_currency(&mut vcx), None);
            assert_eq!(count.get(), 2, "set once on removal, never again");
        }
    }

    /// A failing dataset warns on entering failure only; a good read rearms it.
    #[test]
    fn a_failure_warns_once_until_a_read_succeeds() {
        let (handle, _requests) = DataHandle::for_tests();
        let cache = ReferenceCache::new(handle, &[("underlyings".into(), 1)]);
        assert!(cache.note_failed("underlyings"));
        assert!(!cache.note_failed("underlyings"));
        cache.note_succeeded("underlyings");
        assert!(cache.note_failed("underlyings"));
    }

    #[gpui::test]
    fn a_thread_stopped_event_reaches_the_status_segment(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.events
            .try_send(DataEvent::ThreadStopped {
                thread: "geode-ingest".into(),
                reason: "boom".into(),
            })
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.stopped_segment().map(|s| s.text.to_string())),
            Some("ingest stopped".to_string())
        );
    }

    /// The store-waiting segment through the production drain: shown by
    /// `StoreWaiting`, cleared by `StoreOpened`.
    #[gpui::test]
    fn store_events_show_then_clear_the_waiting_segment(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let segment = |vcx: &gpui::VisualTestContext| {
            diagnostics.read_with(vcx, |d, _| {
                d.store_waiting_segment().map(|s| s.text.to_string())
            })
        };
        f.events
            .try_send(DataEvent::StoreWaiting { holder: Some(812) })
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            segment(&vcx),
            Some("store: waiting for collector".to_string())
        );
        f.events.try_send(DataEvent::StoreOpened).unwrap();
        vcx.run_until_parked();
        assert_eq!(segment(&vcx), None, "the open clears the segment");
    }

    /// Both store events in one burst before the drain runs leave no
    /// segment. This pins the end state only; that the shared `Key::Store`
    /// coalesces the pair is `events::tests::a_store_opened_replaces_a_pending_store_waiting`.
    #[gpui::test]
    fn a_store_burst_that_ends_opened_shows_no_segment(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.events
            .try_send(DataEvent::StoreWaiting { holder: Some(812) })
            .unwrap();
        f.events.try_send(DataEvent::StoreOpened).unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.store_waiting_segment().cloned()),
            None
        );
    }

    /// A failed wait ends with the request loop's `ThreadStopped`: the
    /// waiting segment gives way to the stopped segment carrying the lease
    /// error.
    #[gpui::test]
    fn a_failed_store_wait_hands_the_bar_to_the_stopped_segment(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.events
            .try_send(DataEvent::StoreWaiting { holder: Some(812) })
            .unwrap();
        vcx.run_until_parked();
        assert!(diagnostics.read_with(&vcx, |d, _| d.store_waiting_segment().is_some()));
        let reason = "the background collector did not release the store within 15 s (PID 812)";
        f.events
            .try_send(DataEvent::ThreadStopped {
                thread: "geode-data".into(),
                reason: reason.into(),
            })
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.store_waiting_segment().cloned()),
            None
        );
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d
                .stopped_segment()
                .map(|s| s.detail.to_string())),
            Some(reason.to_string())
        );
    }

    #[gpui::test]
    fn refused_submissions_reach_the_status_summary(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.bridge.handle.fill_for_tests();
        assert_eq!(
            f.bridge.handle.query(geode_data::QueryParams {
                key: QueryKey(1),
                tag: 1,
                submitted: std::time::Instant::now(),
                view: "tree".into(),
                grouping: None,
                scope: Default::default(),
                as_of: AsOf::Live,
                max_depth: 1,
            }),
            Err(geode_data::Refusal::Busy)
        );
        let refused = f.bridge.handle.dropped_requests();
        assert!(refused > 0);
        // Any drained event carries the handle's refusal total with it.
        f.events.try_send(DataEvent::LoadEnded).unwrap();
        vcx.run_until_parked();
        assert_eq!(diagnostics.read_with(&vcx, |d, _| d.refused), refused);
        assert!(diagnostics.read_with(&vcx, |d, _| {
            d.summary().contains(&format!("{refused} refused"))
        }));
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

    /// A forget changes what the database holds without any `Published`,
    /// so the bridge must re-read a watched catalog on `Forgotten` or the
    /// diagnostics page keeps listing the deleted document.
    #[gpui::test]
    fn a_forgotten_document_rereads_a_watched_catalog(cx: &mut gpui::TestAppContext) {
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
        answer_catalog(&f, &first, 4);
        vcx.run_until_parked();
        assert!(f.requests.try_recv().is_err(), "no demand yet");
        f.events
            .try_send(DataEvent::Forgotten {
                dataset: "sheets".into(),
                batch: "a".into(),
            })
            .unwrap();
        vcx.run_until_parked();
        next_catalog(&f);
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
    fn a_stopped_service_does_not_retry_the_catalog(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.bridge.handle.shutdown();
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY * 3);
        vcx.run_until_parked();
        assert!(
            !diagnostics.read_with(&vcx, |d, _| d.pending_catalog_request()),
            "a stopped service keeps no retry demand; the stopped segment says why"
        );
    }

    #[gpui::test]
    fn a_reload_into_a_stopped_service_is_an_error_diagnostic(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap()],
            desk: None,
            user: None,
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (handle, _rx) = DataHandle::for_tests();
        handle.shutdown();
        let bridge = test_bridge(handle);
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        assert!(diagnostics.read_with(&vcx, |d, _| {
            d.data_diagnostics.iter().any(|(_, d)| {
                d.severity == Severity::Error
                    && d.message
                        == "the reloaded views did not reach the data service: \
                            the data service has stopped"
            })
        }));
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
        // Count wakes of the surviving entity: the retry timer itself must
        // not wake diagnostics for a closed window, apart from the
        // observer gate that keeps such a wake from submitting.
        let wakes = Rc::new(Cell::new(0u32));
        let _counter = cx.update({
            let wakes = wakes.clone();
            |cx| cx.observe(&diagnostics, move |_, _| wakes.set(wakes.get() + 1))
        });
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY);
        vcx.run_until_parked();
        assert_eq!(
            wakes.get(),
            0,
            "the retry timer must not wake a closed window's diagnostics"
        );
        let late = f.requests.try_recv();
        assert!(
            late.is_err(),
            "a retry must not submit after window closure: {late:?}"
        );
        // The demand the retry kept is still queued, and the entity outlives
        // the window: any later notify (a module, a stray tick) must not turn
        // it into a submission either.
        diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        let late = f.requests.try_recv();
        assert!(
            late.is_err(),
            "a notify after window closure must not submit: {late:?}"
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
        // This is the identity picker's request door: no diagnostics page exists.
        diagnostics.update(&mut vcx, |d, cx| {
            d.request_catalog();
            cx.notify();
        });
        vcx.run_until_parked();
        let first = next_catalog(&f);
        let at = chrono::Utc::now();
        frame.update(&mut vcx, |frame, cx| {
            frame.shared_mut().set_as_of(AsOf::At(at));
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

    /// The grid tile kinds the shared motion keys must reach. The
    /// diagnostics page is the fourth grid; `page_dispatch_counts` opens it.
    const GRID_KINDS: &[&str] = &["blotter", "cvi", "pricer"];

    /// DataTable key suppression for every grid module, once per test app.
    fn init_grid_modules(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(geode_blotter::init);
        cx.update(geode_marketdata::init);
        cx.update(geode_pricer::init);
    }

    /// A shell whose roster holds every grid factory and whose page roster
    /// holds the diagnostics page, wired as `main` wires them (actions,
    /// renames, fragments, the page toggle, builtin keymap, `user` as the
    /// user layer), with one restored tile of `kind` focused. Returns the
    /// keymap build's diagnostics beside the services.
    fn shell_with_one_grid_tile(
        kind: &str,
        user: Option<&str>,
    ) -> (ShellServices, Vec<Diagnostic>) {
        let mut services = test_shell_services();
        let (handle, _rx) = DataHandle::for_tests();
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        )));
        roster.add(Box::new(MarketDataFactory::new(
            handle.clone(),
            builtin_panel("cvi"),
            Duration::from_secs(900),
        )));
        roster.add(Box::new(PricerFactory::new(
            handle,
            Rc::new(MemorySheetStore::default()),
            Views::builtin(),
            TemplateSet::builtin(),
            PricerSettings::default(),
        )));
        let mut pages = geode_shell::module::PageRoster::new();
        pages.add(Box::new(DiagnosticsPageFactory::new(
            Arc::new(Ring::new(16)),
            services.config.clone(),
        )));
        roster.register_actions(&mut services.registry);
        let page_titles: Vec<(&str, &str)> = pages.entries().map(|e| (e.kind, e.title)).collect();
        register_page_actions(&mut services.registry, &page_titles);
        pages.register_actions(&mut services.registry);
        let (mut fragments, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        let (page_fragments, page_diags) = pages.keymap_fragments();
        assert!(page_diags.is_empty(), "{page_diags:?}");
        fragments.extend(page_fragments);
        let mut docs = vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()];
        if let Some(text) = user {
            docs.push(LayerDoc {
                layer: Layer::User,
                name: "keymap".into(),
                file: "user/keymap.toml".into(),
                table: text.parse().unwrap(),
            });
        }
        let layered = geode_shell::keymap::fragments::splice(&docs, &fragments);
        let (keymap, keymap_diags) = build_keymap(&layered, services.mod_alias, &services.registry);
        services.keymap = keymap;
        services.roster = roster;
        services.pages = pages;
        let mut table = geode_shell::session::to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
        );
        let ws1: toml::Table = format!(
            "focused = 1\n[node]\nkind = \"leaf\"\nid = 1\n[tiles.1]\nmodule = \"{kind}\"\n"
        )
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let restored = geode_shell::session::from_toml(&table).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        services.workspaces = restored.workspaces;
        services.restored_tiles = restored.tiles;
        (services, keymap_diags)
    }

    /// Type `keys` (gpui spelling, one entry per press) into a fresh shell
    /// hosting one `kind` tile; how many times each of `ids` was dispatched.
    fn dispatch_counts(
        cx: &mut gpui::TestAppContext,
        kind: &str,
        user: Option<&str>,
        keys: &[&str],
        ids: &[&str],
    ) -> Vec<usize> {
        use geode_shell::diagnostics::fnv1a;
        let (services, _) = shell_with_one_grid_tile(kind, user);
        let tail = services.action_tail.clone();
        let window = open_shell_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.run_until_parked();
        for key in keys {
            vcx.simulate_keystrokes(key);
        }
        vcx.run_until_parked();
        let tail = tail.lock().unwrap();
        ids.iter()
            .map(|id| {
                let h = fnv1a(id);
                tail.recent().filter(|x| *x == h).count()
            })
            .collect()
    }

    /// With a grid tile's action menu open, `j` and `down` send the shared
    /// menu step and never the grid's motion: the tile publishes `tilelist`
    /// over `mode == menu`, which the grid bindings' context excludes.
    #[gpui::test]
    fn a_menu_motion_steps_the_open_pricer_menu_not_its_grid(cx: &mut gpui::TestAppContext) {
        init_grid_modules(cx);
        for kind in ["pricer", "cvi"] {
            assert_eq!(
                dispatch_counts(
                    cx,
                    kind,
                    None,
                    &[".", "j", "down"],
                    &[
                        &format!("{}::menu", module_context(kind)),
                        "motion::menu_down",
                        "motion::down",
                    ],
                ),
                vec![1, 2, 0],
                "{kind}: the open menu takes j and down"
            );
        }
    }

    /// The module context (and action prefix) a grid kind's tile publishes.
    fn module_context(kind: &str) -> &'static str {
        match kind {
            "cvi" => "marketdata",
            "pricer" => "pricer",
            other => panic!("no menu fixture for {other}"),
        }
    }

    /// One user override of a shared motion, under the shipped context,
    /// reaches every grid tile through the real shell and keymap.
    #[gpui::test]
    fn a_shared_motion_override_reaches_each_grid_tile(cx: &mut gpui::TestAppContext) {
        use geode_shell::defaults::GRID_MOTION_CONTEXT;
        init_grid_modules(cx);
        let user = format!(
            "[[bindings]]\ncontext = \"{GRID_MOTION_CONTEXT}\"\n[bindings.keys]\n\"q\" = \"motion::down\"\n"
        );
        for kind in GRID_KINDS {
            let (_, diags) = shell_with_one_grid_tile(kind, Some(&user));
            assert!(diags.is_empty(), "{kind}: {diags:?}");
            assert_eq!(
                dispatch_counts(cx, kind, Some(&user), &["q"], &["motion::down"]),
                vec![1],
                "{kind}: the one override reaches this tile"
            );
        }
    }

    /// `"none"` on `j` under the shipped context unbinds it in every grid
    /// tile; the arrow keeps working, so the tile still takes motions.
    #[gpui::test]
    fn none_on_j_under_the_shared_context_unbinds_it_in_every_grid_tile(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::defaults::GRID_MOTION_CONTEXT;
        init_grid_modules(cx);
        let user = format!(
            "[[bindings]]\ncontext = \"{GRID_MOTION_CONTEXT}\"\n[bindings.keys]\n\"j\" = \"none\"\n"
        );
        for kind in GRID_KINDS {
            assert_eq!(
                dispatch_counts(cx, kind, None, &["j", "down"], &["motion::down"]),
                vec![2],
                "{kind}: fixture: j and down both move without the override"
            );
            assert_eq!(
                dispatch_counts(cx, kind, Some(&user), &["j", "down"], &["motion::down"]),
                vec![1],
                "{kind}: j silenced, down still moves"
            );
        }
    }

    /// An override written against the retired `blotter::down` keeps working
    /// in the blotter, only there, and the build warns naming both ids.
    #[gpui::test]
    fn an_old_blotter_down_override_still_moves_the_blotter_only_and_warns(
        cx: &mut gpui::TestAppContext,
    ) {
        init_grid_modules(cx);
        let user = "[[bindings]]\ncontext = \"blotter && mode == normal\"\n[bindings.keys]\n\"q\" = \"blotter::down\"\n";
        let (_, diags) = shell_with_one_grid_tile("blotter", Some(user));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0].message.contains("blotter::down") && diags[0].message.contains("motion::down"),
            "{}",
            diags[0].message
        );
        assert_eq!(
            dispatch_counts(cx, "blotter", Some(user), &["q"], &["motion::down"]),
            vec![1]
        );
        assert_reaches_no_other_grid_tile(cx, "blotter", user);
    }

    /// An old-id override under `own`'s context moves no other grid tile.
    fn assert_reaches_no_other_grid_tile(cx: &mut gpui::TestAppContext, own: &str, user: &str) {
        for kind in GRID_KINDS.iter().filter(|k| **k != own) {
            assert_eq!(
                dispatch_counts(cx, kind, Some(user), &["q"], &["motion::down"]),
                vec![0],
                "{kind}: the {own} override stays in its own context"
            );
        }
    }

    /// Rebinding Motion: down from the keybindings dialog while an old
    /// `blotter::down` rebind (its key plus a `"none"` over the `j` the
    /// blotter shipped) is displayed. The dialog's plan clears both and
    /// writes the shared grid context, so the new key moves every grid tile,
    /// the arrow still does, and `j` is silenced everywhere rather than in
    /// the blotter alone.
    #[gpui::test]
    fn a_motion_row_rebind_over_an_old_blotter_override_moves_every_grid_tile(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::keymap::parse_keystroke;
        use geode_shell::shell::keybindings_view::{derive_rows, rebind_plan};
        init_grid_modules(cx);
        let old = "config_version = 1\n\n[[bindings]]\ncontext = \"blotter && mode == normal\"\n\
                   [bindings.keys]\n\"n\" = \"blotter::down\"\n\"j\" = \"none\"\n";
        assert_eq!(
            dispatch_counts(cx, "blotter", Some(old), &["j", "n"], &["motion::down"]),
            vec![1],
            "fixture: j is dead in the blotter and the old n moves it"
        );
        let (services, _) = shell_with_one_grid_tile("blotter", Some(old));
        let rows = derive_rows(&services.registry, &services.keymap);
        let row = rows
            .iter()
            .find(|r| r.action.0 == "motion::down")
            .expect("a Motion: down row");
        let n = parse_keystroke("n", services.mod_alias).unwrap();
        let plan = rebind_plan(row, &[n]);

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keymap.toml"), old).unwrap();
        geode_shell::keymap_edit::apply_rebind_clearing(
            dir.path(),
            &plan.clear,
            plan.write.as_ref().expect("a new key is a write"),
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.path().join("keymap.toml")).unwrap();
        // The emptied blotter entry stays (its comments would); its keys go.
        assert!(!text.contains("blotter::down"), "{text}");
        assert_eq!(
            text.matches("\"none\"").count(),
            1,
            "one shared shadow: {text}"
        );

        for kind in GRID_KINDS {
            let (_, diags) = shell_with_one_grid_tile(kind, Some(&text));
            assert!(diags.is_empty(), "{kind}: {diags:?}");
            assert_eq!(
                dispatch_counts(
                    cx,
                    kind,
                    Some(&text),
                    &["n", "n", "j", "down"],
                    &["motion::down"]
                ),
                // Two presses of n, so a tile where n is dead but j lives
                // cannot score the same total.
                vec![3],
                "{kind}: n twice and down move, j is silenced\n{text}"
            );
        }
    }

    /// An override written against the retired `marketdata::down` keeps
    /// working in the market-data panel, only there, and warns naming both.
    #[gpui::test]
    fn an_old_marketdata_down_override_still_moves_the_panel_only_and_warns(
        cx: &mut gpui::TestAppContext,
    ) {
        init_grid_modules(cx);
        let user = "[[bindings]]\ncontext = \"marketdata && mode == normal\"\n[bindings.keys]\n\"q\" = \"marketdata::down\"\n";
        let (_, diags) = shell_with_one_grid_tile("cvi", Some(user));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0].message.contains("marketdata::down")
                && diags[0].message.contains("motion::down"),
            "{}",
            diags[0].message
        );
        assert_eq!(
            dispatch_counts(cx, "cvi", Some(user), &["q"], &["motion::down"]),
            vec![1]
        );
        assert_reaches_no_other_grid_tile(cx, "cvi", user);
    }

    /// An override written against the retired `pricer::down` keeps working
    /// in the pricer, only there, and warns naming both.
    #[gpui::test]
    fn an_old_pricer_down_override_still_moves_the_pricer_only_and_warns(
        cx: &mut gpui::TestAppContext,
    ) {
        init_grid_modules(cx);
        let user = "[[bindings]]\ncontext = \"pricer && mode == normal\"\n[bindings.keys]\n\"q\" = \"pricer::down\"\n";
        let (_, diags) = shell_with_one_grid_tile("pricer", Some(user));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0].message.contains("pricer::down") && diags[0].message.contains("motion::down"),
            "{}",
            diags[0].message
        );
        assert_eq!(
            dispatch_counts(cx, "pricer", Some(user), &["q"], &["motion::down"]),
            vec![1]
        );
        assert_reaches_no_other_grid_tile(cx, "pricer", user);
    }

    /// `keys` typed with the diagnostics page open over a blotter tile:
    /// `mod+d` first, then `keys`. The tile beneath is out of the context
    /// stack, so only the page can take them.
    fn page_dispatch_counts(
        cx: &mut gpui::TestAppContext,
        user: Option<&str>,
        keys: &[&str],
        ids: &[&str],
    ) -> Vec<usize> {
        let mut all = vec!["alt-d"];
        all.extend_from_slice(keys);
        dispatch_counts(cx, "blotter", user, &all, ids)
    }

    /// The real shell matcher routes section jumps and configuration views,
    /// while search owns its text and Enter/Escape return to the page.
    #[gpui::test]
    fn diagnostics_navigation_and_filter_exits_work_through_the_shell(
        cx: &mut gpui::TestAppContext,
    ) {
        init_grid_modules(cx);
        let (mut services, diags) = shell_with_one_grid_tile("blotter", None);
        assert!(diags.is_empty(), "{diags:?}");
        let config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "name = \"example\"\n").unwrap()],
            ..Default::default()
        });
        let mut pages = geode_shell::module::PageRoster::new();
        pages.add(Box::new(DiagnosticsPageFactory::new(
            Arc::new(Ring::new(16)),
            config,
        )));
        services.pages = pages;
        let window = open_shell_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            window.activate_window();
            let _ = window.draw(cx);
        });
        vcx.run_until_parked();
        vcx.simulate_keystrokes("alt-d");
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("diagnostics-page").is_some());
        vcx.simulate_keystrokes("g c");
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("diagnostics-config-values").is_some());
        vcx.simulate_keystrokes("ctrl-tab ctrl-tab");
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("diagnostics-row-0").is_some(),
            "effective configuration values are visible"
        );
        vcx.simulate_keystrokes("/");
        vcx.simulate_input("no-such-configuration-key");
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("diagnostics-row-0").is_none());
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("diagnostics-page").is_some(),
            "first Escape leaves search"
        );
        assert!(
            vcx.debug_bounds("diagnostics-row-0").is_some(),
            "Escape restores the prior filter"
        );
        vcx.simulate_keystrokes("/");
        vcx.simulate_input("app");
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        vcx.simulate_keystrokes("g p");
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("diagnostics-overlay-switch").is_some(),
            "Enter returned to navigation"
        );
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("diagnostics-page").is_none());
    }

    /// A real Escape over the open Levels popover closes the popover and
    /// keeps the page open with focus on it, whether the popover holds
    /// focus (its own `Cancel` binding dismisses it and hands focus back)
    /// or the page does (the shell offers `page::close` to the page, which
    /// consumes it). A second Escape then closes the page.
    #[gpui::test]
    fn escape_over_the_levels_popover_closes_it_and_keeps_the_page(cx: &mut gpui::TestAppContext) {
        init_grid_modules(cx);
        let (mut services, diags) = shell_with_one_grid_tile("blotter", None);
        assert!(diags.is_empty(), "{diags:?}");
        let config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "name = \"example\"\n").unwrap()],
            ..Default::default()
        });
        let mut pages = geode_shell::module::PageRoster::new();
        pages.add(Box::new(DiagnosticsPageFactory::new(
            Arc::new(Ring::new(16)),
            config,
        )));
        services.pages = pages;
        let window = open_shell_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|window, cx| {
            window.activate_window();
            let _ = window.draw(cx);
        });
        vcx.run_until_parked();
        vcx.simulate_keystrokes("alt-d g l");
        vcx.run_until_parked();
        let draw = |vcx: &mut gpui::VisualTestContext| {
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            vcx.run_until_parked();
        };
        let open_popover = |vcx: &mut gpui::VisualTestContext| {
            draw(vcx);
            let b = vcx
                .debug_bounds("diagnostics-levels-open")
                .expect("the Levels button is painted");
            vcx.simulate_click(b.center(), gpui::Modifiers::default());
            draw(vcx);
            assert!(
                vcx.debug_bounds("diagnostics-level-pick-ingest-debug")
                    .is_some(),
                "the popover is open"
            );
        };
        let page_focused = |vcx: &mut gpui::VisualTestContext| {
            let handle = shell
                .read_with(vcx, |s, cx| s.page_focus_handle_for_test(cx))
                .expect("the page is open");
            vcx.update(|window, _| handle.is_focused(window))
        };
        let page_open = |vcx: &gpui::VisualTestContext| {
            shell.read_with(vcx, |s, _| s.open_page_kind_for_test()) == Some("diagnostics")
        };

        // The popover holds focus: its own Cancel dismisses it.
        open_popover(&mut vcx);
        assert!(!page_focused(&mut vcx), "the open popover took focus");
        vcx.simulate_keystrokes("escape");
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("diagnostics-level-pick-ingest-debug")
                .is_none()
        );
        assert!(page_open(&vcx));
        assert!(page_focused(&mut vcx), "focus returned to the page");

        // The page holds focus: the shell's `page::close` reaches the page.
        open_popover(&mut vcx);
        let handle = shell
            .read_with(&vcx, |s, cx| s.page_focus_handle_for_test(cx))
            .unwrap();
        vcx.update(|window, cx| handle.focus(window, cx));
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("diagnostics-level-pick-ingest-debug")
                .is_some(),
            "the popover stays open with focus on the page"
        );
        vcx.simulate_keystrokes("escape");
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("diagnostics-level-pick-ingest-debug")
                .is_none()
        );
        assert!(page_open(&vcx));
        assert!(page_focused(&mut vcx));

        vcx.simulate_keystrokes("escape");
        draw(&mut vcx);
        assert!(
            !page_open(&vcx),
            "with nothing over it, Escape closes the page"
        );
    }

    /// One user override of a shared motion, under the shipped context,
    /// reaches the diagnostics page: it publishes `grid` like the tiles.
    #[gpui::test]
    fn a_shared_motion_override_reaches_the_diagnostics_page(cx: &mut gpui::TestAppContext) {
        use geode_shell::defaults::GRID_MOTION_CONTEXT;
        init_grid_modules(cx);
        let user = format!(
            "[[bindings]]\ncontext = \"{GRID_MOTION_CONTEXT}\"\n[bindings.keys]\n\"q\" = \"motion::down\"\n"
        );
        let (_, diags) = shell_with_one_grid_tile("blotter", Some(&user));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            page_dispatch_counts(
                cx,
                Some(&user),
                &["q", "j"],
                &["page::toggle_diagnostics", "motion::down"]
            ),
            vec![1, 2],
            "the page opened, and the override and the shipped j both reach it"
        );
    }

    /// An override written against the retired `diagnostics::down` keeps
    /// working on the diagnostics page, only there, and warns naming both.
    #[gpui::test]
    fn an_old_diagnostics_down_override_still_moves_the_page_only_and_warns(
        cx: &mut gpui::TestAppContext,
    ) {
        init_grid_modules(cx);
        let user = "[[bindings]]\ncontext = \"diagnostics\"\n[bindings.keys]\n\"q\" = \"diagnostics::down\"\n";
        let (_, diags) = shell_with_one_grid_tile("blotter", Some(user));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0].message.contains("diagnostics::down")
                && diags[0].message.contains("motion::down"),
            "{}",
            diags[0].message
        );
        assert_eq!(
            page_dispatch_counts(
                cx,
                Some(user),
                &["q"],
                &["page::toggle_diagnostics", "motion::down"]
            ),
            vec![1, 1]
        );
        assert_reaches_no_other_grid_tile(cx, "diagnostics", user);
    }
}
