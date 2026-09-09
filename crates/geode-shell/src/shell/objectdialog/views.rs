//! The `Domain::Views` adapter (spec §8.1): the doc views live in, and
//! the one-line summary a view's browse row shows.
//!
//! One module per domain, holding that domain's own functions and
//! nothing else (spec §4) — the scaffold matches on `Domain` exactly
//! once per function, so an adapter never learns about the stage
//! machine, the key vocabulary or the markers, and the markers never
//! learn about views.
//!
//! Views goes first on purpose (spec §14). It is the only domain whose
//! edit stage has to split one list of columns across two destinations —
//! order, inclusion and width to `view_presentation.toml`, the column
//! set itself to `views.toml` — so building it first settles the
//! vocabulary before three thinner adapters depend on it.

use std::collections::BTreeMap;

use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, Severity, load_views, merge_docs};
use geode_core::schema::SchemaSpec;
use geode_core::view::ViewSpec;

use super::{Destination, Draft, Field, FieldKind, ListItem};

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = "views";

/// The muted second line of a view's browse row: what the view selects,
/// in the order a trader would ask it — which dataset, how many columns,
/// and the rollup it builds.
///
/// Read straight off the TOML value rather than through `ViewSpec` on
/// purpose. `ViewSpec::from_doc` reads the *merged* doc and would need
/// the whole document to produce one view, while this is handed exactly
/// the one table whose provenance the row is reporting — and it must
/// keep describing a view even when that view is malformed, because a
/// browse row a user cannot see is a view they cannot open and fix.
/// That is also why a non-table value still produces a row: `views.toml`
/// with `tree = "oops"` in it is precisely when the dialog needs to show
/// `tree`.
pub fn summary(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "not a table".to_string();
    };
    let dataset = table
        .get("dataset")
        .and_then(|v| v.as_str())
        .unwrap_or("no dataset");
    let columns = table
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut out = match columns {
        1 => format!("{dataset} · 1 column"),
        n => format!("{dataset} · {n} columns"),
    };
    // The grouping is the view's shape, not decoration: two views over
    // the same dataset and the same columns are told apart by nothing
    // else. Named rather than counted for that reason, and with the same
    // arrow the blotter's own header uses for a rollup path.
    let grouping: Vec<&str> = table
        .get("grouping")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !grouping.is_empty() {
        out.push_str(" · grouped by ");
        out.push_str(&grouping.join(" → "));
    }
    out
}

/// The user-layer doc a view's *presentation* is written to (spec §5.6).
///
/// Separate from [`DOC`] on purpose, and the single most important fact
/// in this file: order, inclusion and width land here, merged **over**
/// the view by `config::load_views`, so a trader who drags a column or
/// hides one has not overridden the desk's view and still receives the
/// column the desk adds next week. Only [`Destination::Doc`] forks.
pub const PRESENTATION_DOC: &str = "view_presentation";

/// The fields of one view (spec §8.1), or of no view at all when
/// `object` names nothing — an empty dataset choice and an empty column
/// list, which is what a `Config` with no `views` doc has to produce
/// rather than panicking.
///
/// Read through `load_views`, not `ViewSpec::from_doc`, so the list shows
/// the columns in the order and with the hidden/width state the trader
/// actually sees. `Draft::source` keeps the raw pre-presentation table
/// beside it, which is what a `Doc` write is rendered from.
///
/// **Which destination each field carries is decided here and nowhere
/// else.** `dataset` is `Doc`: it defines what the view selects, so
/// changing it is a definitional change and forks the object. `columns`
/// is `Presentation`: its order, each item's `included` and each item's
/// `width` are exactly what `view_presentation.toml` holds. A change to
/// the column *set* still reaches `views.toml`, but through
/// `Draft::writes_by_destination`'s membership rule rather than through
/// a second destination on this field — so there is one place to read the
/// answer and one place to get it wrong.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let (views, _) = load_views(config);
    let view = object.and_then(|name| views.iter().find(|v| v.name == name));

    let schema = config
        .doc("datasets")
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default();
    let mut options: Vec<String> = schema.datasets.iter().map(|d| d.name.clone()).collect();
    options.sort();
    let current = view.map(|v| v.dataset.clone()).unwrap_or_default();
    // The view's own dataset is always an option, even when the schema
    // has no such dataset (a desk rename, a missing `datasets` doc): a
    // `Choice` that cannot represent the value it is showing would step
    // silently to something else the moment the field is touched.
    if !options.contains(&current) {
        options.insert(0, current.clone());
    }
    let selected = options.iter().position(|o| *o == current).unwrap_or(0);

    let items = view
        .map(|v| {
            v.columns
                .iter()
                .map(|column| {
                    let presentation = v.presentation_of(column.name());
                    ListItem {
                        name: column.name().to_string(),
                        included: !presentation.hidden.unwrap_or(false),
                        width: presentation.width,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    vec![
        Field {
            key: "dataset".to_string(),
            label: "Dataset".to_string(),
            kind: FieldKind::Choice { options, selected },
            dest: Destination::Doc,
        },
        Field {
            key: "columns".to_string(),
            label: "Columns".to_string(),
            kind: FieldKind::OrderedList { items },
            dest: Destination::Presentation,
        },
    ]
}

/// The draft rendered as the table one destination's file receives.
///
/// Each destination is written as a *whole table* for the object, never
/// merged key by key: `views` and `view_presentation` are both atomic at
/// depth one (`config::merge::atomic_depth`), and a key-by-key write
/// would leave a stale `hidden` entry behind the first time a trader
/// unhid the last hidden column.
pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Table {
    match dest {
        Destination::Doc => doc_table(draft),
        Destination::Presentation => presentation_table(draft),
    }
}

/// The view itself, for `views.toml`: the object exactly as the merged
/// doc holds it, with only the `Doc` fields applied over it.
///
/// Starting from `draft.source` rather than from the fields is what keeps
/// a `grouping`, a `sort`, a `join`, a per-column `format`/`label` and a
/// derived column's `sql` alive across an override — none of which the
/// field vocabulary models, all of which would silently vanish if this
/// rendered a view from `dataset` and a list of names.
///
/// Presentation never touches it. The column *order* here stays the
/// source's, not the draft's, because order is presentation; only the
/// member set is definitional, and that is why the columns are rebuilt
/// from the draft's names rather than copied.
fn doc_table(draft: &Draft) -> toml_edit::Table {
    let mut table = super::toml_table_to_edit(&draft.source);
    if let Some(dataset) = draft.choice("dataset") {
        table["dataset"] = toml_edit::value(dataset);
    }
    if let Some(items) = draft.list_items("columns") {
        let wanted: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        table["columns"] = toml_edit::Item::ArrayOfTables(columns_for(&draft.source, &wanted));
    }
    table
}

/// The view's columns as `views.toml` should hold them: every source
/// column whose name the draft still lists, in **source** order, plus a
/// minimal entry for any name the source does not have yet.
fn columns_for(source: &toml::Table, wanted: &[&str]) -> toml_edit::ArrayOfTables {
    let source_columns: Vec<&toml::Table> = source
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_table()).collect())
        .unwrap_or_default();
    let mut out = toml_edit::ArrayOfTables::new();
    let mut seen: Vec<&str> = Vec::new();
    for column in &source_columns {
        let Some(name) = column.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if wanted.contains(&name) {
            seen.push(name);
            out.push(super::toml_table_to_edit(column));
        }
    }
    // A name the source never had is a column the object gained, which is
    // the definitional half of the split. It carries only its name; a
    // `kind` is the reader's default (`measure`), and anything richer
    // needs a field vocabulary the ordered list does not have.
    for name in wanted {
        if seen.contains(name) {
            continue;
        }
        let mut column = toml_edit::Table::new();
        column["name"] = toml_edit::value(*name);
        out.push(column);
    }
    out
}

/// The trader's personal view of the view, for `view_presentation.toml`:
/// the column order, which are hidden, and any widths.
///
/// Rendered fresh from the draft rather than merged into whatever is on
/// disk, because this table is replaced whole (see [`to_table`]). Empty
/// `hidden`/`width` are omitted rather than written as empty containers —
/// a file that says nothing is easier to hand-edit than one full of `[]`.
///
/// **Only what the trader actually changed is written.** `order` and
/// `width` both arrive here off the *effective* view, which already
/// carries whatever `views.toml` declared — so writing every column's
/// position and every declared width back would pin the desk's layout
/// for this trader against the desk's later changes. That is the same
/// freeze [`Destination`] exists to prevent, one field-granularity down,
/// and it would fire on the commonest edit there is: hiding one column
/// would silently adopt the desk's order and widths forever. Both are
/// therefore compared against [`doc_baseline`] — what this same save
/// leaves in `views.toml` — and omitted when they still match it.
/// `hidden` needs no such comparison: nothing but this file can set it.
fn presentation_table(draft: &Draft) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    let items = draft.list_items("columns").unwrap_or_default();
    let (doc_order, doc_widths) = doc_baseline(draft);

    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    if names != doc_order {
        let mut order = toml_edit::Array::new();
        for name in &names {
            order.push(*name);
        }
        table["order"] = toml_edit::value(order);
    }

    let mut hidden = toml_edit::Array::new();
    for item in items.iter().filter(|i| !i.included) {
        hidden.push(item.name.as_str());
    }
    if !hidden.is_empty() {
        table["hidden"] = toml_edit::value(hidden);
    }

    let mut widths = toml_edit::Table::new();
    for item in items {
        let Some(width) = item.width else { continue };
        if doc_widths.get(item.name.as_str()) == Some(&width) {
            continue; // the desk's own width, not the trader's
        }
        widths[item.name.as_str()] = toml_edit::value(f64::from(width));
    }
    if !widths.is_empty() {
        table["width"] = toml_edit::Item::Table(widths);
    }

    table
}

/// The column order and the per-column widths **the view's own doc will
/// hold after this same save** — read back out of [`columns_for`] rather
/// than off `draft.source` directly, so the two halves of one save cannot
/// disagree about what the doc says.
///
/// This is the "pre-presentation view" [`presentation_table`] compares
/// against. Using the post-write doc rather than the raw source is what
/// keeps a membership change honest: adding or dropping a column rewrites
/// `views.toml`'s column list, and the trader has not reordered anything
/// merely by doing so.
fn doc_baseline(draft: &Draft) -> (Vec<&str>, BTreeMap<String, f32>) {
    let wanted: Vec<&str> = draft
        .list_items("columns")
        .unwrap_or_default()
        .iter()
        .map(|i| i.name.as_str())
        .collect();
    let mut order = Vec::with_capacity(wanted.len());
    let mut widths = BTreeMap::new();
    for column in columns_for(&draft.source, &wanted).iter() {
        let Some(name) = column.get("name").and_then(|item| item.as_str()) else {
            continue;
        };
        // The doc's own name is borrowed from the draft's list rather than
        // from the rendered table, which is a temporary.
        let Some(name) = wanted.iter().find(|w| **w == name) else {
            continue;
        };
        if let Some(width) = doc_width(column) {
            widths.insert((*name).to_string(), width);
        }
        order.push(*name);
    }
    (order, widths)
}

/// One column table's declared `width`, read exactly as
/// `ViewSpec::from_doc` reads it — an integer or a float, positive, cast
/// to `f32`. Anything else is not a width the loader would have applied,
/// so it is not one this can be compared against either.
fn doc_width(column: &toml_edit::Table) -> Option<f32> {
    let value = column.get("width")?.as_value()?;
    let width = value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))?;
    (width > 0.0).then_some(width as f32)
}

/// Everything wrong with the draft as it stands (spec §7.2).
///
/// **The draft alone, never the merged result.** The table this renders
/// is wrapped in a `MergedDoc` of its own and handed to `ViewSpec::
/// from_doc` — the same reader the loader uses — so what comes back
/// describes the object being edited and nothing else. Validating the
/// merged doc instead would report every *other* broken view in the
/// config against this dialog's one object, which is both noise and a
/// lie about what the user is editing; anything the merge itself turns up
/// is the reload's to report (spec §7.1).
///
/// The rendered text is what is parsed, not an in-memory shortcut, so
/// what is validated is byte-for-byte what the flush will write.
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    let table = rendered_doc_table(draft);
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table,
        }],
    );
    let (_views, mut diags) = ViewSpec::from_doc(&doc);

    // The one genuinely cross-cutting check the doc's own reader cannot
    // make: `ViewSpec::from_doc` never sees the schema, so a view naming
    // a dataset that does not exist parses cleanly and then compiles into
    // nothing. Reachable exactly because `fields` keeps the view's own
    // dataset in the choice list even when the schema has dropped it.
    if let Some(dataset) = draft.choice("dataset") {
        let known = config
            .doc("datasets")
            .map(|doc| SchemaSpec::from_doc(doc).0)
            .is_some_and(|schema| schema.dataset(dataset).is_some());
        if !known {
            diags.push(Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!(
                    "view '{}': dataset '{dataset}' is not in the schema",
                    draft.name
                ),
            });
        }
    }
    diags
}

/// The draft's `views.toml` table, rendered and parsed back the way the
/// loader would read it off disk.
fn rendered_doc_table(draft: &Draft) -> toml::Table {
    // A round trip through this crate's own writer and the loader's own
    // parser; `unwrap_or_default` rather than a panic because a draft is
    // live UI state and an empty table simply validates as "missing
    // dataset" instead of taking the window down.
    super::object_text(&draft.name, doc_table(draft))
        .parse::<toml::Table>()
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> toml::Value {
        toml::Value::Table(text.parse::<toml::Table>().expect("fixture parses"))
    }

    #[test]
    fn the_summary_names_the_dataset_the_columns_and_the_rollup() {
        let v = value(
            "dataset = \"risk_snapshot\"\ngrouping = [\"lhu\", \"position_ref\"]\n\
             [[columns]]\nname = \"npv\"\n[[columns]]\nname = \"delta01\"\n",
        );
        assert_eq!(
            summary(&v),
            "risk_snapshot · 2 columns · grouped by lhu → position_ref"
        );
    }

    /// A flat view has no rollup to name, and one column is not "1
    /// columns" — the row is read at a glance, so the grammar matters.
    #[test]
    fn a_flat_single_column_view_reads_as_prose() {
        let v = value("dataset = \"risk\"\n[[columns]]\nname = \"npv\"\n");
        assert_eq!(summary(&v), "risk · 1 column");
    }

    /// A malformed view is exactly the one a user opens the dialog to
    /// fix, so it still gets a row and the row still says what is wrong
    /// with it rather than showing an empty line.
    #[test]
    fn a_malformed_view_still_describes_itself() {
        assert_eq!(summary(&toml::Value::String("oops".into())), "not a table");
        assert_eq!(summary(&value("columns = []\n")), "no dataset · 0 columns");
    }
}
