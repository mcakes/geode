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
use geode_core::view::{
    Colour, ColumnFormat, ColumnPresentation, DATASET_PRESENTATION_DOC, DatasetPresentationSpec,
    Negative, Scale, ViewColumn, ViewSpec,
};

use super::{
    ColumnContext, ColumnDoor, ColumnLayers, Destination, Draft, Field, FieldKind, ListItem,
};

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
                        presentation,
                        kind: Some(view_column_kind(column).to_string()),
                        note: None,
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
    let overlay = dataset_overlay(config, &current);
    let available = Some(match schema.dataset(&current) {
        Some(dataset) => dataset_catalogue(&items, dataset, &overlay),
        None => Vec::new(),
    });

    vec![
        Field {
            key: "dataset".to_string(),
            label: "Dataset".to_string(),
            kind: FieldKind::Choice { options, selected },
            dest: Destination::Doc,
            layer: None,
        },
        Field {
            key: "columns".to_string(),
            label: "Columns".to_string(),
            kind: FieldKind::OrderedList { items, available },
            dest: Destination::Presentation,
            layer: None,
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
    let overlay = dataset_overlay(config, &current);
    let rebuilt = match schema.dataset(&current) {
        Some(dataset) => dataset_catalogue(items, dataset, &overlay),
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
///
/// **Each row is seeded with the column's DATASET-level presentation**
/// (`overlay`, this dataset's entries from `dataset_presentation.toml`),
/// not the unset one, because `space` on a catalogue row hands the whole
/// [`ListItem`] to the view's own list ([`Draft::step_selected`]'s
/// `Available` arm moves it verbatim) and nothing re-derives it
/// afterwards. Seeded unset, a column promoted and opened in the same
/// draft lifetime paints seven fields at the KIND default while
/// [`baseline_below`] — refreshed from the config — holds the trader's
/// own dataset-level values: the first keystroke folds all seven and
/// the writer emits every one of them as a view-level override
/// contradicting the dataset level the trader set themselves.
///
/// A lookup by this dataset's name IS
/// [`DatasetPresentationSpec::owner_of`]'s answer for these columns,
/// since a catalogue is by construction the columns of exactly one
/// dataset — the one whose schema declares them, which is what
/// `owner_of` resolves to. `hidden` is never among the keys: the
/// dataset overlay's reader refuses it outright (spec §2.1), membership
/// and sequence belonging to a view.
fn dataset_catalogue(
    items: &[ListItem],
    dataset: &DatasetSpec,
    overlay: &BTreeMap<String, ColumnPresentation>,
) -> Vec<ListItem> {
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
            presentation: overlay.get(&column.name).cloned().unwrap_or_default(),
            kind: Some(kind.to_string()),
            note: None,
        });
    }
    available
}

/// One dataset's own entries from `dataset_presentation.toml`, by column
/// — [`dataset_layer`]'s per-dataset half, for the one caller that has a
/// dataset name rather than a [`ViewSpec`] to resolve owners through
/// ([`dataset_catalogue`], whose columns all belong to that dataset by
/// construction). Empty when the doc is absent or says nothing about it.
fn dataset_overlay(config: &Config, dataset: &str) -> BTreeMap<String, ColumnPresentation> {
    config
        .doc(DATASET_PRESENTATION_DOC)
        .map(|doc| DatasetPresentationSpec::from_doc(doc).0)
        .and_then(|spec| spec.datasets.get(dataset).cloned())
        .unwrap_or_default()
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
pub(super) fn schema_role_kind(role: &ColumnRole) -> Option<&'static str> {
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
        // The dataset overlay is the Schema door's destination alone
        // (dataset-presentation spec §4.1); no Views field carries it,
        // so this arm exists only to keep the match exhaustive.
        Destination::DatasetPresentation => {
            unreachable!("Views has no DatasetPresentation-destined fields")
        }
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

/// The trader's personal view of the view, for `view_presentation.toml`
/// (Part 2c §4.3): the column order, and one `[columns.<name>]` table per
/// column whose presentation the trader has actually changed.
///
/// Rendered fresh from the draft rather than merged into whatever is on
/// disk, because this table is replaced whole (see [`to_table`]). A
/// column with nothing to say gets no table at all — a file that says
/// nothing is easier to hand-edit than one full of empty `[...]`
/// headers, and it is also what keeps a save from ever writing the
/// LEGACY `hidden`/`width` spelling this function superseded: every key
/// this writes lands inside `[columns.<name>]`, never a top-level
/// `hidden` array or `width` table.
///
/// **Only what the trader actually changed is written.** `order` and
/// every per-column key arrive here off the *effective* presentation
/// (`ListItem::presentation`), which already carries whatever
/// `views.toml` declared — so writing every column's position and every
/// declared format key back would pin the desk's layout for this trader
/// against the desk's later changes. That is the same freeze
/// [`Destination`] exists to prevent, one field-granularity down, and it
/// would fire on the commonest edit there is: hiding one column would
/// silently adopt the desk's order and every other column's format
/// forever. Every key is therefore compared, one at a time, against
/// [`baseline_below`] — the layers this file sits ON: what the SAME save
/// leaves in `views.toml`, with the trader's own dataset level merged
/// over it (dataset-presentation spec §5.1) — and omitted when it still
/// matches. Reading the desk alone here would copy every dataset-level
/// key into this view's overlay as a spurious per-view override, which
/// is the same freeze one layer down. The five `ColumnFormat` keys compare
/// RESOLVED (each side through [`kind_default`], §4.3's own definition of
/// the baseline), which is what keeps a key the desk never declared and
/// the trader never moved out of the file; the loop body has the full
/// reasoning and the state that reaches it. `hidden` needs no such
/// comparison:
/// nothing but this file can ever set it, so a column is either
/// unhidden (the universal desk default) or explicitly hidden here.
///
/// Each of the seven comparisons below is deliberately a nested `if <the
/// two sides differ> { if let Some(v) = item.presentation.<key> { … } }`
/// rather than clippy's preferred `if … && let Some(v) = … {}`
/// single-line collapse: the mutation harness anchors two entries on an
/// OUTER condition's own line, and collapsing the two would fold that
/// line into a different one the moment a sibling key's comparison
/// changed — `#[allow]` below, not the suggested rewrite. The five
/// `ColumnFormat` keys spell that outer condition `effective.<key> !=
/// below_format.<key>`, both sides resolved through [`kind_default`];
/// `label` and `width` spell it `item.presentation.<key> != below.<key>`,
/// having no kind default to resolve against.
///
/// **An item key `None` where the desk has `Some` still cannot arise
/// here, and Part 2c is why that is now a guarantee rather than an
/// accident.** The item is seeded from the *merged* presentation
/// (`ListItem::presentation`'s own doc), so a key the desk sets is never
/// `None` on the item unless something cleared it — and the column stage
/// **is** that clear verb: an empty `label`, an `auto` width. What makes
/// it safe is that a clear here means *stop overriding*, never *delete*
/// — this overlay cannot remove a key `views.toml` sets — so
/// [`fold_into`] resolves a cleared key to the DESK's own value and the
/// comparison below simply finds them equal and writes nothing. Writing
/// a literal `None` instead would omit the key and read back as "nothing
/// to say", which is true only when the desk says nothing either; where
/// the desk sets a label, it would swallow the clear whole.
/// **This writer fully owns the overlay's `[view.columns.*]` block**
/// (the final review's M-7). Every other adapter's `to_table` starts
/// from `draft.source` and preserves what it does not model; this one is
/// built from `items` instead, so anything a trader hand-wrote under a
/// column's table that the vocabulary does not model is dropped on the
/// next save. That matches the reader, which already drops unrecognised
/// keys there, and the pre-2c behaviour of `order`/`hidden`/`width` —
/// this doc is the statement of it, not a change to it.
#[allow(clippy::collapsible_if)]
fn presentation_table(draft: &Draft) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    // Only the view's own columns are presented at all — a column still
    // in the available catalogue is not part of the view, so it has no
    // order and no format to write here either.
    let items: Vec<&ListItem> = draft
        .list_items("columns")
        .unwrap_or_default()
        .iter()
        .collect();
    let order = doc_order(draft);
    let baseline = baseline_below(draft);

    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    if names != order {
        let mut ord = toml_edit::Array::new();
        for name in &names {
            ord.push(*name);
        }
        table["order"] = toml_edit::value(ord);
    }

    let mut columns = toml_edit::Table::new();
    for item in &items {
        let below = baseline
            .get(item.name.as_str())
            .cloned()
            .unwrap_or_default();
        // §4.3's baseline is "the kind's default with the layers BELOW
        // this one applied" — the desk view's own format and label, then
        // the trader's dataset level over it (dataset-presentation §5.1)
        // — RESOLVED, not those layers' raw `Option`s, so the five
        // format keys are compared through `ColumnFormat`
        // and only `label`/`width`, which have no kind default to
        // resolve against, compare as bare `Option`s.
        //
        // Resolving matters exactly where the two sides say the same
        // thing in different words: a key the desk leaves unset and the
        // item carries at the kind's own default. `views::fold_into`
        // (Part 2c §5.2) produces that state on every keystroke in the
        // column stage — it writes a `Some` for all five keys, since
        // every field there was seeded with the value in force — and a
        // raw-`Option` comparison called all five "different from the
        // desk" and copied them into the trader's overlay. That is the
        // freeze §4.3 names in so many words: "a trader who changes a
        // precision does not copy the desk's scale into their file and
        // freeze it against the desk's next change". The dataset level
        // reaches the same trap from the other side: a scale the trader
        // set once for the whole dataset would be copied into every view
        // they touch a precision in, and their next dataset-level change
        // would then reach every view BUT those.
        let kind = kind_default(item);
        let below_format = kind.clone().with(&below);
        let effective = kind.with(&item.presentation);
        let mut t = toml_edit::Table::new();

        if effective.precision != below_format.precision {
            if let Some(v) = item.presentation.precision {
                t["precision"] = toml_edit::value(i64::from(v));
            }
        }
        if effective.thousands != below_format.thousands {
            if let Some(v) = item.presentation.thousands {
                t["thousands"] = toml_edit::value(v);
            }
        }
        if effective.negative != below_format.negative {
            if let Some(v) = item.presentation.negative {
                t["negative"] = toml_edit::value(negative_key(v));
            }
        }
        if effective.colour != below_format.colour {
            if let Some(v) = &item.presentation.colour {
                t["colour"] = toml_edit::value(colour_key(v));
            }
        }
        if effective.scale != below_format.scale {
            if let Some(v) = item.presentation.scale {
                t["scale"] = toml_edit::value(scale_key(v));
            }
        }
        if item.presentation.label != below.label {
            if let Some(v) = &item.presentation.label {
                t["label"] = toml_edit::value(v.as_str());
            }
        }
        if item.presentation.width != below.width {
            if let Some(v) = item.presentation.width {
                t["width"] = width_value(v);
            }
        }
        if !item.included {
            t["hidden"] = toml_edit::value(true);
        }

        if !t.is_empty() {
            columns[item.name.as_str()] = toml_edit::Item::Table(t);
        }
    }
    if !columns.is_empty() {
        table["columns"] = toml_edit::Item::Table(columns);
    }

    table
}

/// The column order **the view's own doc will hold after this same
/// save** — read back out of [`columns_for`] rather than off
/// `draft.source` directly, so the two halves of one save cannot
/// disagree about what the doc says.
///
/// This is the "pre-presentation view" [`presentation_table`] compares
/// against. Using the post-write doc rather than the raw source is what
/// keeps a change to the column set honest: adding or dropping one
/// rewrites `views.toml`'s column list, and the trader has not reordered
/// anything merely by doing so.
fn doc_order(draft: &Draft) -> Vec<&str> {
    let wanted: Vec<&ListItem> = draft
        .list_items("columns")
        .unwrap_or_default()
        .iter()
        .collect();
    let mut order = Vec::with_capacity(wanted.len());
    for column in columns_for(&draft.source, &wanted).iter() {
        let Some(name) = column.get("name").and_then(|item| item.as_str()) else {
            continue;
        };
        // The doc's own name is borrowed from the draft's list rather than
        // from the rendered table, which is a temporary.
        let Some(item) = wanted.iter().find(|w| w.name == name) else {
            continue;
        };
        order.push(item.name.as_str());
    }
    order
}

/// Each column's presentation **as the view's own doc already declares
/// it** — one column, one `format` sub-table plus `label`/`width`, read
/// directly off `draft.source`'s own `[[columns]]` tables (never through
/// [`columns_for`]'s `toml_edit` rendering: a bare `toml_edit::Table`
/// with no document to hang a header path off loses a nested table
/// entirely when printed — `object_text`'s own doc comment has the same
/// warning about `Table::to_string` — so a column's `format` sub-table
/// would silently vanish on the very round trip meant to read it back).
/// `draft.source` is already a plain `toml::Table`, which is exactly the
/// shape [`ColumnPresentation`]'s readers want, so no conversion is
/// needed at all.
///
/// A column `columns_for` would synthesise fresh (one `wanted` names but
/// `draft.source` does not yet have — a column just promoted out of the
/// available catalogue) has no entry here, and [`presentation_table`]'s
/// `.unwrap_or_default()` treats that exactly as "no desk baseline yet",
/// which is correct: nothing has published a presentation for a column
/// that does not exist in `views.toml` yet either.
///
/// `read_hidden: false`, always: `hidden` is never a view's own key —
/// only the overlay this function's caller writes ever sets it — so a
/// column's baseline `hidden` is always `None`, which is exactly why
/// [`presentation_table`] compares `included` against the desk's default
/// (unhidden) directly rather than against a baseline field for it. The
/// `warn` callback is a no-op: a column's table here already passed
/// through the loader once as `views.toml` itself, so a key it could not
/// parse would already have been reported there — reporting it again
/// while rendering a save would be noise about the SAME problem from a
/// second place.
pub(super) fn desk_baseline(draft: &Draft) -> BTreeMap<String, ColumnPresentation> {
    let noop_warn = |_: &str, _: String| {};
    let mut baseline = BTreeMap::new();
    let source_columns: Vec<&toml::Table> = draft
        .source
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_table()).collect())
        .unwrap_or_default();
    for column in source_columns {
        let Some(name) = column.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut presentation = ColumnPresentation::default();
        if let Some(format) = column.get("format").and_then(|v| v.as_table()) {
            presentation.parse_format_keys(format, &noop_warn);
        }
        presentation.parse_column_keys(column, false, &noop_warn);
        baseline.insert(name.to_string(), presentation);
    }
    baseline
}

/// The dataset-level entries this view's columns resolve to, by column
/// (dataset-presentation spec §5.1): the `dataset_presentation` doc's
/// tables for whichever dataset OWNS each column
/// ([`DatasetPresentationSpec::owner_of`] — the view's own dataset
/// first, then each join's in file order, the order the compiler
/// resolves names in), read from the config the CALLER hands in.
///
/// Every path that reaches a stage hands in the pending-aware config
/// (`apply::config_with_pending`), never `services.config` alone: inside
/// the 250 ms write debounce the latter is the doc as it stood before
/// the last keystroke, so a layer read from it would be one tick stale
/// and the stage would both hide that tick and, outliving the flush,
/// write the column back without it.
///
/// **The candidates are every column the view's datasets declare, not
/// only the ones it carries today.** A column PROMOTED out of the
/// available catalogue joins the view's own list mid-draft
/// ([`Draft::step_selected`]'s `Available` arm), and
/// [`presentation_table`] compares it against [`baseline_below`] — this
/// map — on that very keystroke, before any reload could widen it.
/// Keyed off `view.columns` alone, that comparison found the dataset's
/// own value on the promoted item and nothing under it, and wrote the
/// trader's dataset-level opinion back as a view-level override on the
/// keystroke that added the column.
///
/// A column no dataset of the view declares — a derived column — takes
/// nothing from this layer, and so has no entry here at all; neither
/// does one whose owner has no table for it, which is why the owner is
/// resolved first and the overlay consulted second rather than the
/// overlay simply being copied per dataset (a dataset that declares the
/// column but says nothing about it still OWNS it, and a later join's
/// table for the same name must not stand in).
pub(super) fn dataset_layer(
    config: &Config,
    view: &ViewSpec,
) -> BTreeMap<String, ColumnPresentation> {
    let Some(doc) = config.doc(DATASET_PRESENTATION_DOC) else {
        return BTreeMap::new();
    };
    let (overlay, _) = DatasetPresentationSpec::from_doc(doc);
    let schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    let mut candidates: Vec<&str> = std::iter::once(view.dataset.as_str())
        .chain(view.joins.iter().map(|j| j.dataset.as_str()))
        .filter_map(|ds| schema.dataset(ds))
        .flat_map(|spec| spec.columns.iter().map(|c| c.name.as_str()))
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
        .into_iter()
        .filter_map(|name| {
            let owner = DatasetPresentationSpec::owner_of(view, name, &schema)?;
            let p = overlay.datasets.get(owner)?.get(name)?.clone();
            Some((name.to_string(), p))
        })
        .collect()
}

/// [`dataset_layer`] for a view named rather than held: the views are
/// reloaded from the same config ([`load_views`], which every other
/// reader here goes through too) and the named one looked up. An unknown
/// name is an empty layer, not a panic — a draft can outlive the object
/// it describes for exactly as long as one keystroke.
///
/// This is what [`super::Domain::draft`] and `render::enter_column_stage`
/// call, because both know a name and neither is holding a [`ViewSpec`].
pub(super) fn dataset_layer_for(
    config: &Config,
    view_name: &str,
) -> BTreeMap<String, ColumnPresentation> {
    let (views, _) = load_views(config);
    views
        .iter()
        .find(|v| v.name == view_name)
        .map(|view| dataset_layer(config, view))
        .unwrap_or_default()
}

/// The layer a VIEW-level key sits over (§5.1): the desk's own keys
/// ([`desk_baseline`]) with the trader's dataset level merged over,
/// per column.
///
/// **Both the fold and the writer read this**, which is the whole point.
/// Compared against the desk ALONE, every key the trader set at the
/// dataset level would read as a divergence from the view's baseline and
/// be copied into `view_presentation.toml` as a spurious per-view
/// override — pinning this view against their own next dataset-level
/// change, which is the same freeze [`Destination`] exists to prevent,
/// one layer down. Equal to the layer below means "I have no opinion
/// here", and the writer emits nothing.
///
/// The dataset layer travels on the draft ([`Draft::dataset_layer`])
/// rather than being read here, because `to_table` — the writer's own
/// entry point — has no `Config` to read it from.
pub(super) fn baseline_below(draft: &Draft) -> BTreeMap<String, ColumnPresentation> {
    let mut below = desk_baseline(draft);
    for (col, dataset) in &draft.dataset_layer {
        below.entry(col.clone()).or_default().merge_over(dataset);
    }
    below
}

/// The two layers below the view overlay under one column (§5.3), for
/// the provenance chip and the fold notice — each as the keys that layer
/// ITSELF sets, never merged, so a layer can be named.
///
/// The view overlay is NOT read here. The fields' own values are
/// compared against `below_view()` at paint
/// (`dataset_columns::provenance_of`), which is what makes a
/// just-stepped field read `view` on the same frame rather than 250 ms
/// later when the write lands — and what makes a field stepped BACK
/// read the layer below on the same frame, which a captured overlay
/// could not (the final whole-branch review's named risk 4).
///
/// [`ColumnLayers::below_view`]: super::ColumnLayers::below_view
pub(super) fn column_layers(draft: &Draft, column: &str) -> ColumnLayers {
    let desk = desk_baseline(draft).remove(column).unwrap_or_default();
    let dataset = draft.dataset_layer.get(column).cloned().unwrap_or_default();
    ColumnLayers { desk, dataset }
}

/// The Views door's [`ColumnContext`], whole — the one place it is built.
///
/// Three callers had a copy of this literal (the dialog's own
/// `render::enter_column_stage` and two test openers that exist to
/// mirror it), and a context is exactly the kind of value where a test
/// that drifts from the door stops testing the door: a missing layer
/// there is a fold that silently names the wrong one, with every
/// assertion still green. `overlay_object` is empty and `item` is
/// `Some`, both by the door's own definition — the view's list holds the
/// item this stage folds into, so the scratch copy here is read only for
/// the column's kind default.
pub(super) fn column_context(draft: &Draft, column: &str, item: ListItem) -> ColumnContext {
    ColumnContext {
        door: ColumnDoor::View,
        layers: column_layers(draft, column),
        overlay_object: toml::Table::new(),
        item: Some(item),
    }
}

/// `negative`'s two written spellings — the same two [`ColumnPresentation
/// ::parse_format_keys`] reads back.
pub(super) fn negative_key(n: Negative) -> &'static str {
    match n {
        Negative::Minus => "minus",
        Negative::Parens => "parens",
    }
}

/// `scale`'s three written spellings — the same three the reader accepts.
pub(super) fn scale_key(s: Scale) -> &'static str {
    match s {
        Scale::None => "none",
        Scale::Thousands => "k",
        Scale::Millions => "M",
    }
}

/// `colour`'s written spelling: the two built-ins, or a name into
/// `colours.toml` verbatim.
pub(super) fn colour_key(c: &Colour) -> String {
    match c {
        Colour::None => "none".to_string(),
        Colour::Sign => "sign".to_string(),
        Colour::Named(name) => name.clone(),
    }
}

/// A whole-number width is written as an integer (`width = 140`), never
/// `140.0` — the common case by far, and the plain integer is what a
/// trader hand-editing the file would type.
pub(super) fn width_value(width: f32) -> toml_edit::Item {
    if width.fract() == 0.0 {
        toml_edit::value(width as i64)
    } else {
        toml_edit::value(f64::from(width))
    }
}

/// The default format a column's kind implies before any presentation is
/// applied — [`ColumnFormat::TEXT`] for a dimension, [`ColumnFormat::
/// MEASURE`] for everything else (a measure or a derived column).
///
/// Three readers, and the third is why this is more than a display
/// nicety: [`column_summary`], so a member row states only what the
/// trader has actually overridden; [`column_fields`], which seeds the
/// column stage's rows with the value in FORCE rather than a raw
/// `Option`; and [`presentation_table`], where it is half of the desk
/// baseline (§4.3: "the kind's default with the desk's `format` and
/// `label` applied"). So what `view_presentation.toml` ends up
/// containing depends on this function — a column whose `kind` is read
/// wrong here does not merely paint a wrong summary, it writes keys the
/// trader never set, or omits ones they did.
pub fn kind_default(item: &ListItem) -> ColumnFormat {
    if item.kind.as_deref() == Some("dimension") {
        ColumnFormat::TEXT
    } else {
        ColumnFormat::MEASURE
    }
}

/// The compact summary painted after a member row's name (Part 2c §5.4):
/// only the presentation keys that differ from `kind_default`, joined by
/// " · ", or the empty string when the column carries no override at
/// all — the common case, so most rows paint nothing here.
///
/// `width` and `label` are read off `p` directly rather than off the
/// resolved format, because neither has a "default" a `ColumnFormat`
/// could compare against — a width is either declared or it isn't, and
/// the same is true of a label. Every other part reads the RESOLVED
/// value (`kind_default.with(p)`) against the kind default, not `p`
/// directly, because `p` being `None` on a key still has a real,
/// meaningful value once the kind default fills it in — precision 0 is
/// not "no precision", it is the dimension default, and there is nothing
/// to call out about it.
pub fn column_summary(kind_default: &ColumnFormat, p: &ColumnPresentation) -> String {
    let effective = kind_default.clone().with(p);
    let mut parts: Vec<String> = Vec::new();
    if let Some(width) = p.width {
        parts.push(format!("{width:.0} px"));
    }
    match effective.scale {
        Scale::None => {}
        Scale::Thousands => parts.push("k".to_string()),
        Scale::Millions => parts.push("M".to_string()),
    }
    if effective.precision != kind_default.precision {
        parts.push(format!("{} dp", effective.precision));
    }
    if effective.negative != Negative::Minus {
        parts.push("parens".to_string());
    }
    if effective.thousands != kind_default.thousands {
        if effective.thousands {
            parts.push("thousands".to_string());
        } else {
            parts.push("no thousands".to_string());
        }
    }
    if effective.colour != kind_default.colour {
        parts.push(match &effective.colour {
            Colour::None => "none".to_string(),
            Colour::Sign => "sign".to_string(),
            Colour::Named(name) => name.clone(),
        });
    }
    if let Some(label) = &p.label {
        parts.push(format!("→ {label}"));
    }
    parts.join(" · ")
}

/// The seven keys of the column stage (Part 2c §5.3), in the order the
/// stage paints them — which is also the order [`column_fields`] builds
/// them in and the order a reader of `view_presentation.toml` meets
/// them.
///
/// A constant rather than seven literals spread across the builder, the
/// fold and the tests, because the fold matches on these strings: a key
/// renamed in one place and not the other would silently stop folding
/// that field, and a silent stop is the failure this whole design
/// exists to remove.
pub const COLUMN_KEYS: [&str; 7] = [
    "label",
    "width",
    "scale",
    "precision",
    "thousands",
    "negative",
    "colour",
];

/// The value `width` takes when the column has none of its own — a real
/// value in the field rather than an empty string, because "no width" is
/// something a trader chooses (the column takes its kind's default
/// width — `geode-blotter`'s plan, not a measurement) and an empty text
/// box would read as an unset field they had failed to fill in.
pub(super) const AUTO: &str = "auto";

/// The largest width the stage will accept, and the smallest. A column
/// narrower than `MIN_WIDTH` cannot show a header glyph and a column
/// wider than `MAX_WIDTH` is wider than any window this shell opens, so
/// both are refused (§5.3) rather than written and silently clamped by
/// the table.
const MIN_WIDTH: i64 = 20;
const MAX_WIDTH: i64 = 2000;

/// One column's presentation as the seven fields the column stage paints
/// (Part 2c §5.3).
///
/// **Every field carries `dest`**, which is the whole reason this stage
/// never forks: there is no key here that could fork the desk's view, so
/// `commit_change` never announces one from inside the stage — whichever
/// overlay the door writes. The Views
/// door passes [`Destination::Presentation`], the Schema door
/// [`Destination::DatasetPresentation`] (dataset-presentation spec
/// §4.1); the seven rows are otherwise identical, which is why one
/// builder serves both.
///
/// Each field is seeded with the **effective** value — the kind default
/// (`kind_default`) with the column's own presentation over it — not with
/// the raw `Option` the overlay holds. A `Choice` seeded from a `None`
/// would have to show some placeholder for "unset", and stepping it
/// would then write whatever option happened to sit beside the
/// placeholder; seeding the value in force means every step moves from
/// what the trader is looking at. What that costs is that the fold
/// writes a `Some` for every key ([`fold_into`]'s own doc), which
/// [`presentation_table`] then compares against the desk before writing
/// anything.
///
/// `colours` is the `colours` doc's own names, sorted, as the caller read
/// them (`render::enter_column_stage`). The two built-in spellings lead;
/// a colour named on the column but absent from the doc is appended
/// rather than sorted in, so the `Choice` can always represent the value
/// it is showing (Views' "keep the object's own value" rule, the same one
/// [`fields`] keeps for a dataset the schema has dropped) while still
/// reading as the odd name out rather than as one the doc defines.
pub fn column_fields(item: &ListItem, colours: &[String], dest: Destination) -> Vec<Field> {
    let p = &item.presentation;
    let effective = kind_default(item).with(p);

    let mut colour_options: Vec<String> = geode_core::colour::RESERVED_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    colour_options.extend(
        colours
            .iter()
            .filter(|name| !geode_core::colour::RESERVED_NAMES.contains(&name.as_str()))
            .cloned(),
    );
    let current_colour = colour_key(&effective.colour);
    if !colour_options.contains(&current_colour) {
        colour_options.push(current_colour.clone());
    }

    let fields = vec![
        text_row("label", "Label", p.label.clone().unwrap_or_default(), dest),
        text_row("width", "Width", width_text(p.width), dest),
        choice_row(
            "scale",
            "Scale",
            scale_keys(),
            scale_key(effective.scale),
            dest,
        ),
        Field {
            key: "precision".to_string(),
            label: "Precision".to_string(),
            kind: FieldKind::Number {
                value: i64::from(effective.precision),
                min: 0,
                max: 12,
                step: 1,
                wrap: false,
            },
            dest,
            layer: None,
        },
        Field {
            key: "thousands".to_string(),
            label: "Thousands".to_string(),
            kind: FieldKind::Bool(effective.thousands),
            dest,
            layer: None,
        },
        choice_row(
            "negative",
            "Negative",
            negative_keys(),
            negative_key(effective.negative),
            dest,
        ),
        choice_row("colour", "Colour", colour_options, current_colour, dest),
    ];
    // [`COLUMN_KEYS`] is the statement of record for what this stage
    // edits and in what order; the literal above is what a reader
    // actually sees. Checked rather than derived because deriving the
    // fields FROM the constant would mean one builder with seven
    // branches, which is harder to read than seven rows — and this runs
    // in every test and every debug build, which is where a drift would
    // be introduced.
    debug_assert!(
        fields.iter().map(|f| f.key.as_str()).eq(COLUMN_KEYS),
        "the column stage's fields must be COLUMN_KEYS, in order"
    );
    fields
}

/// The width a column carries, as the field shows it: a bare pixel count,
/// or [`AUTO`].
///
/// A whole number prints without a fraction — the common case, and the
/// only one [`parse_text`] lets a trader type. A width that is NOT whole
/// (a hand-edited file, or a future header drag) prints in full rather
/// than truncated, so the field never tells the trader a different number
/// than the file holds; [`fold_into`] parses it back as a float, so
/// merely opening the stage on such a column and changing something else
/// leaves the odd width exactly as it was.
pub(super) fn width_text(width: Option<f32>) -> String {
    match width {
        None => AUTO.to_string(),
        Some(w) if w.fract() == 0.0 => format!("{}", w as i64),
        Some(w) => format!("{w}"),
    }
}

pub(super) fn text_row(key: &str, label: &str, value: String, dest: Destination) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Text(value),
        dest,
        layer: None,
    }
}

/// A `Choice` over `options`, selected at `current` — which is always one
/// of them by construction at every call site above (each `current` is
/// spelled by the same `*_key` function that spelled the options), so the
/// `unwrap_or(0)` is a fallback that cannot fire rather than a silent
/// reset.
pub(super) fn choice_row(
    key: &str,
    label: &str,
    options: Vec<String>,
    current: impl AsRef<str>,
    dest: Destination,
) -> Field {
    let selected = options
        .iter()
        .position(|o| o == current.as_ref())
        .unwrap_or(0);
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Choice { options, selected },
        dest,
        layer: None,
    }
}

fn scale_keys() -> Vec<String> {
    ["none", "k", "M"].iter().map(|s| s.to_string()).collect()
}

fn negative_keys() -> Vec<String> {
    ["minus", "parens"].iter().map(|s| s.to_string()).collect()
}

/// The column stage's fields written back onto the item the view's own
/// list holds (Part 2c §5.2) — the fold that makes the stage a
/// projection rather than a second copy of the data.
///
/// Called on every changed value, from `render::revalidate`, BEFORE the
/// validator and the write path read the list: both render from
/// `ListItem::presentation`, so a fold that ran later would validate and
/// write the keystroke before last.
///
/// **Every key becomes `Some`**, because [`column_fields`] seeded each
/// field with the value in force and a field therefore always has one to
/// give back. What keeps that from freezing the layers below into the
/// trader's overlay is [`presentation_table`], which compares each key
/// against [`baseline_below`] — the desk view's own keys with the
/// trader's dataset level merged over — and omits the ones that still
/// match: the one place that decision is made for the whole file.
///
/// **The two `Text` keys are the clear verb, and `below` is what makes
/// the clear honest.** An empty `label` and an [`AUTO`] `width` are how a
/// trader says "I have nothing to say about this" — and what that means
/// is *stop overriding*, never *delete*: this overlay cannot remove a key
/// the layers under it set, it can only decline to override them. So a
/// cleared key resolves to the value BELOW — `None` where nothing down
/// there sets it, the dataset level's `label`/`width` where that sets
/// one, the desk view's own where only it does — and
/// [`presentation_table`] then sees equality and writes no key at all,
/// which is exactly "this trader has no opinion here".
///
/// `below` is [`super::ColumnLayers::below_view`] on the stage path, the
/// same merge [`baseline_below`] performs per column for the writer: the
/// fold and the writer must measure a clear against the SAME thing, or
/// one decides a key is unchanged while the other writes it.
///
/// Writing a literal `None` instead is the bug this signature exists to
/// prevent, and it is silent in the worst way: the writer's `if key !=
/// below.key { if let Some(v) = key { … } }` omits the key, the file
/// reads back as "nothing to say", the layer below returns on the next
/// rebuild, and the trader's clear has vanished with nothing said about
/// it. The caller re-seeds the two fields from the item after the fold
/// ([`super::Draft::fold_column`]), so that value is on screen coming
/// back on the same keystroke rather than a blank that lies.
///
/// Returns the key the trader CLEARED this fold — the caller decides
/// whether that is worth a notice, from what it fell to. Named whether
/// or not `below` sets it (dataset-presentation spec §5.2): the caller
/// holds the layers SEPARATELY ([`super::ColumnLayers`]) and is the only
/// one that can say which of them a cleared key landed on, so deciding
/// silence here from the merged value would hide a key that fell to the
/// dataset level.
///
/// **What "cleared" is measured against is the ITEM, not the baseline.**
/// A key is cleared when the field is empty (or [`AUTO`]) and the item
/// still holds a value for it — which is exactly "this keystroke emptied
/// it", since [`column_fields`] seeds both `Text`s from the item's own
/// merged presentation. Measuring against `below` instead would report a
/// clear on every fold of a column neither the field nor the item has a
/// value for: the `width` arm runs after the `label` arm, so a genuinely
/// cleared label would be overwritten by a `width` nobody touched, and
/// the trader's notice would be about the wrong key or missing entirely.
/// It is also what keeps at most one key cleared per fold — one
/// keystroke types one field, so one key at a time can make that
/// transition.
///
/// An unparseable width leaves the key untouched — `parse_text` refuses
/// one on the way in, so the only way to reach this is a seed this file
/// wrote, and silently clearing a width nobody asked to clear would be
/// worse than ignoring a value that cannot arise.
pub fn fold_into(
    item: &mut ListItem,
    fields: &[Field],
    below: &ColumnPresentation,
) -> Option<&'static str> {
    let mut cleared = None;
    for field in fields {
        match (field.key.as_str(), &field.kind) {
            ("label", FieldKind::Text(text)) => {
                let text = text.trim();
                item.presentation.label = if text.is_empty() {
                    if item.presentation.label.is_some() {
                        cleared = Some("label");
                    }
                    below.label.clone()
                } else {
                    Some(text.to_string())
                };
            }
            ("width", FieldKind::Text(text)) => {
                let text = text.trim();
                if text == AUTO {
                    if item.presentation.width.is_some() {
                        cleared = Some("width");
                    }
                    item.presentation.width = below.width;
                } else if let Ok(width) = text.parse::<f32>() {
                    item.presentation.width = Some(width);
                }
            }
            ("scale", FieldKind::Choice { options, selected }) => {
                if let Some(scale) = options.get(*selected).and_then(|key| scale_from_key(key)) {
                    item.presentation.scale = Some(scale);
                }
            }
            ("precision", FieldKind::Number { value, .. }) => {
                if let Ok(precision) = u8::try_from(*value) {
                    item.presentation.precision = Some(precision);
                }
            }
            ("thousands", FieldKind::Bool(value)) => item.presentation.thousands = Some(*value),
            ("negative", FieldKind::Choice { options, selected }) => {
                if let Some(negative) = options
                    .get(*selected)
                    .and_then(|key| negative_from_key(key))
                {
                    item.presentation.negative = Some(negative);
                }
            }
            ("colour", FieldKind::Choice { options, selected }) => {
                if let Some(colour) = options.get(*selected).map(|key| colour_from_key(key)) {
                    item.presentation.colour = Some(colour);
                }
            }
            // A key this fold does not know, or a field whose kind is not
            // the one that key is built with: left alone rather than
            // guessed at. Unreachable through `column_fields`, which is
            // the only builder of these fields.
            _ => {}
        }
    }
    cleared
}

/// May `i` open a value field on the Views row keyed `key` (§5.3)?
///
/// The column stage's two `Text` rows and nothing else: a view's own
/// `dataset` is a `Choice` and its `columns` an `OrderedList`, so no key
/// outside the stage can answer `true` here even in principle. That is
/// why this is not gated on the stage being open — there is no row for it
/// to wrongly enable. Whether the `i` chip is PAINTED is a narrower
/// question still, asked per row rather than per object since 2026-09-13:
/// [`Draft::selected_vocabulary`] routes a `Text` row here and answers
/// `Types` only where this does.
///
/// [`Draft::selected_vocabulary`]: super::Draft::selected_vocabulary
pub fn text_editable(key: &str) -> bool {
    matches!(key, "label" | "width")
}

/// Normalise a committed `Text` on Views, or refuse it with the reason
/// the notice shows (§5.3).
///
/// `width` is the one key with a grammar: [`AUTO`], or a whole pixel
/// count inside `MIN_WIDTH..=MAX_WIDTH`. Refused rather than clamped, the
/// same rule `Draft::apply_text_entry` keeps for a `Number` — a clamp
/// applies a value the trader did not type, and the field stays open with
/// their own text in it so they can correct it.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    match key {
        "width" => {
            if text == AUTO {
                return Ok(AUTO.to_string());
            }
            match text.parse::<i64>() {
                Ok(px) if (MIN_WIDTH..=MAX_WIDTH).contains(&px) => Ok(px.to_string()),
                _ => Err(format!(
                    "width must be {AUTO} or {MIN_WIDTH}–{MAX_WIDTH} px"
                )),
            }
        }
        _ => Ok(text.to_string()),
    }
}

/// [`scale_key`]'s inverse.
pub(super) fn scale_from_key(key: &str) -> Option<Scale> {
    match key {
        "none" => Some(Scale::None),
        "k" => Some(Scale::Thousands),
        "M" => Some(Scale::Millions),
        _ => None,
    }
}

/// [`negative_key`]'s inverse.
pub(super) fn negative_from_key(key: &str) -> Option<Negative> {
    match key {
        "minus" => Some(Negative::Minus),
        "parens" => Some(Negative::Parens),
        _ => None,
    }
}

/// [`colour_key`]'s inverse. Total, unlike the other two: every string
/// that is not one of the two built-in spellings IS a name into
/// `colours.toml`, which is exactly what the reader
/// (`ColumnPresentation::parse_format_keys`) does with it.
pub(super) fn colour_from_key(key: &str) -> Colour {
    match key {
        "none" => Colour::None,
        "sign" => Colour::Sign,
        name => Colour::Named(name.to_string()),
    }
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
                // The one field this cross-check names is `dataset` —
                // `ViewSpec::from_doc`'s own reader diagnostics land on
                // this same key when it is missing (§19.5); this one
                // lands there too, so `Draft::row_for_path` flags the
                // same row whichever check found the problem.
                path: Some(format!("views.{}.dataset", draft.name)),
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

/// What each field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)); under ~90 characters, since
/// the slot is one line (4c spec §22).
pub fn help(key: &str) -> &'static str {
    match key {
        "dataset" => "The dataset the view reads — it decides which columns are available below",
        "columns" => {
            "The view's columns in display order; under AVAILABLE, the dataset's other columns"
        }
        _ => "",
    }
}

/// The column stage's seven presentation fields, explained — one table
/// for both doors (Views and Schema open the same rows, dataset-
/// presentation spec §4.1), which is why `label`'s sentence names the
/// layer below generically: at the Views door an emptied label falls to
/// the dataset level where one is set and the desk otherwise, at the
/// Schema door to each view's own (`Fold`/`FellTo`). See [`help`].
pub fn column_help(key: &str) -> &'static str {
    match key {
        "label" => "The header text — empty stops overriding what the desk or dataset level sets",
        "width" => "Column width in pixels, or auto for the kind's default width",
        "scale" => "Divide values for display: none, k (thousands), M (millions)",
        "precision" => "Decimal places shown, 0 to 12",
        "thousands" => "Group digits with thousands separators",
        "negative" => "How a negative paints: a leading minus, or parentheses",
        "colour" => {
            "none paints in the foreground, sign colours by sign, or a name from colours.toml"
        }
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Domain, EditRow, FellTo, Fold, Step};
    use super::*;
    use geode_core::config::ConfigSources;

    /// `enter_column` the way `render::enter_column_stage` does it: with
    /// the Views door's [`ColumnContext`] installed, so the fold has a
    /// baseline to be honest about (dataset-presentation spec §5.1).
    /// A test that called `enter_column` alone would open a stage whose
    /// fold does nothing, which is not the stage the dialog opens.
    ///
    /// The context comes from [`column_context`], the door's own builder,
    /// rather than a literal here: a test opener with its own copy of
    /// that literal is how a missing layer stays green.
    fn open_column(draft: &mut Draft, column: &str, colours: &[String]) -> bool {
        let Some(item) = draft
            .list_items("columns")
            .and_then(|items| items.iter().find(|i| i.name == column))
            .cloned()
        else {
            return false;
        };
        let fields = column_fields(&item, colours, Destination::Presentation);
        draft.column_ctx = Some(column_context(draft, column, item));
        draft.enter_column(column, fields)
    }

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

    /// A `views` doc of `views_text` verbatim, over a `risk` dataset
    /// declaring exactly the two columns the writer tests below need:
    /// `npv` (a measure, grain instrument) and `book` (a dimension) —
    /// the smallest schema `presentation_table`'s desk-baseline
    /// comparison can exercise without a third column's presentation
    /// muddying an assertion about the other two.
    fn config_with_view(views_text: &str) -> Config {
        config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n\
                 [risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::Desk, "views", views_text),
        ])
    }

    /// The `columns` field's own list, mutable — for a test that pokes at
    /// a `ListItem`'s presentation or inclusion directly rather than
    /// through a verb, the way `presentation_table`'s desk-comparison
    /// tests need to.
    fn items_mut(draft: &mut Draft) -> &mut Vec<ListItem> {
        let field = draft
            .fields
            .iter_mut()
            .find(|f| f.key == "columns")
            .expect("the draft has a columns field");
        match &mut field.kind {
            FieldKind::OrderedList { items, .. } => items,
            _ => panic!("columns is not an ordered list"),
        }
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

    /// Part 2c §4.3: the overlay writer emits one `[view.columns.<col>]`
    /// table per column with an override, carrying only the keys that
    /// differ from the desk's own baseline — never the legacy `hidden`
    /// array or `width` table `presentation_table` wrote before this
    /// task.
    #[test]
    fn the_writer_emits_only_keys_that_differ_from_the_desk() {
        // desk: npv has scale k, precision 2; the trader sets precision 0 and a colour, and hides book.
        let config = config_with_view(
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nformat = { scale = \"k\", precision = 2 }\n[[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n",
        );
        let mut draft = Domain::Views.draft(&config, "tree");
        {
            let items = items_mut(&mut draft);
            items[0].presentation.precision = Some(0);
            items[0].presentation.colour = Some(Colour::Named("delta".to_string()));
            items[1].included = false;
        }
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(text.contains("[tree.columns.npv]"), "{text}");
        assert!(
            text.contains("precision = 0") && text.contains("colour = \"delta\""),
            "{text}"
        );
        assert!(
            !text.contains("scale"),
            "the desk's own scale is not copied: {text}"
        );
        assert!(
            text.contains("[tree.columns.book]") && text.contains("hidden = true"),
            "{text}"
        );
        assert!(
            !text.contains("\nwidth = {") && !text.contains("hidden = ["),
            "no legacy spelling: {text}"
        );
        // Setting precision back to the desk's value drops the key.
        items_mut(&mut draft)[0].presentation.precision = Some(2);
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(!text.contains("precision"), "{text}");
    }

    /// Dataset-presentation spec §5.1: the view overlay's baseline is
    /// desk + dataset, so a view field equal to the DATASET level writes
    /// nothing at all — only the key the trader actually moved off that
    /// baseline reaches `view_presentation.toml`.
    ///
    /// Compared against the desk alone (this task's whole point), every
    /// dataset-level key would be copied into this view's overlay on the
    /// first keystroke in the stage — a per-view override the trader
    /// never made, pinned against their own next dataset-level change.
    ///
    /// Rendered through `object_text` rather than `Table::to_string`,
    /// which prints a `toml_edit::Table`'s leaf values only and would
    /// render this writer's sub-tables as the empty string (the same
    /// departure `dataset_columns`'s writer tests record).
    #[test]
    fn a_view_field_equal_to_the_dataset_level_writes_nothing() {
        // desk: npv width 50. dataset level: npv width 140, scale k.
        // The trader steps scale to k in the VIEW stage — equal to the
        // dataset level, so nothing is written; then width to 200 — only
        // that key is written.
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n\
                 [risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                 [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\nwidth = 50\n",
            ),
            (
                Layer::User,
                "dataset_presentation",
                "[risk.columns.npv]\nwidth = 140\nscale = \"k\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        assert_eq!(draft.dataset_layer["npv"].width, Some(140.0));
        let item = draft
            .list_items("columns")
            .unwrap()
            .iter()
            .find(|i| i.name == "npv")
            .unwrap()
            .clone();
        assert_eq!(
            item.presentation.width,
            Some(140.0),
            "the member row shows the effective value"
        );
        // Simulate the stage: fields from the item, fold with scale = k
        // (unchanged — the field was seeded with the value in force),
        // width 200.
        let mut fields = column_fields(&item, &[], Destination::Presentation);
        for f in &mut fields {
            if f.key == "width" {
                f.kind = FieldKind::Text("200".into());
            }
        }
        let below = baseline_below(&draft).remove("npv").unwrap();
        assert_eq!(
            below.width,
            Some(140.0),
            "the dataset level is merged over the desk's own 50"
        );
        let mut folded = item.clone();
        fold_into(&mut folded, &fields, &below);
        // Put the folded item back and render.
        if let Some(FieldKind::OrderedList { items, .. }) = draft
            .fields
            .iter_mut()
            .find(|f| f.key == "columns")
            .map(|f| &mut f.kind)
        {
            *items.iter_mut().find(|i| i.name == "npv").unwrap() = folded;
        }
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(text.contains("width = 200"), "{text}");
        assert!(
            !text.contains("scale"),
            "equal to the dataset level: not written — {text}"
        );
    }

    /// §5.1 and §3.2: the dataset layer is looked up by the column's
    /// OWNER — the view's own dataset first, then each join's in file
    /// order, the order the compiler resolves names in — so a column a
    /// view reaches through a join takes the JOIN's dataset-level entry.
    ///
    /// Read off only the view's own dataset (the obvious shortcut), a
    /// joined column silently takes nothing from this layer: the trader's
    /// `[ref.columns.sector]` label would paint in every view of `ref`
    /// except the ones that reach it by join, and the writer's baseline
    /// would then call the value in force a divergence and copy it into
    /// that view's own overlay.
    #[test]
    fn a_joined_columns_dataset_layer_comes_from_the_join() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n\
                 [risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [ref.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [ref.columns.exposure]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n\
                 [ref.columns.sector]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"instrument\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
                 joins = [{ dataset = \"ref\", on = [\"book\"] }]\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                 [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\n\
                 [[tree.columns]]\nname = \"sector\"\nkind = \"dimension\"\n",
            ),
            (
                Layer::User,
                "dataset_presentation",
                "[risk.columns.npv]\nscale = \"k\"\n\
                 [ref.columns.sector]\nlabel = \"Sector\"\n",
            ),
        ]);
        let draft = Domain::Views.draft(&config, "tree");
        assert_eq!(
            draft.dataset_layer["sector"].label.as_deref(),
            Some("Sector"),
            "the join's dataset owns `sector`, so its entry is the one that applies"
        );
        assert_eq!(
            draft.dataset_layer["npv"].scale,
            Some(Scale::Thousands),
            "and the view's own dataset still owns its own columns"
        );
        // `book` is declared by BOTH datasets and personalised in
        // neither, so it simply has no entry — the own-dataset-first rule
        // is about which one is asked, not about inventing a default.
        assert!(!draft.dataset_layer.contains_key("book"));
    }

    /// §5.1, the promotion case: a column moved out of the available
    /// catalogue by `space` carries the trader's DATASET-level
    /// presentation with it, so the column stage it opens next seeds
    /// from that layer and the writer sees no divergence.
    ///
    /// Seeded unset, the promoted item paints seven fields at the KIND
    /// default while the stage's baseline — refreshed from the config —
    /// holds the dataset's values: the first keystroke folds all seven
    /// and the overlay gains a view-level `scale = "none"` contradicting
    /// the trader's own dataset level. The two halves are separate
    /// defects and both are asserted here: the catalogue's seed (the
    /// item and the field), and the baseline's reach over a column the
    /// view did not carry when the draft was built (the writer).
    #[test]
    fn a_promoted_columns_stage_seeds_from_the_dataset_level() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n\
                 [risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                 [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\n",
            ),
            (
                Layer::User,
                "dataset_presentation",
                "[risk.columns.delta01]\nscale = \"M\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        // `space` on `delta01`'s available row — the row the catalogue
        // built, stepped the way a keystroke steps it. Rows: dataset,
        // columns, `book`, `npv`, then the catalogue's one entry.
        draft.selected = 4;
        assert_eq!(cursor_item(&draft).as_deref(), Some("delta01"));
        assert!(matches!(
            draft.selected_row(),
            Some(EditRow::Available { .. })
        ));
        assert_eq!(draft.toggle_selected(), Step::Changed);

        let item = draft
            .list_items("columns")
            .unwrap()
            .iter()
            .find(|i| i.name == "delta01")
            .expect("promoted onto the view's own list")
            .clone();
        assert_eq!(
            item.presentation.scale,
            Some(Scale::Millions),
            "the promotion carries the dataset level with it"
        );
        let fields = column_fields(&item, &[], Destination::Presentation);
        let scale = fields.iter().find(|f| f.key == "scale").unwrap();
        assert!(
            matches!(&scale.kind, FieldKind::Choice { options, selected }
                if options.get(*selected).map(String::as_str) == Some(scale_key(Scale::Millions))),
            "the stage seeds Scale from the dataset level, not the kind default: {:?}",
            scale.kind
        );
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(
            !text.contains("scale"),
            "equal to the layer below: the promotion writes no override — {text}"
        );
    }

    /// A bare `ListItem` of `kind`, for [`kind_default`]'s own test —
    /// nothing else about the item matters to it.
    fn item_of_kind(kind: Option<&str>) -> ListItem {
        ListItem {
            name: "x".to_string(),
            included: true,
            presentation: ColumnPresentation::default(),
            kind: kind.map(str::to_string),
            note: None,
        }
    }

    /// Part 2c §5.4: [`kind_default`] answers [`ColumnFormat::TEXT`] only
    /// for a dimension, and [`column_summary`] names only the keys the
    /// trader actually overrode — nothing at all for an untouched column,
    /// and both directions of `thousands` (a dimension turning it ON is
    /// as much an override as a measure turning it off).
    #[test]
    fn column_summary_names_only_the_keys_in_force() {
        assert_eq!(
            kind_default(&item_of_kind(Some("dimension"))),
            ColumnFormat::TEXT
        );
        assert_eq!(
            kind_default(&item_of_kind(Some("measure"))),
            ColumnFormat::MEASURE
        );
        assert_eq!(kind_default(&item_of_kind(None)), ColumnFormat::MEASURE);

        let overridden = ColumnPresentation {
            width: Some(120.0),
            scale: Some(Scale::Thousands),
            precision: Some(0),
            colour: Some(Colour::Named("delta".to_string())),
            ..Default::default()
        };
        assert_eq!(
            column_summary(&ColumnFormat::MEASURE, &overridden),
            "120 px · k · 0 dp · delta"
        );

        assert_eq!(
            column_summary(&ColumnFormat::MEASURE, &ColumnPresentation::default()),
            "",
            "an untouched column paints nothing"
        );

        // A dimension defaults to no thousands separators; turning them ON
        // is exactly as much an override as a measure turning them off.
        let dimension_thousands = ColumnPresentation {
            thousands: Some(true),
            ..Default::default()
        };
        let summary = column_summary(&ColumnFormat::TEXT, &dimension_thousands);
        assert!(
            summary.contains("thousands"),
            "a dimension's non-default thousands must be named: {summary:?}"
        );
    }

    /// Part 2c §5.3: the column stage's seven fields, in the overlay's own
    /// key order, every one `Destination::Presentation` (nothing here can
    /// fork, so nothing here asks), and each seeded with the value the
    /// trader currently SEES — the kind default with the column's own
    /// presentation over it — rather than with the raw `Option` the
    /// overlay happens to hold.
    #[test]
    fn column_fields_are_seven_presentation_rows_seeded_from_the_item() {
        let item = ListItem {
            name: "npv".into(),
            included: true,
            kind: Some("measure".into()),
            presentation: ColumnPresentation {
                scale: Some(Scale::Thousands),
                precision: Some(0),
                width: Some(120.0),
                colour: Some(Colour::Named("delta".into())),
                ..Default::default()
            },
            note: None,
        };
        let fields = column_fields(
            &item,
            &["delta".to_string(), "gamma".to_string()],
            Destination::Presentation,
        );
        let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, COLUMN_KEYS);
        assert!(fields.iter().all(|f| f.dest == Destination::Presentation));
        let by = |k: &str| fields.iter().find(|f| f.key == k).unwrap();
        assert!(matches!(&by("label").kind, FieldKind::Text(t) if t.is_empty()));
        assert!(matches!(&by("width").kind, FieldKind::Text(t) if t == "120"));
        assert!(
            matches!(&by("scale").kind, FieldKind::Choice { options, selected } if options[*selected] == "k")
        );
        assert!(matches!(
            &by("precision").kind,
            FieldKind::Number {
                value: 0,
                min: 0,
                max: 12,
                step: 1,
                wrap: false
            }
        ));
        assert!(
            matches!(&by("thousands").kind, FieldKind::Bool(true)),
            "the measure default"
        );
        assert!(
            matches!(&by("negative").kind, FieldKind::Choice { options, selected } if options[*selected] == "minus")
        );
        assert!(
            matches!(&by("colour").kind, FieldKind::Choice { options, selected }
                if options == &["none", "sign", "delta", "gamma"] && options[*selected] == "delta")
        );
    }

    /// Part 2c §5.2: the fold is what carries a keystroke in the stage
    /// back onto the item the overlay writer renders from. `auto` is a
    /// value, not a blank — it clears the width rather than parsing as
    /// one — and an empty label is the column's own name, so it clears
    /// too.
    #[test]
    fn fold_into_writes_the_fields_back_and_auto_clears_the_width() {
        let mut item = ListItem {
            name: "npv".into(),
            included: true,
            kind: Some("measure".into()),
            presentation: ColumnPresentation::default(),
            note: None,
        };
        let mut fields = column_fields(&item, &[], Destination::Presentation);
        for f in &mut fields {
            match f.key.as_str() {
                "width" => f.kind = FieldKind::Text("auto".into()),
                "precision" => {
                    f.kind = FieldKind::Number {
                        value: 4,
                        min: 0,
                        max: 12,
                        step: 1,
                        wrap: false,
                    }
                }
                "colour" => {
                    f.kind = FieldKind::Choice {
                        options: vec!["none".into(), "sign".into()],
                        selected: 1,
                    }
                }
                "label" => f.kind = FieldKind::Text("NPV".into()),
                _ => {}
            }
        }
        // A desk that declares nothing: `auto` and an empty label have
        // nothing to fall back to, so each really does clear its key.
        assert_eq!(
            fold_into(&mut item, &fields, &ColumnPresentation::default()),
            None,
            "nothing followed the desk — there is no desk value to follow"
        );
        assert_eq!(item.presentation.width, None);
        assert_eq!(item.presentation.precision, Some(4));
        assert_eq!(item.presentation.colour, Some(Colour::Sign));
        assert_eq!(item.presentation.label.as_deref(), Some("NPV"));
    }

    /// Part 2c §5.3, the clear verb: an empty `label` means "stop
    /// overriding", which resolves to the DESK's label — never a literal
    /// `None`, which the writer would omit and the next rebuild would
    /// read as "nothing to say" while the desk's label came back anyway,
    /// silently swallowing the clear.
    ///
    /// Both directions: a desk that sets a label, and one that does not.
    #[test]
    fn clearing_a_desk_label_falls_back_to_the_desk() {
        let with_desk_label = config_with_view(
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nlabel = \"NPV\"\n",
        );
        let mut draft = Domain::Views.draft(&with_desk_label, "tree");
        let item = draft.list_items("columns").unwrap()[0].clone();
        assert_eq!(
            item.presentation.label.as_deref(),
            Some("NPV"),
            "sanity: the item carries the desk's label"
        );
        assert!(open_column(&mut draft, "npv", &[]));
        clear_text_field(&mut draft, "label");
        assert_eq!(
            draft.fold_column(),
            Some(Fold {
                key: "label",
                to: Some(FellTo::Desk)
            })
        );

        assert_eq!(
            draft.list_items("columns").unwrap()[0]
                .presentation
                .label
                .as_deref(),
            Some("NPV"),
            "the clear stops overriding; it cannot delete the desk's key"
        );
        assert!(
            matches!(&draft.fields[0].kind, FieldKind::Text(t) if t == "NPV"),
            "and the field is re-seeded, so the screen is not a blank that lies"
        );
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(!text.contains("label"), "so no key is written: {text}");

        // No desk label: the same clear really does clear, and says
        // nothing about following anything.
        let bare =
            config_with_view("[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n");
        let mut draft = Domain::Views.draft(&bare, "tree");
        assert!(open_column(&mut draft, "npv", &[]));
        clear_text_field(&mut draft, "label");
        assert_eq!(
            draft.fold_column(),
            None,
            "the field was already empty, so this keystroke cleared \
             nothing — a clear is measured against the ITEM, not against \
             whether some layer below sets the key"
        );
        assert_eq!(
            draft.list_items("columns").unwrap()[0].presentation.label,
            None
        );
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(!text.contains("label"), "{text}");
    }

    /// [`clearing_a_desk_label_falls_back_to_the_desk`]'s twin for
    /// `width`, where the clear is spelled [`AUTO`] rather than an empty
    /// string — the same ruling, the same failure if it wrote `None`.
    #[test]
    fn an_auto_width_falls_back_to_the_desk() {
        let with_desk_width = config_with_view(
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nwidth = 140\n",
        );
        let mut draft = Domain::Views.draft(&with_desk_width, "tree");
        let item = draft.list_items("columns").unwrap()[0].clone();
        assert_eq!(item.presentation.width, Some(140.0), "sanity");
        assert!(open_column(&mut draft, "npv", &[]));
        set_text_field(&mut draft, "width", AUTO);
        assert_eq!(
            draft.fold_column(),
            Some(Fold {
                key: "width",
                to: Some(FellTo::Desk)
            })
        );

        assert_eq!(
            draft.list_items("columns").unwrap()[0].presentation.width,
            Some(140.0)
        );
        assert!(matches!(&draft.fields[1].kind, FieldKind::Text(t) if t == "140"));
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(!text.contains("width"), "{text}");

        let bare =
            config_with_view("[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n");
        let mut draft = Domain::Views.draft(&bare, "tree");
        assert!(open_column(&mut draft, "npv", &[]));
        set_text_field(&mut draft, "width", AUTO);
        assert_eq!(draft.fold_column(), None, "already auto: nothing cleared");
        assert_eq!(
            draft.list_items("columns").unwrap()[0].presentation.width,
            None
        );
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(!text.contains("width"), "{text}");
    }

    /// What `i`, a typed value and `enter` leave behind on one installed
    /// `Text` row — the pure half of that path, with no window.
    fn set_text_field(draft: &mut Draft, key: &str, value: &str) {
        let field = draft
            .fields
            .iter_mut()
            .find(|f| f.key == key)
            .expect("the column stage installs this field");
        field.kind = FieldKind::Text(value.to_string());
    }

    fn clear_text_field(draft: &mut Draft, key: &str) {
        set_text_field(draft, key, "");
    }

    /// Part 2c §4.3 against §5.2: one change in the column stage writes
    /// ONE key.
    ///
    /// The fold gives every format key a `Some` (it has a field for each,
    /// seeded with the value in force), so the writer's baseline
    /// comparison is the only thing standing between a trader who steps
    /// the scale and a file that has also adopted the kind's precision,
    /// thousands, negative and colour — frozen against the desk's next
    /// change, which is the exact freeze §4.3 forbids.
    #[test]
    fn a_fold_of_untouched_keys_leaves_them_out_of_the_overlay() {
        let config = config_with_view(
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n",
        );
        let mut draft = Domain::Views.draft(&config, "tree");
        assert!(open_column(&mut draft, "npv", &[]));
        let scale = draft.fields.iter().position(|f| f.key == "scale").unwrap();
        draft.selected = scale;
        assert_eq!(draft.toggle_selected(), Step::Changed);
        draft.fold_column();

        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(text.contains("scale = \"k\""), "{text}");
        for untouched in ["precision", "thousands", "negative", "colour"] {
            assert!(
                !text.contains(untouched),
                "{untouched} is the measure default the desk never declared: {text}"
            );
        }
    }

    /// Part 2c §5.3: `width` is a `Text` because `auto` is one of its
    /// values; everything else it accepts is a pixel count inside the
    /// range a table can actually lay out. `label` and `width` are the
    /// only two keys `i` may open on Views.
    #[test]
    fn width_text_is_auto_or_a_pixel_count_in_range() {
        assert_eq!(parse_text("width", " 120 ").unwrap(), "120");
        assert_eq!(parse_text("width", "auto").unwrap(), "auto");
        assert!(parse_text("width", "5").is_err() && parse_text("width", "wide").is_err());
        assert!(text_editable("label") && text_editable("width") && !text_editable("dataset"));
    }
}
