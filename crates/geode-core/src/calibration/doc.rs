//! The `calibration.toml` reader: `from_doc` over the merged document and
//! `check_against` over the schema and egress targets.
//!
//! Every problem is an Error carrying a `path` (`pipelines.<name>.<field>`,
//! `stages.<name>.<field>` or `stages.<name>.inputs.<i>`) so a dialog can
//! attach it to a field, and the `layer` that supplied the object. A
//! pipeline with any error is dropped whole and the others are kept: a
//! half-read DAG would run stages against inputs the document never
//! connected, which is worse than running nothing.

use super::{Noise, Pipeline, Pipelines, Ports, RunMode, Stage, StageKind};
use crate::config::{Diagnostic, Layer, MergedDoc, Severity};
use crate::egress_config::EgressSpec;
use crate::schema::SchemaSpec;
use std::collections::{BTreeMap, BTreeSet};
use toml::{Table, Value};

pub const KINDS: &[&str] = &[
    "source",
    "parity_fit",
    "implied_rates",
    "check",
    "kalman_fit",
    "rasterize",
    "publish",
];
pub const CHECKS: &[&str] = &["monotone_df"];
pub const GRIDS: &[&str] = &["monthly_to_5y"];

fn diagnostic(
    severity: Severity,
    layer: Option<Layer>,
    path: String,
    message: String,
) -> Diagnostic {
    Diagnostic {
        severity,
        layer,
        file: None,
        message: format!("calibration: {message}"),
        path: Some(path),
    }
}

fn error(layer: Option<Layer>, path: String, message: String) -> Diagnostic {
    diagnostic(Severity::Error, layer, path, message)
}

/// The layer that supplied a whole object of `table` (`pipelines` or
/// `stages`): the document replaces a named object whole, so its root is
/// where provenance is recorded.
fn layer_of(doc: &MergedDoc, table: &str, name: &str) -> Option<Layer> {
    doc.provenance.get(&format!("{table}.{name}")).copied()
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// One top-level table of the document, empty when absent; a non-table
/// value is an error with no object to attribute it to.
fn top_table<'a>(doc: &'a MergedDoc, key: &str, diags: &mut Vec<Diagnostic>) -> Option<&'a Table> {
    match doc.value.get(key) {
        None => None,
        Some(Value::Table(t)) => Some(t),
        Some(_) => {
            diags.push(error(
                None,
                key.to_string(),
                format!("'{key}' is a table of named {key}"),
            ));
            None
        }
    }
}

/// The fields of one stage table, reporting each problem at its field and
/// remembering whether any was found. Reading goes on past a bad field so
/// a dialog sees every problem at once.
struct Fields<'a> {
    name: &'a str,
    table: &'a Table,
    layer: Option<Layer>,
    diags: &'a mut Vec<Diagnostic>,
    ok: bool,
}

impl Fields<'_> {
    fn err(&mut self, field: &str, message: impl std::fmt::Display) {
        self.ok = false;
        self.diags.push(error(
            self.layer,
            format!("stages.{}.{field}", self.name),
            format!("stage '{}': {message}", self.name),
        ));
    }

    fn string(&mut self, field: &str) -> Option<String> {
        match self.table.get(field) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                self.err(field, format!("{field} is a string"));
                None
            }
            None => {
                self.err(field, format!("{field} is required"));
                None
            }
        }
    }

    fn optional_string(&mut self, field: &str) -> Option<String> {
        match self.table.get(field) {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                self.err(field, format!("{field} is a string"));
                None
            }
        }
    }

    /// A string drawn from a closed word list.
    fn word(&mut self, field: &str, allowed: &[&str]) -> Option<String> {
        let value = self.string(field)?;
        if allowed.contains(&value.as_str()) {
            Some(value)
        } else {
            self.err(
                field,
                format!("{field} is one of {}; '{value}' is not", allowed.join(", ")),
            );
            None
        }
    }

    fn positive(&mut self, field: &str) -> Option<f64> {
        match self.table.get(field).map(|v| (v, as_number(v))) {
            Some((_, Some(n))) if n > 0.0 => Some(n),
            Some(_) => {
                self.err(field, format!("{field} is a positive number"));
                None
            }
            None => {
                self.err(field, format!("{field} is required"));
                None
            }
        }
    }

    fn inputs(&mut self) -> Vec<String> {
        let Some(value) = self.table.get("inputs") else {
            return Vec::new();
        };
        let Some(items) = value.as_array() else {
            self.err("inputs", "inputs is an array of stage names");
            return Vec::new();
        };
        let mut inputs = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            match item.as_str() {
                Some(name) => inputs.push(name.to_string()),
                None => self.err(&format!("inputs.{i}"), "an input is a stage name"),
            }
        }
        inputs
    }

    fn kind(&mut self) -> Option<StageKind> {
        let kind = self.word("kind", KINDS)?;
        let kind = match kind.as_str() {
            "source" => StageKind::Source {
                dataset: self.string("dataset")?,
                series: self.optional_string("series"),
            },
            "parity_fit" => {
                let min_strikes = match self.table.get("min_strikes") {
                    Some(Value::Integer(n)) if *n >= 1 => Some(*n as usize),
                    Some(_) => {
                        self.err("min_strikes", "min_strikes is a positive whole number");
                        None
                    }
                    None => {
                        self.err("min_strikes", "min_strikes is required");
                        None
                    }
                };
                let strike_window = match self
                    .table
                    .get("strike_window")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().map(as_number).collect::<Vec<_>>())
                    .as_deref()
                {
                    Some([Some(low), Some(high)]) if low < high => Some((*low, *high)),
                    _ => {
                        self.err(
                            "strike_window",
                            "strike_window is [low, high], two numbers with low < high",
                        );
                        None
                    }
                };
                StageKind::ParityFit {
                    min_strikes: min_strikes?,
                    strike_window: strike_window?,
                }
            }
            "implied_rates" => StageKind::ImpliedRates,
            "check" => StageKind::Check {
                check: self.word("check", CHECKS)?,
            },
            "kalman_fit" => {
                let series = self.string("series");
                let knots: Option<Vec<f64>> = self
                    .table
                    .get("knots")
                    .and_then(Value::as_array)
                    .and_then(|items| items.iter().map(as_number).collect())
                    .filter(|knots: &Vec<f64>| {
                        knots.len() >= 2 && knots.windows(2).all(|w| w[0] < w[1])
                    });
                if knots.is_none() {
                    self.err("knots", "knots is two or more strictly ascending numbers");
                }
                let process_noise = self.positive("process_noise");
                let measurement_noise = match self.table.get("measurement_noise") {
                    Some(Value::String(s)) if s == "spread" => Some(Noise::Spread),
                    Some(v) if as_number(v).is_some_and(|n| n > 0.0) => {
                        as_number(v).map(Noise::Fixed)
                    }
                    _ => {
                        self.err(
                            "measurement_noise",
                            "measurement_noise is \"spread\" or a positive number",
                        );
                        None
                    }
                };
                StageKind::KalmanFit {
                    series: series?,
                    knots: knots?,
                    process_noise: process_noise?,
                    measurement_noise: measurement_noise?,
                }
            }
            "rasterize" => StageKind::Rasterize {
                grid: self.word("grid", GRIDS)?,
            },
            "publish" => {
                let document = self.string("document");
                let target = self.string("target");
                StageKind::Publish {
                    document: document?,
                    target: target?,
                }
            }
            _ => unreachable!("`word` holds kind to KINDS"),
        };
        Some(kind)
    }
}

/// Read one stage's fields. `None` when any field is refused; every
/// refusal is already in `diags`.
fn read_stage(
    name: &str,
    table: &Table,
    layer: Option<Layer>,
    diags: &mut Vec<Diagnostic>,
) -> Option<Stage> {
    let mut fields = Fields {
        name,
        table,
        layer,
        diags,
        ok: true,
    };
    let inputs = fields.inputs();
    let kind = fields.kind();
    if !fields.ok {
        return None;
    }
    Some(Stage {
        name: name.to_string(),
        kind: kind?,
        inputs,
    })
}

/// The first cycle among `stages`' inputs, as the names from the repeated
/// stage round to itself, or `None` when the graph is acyclic. Inputs that
/// name no stage in the list are not edges.
fn find_cycle(stages: &[Stage]) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    fn visit(
        i: usize,
        stages: &[Stage],
        marks: &mut [Mark],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<String>> {
        marks[i] = Mark::Open;
        stack.push(i);
        for input in &stages[i].inputs {
            let Some(j) = stages.iter().position(|s| &s.name == input) else {
                continue;
            };
            match marks[j] {
                Mark::Open => {
                    let start = stack.iter().position(|&k| k == j).unwrap_or(0);
                    let mut cycle: Vec<String> = stack[start..]
                        .iter()
                        .map(|&k| stages[k].name.clone())
                        .collect();
                    cycle.push(stages[j].name.clone());
                    return Some(cycle);
                }
                Mark::New => {
                    if let Some(cycle) = visit(j, stages, marks, stack) {
                        return Some(cycle);
                    }
                }
                Mark::Done => {}
            }
        }
        stack.pop();
        marks[i] = Mark::Done;
        None
    }
    let mut marks = vec![Mark::New; stages.len()];
    let mut stack = Vec::new();
    for i in 0..stages.len() {
        if marks[i] == Mark::New
            && let Some(cycle) = visit(i, stages, &mut marks, &mut stack)
        {
            return Some(cycle);
        }
    }
    None
}

/// Check one pipeline's edges: every input names a stage of this pipeline,
/// each kind's ports are filled by the right number of inputs of the right
/// payload type, and the inputs form no cycle. `owner` maps every stage
/// name in the document to its pipeline, read or not, so an input naming a
/// stage whose own fields failed is still recognised as a stage. Answers
/// whether the pipeline is clean.
fn check_graph(
    doc: &MergedDoc,
    pipeline: &str,
    stages: &[Stage],
    owner: &BTreeMap<String, String>,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    let mut ok = true;
    let output_of = |name: &str| {
        stages
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.kind.output())
    };
    // What an input is, for a message: its payload, and for a source the
    // dataset that decided it, so a misspelt dataset is named where the
    // type check trips rather than left to be guessed.
    let describe = |name: &str, got: super::PayloadType| match stages
        .iter()
        .find(|s| s.name == name)
        .map(|s| &s.kind)
    {
        Some(StageKind::Source { dataset, .. }) => {
            format!("'{name}' is {got:?} (dataset '{dataset}')")
        }
        _ => format!("'{name}' is {got:?}"),
    };
    for stage in stages {
        let layer = layer_of(doc, "stages", &stage.name);
        let mut err = |field: String, message: String| {
            diags.push(error(
                layer,
                format!("stages.{}.{field}", stage.name),
                format!("stage '{}': {message}", stage.name),
            ));
        };
        let mut unresolved = 0;
        for (i, input) in stage.inputs.iter().enumerate() {
            match owner.get(input) {
                None => {
                    err(format!("inputs.{i}"), format!("'{input}' names no stage"));
                    ok = false;
                    unresolved += 1;
                }
                Some(home) => {
                    if home != pipeline {
                        err(
                            format!("inputs.{i}"),
                            format!("'{input}' is a stage of pipeline '{home}', not '{pipeline}'"),
                        );
                        ok = false;
                        unresolved += 1;
                    }
                }
            }
        }
        let kind = stage.kind.kind_name();
        match stage.kind.ports() {
            Ports::Fixed(ports) => {
                if stage.inputs.len() != ports.len() {
                    let names: Vec<&str> = ports.iter().map(|(port, _)| *port).collect();
                    err(
                        "inputs".into(),
                        format!(
                            "{kind} takes {} inputs ({}); {} given",
                            ports.len(),
                            names.join(", "),
                            stage.inputs.len()
                        ),
                    );
                    ok = false;
                } else {
                    for (i, ((port, wanted), input)) in ports.iter().zip(&stage.inputs).enumerate()
                    {
                        if let Some(got) = output_of(input)
                            && got != *wanted
                        {
                            err(
                                format!("inputs.{i}"),
                                format!("{}; port '{port}' takes {wanted:?}", describe(input, got)),
                            );
                            ok = false;
                        }
                    }
                }
            }
            Ports::Variadic { required, gates } => {
                let mut required_seen = false;
                for (i, input) in stage.inputs.iter().enumerate() {
                    let Some(got) = output_of(input) else {
                        continue;
                    };
                    if got == required {
                        required_seen = true;
                    } else if Some(got) != gates {
                        let accepts = match gates {
                            Some(gate) => format!("{required:?} inputs and {gate:?} gates"),
                            None => format!("{required:?} inputs"),
                        };
                        err(
                            format!("inputs.{i}"),
                            format!("{}; {kind} takes {accepts}", describe(input, got)),
                        );
                        ok = false;
                    }
                }
                if !required_seen && unresolved == 0 {
                    err(
                        "inputs".into(),
                        format!("{kind} takes at least one {required:?} input"),
                    );
                    ok = false;
                }
            }
        }
    }
    if let Some(cycle) = find_cycle(stages) {
        let first = &cycle[0];
        let trail: Vec<String> = cycle.iter().map(|name| format!("'{name}'")).collect();
        diags.push(error(
            layer_of(doc, "stages", first),
            format!("stages.{first}.inputs"),
            format!("inputs form a cycle: {}", trail.join(" -> ")),
        ));
        ok = false;
    }
    ok
}

/// Read every pipeline and stage. A pipeline with any error is dropped
/// whole; a stage naming an unknown pipeline is reported and belongs to
/// none, so it drops nothing.
pub fn from_doc(doc: &MergedDoc) -> (Pipelines, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for key in doc.value.keys() {
        if !matches!(key.as_str(), "config_version" | "pipelines" | "stages") {
            diags.push(Diagnostic {
                severity: Severity::Warning,
                layer: doc.provenance.get(key).copied(),
                file: None,
                message: format!("calibration: unknown top-level key '{key}' is ignored"),
                path: Some(key.clone()),
            });
        }
    }
    let empty = Table::new();
    let pipelines_table = top_table(doc, "pipelines", &mut diags).unwrap_or(&empty);
    let stages_table = top_table(doc, "stages", &mut diags).unwrap_or(&empty);

    let mut pipelines: Vec<Pipeline> = Vec::with_capacity(pipelines_table.len());
    for (name, value) in pipelines_table {
        let layer = layer_of(doc, "pipelines", name);
        let mut pipeline = Pipeline {
            name: name.clone(),
            title: name.clone(),
            run: RunMode::OnInput,
            stages: Vec::new(),
        };
        let mut err = |field: &str, message: &str| {
            failed.insert(name.clone());
            diags.push(error(
                layer,
                if field.is_empty() {
                    format!("pipelines.{name}")
                } else {
                    format!("pipelines.{name}.{field}")
                },
                format!("pipeline '{name}': {message}"),
            ));
        };
        match value.as_table() {
            None => err("", "a pipeline is a table"),
            Some(table) => {
                match table.get("title") {
                    None => {}
                    Some(Value::String(title)) => pipeline.title = title.clone(),
                    Some(_) => err("title", "title is a string"),
                }
                match table.get("run").map(Value::as_str) {
                    None => {}
                    Some(Some("on_input")) => pipeline.run = RunMode::OnInput,
                    Some(Some("manual")) => pipeline.run = RunMode::Manual,
                    Some(_) => err("run", "run is \"on_input\" or \"manual\""),
                }
            }
        }
        pipelines.push(pipeline);
    }

    // Every stage with a declared pipeline, read or not, so an input naming
    // a stage whose fields failed is a resolved edge rather than a second,
    // misleading "names no stage".
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    let mut read: Vec<(String, Stage)> = Vec::with_capacity(stages_table.len());
    for (name, value) in stages_table {
        let layer = layer_of(doc, "stages", name);
        let Some(table) = value.as_table() else {
            diags.push(error(
                layer,
                format!("stages.{name}"),
                format!("stage '{name}': a stage is a table"),
            ));
            continue;
        };
        let pipeline = match table.get("pipeline") {
            Some(Value::String(pipeline)) => pipeline,
            _ => {
                diags.push(error(
                    layer,
                    format!("stages.{name}.pipeline"),
                    format!("stage '{name}': pipeline names the pipeline this stage belongs to"),
                ));
                continue;
            }
        };
        if !pipelines.iter().any(|p| &p.name == pipeline) {
            diags.push(error(
                layer,
                format!("stages.{name}.pipeline"),
                format!("stage '{name}': unknown pipeline '{pipeline}'"),
            ));
            continue;
        }
        owner.insert(name.clone(), pipeline.clone());
        // `request` is the one synthetic stage of an outcome the engine never
        // produced (see `engine::REQUEST_STAGE`); a pipeline stage of that
        // name would be painted as that refusal, so the name is reserved.
        if name == super::REQUEST_STAGE {
            diags.push(error(
                layer,
                format!("stages.{name}"),
                format!(
                    "stage '{name}': the name is reserved for the outcome of a request the \
                     engine never ran"
                ),
            ));
            failed.insert(pipeline.clone());
            continue;
        }
        match read_stage(name, table, layer, &mut diags) {
            Some(stage) => read.push((pipeline.clone(), stage)),
            None => {
                failed.insert(pipeline.clone());
            }
        }
    }

    for pipeline in &mut pipelines {
        pipeline.stages = read
            .iter()
            .filter(|(home, _)| *home == pipeline.name)
            .map(|(_, stage)| stage.clone())
            .collect();
        if !check_graph(doc, &pipeline.name, &pipeline.stages, &owner, &mut diags) {
            failed.insert(pipeline.name.clone());
        }
    }
    pipelines.retain(|p| !failed.contains(&p.name));
    (Pipelines { pipelines }, diags)
}

/// The checks that need the schema and the egress targets: a `source`
/// whose dataset is not a document-family dataset, a `publish` whose
/// `document` is not one or whose `target` is not an egress target. A
/// target's own document list is not consulted here; the upload path
/// answers that at publish time. Each miss is a Warning, not an Error,
/// and drops the pipeline from the returned set: the builtin pipeline
/// ships in every build while its datasets and target exist only where a
/// desk declares them, so a desk without those feeds must start clean
/// with the pipeline absent rather than with permanent Errors, as a
/// `vol_views` problem does. Every message says the pipeline was dropped
/// and why. `from_doc`'s structural problems stay Errors. Separate from
/// `from_doc` because the reader has no schema in hand; a caller runs both.
pub fn check_against(
    pipelines: Pipelines,
    schema: &SchemaSpec,
    egress: &[EgressSpec],
) -> (Pipelines, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    // Why a dataset name fails the document test, for the message.
    let dataset_problem = |name: &str| match schema.dataset(name) {
        None => Some("which the schema does not declare"),
        Some(d) if !d.is_document() => Some("which is not a document dataset"),
        Some(_) => None,
    };
    let kept = pipelines
        .pipelines
        .into_iter()
        .filter(|pipeline| {
            let mut ok = true;
            for stage in &pipeline.stages {
                let mut dropped = |field: &str, message: String| {
                    diags.push(diagnostic(
                        Severity::Warning,
                        None,
                        format!("stages.{}.{field}", stage.name),
                        format!(
                            "pipeline '{}' dropped: stage '{}' {message}",
                            pipeline.name, stage.name
                        ),
                    ));
                    ok = false;
                };
                match &stage.kind {
                    StageKind::Source { dataset, .. } => {
                        if let Some(why) = dataset_problem(dataset) {
                            dropped("dataset", format!("reads dataset '{dataset}', {why}"));
                        }
                    }
                    StageKind::Publish { document, target } => {
                        if let Some(why) = dataset_problem(document) {
                            dropped(
                                "document",
                                format!("publishes document '{document}', {why}"),
                            );
                        }
                        if !egress.iter().any(|e| e.name == *target) {
                            dropped(
                                "target",
                                format!(
                                    "publishes to target '{target}', which egress.toml does not declare"
                                ),
                            );
                        }
                    }
                    _ => {}
                }
            }
            ok
        })
        .collect();
    (Pipelines { pipelines: kept }, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::{BUILTIN_CALIBRATION, CALIBRATION_DOC, PayloadType};
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> MergedDoc {
        merge_docs(
            CALIBRATION_DOC,
            &[LayerDoc::builtin(CALIBRATION_DOC, text).unwrap()],
        )
    }

    fn read(text: &str) -> (Pipelines, Vec<Diagnostic>) {
        from_doc(&doc(text))
    }

    fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
        diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect()
    }

    fn error_at<'a>(diags: &'a [Diagnostic], path: &str) -> &'a Diagnostic {
        diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.path.as_deref() == Some(path))
            .unwrap_or_else(|| panic!("no error at {path}: {diags:?}"))
    }

    /// A minimal chain → parity → fit prefix every variation below extends.
    const PREFIX: &str = r#"
[pipelines.p]

[stages.chain]
pipeline = "p"
kind = "source"
dataset = "option_chain"

[stages.parity]
pipeline = "p"
kind = "parity_fit"
inputs = ["chain"]
min_strikes = 6
strike_window = [0.8, 1.2]
"#;

    fn kalman(name: &str, inputs: &str, extra: &str) -> String {
        format!(
            r#"
[stages.{name}]
pipeline = "p"
kind = "kalman_fit"
inputs = {inputs}
series = "repo"
knots = [0.25, 0.5, 1.0]
process_noise = 1.0e-4
measurement_noise = "spread"
{extra}
"#
        )
    }

    #[test]
    fn the_builtin_document_reads_one_pipeline_of_eleven_stages_without_diagnostics() {
        let (pipelines, diags) = read(BUILTIN_CALIBRATION);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(pipelines.pipelines.len(), 1);
        let p = &pipelines.pipelines[0];
        assert_eq!(p.name, "implied_forwards");
        assert_eq!(p.title, "Implied forwards");
        assert_eq!(p.run, RunMode::OnInput);
        assert_eq!(p.stages.len(), 11);
        assert_eq!(
            p.stage("publish").expect("publish stage").inputs,
            vec!["repo_curve", "box_curve", "df_monotone"]
        );
        assert_eq!(
            p.stage("rates").expect("rates stage").kind,
            StageKind::Source {
                dataset: "rate_curve".into(),
                series: Some("funding".into()),
            }
        );
        let names: Vec<&str> = p
            .consumers("parity")
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["implied_rates", "df_monotone"]);
    }

    #[test]
    fn a_stage_naming_an_undeclared_pipeline_is_an_error_with_its_path() {
        let text = format!(
            "{PREFIX}\n[stages.x]\npipeline = \"nope\"\nkind = \"source\"\ndataset = \"option_chain\"\n"
        );
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.x.pipeline");
        assert!(d.message.contains("unknown pipeline"), "{}", d.message);
        assert_eq!(d.layer, Some(Layer::Builtin));
        // The orphan stage touches no declared pipeline, so `p` is kept.
        assert_eq!(pipelines.pipelines.len(), 1, "{pipelines:?}");
    }

    #[test]
    fn a_stage_named_request_is_refused() {
        let text = format!(
            "{PREFIX}\n[stages.request]\npipeline = \"p\"\nkind = \"check\"\n\
             inputs = [\"parity\"]\ncheck = \"monotone_df\"\n"
        );
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.request");
        assert_eq!(
            d.message,
            "calibration: stage 'request': the name is reserved for the outcome of a request \
             the engine never ran"
        );
        assert_eq!(d.layer, Some(Layer::Builtin));
        assert!(pipelines.pipelines.is_empty(), "{pipelines:?}");
        // The same stage under another name is clean: only the name is refused.
        let (pipelines, diags) = read(&text.replace("[stages.request]", "[stages.gate]"));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(pipelines.pipelines.len(), 1);
    }

    #[test]
    fn an_input_naming_a_stage_of_another_pipeline_is_an_error() {
        let text = format!(
            r#"{PREFIX}
[pipelines.b]

[stages.b_fit]
pipeline = "b"
kind = "parity_fit"
inputs = ["chain"]
min_strikes = 6
strike_window = [0.8, 1.2]
"#
        );
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.b_fit.inputs.0");
        assert!(d.message.contains("'p'"), "{}", d.message);
        let names: Vec<&str> = pipelines
            .pipelines
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["p"], "b is dropped, p is kept");
    }

    #[test]
    fn an_unknown_input_name_is_an_error_at_its_index() {
        let text = format!(
            r#"{PREFIX}
[stages.fit]
pipeline = "p"
kind = "check"
inputs = ["ghost"]
check = "monotone_df"
"#
        );
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.fit.inputs.0");
        assert!(d.message.contains("ghost"), "{}", d.message);
        assert!(pipelines.pipelines.is_empty(), "{pipelines:?}");
    }

    #[test]
    fn an_input_cycle_drops_the_pipeline_whole() {
        // Two implied_rates stages reading each other as `parity`: every
        // port is filled with the right payload (TermPoints, Schedule,
        // Curve), so the cycle is the only error and the drop is its doing.
        let text = r#"
[pipelines.p]

[stages.dividends]
pipeline = "p"
kind = "source"
dataset = "dividend_schedule"

[stages.rates]
pipeline = "p"
kind = "source"
dataset = "rate_curve"

[stages.a]
pipeline = "p"
kind = "implied_rates"
inputs = ["b", "dividends", "rates"]

[stages.b]
pipeline = "p"
kind = "implied_rates"
inputs = ["a", "dividends", "rates"]
"#;
        let (pipelines, diags) = read(text);
        assert!(pipelines.pipelines.is_empty(), "{pipelines:?}");
        let all = errors(&diags);
        assert_eq!(all.len(), 1, "the cycle is the only error: {diags:?}");
        let cycle = all[0];
        assert!(cycle.message.contains("cycle"), "{}", cycle.message);
        assert!(cycle.message.contains("'a'") && cycle.message.contains("'b'"));
        assert_eq!(cycle.path.as_deref(), Some("stages.a.inputs"));
    }

    #[test]
    fn a_port_type_mismatch_is_an_error() {
        let text = format!("{PREFIX}{}", kalman("fit", r#"["chain"]"#, ""));
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.fit.inputs.0");
        assert!(d.message.contains("TermPoints"), "{}", d.message);
        assert!(d.message.contains("Chain"), "{}", d.message);
        assert!(pipelines.pipelines.is_empty(), "{pipelines:?}");
    }

    #[test]
    fn fixed_arity_kinds_refuse_a_missing_or_extra_input() {
        let sources = r#"
[stages.dividends]
pipeline = "p"
kind = "source"
dataset = "dividend_schedule"

[stages.rates]
pipeline = "p"
kind = "source"
dataset = "rate_curve"
"#;
        for inputs in [
            r#"["parity", "dividends"]"#,
            r#"["parity", "dividends", "rates", "chain"]"#,
        ] {
            let text = format!(
                "{PREFIX}{sources}\n[stages.ir]\npipeline = \"p\"\nkind = \"implied_rates\"\ninputs = {inputs}\n"
            );
            let (pipelines, diags) = read(&text);
            let d = error_at(&diags, "stages.ir.inputs");
            assert!(d.message.contains("3 inputs"), "{}", d.message);
            assert!(pipelines.pipelines.is_empty(), "{inputs}: {pipelines:?}");
        }
        let text = format!(
            "{PREFIX}{sources}\n[stages.ir]\npipeline = \"p\"\nkind = \"implied_rates\"\ninputs = [\"parity\", \"dividends\", \"rates\"]\n"
        );
        let (pipelines, diags) = read(&text);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(pipelines.pipelines.len(), 1);
    }

    #[test]
    fn a_publish_accepts_rasters_and_check_gates_but_nothing_else() {
        let body = format!(
            r#"{PREFIX}
[stages.df_monotone]
pipeline = "p"
kind = "check"
inputs = ["parity"]
check = "monotone_df"
{}
[stages.repo_curve]
pipeline = "p"
kind = "rasterize"
inputs = ["repo_fit"]
grid = "monthly_to_5y"
"#,
            kalman("repo_fit", r#"["parity"]"#, "")
        );
        let publish = |inputs: &str| {
            format!(
                "{body}\n[stages.publish]\npipeline = \"p\"\nkind = \"publish\"\ninputs = {inputs}\ndocument = \"rate_curve\"\ntarget = \"sophis\"\n"
            )
        };
        let (pipelines, diags) = read(&publish(r#"["repo_curve", "df_monotone"]"#));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(pipelines.pipelines.len(), 1);

        let (pipelines, diags) = read(&publish(r#"["repo_curve", "parity"]"#));
        let d = error_at(&diags, "stages.publish.inputs.1");
        assert!(d.message.contains("TermPoints"), "{}", d.message);
        assert!(pipelines.pipelines.is_empty(), "{pipelines:?}");

        // A gate alone publishes nothing.
        let (pipelines, diags) = read(&publish(r#"["df_monotone"]"#));
        let d = error_at(&diags, "stages.publish.inputs");
        assert!(d.message.contains("Raster"), "{}", d.message);
        assert!(pipelines.pipelines.is_empty(), "{pipelines:?}");
    }

    #[test]
    fn knots_must_be_two_or_more_and_strictly_ascending() {
        for knots in ["[1.0]", "[]", "[0.5, 0.5]", "[1.0, 0.5, 2.0]"] {
            let text = format!(
                r#"{PREFIX}
[stages.fit]
pipeline = "p"
kind = "kalman_fit"
inputs = ["parity"]
series = "repo"
knots = {knots}
process_noise = 1.0e-4
measurement_noise = "spread"
"#
            );
            let (pipelines, diags) = read(&text);
            error_at(&diags, "stages.fit.knots");
            assert!(pipelines.pipelines.is_empty(), "{knots}: {pipelines:?}");
        }
        let text = format!("{PREFIX}{}", kalman("fit", r#"["parity"]"#, ""));
        let (pipelines, diags) = read(&text);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            pipelines.pipelines[0].stage("fit").unwrap().kind,
            StageKind::KalmanFit {
                series: "repo".into(),
                knots: vec![0.25, 0.5, 1.0],
                process_noise: 1.0e-4,
                measurement_noise: Noise::Spread,
            }
        );
    }

    #[test]
    fn noise_fields_are_validated() {
        let fit = |process: &str, measurement: &str| {
            format!(
                r#"{PREFIX}
[stages.fit]
pipeline = "p"
kind = "kalman_fit"
inputs = ["parity"]
series = "repo"
knots = [0.25, 0.5]
process_noise = {process}
measurement_noise = {measurement}
"#
            )
        };
        let (pipelines, diags) = read(&fit("0", r#""spread""#));
        error_at(&diags, "stages.fit.process_noise");
        assert!(pipelines.pipelines.is_empty());

        let (pipelines, diags) = read(&fit("1.0e-4", r#""foo""#));
        error_at(&diags, "stages.fit.measurement_noise");
        assert!(pipelines.pipelines.is_empty());

        let (pipelines, diags) = read(&fit("1.0e-4", "-0.5"));
        error_at(&diags, "stages.fit.measurement_noise");
        assert!(pipelines.pipelines.is_empty());

        let (pipelines, diags) = read(&fit("1.0e-4", "0.5"));
        assert!(diags.is_empty(), "{diags:?}");
        match &pipelines.pipelines[0].stage("fit").unwrap().kind {
            StageKind::KalmanFit {
                measurement_noise, ..
            } => assert_eq!(*measurement_noise, Noise::Fixed(0.5)),
            other => panic!("{other:?}"),
        }
        let (pipelines, diags) = read(&fit("1.0e-4", r#""spread""#));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(pipelines.pipelines.len(), 1);
    }

    #[test]
    fn unknown_kind_grid_and_check_names_are_errors() {
        let text = format!(
            "{PREFIX}\n[stages.x]\npipeline = \"p\"\nkind = \"smooth\"\ninputs = [\"parity\"]\n"
        );
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.x.kind");
        assert!(d.message.contains("smooth"), "{}", d.message);
        assert!(pipelines.pipelines.is_empty());

        let text = format!(
            "{PREFIX}{}\n[stages.r]\npipeline = \"p\"\nkind = \"rasterize\"\ninputs = [\"fit\"]\ngrid = \"weekly\"\n",
            kalman("fit", r#"["parity"]"#, "")
        );
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.r.grid");
        assert!(d.message.contains("weekly"), "{}", d.message);
        assert!(pipelines.pipelines.is_empty());

        let text = format!(
            "{PREFIX}\n[stages.c]\npipeline = \"p\"\nkind = \"check\"\ninputs = [\"parity\"]\ncheck = \"positive\"\n"
        );
        let (pipelines, diags) = read(&text);
        let d = error_at(&diags, "stages.c.check");
        assert!(d.message.contains("positive"), "{}", d.message);
        assert!(pipelines.pipelines.is_empty());
    }

    #[test]
    fn a_pipeline_with_no_title_or_run_takes_defaults() {
        let (pipelines, diags) = read(PREFIX);
        assert!(diags.is_empty(), "{diags:?}");
        let p = &pipelines.pipelines[0];
        assert_eq!(p.title, "p");
        assert_eq!(p.run, RunMode::OnInput);

        let text = PREFIX.replace("[pipelines.p]", "[pipelines.p]\nrun = \"manual\"");
        let (pipelines, _) = read(&text);
        assert_eq!(pipelines.pipelines[0].run, RunMode::Manual);

        let text = PREFIX.replace("[pipelines.p]", "[pipelines.p]\nrun = \"sometimes\"");
        let (pipelines, diags) = read(&text);
        error_at(&diags, "pipelines.p.run");
        assert!(pipelines.pipelines.is_empty());
    }

    #[test]
    fn topological_puts_sources_first_and_keeps_document_order_for_ties() {
        // Document order: a fit, its parity, a late second source, the chain.
        let text = format!(
            r#"
[pipelines.p]
{}
[stages.parity]
pipeline = "p"
kind = "parity_fit"
inputs = ["chain"]
min_strikes = 6
strike_window = [0.8, 1.2]

[stages.late_source]
pipeline = "p"
kind = "source"
dataset = "dividend_schedule"

[stages.chain]
pipeline = "p"
kind = "source"
dataset = "option_chain"
"#,
            kalman("fit", r#"["parity"]"#, "")
        );
        let (pipelines, diags) = read(&text);
        assert!(diags.is_empty(), "{diags:?}");
        let order: Vec<&str> = pipelines.pipelines[0]
            .topological()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(order, vec!["late_source", "chain", "parity", "fit"]);
        assert_eq!(
            pipelines.pipelines[0].stages[0].name, "fit",
            "document order is kept"
        );
    }

    #[test]
    fn a_higher_layer_replaces_one_stage_whole_and_keeps_the_others() {
        // A user stage with no `min_strikes` must not inherit the builtin
        // value: the object is replaced whole, so the gap is an error on the
        // user layer, and the other ten builtin stages are still present.
        let user = r#"
[stages.parity]
pipeline = "implied_forwards"
kind = "parity_fit"
inputs = ["chain"]
strike_window = [0.9, 1.1]
"#;
        let merged = merge_docs(
            CALIBRATION_DOC,
            &[
                LayerDoc::builtin(CALIBRATION_DOC, BUILTIN_CALIBRATION).unwrap(),
                LayerDoc {
                    layer: Layer::User,
                    name: CALIBRATION_DOC.to_string(),
                    file: std::path::PathBuf::from("user/calibration.toml"),
                    table: user.parse().unwrap(),
                },
            ],
        );
        assert_eq!(merged.value["stages"].as_table().unwrap().len(), 11);
        let (pipelines, diags) = from_doc(&merged);
        let d = error_at(&diags, "stages.parity.min_strikes");
        assert_eq!(d.layer, Some(Layer::User));
        assert!(pipelines.pipelines.is_empty());
    }

    #[test]
    fn ports_and_outputs_follow_the_kind_table() {
        let source = |dataset: &str| StageKind::Source {
            dataset: dataset.into(),
            series: None,
        };
        assert_eq!(source("option_chain").output(), PayloadType::Chain);
        assert_eq!(source("dividend_schedule").output(), PayloadType::Schedule);
        assert_eq!(source("rate_curve").output(), PayloadType::Curve);
        assert_eq!(source("option_chain").kind_name(), "source");
        assert_eq!(StageKind::ImpliedRates.output(), PayloadType::TermPoints);
        assert_eq!(
            StageKind::ImpliedRates.ports(),
            Ports::Fixed(vec![
                ("parity", PayloadType::TermPoints),
                ("dividends", PayloadType::Schedule),
                ("rates", PayloadType::Curve),
            ])
        );
        let publish = StageKind::Publish {
            document: "rate_curve".into(),
            target: "sophis".into(),
        };
        assert_eq!(publish.output(), PayloadType::PublishReady);
        assert_eq!(
            publish.ports(),
            Ports::Variadic {
                required: PayloadType::Raster,
                gates: Some(PayloadType::Check),
            }
        );
    }

    fn schema() -> SchemaSpec {
        // Three document datasets the builtin pipeline names, a fourth
        // document dataset, and one measures dataset to exercise the
        // "document family" rejection.
        let mut text = String::from(
            r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#,
        );
        for name in [
            "option_chain",
            "dividend_schedule",
            "rate_curve",
            "cvi_params",
        ] {
            text.push_str(&format!(
                r#"
[{name}]
family = "document"
key = ["underlying_ref"]
axes = ["term"]
[{name}.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[{name}.columns.term]
type = "date"
role = "axis"
[{name}.columns.value]
type = "f64"
role = "value"
"#
            ));
        }
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", &text).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema
    }

    fn sophis() -> Vec<EgressSpec> {
        vec![EgressSpec {
            name: "sophis".into(),
            adapter: "demo_bus".into(),
            documents: vec![("rate_curve".into(), "marketdata/rates/{key}".into())],
        }]
    }

    /// The one Warning at `path`; a miss of `check_against` is never an
    /// Error, since the builtin pipeline ships to desks without its feeds.
    fn warning_at<'a>(diags: &'a [Diagnostic], path: &str) -> &'a Diagnostic {
        assert!(
            errors(diags).is_empty(),
            "check_against reports no Errors: {diags:?}"
        );
        let found: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning && d.path.as_deref() == Some(path))
            .collect();
        assert_eq!(found.len(), 1, "one warning at {path}: {diags:?}");
        found[0]
    }

    #[test]
    fn check_against_drops_a_pipeline_whose_source_dataset_or_publish_target_is_unknown() {
        let (pipelines, diags) = read(BUILTIN_CALIBRATION);
        assert!(diags.is_empty());
        let (kept, diags) = check_against(pipelines, &schema(), &sophis());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(kept.pipelines.len(), 1);

        // `rates` reads a Curve whatever its dataset is called, so the typo
        // reaches this pass; a typo on `chain` makes it a Curve too, and
        // `from_doc` refuses the pipeline at parity's port first, naming
        // the dataset.
        let text = BUILTIN_CALIBRATION.replace(r#"dataset  = "rate_curve""#, r#"dataset = "nope""#);
        let (pipelines, diags) = read(&text);
        assert!(diags.is_empty(), "{diags:?}");
        let (kept, diags) = check_against(pipelines, &schema(), &sophis());
        let d = warning_at(&diags, "stages.rates.dataset");
        assert_eq!(
            d.message,
            "calibration: pipeline 'implied_forwards' dropped: stage 'rates' reads dataset \
             'nope', which the schema does not declare"
        );
        assert!(kept.pipelines.is_empty(), "the pipeline is dropped");

        let text =
            BUILTIN_CALIBRATION.replace(r#"dataset  = "option_chain""#, r#"dataset = "nope""#);
        let (pipelines, diags) = read(&text);
        assert!(pipelines.pipelines.is_empty());
        let d = error_at(&diags, "stages.parity.inputs.0");
        assert!(d.message.contains("'nope'"), "{}", d.message);

        // A declared dataset of another family is named as such.
        let text = BUILTIN_CALIBRATION
            .replace(r#"dataset  = "rate_curve""#, r#"dataset = "risk_snapshot""#);
        let (pipelines, _) = read(&text);
        let (kept, diags) = check_against(pipelines, &schema(), &sophis());
        let d = warning_at(&diags, "stages.rates.dataset");
        assert!(
            d.message
                .ends_with("reads dataset 'risk_snapshot', which is not a document dataset"),
            "{}",
            d.message
        );
        assert!(kept.pipelines.is_empty());

        let text = BUILTIN_CALIBRATION.replace(r#"target   = "sophis""#, r#"target = "prod""#);
        let (pipelines, _) = read(&text);
        let (kept, diags) = check_against(pipelines, &schema(), &sophis());
        let d = warning_at(&diags, "stages.publish.target");
        assert_eq!(
            d.message,
            "calibration: pipeline 'implied_forwards' dropped: stage 'publish' publishes to \
             target 'prod', which egress.toml does not declare"
        );
        assert!(kept.pipelines.is_empty(), "the pipeline is dropped");

        let text =
            BUILTIN_CALIBRATION.replace(r#"document = "rate_curve""#, r#"document = "cvi_params""#);
        let (pipelines, _) = read(&text);
        let (kept, diags) = check_against(pipelines, &schema(), &sophis());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(kept.pipelines.len(), 1);

        let text = BUILTIN_CALIBRATION.replace(
            r#"document = "rate_curve""#,
            r#"document = "risk_snapshot""#,
        );
        let (pipelines, _) = read(&text);
        let (kept, diags) = check_against(pipelines, &schema(), &sophis());
        let d = warning_at(&diags, "stages.publish.document");
        assert!(
            d.message
                .ends_with("publishes document 'risk_snapshot', which is not a document dataset"),
            "{}",
            d.message
        );
        assert!(kept.pipelines.is_empty(), "the pipeline is dropped");
    }
}
