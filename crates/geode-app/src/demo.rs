//! Generated inputs and builtin configuration for `--demo`.
//!
//! Source files and the demo database share a temporary directory keyed by
//! row count and seed. The compiled-in demo layer sits below desk and user
//! configuration, which can override its source and view definitions.

use geode_core::config::LayerDoc;
use std::path::{Path, PathBuf};

const SEED: u64 = 42;

pub fn demo_dir(rows: usize) -> PathBuf {
    std::env::temp_dir()
        .join("geode-demo")
        .join(format!("{rows}-{SEED}"))
}

/// Returns `dir/src`, generating source files when it has no directory entries.
/// Any existing entry suppresses generation; existing contents are reused
/// without checking completeness so repeated launches can use a warm store.
/// Directory creation and emission errors propagate to the caller.
pub fn ensure_emitted(dir: &Path, rows: usize) -> std::io::Result<PathBuf> {
    let src = dir.join("src");
    let populated = std::fs::read_dir(&src)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if !populated {
        std::fs::create_dir_all(&src)?;
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows,
            seed: SEED,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(&src);
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts)?;
    }
    Ok(src)
}

/// Builds the builtin demo layer from compiled-in example documents and
/// source paths rooted at `source_dir`.
///
/// The risk CSV source polls every two seconds using sentinel readiness.
/// CVI and dividend sources subscribe to `demo_bus`, coalescing updates per
/// key over 500 ms. `demo_kdb` and `demo_rest` fetch the `series` dataset;
/// the former offers a catalogue and the latter requires entered identities.
pub fn layer(source_dir: &Path) -> Vec<LayerDoc> {
    let sources = format!(
        "config_version = 1\n[demo]\ndataset = \"risk_snapshot\"\npaths = [{:?}]\n\
         readiness = \"sentinel\"\npriority = \"latest_risk\"\npoll_interval = \"2s\"\n\
         pending_timeout = \"1m\"\nbatch_pattern = '^risk_\\d{{4}}-\\d{{2}}-\\d{{2}}_(?P<batch>.+)$'\n\
         [cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
         topics = [\"marketdata/cvi/>\"]\ncoalesce = \"500ms\"\nsource_time = \"receive\"\n\
         priority = \"latest_other\"\n\
         [dividend]\nadapter = \"demo_bus\"\ndataset = \"dividend_schedule\"\n\
         document = \"dividend_schedule\"\ntopics = [\"marketdata/dividend/>\"]\n\
         coalesce = \"500ms\"\nsource_time = \"receive\"\npriority = \"latest_other\"\n\
         [demo_kdb]\nadapter = \"demo_kdb\"\ndataset = \"series\"\n\
         [demo_rest]\nadapter = \"demo_rest\"\ndataset = \"series\"\n",
        source_dir.join("*.csv").to_string_lossy()
    );
    // The `sophis` target publishes both document kinds through the same
    // adapter as their subscribed sources. Per-key addresses route uploads
    // back to those sources for an echo. Builtin documents are exempt from
    // the config-version check, so this generated document needs no header.
    let egress = "[sophis]\nadapter = \"demo_bus\"\n\
         [sophis.documents]\ncvi_params = \"marketdata/cvi/{key}\"\n\
         dividend_schedule = \"marketdata/dividend/{key}\"\n"
        .to_string();
    let docs = [
        (
            "app",
            include_str!("../../../examples/demo-config/app.toml").to_string(),
        ),
        (
            "datasets",
            include_str!("../../../examples/demo-config/datasets.toml").to_string(),
        ),
        (
            "dimensions",
            include_str!("../../../examples/demo-config/dimensions.toml").to_string(),
        ),
        ("egress", egress),
        (
            "groupings",
            include_str!("../../../examples/demo-config/groupings.toml").to_string(),
        ),
        ("sources", sources),
        (
            "views",
            include_str!("../../../examples/demo-config/views.toml").to_string(),
        ),
    ];
    docs.into_iter()
        .map(|(name, text)| LayerDoc::builtin(name, &text).expect("demo config is well-formed"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_layer_is_complete_and_points_sources_at_the_directory() {
        let docs = layer(std::path::Path::new("/tmp/geode-demo/100-42/src"));
        let names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "app",
                "datasets",
                "dimensions",
                "egress",
                "groupings",
                "sources",
                "views"
            ]
        );
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        let paths = sources.table["demo"]["paths"].as_array().unwrap();
        assert_eq!(paths[0].as_str(), Some("/tmp/geode-demo/100-42/src/*.csv"));
        assert_eq!(sources.table["demo"]["poll_interval"].as_str(), Some("2s"));
    }

    /// The `sophis` target preserves both document kinds and their per-key
    /// addresses through config parsing and resolves against `demo_bus`.
    #[test]
    fn the_demo_layers_egress_doc_reads_two_documents_for_sophis_and_resolves_against_demo_bus() {
        let docs = layer(std::path::Path::new("/tmp/geode-demo/100-42/src"));
        let egress = docs.iter().find(|d| d.name == "egress").unwrap();
        assert_eq!(egress.table["sophis"]["adapter"].as_str(), Some("demo_bus"));
        assert_eq!(
            egress.table["sophis"]["documents"]["cvi_params"].as_str(),
            Some("marketdata/cvi/{key}")
        );
        assert_eq!(
            egress.table["sophis"]["documents"]["dividend_schedule"].as_str(),
            Some("marketdata/dividend/{key}")
        );

        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (specs, d) =
            geode_core::egress_config::from_doc(config.doc("egress").unwrap(), &schema);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "sophis");
        assert_eq!(specs[0].adapter, "demo_bus");
        assert_eq!(
            specs[0].documents,
            vec![
                ("cvi_params".to_string(), "marketdata/cvi/{key}".to_string()),
                (
                    "dividend_schedule".to_string(),
                    "marketdata/dividend/{key}".to_string()
                ),
            ],
            "documents keep TOML order"
        );

        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        let (adapter, _feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        adapters.register(adapter);
        let (kept, d) = geode_data::egress::resolve(specs, &adapters);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "sophis");
    }

    /// The CVI source declares the subscription fields accepted by the
    /// source reader, and the full demo source document parses cleanly.
    #[test]
    fn the_demo_layer_declares_the_cvi_source() {
        let docs = layer(std::path::Path::new("/tmp/geode-demo/100-42/src"));
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        let cvi = &sources.table["cvi"];
        assert_eq!(cvi["adapter"].as_str(), Some("demo_bus"));
        assert_eq!(cvi["dataset"].as_str(), Some("cvi_params"));
        assert_eq!(cvi["document"].as_str(), Some("cvi_params"));
        assert_eq!(
            cvi["topics"].as_array().unwrap()[0].as_str(),
            Some("marketdata/cvi/>")
        );
        assert_eq!(cvi["coalesce"].as_str(), Some("500ms"));
        assert_eq!(cvi["priority"].as_str(), Some("latest_other"));

        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (sources, d) =
            geode_data::source::SourceSpec::from_doc(config.doc("sources").unwrap(), &schema);
        assert!(
            d.is_empty(),
            "SourceSpec::from_doc found diagnostics: {d:?}"
        );
        assert_eq!(sources.len(), 5);
    }

    /// The dividend source declares its document kind, topic, coalescing,
    /// and priority alongside the other demo sources without diagnostics.
    #[test]
    fn the_demo_layer_declares_the_dividend_source() {
        let docs = layer(std::path::Path::new("/tmp/geode-demo/100-42/src"));
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        let dividend = &sources.table["dividend"];
        assert_eq!(dividend["adapter"].as_str(), Some("demo_bus"));
        assert_eq!(dividend["dataset"].as_str(), Some("dividend_schedule"));
        assert_eq!(dividend["document"].as_str(), Some("dividend_schedule"));
        assert_eq!(
            dividend["topics"].as_array().unwrap()[0].as_str(),
            Some("marketdata/dividend/>")
        );
        assert_eq!(dividend["coalesce"].as_str(), Some("500ms"));
        assert_eq!(dividend["priority"].as_str(), Some("latest_other"));

        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (sources, d) =
            geode_data::source::SourceSpec::from_doc(config.doc("sources").unwrap(), &schema);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(sources.len(), 5, "demo, cvi, dividend, demo_kdb, demo_rest");
        assert!(sources.iter().any(|s| s.name == "dividend"));
    }

    /// Both timeseries sources resolve to the fetch shape over the demo
    /// `series` dataset and use their named adapters.
    #[test]
    fn the_demo_layer_declares_the_two_fetch_sources() {
        let docs = layer(std::path::Path::new("/tmp/geode-demo/100-42/src"));
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        assert_eq!(
            sources.table["demo_kdb"]["adapter"].as_str(),
            Some("demo_kdb")
        );
        assert_eq!(
            sources.table["demo_kdb"]["dataset"].as_str(),
            Some("series")
        );
        assert_eq!(
            sources.table["demo_rest"]["adapter"].as_str(),
            Some("demo_rest")
        );
        assert_eq!(
            sources.table["demo_rest"]["dataset"].as_str(),
            Some("series")
        );

        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (sources, d) =
            geode_data::source::SourceSpec::from_doc(config.doc("sources").unwrap(), &schema);
        assert!(
            d.is_empty(),
            "SourceSpec::from_doc found diagnostics: {d:?}"
        );
        assert_eq!(sources.len(), 5);
        let kdb = sources.iter().find(|s| s.name == "demo_kdb").unwrap();
        let rest = sources.iter().find(|s| s.name == "demo_rest").unwrap();
        assert_eq!(
            kdb.shape(&schema),
            geode_core::source_config::SourceShape::Fetch
        );
        assert_eq!(
            rest.shape(&schema),
            geode_core::source_config::SourceShape::Fetch
        );
    }

    #[test]
    fn emitting_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let src = ensure_emitted(dir.path(), 500).unwrap();
        let count = std::fs::read_dir(&src).unwrap().count();
        assert!(count > 2);
        let again = ensure_emitted(dir.path(), 500).unwrap();
        assert_eq!(again, src);
        assert_eq!(
            std::fs::read_dir(&src).unwrap().count(),
            count,
            "not emitted twice"
        );
    }
}

#[cfg(test)]
mod demo_config_integration {
    use super::*;
    use geode_core::config::{Config, ConfigSources};

    /// A registry holding the mock pricer, matching what `main.rs`
    /// builds before calling `data_setup` — without it, the demo
    /// config's implicit `[pricing] adapter = "mock"` default would
    /// resolve to nothing and every fixture below would gain a spurious
    /// "pricer" diagnostic.
    fn test_pricers() -> geode_data::PricerRegistry {
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(std::sync::Arc::new(geode_pricing::MockPricer::new()));
        pricers
    }

    /// Registers the demo bus required by the `sophis` egress target.
    ///
    /// Keep the returned feed alive through `data_setup`: egress resolution
    /// upgrades a weak sender reference, and a dropped feed makes the
    /// adapter report that it has no egress side.
    fn test_adapters() -> (
        geode_data::adapter::AdapterRegistry,
        geode_data::adapter::ChannelFeed,
    ) {
        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        let (adapter, feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        adapters.register(adapter);
        (adapters, feed)
    }

    /// The demo documents produce a usable data-service configuration
    /// through the normal config loader and setup path. Typed readers must
    /// skip `config_version` headers without reporting invalid entries.
    #[test]
    fn the_demo_layer_produces_a_servable_data_setup() {
        let src = std::path::Path::new("/tmp/geode-demo/100000-42/src");
        let config = Config::load(&ConfigSources {
            builtin: layer(src),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (adapters, _feed) = test_adapters();
        let setup = crate::bridge::data_setup(
            &config,
            "/tmp/geode-demo/100000-42/geode.duckdb".into(),
            adapters,
            test_pricers(),
        )
        .expect("datasets + views are both present in the demo layer");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        let names: Vec<&str> = setup.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["tree", "wide"]);
        let wide = setup.views.iter().find(|v| v.name == "wide").unwrap();
        assert_eq!(wide.columns.len(), 100, "spec §6.6's 100-column view");
        // All five source definitions survive setup: risk CSVs, CVI and
        // dividend subscriptions, and the two timeseries fetch adapters.
        assert_eq!(setup.config.sources.len(), 5);
        // Setup must carry the resolved egress target into the service config.
        assert_eq!(setup.config.egress.len(), 1);
        assert_eq!(setup.config.egress[0].name, "sophis");
        // Each document dataset must match its registered kind's columns.
        // A mismatch would fail the subscribed source's discovery health
        // when the service opens.
        geode_core::document::check_kind_against(
            &geode_documents::CviKind,
            setup.config.schema.dataset("cvi_params").unwrap(),
        )
        .unwrap();
        geode_core::document::check_kind_against(
            &geode_documents::DividendKind,
            setup.config.schema.dataset("dividend_schedule").unwrap(),
        )
        .unwrap();
    }

    /// The default timeseries source must name a declared fetch source.
    /// The shell uses this validation at startup; an invalid default would
    /// warn and require an explicit `@source` for added series.
    #[test]
    fn the_demo_layers_default_timeseries_source_names_one_of_its_own() {
        let src = std::path::Path::new("/tmp/geode-demo/100000-42/src");
        let config = Config::load(&ConfigSources {
            builtin: layer(src),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let diags = geode_shell::series::default_source_diagnostic(&config);
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// The compiled-in schema exposes `currency`, `model_code`, and `expiry`
    /// as categorical columns used by both the picker and dictionary interning.
    #[test]
    fn the_demo_schema_declares_currency_model_code_and_expiry_as_categorical() {
        let src = std::path::Path::new("/tmp/geode-demo/100000-42/src");
        let config = Config::load(&ConfigSources {
            builtin: layer(src),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (adapters, _feed) = test_adapters();
        let setup = crate::bridge::data_setup(
            &config,
            "/tmp/geode-demo/100000-42/geode.duckdb".into(),
            adapters,
            test_pricers(),
        )
        .expect("datasets + views are both present in the demo layer");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        let ds = setup
            .config
            .schema
            .dataset("risk_snapshot")
            .expect("risk_snapshot is declared");
        let categorical = ds.categorical_columns();
        for name in ["currency", "model_code", "expiry"] {
            assert!(
                categorical.contains(&name),
                "'{name}' must be categorical: {categorical:?}"
            );
        }
    }
}
