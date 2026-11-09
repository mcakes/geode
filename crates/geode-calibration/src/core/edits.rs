//! The graph edits as whole-object configuration edits, pure. A stage is
//! one object of the `calibration` document (`[stages.<name>]`) and the
//! shell writes an object whole, so every edit here is the stage's current
//! table with its `inputs` changed, a new table for a new stage, or `None`
//! to remove one. The tile hands each table to the shell through
//! `FrameRef::queue_config_edits`; the shell writes the user layer (forking
//! an inherited stage) and the reload brings the new pipeline back.
//!
//! The candidates a connect offers are computed here from the pipeline's
//! typed ports: a stage's output must fit the open port, must not already
//! be an input, and must not sit downstream of the stage, since connecting
//! a stage to one of its own descendants closes a cycle the reader would
//! refuse, dropping the whole pipeline.
//!
//! Every edit is checked before it leaves (`would_read`): the reader runs
//! over the document as the edit would leave it, and an edit that drops
//! the pipeline (a fixed-arity stage short of an input, a reader losing a
//! required port) is refused with the reader's own words rather than
//! written and reported one reload later, when the tile would hold a
//! dropped graph it could no longer edit.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use geode_core::calibration::{
    BUILTIN_CALIBRATION, CALIBRATION_DOC, PayloadType, Pipeline, Ports, REQUEST_STAGE, StageName,
    check_against, from_doc,
};
use geode_core::config::{Diagnostic, MergedDoc, Severity};
use geode_core::egress_config::EgressSpec;
use geode_core::schema::SchemaSpec;
use geode_shell::frame::ConfigEdit;
use toml::{Table, Value};

/// The kinds the add picker offers, in the reader's order, each with the
/// line the picker shows beside it.
pub const KINDS: &[(&str, &str)] = &[
    ("source", "reads one dataset's documents"),
    (
        "parity_fit",
        "forwards from put\u{2013}call parity over a chain",
    ),
    (
        "implied_rates",
        "repo and box points from parity, dividends and rates",
    ),
    (
        "check",
        "a gate over term points; a failure holds the publish",
    ),
    ("kalman_fit", "a Kalman smoother of one series over knots"),
    ("rasterize", "a fit sampled onto a term grid"),
    ("publish", "rasters sent to a target as one document"),
];

/// The fields the reader requires of each kind, seeded from the builtin
/// document's first stage of that kind when a stage is added. `inputs` is
/// not among them: a new stage is connected afterwards.
const REQUIRED: &[(&str, &[&str])] = &[
    ("source", &["dataset"]),
    ("parity_fit", &["min_strikes", "strike_window"]),
    ("implied_rates", &[]),
    ("check", &["check"]),
    (
        "kalman_fit",
        &["series", "knots", "process_noise", "measurement_noise"],
    ),
    ("rasterize", &["grid"]),
    ("publish", &["document", "target"]),
];

/// The refusal for a name outside `[a-z0-9_]+`.
pub const BAD_NAME: &str = "stage names are lowercase letters, digits and _";

fn edit(object: &str, value: Option<Table>) -> ConfigEdit {
    ConfigEdit {
        doc: CALIBRATION_DOC,
        object: object.to_string(),
        value: value.map(Value::Table),
        origin: None,
    }
}

fn inputs_value(inputs: &[StageName]) -> Value {
    Value::Array(inputs.iter().map(|s| Value::String(s.clone())).collect())
}

/// The port a connect fills on `stage`: the first unfilled fixed port
/// (inputs match fixed ports by position, so it is the port at
/// `inputs.len()`), or a variadic kind's slot with its required type,
/// which is never filled. `None` for a fixed kind whose ports are all
/// taken, and for a stage the pipeline lacks.
pub fn open_port(pipeline: &Pipeline, stage: &str) -> Option<(usize, PayloadType)> {
    let s = pipeline.stage(stage)?;
    match s.kind.ports() {
        Ports::Fixed(ports) => ports.get(s.inputs.len()).map(|(_, t)| (s.inputs.len(), *t)),
        Ports::Variadic { required, .. } => Some((s.inputs.len(), required)),
    }
}

/// Every stage that reads `stage`, directly or through others: the stages
/// a new input of `stage` must not come from.
fn downstream_of(pipeline: &Pipeline, stage: &str) -> BTreeSet<StageName> {
    let mut seen: BTreeSet<StageName> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([stage]);
    while let Some(name) = queue.pop_front() {
        for consumer in pipeline.consumers(name) {
            if seen.insert(consumer.name.clone()) {
                queue.push_back(&consumer.name);
            }
        }
    }
    seen
}

/// The stages `stage` may take as its next input, in document order, each
/// with its output type: of the same pipeline, not the stage itself, not
/// already an input, emitting what the open port takes — or, for a
/// variadic kind, its gate type, which the reader accepts any number of
/// — and not downstream of the stage, which would close a cycle.
pub fn connect_candidates(pipeline: &Pipeline, stage: &str) -> Vec<(StageName, PayloadType)> {
    let Some(s) = pipeline.stage(stage) else {
        return Vec::new();
    };
    let Some((_, wanted)) = open_port(pipeline, stage) else {
        return Vec::new();
    };
    let gate = match s.kind.ports() {
        Ports::Variadic { gates, .. } => gates,
        Ports::Fixed(_) => None,
    };
    let downstream = downstream_of(pipeline, stage);
    pipeline
        .stages
        .iter()
        .filter(|c| c.name != stage)
        .filter(|c| !s.inputs.contains(&c.name))
        .filter(|c| !downstream.contains(&c.name))
        .filter_map(|c| {
            let out = c.kind.output();
            (out == wanted || Some(out) == gate).then(|| (c.name.clone(), out))
        })
        .collect()
}

/// `stage`'s table (`source`, as the document holds it) with `from` added
/// to `inputs` at the open port's position. `Err` when `from` is not a
/// candidate (`connect_candidates`), so a stale picker can write nothing
/// the reader would refuse.
pub fn connect_edit(
    pipeline: &Pipeline,
    stage: &str,
    from: &str,
    source: &Table,
) -> Result<ConfigEdit, String> {
    let s = pipeline
        .stage(stage)
        .ok_or_else(|| format!("unknown stage '{stage}'"))?;
    if !connect_candidates(pipeline, stage)
        .iter()
        .any(|(name, _)| name == from)
    {
        return Err(format!("{from} cannot feed {stage}"));
    }
    let (port, _) =
        open_port(pipeline, stage).ok_or_else(|| format!("{stage} has no open input"))?;
    let mut inputs = s.inputs.clone();
    inputs.insert(port.min(inputs.len()), from.to_string());
    let mut table = source.clone();
    table.insert("inputs".into(), inputs_value(&inputs));
    Ok(edit(stage, Some(table)))
}

/// `stage`'s table with `input` taken out of `inputs`. `Err` when it is
/// not an input of the stage.
pub fn disconnect_edit(
    pipeline: &Pipeline,
    stage: &str,
    input: &str,
    source: &Table,
) -> Result<ConfigEdit, String> {
    let s = pipeline
        .stage(stage)
        .ok_or_else(|| format!("unknown stage '{stage}'"))?;
    let at = s
        .inputs
        .iter()
        .position(|i| i == input)
        .ok_or_else(|| format!("{input} is not an input of {stage}"))?;
    let mut inputs = s.inputs.clone();
    inputs.remove(at);
    let mut table = source.clone();
    table.insert("inputs".into(), inputs_value(&inputs));
    Ok(edit(stage, Some(table)))
}

/// The one batch that deletes `stage`: `None` for the stage, then each
/// consumer's table with the stage struck from its `inputs`, in document
/// order, so no consumer is left naming a stage that is gone. The count is
/// the edges into those consumers, the ones the batch rewrites.
///
/// `None` for a stage removes the user layer's copy and nothing below it,
/// so the tile offers a delete only for a stage no lower layer defines
/// (`CalibrationConfig::inherited`); this function does not know layers.
pub fn delete_edits(
    pipeline: &Pipeline,
    stage: &str,
    sources: &BTreeMap<StageName, Table>,
) -> (Vec<ConfigEdit>, usize) {
    let mut edits = vec![edit(stage, None)];
    let mut edges = 0;
    for consumer in pipeline.consumers(stage) {
        // `sources` and `pipeline` are read off ONE merged document
        // (`content::stage_tables` and `from_doc` in the bridge's
        // `calibration_config`), so every stage of the pipeline has a
        // table here; a consumer without one means the config was built
        // from two documents, and it is left alone rather than rewritten
        // from a guess.
        let Some(source) = sources.get(&consumer.name) else {
            debug_assert!(
                false,
                "{} is a stage of {} but has no table in sources",
                consumer.name, pipeline.name
            );
            continue;
        };
        let inputs: Vec<StageName> = consumer
            .inputs
            .iter()
            .filter(|i| *i != stage)
            .cloned()
            .collect();
        edges += consumer.inputs.len() - inputs.len();
        let mut table = source.clone();
        table.insert("inputs".into(), inputs_value(&inputs));
        edits.push(edit(&consumer.name, Some(table)));
    }
    (edits, edges)
}

/// A new stage `name` of `kind` (a `KINDS` word) in `pipeline` as a
/// whole-object edit: [`add_table`] under the name.
pub fn add_edit(pipeline: &str, name: &str, kind: &str) -> ConfigEdit {
    edit(name, Some(add_table(pipeline, kind)))
}

/// The table a new stage of `kind` in `pipeline` starts from: its
/// `pipeline` and `kind`, then the kind's required fields at the values of
/// the builtin document's first stage of that kind, so the Edit screen
/// the tile's `a` opens shows the kind's rows at real values. No `inputs`:
/// the stage is connected in the dialog, where the reader's arity check
/// is reported on the row.
pub fn add_table(pipeline: &str, kind: &str) -> Table {
    let mut table = Table::new();
    table.insert("pipeline".into(), Value::String(pipeline.to_string()));
    table.insert("kind".into(), Value::String(kind.to_string()));
    let required = REQUIRED
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, fields)| *fields)
        .unwrap_or(&[]);
    if let Some(defaults) = builtin_stage_of(kind) {
        for field in required {
            if let Some(v) = defaults.get(*field) {
                table.insert((*field).into(), v.clone());
            }
        }
    }
    table
}

/// Whether the reader would still accept pipeline `pipeline` with `edits`
/// applied: the document is rebuilt from `sources` (the stage tables, each
/// edit's `Some` replacing and `None` removing its object) and
/// `pipelines_table`, read by `from_doc` and checked by `check_against`
/// over `schema` and the egress targets (`egress`: name → documents, as
/// the config carries them; the check reads names alone). `Ok` when the
/// pipeline is in the result; else the first diagnostic that names the
/// pipeline or one of its stages — the reader's own words, which the tile
/// shows instead of writing an edit it would have to report one reload
/// later.
pub fn would_read(
    sources: &BTreeMap<StageName, Table>,
    pipelines_table: &Table,
    schema: &SchemaSpec,
    egress: &[(String, Vec<String>)],
    pipeline: &str,
    edits: &[ConfigEdit],
) -> Result<(), Diagnostic> {
    let mut stages = sources.clone();
    for e in edits {
        match &e.value {
            Some(Value::Table(t)) => {
                stages.insert(e.object.clone(), t.clone());
            }
            Some(_) => {}
            None => {
                stages.remove(&e.object);
            }
        }
    }
    let mut value = Table::new();
    value.insert("config_version".into(), Value::Integer(1));
    value.insert("pipelines".into(), Value::Table(pipelines_table.clone()));
    value.insert(
        "stages".into(),
        Value::Table(
            stages
                .iter()
                .map(|(k, v)| (k.clone(), Value::Table(v.clone())))
                .collect(),
        ),
    );
    let doc = MergedDoc {
        value,
        provenance: BTreeMap::new(),
    };
    let (pipelines, mut diags) = from_doc(&doc);
    let specs: Vec<EgressSpec> = egress
        .iter()
        .map(|(name, documents)| EgressSpec {
            name: name.clone(),
            adapter: String::new(),
            documents: documents
                .iter()
                .map(|d| (d.clone(), String::new()))
                .collect(),
        })
        .collect();
    let (pipelines, d) = check_against(pipelines, schema, &specs);
    diags.extend(d);
    if pipelines.pipelines.iter().any(|p| p.name == pipeline) {
        return Ok(());
    }
    let of_pipeline = |path: &str| {
        let own = format!("pipelines.{pipeline}");
        if path == own || path.starts_with(&format!("{own}.")) {
            return true;
        }
        path.strip_prefix("stages.")
            .map(|rest| rest.split('.').next().unwrap_or(rest))
            .is_some_and(|stage| {
                stages
                    .get(stage)
                    .and_then(|t| t.get("pipeline"))
                    .and_then(Value::as_str)
                    == Some(pipeline)
            })
    };
    Err(diags
        .into_iter()
        .find(|d| d.path.as_deref().is_some_and(of_pipeline))
        .unwrap_or_else(|| Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!("calibration: pipeline '{pipeline}' would not read"),
            path: Some(format!("pipelines.{pipeline}")),
        }))
}

/// The builtin document's first stage table of `kind`, in document order.
fn builtin_stage_of(kind: &str) -> Option<Table> {
    let doc: Table = toml::from_str(BUILTIN_CALIBRATION).ok()?;
    let stages = doc.get("stages")?.as_table()?;
    stages
        .values()
        .filter_map(Value::as_table)
        .find(|t| t.get("kind").and_then(Value::as_str) == Some(kind))
        .cloned()
}

/// Whether `name` can name a new stage: `[a-z0-9_]+`, not the engine's
/// reserved `request`, and not a stage the document already holds in any
/// pipeline (`taken`: the document's stage tables), since the writer would
/// replace that stage's table whole.
pub fn valid_new_name(taken: &BTreeMap<StageName, Table>, name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(BAD_NAME.to_string());
    }
    if name == REQUEST_STAGE {
        return Err(format!("{name} is reserved for the engine's answer"));
    }
    if taken.contains_key(name) {
        return Err(format!("{name} is already a stage"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::{builtin_pipeline, builtin_sources};
    use geode_core::calibration::{Stage, StageKind};

    fn set_inputs(pipeline: &mut Pipeline, stage: &str, inputs: &[&str]) {
        let s = pipeline
            .stages
            .iter_mut()
            .find(|s| s.name == stage)
            .unwrap();
        s.inputs = inputs.iter().map(|s| s.to_string()).collect();
    }

    fn names(c: &[(StageName, PayloadType)]) -> Vec<&str> {
        c.iter().map(|(n, _)| n.as_str()).collect()
    }

    fn inputs_of(edit: &ConfigEdit) -> Vec<String> {
        edit.value
            .as_ref()
            .and_then(Value::as_table)
            .and_then(|t| t.get("inputs"))
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    /// `repo_fit`'s one port is filled: nothing is offered. `publish` is
    /// offered the rasters and the checks it does not read yet, document
    /// order — a gate already present excludes only itself, since the
    /// reader takes any number of gates. A fresh `kalman_fit` is offered
    /// the term points, not a raster or a source. A stage downstream of the
    /// cursor's is never offered: with `implied_rates` reading `ir2`, `ir2`
    /// may take `parity` but not `implied_rates`.
    #[test]
    fn connect_candidates_exclude_cycles_type_mismatches_and_existing_inputs() {
        let mut p = builtin_pipeline();
        assert_eq!(open_port(&p, "repo_fit"), None);
        assert!(connect_candidates(&p, "repo_fit").is_empty());

        // A second check beside df_monotone, reading parity.
        p.stages.push(Stage {
            name: "df2".into(),
            kind: StageKind::Check {
                check: "monotone_df".into(),
            },
            inputs: vec!["parity".into()],
        });
        set_inputs(&mut p, "publish", &["repo_curve", "df_monotone"]);
        assert_eq!(open_port(&p, "publish"), Some((2, PayloadType::Raster)));
        assert_eq!(
            connect_candidates(&p, "publish"),
            [
                ("box_curve".to_string(), PayloadType::Raster),
                ("df2".to_string(), PayloadType::Check),
            ],
            "the missing raster and the second gate; df_monotone is an input already"
        );
        set_inputs(&mut p, "publish", &["repo_curve"]);
        assert_eq!(
            names(&connect_candidates(&p, "publish")),
            ["df_monotone", "box_curve", "df2"],
            "document order"
        );

        let mut p = builtin_pipeline();
        p.stages.push(Stage {
            name: "new_fit".into(),
            kind: StageKind::KalmanFit {
                series: "x".into(),
                knots: vec![1.0, 2.0],
                process_noise: 1e-4,
                measurement_noise: geode_core::calibration::Noise::Spread,
            },
            inputs: Vec::new(),
        });
        assert_eq!(open_port(&p, "new_fit"), Some((0, PayloadType::TermPoints)));
        assert_eq!(
            connect_candidates(&p, "new_fit"),
            [
                ("parity".to_string(), PayloadType::TermPoints),
                ("implied_rates".to_string(), PayloadType::TermPoints),
            ]
        );

        // ir2 feeds implied_rates; implied_rates is downstream of ir2.
        let mut p = builtin_pipeline();
        p.stages.push(Stage {
            name: "ir2".into(),
            kind: StageKind::ImpliedRates,
            inputs: Vec::new(),
        });
        set_inputs(&mut p, "implied_rates", &["ir2", "dividends", "rates"]);
        assert_eq!(
            names(&connect_candidates(&p, "ir2")),
            ["parity"],
            "implied_rates would close a cycle"
        );
        assert!(connect_candidates(&p, "nope").is_empty());
    }

    /// `implied_rates` short of its third input takes the rates source at
    /// port 2; a variadic publish takes a raster at the end; every other
    /// key of the table rides along; a stage that is no candidate is
    /// refused. A disconnect strikes the one input and refuses a stranger.
    #[test]
    fn connect_edit_places_the_input_at_the_matching_port() {
        let mut p = builtin_pipeline();
        let sources = builtin_sources();
        set_inputs(&mut p, "implied_rates", &["parity", "dividends"]);
        let edit = connect_edit(&p, "implied_rates", "rates", &sources["implied_rates"]).unwrap();
        assert_eq!(edit.doc, CALIBRATION_DOC);
        assert_eq!(edit.object, "implied_rates");
        assert_eq!(edit.origin, None);
        assert_eq!(inputs_of(&edit), ["parity", "dividends", "rates"]);
        let table = edit.value.as_ref().unwrap().as_table().unwrap();
        assert_eq!(table["kind"].as_str(), Some("implied_rates"));
        assert_eq!(table["pipeline"].as_str(), Some("implied_forwards"));
        assert_eq!(
            connect_edit(&p, "implied_rates", "chain", &sources["implied_rates"]),
            Err("chain cannot feed implied_rates".to_string()),
            "a Chain does not fit the Curve port"
        );

        set_inputs(&mut p, "publish", &["repo_curve", "df_monotone"]);
        let edit = connect_edit(&p, "publish", "box_curve", &sources["publish"]).unwrap();
        assert_eq!(inputs_of(&edit), ["repo_curve", "df_monotone", "box_curve"]);
        assert_eq!(
            edit.value.as_ref().unwrap()["target"].as_str(),
            Some("sophis"),
            "the rest of the table is kept"
        );

        let p = builtin_pipeline();
        let edit =
            disconnect_edit(&p, "implied_rates", "dividends", &sources["implied_rates"]).unwrap();
        assert_eq!(inputs_of(&edit), ["parity", "rates"]);
        assert_eq!(
            disconnect_edit(&p, "implied_rates", "chain", &sources["implied_rates"]),
            Err("chain is not an input of implied_rates".to_string())
        );
        let edit = disconnect_edit(&p, "parity", "chain", &sources["parity"]).unwrap();
        assert!(inputs_of(&edit).is_empty(), "an emptied list is written");
    }

    /// Deleting `implied_rates` removes it and rewrites both fits without
    /// it, in one batch of three, counting the two edges into the fits; the
    /// fits keep their other fields. A source nothing reads is one edit.
    #[test]
    fn delete_edits_remove_the_stage_and_strip_every_consumer_in_one_batch() {
        let p = builtin_pipeline();
        let sources = builtin_sources();
        let (edits, edges) = delete_edits(&p, "implied_rates", &sources);
        assert_eq!(edges, 2);
        let objects: Vec<&str> = edits.iter().map(|e| e.object.as_str()).collect();
        assert_eq!(objects, ["implied_rates", "repo_fit", "box_fit"]);
        assert_eq!(edits[0].value, None, "the stage goes");
        for e in &edits[1..] {
            assert!(inputs_of(e).is_empty(), "{}: {:?}", e.object, inputs_of(e));
            let table = e.value.as_ref().unwrap().as_table().unwrap();
            assert_eq!(table["kind"].as_str(), Some("kalman_fit"));
            assert!(
                table.contains_key("knots"),
                "{}: the fit keeps its fields",
                e.object
            );
        }
        // parity feeds implied_rates and df_monotone: two consumers, two edges.
        let (edits, edges) = delete_edits(&p, "parity", &sources);
        assert_eq!(edges, 2);
        assert_eq!(inputs_of(&edits[1]), ["dividends", "rates"]);
        // The publish reads nothing further: one edit, no edges.
        let (edits, edges) = delete_edits(&p, "publish", &sources);
        assert_eq!((edits.len(), edges), (1, 0));
    }

    /// A new Kalman fit carries the reader's required fields at the
    /// builtin's values and nothing else; a source its dataset; the add
    /// picker's kinds are the reader's, in order. A name is refused when
    /// it is not `[a-z0-9_]+`, is the engine's `request`, or is a stage of
    /// the document already.
    #[test]
    fn add_edit_seeds_the_kinds_required_fields_and_new_names_are_validated() {
        let edit = add_edit("implied_forwards", "fit2", "kalman_fit");
        assert_eq!(edit.object, "fit2");
        assert_eq!(
            edit.value.as_ref().and_then(Value::as_table),
            Some(&add_table("implied_forwards", "kalman_fit")),
            "the edit wraps the seed table"
        );
        let table = edit.value.as_ref().unwrap().as_table().unwrap();
        let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "kind",
                "knots",
                "measurement_noise",
                "pipeline",
                "process_noise",
                "series"
            ]
        );
        assert_eq!(table["pipeline"].as_str(), Some("implied_forwards"));
        assert_eq!(table["kind"].as_str(), Some("kalman_fit"));
        assert_eq!(table["series"].as_str(), Some("repo"));
        assert_eq!(table["measurement_noise"].as_str(), Some("spread"));
        assert_eq!(
            table["knots"].as_array().map(Vec::len),
            Some(6),
            "the builtin's knots"
        );
        let source = add_edit("implied_forwards", "src", "source");
        let table = source.value.as_ref().unwrap().as_table().unwrap();
        assert_eq!(table["dataset"].as_str(), Some("option_chain"));
        assert_eq!(table.len(), 3);
        let bare = add_edit("implied_forwards", "ir", "implied_rates");
        assert_eq!(bare.value.as_ref().unwrap().as_table().unwrap().len(), 2);
        // The reader must accept what add_edit seeds: a document of the
        // builtin plus a fresh publish reads clean but for its arity.
        let publish = add_edit("implied_forwards", "pub2", "publish");
        let table = publish.value.as_ref().unwrap().as_table().unwrap();
        assert_eq!(table["document"].as_str(), Some("rate_curve"));
        assert_eq!(table["target"].as_str(), Some("sophis"));

        let kinds: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, geode_core::calibration::doc::KINDS);
        assert!(KINDS.iter().all(|(_, line)| !line.is_empty()));

        let taken = builtin_sources();
        assert_eq!(valid_new_name(&taken, "fit2"), Ok(()));
        assert_eq!(valid_new_name(&taken, ""), Err(BAD_NAME.to_string()));
        assert_eq!(valid_new_name(&taken, "Fit"), Err(BAD_NAME.to_string()));
        assert_eq!(valid_new_name(&taken, "a-b"), Err(BAD_NAME.to_string()));
        assert_eq!(
            valid_new_name(&taken, "repo_fit"),
            Err("repo_fit is already a stage".to_string())
        );
        assert_eq!(
            valid_new_name(&taken, REQUEST_STAGE),
            Err("request is reserved for the engine's answer".to_string())
        );
    }

    /// The reader's verdict on an edit before it is written: striking
    /// `parity`'s only input leaves it short of its one port (refused with
    /// the reader's arity message at `stages.parity.inputs`); striking the
    /// publish's gate still reads; deleting `implied_rates` strips both
    /// fits to no input (refused at the first fit); deleting a stage nothing
    /// reads is fine; a source over a dataset the schema lacks is the
    /// check's refusal; and a stage of another pipeline going wrong is not
    /// this pipeline's problem.
    #[test]
    fn would_read_refuses_an_edit_that_drops_the_pipeline() {
        let p = builtin_pipeline();
        let sources = builtin_sources();
        let schema = crate::core::testing::demo_schema();
        let egress = vec![("sophis".to_string(), vec!["rate_curve".to_string()])];
        let check = |edits: &[ConfigEdit]| {
            would_read(
                &sources,
                &builtin_pipelines_table(),
                &schema,
                &egress,
                "implied_forwards",
                edits,
            )
        };
        assert_eq!(check(&[]), Ok(()));

        let short = disconnect_edit(&p, "parity", "chain", &sources["parity"]).unwrap();
        let refused = check(&[short]).unwrap_err();
        assert_eq!(refused.path.as_deref(), Some("stages.parity.inputs"));
        assert_eq!(
            refused.message,
            "calibration: stage 'parity': parity_fit takes 1 inputs (chain); 0 given"
        );

        let ungated = disconnect_edit(&p, "publish", "df_monotone", &sources["publish"]).unwrap();
        assert_eq!(check(&[ungated]), Ok(()), "a gate is optional");

        let (cascade, _) = delete_edits(&p, "implied_rates", &sources);
        let refused = check(&cascade).unwrap_err();
        // The reader walks the stages table by name: box_fit before repo_fit.
        assert_eq!(refused.path.as_deref(), Some("stages.box_fit.inputs"));
        assert_eq!(
            refused.message,
            "calibration: stage 'box_fit': kalman_fit takes 1 inputs (points); 0 given"
        );

        let (leaf, _) = delete_edits(&p, "publish", &sources);
        assert_eq!(check(&leaf), Ok(()));

        // A source over a dataset the schema lacks (still a Curve, so the
        // types read): the schema check's refusal.
        let mut bad = sources["rates"].clone();
        bad.insert("dataset".into(), Value::String("nope".into()));
        let refused = check(&[edit("rates", Some(bad))]).unwrap_err();
        assert_eq!(refused.path.as_deref(), Some("stages.rates.dataset"));
        assert!(
            refused
                .message
                .contains("which the schema does not declare"),
            "{}",
            refused.message
        );

        // Another pipeline's stage, broken: not this pipeline's problem.
        let mut other = Table::new();
        other.insert("pipeline".into(), Value::String("other".into()));
        other.insert("kind".into(), Value::String("check".into()));
        other.insert("check".into(), Value::String("monotone_df".into()));
        let mut pipelines = builtin_pipelines_table();
        pipelines.insert("other".into(), Value::Table(Table::new()));
        assert_eq!(
            would_read(
                &sources,
                &pipelines,
                &schema,
                &egress,
                "implied_forwards",
                &[edit("lonely", Some(other))],
            ),
            Ok(())
        );
    }

    fn builtin_pipelines_table() -> Table {
        let doc: Table = toml::from_str(BUILTIN_CALIBRATION).unwrap();
        doc["pipelines"].as_table().unwrap().clone()
    }
}
