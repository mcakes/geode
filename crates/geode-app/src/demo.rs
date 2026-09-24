//! `--demo` (Phase 3 spec §7.1): boot on generated data with no real
//! source. Emits the generator's directory once per row count, layers
//! the compiled-in demo config under any desk/user config, and points
//! the database at the same temp directory so a demo never touches a
//! real one.

use geode_core::config::LayerDoc;
use std::path::{Path, PathBuf};

const SEED: u64 = 42;

pub fn demo_dir(rows: usize) -> PathBuf {
    std::env::temp_dir()
        .join("geode-demo")
        .join(format!("{rows}-{SEED}"))
}

/// The source directory, emitted if absent. Idempotent: a directory
/// with files in it is reused, so a second run is a warm start.
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

/// The demo layer: every doc under `examples/demo-config`, compiled in,
/// plus a `sources` doc over `source_dir` polled every two seconds so a
/// file dropped into it shows up while you watch, a `[cvi]` source
/// subscribing to `geode_app::demo_bus`'s CVI documents (market-data-
/// documents plan, Task 10), a `[dividend]` source subscribing to its
/// dividend-schedule documents (dividend-schedule plan, Task 11) — both
/// subscribed sources, so none of the directory-only keys `[demo]` carries
/// applies to either — and two fetch sources, `demo_kdb` and `demo_rest`,
/// over the timeseries demo's `series` dataset (Task 9:
/// `geode_app::demo_series::DemoSeries`), one with a catalogue and one
/// without.
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
    // The demo layer's own egress target (Task 8; egress spec §4, §10
    // amendment 1): `[sophis]` on `demo_bus` — the same adapter the
    // `[cvi]`/`[dividend]` sources above already subscribe through —
    // accepting both document kinds this crate builds, at an address per
    // document key so the demo `ChannelAdapter`'s echo lands back on the
    // same topic its source subscribes to. No `config_version` header
    // (unlike `sources`, above): `egress_config::from_doc` reads every
    // top-level key as a target, with no such key skipped — builtin docs
    // are exempt from the version check regardless (`config/mod.rs`), so
    // there is nothing this doc needs the key for.
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

    /// Task 8 (egress spec §4, §10 amendments 1/2): the demo layer's own
    /// `[sophis]` egress target parses into two documents via
    /// `egress_config::from_doc`, with the per-key address shape
    /// amendment 1 settled on, and survives `egress::resolve` once
    /// `demo_bus` is registered — the same adapter the demo bus's
    /// `[cvi]`/`[dividend]` sources already name.
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

    /// Task 10 (the demo bus): the `[cvi]` source is declared with every
    /// field a subscribed source needs, and — the reader's own
    /// vocabulary, not just well-formed TOML — `SourceSpec::from_doc`
    /// accepts both `[demo]` and `[cvi]` with no diagnostics at all.
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

    /// Task 11 (the demo bus's second producer): the `[dividend]` source
    /// is declared with every field a subscribed source needs, alongside
    /// `[demo]` and `[cvi]` — same shape as
    /// `the_demo_layer_declares_the_cvi_source`, over the dividend
    /// vocabulary.
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

    /// Task 9 (the demo adapter): `demo_kdb` and `demo_rest` are declared
    /// over the `series` dataset, with no diagnostics at all, and both
    /// name a fetch shape once `SourceSpec::shape` sees the demo schema
    /// (`series` is `family = "series"`, and both name an adapter other
    /// than the directory default).
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

    /// A registry holding `demo_bus`, matching what `main.rs` registers
    /// under `--demo` before calling `data_setup` — without it, the demo
    /// layer's own `[sophis]` egress target (egress spec §4) would be
    /// dropped with an "adapter … is not in this build" diagnostic and
    /// every fixture below asserting `setup.diagnostics.is_empty()` would
    /// fail on that spurious entry.
    ///
    /// Returns the `ChannelFeed` too, and the caller must keep it alive
    /// through `data_setup`/`egress::resolve`: `ChannelAdapter::egress`
    /// upgrades a `Weak` reference to the feed's sender, so a feed
    /// dropped before `resolve` runs answers "has no egress side" —
    /// exactly the failure this helper exists to avoid.
    fn test_adapters() -> (
        geode_data::adapter::AdapterRegistry,
        geode_data::adapter::ChannelFeed,
    ) {
        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        let (adapter, feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        adapters.register(adapter);
        (adapters, feed)
    }

    /// Self-review / headless verification (Task 8): the demo layer's
    /// docs are not just individually well-formed TOML (`layer` already
    /// panics otherwise) — merged through the real `Config` loader and
    /// fed to `data_setup`, they produce a servable `DataServiceConfig`
    /// with the same views the blotter is meant to run.
    ///
    /// **Fix round 1, Finding 2:** `ViewSpec::from_doc` and
    /// `DerivedDimensions::from_doc` used to iterate every top-level key
    /// of the doc without skipping `config_version` (unlike
    /// `GroupingSlots::from_doc`, which already did), so `views.toml`/
    /// `dimensions.toml`'s `config_version = 1` header — the same
    /// convention every other config doc uses — produced one spurious
    /// "not a table" diagnostic apiece. Both `from_doc`s now skip it
    /// (`crates/geode-core/src/{view,dimensions}.rs`), so this asserts
    /// zero diagnostics rather than the two it used to tolerate.
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
        // [demo] (a csv_dir source over risk_snapshot), [cvi] (a
        // subscribed source over cvi_params, Task 10) and [dividend] (a
        // subscribed source over cvi_params, Task 10), [dividend] (a
        // subscribed source over dividend_schedule, Task 11), and the two
        // fetch sources [demo_kdb]/[demo_rest] over series (Task 9) — all
        // five parse with no diagnostics, per this same fixture's own
        // the_demo_layer_declares_the_cvi_source,
        // the_demo_layer_declares_the_dividend_source and
        // the_demo_layer_declares_the_two_fetch_sources.
        assert_eq!(setup.config.sources.len(), 5);
        // Task 8: the demo layer's own `[sophis]` egress target must
        // reach `DataServiceConfig.egress` — the field `data_setup`
        // used to hardcode to `Vec::new()` before this task read and
        // resolved `egress.toml` (this same fixture's own
        // the_demo_layers_egress_doc_reads_two_documents_for_sophis_and_resolves_against_demo_bus
        // pins `from_doc`/`resolve` in isolation; this pins the wiring).
        assert_eq!(setup.config.egress.len(), 1);
        assert_eq!(setup.config.egress[0].name, "sophis");
        // The demo schema's own `cvi_params` and `dividend_schedule`
        // datasets must each agree with their built-in kind's column set
        // (spec §6.4) — the same check `DataService::open` runs per
        // subscribed source at open time, pinned here so a demo-config
        // edit that drifts the two apart fails this fixture rather than
        // only ever failing silently as a discovery-lane `Failed` a
        // trader has to notice at runtime.
        // Task 11 carry-in: the demo schema's own `cvi_params` dataset
        // must agree with the built-in `CviKind`'s column set (spec
        // §6.4) — the same check `DataService::open` runs per subscribed
        // source at open time, pinned here so a demo-config edit that
        // drifts the two apart fails this fixture rather than only ever
        // failing silently as a discovery-lane `Failed` a trader has to
        // notice at runtime.
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

    /// Task 5 (timeseries spec §10): the demo layer's own `[timeseries]
    /// default_source = "demo_kdb"` must name a fetch source the demo
    /// layer actually declares. `geode_shell::series::
    /// default_source_diagnostic` is what `ShellView::new` folds into
    /// its startup diagnostics, so anything else here would boot the
    /// demo with a config warning about the demo's own config — and one
    /// naming a source `:add` could not use, since a default that
    /// resolves to nothing makes every `@source` explicit again.
    ///
    /// The one assertion that catches a renamed `[demo_kdb]` source, a
    /// renamed key, or a `default_source` typed as anything but a
    /// string; the sources themselves are pinned by
    /// `the_demo_layer_declares_the_two_fetch_sources` next door.
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

    /// Task 1 (Phase 4 spec §3.3): `currency`, `model_code` and `expiry`
    /// are carried dimensions in the demo schema now, not lookup-only
    /// attributes — `categorical_columns` is what the picker and the
    /// interning loop both read, and this is the compiled-in doc's own
    /// promise that the flag reaches them.
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
