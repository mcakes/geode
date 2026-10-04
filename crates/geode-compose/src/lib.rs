//! The gpui-free half of Geode's composition root. The app and the
//! background collector build their store configuration from these
//! functions, so the two agree on what the store holds.

pub mod demo;
pub mod demo_bus;
pub mod demo_refdb;
pub mod demo_series;
pub mod paths;

pub use paths::{config_dirs, db_path, user_config_dir};

use geode_core::builtin::{
    PRICER_DATASET, PRICER_DATASET_DECLARATION, PRICER_SHEETS_DATASET, PRICER_SHEETS_DECLARATION,
};
use geode_core::config::{
    Config, DIMENSIONS_DOC, Diagnostic, Layer, LayerDoc, Severity, merge_docs,
};
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::DocumentKind;
use geode_core::schema::SchemaSpec;
use geode_data::adapter::{AdapterRegistry, ChannelAdapter, ChannelFeed};
use geode_data::documents::DocumentRegistry;
use geode_data::source::SourceSpec;
use geode_data::{DataServiceConfig, PricerConfig, VolConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The transport registry both binaries build. Without a demo directory it
/// is empty (real adapters register here when they exist); with one it
/// holds the demo bus, the two demo series transports, the demo reference
/// database and the demo position service, and returns the bus's feed for
/// whoever runs the demo producers.
pub fn adapters(demo_root: Option<&Path>) -> (AdapterRegistry, Option<ChannelFeed>) {
    let Some(root) = demo_root else {
        return (AdapterRegistry::default(), None);
    };
    let (adapter, feed) = ChannelAdapter::new("demo_bus");
    let mut adapters = AdapterRegistry::default();
    adapters.register(adapter);
    // Use the risk generator's seed for both series sources, exercising
    // catalogue and manual-identity discovery.
    adapters.register(demo_series::DemoSeries::new("demo_kdb", 42, true));
    adapters.register(demo_series::DemoSeries::new("demo_rest", 42, false));
    // The demo reference database behind the `refdb` snapshot source.
    adapters.register(demo_refdb::DemoRefDb::new(Duration::ZERO));
    // The demo position service rewrites the risk CSVs the demo source
    // polls; the demo layer's `positions.toml` names it.
    adapters.register(Arc::new(demo::DemoPositions::new(root.join("src"))));
    (adapters, Some(feed))
}

/// The builtin documents that decide what the store holds: the app's two
/// datasets (`pricer_sheets`, its local documents, and `pricer`, its
/// computed vocabulary) and, with a demo directory, the `--demo` layer.
/// The app's builtin layer is its own documents plus these; the collector's
/// is these alone, so both build the same schema and sources from the same
/// desk and user directories. `datasets` merges per dataset name, so a
/// demo, desk or user `datasets` doc adds its own datasets beside these.
pub fn builtin_data_layer(demo_root: Option<&Path>) -> Vec<LayerDoc> {
    let mut layer = vec![
        LayerDoc::builtin("datasets", PRICER_SHEETS_DECLARATION)
            .expect("PRICER_SHEETS_DECLARATION is well-formed TOML"),
        // The pricer's vocabulary as a computed dataset: views, scopes and
        // groupings see its columns; nothing stores or queries it.
        LayerDoc::builtin("datasets", PRICER_DATASET_DECLARATION)
            .expect("PRICER_DATASET_DECLARATION is well-formed TOML"),
    ];
    if let Some(root) = demo_root {
        layer.extend(demo::layer(&root.join("src")));
    }
    layer
}

/// The data engine's half of the service configuration: what decides the
/// store and keeps it current. `views` is empty, the pricer and vol model
/// are absent, and there are no egress targets or position service; the
/// app fills those in, the collector leaves them out. Nothing on the ingest
/// path reads views. Without a `datasets` document the schema is empty.
pub struct EngineSetup {
    pub config: DataServiceConfig,
    /// The kinds registered in `config.documents`, for checks that must see
    /// exactly what the service registers (the app's market-data panels).
    pub document_kinds: Vec<Arc<dyn DocumentKind>>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Infallible: problems become diagnostics, and without a `datasets` document
/// the schema is empty.
pub fn engine_setup(config: &Config, db_path: PathBuf, adapters: AdapterRegistry) -> EngineSetup {
    let mut diagnostics = Vec::new();
    let mut schema = match config.doc("datasets") {
        Some(doc) => {
            let (schema, d) = SchemaSpec::from_doc(doc);
            diagnostics.extend(d);
            schema
        }
        None => SchemaSpec::default(),
    };
    diagnostics.extend(pin_app_datasets(&mut schema, config));
    let (dimensions, d) = config
        .doc(DIMENSIONS_DOC)
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    diagnostics.extend(d);
    let (sources, d) = config
        .doc("sources")
        .map(|doc| SourceSpec::from_doc(doc, &schema))
        .unwrap_or_default();
    diagnostics.extend(d);
    let document_kinds = geode_documents::builtin_kinds();
    let mut documents = DocumentRegistry::default();
    for kind in &document_kinds {
        documents.register(Arc::clone(kind));
    }
    EngineSetup {
        config: DataServiceConfig {
            db_path,
            schema,
            views: Vec::new(),
            dimensions,
            query_workers: 4,
            sources,
            adapters,
            documents,
            clock: geode_core::clock::Clock::from_config(config).0,
            pricer: PricerConfig::default(),
            vol: VolConfig::default(),
            egress: Vec::new(),
            positions: None,
        },
        document_kinds,
        diagnostics,
    }
}

/// Keep the app's own datasets exactly as it declares them. `pricer_sheets`
/// tables are created once and written positionally (`insert … select *`),
/// so a desk or user layer redeclaring it with other columns, or the same
/// columns in another order, would put sheet values into the wrong columns
/// of an existing database while reads by name decode a plausible wrong
/// sheet. `pricer` is the vocabulary views, scopes and groupings compile
/// against, so a differing redeclaration would silently change what they
/// mean. Each is pinned by `pin_app_dataset`; a config with neither (no
/// builtin layer) is left alone.
pub fn pin_app_datasets(schema: &mut SchemaSpec, config: &Config) -> Vec<Diagnostic> {
    [
        pin_app_dataset(
            schema,
            config,
            PRICER_SHEETS_DATASET,
            PRICER_SHEETS_DECLARATION,
            "a different column list would put sheet values in the wrong columns",
        ),
        pin_app_dataset(
            schema,
            config,
            PRICER_DATASET,
            PRICER_DATASET_DECLARATION,
            "a differing declaration would change what a view, scope or grouping over the \
             pricer means",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Keep dataset `name` exactly as the app's `declaration` reads. A
/// redeclaration that differs (or is invalid, and so dropped from the
/// schema) is replaced by the builtin one and reported as an error naming
/// the layer and file, with `why` explaining what a differing declaration
/// would break; an identical one is accepted silently. A config with no
/// `name` at all (no builtin layer) is left alone.
fn pin_app_dataset(
    schema: &mut SchemaSpec,
    config: &Config,
    name: &str,
    declaration: &str,
    why: &str,
) -> Option<Diagnostic> {
    if !config
        .doc("datasets")
        .is_some_and(|d| d.value.contains_key(name))
    {
        return None;
    }
    let builtin = LayerDoc::builtin("datasets", declaration)
        .unwrap_or_else(|e| panic!("the app's `{name}` declaration is not well-formed TOML: {e}"));
    let (alone, _) = SchemaSpec::from_doc(&merge_docs("datasets", &[builtin]));
    let declared = alone
        .dataset(name)
        .unwrap_or_else(|| panic!("the app's declaration does not declare `{name}`"))
        .clone();
    let slot = schema.datasets.iter().position(|d| d.name == name);
    if slot.is_some_and(|i| schema.datasets[i] == declared) {
        return None;
    }
    match slot {
        Some(i) => schema.datasets[i] = declared,
        None => schema.datasets.push(declared),
    }
    let redeclared = config
        .layered_docs("datasets")
        .iter()
        .rev()
        .find(|d| d.layer != Layer::Builtin && d.table.contains_key(name));
    Some(Diagnostic {
        severity: Severity::Error,
        layer: redeclared.map(|d| d.layer),
        file: redeclared.map(|d| d.file.clone()),
        message: format!(
            "`{name}` is declared by the app; this redeclaration is ignored \
             (its columns are fixed: {why})"
        ),
        path: Some(format!("datasets.{name}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::ConfigSources;

    fn load(builtin: Vec<geode_core::config::LayerDoc>) -> Config {
        Config::load(&ConfigSources {
            builtin,
            ..ConfigSources::default()
        })
    }

    #[test]
    fn the_data_layer_declares_the_two_app_datasets_in_order() {
        let config = load(builtin_data_layer(None));
        let setup = engine_setup(&config, "/tmp/x.duckdb".into(), AdapterRegistry::default());
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        let names: Vec<&str> = setup
            .config
            .schema
            .datasets
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                geode_core::builtin::PRICER_SHEETS_DATASET,
                geode_core::builtin::PRICER_DATASET
            ]
        );
        assert!(setup.config.sources.is_empty());
    }

    /// The collector's half: no views, no pricer or vol model, no egress or
    /// position service, and the same document kinds the service registers.
    #[test]
    fn an_engine_setup_carries_only_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let config = load(builtin_data_layer(Some(dir.path())));
        let (adapters, _feed) = adapters(Some(dir.path()));
        let setup = engine_setup(&config, dir.path().join("geode.duckdb"), adapters);
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        assert!(!setup.config.sources.is_empty());
        assert!(setup.config.views.is_empty());
        assert!(setup.config.pricer.pricer.is_none());
        assert!(setup.config.vol.model.is_none());
        assert!(setup.config.egress.is_empty());
        assert!(setup.config.positions.is_none());
        let mut kinds: Vec<String> = setup
            .document_kinds
            .iter()
            .map(|k| k.name().to_string())
            .collect();
        kinds.sort();
        let mut registered = setup.config.documents.names();
        registered.sort();
        assert_eq!(kinds, registered);
    }

    #[test]
    fn without_a_datasets_doc_the_schema_is_empty() {
        let setup = engine_setup(
            &load(Vec::new()),
            "/tmp/x.duckdb".into(),
            AdapterRegistry::default(),
        );
        assert!(setup.config.schema.datasets.is_empty());
    }

    #[test]
    fn a_non_demo_build_registers_no_adapters() {
        let (registry, feed) = adapters(None);
        assert!(registry.names().is_empty(), "{:?}", registry.names());
        assert!(feed.is_none());
    }

    #[test]
    fn a_demo_build_registers_the_five_demo_transports_and_returns_the_bus_feed() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, feed) = adapters(Some(dir.path()));
        let mut names = registry.names();
        names.sort();
        assert_eq!(
            names,
            [
                "demo_bus",
                "demo_kdb",
                "demo_positions",
                "demo_refdb",
                "demo_rest"
            ]
        );
        assert!(feed.is_some());
    }

    /// The demo position service rewrites the risk CSVs the demo source
    /// polls, which `ensure_emitted` writes under `<demo_root>/src`; one
    /// registered at the demo root itself would find no position to move.
    #[test]
    fn the_demo_position_service_moves_positions_under_the_demo_source_directory() {
        let root = tempfile::tempdir().unwrap();
        let src = demo::ensure_emitted(root.path(), 100).unwrap();
        let csv = std::fs::read_dir(&src)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().ends_with("_BK003.csv"))
            .unwrap();
        let text = std::fs::read_to_string(&csv).unwrap();
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap().split(',').collect();
        let at = |name| header.iter().position(|c| *c == name).unwrap();
        let first: Vec<&str> = lines.next().unwrap().split(',').collect();
        let position = first[at("PositionRef")].to_string();
        let target = "BK007_LHU2";
        assert_ne!(first[at("LHU")], target);

        let (registry, _feed) = adapters(Some(root.path()));
        let mut commands = registry
            .get(demo::DEMO_POSITIONS)
            .and_then(|a| a.positions())
            .expect("the demo build registers a position service");
        commands
            .move_lhu(std::slice::from_ref(&position), target)
            .unwrap();

        let after = std::fs::read_to_string(&csv).unwrap();
        let moved = after
            .lines()
            .skip(1)
            .map(|l| l.split(',').collect::<Vec<_>>())
            .find(|f| f[at("PositionRef")] == position)
            .unwrap();
        assert_eq!(moved[at("LHU")], target);
    }
}
