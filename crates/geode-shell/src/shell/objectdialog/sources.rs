//! The Sources adapter (spec §8.3, §19.3) — `Domain::Sources`.
//!
//! Every field is `Destination::Doc`; there is no presentation doc. The
//! browse list is every dataset·source pair, flat — `prefix` is the
//! dataset, painted first and the primary sort key — and `enter` opens
//! the arguments directly. A write reaches the running data service
//! through nothing new: the dialog's in-memory apply runs
//! `apply_reload`, whose `sources` baseline comparison raises the
//! existing restart-required stripe.
//!
//! This is also the first adapter with a genuinely editable `Text` row
//! (`paths`, `poll_interval`, `pending_timeout`, `batch_pattern`, via
//! `i` — Task 1's `text_editable`/`parse_text` machinery), and the first
//! whose object can be created *idle* — `n` seeds only `dataset` and an
//! empty `paths`, which the reader (`SourceSpec::from_doc`) accepts as a
//! warning rather than an error (§19.3's ruling on `geode-core`'s own
//! reader, `source_config.rs`), so a fresh source never blocks the
//! confirm on the `Severity::Error` gate `apply::blocking_diagnostic`
//! enforces.

use super::{Destination, Draft, Field, FieldKind};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::schema::SchemaSpec;
use geode_core::source_config::{
    CSV_DIR_ADAPTER, DEFAULT_COALESCE, DEFAULT_PENDING_TIMEOUT, DEFAULT_POLL, SourceSpec,
    check_batch_pattern, parse_duration,
};
use std::time::Duration;

pub const DOC: &str = "sources";
/// Between globs in the `paths` text (§19.3). `;` is not reserved by
/// either platform's filesystem — NTFS's own reserved set is `< > : " /
/// \ | ? *` — the choice is unambiguous only in the common case: it is
/// rare inside a glob and never one of glob's own metacharacters, while
/// a space is legal in a path on both platforms and so cannot separate
/// them. A glob that needs a literal `;` cannot be expressed in this
/// field.
pub const PATH_SEPARATOR: char = ';';

const READINESS: [&str; 2] = ["sentinel", "stable_mtime"];
const PRIORITY: [&str; 3] = ["latest_risk", "latest_other", "backfill"];

/// The browse row's muted second line: how many paths a source watches
/// and which cold-start priority it claims — the two facts a trader
/// scans the list for (§19.3). Read straight off the raw table, for the
/// reason `views::summary` and `groupings::summary` both give for doing
/// the same: a malformed source is exactly the one this dialog exists to
/// fix, and the reader would drop it from the merged result entirely.
pub fn summary(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "not a table".to_string();
    };
    let n = table
        .get("paths")
        .and_then(|v| v.as_array())
        .map_or(0, |a| a.len());
    let priority = table
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or(PRIORITY[0]);
    format!("{n} path{} · {priority}", if n == 1 { "" } else { "s" })
}

/// The dataset a source feeds — the browse row's prefix (§19.3,
/// `Domain::prefix_fn`).
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

/// The eight fields of one source, or of no source at all when `object`
/// names nothing (`n`'s empty draft): `dataset` and `priority` are
/// choices, `readiness`/`stable_polls` split the reader's one
/// `readiness` key into a kind and its poll count, and the rest are
/// editable text (§19.3's own list — `text_editable`).
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    let get_str = |key: &str| table.and_then(|t| t.get(key)).and_then(|v| v.as_str());

    let schema = config
        .doc("datasets")
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default();
    let mut datasets: Vec<&str> = schema.datasets.iter().map(|d| d.name.as_str()).collect();
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

    vec![
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
        // The five subscribed-source fields (market-data-documents plan,
        // Task 5) are read-only here — `to_table` never writes them back
        // and `text_editable` never offers `i` on them — so a subscribed
        // source is at least visibly one, not silently painted as a
        // directory source missing its `paths`. A real editing surface
        // for these (a topic-pattern list, an adapter picker) is future
        // work; this dialog's own vocabulary predates the adapter.
        text(
            "adapter",
            "Adapter",
            get_str("adapter").unwrap_or(CSV_DIR_ADAPTER).to_string(),
        ),
        text(
            "document",
            "Document",
            get_str("document").unwrap_or("").to_string(),
        ),
        text(
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
        ),
        text(
            "coalesce",
            "Coalesce",
            duration("coalesce", DEFAULT_COALESCE),
        ),
        text(
            "source_time",
            "Source time",
            get_str("source_time").unwrap_or("receive").to_string(),
        ),
    ]
}

/// `n` seeds the new source's dataset from the browse row under the
/// cursor (§19.3), not the schema's first — the option is added when the
/// schema lacks it, for the same reason `fields` keeps an object's own.
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
/// kind, so only the four text fields need this door.
pub fn text_editable(key: &str) -> bool {
    matches!(
        key,
        "paths" | "poll_interval" | "pending_timeout" | "batch_pattern"
    )
}

/// The inline refusals (§19.3): a duration the reader could not read, a
/// regex that does not compile or has no `batch` capture. `paths` is
/// normalised to the canonical `a; b` spelling; empty is legal — an idle
/// source, the reader's own ruling (`source_config::IDLE_PATHS`).
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
    for key in ["poll_interval", "pending_timeout"] {
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

/// Everything wrong with the draft as it stands (spec §7.2): the
/// rendered table, parsed back and read by exactly the reader that
/// decides which sources the scheduler and ingest runner see
/// (`SourceSpec::from_doc`) — on the object being edited alone, wrapped
/// in a document of its own, for the reason `views::validate` and
/// `groupings::validate` both give for doing the same: validating the
/// whole merged doc would report every other source's problems against
/// this one. The reader errors on an undeclared dataset, so no
/// cross-check of ours is needed — and an idle (empty `paths`) source is
/// only ever a warning (`source_config::IDLE_PATHS`), never blocking `n`
/// on `apply::blocking_diagnostic`'s error gate.
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
        assert_eq!(summary(&value), "2 paths · latest_other");
        assert_eq!(prefix(&value).as_deref(), Some("risk"));
    }

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
                max: 100
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

    /// A subscribed source's five adapter fields (market-data-documents
    /// plan, Task 5) are painted, not silently dropped — so this dialog
    /// never paints one as a directory source missing its `paths` — but
    /// stay `!text_editable`: `to_table` never writes any of them back,
    /// so an editable row here would look live and do nothing.
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
