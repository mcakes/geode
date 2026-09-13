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
/// file dropped into it shows up while you watch, and a `[cvi]` source
/// subscribing to `geode_app::demo_bus`'s CVI documents (market-data-
/// documents plan, Task 10) — a subscribed source, so none of the
/// directory-only keys `[demo]` carries apply to it.
pub fn layer(source_dir: &Path) -> Vec<LayerDoc> {
    let sources = format!(
        "config_version = 1\n[demo]\ndataset = \"risk_snapshot\"\npaths = [{:?}]\n\
         readiness = \"sentinel\"\npriority = \"latest_risk\"\npoll_interval = \"2s\"\n\
         pending_timeout = \"1m\"\nbatch_pattern = '^risk_\\d{{4}}-\\d{{2}}-\\d{{2}}_(?P<batch>.+)$'\n\
         [cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
         topics = [\"marketdata/cvi/>\"]\ncoalesce = \"500ms\"\nsource_time = \"receive\"\n\
         priority = \"latest_other\"\n",
        source_dir.join("*.csv").to_string_lossy()
    );
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
        assert_eq!(sources.len(), 2);
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
        let setup = crate::bridge::data_setup(
            &config,
            "/tmp/geode-demo/100000-42/geode.duckdb".into(),
            geode_data::adapter::AdapterRegistry::default(),
        )
        .expect("datasets + views are both present in the demo layer");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        let names: Vec<&str> = setup.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["tree", "wide"]);
        let wide = setup.views.iter().find(|v| v.name == "wide").unwrap();
        assert_eq!(wide.columns.len(), 100, "spec §6.6's 100-column view");
        // [demo] (a csv_dir source over risk_snapshot) and [cvi] (a
        // subscribed source over cvi_params, Task 10) — both parse with
        // no diagnostics, per this same fixture's own
        // the_demo_layer_declares_the_cvi_source.
        assert_eq!(setup.config.sources.len(), 2);
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
        let setup = crate::bridge::data_setup(
            &config,
            "/tmp/geode-demo/100000-42/geode.duckdb".into(),
            geode_data::adapter::AdapterRegistry::default(),
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
