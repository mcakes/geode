//! Adapter for source definitions. Browse rows sort by dataset and source name; the
//! source name remains the persistence identity.
//!
//! Directory and subscribed adapters expose their respective fields, all as user-layer
//! definition edits. A copied inherited source therefore becomes a whole-object
//! override. Validation round-trips the current object through the core source reader;
//! warnings remain editable, while errors block queuing. Source changes can be saved
//! here but require restart to rebuild ingestion.

use super::{Destination, Draft, Field, FieldKind};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::schema::SchemaSpec;
use geode_core::source_config::{
    CSV_DIR_ADAPTER, DEFAULT_COALESCE, DEFAULT_PENDING_TIMEOUT, DEFAULT_POLL,
    DEFAULT_SNAPSHOT_POLL, SourceSpec, check_batch_pattern, parse_duration,
};
use std::time::Duration;

pub const DOC: &str = "sources";
/// Between globs in the `paths` text. `;` is not reserved by either platform's
/// filesystem — NTFS's own reserved set is `< > : " / \ | ? *` — the choice is
/// unambiguous only in the common case: it is rare inside a glob and never one of
/// glob's own metacharacters, while a space is legal in a path on both platforms and so
/// cannot separate them. A glob that needs a literal `;` cannot be expressed in this
/// field.
pub const PATH_SEPARATOR: char = ';';

const READINESS: [&str; 2] = ["sentinel", "stable_mtime"];
const PRIORITY: [&str; 3] = ["latest_risk", "latest_other", "backfill"];
/// The reader's priority for a snapshot source that sets none.
const SNAPSHOT_PRIORITY: &str = "latest_other";

/// The browse row's muted second line: how many paths a source watches and which
/// cold-start priority it claims — the two facts a trader scans the list for. A snapshot
/// source watches no paths, so its line names the table it reads and the snapshot
/// priority default instead. Read straight off the raw table, for the reason
/// `views::summary` and `groupings::summary` both give for doing the same: a malformed
/// source is exactly the one this dialog exists to fix, and the reader would drop it
/// from the merged result entirely.
pub fn summary(config: &Config, value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "not a table".to_string();
    };
    let priority = table.get("priority").and_then(|v| v.as_str());
    if is_snapshot(table, &schema_of(config)) {
        let priority = priority.unwrap_or(SNAPSHOT_PRIORITY);
        return match table.get("table").and_then(|v| v.as_str()) {
            Some(name) => format!("table {name} · {priority}"),
            None => format!("no table · {priority}"),
        };
    }
    let n = table
        .get("paths")
        .and_then(|v| v.as_array())
        .map_or(0, |a| a.len());
    let priority = priority.unwrap_or(PRIORITY[0]);
    format!("{n} path{} · {priority}", if n == 1 { "" } else { "s" })
}

/// The datasets document as the reader parses it; empty when there is none.
fn schema_of(config: &Config) -> SchemaSpec {
    config
        .doc("datasets")
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default()
}

/// `SourceSpec::shape`'s own rule, read off the raw table: another adapter over a
/// reference dataset is a snapshot source. The directory rows and defaults mean nothing
/// to it, so showing them would misstate what the reader applies.
fn is_snapshot(table: &toml::Table, schema: &SchemaSpec) -> bool {
    let adapter = table
        .get("adapter")
        .and_then(|v| v.as_str())
        .unwrap_or(CSV_DIR_ADAPTER);
    adapter != CSV_DIR_ADAPTER
        && table
            .get("dataset")
            .and_then(|v| v.as_str())
            .and_then(|d| schema.dataset(d))
            .is_some_and(|d| d.is_reference())
}

/// Dataset prefix for the source's browse label and primary sort key.
pub fn prefix(value: &toml::Value) -> Option<String> {
    value
        .as_table()
        .and_then(|t| t.get("dataset"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `2h` / `10m` / `45s`: the largest unit that divides exactly, the
/// reader's own grammar (`parse_duration`) spelled back out. A field
/// shows the value that will actually apply, so an omitted key is
/// spelled from `DEFAULT_POLL`/`DEFAULT_PENDING_TIMEOUT` rather than
/// left blank. A sub-second remainder (a subscribed source's `coalesce`,
/// default 500ms) spells in `ms` — `as_secs()` alone would round it away
/// to "0s", the one unit `parse_duration` would read back as zero.
pub fn spell_duration(d: Duration) -> String {
    let s = d.as_secs();
    if d.subsec_millis() > 0 {
        format!("{}ms", d.as_millis())
    } else if s > 0 && s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s > 0 && s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// A `Choice` field whose options always include the object's own
/// current value (`views::fields`'s own rule, restated here): a `Choice`
/// that cannot show its value would step silently.
fn choice(options: &[&str], current: &str) -> FieldKind {
    let mut options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
    if !options.iter().any(|o| o == current) {
        options.insert(0, current.to_string());
    }
    let selected = options.iter().position(|o| o == current).unwrap_or(0);
    FieldKind::Choice { options, selected }
}

/// Build nine fields for directory sources, adding document/topics/coalesce/
/// source_time for other adapters. A snapshot source (another adapter over a reference
/// dataset) shows only dataset, priority, poll interval, table and adapter, with the
/// reader's snapshot defaults (`latest_other`, `DEFAULT_SNAPSHOT_POLL`). Missing
/// objects use the directory defaults. Dataset and priority are choices; readiness
/// splits kind from stable poll count. Only the supported paths, duration,
/// batch-pattern and table text fields are editable; adapter and subscribed-only text
/// fields are read-only.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    let get_str = |key: &str| table.and_then(|t| t.get(key)).and_then(|v| v.as_str());

    let schema = schema_of(config);
    // A local dataset is written by the app and never fed by a source (the
    // source reader refuses one), so it is not offered; computed: a module
    // answers for it and no source may feed it.
    let mut datasets: Vec<&str> = schema
        .datasets
        .iter()
        .filter(|d| !d.local && !d.computed)
        .map(|d| d.name.as_str())
        .collect();
    datasets.sort_unstable();
    // The object's own dataset is always an option (Views' rule): a
    // `Choice` that cannot show its value would step silently.
    let current_dataset = get_str("dataset")
        .map(str::to_string)
        .or_else(|| datasets.first().map(|d| d.to_string()))
        .unwrap_or_default();

    let paths: Vec<String> = table
        .and_then(|t| t.get("paths"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let (readiness, polls) = match table.and_then(|t| t.get("readiness")) {
        // Any string reads as `sentinel` (the reader itself only ever
        // writes the literal `"sentinel"`, but a hand-edited file could
        // hold something else, and there is no third string form to
        // distinguish it from) — a bare `as_str() == Some("sentinel")`
        // check bought nothing `is_str()` alone doesn't already cover.
        Some(v) if v.is_str() => ("sentinel", 3),
        Some(v) => match v
            .as_table()
            .and_then(|t| t.get("stable_mtime"))
            .and_then(|p| p.as_integer())
        {
            Some(p) if p > 0 => ("stable_mtime", p),
            _ => ("sentinel", 3),
        },
        None => ("sentinel", 3),
    };

    let duration = |key: &str, default: Duration| -> String {
        get_str(key)
            .map(str::to_string)
            .unwrap_or_else(|| spell_duration(default))
    };

    let text = |key: &str, label: &str, value: String| Field {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Text(value),
        dest: Destination::Doc,
        layer: None,
    };
    let field = |key: &str, label: &str, kind: FieldKind| Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        dest: Destination::Doc,
        layer: None,
    };

    // Read-only (`to_table` never writes it back and it is not in
    // `text_editable`'s list): always shown, since it is what tells a
    // trader whether the four subscribed-only rows below apply at all.
    let adapter = get_str("adapter").unwrap_or(CSV_DIR_ADAPTER);
    let subscribed = adapter != CSV_DIR_ADAPTER;
    // `SourceSpec::shape`'s own rule, read off the raw table: another adapter over a
    // reference dataset is a snapshot source. The directory rows mean nothing to it and
    // its defaults differ, so showing them would misstate what the reader applies.
    let snapshot = subscribed
        && schema
            .dataset(&current_dataset)
            .is_some_and(|d| d.is_reference());
    if snapshot {
        return vec![
            field("dataset", "Dataset", choice(&datasets, &current_dataset)),
            field(
                "priority",
                "Priority",
                choice(&PRIORITY, get_str("priority").unwrap_or(SNAPSHOT_PRIORITY)),
            ),
            text(
                "poll_interval",
                "Poll interval",
                duration("poll_interval", DEFAULT_SNAPSHOT_POLL),
            ),
            text("table", "Table", get_str("table").unwrap_or("").to_string()),
            text("adapter", "Adapter", adapter.to_string()),
        ];
    }

    let mut out = vec![
        field("dataset", "Dataset", choice(&datasets, &current_dataset)),
        text("paths", "Paths", paths.join(&format!("{PATH_SEPARATOR} "))),
        field("readiness", "Readiness", choice(&READINESS, readiness)),
        field(
            "stable_polls",
            "Stable polls",
            FieldKind::Number {
                value: polls,
                min: 1,
                max: 100,
                step: 1,
                wrap: false,
            },
        ),
        field(
            "priority",
            "Priority",
            choice(&PRIORITY, get_str("priority").unwrap_or(PRIORITY[0])),
        ),
        text(
            "poll_interval",
            "Poll interval",
            duration("poll_interval", DEFAULT_POLL),
        ),
        text(
            "pending_timeout",
            "Pending timeout",
            duration("pending_timeout", DEFAULT_PENDING_TIMEOUT),
        ),
        text(
            "batch_pattern",
            "Batch pattern",
            get_str("batch_pattern").unwrap_or("").to_string(),
        ),
        text("adapter", "Adapter", adapter.to_string()),
    ];

    // Subscribed adapters display document, topics, coalescing, and source-time
    // settings read-only. The source table retains these values during other edits.
    if subscribed {
        out.push(text(
            "document",
            "Document",
            get_str("document").unwrap_or("").to_string(),
        ));
        out.push(text(
            "topics",
            "Topics",
            table
                .and_then(|t| t.get("topics"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(&format!("{PATH_SEPARATOR} "))
                })
                .unwrap_or_default(),
        ));
        out.push(text(
            "coalesce",
            "Coalesce",
            duration("coalesce", DEFAULT_COALESCE),
        ));
        out.push(text(
            "source_time",
            "Source time",
            get_str("source_time").unwrap_or("receive").to_string(),
        ));
    }

    out
}

/// `n` seeds the new source's dataset from the browse row under the cursor, not the
/// schema's first — the option is added when the schema lacks it, for the same reason
/// `fields` keeps an object's own.
pub fn seed_dataset(draft: &mut Draft, dataset: &str) {
    if let Some(field) = draft.fields.iter_mut().find(|f| f.key == "dataset")
        && let FieldKind::Choice { options, selected } = &mut field.kind
    {
        if !options.iter().any(|o| o == dataset) {
            options.insert(0, dataset.to_string());
        }
        *selected = options.iter().position(|o| o == dataset).unwrap_or(0);
    }
}

/// May `i` edit the `Text` row keyed `key`? Every genuinely free-text
/// field: `dataset`/`readiness`/`priority`/`stable_polls` are `Choice`
/// and `Number` rows already reachable through `space`/`i` on their own
/// kind, so only the five text fields need this door.
pub fn text_editable(key: &str) -> bool {
    matches!(
        key,
        "paths" | "poll_interval" | "pending_timeout" | "batch_pattern" | "table"
    )
}

/// Refuse unreadable durations, invalid regexes, and malformed path lists before
/// queuing. An empty path list remains valid as an idle source.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    match key {
        "poll_interval" | "pending_timeout" => match parse_duration(text) {
            Some(_) => Ok(text.to_string()),
            None => {
                // Named like the `Number` refusals name their field's
                // label, not the raw config key — `parse_text` only has
                // the key, so the two Sources duration fields are
                // spelled out here rather than threading a label
                // through the `Domain::parse_text` signature for one
                // caller.
                let label = match key {
                    "poll_interval" => "Poll interval",
                    _ => "Pending timeout",
                };
                Err(format!("{label}: a number and a unit, like 45s, 5m or 2h"))
            }
        },
        "batch_pattern" if text.is_empty() => Ok(String::new()),
        "batch_pattern" => check_batch_pattern(text).map(|()| text.to_string()),
        "paths" => Ok(split_paths(text).join(&format!("{PATH_SEPARATOR} "))),
        _ => Ok(text.to_string()),
    }
}

fn split_paths(text: &str) -> Vec<String> {
    text.split(PATH_SEPARATOR)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The draft as `sources.toml` holds it: the source table with every
/// field applied over it (a key the vocabulary does not model — none
/// today, but the same "preserve what we don't own" rule every other
/// adapter's `to_table` follows — survives). `stable_polls` is written
/// only under `stable_mtime`; an empty `batch_pattern` removes the key
/// rather than writing an empty string the reader would treat as
/// "present but useless".
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    let mut table = super::toml_table_to_edit(&draft.source);
    let text = |key: &str| {
        draft
            .fields
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| match &f.kind {
                FieldKind::Text(t) => Some(t.clone()),
                _ => None,
            })
    };
    if let Some(dataset) = draft.choice("dataset") {
        table["dataset"] = toml_edit::value(dataset);
    }
    if let Some(paths) = text("paths") {
        let mut array = toml_edit::Array::new();
        for p in split_paths(&paths) {
            array.push(p);
        }
        table["paths"] = toml_edit::value(array);
    }
    let polls = draft
        .fields
        .iter()
        .find(|f| f.key == "stable_polls")
        .and_then(|f| match f.kind {
            FieldKind::Number { value, .. } => Some(value),
            _ => None,
        });
    match (draft.choice("readiness"), polls) {
        (Some("stable_mtime"), Some(polls)) => {
            let mut inline = toml_edit::InlineTable::new();
            inline.insert("stable_mtime", toml_edit::Value::from(polls));
            table["readiness"] = toml_edit::value(inline);
        }
        (Some(_), _) => table["readiness"] = toml_edit::value("sentinel"),
        (None, _) => {}
    }
    if let Some(priority) = draft.choice("priority") {
        table["priority"] = toml_edit::value(priority);
    }
    // `table` exists only on a snapshot source's draft, so no other shape gains it.
    for key in ["poll_interval", "pending_timeout", "table"] {
        if let Some(value) = text(key) {
            table[key] = toml_edit::value(value);
        }
    }
    match text("batch_pattern") {
        Some(p) if !p.is_empty() => table["batch_pattern"] = toml_edit::value(p),
        Some(_) => {
            table.remove("batch_pattern");
        }
        None => {}
    }
    toml_edit::Item::Table(table)
}

/// Everything wrong with the draft as it stands: the rendered table, parsed back and
/// read by exactly the reader that decides which sources the scheduler and ingest
/// runner see (`SourceSpec::from_doc`) — on the object being edited alone, wrapped in a
/// document of its own, for the reason `views::validate` and `groupings::validate` both
/// give for doing the same: validating the whole merged doc would report every other
/// source's problems against this one. The reader errors on an undeclared dataset, so
/// no cross-check of ours is needed — and an idle (empty `paths`) source is only ever a
/// warning (`source_config::IDLE_PATHS`), never blocking `n` on
/// `apply::blocking_diagnostic`'s error gate.
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table: rendered_doc_table(draft),
        }],
    );
    let schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    SourceSpec::from_doc(&doc, &schema).1
}

/// The draft's `sources.toml` entry, rendered and parsed back the way
/// the loader would read it off disk — `views::rendered_doc_table`'s own
/// shape, mirrored here for the same reason.
fn rendered_doc_table(draft: &Draft) -> toml::Table {
    super::object_text(&draft.name, to_table(draft, Destination::Doc))
        .parse::<toml::Table>()
        .unwrap_or_default()
}

/// What each field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)). Meaning and value grammar only — the keys
/// are the hint rows' job — and under ~90 characters, since the slot is one line and
/// never wraps.
///
/// `readiness`/`stable_polls` say out loud that `stable_mtime` is not
/// implemented: the `Choice` offers it (`READINESS`) and discovery
/// orphans every file of such a source, so a sentence presenting it as
/// a working alternative would stop a trader's source loading.
pub fn help(key: &str) -> &'static str {
    match key {
        "dataset" => "The dataset this source's data loads into, as datasets.toml declares it",
        "paths" => "Globs matching the CSV files to load, ';'-separated — empty leaves it idle",
        "readiness" => {
            "A file is complete once its .done sentinel exists — stable_mtime never loads"
        }
        "stable_polls" => {
            "Polls of unchanged size and mtime before a load — stable_mtime only, unbuilt"
        }
        "priority" => "Cold-start order: latest_risk first, then latest_other, then backfill",
        "poll_interval" => {
            "How often directories are scanned, or a snapshot's table reread — 30s, 5m"
        }
        "pending_timeout" => {
            "How long a file may wait for its sentinel before it is reported stuck — 10m"
        }
        "batch_pattern" => {
            "Regex over the file stem with a named 'batch' capture — empty uses the stem"
        }
        "adapter" => {
            "csv_dir watches directories; another adapter subscribes, fetches or reads a table"
        }
        "table" => "The table a snapshot source reads whole on every poll, as its adapter names it",
        "document" => "The document kind a subscribed source publishes, which decides its parsing",
        "topics" => {
            "Topic patterns to subscribe to, levels '/'-separated — '*' one level, '>' the rest"
        }
        "coalesce" => {
            "At most one publish per key within this window — 500ms; 0 publishes every message"
        }
        "source_time" => {
            "What stamps a publish: receive, or document:<field> (a date or RFC 3339 attribute)"
        }
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::super::Domain;
    use super::*;
    use geode_core::config::{Config, ConfigSources, LayerDoc};

    fn config() -> Config {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "sources",
                    "[live]\ndataset = \"risk\"\npaths = [\"/a/*.csv\", \"/b/*.csv\"]\n\
                     readiness = { stable_mtime = 4 }\npriority = \"latest_other\"\n\
                     poll_interval = \"45s\"\nbatch_pattern = '^r_(?P<batch>.+)$'\n\
                     [vols]\ndataset = \"vol\"\npaths = [\"/v/*.csv\"]\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        })
    }

    #[test]
    fn summary_and_prefix_read_the_raw_table() {
        let value = config()
            .doc("sources")
            .unwrap()
            .value
            .get("live")
            .cloned()
            .unwrap();
        assert_eq!(summary(&config(), &value), "2 paths · latest_other");
        assert_eq!(prefix(&value).as_deref(), Some("risk"));
    }

    /// A local dataset is written by the app, never fed by a source (the
    /// source reader refuses one), so the dataset choice does not offer it.
    #[test]
    fn the_dataset_choice_offers_no_local_dataset() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    &format!(
                        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n{}",
                        LOCAL_DATASET
                    ),
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let fields = fields(&config, None);
        let dataset = fields.iter().find(|f| f.key == "dataset").unwrap();
        assert!(
            matches!(&dataset.kind, FieldKind::Choice { options, .. } if options == &["risk"]),
            "{:?}",
            dataset.kind
        );
    }

    /// A computed dataset is answered by a module in process and no source
    /// may feed it (the source reader refuses one), so it is not offered.
    #[test]
    fn the_dataset_choice_offers_no_computed_dataset() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    &format!(
                        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n{}",
                        COMPUTED_DATASET
                    ),
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let fields = fields(&config, None);
        let dataset = fields.iter().find(|f| f.key == "dataset").unwrap();
        assert!(
            matches!(&dataset.kind, FieldKind::Choice { options, .. } if options == &["risk"]),
            "{:?}",
            dataset.kind
        );
    }

    const COMPUTED_DATASET: &str =
        "[pricer]\ncomputed = true\n[pricer.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n";

    const LOCAL_DATASET: &str = r#"[sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]
[sheets.columns.sheet]
type = "utf8"
role = "dimension"
textual = true
[sheets.columns.line]
type = "i64"
role = "axis"
[sheets.columns.qty]
type = "i64"
role = "value"
"#;

    #[test]
    fn fields_spell_every_key_and_default_the_absent_ones() {
        let fields = fields(&config(), Some("live"));
        let by_key = |k: &str| fields.iter().find(|f| f.key == k).unwrap();
        assert!(
            matches!(&by_key("dataset").kind, FieldKind::Choice { options, selected } if options[*selected] == "risk" && options == &["risk", "vol"])
        );
        assert!(matches!(&by_key("paths").kind, FieldKind::Text(t) if t == "/a/*.csv; /b/*.csv"));
        assert!(
            matches!(&by_key("readiness").kind, FieldKind::Choice { options, selected } if options[*selected] == "stable_mtime")
        );
        assert!(matches!(
            &by_key("stable_polls").kind,
            FieldKind::Number {
                value: 4,
                min: 1,
                max: 100,
                ..
            }
        ));
        assert!(
            matches!(&by_key("priority").kind, FieldKind::Choice { options, selected } if options[*selected] == "latest_other")
        );
        assert!(matches!(&by_key("poll_interval").kind, FieldKind::Text(t) if t == "45s"));
        assert!(
            matches!(&by_key("pending_timeout").kind, FieldKind::Text(t) if t == "10m"),
            "the reader's default, spelled"
        );
        assert!(
            matches!(&by_key("batch_pattern").kind, FieldKind::Text(t) if t == "^r_(?P<batch>.+)$")
        );
        assert!(
            fields
                .iter()
                .all(|f| f.dest == Destination::Doc && f.layer.is_none())
        );
    }

    /// Subscribed-source adapter fields survive rendering and validation.
    #[test]
    fn subscribed_source_fields_are_painted_as_read_only_text() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[cvi_params]\nfamily = \"document\"\nkey = [\"underlying_ref\"]\n\
                     axes = [\"term\"]\n[cvi_params.columns.underlying_ref]\n\
                     type = \"utf8\"\nrole = \"dimension\"\n[cvi_params.columns.term]\n\
                     type = \"date\"\nrole = \"axis\"\n[cvi_params.columns.param]\n\
                     type = \"f64\"\nrole = \"value\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "sources",
                    "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\n\
                     document = \"cvi_params\"\ntopics = [\"a/>\", \"b/>\"]\n\
                     coalesce = \"250ms\"\nsource_time = \"document:anchor_date\"\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let fields = fields(&config, Some("cvi"));
        let by_key = |k: &str| fields.iter().find(|f| f.key == k).unwrap();
        assert_eq!(by_key("adapter").kind, FieldKind::Text("demo_bus".into()));
        assert_eq!(
            by_key("document").kind,
            FieldKind::Text("cvi_params".into())
        );
        assert_eq!(
            by_key("topics").kind,
            FieldKind::Text(format!("a/>{PATH_SEPARATOR} b/>"))
        );
        assert_eq!(by_key("coalesce").kind, FieldKind::Text("250ms".into()));
        assert_eq!(
            by_key("source_time").kind,
            FieldKind::Text("document:anchor_date".into())
        );
        for key in ["adapter", "document", "topics", "coalesce", "source_time"] {
            assert!(!text_editable(key), "{key}");
        }
    }

    /// A directory source keeps its nine rows, in order, with no subscribed-only or
    /// snapshot-only row among them.
    #[test]
    fn a_directory_source_still_shows_its_rows() {
        let keys: Vec<String> = fields(&config(), Some("live"))
            .into_iter()
            .map(|f| f.key)
            .collect();
        assert_eq!(
            keys,
            vec![
                "dataset",
                "paths",
                "readiness",
                "stable_polls",
                "priority",
                "poll_interval",
                "pending_timeout",
                "batch_pattern",
                "adapter",
            ]
        );
    }

    /// A reference dataset and a snapshot source over it that sets neither priority
    /// nor poll interval, so the dialog has to spell both defaults. Its hand-written
    /// `topics` is a key the dialog shows no row for.
    fn snapshot_config() -> Config {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [underlyings]\nfamily = \"reference\"\nkey = [\"underlying_ref\"]\n\
                     [underlyings.columns.underlying_ref]\ntype = \"utf8\"\n\
                     role = \"dimension\"\n[underlyings.columns.currency]\n\
                     type = \"utf8\"\nrole = \"attribute\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "sources",
                    "[refdb]\nadapter = \"demo_refdb\"\ndataset = \"underlyings\"\n\
                     table = \"underlyings\"\ntopics = [\"a/>\"]\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        })
    }

    /// A snapshot source has no directory rows and no subscription rows; it shows the
    /// table it reads and the reader's snapshot defaults, not the directory ones.
    #[test]
    fn a_snapshot_source_shows_table_and_its_defaults() {
        let fields = fields(&snapshot_config(), Some("refdb"));
        let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["dataset", "priority", "poll_interval", "table", "adapter"]
        );
        let by_key = |k: &str| fields.iter().find(|f| f.key == k).unwrap();
        assert_eq!(by_key("table").label, "Table");
        assert_eq!(by_key("table").kind, FieldKind::Text("underlyings".into()));
        assert_eq!(by_key("poll_interval").kind, FieldKind::Text("5m".into()));
        assert!(
            matches!(&by_key("priority").kind, FieldKind::Choice { options, selected } if options[*selected] == "latest_other"),
            "{:?}",
            by_key("priority").kind
        );
        assert!(text_editable("table"));
        assert!(!help("table").is_empty());
    }

    /// A snapshot source watches no paths; its browse line names the table it reads
    /// and the snapshot priority default, not `0 paths · latest_risk`.
    #[test]
    fn a_snapshot_source_summary_names_its_table_and_snapshot_priority() {
        let config = snapshot_config();
        let rows = Domain::Sources.objects(&config);
        let refdb = rows.iter().find(|r| r.name == "refdb").unwrap();
        assert_eq!(refdb.summary, "table underlyings · latest_other");
    }

    #[test]
    fn editing_the_table_row_writes_table() {
        let config = snapshot_config();
        let mut draft = Domain::Sources.draft(&config, "refdb");
        let i = draft.fields.iter().position(|f| f.key == "table").unwrap();
        draft.fields[i].kind = FieldKind::Text("underlyings_v2".into());
        let text = super::super::object_text("refdb", to_table(&draft, Destination::Doc));
        assert!(text.contains("table = \"underlyings_v2\""), "{text}");
        assert!(
            text.contains("topics = [\"a/>\"]"),
            "a key the dialog has no row for is kept: {text}"
        );
        for key in ["paths", "readiness", "pending_timeout", "batch_pattern"] {
            assert!(
                !text.contains(key),
                "{key} written to a snapshot source: {text}"
            );
        }
        // The kept `topics` is the reader's only complaint, and only a warning.
        let diags = validate(&draft, &config);
        assert!(
            matches!(&diags[..], [d] if d.severity == geode_core::config::Severity::Warning
                && d.path.as_deref() == Some("sources.refdb.topics")),
            "{diags:?}"
        );
    }

    #[test]
    fn to_table_writes_readiness_polls_only_under_stable_mtime() {
        let config = config();
        let mut draft = Domain::Sources.draft(&config, "live");
        let item = to_table(&draft, Destination::Doc);
        let text = super::super::object_text("live", item);
        assert!(text.contains("readiness = { stable_mtime = 4 }"), "{text}");
        // Step readiness back to sentinel: polls must vanish from the file.
        let i = draft
            .fields
            .iter()
            .position(|f| f.key == "readiness")
            .unwrap();
        if let FieldKind::Choice { selected, .. } = &mut draft.fields[i].kind {
            *selected = 0;
        }
        let text = super::super::object_text("live", to_table(&draft, Destination::Doc));
        assert!(text.contains("readiness = \"sentinel\""), "{text}");
        assert!(!text.contains("stable_mtime"), "{text}");
        assert!(
            text.contains("paths = [\"/a/*.csv\", \"/b/*.csv\"]"),
            "{text}"
        );
    }

    #[test]
    fn parse_text_refuses_bad_durations_and_patterns_and_splits_paths() {
        assert_eq!(parse_text("poll_interval", " 30s ").unwrap(), "30s");
        assert!(
            parse_text("poll_interval", "2 minutes")
                .unwrap_err()
                .contains("45s")
        );
        assert_eq!(
            parse_text("paths", "/a/*.csv ;/b/*.csv;").unwrap(),
            "/a/*.csv; /b/*.csv"
        );
        assert_eq!(
            parse_text("paths", "  ").unwrap(),
            "",
            "no paths is an idle source, not a refusal"
        );
        assert_eq!(parse_text("batch_pattern", "").unwrap(), "");
        assert!(
            parse_text("batch_pattern", "(.+)")
                .unwrap_err()
                .contains("batch")
        );
        assert!(text_editable("paths") && text_editable("batch_pattern"));
        assert!(!text_editable("dataset"));
    }

    #[test]
    fn spell_duration_uses_the_largest_exact_unit() {
        use std::time::Duration;
        assert_eq!(spell_duration(Duration::from_secs(7200)), "2h");
        assert_eq!(spell_duration(Duration::from_secs(600)), "10m");
        assert_eq!(spell_duration(Duration::from_secs(45)), "45s");
        assert_eq!(spell_duration(Duration::from_secs(90)), "90s");
    }

    #[test]
    fn a_new_source_is_idle_with_the_seeded_dataset() {
        let config = config();
        let mut draft = Domain::Sources.new_draft(&config, "fresh");
        seed_dataset(&mut draft, "vol");
        assert_eq!(draft.choice("dataset"), Some("vol"));
        let text = super::super::object_text("fresh", to_table(&draft, Destination::Doc));
        assert!(text.contains("dataset = \"vol\""), "{text}");
        assert!(text.contains("paths = []"), "{text}");
        let diags = validate(&draft, &config);
        assert!(
            diags.iter().any(|d| d.message.contains("idle")),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .all(|d| d.severity != geode_core::config::Severity::Error)
        );
    }

    #[test]
    fn rows_are_sorted_by_dataset_then_name_and_carry_the_prefix() {
        let rows = Domain::Sources.objects(&config());
        let names: Vec<(Option<&str>, &str)> = rows
            .iter()
            .map(|r| (r.prefix.as_deref(), r.name.as_str()))
            .collect();
        assert_eq!(names, vec![(Some("risk"), "live"), (Some("vol"), "vols")]);
        assert_eq!(rows[0].display_name(), "risk · live");
        // The muted lead is exactly the prefix and its separator.
        let shown = rows[0].display_name();
        assert_eq!(&shown[..rows[0].display_lead()], "risk · ");
        assert_eq!(rows[1].display_lead(), "vol · ".len());

        // The fixture above's dataset order and name order happen to
        // agree ("risk" < "vol", "live" < "vols"), so it cannot tell a
        // by-`(prefix, name)` sort from a by-name-alone one — a fixture
        // where they disagree is what actually pins the tie-break:
        // `zsource` feeds `aaa`, `asource` feeds `zzz`, so a name-only
        // sort would list `asource` first while the real sort, dataset
        // first, lists `zsource` first.
        let disagreeing = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[aaa.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [zzz.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "sources",
                    "[zsource]\ndataset = \"aaa\"\npaths = []\n\
                     [asource]\ndataset = \"zzz\"\npaths = []\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let rows = Domain::Sources.objects(&disagreeing);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["zsource", "asource"],
            "sorted by dataset first, not by name"
        );
    }
}
