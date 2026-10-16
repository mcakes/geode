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
pub const DEMO_POSITIONS: &str = "demo_positions";

/// The demo position system: rewrites the risk CSVs under `dir` so the
/// next ingest sees the move. Same stem, later sentinel `as_of`: the poller
/// replaces the partition rather than adding one.
pub struct DemoPositions {
    dir: PathBuf,
}

impl DemoPositions {
    pub fn new(dir: PathBuf) -> Self {
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
/// all or nothing with respect to validation: a position no CSV holds
/// refuses the whole move, `unknown position {p}` naming the first, before
/// anything is written. A write that fails part-way through can leave the
/// files rewritten before it in place.
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

/// Returns `dir/src`, generating risk source files when it has no directory
/// entries, and fills `dir/scenario` with the scenario files the same way.
/// Any existing entry suppresses generation of that directory; existing
/// contents are reused without checking completeness so repeated launches
/// can use a warm store. Directory creation and emission errors propagate
/// to the caller.
pub fn ensure_emitted(dir: &Path, rows: usize) -> std::io::Result<PathBuf> {
    let populated = |path: &Path| {
        std::fs::read_dir(path)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    };
    let src = dir.join("src");
    // Scenario files live beside `src`, not under it: the risk source's
    // `<src>/*.csv` glob must never pick one up.
    let scenario = dir.join("scenario");
    let src_populated = populated(&src);
    let scenario_populated = populated(&scenario);
    if src_populated && scenario_populated {
        return Ok(src);
    }
    let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
        rows,
        seed: SEED,
        business_dates: 1,
    });
    if !src_populated {
        std::fs::create_dir_all(&src)?;
        let mut opts = geode_demo_data::EmitOptions::new(&src);
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts)?;
    }
    if !scenario_populated {
        let set =
            geode_demo_data::generate_scenarios(&batch, geode_demo_data::SCENARIO_COVERAGE, SEED);
        geode_demo_data::emit_scenario_directory(&set, &scenario)?;
    }
    Ok(src)
}

/// The scenario dataset fed by one report per measure
/// (`geode_demo_data::TERM_REPORTS`).
const TERM_DATASET: &str = "term_sensitivities";

/// Builds the builtin demo layer from compiled-in example documents and
/// source paths rooted at `source_dir`.
///
/// The risk CSV source polls every two seconds using sentinel readiness.
/// CVI, dividend and option-chain (`opra_sim`) sources subscribe to the
/// `<base>/NOTIFY` topics on `demo_bus`, recover each recorded topic's
/// latest document at start, and coalesce updates per key over 500 ms.
/// Chains are never uploaded, so the egress target names only CVI and
/// dividends, at the NOTIFY topics those sources subscribe to. `demo_kdb`
/// and `demo_rest` fetch the `series` dataset; the former offers a
/// catalogue and the latter requires entered identities. The position
/// service is `demo_positions`, which rewrites the risk CSVs in place.
/// `refdb` polls the `underlyings` reference table from `demo_refdb` every
/// 30 s. `hanweck_surface`, `hanweck_realized` and `hanweck_varswap` answer
/// the `vol_surface`, `realized_vol` and `varswap` history datasets from
/// `demo_hanweck`'s `surface`, `realized` and `varswap` tables.
/// Each scenario dataset has a `demo_scenario_<dataset>` directory
/// source over `scenario/` beside `source_dir`, polled every five seconds,
/// except `term_sensitivities`: one report per measure feeds it, each from
/// its own `demo_scenario_term_<report>` source listing its `measures`.
pub fn layer(source_dir: &Path) -> Vec<LayerDoc> {
    let scenario_dir = source_dir
        .parent()
        .map(|p| p.join("scenario"))
        .unwrap_or_else(|| source_dir.join("scenario"));
    let mut scenario_sources = String::new();
    for ds in geode_demo_data::SCENARIO_DATASETS {
        if ds == TERM_DATASET {
            continue;
        }
        scenario_sources.push_str(&format!(
            "[demo_scenario_{ds}]\ndataset = \"{ds}\"\npaths = [{:?}]\n\
             readiness = \"sentinel\"\npriority = \"latest_other\"\npoll_interval = \"5s\"\n\
             pending_timeout = \"1m\"\nbatch_pattern = '^{ds}_\\d{{4}}-\\d{{2}}-\\d{{2}}_(?P<batch>.+)$'\n",
            scenario_dir.join(format!("{ds}_*.csv")).to_string_lossy()
        ));
    }
    // `term_sensitivities` is fed by one report per measure, each its own
    // source over `<report>_<book>_<date>.csv` with batch `<report>_<book>`:
    // a partition is (dataset, batch, book), so reports sharing a batch
    // would replace each other's rows. Each lists the measure (and twin)
    // its files carry, so the others' read as NULL without a signal while
    // a file missing its own still degrades.
    for report in geode_demo_data::TERM_REPORTS {
        scenario_sources.push_str(&format!(
            "[demo_scenario_term_{report}]\ndataset = \"{TERM_DATASET}\"\npaths = [{:?}]\n\
             readiness = \"sentinel\"\npriority = \"latest_other\"\npoll_interval = \"5s\"\n\
             pending_timeout = \"1m\"\nbatch_pattern = '^(?P<batch>{report}_.+)_\\d{{4}}-\\d{{2}}-\\d{{2}}$'\n\
             measures = [\"{report}\", \"{report}_usd\"]\n",
            scenario_dir.join(format!("{report}_*.csv")).to_string_lossy()
        ));
    }
    let sources = format!(
        "config_version = 1\n[demo]\ndataset = \"risk_snapshot\"\npaths = [{:?}]\n\
         readiness = \"sentinel\"\npriority = \"latest_risk\"\npoll_interval = \"2s\"\n\
         pending_timeout = \"1m\"\nbatch_pattern = '^risk_\\d{{4}}-\\d{{2}}-\\d{{2}}_(?P<batch>.+)$'\n\
         [cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
         topics = [\"marketdata/cvi/*/NOTIFY\"]\ncoalesce = \"500ms\"\nsource_time = \"receive\"\n\
         priority = \"latest_other\"\n\
         [dividend]\nadapter = \"demo_bus\"\ndataset = \"dividend_schedule\"\n\
         document = \"dividend_schedule\"\ntopics = [\"marketdata/dividend/*/NOTIFY\"]\n\
         coalesce = \"500ms\"\nsource_time = \"receive\"\npriority = \"latest_other\"\n\
         [opra_sim]\nadapter = \"demo_bus\"\ndataset = \"option_chain\"\n\
         document = \"option_chain\"\ntopics = [\"marketdata/chain/*/NOTIFY\"]\n\
         coalesce = \"500ms\"\nsource_time = \"receive\"\npriority = \"latest_other\"\n\
         [demo_kdb]\nadapter = \"demo_kdb\"\ndataset = \"series\"\n\
         [demo_rest]\nadapter = \"demo_rest\"\ndataset = \"series\"\n\
         [refdb]\nadapter = \"demo_refdb\"\ndataset = \"underlyings\"\n\
         table = \"underlyings\"\npoll_interval = \"30s\"\n\
         [hanweck_surface]\nadapter = \"demo_hanweck\"\ndataset = \"vol_surface\"\n\
         table = \"surface\"\n\
         [hanweck_realized]\nadapter = \"demo_hanweck\"\ndataset = \"realized_vol\"\n\
         table = \"realized\"\n\
         [hanweck_varswap]\nadapter = \"demo_hanweck\"\ndataset = \"varswap\"\n\
         table = \"varswap\"\n{}",
        source_dir.join("*.csv").to_string_lossy(),
        scenario_sources
    );
    // The `sophis` target publishes both document kinds through the same
    // adapter as their subscribed sources. Per-key NOTIFY addresses route
    // uploads back to those sources for an echo; an address off the
    // subscribed `*/NOTIFY` patterns would upload with no echo. Builtin
    // documents are exempt from the config-version check, so this generated
    // document needs no header.
    let egress = "[sophis]\nadapter = \"demo_bus\"\n\
         [sophis.documents]\ncvi_params = \"marketdata/cvi/{key}/NOTIFY\"\n\
         dividend_schedule = \"marketdata/dividend/{key}/NOTIFY\"\n"
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
        (
            "watchlists",
            include_str!("../../../examples/demo-config/watchlists.toml").to_string(),
        ),
    ];
    docs.into_iter()
        .map(|(name, text)| LayerDoc::builtin(name, &text).expect("demo config is well-formed"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source's `topics` patterns, read from the demo `sources` document.
    fn topics_of(sources: &LayerDoc, source: &str) -> Vec<String> {
        sources.table[source]["topics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect()
    }

    /// Each subscribed demo source matches the NOTIFY topic its producer
    /// publishes on, and its patterns end at NOTIFY: recovery asks the
    /// recorded topics that match, and a pattern reaching any other level
    /// would ask the bus for a topic no producer notifies on.
    #[test]
    fn every_demo_subscription_pattern_matches_its_producers_notify_topics() {
        let layer = layer(Path::new("/tmp/src"));
        let sources = layer.iter().find(|d| d.name == "sources").unwrap();
        for (prefix, source) in [
            ("marketdata/cvi/", "cvi"),
            ("marketdata/dividend/", "dividend"),
            ("marketdata/chain/", "opra_sim"),
        ] {
            let patterns = topics_of(sources, source);
            let topic = format!("{prefix}SPX/NOTIFY");
            assert!(
                patterns
                    .iter()
                    .any(|p| geode_data::adapter::topic_matches(p, &topic)),
                "{source}"
            );
            assert!(
                patterns.iter().all(|p| p.ends_with("/NOTIFY")),
                "{source} recovers only NOTIFY patterns"
            );
        }
    }

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
                "views",
                "watchlists"
            ]
        );
        let sources = docs.iter().find(|d| d.name == "sources").unwrap();
        let paths = sources.table["demo"]["paths"].as_array().unwrap();
        assert_eq!(paths[0].as_str(), Some("/tmp/geode-demo/100-42/src/*.csv"));
        assert_eq!(sources.table["demo"]["poll_interval"].as_str(), Some("2s"));
        assert_eq!(
            sources.table["refdb"]["adapter"].as_str(),
            Some("demo_refdb")
        );
        assert_eq!(
            sources.table["refdb"]["poll_interval"].as_str(),
            Some("30s")
        );
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
            Some("marketdata/cvi/{key}/NOTIFY")
        );
        assert_eq!(
            egress.table["sophis"]["documents"]["dividend_schedule"].as_str(),
            Some("marketdata/dividend/{key}/NOTIFY")
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
                (
                    "cvi_params".to_string(),
                    "marketdata/cvi/{key}/NOTIFY".to_string()
                ),
                (
                    "dividend_schedule".to_string(),
                    "marketdata/dividend/{key}/NOTIFY".to_string()
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
            Some("marketdata/cvi/*/NOTIFY")
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
        assert_eq!(
            sources.len(),
            19,
            "demo, cvi, dividend, opra_sim, demo_kdb, demo_rest, refdb, hanweck_surface, \
             hanweck_realized, hanweck_varswap + four scenario datasets' sources + \
             five term reports' sources"
        );
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
            Some("marketdata/dividend/*/NOTIFY")
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
            19,
            "demo, cvi, dividend, opra_sim, demo_kdb, demo_rest, refdb, hanweck_surface, \
             hanweck_realized, hanweck_varswap + four scenario datasets' sources + \
             five term reports' sources"
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
        assert_eq!(topics[0].as_str(), Some("marketdata/chain/*/NOTIFY"));
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
            19,
            "demo, cvi, dividend, opra_sim, demo_kdb, demo_rest, refdb, hanweck_surface, \
             hanweck_realized, hanweck_varswap + four scenario datasets' sources + \
             five term reports' sources"
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
        assert_eq!(
            sources.len(),
            19,
            "demo, cvi, dividend, opra_sim, demo_kdb, demo_rest, refdb, hanweck_surface, \
             hanweck_realized, hanweck_varswap + four scenario datasets' sources + \
             five term reports' sources"
        );
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

    /// The three Hanweck sources resolve to the history shape over their
    /// datasets, each reading its own `demo_hanweck` table, and the
    /// adapter answers every table they name.
    #[test]
    fn the_demo_layer_declares_the_three_history_sources() {
        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (sources, d) =
            geode_data::source::SourceSpec::from_doc(config.doc("sources").unwrap(), &schema);
        assert!(d.is_empty(), "{d:?}");
        let adapter = crate::demo_hanweck::DemoHanweck::new(42);
        for (name, dataset, table) in [
            ("hanweck_surface", "vol_surface", "surface"),
            ("hanweck_realized", "realized_vol", "realized"),
            ("hanweck_varswap", "varswap", "varswap"),
        ] {
            let source = sources.iter().find(|s| s.name == name).unwrap();
            assert_eq!(
                source.shape(&schema),
                geode_core::source_config::SourceShape::History,
                "{name}"
            );
            assert_eq!(source.adapter, crate::demo_hanweck::DEMO_HANWECK);
            assert_eq!(source.dataset, dataset);
            assert_eq!(source.table.as_deref(), Some(table));
            let ds = schema.dataset(dataset).unwrap();
            let mut history = adapter.history().unwrap();
            for column in &ds.key {
                assert!(
                    history.keys(table, column).is_some(),
                    "{table} enumerates its key column {column}"
                );
            }
        }
    }

    /// The `refdb` source resolves to the snapshot shape over the
    /// `underlyings` reference dataset, reading the table of that name
    /// every 30 s.
    #[test]
    fn the_demo_layer_declares_the_reference_snapshot_source() {
        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (sources, d) =
            geode_data::source::SourceSpec::from_doc(config.doc("sources").unwrap(), &schema);
        assert!(d.is_empty(), "{d:?}");
        let refdb = sources.iter().find(|s| s.name == "refdb").unwrap();
        assert_eq!(
            refdb.shape(&schema),
            geode_core::source_config::SourceShape::Snapshot
        );
        assert_eq!(refdb.adapter, crate::demo_refdb::DEMO_REFDB);
        assert_eq!(refdb.dataset, "underlyings");
        assert_eq!(refdb.table.as_deref(), Some("underlyings"));
        assert_eq!(refdb.poll_interval, std::time::Duration::from_secs(30));
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

    /// The demo schema and sources at `/tmp/geode-demo/100-42/src`.
    fn demo_schema_and_sources() -> (
        geode_core::schema::SchemaSpec,
        Vec<geode_data::source::SourceSpec>,
    ) {
        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: layer(std::path::Path::new("/tmp/geode-demo/100-42/src")),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let (sources, d) =
            geode_data::source::SourceSpec::from_doc(config.doc("sources").unwrap(), &schema);
        assert!(d.is_empty(), "{d:?}");
        (schema, sources)
    }

    #[test]
    fn the_demo_layer_declares_the_scenario_sources() {
        let (schema, sources) = demo_schema_and_sources();
        for ds in geode_demo_data::SCENARIO_DATASETS {
            let spec = schema
                .dataset(ds)
                .unwrap_or_else(|| panic!("{ds} declared"));
            assert!(spec.has_axes(), "{ds} has axes");
            assert!(
                spec.column("underlying_ref").is_some() && spec.column("currency").is_some(),
                "{ds} keeps the risk dimensions"
            );
            if ds == TERM_DATASET {
                assert!(
                    !sources
                        .iter()
                        .any(|s| s.name == format!("demo_scenario_{ds}")),
                    "no combined term source"
                );
                continue;
            }
            let src = sources
                .iter()
                .find(|s| s.name == format!("demo_scenario_{ds}"))
                .unwrap_or_else(|| panic!("source for {ds}"));
            assert_eq!(src.dataset, ds);
            assert_eq!(
                src.paths,
                [format!("/tmp/geode-demo/100-42/scenario/{ds}_*.csv")]
            );
            assert_eq!(
                src.priority,
                geode_core::source_config::Priority::LatestOther
            );
            assert!(
                src.batch_pattern
                    .as_deref()
                    .unwrap()
                    .starts_with(&format!("^{ds}_")),
                "{:?}",
                src.batch_pattern
            );
        }
        assert_eq!(
            sources.len(),
            19,
            "demo, cvi, dividend, opra_sim, demo_kdb, demo_rest, refdb, hanweck_surface, \
             hanweck_realized, hanweck_varswap + four scenario datasets' sources + \
             five term reports' sources"
        );
    }

    /// One source per term report feeds `term_sensitivities`, each over
    /// its own files and under its own batch `<report>_<book>`: a
    /// partition is (dataset, batch, book), so reports sharing a batch would
    /// replace each other's rows. Each lists its own measure and twin, and
    /// together the lists are exactly the dataset's measures.
    #[test]
    fn each_term_report_feeds_term_sensitivities_under_its_own_batch() {
        let (schema, sources) = demo_schema_and_sources();
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 300,
            seed: SEED,
            business_dates: 1,
        });
        let set = geode_demo_data::generate_scenarios(&batch, 20, SEED);
        let dir = tempfile::tempdir().unwrap();
        let files = geode_demo_data::emit_scenario_directory(&set, dir.path()).unwrap();
        let book = files
            .iter()
            .find(|f| {
                f.csv_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("diva_")
            })
            .unwrap()
            .books[0]
            .clone();
        let mut listed: Vec<String> = Vec::new();
        let mut batches: Vec<String> = Vec::new();
        for report in geode_demo_data::TERM_REPORTS {
            let src = sources
                .iter()
                .find(|s| s.name == format!("demo_scenario_term_{report}"))
                .unwrap_or_else(|| panic!("a source for {report}"));
            assert_eq!(src.dataset, TERM_DATASET);
            assert_eq!(
                src.paths,
                [format!("/tmp/geode-demo/100-42/scenario/{report}_*.csv")]
            );
            assert_eq!(
                src.priority,
                geode_core::source_config::Priority::LatestOther
            );
            assert_eq!(
                src.measures,
                Some(vec![report.to_string(), format!("{report}_usd")])
            );
            listed.extend(src.measures.clone().unwrap());
            // The file the emitter writes for this report and book.
            let file = files
                .iter()
                .map(|f| f.csv_path.clone())
                .find(|p| {
                    let n = p.file_name().unwrap().to_string_lossy();
                    n.starts_with(&format!("{report}_")) && n.contains(&book)
                })
                .unwrap_or_else(|| panic!("a {report} file for {book}"));
            let batch = src.batch_of(&file);
            assert_eq!(batch, format!("{report}_{book}"));
            // A second date keeps the same batch, so it replaces, not adds.
            assert_eq!(
                src.batch_of(Path::new(&format!("{report}_{book}_2026-08-25.csv"))),
                batch
            );
            // No other report's glob picks this file up.
            for other in geode_demo_data::TERM_REPORTS
                .iter()
                .filter(|o| **o != report)
            {
                assert!(
                    !file
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(&format!("{other}_")),
                    "{other}'s glob would read a {report} file"
                );
            }
            batches.push(batch);
        }
        batches.sort();
        batches.dedup();
        assert_eq!(batches.len(), geode_demo_data::TERM_REPORTS.len());
        let mut declared: Vec<String> = schema
            .dataset(TERM_DATASET)
            .unwrap()
            .columns
            .iter()
            .filter(|c| matches!(c.role, geode_core::schema::ColumnRole::Measure { .. }))
            .map(|c| c.name.clone())
            .collect();
        declared.sort();
        listed.sort();
        assert_eq!(listed, declared, "each measure is exactly one report's");
    }

    /// `[demo]` reads `<src>/*.csv`; the scenario files live beside, not
    /// under, `src`, so a scenario CSV never loads as risk.
    #[test]
    fn the_risk_glob_does_not_match_scenario_files() {
        let (_, sources) = demo_schema_and_sources();
        let risk = sources.iter().find(|s| s.name == "demo").unwrap();
        assert_eq!(risk.paths, ["/tmp/geode-demo/100-42/src/*.csv"]);
        let risk_dir = Path::new(&risk.paths[0]).parent().unwrap();
        for src in sources
            .iter()
            .filter(|s| s.name.starts_with("demo_scenario_"))
        {
            let dir = Path::new(&src.paths[0]).parent().unwrap();
            assert_ne!(dir, risk_dir, "{}", src.name);
            assert_eq!(
                dir,
                Path::new("/tmp/geode-demo/100-42/scenario"),
                "{}",
                src.name
            );
        }
    }

    #[test]
    fn ensure_emitted_fills_the_scenario_directory_too() {
        let dir = tempfile::tempdir().unwrap();
        ensure_emitted(dir.path(), 300).unwrap();
        let scenario = dir.path().join("scenario");
        let n = std::fs::read_dir(&scenario).unwrap().count();
        assert!(n > 0, "scenario files emitted");
        assert!(std::fs::read_dir(&scenario).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".csv.done")
        }));
        // A warm scenario directory is reused, like src.
        ensure_emitted(dir.path(), 300).unwrap();
        assert_eq!(std::fs::read_dir(&scenario).unwrap().count(), n);
    }

    /// Every scenario column's `source_name` is a header the emitter writes,
    /// so a misspelling on either side fails here rather than as a
    /// missing-column health condition at runtime.
    #[test]
    fn every_scenario_source_name_is_a_header_the_emitter_writes() {
        let (schema, _) = demo_schema_and_sources();
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 300,
            seed: SEED,
            business_dates: 1,
        });
        let set = geode_demo_data::generate_scenarios(&batch, 20, SEED);
        let dir = tempfile::tempdir().unwrap();
        geode_demo_data::emit_scenario_directory(&set, dir.path()).unwrap();
        // Each dataset's columns against the headers of every report into
        // it: `term_sensitivities` reads the five term reports.
        let header_of = |prefix: &str| -> Vec<String> {
            let csv = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    let name = p.file_name().unwrap().to_string_lossy();
                    name.starts_with(&format!("{prefix}_")) && name.ends_with(".csv")
                })
                .unwrap_or_else(|| panic!("a CSV for {prefix}"));
            let text = std::fs::read_to_string(&csv).unwrap();
            text.lines()
                .next()
                .unwrap()
                .split(',')
                .map(str::to_string)
                .collect()
        };
        for ds in geode_demo_data::SCENARIO_DATASETS {
            let header: Vec<String> = if ds == TERM_DATASET {
                geode_demo_data::TERM_REPORTS
                    .iter()
                    .flat_map(|r| header_of(r))
                    .collect()
            } else {
                header_of(ds)
            };
            let spec = schema.dataset(ds).unwrap();
            for column in &spec.columns {
                assert!(
                    header.iter().any(|h| h == column.source_name()),
                    "{ds}.{} reads {:?}, absent from {header:?}",
                    column.name,
                    column.source_name()
                );
            }
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
        // One rewritten line keeps its ending byte for byte.
        let ending = |line: &str| line[line.trim_end_matches(['\r', '\n']).len()..].to_string();
        let (old_line, new_line) = before
            .split_inclusive('\n')
            .zip(after.split_inclusive('\n'))
            .find(|(o, n)| o != n)
            .expect("a rewritten line");
        assert!(!ending(old_line).is_empty(), "{old_line:?}");
        assert_eq!(ending(new_line).as_bytes(), ending(old_line).as_bytes());
        assert!(as_of(dir.path(), &sentinel) > as_of_before);
        // CSV before sentinel: discovery holds a CSV newer than its sentinel
        // as pending, so the sentinel is written last.
        let mtime = |n: &str| {
            std::fs::metadata(dir.path().join(n))
                .and_then(|m| m.modified())
                .unwrap()
        };
        assert!(
            mtime(&sentinel) >= mtime(&name),
            "the sentinel is written after its CSV"
        );
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
