//! Generated inputs and builtin configuration for `--demo`.
//!
//! Source files and the demo database share a temporary directory keyed by
//! row count and seed. The compiled-in demo layer sits below desk and user
//! configuration, which can override its source and view definitions.

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use geode_core::config::LayerDoc;
use geode_data::adapter::{Adapter, AdapterError, Egress, PositionCommands, Subscription};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const SEED: u64 = 42;

/// The demo position service's adapter name, as `positions.toml` names it.
pub(crate) const DEMO_POSITIONS: &str = "demo_positions";

/// The demo position system: rewrites the risk CSVs under `dir` so the
/// next ingest sees the move. Same stem, later sentinel `as_of`: the poller
/// replaces the partition rather than adding one.
pub(crate) struct DemoPositions {
    dir: PathBuf,
}

impl DemoPositions {
    pub(crate) fn new(dir: PathBuf) -> Self {
        DemoPositions { dir }
    }
}

impl Adapter for DemoPositions {
    fn name(&self) -> &'static str {
        DEMO_POSITIONS
    }
    fn subscription(&self) -> Option<Box<dyn Subscription>> {
        None
    }
    fn egress(&self) -> Option<Box<dyn Egress>> {
        None
    }
    fn positions(&self) -> Option<Box<dyn PositionCommands>> {
        Some(Box::new(Simulator {
            dir: self.dir.clone(),
        }))
    }
}

/// One command handle over the demo source directory.
struct Simulator {
    dir: PathBuf,
}

impl PositionCommands for Simulator {
    fn move_lhu(&mut self, positions: &[String], lhu: &str) -> Result<(), AdapterError> {
        move_lhu_in(&self.dir, positions, lhu, Utc::now())
    }
}

/// The risk CSVs' source spellings (`geode-demo-data`'s `SOURCE_NAMES`).
const POSITION_REF_COLUMN: &str = "PositionRef";
const LHU_COLUMN: &str = "LHU";

/// One file a move rewrites: its new CSV text and its new sentinel JSON.
struct Rewrite {
    csv: PathBuf,
    text: String,
    sentinel: PathBuf,
    sentinel_json: String,
}

fn adapter_error(message: String) -> AdapterError {
    AdapterError { message }
}

/// `path` with `suffix` appended to its file name (`x.csv` → `x.csv.done`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Write `text` to `path` through `{path}.tmp` and a rename, so the poller
/// never reads a half-written file. The temp name does not match `*.csv`.
fn replace_file(path: &Path, text: &str) -> Result<(), AdapterError> {
    let tmp = with_suffix(path, ".tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| adapter_error(format!("cannot write {}: {e}", path.display())))
}

/// The sentinel beside `csv` with its `as_of` advanced to
/// `max(now, previous + 1s)`, whole seconds, keeping its other fields.
fn advanced_sentinel(csv: &Path, now: DateTime<Utc>) -> Result<(PathBuf, String), AdapterError> {
    let sentinel = with_suffix(csv, ".done");
    let unreadable = |e: &dyn std::fmt::Display| {
        adapter_error(format!("cannot read {}: {e}", sentinel.display()))
    };
    let text = std::fs::read_to_string(&sentinel).map_err(|e| unreadable(&e))?;
    let mut doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| unreadable(&e))?;
    let previous = doc
        .get("as_of")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .ok_or_else(|| unreadable(&"no RFC 3339 'as_of'"))?
        .with_timezone(&Utc);
    // Whole seconds first: the sentinel is written at second precision, so
    // a sub-second `now` would print as a time no later than `previous`.
    let now = now.with_nanosecond(0).unwrap_or(now);
    let as_of = now.max(previous + chrono::Duration::seconds(1));
    doc["as_of"] = serde_json::Value::String(as_of.to_rfc3339_opts(SecondsFormat::Secs, true));
    let json = serde_json::to_string_pretty(&doc).map_err(|e| unreadable(&e))?;
    Ok((sentinel, json))
}

/// Move every one of `positions` to LHU `lhu` in the risk CSVs under `dir`,
/// all or nothing: a position no CSV holds refuses the whole move,
/// `unknown position {p}` naming the first, before anything is written.
///
/// Each affected file keeps its name (the same batch), with only the `LHU`
/// field of the moved positions' rows changed, then its sentinel is
/// rewritten with a strictly later `as_of`, so the poller replaces that
/// partition. CSV before sentinel: discovery holds a CSV newer than its
/// sentinel as pending. The emitted CSVs carry no quoting (`emit.rs`), so a
/// line splits on `,`.
fn move_lhu_in(
    dir: &Path,
    positions: &[String],
    lhu: &str,
    now: DateTime<Utc>,
) -> Result<(), AdapterError> {
    let unreadable = |path: &Path, e: std::io::Error| {
        adapter_error(format!("cannot read {}: {e}", path.display()))
    };
    let mut csvs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| unreadable(dir, e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "csv"))
        .collect();
    csvs.sort();

    let wanted: HashSet<&str> = positions.iter().map(String::as_str).collect();
    let mut found: HashSet<&str> = HashSet::new();
    let mut rewrites = Vec::new();
    for csv in csvs {
        let text = std::fs::read_to_string(&csv).map_err(|e| unreadable(&csv, e))?;
        let mut lines = text.split_inclusive('\n');
        let Some(header) = lines.next() else {
            continue;
        };
        let columns: Vec<&str> = header.trim_end_matches(['\r', '\n']).split(',').collect();
        let index = |name| columns.iter().position(|c| *c == name);
        let (Some(p), Some(l)) = (index(POSITION_REF_COLUMN), index(LHU_COLUMN)) else {
            continue;
        };
        let mut out = String::with_capacity(text.len());
        out.push_str(header);
        let mut changed = false;
        for line in lines {
            let body = line.trim_end_matches(['\r', '\n']);
            let mut fields: Vec<&str> = body.split(',').collect();
            if let Some(position) = fields.get(p).and_then(|f| wanted.get(*f))
                && l < fields.len()
            {
                found.insert(*position);
                if fields[l] != lhu {
                    fields[l] = lhu;
                    out.push_str(&fields.join(","));
                    out.push_str(&line[body.len()..]);
                    changed = true;
                    continue;
                }
            }
            out.push_str(line);
        }
        if changed {
            let (sentinel, sentinel_json) = advanced_sentinel(&csv, now)?;
            rewrites.push(Rewrite {
                csv,
                text: out,
                sentinel,
                sentinel_json,
            });
        }
    }
    if let Some(missing) = positions.iter().find(|p| !found.contains(p.as_str())) {
        return Err(adapter_error(format!("unknown position {missing}")));
    }
    for r in rewrites {
        replace_file(&r.csv, &r.text)?;
        replace_file(&r.sentinel, &r.sentinel_json)?;
    }
    Ok(())
}

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
/// CVI, dividend and option-chain (`opra_sim`) sources subscribe to
/// `demo_bus`, coalescing updates per key over 500 ms. Chains are never
/// uploaded, so the egress target names only CVI and dividends. `demo_kdb`
/// and `demo_rest` fetch the `series` dataset; the former offers a
/// catalogue and the latter requires entered identities. The position
/// service is `demo_positions`, which rewrites the risk CSVs in place.
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
         [opra_sim]\nadapter = \"demo_bus\"\ndataset = \"option_chain\"\n\
         document = \"option_chain\"\ntopics = [\"marketdata/chain/>\"]\n\
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
    // Moves go to the demo position system, which rewrites the risk CSVs
    // the `demo` source polls (`DemoPositions`).
    let positions = format!("[service]\nadapter = \"{DEMO_POSITIONS}\"\n");
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
        ("positions", positions),
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
                "positions",
                "sources",
                "views"
            ]
        );
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        let paths = sources.table["demo"]["paths"].as_array().unwrap();
        assert_eq!(paths[0].as_str(), Some("/tmp/geode-demo/100-42/src/*.csv"));
        assert_eq!(sources.table["demo"]["poll_interval"].as_str(), Some("2s"));
    }

    /// The demo `positions` doc names `demo_positions`, reads without
    /// diagnostics and resolves against the demo position adapter.
    #[test]
    fn the_demo_layers_positions_doc_names_demo_positions_and_resolves() {
        let src = std::path::Path::new("/tmp/geode-demo/100-42/src");
        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(src),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (spec, d) = geode_core::positions::from_doc(config.doc("positions").unwrap());
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            spec,
            Some(geode_core::positions::PositionsSpec {
                adapter: DEMO_POSITIONS.to_string()
            })
        );
        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        adapters.register(std::sync::Arc::new(DemoPositions::new(src.to_path_buf())));
        let (kept, d) = geode_data::positions::resolve(spec.clone(), &adapters);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(kept, spec);
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
        assert_eq!(sources.len(), 6);
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
        assert_eq!(
            sources.len(),
            6,
            "demo, cvi, dividend, opra_sim, demo_kdb, demo_rest"
        );
        assert!(sources.iter().any(|s| s.name == "dividend"));
    }

    /// The option-chain source declares its document kind, topic,
    /// coalescing, and priority alongside the other demo sources without
    /// diagnostics.
    #[test]
    fn the_demo_layer_declares_the_option_chain_source() {
        let docs = layer(std::path::Path::new("/tmp/geode-demo/100-42/src"));
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        let chain = &sources.table["opra_sim"];
        assert_eq!(chain["adapter"].as_str(), Some("demo_bus"));
        assert_eq!(chain["dataset"].as_str(), Some("option_chain"));
        assert_eq!(chain["document"].as_str(), Some("option_chain"));
        let topics = chain["topics"].as_array().unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].as_str(), Some("marketdata/chain/>"));
        assert_eq!(chain["coalesce"].as_str(), Some("500ms"));
        assert_eq!(chain["priority"].as_str(), Some("latest_other"));

        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (sources, d) =
            geode_data::source::SourceSpec::from_doc(config.doc("sources").unwrap(), &schema);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            sources.len(),
            6,
            "demo, cvi, dividend, opra_sim, demo_kdb, demo_rest"
        );
        assert!(sources.iter().any(|s| s.name == "opra_sim"));
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
        assert_eq!(sources.len(), 6);
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

    /// The same for the default `demo` vol model: an empty registry would
    /// gain a spurious "vol model" diagnostic.
    fn test_vol_models() -> geode_data::VolModelRegistry {
        let mut vol_models = geode_data::VolModelRegistry::default();
        vol_models.register(std::sync::Arc::new(geode_pricing::DemoVolModel));
        vol_models
    }

    /// Registers the demo bus required by the `sophis` egress target, and
    /// the demo position service `positions.toml` names, as `main.rs` does.
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
        adapters.register(std::sync::Arc::new(DemoPositions::new(
            "/tmp/geode-demo/100000-42/src".into(),
        )));
        (adapters, feed)
    }

    /// `Bridge::positions_configured` is whether `positions.toml`'s service
    /// survived resolution: the demo layer names `demo_positions`, so it is
    /// configured when that adapter is registered and not when it is absent.
    #[gpui::test]
    fn the_bridge_says_whether_a_position_service_is_configured(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: layer(&dir.path().join("src")),
            ..ConfigSources::default()
        });
        let (with, _feed) = test_adapters();
        let (bus, _bus_feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        let mut without = geode_data::adapter::AdapterRegistry::default();
        without.register(bus);
        for (i, (adapters, expected)) in [(with, true), (without, false)].into_iter().enumerate() {
            let setup = crate::bridge::data_setup(
                &config,
                dir.path().join(format!("geode-{i}.duckdb")),
                adapters,
                test_pricers(),
                test_vol_models(),
            )
            .unwrap();
            let bridge = cx.update(|cx| {
                crate::bridge::start(
                    setup,
                    geode_shell::vimfind::FindStyle::default(),
                    std::time::Duration::from_secs(60),
                    cx,
                )
            });
            assert_eq!(bridge.positions_configured, expected);
            bridge.handle.shutdown();
        }
    }

    /// End to end through the real service: the demo layer's `positions.toml`
    /// resolves `DemoPositions`, `move_lhu` reaches the simulator through the
    /// position worker and is answered `Ok`, and the demo source's next poll
    /// ingests the rewritten partition, so the position's LHU is the target
    /// and only the target (the partition replaced, not added to).
    #[test]
    fn a_move_is_ingested_as_the_new_lhu() {
        use geode_core::positions::MoveLhuParams;
        use geode_core::query::{DistinctParams, QueryKey};
        use geode_core::scope::{DimensionSelection, Scope};
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService};
        use std::sync::mpsc::Receiver;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let src = ensure_emitted(dir.path(), 100).unwrap();
        let config = Config::load(&ConfigSources {
            builtin: layer(&src),
            ..ConfigSources::default()
        });
        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        let (bus, _feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        adapters.register(bus);
        adapters.register(std::sync::Arc::new(DemoPositions::new(src.clone())));
        let setup = crate::bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            adapters,
            test_pricers(),
            test_vol_models(),
        )
        .unwrap();
        assert!(setup.config.positions.is_some());
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = DataService::spawn(
            setup.config,
            std::sync::Arc::new(move |e| tx.send(e).is_ok()),
        );

        // A position of one book's file, and an LHU it does not hold.
        let csv = std::fs::read_dir(&src)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().ends_with("_BK003.csv"))
            .unwrap();
        let text = std::fs::read_to_string(csv).unwrap();
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap().split(',').collect();
        let at = |name| header.iter().position(|c| *c == name).unwrap();
        let first: Vec<&str> = lines.next().unwrap().split(',').collect();
        let p = first[at("PositionRef")].to_string();
        let target = "BK007_LHU2".to_string();
        assert_ne!(first[at("LHU")], target);

        // The LHU values ingested for `p`, polled until `done` holds.
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut tag = 0;
        let mut lhus_until = |rx: &Receiver<DataEvent>, done: &dyn Fn(&[String]) -> bool| loop {
            assert!(
                Instant::now() < deadline,
                "timed out polling the LHU of {p}"
            );
            tag += 1;
            handle
                .distinct(DistinctParams {
                    key: QueryKey(1),
                    tag,
                    column: "lhu".into(),
                    scope: Scope {
                        dimensions: vec![DimensionSelection {
                            column: "position_ref".into(),
                            values: vec![p.clone()],
                        }],
                        ..Scope::default()
                    },
                    as_of: AsOf::Live,
                })
                .unwrap();
            let values = loop {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(DataEvent::Distinct(o)) if o.tag == tag => break o.values.unwrap(),
                    Ok(_) => continue,
                    Err(e) => panic!("no distinct answer: {e}"),
                }
            };
            let values: Vec<String> = values.into_iter().map(|(v, _)| v).collect();
            if done(&values) {
                break values;
            }
            std::thread::sleep(Duration::from_millis(250));
        };

        let before = lhus_until(&rx, &|v| !v.is_empty());
        assert!(!before.contains(&target), "{before:?}");

        handle
            .move_lhu(MoveLhuParams {
                tag: 1,
                positions: vec![p.clone()],
                lhu: target.clone(),
            })
            .unwrap();
        let outcome = loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(DataEvent::Command(o)) => break o,
                Ok(_) => continue,
                Err(e) => panic!("no command answer: {e}"),
            }
        };
        assert_eq!(outcome.result, Ok(()));

        let after = lhus_until(&rx, &|v| v.contains(&target));
        assert_eq!(after, vec![target], "replaced, not added to");
        handle.shutdown();
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
            test_vol_models(),
        )
        .expect("datasets + views are both present in the demo layer");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        let names: Vec<&str> = setup.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["tree", "wide"]);
        let wide = setup.views.iter().find(|v| v.name == "wide").unwrap();
        assert_eq!(wide.columns.len(), 100, "the wide view has 100 columns");
        // All six source definitions survive setup: risk CSVs, the CVI,
        // dividend and option-chain subscriptions, and the two timeseries
        // fetch adapters.
        assert_eq!(setup.config.sources.len(), 6);
        // Setup must carry the resolved egress target into the service config.
        assert_eq!(setup.config.egress.len(), 1);
        assert_eq!(setup.config.egress[0].name, "sophis");
        // And the resolved position service.
        assert_eq!(
            setup.config.positions,
            Some(geode_core::positions::PositionsSpec {
                adapter: DEMO_POSITIONS.to_string()
            })
        );
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
        geode_core::document::check_kind_against(
            &geode_documents::OptionChainKind,
            setup.config.schema.dataset("option_chain").unwrap(),
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
            test_vol_models(),
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

#[cfg(test)]
mod demo_positions_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A tiny emitted demo directory: one business date, every book.
    fn emitted() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 100,
            seed: SEED,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(dir.path());
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts).unwrap();
        dir
    }

    /// Every file in `dir` by name.
    fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&p).unwrap(),
                )
            })
            .collect()
    }

    /// The one CSV whose name ends in `suffix`.
    fn csv_ending(dir: &Path, suffix: &str) -> String {
        let names: Vec<String> = files(dir)
            .into_keys()
            .filter(|n| n.ends_with(suffix))
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        names[0].clone()
    }

    fn column(header: &str, name: &str) -> usize {
        header.split(',').position(|c| c == name).unwrap()
    }

    /// The distinct `PositionRef`s of a CSV, in file order.
    fn positions_in(text: &str) -> Vec<String> {
        let mut lines = text.lines();
        let p = column(lines.next().unwrap(), "PositionRef");
        let mut out: Vec<String> = Vec::new();
        for line in lines {
            let v = line.split(',').nth(p).unwrap().to_string();
            if !out.contains(&v) {
                out.push(v);
            }
        }
        out
    }

    fn as_of(dir: &Path, sentinel: &str) -> DateTime<Utc> {
        let text = std::fs::read_to_string(dir.join(sentinel)).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        DateTime::parse_from_rfc3339(doc["as_of"].as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_move_rewrites_the_lhu_of_those_positions_only() {
        let dir = emitted();
        let name = csv_ending(dir.path(), "_BK003.csv");
        let before = std::fs::read_to_string(dir.path().join(&name)).unwrap();
        let sentinel = format!("{name}.done");
        let as_of_before = as_of(dir.path(), &sentinel);
        let held = positions_in(&before);
        assert!(held.len() >= 3, "{held:?}");
        let moved = vec![held[0].clone(), held[1].clone()];

        move_lhu_in(dir.path(), &moved, "BK007_LHU2", now()).unwrap();

        let after = std::fs::read_to_string(dir.path().join(&name)).unwrap();
        let (b, a): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0], b[0], "the header is unchanged");
        let p = column(b[0], "PositionRef");
        let l = column(b[0], "LHU");
        let mut rewritten = 0;
        for (old, new) in b.iter().zip(&a).skip(1) {
            let old_f: Vec<&str> = old.split(',').collect();
            let new_f: Vec<&str> = new.split(',').collect();
            if moved.iter().any(|m| m == old_f[p]) {
                rewritten += 1;
                assert_eq!(new_f[l], "BK007_LHU2", "{new}");
                for (i, (o, n)) in old_f.iter().zip(&new_f).enumerate() {
                    if i != l {
                        assert_eq!(o, n, "only LHU changes: {new}");
                    }
                }
            } else {
                assert_eq!(old, new, "an unmoved row is byte-identical");
            }
        }
        assert!(rewritten >= 2);
        assert_eq!(after.ends_with('\n'), before.ends_with('\n'));
        assert!(as_of(dir.path(), &sentinel) > as_of_before);
    }

    #[test]
    fn a_position_in_a_split_book_is_moved_in_its_own_file() {
        let dir = emitted();
        let name = csv_ending(dir.path(), "_BK000_part1.csv");
        let text = std::fs::read_to_string(dir.path().join(&name)).unwrap();
        let p = positions_in(&text)[0].clone();
        let before = files(dir.path());

        move_lhu_in(dir.path(), &[p], "BK000_LHU3", now()).unwrap();

        let after = files(dir.path());
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "no file appears or disappears"
        );
        let changed: Vec<&String> = before.keys().filter(|k| before[*k] != after[*k]).collect();
        assert_eq!(changed, vec![&name, &format!("{name}.done")]);
    }

    #[test]
    fn an_unknown_position_refuses_and_changes_nothing() {
        let dir = emitted();
        let name = csv_ending(dir.path(), "_BK003.csv");
        let text = std::fs::read_to_string(dir.path().join(&name)).unwrap();
        let real = positions_in(&text)[0].clone();
        let before = files(dir.path());

        let err =
            move_lhu_in(dir.path(), &[real, "P99".to_string()], "BK007_LHU2", now()).unwrap_err();

        assert_eq!(err.message, "unknown position P99");
        assert_eq!(files(dir.path()), before, "every file is byte-identical");
    }

    #[test]
    fn the_sentinel_is_strictly_later_even_within_the_same_second() {
        let dir = emitted();
        let name = csv_ending(dir.path(), "_BK003.csv");
        let text = std::fs::read_to_string(dir.path().join(&name)).unwrap();
        let p = positions_in(&text)[0].clone();
        let sentinel = format!("{name}.done");
        // Mid-second, so a sub-second `now` cannot pass for "later".
        let now = now() + chrono::Duration::milliseconds(700);

        move_lhu_in(dir.path(), std::slice::from_ref(&p), "BK007_LHU2", now).unwrap();
        let first = as_of(dir.path(), &sentinel);
        move_lhu_in(dir.path(), std::slice::from_ref(&p), "BK007_LHU1", now).unwrap();
        let second = as_of(dir.path(), &sentinel);

        assert!(second > first, "{second} > {first}");
    }
}
