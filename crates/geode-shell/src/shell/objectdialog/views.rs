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
/// Read through `load_views`, not `ViewSpec::from_doc`, so the member
/// block shows the columns in the order and with the hidden/width state
/// the trader actually sees. Behind the members, the list also carries
/// every other column the chosen dataset has (§18.2), each a non-member
/// row with nothing ticked — the "available" block a new or growing view
/// has something to add from. `Draft::source` keeps the raw
/// pre-presentation table beside it, which is what a `Doc` write is
/// rendered from.
///
/// **Every item, member or available, carries the `kind` a first-time
/// `[[columns]]` write of it needs** ([`ListItem::kind`]): a member's own
/// [`ViewColumn`] variant for one already in the view, the schema role for
/// an available one ([`schema_role_kind`]). Getting this wrong is not
/// cosmetic — `ViewSpec::from_doc` defaults a missing `kind` to
/// `"measure"`, so an added dimension written without one is silently
/// summed as nothing (an `Aggregate::Sum` over a schema-Key/Dimension
/// column has no grain to aggregate at) or, worse, resolved as a real
/// measure of the same name that happens to exist. A schema role with no
/// honest `[[columns]]` kind — `key`, `attribute` — is therefore not
/// offered as available at all: seeing `schema_role_kind`'s own doc.
///
/// **Derived dimensions (`dimensions.toml`) are deliberately NOT in the
/// available block**, even though a config author can group or scope by
/// one today. `ViewSpec::from_doc` accepts exactly three `kind` strings —
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

    let mut items: Vec<ListItem> = view
        .map(|v| {
            v.columns
                .iter()
                .map(|column| {
                    let presentation = v.presentation_of(column.name());
                    ListItem {
                        name: column.name().to_string(),
                        included: !presentation.hidden.unwrap_or(false),
                        width: presentation.width,
                        member: true,
                        kind: Some(view_column_kind(column).to_string()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // The available block (§18.2): the chosen dataset's other columns,
    // each a non-member row with nothing to write until `space` promotes
    // it into the member block above. No derived dimensions here — see
    // this function's own doc for why.
    if let Some(dataset) = schema.dataset(&current) {
        push_dataset_columns(&mut items, dataset);
    }

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

/// Rebuild the `columns` list's available block after the `dataset` field
/// changes (spec §18.2): "changing the dataset empties Available and
/// repopulates it; members that the new dataset lacks stay listed, as
/// today, so the diagnostic can name them." Members are left exactly as
/// they are — untouched, in place, `kind` included — because they are
/// still the view's own columns regardless of what the new dataset has;
/// [`validate`] is what tells the trader a member the new dataset lacks
/// is now a problem. Only the block *behind* them is thrown away and
/// rebuilt from scratch, from the new dataset's columns, which is what
/// keeps the member-then-available invariant: a stale available row from
/// the old dataset never lingers to be reordered against the new one's.
///
/// Called from `render::maybe_refresh_available`, after a `Toggle`/
/// `ToggleBack` step on the `dataset` field itself — never from
/// `Draft::step_selected`, which has no `Config` to read a schema from.
pub fn refresh_available(draft: &mut Draft, config: &Config) {
    let Some(current) = draft.choice("dataset").map(str::to_string) else {
        return;
    };
    let Some(field) = draft.fields.iter_mut().find(|f| f.key == "columns") else {
        return;
    };
    let FieldKind::OrderedList { items } = &mut field.kind else {
        return;
    };
    items.retain(|i| i.member);
    let schema = config
        .doc("datasets")
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default();
    if let Some(dataset) = schema.dataset(&current) {
        push_dataset_columns(items, dataset);
    }
    // The rebuild can shrink the FILTERED list out from under the cursor
    // (an active query that matched an available column the old dataset
    // had, say), and `selected` is an index into `visible_rows()`
    // (§18.3) rather than a value this function can leave to chance.
    let last_visible = draft.visible_rows().len().saturating_sub(1);
    draft.selected = draft.selected.min(last_visible);
}

/// Append `dataset`'s columns that are not already on `items`, as
/// non-member rows each carrying the `kind` a first-time write of it
/// needs ([`schema_role_kind`]) — the available-block builder [`fields`]
/// and [`refresh_available`] share, so a dataset switch cannot populate a
/// different available block than opening the view fresh would have.
fn push_dataset_columns(items: &mut Vec<ListItem>, dataset: &DatasetSpec) {
    for column in &dataset.columns {
        if items.iter().any(|i| i.name == column.name) {
            continue;
        }
        let Some(kind) = schema_role_kind(&column.role) else {
            continue;
        };
        items.push(ListItem {
            name: column.name.clone(),
            included: false,
            width: None,
            member: false,
            kind: Some(kind.to_string()),
        });
    }
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
/// column vanishes). `None` here is why such a column is not on the
/// available block at all — see [`fields`]'s own doc.
fn schema_role_kind(role: &ColumnRole) -> Option<&'static str> {
    match role {
        ColumnRole::Dimension { .. } => Some("dimension"),
        ColumnRole::Measure { .. } => Some("measure"),
        ColumnRole::Key | ColumnRole::Attribute { .. } => None,
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
/// source's, not the draft's, because order is presentation; only the
/// member set is definitional, and that is why the columns are rebuilt
/// from the draft's names rather than copied.
fn doc_table(draft: &Draft) -> toml_edit::Table {
    let mut table = super::toml_table_to_edit(&draft.source);
    if let Some(dataset) = draft.choice("dataset") {
        table["dataset"] = toml_edit::value(dataset);
    }
    if let Some(items) = draft.list_items("columns") {
        // Only the member block defines the view — an available row the
        // trader has not ticked on is not one of its columns.
        let wanted: Vec<&ListItem> = items.iter().filter(|i| i.member).collect();
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
    // Only the member block is presented at all — an available row the
    // trader has not added is not part of the view, so it has no order, no
    // hidden state and no width to write here either.
    let items: Vec<&ListItem> = draft
        .list_items("columns")
        .unwrap_or_default()
        .iter()
        .filter(|i| i.member)
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
/// keeps a membership change honest: adding or dropping a column rewrites
/// `views.toml`'s column list, and the trader has not reordered anything
/// merely by doing so.
fn doc_baseline(draft: &Draft) -> (Vec<&str>, BTreeMap<String, f32>) {
    let wanted: Vec<&ListItem> = draft
        .list_items("columns")
        .unwrap_or_default()
        .iter()
        .filter(|i| i.member)
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
    use super::super::{Domain, Step};
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
    /// every member/available test below shares. `desk` stays in the
    /// fixture (rather than being dropped along with the rest of this
    /// module's derived-dimension handling) precisely so
    /// [`the_column_list_is_members_then_the_datasets_other_columns`] can
    /// prove it is deliberately excluded, not merely absent because
    /// nothing declared one.
    fn tree_with_two_available_columns() -> Config {
        config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            ),
            (Layer::Builtin, "dimensions", "[desk]\nfrom = \"book\"\n"),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ])
    }

    /// §18.2: the member block first, in the view's own order, then the
    /// dataset's other columns as non-members — a new or growing view has
    /// something to tick. Each item carries the `kind` a first-time write
    /// would need: `npv`'s own (already a `ViewColumn::Measure` in the
    /// view), `book`'s schema role (`dimension`), `delta01`'s (`measure`)
    /// — and `desk`, a *derived* dimension, does not appear at all
    /// (`fields`'s own doc has the reasoning).
    #[test]
    fn the_column_list_is_members_then_the_datasets_other_columns() {
        let config = tree_with_two_available_columns();
        let items = Domain::Views
            .draft(&config, "tree")
            .list_items("columns")
            .unwrap()
            .to_vec();
        let shape: Vec<(&str, bool, bool, Option<&str>)> = items
            .iter()
            .map(|i| (i.name.as_str(), i.member, i.included, i.kind.as_deref()))
            .collect();
        assert_eq!(
            shape,
            [
                ("npv", true, true, Some("measure")),
                ("book", false, false, Some("dimension")),
                ("delta01", false, false, Some("measure")),
            ]
        );
    }

    /// Hiding a member is presentation; adding an available column is
    /// definitional — the split this whole design exists to keep.
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
        assert!(
            items[1].member && items[1].included,
            "an added column joins the member block, shown"
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

    /// The item promoted above (`book`) sits immediately after the member
    /// block's last entry, so inserting at its own (post-removal) index
    /// and inserting at the member block's end land in the same place by
    /// coincidence. Promoting a LATER available row — `delta01`, with
    /// `book` still sitting between it and the member block — is what
    /// actually distinguishes the two: get it wrong, and a member ends up
    /// painted after a non-member.
    #[test]
    fn adding_a_later_available_column_still_joins_the_end_of_the_member_block() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3, delta01=4 …
        draft.selected = 4;
        assert!(draft.toggle_selected().changed());
        let items = draft.list_items("columns").unwrap();
        assert_eq!(
            items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["npv", "delta01", "book"],
            "delta01 joins right after the last member, ahead of book — \
             never behind it"
        );
        assert!(items[1].member && items[1].included);
        assert!(!items[2].member, "book is still merely available");
    }

    /// `x` removes a member outright — the definitional twin of `space`'s
    /// hide — and it lands at the end of the available block, not merely
    /// wherever it happened to sit.
    #[test]
    fn x_removes_a_member_and_moves_it_to_the_available_block() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = 2; // npv
        assert!(draft.remove_selected().changed());
        let items = draft.list_items("columns").unwrap();
        assert!(items.iter().all(|i| !i.member));
        assert_eq!(items.last().unwrap().name, "npv");
        assert!(
            draft
                .writes_by_destination()
                .contains_key(&Destination::Doc)
        );
    }

    /// `x` on a row that is not a member yet has nothing to remove, and
    /// says so with the verb that actually adds it — the second of
    /// `remove_selected`'s two distinguishable refusals (the first,
    /// `"space unticks here"`, is Groupings' own —
    /// `remove_selected_refuses_where_membership_is_inclusion` in
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

    /// `shift+j`/`shift+k` never carry an item across the member/available
    /// boundary — an available column reordered among the members would be
    /// painted in one block while written in neither.
    #[test]
    fn reordering_never_crosses_the_member_boundary() {
        let config = tree_with_two_available_columns();
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = 2; // npv, the only member
        assert!(
            draft.move_item(1).is_none(),
            "book is not a member; npv cannot move past it"
        );
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
            .list_items("columns")
            .unwrap()
            .iter()
            .filter(|i| !i.member)
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
            items.iter().any(|i| i.name == "npv" && i.member),
            "a member the new dataset lacks stays listed, so the \
             diagnostic can name it: {items:?}"
        );
        let available: Vec<&str> = items
            .iter()
            .filter(|i| !i.member)
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
}
