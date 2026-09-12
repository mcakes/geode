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
use geode_core::schema::{ColumnRole, DatasetSpec, SchemaSpec};
use geode_core::view::{ViewColumn, ViewSpec};

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
/// Read through `load_views`, not `ViewSpec::from_doc`, so the view's own
/// columns are listed in the order and with the hidden/width state the
/// trader actually sees. Behind them, the field carries a second list
/// (§18.7): every other column the chosen dataset has, the "available"
/// catalogue a new or growing view adds from — `Some`, and possibly
/// empty, because Views is a domain where a catalogue EXISTS even once
/// the trader has added everything in it (`Draft::remove_selected`'s own
/// doc has what depends on that). `Draft::source` keeps the raw
/// pre-presentation table beside them, which is what a `Doc` write is
/// rendered from.
///
/// **Every entry of either list carries the `kind` a first-time
/// `[[columns]]` write of it needs** ([`ListItem::kind`]): its own
/// [`ViewColumn`] variant for a column already in the view, the schema
/// role for an available one ([`schema_role_kind`]). Getting this wrong
/// is not
/// cosmetic — `ViewSpec::from_doc` defaults a missing `kind` to
/// `"measure"`, so an added dimension written without one is silently
/// summed as nothing (an `Aggregate::Sum` over a schema-Key/Dimension
/// column has no grain to aggregate at) or, worse, resolved as a real
/// measure of the same name that happens to exist. A schema role with no
/// honest `[[columns]]` kind — `key`, `attribute` — is therefore not
/// offered as available at all: seeing `schema_role_kind`'s own doc.
///
/// **Derived dimensions (`dimensions.toml`) are deliberately NOT in the
/// available catalogue**, even though a config author can group or scope
/// by one today. `ViewSpec::from_doc` accepts exactly three `kind` strings —
/// `"dimension"`, `"measure"`, `"derived"` (a SQL expression, needing its
/// own `sql` this dialog has no way to invent) — and none of them makes a
/// derived dimension queryable as a plain `[[columns]]` entry: a derived
/// dimension's value only ever exists via `compile.rs`'s own `case`
/// expression over `view.grouping`/scope columns, never through
/// `view.columns`. Offering `space` on a row that would silently do
/// nothing once written is the exact defect class this whole design
/// exists to remove, so the row is not offered rather than offered wrong.
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
    let current = match view {
        Some(v) => v.dataset.clone(),
        // A view that does not exist yet has no dataset to keep; it
        // starts on the schema's first (sorted) dataset rather than on
        // an empty placeholder the reader would reject with an error —
        // which would block `commit_create` before a trader could pick.
        None => options.first().cloned().unwrap_or_default(),
    };
    // The view's own dataset is always an option, even when the schema
    // has no such dataset (a desk rename, a missing `datasets` doc): a
    // `Choice` that cannot represent the value it is showing would step
    // silently to something else the moment the field is touched.
    if !options.contains(&current) {
        options.insert(0, current.clone());
    }
    let selected = options.iter().position(|o| *o == current).unwrap_or(0);

    let items: Vec<ListItem> = view
        .map(|v| {
            v.columns
                .iter()
                .map(|column| {
                    let presentation = v.presentation_of(column.name());
                    ListItem {
                        name: column.name().to_string(),
                        included: !presentation.hidden.unwrap_or(false),
                        width: presentation.width,
                        kind: Some(view_column_kind(column).to_string()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // The available catalogue (§18.2, §18.7): the chosen dataset's other
    // columns, none of them the view's until `space` moves one across.
    // `Some` even when the dataset is unknown or has nothing left to
    // offer — the catalogue exists on this domain; it is merely empty.
    // No derived dimensions in it — see this function's own doc for why.
    let available = Some(match schema.dataset(&current) {
        Some(dataset) => dataset_catalogue(&items, dataset),
        None => Vec::new(),
    });

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
            kind: FieldKind::OrderedList { items, available },
            dest: Destination::Presentation,
        },
    ]
}

/// Rebuild the `columns` field's available catalogue after the `dataset`
/// field changes (spec §18.2): "changing the dataset empties Available
/// and repopulates it; members that the new dataset lacks stay listed, as
/// today, so the diagnostic can name them." The view's own columns are
/// left exactly as they are — untouched, in place, `kind` included —
/// because they are still the view's own regardless of what the new
/// dataset has; [`validate`] is what tells the trader a column the new
/// dataset lacks is now a problem. Only the catalogue is thrown away and
/// rebuilt wholesale from the new dataset's columns, so a stale row from
/// the old dataset can never linger behind the new one's.
///
/// Called from `render::maybe_refresh_available`, after a `Toggle`/
/// `ToggleBack` step on the `dataset` field itself — never from
/// `Draft::step_selected`, which has no `Config` to read a schema from.
///
/// The cursor is preserved by IDENTITY (review round 1, finding 3), not
/// by re-clamping its raw index: the caller only ever calls this with
/// the cursor on the `dataset` field itself
/// (`render::maybe_refresh_available`'s own `is_dataset_row` guard), and
/// an index-based clamp — `draft.selected.min(new_len - 1)` — happened
/// to keep landing on that same field only because [`Draft::visible_rows`]
/// now sorts by row rather than by fuzzy score (review round 1, finding
/// 2): the dataset field is always `rows()`'s very first entry, so
/// whenever it matches the query at all it is unconditionally the first
/// SURVIVING row too, index-clamp or not. `Draft::follow` is the general,
/// correct primitive regardless — indexing is what this whole task's
/// other two fixes replaced everywhere else a cursor had to survive a
/// list changing under it, and this call site should not be the one
/// spot still reasoning about a raw index.
///
/// The `None` arm below is narrower than it might look: it only fires
/// when `cursor` (`draft.selected_row()`, read *before* the rebuild)
/// could not resolve to a row at all, and clamps `selected` to the
/// rebuilt list's last visible row. A row that *did* resolve but is gone
/// *after* the rebuild takes the `Some` arm instead and calls
/// `draft.follow`, which is not a clamp — a miss there leaves `selected`
/// exactly where it was, potentially out of bounds against the shrunk
/// list. That path is unreachable through today's one caller (the same
/// identity argument above), but it is not this function's job to
/// assume so, and no clamp runs there if it ever isn't.
pub fn refresh_available(draft: &mut Draft, config: &Config) {
    let Some(current) = draft.choice("dataset").map(str::to_string) else {
        return;
    };
    let cursor = draft.selected_row();
    let Some(field) = draft.fields.iter_mut().find(|f| f.key == "columns") else {
        return;
    };
    let FieldKind::OrderedList { items, available } = &mut field.kind else {
        return;
    };
    let schema = config
        .doc("datasets")
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default();
    let rebuilt = match schema.dataset(&current) {
        Some(dataset) => dataset_catalogue(items, dataset),
        None => Vec::new(),
    };
    *available = Some(rebuilt);
    match cursor {
        Some(row) => draft.follow(row),
        None => {
            let last_visible = draft.visible_rows().len().saturating_sub(1);
            draft.selected = draft.selected.min(last_visible);
        }
    }
}

/// `dataset`'s columns that the view does not already have, each
/// carrying the `kind` a first-time write of it needs
/// ([`schema_role_kind`]) — the catalogue builder [`fields`] and
/// [`refresh_available`] share, so a dataset switch cannot populate a
/// different catalogue than opening the view fresh would have.
fn dataset_catalogue(items: &[ListItem], dataset: &DatasetSpec) -> Vec<ListItem> {
    let mut available = Vec::new();
    for column in &dataset.columns {
        if items.iter().any(|i| i.name == column.name) {
            continue;
        }
        let Some(kind) = schema_role_kind(&column.role) else {
            continue;
        };
        available.push(ListItem {
            name: column.name.clone(),
            included: false,
            width: None,
            kind: Some(kind.to_string()),
        });
    }
    available
}

/// The `[[columns]]` `kind` string a schema role maps to, or `None` when
/// no kind `ViewSpec::from_doc` recognises describes it honestly.
///
/// `ColumnRole::Key` and `ColumnRole::Attribute` have no such kind: the
/// reader accepts only `"dimension"`, `"measure"` and `"derived"` (the
/// last needing a `sql` expression this dialog cannot invent from a bare
/// column name), and forcing either role into `"dimension"` or
/// `"measure"` would mislabel it exactly the way a missing `kind` used to
/// (silently, and only visible once the blotter comes back wrong or the
/// column vanishes). `None` here is why such a column is not in the
/// available catalogue at all — see [`fields`]'s own doc.
fn schema_role_kind(role: &ColumnRole) -> Option<&'static str> {
    match role {
        ColumnRole::Dimension { .. } => Some("dimension"),
        ColumnRole::Measure { .. } => Some("measure"),
        ColumnRole::Key | ColumnRole::Attribute { .. } => None,
        // Document family only; a measure-family view never sees these.
        ColumnRole::Axis | ColumnRole::Value => None,
    }
}

/// The `[[columns]]` `kind` string an already-parsed view column resolves
/// to — read off the [`ViewColumn`] variant itself, not re-derived from
/// the schema, because a `Derived` column (a SQL expression) has no
/// schema role to derive it from at all.
fn view_column_kind(column: &ViewColumn) -> &'static str {
    match column {
        ViewColumn::Dimension { .. } => "dimension",
        ViewColumn::Measure { .. } => "measure",
        ViewColumn::Derived { .. } => "derived",
    }
}

/// The draft rendered as the table one destination's file receives.
///
/// Each destination is written as a *whole table* for the object, never
/// merged key by key: `views` and `view_presentation` are both atomic at
/// depth one (`config::merge::atomic_depth`), and a key-by-key write
/// would leave a stale `hidden` entry behind the first time a trader
/// unhid the last hidden column.
///
/// Wrapped in `toml_edit::Item::Table` for [`Domain::to_table`]'s sake —
/// every Views field is a table, so this adapter never sees the bare
/// array Groupings does (`groupings::to_table`).
///
/// [`Domain::to_table`]: super::Domain::to_table
pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item {
    let table = match dest {
        Destination::Doc => doc_table(draft),
        Destination::Presentation => presentation_table(draft),
    };
    toml_edit::Item::Table(table)
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
/// source's, not the draft's, because order is presentation; only which
/// columns the view HAS is definitional, and that is why the columns are
/// rebuilt from the draft's names rather than copied.
fn doc_table(draft: &Draft) -> toml_edit::Table {
    let mut table = super::toml_table_to_edit(&draft.source);
    if let Some(dataset) = draft.choice("dataset") {
        table["dataset"] = toml_edit::value(dataset);
    }
    if let Some(items) = draft.list_items("columns") {
        // The view's own list defines the view — a column still sitting in
        // the available catalogue is not one of its columns, which is why
        // `available_items` is not read here at all.
        let wanted: Vec<&ListItem> = items.iter().collect();
        table["columns"] = toml_edit::Item::ArrayOfTables(columns_for(&draft.source, &wanted));
    }
    table
}

/// The view's columns as `views.toml` should hold them: every source
/// column whose name the draft still lists, in **source** order, plus a
/// minimal entry — name and `kind` — for any name the source does not
/// have yet, `kind` read off the item itself
/// ([`ListItem::kind`]) rather than defaulted, so a newly promoted
/// dimension is never silently written (and read back) as a measure.
fn columns_for(source: &toml::Table, wanted: &[&ListItem]) -> toml_edit::ArrayOfTables {
    let names: Vec<&str> = wanted.iter().map(|i| i.name.as_str()).collect();
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
        if names.contains(&name) {
            seen.push(name);
            out.push(super::toml_table_to_edit(column));
        }
    }
    // A name the source never had is a column the object gained, which is
    // the definitional half of the split. It carries its name and its
    // `kind` — never the reader's default (`measure`), which is exactly
    // what silently mis-resolves a newly added dimension (this function's
    // own doc). Anything richer than name and kind (a `label`, a
    // `format`) needs a field vocabulary the ordered list does not have.
    for item in wanted {
        if seen.contains(&item.name.as_str()) {
            continue;
        }
        let mut column = toml_edit::Table::new();
        column["name"] = toml_edit::value(item.name.as_str());
        if let Some(kind) = &item.kind {
            column["kind"] = toml_edit::value(kind.as_str());
        }
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
    // Only the view's own columns are presented at all — a column still
    // in the available catalogue is not part of the view, so it has no
    // order, no hidden state and no width to write here either.
    let items: Vec<&ListItem> = draft
        .list_items("columns")
        .unwrap_or_default()
        .iter()
        .collect();
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
/// keeps a change to the column set honest: adding or dropping one
/// rewrites
/// `views.toml`'s column list, and the trader has not reordered anything
/// merely by doing so.
fn doc_baseline(draft: &Draft) -> (Vec<&str>, BTreeMap<String, f32>) {
    let wanted: Vec<&ListItem> = draft
        .list_items("columns")
        .unwrap_or_default()
        .iter()
        .collect();
    let mut order = Vec::with_capacity(wanted.len());
    let mut widths = BTreeMap::new();
    for column in columns_for(&draft.source, &wanted).iter() {
        let Some(name) = column.get("name").and_then(|item| item.as_str()) else {
            continue;
        };
        // The doc's own name is borrowed from the draft's list rather than
        // from the rendered table, which is a temporary.
        let Some(item) = wanted.iter().find(|w| w.name == name) else {
            continue;
        };
        if let Some(width) = doc_width(column) {
            widths.insert(item.name.clone(), width);
        }
        order.push(item.name.as_str());
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
                // `None`, like every other build site in the workspace:
                // 4b added the field and filled it in from no reader
                // ("Not filled in by any reader in 4b" — its own doc),
                // and nothing yet reads it back. Attaching a field-row
                // path here via `with_path` is the feature that field
                // was added for, not something a merge should invent.
                path: None,
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
    super::object_text(&draft.name, toml_edit::Item::Table(doc_table(draft)))
        .parse::<toml::Table>()
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::super::{Domain, EditRow, Step};
    use super::*;
    use geode_core::config::ConfigSources;

    fn value(text: &str) -> toml::Value {
        toml::Value::Table(text.parse::<toml::Table>().expect("fixture parses"))
    }

    /// A `Config` assembled from literal per-layer documents — the same
    /// shape `objectdialog::tests::config_from` builds (this adapter's own
    /// tests need a two-dataset fixture no existing helper here reaches).
    fn config_from(docs: &[(Layer, &str, &str)]) -> Config {
        let layered: Vec<LayerDoc> = docs
            .iter()
            .map(|(layer, name, text)| LayerDoc {
                layer: *layer,
                name: (*name).to_string(),
                file: std::path::PathBuf::from(format!("<test:{}:{name}>", layer.name())),
                table: text.parse().expect("fixture TOML parses"),
            })
            .collect();
        Config::load(&ConfigSources {
            builtin: layered,
            desk: None,
            user: None,
        })
    }

    /// `tree` selects `npv` out of a `risk` dataset that also has `book`
    /// and `delta01`, plus one derived dimension (`desk`) — the fixture
    /// every two-list test below shares. `desk` stays in the
    /// fixture (rather than being dropped along with the rest of this
    /// module's derived-dimension handling) precisely so
    /// [`the_column_list_is_members_then_the_datasets_other_columns`] can
    /// prove it is deliberately excluded, not merely absent because
    /// nothing declared one.
    ///
    /// `instrument_id` (a `Key`) and `strike` (an `Attribute`) are here
    /// for the same reason and no other: [`schema_role_kind`] answers
    /// `None` for both roles, and without a column of each in the
    /// fixture nothing could tell that exclusion from an empty match arm
    /// (see [`key_and_attribute_columns_are_not_offered`]).
    fn tree_with_two_available_columns() -> Config {
        config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.instrument_id]\ntype = \"utf8\"\nrole = \"key\"\n\
                 [risk.columns.strike]\ntype = \"f64\"\nrole = \"attribute\"\ngrain = \"instrument\"\n",
            ),
            (Layer::Builtin, "dimensions", "[desk]\nfrom = \"book\"\n"),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ])
    }

    /// §18.7: the view's own columns are `items`, in the view's own
    /// order; the dataset's other columns are the `available` catalogue
    /// behind them — a new or growing view has something to tick. Each
    /// entry of either list carries the `kind` a first-time write would
    /// need: `npv`'s own (already a `ViewColumn::Measure` in the view),
    /// `book`'s schema role (`dimension`), `delta01`'s (`measure`) — and
    /// `desk`, a *derived* dimension, appears in neither list at all
    /// (`fields`'s own doc has the reasoning).
    #[test]
    fn the_column_list_is_members_then_the_datasets_other_columns() {
        let config = tree_with_two_available_columns();
        let draft = Domain::Views.draft(&config, "tree");
        let shape = |items: &[ListItem]| -> Vec<(String, bool, Option<String>)> {
            items
                .iter()
                .map(|i| (i.name.clone(), i.included, i.kind.clone()))
                .collect()
        };
        assert_eq!(
            shape(draft.list_items("columns").unwrap()),
            [("npv".to_string(), true, Some("measure".to_string()))]
        );
        assert_eq!(
            shape(draft.available_items("columns").unwrap()),
            [
                ("book".to_string(), false, Some("dimension".to_string())),
                ("delta01".to_string(), false, Some("measure".to_string())),
            ]
        );
        // The third row variant is what makes every consumer say what an
        // available row means, rather than treating it as one of the
        // view's own columns by omission (§18.7.1).
        assert!(
            draft
                .rows()
                .contains(&EditRow::Available { field: 1, item: 0 })
        );
    }

    /// A `Key` and an `Attribute` column are not offered in the
    /// available block, each for its own reason and neither by accident.
    /// `ViewSpec::from_doc` accepts only `"dimension"`, `"measure"` and
    /// `"derived"`, so writing either role into `[[columns]]` means
    /// picking a kind that is not true of it — the exact defect
    /// `ListItem.kind` exists to remove (a promoted column silently
    /// summing, or vanishing from the view). Asserted per role, so
    /// mislabelling one of the two arms cannot hide behind the other.
    #[test]
    fn key_and_attribute_columns_are_not_offered() {
        let config = tree_with_two_available_columns();
        let draft = Domain::Views.draft(&config, "tree");
        // Neither list may hold one: `items` is what the view already
        // has, `available` what it may gain.
        let names: Vec<&str> = draft
            .list_items("columns")
            .unwrap()
            .iter()
            .chain(draft.available_items("columns").unwrap())
            .map(|i| i.name.as_str())
            .collect();
        assert!(
            !names.contains(&"instrument_id"),
            "a Key column has no honest [[columns]] kind: {names:?}"
        );
        assert!(
            !names.contains(&"strike"),
            "nor does an Attribute column: {names:?}"
        );
        assert_eq!(
            schema_role_kind(&ColumnRole::Key),
            None,
            "and the exclusion is the role's own answer, not a filter \
             somewhere above it"
        );
        assert_eq!(
            schema_role_kind(&ColumnRole::Attribute {
                grain: Some(geode_core::schema::Grain::Instrument)
            }),
            None
        );
    }

    /// Hiding one of the view's own columns is presentation; adding one
    /// out of the catalogue is definitional — the split this whole design
    /// exists to keep.
    #[test]
    fn adding_an_available_column_is_a_doc_write_and_hiding_is_not() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3 …
        draft.selected = 2;
        assert!(draft.toggle_selected().changed()); // hide npv
        assert_eq!(
            draft
                .writes_by_destination()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [Destination::Presentation]
        );
        draft.mark_saved();
        draft.selected = 3;
        assert!(draft.toggle_selected().changed()); // add book
        let dests = draft.writes_by_destination();
        assert!(
            dests.contains_key(&Destination::Doc),
            "membership is definitional"
        );
        let items = draft.list_items("columns").unwrap();
        assert_eq!(items[1].name, "book");
        assert!(items[1].included, "an added column joins the view, shown");
        assert_eq!(
            draft
                .available_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["delta01"],
            "and leaves the catalogue it came from"
        );
        // The doc table now lists both, in source order then additions.
        let text =
            super::super::object_text("tree", Domain::Views.to_table(&draft, Destination::Doc));
        assert!(
            text.contains("name = \"npv\"") && text.contains("name = \"book\""),
            "{text}"
        );
        // `book` is a schema `dimension`, and the newly written entry has
        // to say so — a missing `kind` defaults to `"measure"` in
        // `ViewSpec::from_doc`, and a dimension resolved as a measure
        // compiles to nothing (`views::fields`'s own doc has the chain).
        assert!(
            text.contains("kind = \"dimension\""),
            "the added column must carry its real kind, not the reader's \
             measure default:\n{text}"
        );
        // And `delta01`, still merely available (never promoted), must
        // not appear in `views.toml` at all — the single line this
        // whole split exists to keep honest.
        assert!(
            !text.contains("delta01"),
            "an available column reached the doc table it has no business \
             being in:\n{text}"
        );
    }

    /// The row promoted above (`book`) is the catalogue's FIRST, so
    /// appending it to the view's own list and inserting it at its own
    /// index in that list land in the same place by coincidence.
    /// Promoting a later catalogue row — `delta01`, with `book` still
    /// ahead of it — is what actually distinguishes the two: get it
    /// wrong and the added column lands at the front of the view.
    #[test]
    fn adding_a_later_available_column_still_joins_the_end_of_the_views_own_list() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3, delta01=4 …
        draft.selected = 4;
        assert!(draft.toggle_selected().changed());
        assert_eq!(
            draft
                .list_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["npv", "delta01"],
            "delta01 joins the END of the view's own columns, never its front"
        );
        assert!(draft.list_items("columns").unwrap()[1].included);
        assert_eq!(
            draft
                .available_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["book"],
            "book is still merely available"
        );
    }

    /// The name of the list item under the cursor, for the three tests
    /// below — `None` on a field row or past the end.
    fn cursor_item(draft: &Draft) -> Option<String> {
        match draft.selected_row()? {
            EditRow::Item { field, item } => match &draft.fields[field].kind {
                FieldKind::OrderedList { items, .. } => Some(items[item].name.clone()),
                _ => None,
            },
            EditRow::Available { field, item } => match &draft.fields[field].kind {
                FieldKind::OrderedList {
                    available: Some(available),
                    ..
                } => Some(available[item].name.clone()),
                _ => None,
            },
            EditRow::Field(_) => None,
        }
    }

    /// A trader adding several columns wants the cursor where their eye
    /// is: on the next available row, not on the column that just left
    /// for the view's own list (user ruling 2026-09-11). The added item
    /// moves *earlier* in row order, so the set of rows ahead of the next
    /// one is unchanged and its visible index is the old cursor plus one.
    #[test]
    fn adding_a_column_leaves_the_cursor_on_the_next_available_row() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3, delta01=4
        draft.selected = 3;
        assert!(draft.toggle_selected().changed()); // add book
        assert_eq!(
            cursor_item(&draft).as_deref(),
            Some("delta01"),
            "the cursor moved on to the next available column"
        );
        assert_eq!(draft.selected, 4, "one visible row further down");
    }

    /// Adding the block's LAST available column has no next row to move
    /// on to, so the cursor stays at the same visible index — which is
    /// now the row that preceded it — rather than running off the end.
    #[test]
    fn adding_the_last_available_column_leaves_the_cursor_on_the_row_before() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = 4; // delta01, the last row
        assert!(draft.toggle_selected().changed());
        assert_eq!(
            cursor_item(&draft).as_deref(),
            Some("book"),
            "book, still available, is now the last row and the cursor is on it"
        );
        assert_eq!(draft.selected, 4);
    }

    /// Under a filter, "the next row" means the next VISIBLE one, the
    /// same reading `shift+j` gives it: `bb` sits between `aa` and `ab`
    /// in row order but the query hides it, so the cursor steps to `ab`.
    #[test]
    fn adding_a_column_under_a_filter_moves_the_cursor_to_the_next_visible_row() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.aa]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.bb]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.ab]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.instrument_id]\ntype = \"utf8\"\nrole = \"key\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.query = "a".to_string();
        let rows = draft.rows();
        let visible: Vec<String> = draft
            .visible_rows()
            .iter()
            .filter_map(|m| rows.get(m.row))
            .map(|r| draft.row_label(*r))
            .collect();
        assert!(
            !visible.iter().any(|l| l == "bb"),
            "sanity: the query must hide bb: {visible:?}"
        );
        draft.selected = visible
            .iter()
            .position(|l| l == "aa")
            .unwrap_or_else(|| panic!("aa should be visible: {visible:?}"));
        assert!(draft.toggle_selected().changed()); // add aa
        assert_eq!(
            cursor_item(&draft).as_deref(),
            Some("ab"),
            "the cursor skipped the hidden bb for the next visible row"
        );
    }

    /// `x` removes a column outright — the definitional twin of
    /// `space`'s hide — and it lands at the END of the available
    /// catalogue, not merely wherever it happened to sit.
    #[test]
    fn x_moves_a_member_to_the_end_of_available() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = 2; // npv
        assert!(draft.remove_selected().changed());
        assert!(draft.list_items("columns").unwrap().is_empty());
        let available: Vec<&str> = draft
            .available_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(available, ["book", "delta01", "npv"]);
        assert!(
            draft
                .writes_by_destination()
                .contains_key(&Destination::Doc)
        );
    }

    /// A view with two members (`npv`, `book`) and one available column
    /// (`delta01`), for the cursor tests below.
    fn tree_with_two_members() -> Config {
        config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.instrument_id]\ntype = \"utf8\"\nrole = \"key\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n",
            ),
        ])
    }

    /// Like `space`'s add, `x` leaves the cursor where the trader's eye
    /// is — on the row that was next — rather than following the removed
    /// column to the end of the available block (user ruling 2026-09-11).
    /// The removed item moves *later* in row order, so the rows ahead of
    /// the next one lose exactly one and the next one now sits at the old
    /// visible index.
    #[test]
    fn x_leaves_the_cursor_on_the_next_row() {
        let config = tree_with_two_members();
        let mut draft = Domain::Views.draft(&config, "tree");
        // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3, delta01=4
        draft.selected = 2; // npv
        assert!(draft.remove_selected().changed());
        assert_eq!(
            cursor_item(&draft).as_deref(),
            Some("book"),
            "the cursor stayed on the next member, not with npv at the bottom"
        );
        assert_eq!(draft.selected, 2, "the same visible index");
    }

    /// Removing the list's LAST row leaves nothing next, and the same
    /// visible index would hold the removed item itself (it moved to the
    /// end, which is where it already was), so the cursor steps back to
    /// the previous row instead — `dd` on the last line in vim.
    #[test]
    fn x_on_the_last_row_steps_the_cursor_back_to_the_previous_row() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.instrument_id]\ntype = \"utf8\"\nrole = \"key\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3 — and
        // nothing available, so book is the last row of the whole list.
        draft.selected = 3; // book
        assert!(draft.remove_selected().changed());
        assert_eq!(
            cursor_item(&draft).as_deref(),
            Some("npv"),
            "nothing followed book, so the cursor stepped back to npv"
        );
        assert_eq!(draft.selected, 2);
    }

    /// Under a filter "the next row" is the next VISIBLE one: `bb` sits
    /// between `aa` and `ab` in row order but the query hides it.
    #[test]
    fn x_under_a_filter_leaves_the_cursor_on_the_next_visible_row() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.aa]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.bb]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.ab]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.instrument_id]\ntype = \"utf8\"\nrole = \"key\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"aa\"\n\
                 [[tree.columns]]\nname = \"bb\"\n[[tree.columns]]\nname = \"ab\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.query = "a".to_string();
        let rows = draft.rows();
        let visible: Vec<String> = draft
            .visible_rows()
            .iter()
            .filter_map(|m| rows.get(m.row))
            .map(|r| draft.row_label(*r))
            .collect();
        assert!(
            !visible.iter().any(|l| l == "bb"),
            "sanity: the query must hide bb: {visible:?}"
        );
        draft.selected = visible
            .iter()
            .position(|l| l == "aa")
            .unwrap_or_else(|| panic!("aa should be visible: {visible:?}"));
        assert!(draft.remove_selected().changed()); // remove aa
        assert_eq!(
            cursor_item(&draft).as_deref(),
            Some("ab"),
            "the cursor skipped the hidden bb for the next visible row"
        );
    }

    /// `x` on a row the view does not have yet has nothing to remove, and
    /// says so with the verb that actually adds it — the second of
    /// `remove_selected`'s two distinguishable refusals (the first,
    /// `"space unticks here"`, is Groupings' own —
    /// `a_groupings_list_has_no_available_block_and_x_refuses` in
    /// `mod.rs`).
    #[test]
    fn x_on_an_available_row_says_not_in_the_view() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = 3; // book, available
        assert_eq!(
            draft.remove_selected(),
            Step::Refused("not in the view — space adds it".to_string())
        );
    }

    /// §18.7.2: `shift+j`/`shift+k` move within the view's own columns
    /// only. The available catalogue is unordered by construction —
    /// nothing writes it and nothing reads its order — so a reorder there
    /// is inert rather than a move painted in one list and written in
    /// neither.
    ///
    /// The fixture is `tree_with_two_members` rather than this module's
    /// usual one, and deliberately: an available row's index is a
    /// position in the CATALOGUE, and the failure this pins is one that
    /// reads it as a position in the view's own list instead. That
    /// misreading is only visible where the same index names a column
    /// there that can actually move — with one own column (the other
    /// fixture) the wrong index simply runs off the end and answers
    /// `None` for the right reason by accident.
    #[test]
    fn an_available_row_cannot_be_reordered() {
        let config = tree_with_two_members();
        let mut draft = Domain::Views.draft(&config, "tree");
        // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3, delta01=4
        draft.selected = 4; // delta01, the catalogue's only row
        assert_eq!(draft.move_item(1), None);
        assert_eq!(draft.move_item(-1), None);
        assert_eq!(
            draft
                .list_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["npv", "book"],
            "and no column of the view's own moved in its place"
        );
        let available: Vec<&str> = draft
            .available_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(available, ["delta01"], "untouched");
    }

    /// And the other half of the same rule: the view's own list is still
    /// reorderable, and its LAST column has nowhere further down to go —
    /// the catalogue painted below it is not part of the order.
    #[test]
    fn the_last_column_has_nothing_below_it_to_move_past() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = 2; // npv, the view's only column
        assert_eq!(draft.move_item(1), None);
    }

    /// §18.2: "changing the dataset empties Available and repopulates it;
    /// members that the new dataset lacks stay listed... so the
    /// diagnostic can name them." Two datasets, neither sharing a column
    /// name with the other, so the assertion cannot pass by an available
    /// row surviving the switch by coincidence.
    #[test]
    fn refresh_available_repopulates_for_the_newly_chosen_dataset() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [other.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [other.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        assert_eq!(draft.choice("dataset"), Some("risk"));
        let before: Vec<&str> = draft
            .available_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(before, ["book"], "sanity: risk's own available block");

        // `options` is sorted (`other` < `risk`), and `dataset` opens on
        // `risk` (index 1), so one forward step wraps to `other`.
        draft.selected = 0;
        assert!(draft.toggle_selected().changed());
        assert_eq!(draft.choice("dataset"), Some("other"));

        refresh_available(&mut draft, &config);
        let items = draft.list_items("columns").unwrap();
        assert!(
            items.iter().any(|i| i.name == "npv"),
            "a column the new dataset lacks stays listed, so the \
             diagnostic can name it: {items:?}"
        );
        let available: Vec<&str> = draft
            .available_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(
            available,
            ["lhu", "delta01"],
            "the available block is emptied and repopulated from the new \
             dataset alone — none of risk's own columns survive"
        );
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

    /// Review round 1, finding 3: `refresh_available` used to clamp
    /// `selected` by raw index rather than re-find the cursor's own row
    /// by identity. A dataset switch tears the available block down and
    /// rebuilds it from scratch, so a query that matched the OLD
    /// dataset's available column and not the new one's leaves the
    /// Dataset row as the only survivor — this pins the cursor there by
    /// identity (`Draft::follow`) rather than by an index that would
    /// only coincidentally still be right.
    #[test]
    fn refresh_available_preserves_the_cursor_on_the_dataset_field_by_identity() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[onedata.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [onedata.columns.atom]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [twodata.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [twodata.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[v]\ndataset = \"onedata\"\n[[v.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "v");
        // The premise the accepted "an index clamp would be equivalent
        // here" argument rests on (spec §18.6): `dataset` is `rows()`'s
        // own first entry, so whenever it matches the query at all it is
        // also the first SURVIVING row. Asserted rather than assumed —
        // an adapter that grew a field above `dataset` would silently
        // retire that equivalence, and this test would go on passing on
        // `follow`'s strength alone while the spec's claim quietly went
        // false.
        assert_eq!(draft.rows()[0], EditRow::Field(0));
        assert_eq!(draft.fields[0].key, "dataset");
        // "at" matches "Dataset" (the field label) and "atom" (onedata's
        // own available column) — and neither "npv" (the view's own), nor
        // "lhu"/"delta01" (twodata's columns, the new available block).
        draft.query = "at".to_string();
        assert_eq!(draft.selected, 0);
        assert_eq!(draft.selected_row(), Some(EditRow::Field(0)));

        // Step the `Choice` from `onedata` to `twodata` (sorted options,
        // one forward step) — the same sequence `render::maybe_refresh_
        // available`'s caller performs before calling this function.
        assert!(draft.toggle_selected().changed());
        assert_eq!(draft.choice("dataset"), Some("twodata"));
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(0)),
            "sanity: the cursor is still on the dataset field after the step"
        );

        refresh_available(&mut draft, &config);

        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(0)),
            "the cursor followed the dataset field through the rebuild"
        );
    }
}
