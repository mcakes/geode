//! The Calibration stages domain: `calibration.toml`'s `stages` table, one
//! object per stage. The document keeps its stages one level down
//! (`geode_core::config::object_table`), so the scaffold's readers and
//! writers resolve the container; this adapter sees a stage's own table.
//!
//! A stage is its `pipeline`, its `kind`, its `inputs` and the kind's
//! parameters. The fields follow the kind the table holds when the edit
//! opens: `inputs` is one ordered list of the pipeline's other stages,
//! ticked when the stage reads them and in port order, with no catalogue
//! (an input is a stage of the same pipeline or nothing); the parameters
//! are each kind's from the reader's rules. `kind` is a choice whose step
//! is refused (`step_refusal`): the parameter rows are derived from it when
//! the stage opens, so a change would leave rows of the old kind under the
//! new one — delete and re-add the stage instead. Floats and number lists
//! are `Text` rows with `parse_text` (the dialog's `Number` is integer
//! only); `min_strikes` is a `Number`.
//!
//! Every field is `Destination::Doc`: a stage has no presentation overlay.
//! Validation runs the document's own reader over the merged document with
//! this draft's table in the stage's place and keeps what it says about
//! this stage, so the dialog agrees with the tile's bridge on every rule.

use std::collections::BTreeSet;

use geode_core::calibration::doc::{CHECKS, GRIDS, KINDS};
use geode_core::calibration::{CALIBRATION_DOC, check_against, from_doc};
use geode_core::config::{Config, Diagnostic, MergedDoc};
use geode_core::egress_config::EgressSpec;
use geode_core::schema::SchemaSpec;

use super::{Destination, Draft, EditRow, Field, FieldKind, ListItem};

pub const DOC: &str = CALIBRATION_DOC;

/// The refusal a step of the `kind` row gets.
pub const KIND_REFUSAL: &str = "delete and re-add the stage to change its kind";

/// The `series` a Kalman fit produces.
const FIT_SERIES: &[&str] = &["repo", "box"];

/// Every kind's parameter keys: what the writer clears before it writes
/// the current kind's, so a table holds one kind's parameters.
const PARAMETER_KEYS: &[&str] = &[
    "dataset",
    "series",
    "min_strikes",
    "strike_window",
    "check",
    "knots",
    "process_noise",
    "measurement_noise",
    "grid",
    "document",
    "target",
];

/// The word painted for a stage field the table lacks.
const MISSING: &str = "\u{2014}";

/// `{pipeline} · {kind}`, as the table spells them.
pub fn summary_in(_config: &Config, _name: &str, value: &toml::Value) -> String {
    let word = |key: &str| {
        value
            .get(key)
            .and_then(toml::Value::as_str)
            .unwrap_or(MISSING)
    };
    format!("{} \u{b7} {}", word("pipeline"), word("kind"))
}

/// The merged `stages` table, empty when the document lacks one.
fn stages_of(config: &Config) -> toml::Table {
    config
        .doc(DOC)
        .and_then(|doc| super::objects_in(&doc.value, DOC))
        .cloned()
        .unwrap_or_default()
}

/// The pipelines the document declares, in document order.
fn pipelines_of(config: &Config) -> Vec<String> {
    config
        .doc(DOC)
        .and_then(|doc| doc.value.get("pipelines"))
        .and_then(toml::Value::as_table)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

/// The document-family datasets the schema declares, in schema order:
/// what a source reads and a publish writes.
fn document_datasets(config: &Config) -> Vec<String> {
    config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default()
        .datasets
        .iter()
        .filter(|d| d.is_document())
        .map(|d| d.name.clone())
        .collect()
}

/// The egress targets `egress.toml` names, in document order. Read as
/// names from the merged document rather than through its reader, which
/// also checks each target's documents against the schema: a target with
/// a bad document list is still the target a publish names.
fn targets_of(config: &Config) -> Vec<String> {
    config
        .doc("egress")
        .map(|d| {
            d.value
                .iter()
                .filter(|(k, v)| *k != "config_version" && v.is_table())
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The fields of `object`'s stage, from its table in the merged document;
/// a stage the document lacks opens with the empty shape (`new_fields`).
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let stages = stages_of(config);
    let table = object
        .and_then(|name| stages.get(name)?.as_table())
        .cloned()
        .unwrap_or_default();
    fields_of(config, &stages, object.unwrap_or_default(), &table)
}

/// The fields a new stage named `name` opens with: the document's first
/// pipeline, kind `source` over the first document dataset, and every
/// stage of that pipeline unticked in `inputs` — the stage itself left
/// out, as it is from an existing stage's list.
pub fn new_fields(config: &Config, name: &str) -> Vec<Field> {
    fields_of(config, &stages_of(config), name, &toml::Table::new())
}

/// The fields a new stage seeded with `table` opens with (a tile's `a`:
/// `pipeline`, `kind` and the kind's parameters): the kind's rows at the
/// seeded values. The stage is not in the document, so the inputs list
/// is the seeded pipeline's stages, every one unticked, and no name need
/// be left out.
pub fn fields_from_table(config: &Config, table: &toml::Table) -> Vec<Field> {
    fields_of(config, &stages_of(config), "", table)
}

fn text(key: &str, label: &str, value: String) -> Field {
    super::views::text_row(key, label, value, Destination::Doc)
}

/// A `Choice` over `options` at `current`, which is added as the last
/// option when the options lack it, so a value the pickers do not offer
/// (a hand-written word, a dataset the schema no longer declares) stays
/// visible for repair rather than being read as the first option, which
/// a later write would then put in the file.
fn choice(key: &str, label: &str, mut options: Vec<String>, current: &str) -> Field {
    if !current.is_empty() && !options.iter().any(|o| o == current) {
        options.push(current.to_string());
    }
    if options.is_empty() {
        options.push(String::new());
    }
    super::views::choice_row(key, label, options, current, Destination::Doc)
}

fn word<'a>(table: &'a toml::Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(toml::Value::as_str)
}

/// A number as the text row shows it: an integer as itself, a float by
/// its shortest round-tripping spelling (`0.0001`, `1`, `0.25`).
fn number_text(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::Integer(n) => Some(n.to_string()),
        toml::Value::Float(f) => Some(format!("{f}")),
        _ => None,
    }
}

/// A number array as a comma-separated text, or `None` when any element
/// is not a number.
fn numbers_text(value: &toml::Value) -> Option<String> {
    let items = value.as_array()?;
    let words: Option<Vec<String>> = items.iter().map(number_text).collect();
    Some(words?.join(", "))
}

/// The table's value for `key` as the text row shows it, else `default`;
/// a value of the wrong shape is shown as the file spells it so it can be
/// repaired, not replaced by the default.
fn text_of(table: &toml::Table, key: &str, default: &str) -> String {
    match table.get(key) {
        None => default.to_string(),
        Some(v) => number_text(v)
            .or_else(|| numbers_text(v))
            .or_else(|| v.as_str().map(str::to_string))
            .unwrap_or_else(|| v.to_string()),
    }
}

fn fields_of(config: &Config, stages: &toml::Table, name: &str, table: &toml::Table) -> Vec<Field> {
    let pipelines = pipelines_of(config);
    let pipeline = word(table, "pipeline")
        .map(str::to_string)
        .or_else(|| pipelines.first().cloned())
        .unwrap_or_default();
    let kind = word(table, "kind").unwrap_or("source").to_string();
    let datasets = document_datasets(config);
    let first_dataset = datasets.first().cloned().unwrap_or_default();

    // The stage's inputs first, in port order (a name no stage of the
    // pipeline has stays, ticked, so it can be unticked), then the
    // pipeline's other stages in document order, unticked.
    let read: Vec<String> = table
        .get("inputs")
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut items: Vec<ListItem> = read.iter().map(|n| item(n, true)).collect();
    for (other, value) in stages {
        if other == name || read.contains(other) {
            continue;
        }
        if value.get("pipeline").and_then(toml::Value::as_str) == Some(pipeline.as_str()) {
            items.push(item(other, false));
        }
    }

    let mut fields = vec![
        choice("pipeline", "Pipeline", pipelines, &pipeline),
        choice(
            "kind",
            "Kind",
            KINDS.iter().map(|k| k.to_string()).collect(),
            &kind,
        ),
        Field {
            key: "inputs".into(),
            label: "Inputs".into(),
            kind: FieldKind::OrderedList {
                items,
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        },
    ];
    match kind.as_str() {
        "source" => {
            fields.push(choice(
                "dataset",
                "Dataset",
                datasets,
                word(table, "dataset").unwrap_or(&first_dataset),
            ));
            fields.push(text("series", "Series", text_of(table, "series", "")));
        }
        "parity_fit" => {
            let value = match table.get("min_strikes") {
                Some(toml::Value::Integer(n)) => *n,
                _ => 6,
            };
            fields.push(Field {
                key: "min_strikes".into(),
                label: "Min strikes".into(),
                kind: FieldKind::Number {
                    value,
                    min: 1,
                    max: 1000,
                    step: 1,
                    wrap: false,
                },
                dest: Destination::Doc,
                layer: None,
            });
            fields.push(text(
                "strike_window",
                "Strike window",
                text_of(table, "strike_window", "0.8, 1.2"),
            ));
        }
        "check" => fields.push(choice(
            "check",
            "Check",
            CHECKS.iter().map(|c| c.to_string()).collect(),
            word(table, "check").unwrap_or(CHECKS[0]),
        )),
        "kalman_fit" => {
            fields.push(choice(
                "series",
                "Series",
                FIT_SERIES.iter().map(|s| s.to_string()).collect(),
                word(table, "series").unwrap_or(FIT_SERIES[0]),
            ));
            fields.push(text(
                "knots",
                "Knots",
                text_of(table, "knots", "0.25, 0.5, 1, 2, 3, 5"),
            ));
            fields.push(text(
                "process_noise",
                "Process noise",
                text_of(table, "process_noise", "0.0001"),
            ));
            fields.push(text(
                "measurement_noise",
                "Measurement noise",
                text_of(table, "measurement_noise", "spread"),
            ));
        }
        "rasterize" => fields.push(choice(
            "grid",
            "Grid",
            GRIDS.iter().map(|g| g.to_string()).collect(),
            word(table, "grid").unwrap_or(GRIDS[0]),
        )),
        "publish" => {
            let targets = targets_of(config);
            let first_target = targets.first().cloned().unwrap_or_default();
            fields.push(choice(
                "document",
                "Document",
                datasets,
                word(table, "document").unwrap_or(&first_dataset),
            ));
            fields.push(choice(
                "target",
                "Target",
                targets,
                word(table, "target").unwrap_or(&first_target),
            ));
        }
        // `implied_rates` has no parameters; an unknown kind has none the
        // dialog knows, and keeps its table's keys through the writer.
        _ => {}
    }
    fields
}

fn item(name: &str, included: bool) -> ListItem {
    ListItem {
        name: name.to_string(),
        included,
        presentation: Default::default(),
        kind: None,
        source: None,
    }
}

/// The numbers in `text`, separated by commas or spaces; `None` when any
/// word is not a number or there is none.
fn numbers(text: &str) -> Option<Vec<f64>> {
    let words: Vec<&str> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    words.iter().map(|w| w.parse::<f64>().ok()).collect()
}

fn float_array(values: &[f64]) -> toml_edit::Item {
    let mut array = toml_edit::Array::new();
    for v in values {
        array.push(*v);
    }
    toml_edit::value(array)
}

/// The stage as `calibration.toml` holds it: the source table with the
/// fields applied over it, so a key the dialog does not model (`basis`)
/// survives. Every parameter key of every kind is cleared first when the
/// kind is one the dialog knows, so a table holds one kind's parameters.
/// A typed number that does not parse (the field refused it, so this is
/// a hand-edited seed) is written as its text, which the reader then
/// refuses at that field rather than silently losing it.
pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item {
    debug_assert_eq!(dest, Destination::Doc);
    let mut table = super::toml_table_to_edit(&draft.source);
    if let Some(pipeline) = draft.choice("pipeline") {
        table["pipeline"] = toml_edit::value(pipeline);
    }
    let kind = draft.choice("kind").unwrap_or_default().to_string();
    table["kind"] = toml_edit::value(kind.as_str());
    let inputs: Vec<&str> = draft
        .list_items("inputs")
        .unwrap_or_default()
        .iter()
        .filter(|i| i.included)
        .map(|i| i.name.as_str())
        .collect();
    if inputs.is_empty() {
        table.remove("inputs");
    } else {
        let mut array = toml_edit::Array::new();
        for name in inputs {
            array.push(name);
        }
        table["inputs"] = toml_edit::value(array);
    }
    if KINDS.contains(&kind.as_str()) {
        for key in PARAMETER_KEYS {
            table.remove(key);
        }
    }
    let set_choice = |table: &mut toml_edit::Table, key: &str| {
        if let Some(v) = draft.choice(key) {
            table[key] = toml_edit::value(v);
        }
    };
    // A float as typed, or the text when it does not parse.
    let set_number = |table: &mut toml_edit::Table, key: &str| {
        if let Some(t) = draft.text(key) {
            table[key] = match t.trim().parse::<f64>() {
                Ok(f) => toml_edit::value(f),
                Err(_) => toml_edit::value(t),
            };
        }
    };
    let set_numbers = |table: &mut toml_edit::Table, key: &str| {
        if let Some(t) = draft.text(key) {
            table[key] = match numbers(t) {
                Some(values) => float_array(&values),
                None => toml_edit::value(t),
            };
        }
    };
    match kind.as_str() {
        "source" => {
            set_choice(&mut table, "dataset");
            if let Some(series) = draft
                .text("series")
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                table["series"] = toml_edit::value(series);
            }
        }
        "parity_fit" => {
            if let Some(FieldKind::Number { value, .. }) =
                draft.field_by_key("min_strikes").map(|f| &f.kind)
            {
                table["min_strikes"] = toml_edit::value(*value);
            }
            set_numbers(&mut table, "strike_window");
        }
        "check" => set_choice(&mut table, "check"),
        "kalman_fit" => {
            set_choice(&mut table, "series");
            set_numbers(&mut table, "knots");
            set_number(&mut table, "process_noise");
            if let Some(t) = draft.text("measurement_noise") {
                table["measurement_noise"] = if t.trim() == "spread" {
                    toml_edit::value("spread")
                } else {
                    match t.trim().parse::<f64>() {
                        Ok(f) => toml_edit::value(f),
                        Err(_) => toml_edit::value(t),
                    }
                };
            }
        }
        "rasterize" => set_choice(&mut table, "grid"),
        "publish" => {
            set_choice(&mut table, "document");
            set_choice(&mut table, "target");
        }
        _ => {}
    }
    toml_edit::Item::Table(table)
}

/// Everything the document's readers say about this stage: `from_doc` and
/// `check_against` run over the merged document with this draft's table in
/// the stage's place, keeping the findings whose path is the stage's
/// (`stages.<name>` or under it). The other stages' findings are theirs,
/// not this draft's; the pipeline's drop that a bad stage causes is said at
/// the stage's own field. Checked against the live `datasets` document;
/// the dialog's own drafts go through [`validate_with`].
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    validate_against(draft, config, None)
}

/// [`validate`] against the schema the data engine was built over:
/// `startup` is the set of datasets declared at launch
/// (`ObjectDialogState::startup_datasets`), and the live schema is cut to
/// it before `check_against` runs. The bridge reads the pipeline against
/// that startup schema, so a stage reading or publishing a dataset a
/// reload has since added would be dropped at the bridge with the
/// reader's warning; the dialog refuses it in the same words rather than
/// let the save drop the pipeline. VolMonitor views warn and offer the
/// column instead, because a column that cannot paint until a restart
/// harms nothing; a dropped pipeline takes every stage with it.
pub fn validate_with(
    draft: &Draft,
    config: &Config,
    startup: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    validate_against(draft, config, Some(startup))
}

fn validate_against(
    draft: &Draft,
    config: &Config,
    startup: Option<&BTreeSet<String>>,
) -> Vec<Diagnostic> {
    let text = super::object_text(&draft.name, to_table(draft, Destination::Doc));
    let stage = text
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut t| t.remove(&draft.name))
        .unwrap_or(toml::Value::Table(toml::Table::new()));
    let mut doc: MergedDoc = config.doc(DOC).cloned().unwrap_or_default();
    super::objects_in_mut(&mut doc.value, DOC).insert(draft.name.clone(), stage);
    let (pipelines, mut diags) = from_doc(&doc);
    let mut schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    if let Some(startup) = startup {
        schema.datasets.retain(|d| startup.contains(&d.name));
    }
    let egress: Vec<EgressSpec> = targets_of(config)
        .into_iter()
        .map(|name| EgressSpec {
            name,
            adapter: String::new(),
            documents: Vec::new(),
        })
        .collect();
    let (_, more) = check_against(pipelines, &schema, &egress);
    diags.extend(more);
    let exact = format!("stages.{}", draft.name);
    let own = format!("{exact}.");
    diags.retain(|d| {
        d.path
            .as_deref()
            .is_some_and(|p| p == exact || p.starts_with(&own))
    });
    diags
}

/// The one step this domain refuses before the draft takes it: the
/// `kind` row. The parameter rows were derived from the kind when the
/// stage opened, so stepping it would leave the old kind's rows under the
/// new kind and write both; the way to another kind is a new stage.
pub fn step_refusal(draft: &Draft, _config: &Config) -> Option<String> {
    let Some(EditRow::Field(i)) = draft.selected_row() else {
        return None;
    };
    (draft.fields.get(i)?.key == "kind").then(|| KIND_REFUSAL.to_string())
}

/// What each field means, for the edit footer's help line; under ~90
/// characters, since the slot is one line.
pub fn help(key: &str) -> &'static str {
    match key {
        "pipeline" => "The pipeline this stage belongs to; inputs may name only its stages",
        "kind" => "The stage's kind sets its ports and parameters; a new stage takes another kind",
        "inputs" => "The stages this one reads, in port order; space includes, shift+j/k reorder",
        "dataset" => "The document dataset this source reads: a chain, a schedule or a curve",
        "series" => "Which series of the document: the one a source reads, or a fit produces",
        "min_strikes" => "The fewest strikes an expiry needs for the parity fit to use it",
        "strike_window" => "The moneyness band the fit uses, as low, high with low < high",
        "check" => "The check run on the points; monotone_df refuses a rising discount factor",
        "knots" => "The fit's knot tenors in years: two or more strictly ascending numbers",
        "process_noise" => "How far the fit may move between runs; a positive number",
        "measurement_noise" => "Each point's weight: spread uses its quoted spread, or one number",
        "grid" => "The tenor grid the fit is rastered onto",
        "document" => "The document dataset the rasters are published as",
        "target" => "The egress target the publish goes to, from egress.toml",
        _ => "",
    }
}

/// The text rows typed entry may change: a source's series and the
/// numbers the dialog's integer `Number` cannot hold.
pub fn text_editable(key: &str) -> bool {
    matches!(
        key,
        "series" | "strike_window" | "knots" | "process_noise" | "measurement_noise"
    )
}

/// Normalize committed text by the reader's rules, or return its refusal
/// — the same rule the reader applies, so a value the dialog accepts is
/// one the file's reader accepts.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    let positive = |label: &str| -> Result<String, String> {
        match text.parse::<f64>() {
            Ok(f) if f > 0.0 => Ok(format!("{f}")),
            _ => Err(format!("{label} is a positive number")),
        }
    };
    match key {
        "strike_window" => match numbers(text).as_deref() {
            Some([low, high]) if low < high => Ok(format!("{low}, {high}")),
            _ => Err("Strike window is two numbers, low, high, with low < high".to_string()),
        },
        "knots" => match numbers(text) {
            Some(values) if values.len() >= 2 && values.windows(2).all(|w| w[0] < w[1]) => {
                Ok(values
                    .iter()
                    .map(|v| format!("{v}"))
                    .collect::<Vec<_>>()
                    .join(", "))
            }
            _ => Err(
                "Knots are two or more strictly ascending numbers, like 0.25, 0.5, 1".to_string(),
            ),
        },
        "process_noise" => positive("Process noise"),
        "measurement_noise" if text == "spread" => Ok(text.to_string()),
        "measurement_noise" => positive("Measurement noise")
            .map_err(|_| "Measurement noise is spread or a positive number".to_string()),
        _ => Ok(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::Domain;
    use super::*;
    use crate::shell::ShellServices;
    use geode_core::calibration::BUILTIN_CALIBRATION;
    use geode_core::config::{ConfigSources, LayerDoc, Severity};

    /// The demo schema: the datasets the builtin pipeline's sources read
    /// and its publish stage writes are document datasets here.
    const DEMO_DATASETS: &str = include_str!("../../../../../examples/demo-config/datasets.toml");
    const EGRESS: &str = "config_version = 1\n[sophis]\nadapter = \"demo_bus\"\n[sophis.documents]\nrate_curve = \"marketdata/ratecurve/{key}/NOTIFY\"\n";

    fn config_with(calibration: &str) -> Config {
        let docs = vec![
            LayerDoc::builtin(DOC, calibration).unwrap(),
            LayerDoc::builtin("datasets", DEMO_DATASETS).unwrap(),
            LayerDoc::builtin("egress", EGRESS).unwrap(),
        ];
        ShellServices::config_and_builtin(ConfigSources {
            builtin: docs,
            desk: None,
            user: None,
        })
        .0
    }

    fn keys(fields: &[Field]) -> Vec<&str> {
        fields.iter().map(|f| f.key.as_str()).collect()
    }

    fn field<'a>(fields: &'a [Field], key: &str) -> &'a Field {
        fields
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("{key} is a field"))
    }

    fn choice_of(fields: &[Field], key: &str) -> (Vec<String>, String) {
        match &field(fields, key).kind {
            FieldKind::Choice { options, selected } => {
                (options.clone(), options[*selected].clone())
            }
            other => panic!("{key} is a choice, not {other:?}"),
        }
    }

    fn text_of(fields: &[Field], key: &str) -> String {
        match &field(fields, key).kind {
            FieldKind::Text(t) => t.clone(),
            other => panic!("{key} is a text, not {other:?}"),
        }
    }

    #[test]
    fn calibration_rows_are_stages_in_document_order_with_pipeline_and_kind_summaries() {
        let config = config_with(BUILTIN_CALIBRATION);
        let rows = Domain::Calibration.objects(&config);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "chain",
                "dividends",
                "rates",
                "parity",
                "implied_rates",
                "df_monotone",
                "repo_fit",
                "box_fit",
                "repo_curve",
                "box_curve",
                "publish",
            ],
            "the stages table's order, never the `pipelines` table"
        );
        assert_eq!(rows[0].summary, "implied_forwards \u{b7} source");
        assert_eq!(rows[6].summary, "implied_forwards \u{b7} kalman_fit");
        assert!(
            rows.iter()
                .all(|r| r.layer == Some(geode_core::config::Layer::Builtin))
        );
    }

    #[test]
    fn calibration_fields_follow_the_kind() {
        let config = config_with(BUILTIN_CALIBRATION);
        let fit = fields(&config, Some("repo_fit"));
        assert_eq!(
            keys(&fit),
            [
                "pipeline",
                "kind",
                "inputs",
                "series",
                "knots",
                "process_noise",
                "measurement_noise"
            ]
        );
        assert_eq!(
            choice_of(&fit, "series"),
            (
                vec!["repo".to_string(), "box".to_string()],
                "repo".to_string()
            )
        );
        assert_eq!(choice_of(&fit, "kind").1, "kalman_fit");
        assert_eq!(text_of(&fit, "knots"), "0.25, 0.5, 1, 2, 3, 5");
        assert_eq!(text_of(&fit, "process_noise"), "0.0001");
        assert_eq!(text_of(&fit, "measurement_noise"), "spread");
        // The inputs: the one read first, ticked, then the pipeline's other
        // stages in document order, unticked, the stage itself left out.
        let FieldKind::OrderedList { items, available } = &field(&fit, "inputs").kind else {
            panic!("inputs is a list");
        };
        assert_eq!(available, &None, "no catalogue: ticking is membership");
        assert_eq!(items[0].name, "implied_rates");
        assert!(items[0].included);
        assert_eq!(items.len(), 10);
        assert!(items[1..].iter().all(|i| !i.included));
        assert!(items.iter().all(|i| i.name != "repo_fit"));
        assert_eq!(items[1].name, "chain");

        let publish = fields(&config, Some("publish"));
        assert_eq!(
            keys(&publish),
            ["pipeline", "kind", "inputs", "document", "target"]
        );
        let (documents, document) = choice_of(&publish, "document");
        assert_eq!(document, "rate_curve");
        assert!(documents.contains(&"option_chain".to_string()));
        assert!(
            !documents.contains(&"risk".to_string()),
            "a measures dataset is no document"
        );
        assert_eq!(
            choice_of(&publish, "target"),
            (vec!["sophis".to_string()], "sophis".to_string())
        );

        let parity = fields(&config, Some("parity"));
        assert!(matches!(
            field(&parity, "min_strikes").kind,
            FieldKind::Number {
                value: 6,
                min: 1,
                ..
            }
        ));
        assert_eq!(text_of(&parity, "strike_window"), "0.8, 1.2");
        let source = fields(&config, Some("rates"));
        assert_eq!(
            keys(&source),
            ["pipeline", "kind", "inputs", "dataset", "series"]
        );
        assert_eq!(choice_of(&source, "dataset").1, "rate_curve");
        assert_eq!(text_of(&source, "series"), "funding");
        assert_eq!(
            keys(&fields(&config, Some("implied_rates"))),
            ["pipeline", "kind", "inputs"]
        );
    }

    /// A new stage is a source of the first pipeline over the first
    /// document dataset, with the pipeline's stages unticked.
    #[test]
    fn a_new_stage_opens_as_a_source_with_nothing_read() {
        let config = config_with(BUILTIN_CALIBRATION);
        let fresh = new_fields(&config, "fresh");
        assert_eq!(choice_of(&fresh, "pipeline").1, "implied_forwards");
        assert_eq!(choice_of(&fresh, "kind").1, "source");
        let FieldKind::OrderedList { items, .. } = &field(&fresh, "inputs").kind else {
            panic!()
        };
        assert_eq!(items.len(), 11);
        assert!(items.iter().all(|i| !i.included));
        assert_eq!(text_of(&fresh, "series"), "");
    }

    /// A bad `knots` on the draft is one finding at the draft's own field;
    /// another stage's finding (box_fit's, broken in the document) is not
    /// the draft's, although the reader reports both.
    #[test]
    fn a_calibration_draft_validates_through_from_doc_and_keeps_only_its_own_paths() {
        let broken = BUILTIN_CALIBRATION.replacen(
            "knots             = [0.25, 0.5, 1.0, 2.0, 3.0, 5.0]\nprocess_noise     = 1.0e-4\nmeasurement_noise = \"spread\"\n\n[stages.repo_curve]",
            "knots             = [9.0]\nprocess_noise     = 1.0e-4\nmeasurement_noise = \"spread\"\n\n[stages.repo_curve]",
            1,
        );
        assert_ne!(broken, BUILTIN_CALIBRATION, "box_fit's knots are broken");
        let config = config_with(&broken);
        let mut draft = Domain::Calibration.draft(&config, "repo_fit");
        assert!(draft.diagnostics.is_empty(), "{:?}", draft.diagnostics);
        let knots = draft.fields.iter().position(|f| f.key == "knots").unwrap();
        draft.fields[knots].kind = FieldKind::Text("1".into());
        let diags = Domain::Calibration.validate(&draft, &config);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].path.as_deref(), Some("stages.repo_fit.knots"));
        assert_eq!(diags[0].severity, Severity::Error);
        // The finding lands on its row, under the reader's own spelling of
        // the path.
        let row = draft.row_for_path(DOC, "stages.repo_fit.knots");
        assert_eq!(row, Some(EditRow::Field(knots)));
    }

    /// A source over a dataset the live `datasets` document declares but
    /// the startup schema lacks is refused in the reader's words at its
    /// `dataset` field, as the bridge's startup read would drop it; against
    /// a startup set that has the dataset, or with no startup set (a draft
    /// built outside a dialog), the live document decides and nothing is
    /// said. `Domain::validate` reads the set the dialog hands the draft.
    #[test]
    fn a_dataset_only_the_live_schema_declares_is_refused_by_the_dialog() {
        let config = config_with(BUILTIN_CALIBRATION);
        let mut draft = Domain::Calibration.draft(&config, "rates");
        assert!(draft.diagnostics.is_empty(), "{:?}", draft.diagnostics);
        assert_eq!(choice_of(&draft.fields, "dataset").1, "rate_curve");
        assert!(validate(&draft, &config).is_empty());

        let restart = |diags: &[Diagnostic]| {
            diags
                .iter()
                .find(|d| d.message.ends_with("which the schema does not declare"))
                .map(|d| (d.severity, d.path.clone()))
        };
        let lacking: BTreeSet<String> = ["option_chain".to_string()].into();
        let with = validate_with(&draft, &config, &lacking);
        assert_eq!(
            restart(&with),
            Some((Severity::Warning, Some("stages.rates.dataset".into()))),
            "{with:?}"
        );
        let having: BTreeSet<String> = ["rate_curve".to_string()].into();
        assert_eq!(restart(&validate_with(&draft, &config, &having)), None);

        draft.startup_datasets = Some(lacking);
        assert!(restart(&Domain::Calibration.validate(&draft, &config)).is_some());
        draft.startup_datasets = None;
        assert!(restart(&Domain::Calibration.validate(&draft, &config)).is_none());
    }

    #[test]
    fn changing_a_stage_kind_is_refused() {
        let config = config_with(BUILTIN_CALIBRATION);
        let mut draft = Domain::Calibration.draft(&config, "repo_fit");
        let kind = draft
            .rows()
            .iter()
            .position(|r| *r == EditRow::Field(1))
            .unwrap();
        draft.selected = kind;
        assert_eq!(
            Domain::Calibration.step_refusal(&draft, &config),
            Some(KIND_REFUSAL.to_string())
        );
        assert_eq!(
            choice_of(&draft.fields, "kind").1,
            "kalman_fit",
            "unchanged"
        );
        draft.selected = 0;
        assert_eq!(Domain::Calibration.step_refusal(&draft, &config), None);
    }

    /// The writer applies the fields over the stage's table: a typed
    /// strike window becomes two floats, an unticked input leaves the
    /// array, and a key the dialog does not model (`basis`) survives.
    #[test]
    fn the_writer_keeps_unmodelled_keys_and_spells_numbers_as_numbers() {
        let config = config_with(BUILTIN_CALIBRATION);
        let mut draft = Domain::Calibration.draft(&config, "parity");
        let window = draft
            .fields
            .iter()
            .position(|f| f.key == "strike_window")
            .unwrap();
        draft.fields[window].kind = FieldKind::Text("0.7, 1.3".into());
        let text = super::super::object_text("parity", to_table(&draft, Destination::Doc));
        assert!(text.contains("strike_window = [0.7, 1.3]"), "{text}");
        assert!(text.contains("inputs = [\"chain\"]"), "{text}");
        assert!(text.contains("min_strikes = 6"), "{text}");

        let mut fit = Domain::Calibration.draft(&config, "repo_fit");
        let knots = fit.fields.iter().position(|f| f.key == "knots").unwrap();
        fit.fields[knots].kind = FieldKind::Text("0.5, 1, 2".into());
        let noise = fit
            .fields
            .iter()
            .position(|f| f.key == "measurement_noise")
            .unwrap();
        fit.fields[noise].kind = FieldKind::Text("0.002".into());
        let text = super::super::object_text("repo_fit", to_table(&fit, Destination::Doc));
        assert!(text.contains("basis = \"knots\""), "{text}");
        assert!(text.contains("knots = [0.5, 1.0, 2.0]"), "{text}");
        assert!(text.contains("measurement_noise = 0.002"), "{text}");
        assert!(text.contains("process_noise = 0.0001"), "{text}");
        assert!(
            Domain::Calibration.validate(&fit, &config).is_empty(),
            "the written table reads clean"
        );
    }

    #[test]
    fn typed_numbers_follow_the_readers_rules() {
        assert_eq!(
            parse_text("knots", "0.25 ,0.5, 1"),
            Ok("0.25, 0.5, 1".to_string())
        );
        assert!(parse_text("knots", "1").is_err(), "two or more");
        assert!(parse_text("knots", "1, 0.5").is_err(), "ascending");
        assert_eq!(
            parse_text("strike_window", "0.8 1.2"),
            Ok("0.8, 1.2".to_string())
        );
        assert!(parse_text("strike_window", "1.2, 0.8").is_err());
        assert!(parse_text("strike_window", "0.8").is_err());
        assert_eq!(
            parse_text("process_noise", "1e-4"),
            Ok("0.0001".to_string())
        );
        assert!(parse_text("process_noise", "0").is_err());
        assert_eq!(
            parse_text("measurement_noise", " spread "),
            Ok("spread".to_string())
        );
        assert_eq!(
            parse_text("measurement_noise", "0.5"),
            Ok("0.5".to_string())
        );
        assert_eq!(
            parse_text("measurement_noise", "wide"),
            Err("Measurement noise is spread or a positive number".to_string())
        );
        assert_eq!(parse_text("series", " funding "), Ok("funding".to_string()));
        for key in [
            "series",
            "strike_window",
            "knots",
            "process_noise",
            "measurement_noise",
        ] {
            assert!(text_editable(key), "{key}");
        }
        assert!(!text_editable("pipeline"));
        for key in [
            "pipeline",
            "kind",
            "inputs",
            "dataset",
            "series",
            "min_strikes",
            "strike_window",
            "check",
            "knots",
            "process_noise",
            "measurement_noise",
            "grid",
            "document",
            "target",
        ] {
            assert!(!help(key).is_empty(), "{key} has a help line");
            assert!(help(key).len() <= 90, "{key}: {}", help(key).len());
        }
    }
}
