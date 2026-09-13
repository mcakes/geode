//! The Schema dialog's column stage — the dataset-level door into the
//! same seven-field stage the Views dialog has (dataset-presentation
//! spec §4). Pure: no gpui.
//!
//! Three jobs, all of them about the ONE file this door writes,
//! `dataset_presentation.toml`: seeding a scratch [`ListItem`] from the
//! dataset's overlay table ([`item_for`]), rendering that table back with
//! only the keys that differ from the column's kind default
//! ([`table`], §4.5), and the summary a personalised Schema row carries
//! ([`row_summary`], §4.7). [`provenance_of`] serves both doors.

use super::views;
use super::{ColumnContext, ColumnDoor, Draft, Field, FieldKind, ListItem, Provenance};
use geode_core::config::Config;
use geode_core::schema::{ColumnType, DatasetSpec};
use geode_core::view::{ColumnFormat, ColumnPresentation};

/// The doc this door writes: `dataset_presentation.toml`, user layer.
/// Named here rather than spelled again, the same way `views::DOC` and
/// `views::PRESENTATION_DOC` are the one spelling for theirs.
pub const DOC: &str = geode_core::view::DATASET_PRESENTATION_DOC;

/// The per-column values [`provenance_of`] needs that do not vary from
/// field to field, computed **once per render** rather than once per
/// field.
///
/// Both are allocating — `below_view` clones the desk layer and merges
/// the dataset one over it (a `label` and a named `colour` are `String`s),
/// and `kind_default` yields a `ColumnFormat` carrying a `Colour` — so
/// building them inside `provenance_of` meant seven clones per painted
/// frame of the column stage, per-frame heap churn on the render thread
/// that the charter names a defect (review round 1's Minor).
pub struct ProvenanceInputs<'a> {
    pub ctx: &'a ColumnContext,
    /// The layers below the view overlay (desk + dataset) — `Some` for
    /// the Views door alone. The Dataset door compares against the unset
    /// presentation and never reads this, so it is not built there
    /// either: a Schema column stage would otherwise pay `below_view`'s
    /// clone-and-merge on every painted frame for a value nothing looks
    /// at (fix round 1's Minor).
    pub below: Option<ColumnPresentation>,
    pub kind: ColumnFormat,
}

impl<'a> ProvenanceInputs<'a> {
    pub fn new(ctx: &'a ColumnContext) -> ProvenanceInputs<'a> {
        ProvenanceInputs {
            below: match ctx.door {
                ColumnDoor::View => Some(ctx.layers.below_view()),
                ColumnDoor::Dataset => None,
            },
            kind: ctx
                .item
                .as_ref()
                .map(views::kind_default)
                .unwrap_or(ColumnFormat::MEASURE),
            ctx,
        }
    }
}

/// Which layer's value `field` is showing (§5.3). Through the Views
/// door: `View` when the field differs from the desk + dataset baseline
/// (the trader has diverged here, whether or not the write has landed),
/// else `Dataset` / `Desk` by which layer sets the key, else `None`.
/// Through the Schema door: `Dataset` when the field differs from the
/// kind default, else `None` — there is no desk value to name (§4.3, a
/// plan-level ruling over the spec's `user` badge).
///
/// Computed at paint from the draft's own context, never stored on the
/// [`Field`]: a stored provenance goes stale the keystroke after a step,
/// and the whole point of the chip is that a stepped field reads `view`
/// on the same frame rather than 250 ms later when the write lands.
pub fn provenance_of(inputs: &ProvenanceInputs, field: &Field) -> Option<Provenance> {
    let ctx = inputs.ctx;
    let kind = &inputs.kind;
    let key = field.key.as_str();
    let set_in = |p: &ColumnPresentation| match key {
        "label" => p.label.is_some(),
        "width" => p.width.is_some(),
        "scale" => p.scale.is_some(),
        "precision" => p.precision.is_some(),
        "thousands" => p.thousands.is_some(),
        "negative" => p.negative.is_some(),
        "colour" => p.colour.is_some(),
        // A key this stage does not paint is set at no layer, so it
        // carries no provenance — the same answer `differs_from`'s own
        // fallthrough gives, and between them this function answers
        // `None` for it through either door.
        _ => false,
    };
    let differs_from = |p: &ColumnPresentation| {
        let effective = kind.clone().with(p);
        match (key, &field.kind) {
            // Both sides trimmed: the field's own text is trimmed on the
            // way into `views::fold_into`, so a layer whose label was
            // written with surrounding space would otherwise read as a
            // divergence the trader never made.
            ("label", FieldKind::Text(t)) => t.trim() != p.label.as_deref().unwrap_or("").trim(),
            ("width", FieldKind::Text(t)) => t.trim() != views::width_text(p.width),
            ("scale", FieldKind::Choice { options, selected }) => {
                options.get(*selected).map(String::as_str)
                    != Some(views::scale_key(effective.scale))
            }
            ("precision", FieldKind::Number { value, .. }) => {
                *value != i64::from(effective.precision)
            }
            ("thousands", FieldKind::Bool(b)) => *b != effective.thousands,
            ("negative", FieldKind::Choice { options, selected }) => {
                options.get(*selected).map(String::as_str)
                    != Some(views::negative_key(effective.negative))
            }
            ("colour", FieldKind::Choice { options, selected }) => {
                options.get(*selected).map(String::as_str)
                    != Some(views::colour_key(&effective.colour).as_str())
            }
            _ => false,
        }
    };
    let unset = ColumnPresentation::default();
    match ctx.door {
        ColumnDoor::Dataset => {
            (differs_from(&unset) || set_in(&ctx.layers.dataset)).then_some(Provenance::Dataset)
        }
        ColumnDoor::View => {
            // `Some` for this door by construction
            // ([`ProvenanceInputs::new`]); borrowed, never cloned.
            let below = inputs.below.as_ref().unwrap_or(&unset);
            if differs_from(below) || set_in(&ctx.layers.view) {
                Some(Provenance::View)
            } else if set_in(&ctx.layers.dataset) {
                Some(Provenance::Dataset)
            } else if set_in(&ctx.layers.desk) {
                Some(Provenance::Desk)
            } else {
                None
            }
        }
    }
}

/// The `[<dataset>]` table of `dataset_presentation.toml` as the config
/// holds it (§4.5) — empty when there is none. Callers pass the
/// pending-aware config (`apply::config_with_pending`), never
/// `services.config` alone: inside the 250 ms write debounce the latter
/// is the doc as it stood before the last keystroke, and a writer seeded
/// from it would render the other columns as they were BEFORE that
/// keystroke and undo it.
pub fn overlay_object(config: &Config, dataset: &str) -> toml::Table {
    config
        .doc(DOC)
        .and_then(|doc| doc.value.get(dataset))
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default()
}

/// The scratch item the Schema door's fields fold into: the column's
/// kind from its schema role (`views::schema_role_kind`), its
/// presentation from the overlay table's `columns.<col>` entry if any
/// (§4.3). `None` for a column the dataset does not declare.
///
/// The presentation is seeded from the overlay **alone** — not merged
/// with any desk value — because there is no desk value at this level to
/// merge: the desk's keys live on each view's own `[[columns]]` entry and
/// sit BELOW this layer (§4.3). A field whose key the overlay does not
/// set therefore seeds from the kind default, which is what
/// `views::column_fields` does with an unset `ColumnPresentation`.
///
/// Diagnostics are dropped (`noop`): the reader has already reported
/// every one of them against the file, with its path, and reporting them
/// a second time from inside a keystroke would put a file-level warning
/// on a stage that is about one column.
///
/// **A `key` or `attribute` column takes its kind from its TYPE**
/// ([`kind_for_type`]). This door is open to every column the dataset
/// declares — a trader wants a label and a width on `position_ref` as
/// much as on `npv` — but `views::schema_role_kind` has no kind for
/// those two roles (the view reader accepts only `dimension`, `measure`
/// and `derived`), and a bare `None` makes `views::kind_default` answer
/// `ColumnFormat::MEASURE`. That painted a `utf8` key column with
/// Precision 2, Thousands on and a Colour, and stepping any of them
/// wrote a real key into the overlay that every view carrying that
/// column then merges (fix round 1's Important).
pub fn item_for(
    dataset: &DatasetSpec,
    column: &str,
    overlay_object: &toml::Table,
) -> Option<ListItem> {
    let spec = dataset.column(column)?;
    let mut presentation = ColumnPresentation::default();
    if let Some(ct) = overlay_object
        .get("columns")
        .and_then(|c| c.as_table())
        .and_then(|c| c.get(column))
        .and_then(|v| v.as_table())
    {
        let noop = |_: &str, _: String| {};
        presentation.parse_format_keys(ct, &noop);
        // `read_hidden: false`: membership belongs to a view, and the
        // reader refuses `hidden` here for the same reason (§2.1).
        presentation.parse_column_keys(ct, false, &noop);
    }
    Some(ListItem {
        name: column.to_string(),
        included: true,
        presentation,
        kind: views::schema_role_kind(&spec.role)
            .or_else(|| kind_for_type(spec.ty))
            .map(str::to_string),
    })
}

/// The `kind` a column whose ROLE names none should be presented as — the
/// fallback [`item_for`]'s doc explains, for `key` and `attribute`.
///
/// A non-numeric column is `"dimension"`, so `views::kind_default` gives
/// it [`ColumnFormat::TEXT`]: precision 0, no thousands separator, no
/// colour — the format a text column actually has in a blotter. A numeric
/// one answers `None` and keeps the MEASURE default, which is right for
/// it: a numeric attribute is formatted like a measure even though it
/// does not sum.
///
/// Deliberately keyed on the type rather than on the role: the question
/// "how is this column formatted" is a question about its values, and a
/// future role would otherwise have to be remembered here as well as in
/// `views::schema_role_kind`.
fn kind_for_type(ty: ColumnType) -> Option<&'static str> {
    match ty {
        ColumnType::F64 | ColumnType::I64 => None,
        ColumnType::Utf8 | ColumnType::Date | ColumnType::Timestamp | ColumnType::Bool => {
            Some("dimension")
        }
    }
}

/// The whole `[<dataset>]` object (§4.5): every OTHER column's table
/// copied verbatim from the overlay, the open column re-rendered from the
/// fields with only the keys that differ from the kind default (label
/// from the column's name → absent, width from `auto` → absent), and
/// dropped when nothing differs. Empty when no column remains, which
/// `apply::object_value` turns into a removal.
///
/// **The other columns are copied, not re-derived.** This doc is atomic
/// at depth one (`config::merge::atomic_depth`), so a write replaces the
/// dataset's whole table: rendering only the open column would erase
/// every other personalised column of that dataset on the first
/// keystroke.
///
/// **Sibling keys of `columns` are NOT carried through.** `[<dataset>]`
/// holds one key in this vocabulary and `columns` is it — a hand-written
/// `order`, or any unknown key, is already refused by
/// `DatasetPresentationSpec::from_doc` with a warning naming
/// `view_presentation.toml` (§2.1), so it has no effect on anything the
/// app reads and nothing here is preserving state by keeping it. A write
/// through this dialog therefore normalises the object to `columns`
/// alone, which is the same thing the warning asks the trader to do by
/// hand. Contrast the columns themselves, which ARE state and are copied
/// verbatim above.
///
/// The fields are folded into a CLONE of the context's item first, so the
/// writer sees the keystroke that is being committed rather than the one
/// before it. `Draft::fold_column` has already folded the same fields
/// into the context's own item by the time a real write reaches here
/// (`render::revalidate` runs first), which makes this fold idempotent —
/// but it is what lets a caller render the table without a draft that has
/// been through the dialog's key path.
pub fn table(draft: &Draft) -> toml_edit::Table {
    let mut out = toml_edit::Table::new();
    let Some(ctx) = draft.column_ctx.as_ref() else {
        return out;
    };
    let Some(open) = draft.column() else {
        return out;
    };
    let mut columns = toml_edit::Table::new();
    if let Some(existing) = ctx.overlay_object.get("columns").and_then(|c| c.as_table()) {
        for (name, value) in existing {
            if name != open
                && let Some(t) = value.as_table()
            {
                columns[name.as_str()] = toml_edit::Item::Table(super::toml_table_to_edit(t));
            }
        }
    }
    if let Some(item) = ctx.item.as_ref() {
        let mut folded = item.clone();
        views::fold_into(&mut folded, &draft.fields, &ColumnPresentation::default());
        let kind = views::kind_default(&folded);
        let effective = kind.clone().with(&folded.presentation);
        let p = &folded.presentation;
        let mut t = toml_edit::Table::new();
        // The five format keys compare RESOLVED against the kind default,
        // for `views::presentation_table`'s own reason: the fold writes a
        // `Some` for every one of them (each field was seeded with the
        // value in force), so a raw-`Option` comparison would copy the
        // whole kind default into the trader's file on the first
        // keystroke.
        if effective.precision != kind.precision
            && let Some(v) = p.precision
        {
            t["precision"] = toml_edit::value(i64::from(v));
        }
        if effective.thousands != kind.thousands
            && let Some(v) = p.thousands
        {
            t["thousands"] = toml_edit::value(v);
        }
        if effective.negative != kind.negative
            && let Some(v) = p.negative
        {
            t["negative"] = toml_edit::value(views::negative_key(v));
        }
        if effective.colour != kind.colour
            && let Some(v) = &p.colour
        {
            t["colour"] = toml_edit::value(views::colour_key(v));
        }
        if effective.scale != kind.scale
            && let Some(v) = p.scale
        {
            t["scale"] = toml_edit::value(views::scale_key(v));
        }
        // `label` and `width` have no kind default to resolve against —
        // a column either has one or it does not — so both compare as
        // bare `Option`s, and a cleared one (`fold_into` wrote the
        // baseline's `None` back) simply is not written.
        if let Some(v) = &p.label
            && !v.is_empty()
        {
            t["label"] = toml_edit::value(v.as_str());
        }
        if let Some(v) = p.width {
            t["width"] = views::width_value(v);
        }
        if !t.is_empty() {
            columns[open] = toml_edit::Item::Table(t);
        }
    }
    if !columns.is_empty() {
        out["columns"] = toml_edit::Item::Table(columns);
    }
    out
}

/// The Schema row's suffix (§4.7): `" · k · 0 dp · delta"` when the
/// dataset table sets anything for this column, `""` otherwise.
///
/// Guarded on the presentation being unset rather than on the summary
/// being empty, because the two are different facts: a column whose
/// overlay sets only keys that happen to equal the kind default has a
/// personalisation with nothing to say about it, and painting a bare
/// ` · ` for it would be worse than painting nothing.
pub fn row_summary(item: &ListItem) -> String {
    if item.presentation == ColumnPresentation::default() {
        return String::new();
    }
    let summary = views::column_summary(&views::kind_default(item), &item.presentation);
    if summary.is_empty() {
        String::new()
    } else {
        format!(" · {summary}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ColumnLayers, Destination, ListItem};
    use super::*;
    use geode_core::view::{ColumnPresentation, Scale};

    fn item(p: ColumnPresentation) -> ListItem {
        ListItem {
            name: "npv".into(),
            included: true,
            presentation: p,
            kind: Some("measure".into()),
        }
    }

    fn ctx(door: ColumnDoor, layers: ColumnLayers) -> ColumnContext {
        let merged = {
            let mut p = layers.below_view();
            p.merge_over(&layers.view);
            p
        };
        ColumnContext {
            door,
            item: Some(item(merged)),
            layers,
            overlay_object: toml::Table::new(),
        }
    }

    fn field(ctx: &ColumnContext, key: &str) -> Field {
        let fields =
            views::column_fields(ctx.item.as_ref().unwrap(), &[], Destination::Presentation);
        fields.into_iter().find(|f| f.key == key).unwrap()
    }

    fn provenance(ctx: &ColumnContext, field: &Field) -> Option<Provenance> {
        provenance_of(&ProvenanceInputs::new(ctx), field)
    }

    #[test]
    fn provenance_names_the_layer_whose_value_is_in_force() {
        let layers = ColumnLayers {
            desk: ColumnPresentation {
                label: Some("desk".into()),
                ..Default::default()
            },
            dataset: ColumnPresentation {
                scale: Some(Scale::Thousands),
                ..Default::default()
            },
            view: ColumnPresentation {
                precision: Some(4),
                ..Default::default()
            },
        };
        let c = ctx(ColumnDoor::View, layers);
        assert_eq!(provenance(&c, &field(&c, "label")), Some(Provenance::Desk));
        assert_eq!(
            provenance(&c, &field(&c, "scale")),
            Some(Provenance::Dataset)
        );
        assert_eq!(
            provenance(&c, &field(&c, "precision")),
            Some(Provenance::View)
        );
        assert_eq!(
            provenance(&c, &field(&c, "colour")),
            None,
            "kind default: no chip"
        );
    }

    #[test]
    fn a_stepped_field_reads_view_before_its_write_lands() {
        let layers = ColumnLayers {
            dataset: ColumnPresentation {
                scale: Some(Scale::Thousands),
                ..Default::default()
            },
            ..Default::default()
        };
        let c = ctx(ColumnDoor::View, layers);
        let mut scale = field(&c, "scale");
        if let FieldKind::Choice { options, selected } = &mut scale.kind {
            *selected = options
                .iter()
                .position(|o| o == views::scale_key(Scale::Millions))
                .unwrap();
        }
        assert_eq!(provenance(&c, &scale), Some(Provenance::View));
    }

    #[test]
    fn the_dataset_door_reads_dataset_or_nothing() {
        let layers = ColumnLayers {
            dataset: ColumnPresentation {
                width: Some(90.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let c = ctx(ColumnDoor::Dataset, layers);
        assert_eq!(
            provenance(&c, &field(&c, "width")),
            Some(Provenance::Dataset)
        );
        assert_eq!(provenance(&c, &field(&c, "label")), None);
    }
}

#[cfg(test)]
mod writer_tests {
    use super::super::{ColumnLayers, Destination, Draft};
    use super::*;
    use geode_core::view::Scale;

    fn risk_schema() -> geode_core::schema::SchemaSpec {
        geode_core::schema::SchemaSpec::from_doc(&geode_core::config::merge_docs(
            "datasets",
            &[geode_core::config::LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
            )
            .unwrap()],
        ))
        .0
    }

    fn overlay(text: &str) -> toml::Table {
        text.parse().unwrap()
    }

    /// The rendered object as the FILE would hold it. A bare
    /// `toml_edit::Table`'s own `Display` prints its LEAF values only
    /// (`get_values` recurses into dotted tables alone), so a writer whose
    /// whole output is sub-tables renders as the empty string through it.
    /// `super::super::object_text` is this crate's one spelling of "what a
    /// write produces", and it is what `apply::object_value` parses back,
    /// so asserting on it is asserting on the write itself.
    fn written(table: toml_edit::Table) -> String {
        super::super::object_text("risk", toml_edit::Item::Table(table))
    }

    #[test]
    fn item_for_seeds_from_the_overlay_or_the_kind_default() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        let set = item_for(
            risk,
            "npv",
            &overlay("[columns.npv]\nwidth = 90\nscale = \"k\"\n"),
        )
        .unwrap();
        assert_eq!(set.presentation.width, Some(90.0));
        assert_eq!(set.presentation.scale, Some(Scale::Thousands));
        assert_eq!(set.kind.as_deref(), Some("measure"));
        let unset = item_for(risk, "book", &overlay("")).unwrap();
        assert_eq!(unset.presentation, ColumnPresentation::default());
        assert_eq!(unset.kind.as_deref(), Some("dimension"));
        assert!(item_for(risk, "ghost", &overlay("")).is_none());
    }

    /// Fix round 1's Important. `position_ref` is a `utf8` KEY column, a
    /// role `views::schema_role_kind` has no kind for — and a bare `None`
    /// there makes `views::kind_default` answer `ColumnFormat::MEASURE`,
    /// so the stage painted a text column with Precision 2 and Thousands
    /// on, and stepping either wrote a real key into the overlay that
    /// every view carrying the column then merges. The type decides
    /// instead, so the seven fields open at `ColumnFormat::TEXT`.
    #[test]
    fn a_key_column_takes_the_text_kind_from_its_type() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        let key = item_for(risk, "position_ref", &overlay("")).unwrap();
        assert_eq!(key.kind.as_deref(), Some("dimension"));
        assert_eq!(views::kind_default(&key), ColumnFormat::TEXT);

        let fields = views::column_fields(&key, &[], Destination::DatasetPresentation);
        let kind_of = |k: &str| fields.iter().find(|f| f.key == k).unwrap().kind.clone();
        assert!(
            matches!(kind_of("precision"), FieldKind::Number { value, .. }
                     if value == i64::from(ColumnFormat::TEXT.precision)),
            "{:?}",
            kind_of("precision")
        );
        assert_eq!(
            kind_of("thousands"),
            FieldKind::Bool(ColumnFormat::TEXT.thousands)
        );
        // The numeric roles are untouched: `npv` is a measure by role and
        // never reaches the fallback, and a numeric column whose role has
        // no kind keeps MEASURE deliberately.
        assert_eq!(kind_for_type(ColumnType::F64), None);
        assert_eq!(kind_for_type(ColumnType::I64), None);
    }

    #[test]
    fn the_writer_keeps_other_columns_verbatim_and_emits_only_keys_off_the_kind_default() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        let overlay_object =
            overlay("[columns.book]\nlabel = \"Book\"\n[columns.npv]\nwidth = 90\n");
        let item = item_for(risk, "npv", &overlay_object).unwrap();
        let fields = views::column_fields(&item, &[], Destination::DatasetPresentation);
        let mut draft = Draft::new_object("risk", fields, toml::Table::new());
        draft.column_ctx = Some(ColumnContext {
            door: ColumnDoor::Dataset,
            layers: ColumnLayers::default(),
            overlay_object,
            item: Some(item),
        });
        // Precision left at the kind default (2): not written. Width kept.
        // Thousands stepped: written.
        for f in &mut draft.fields {
            if f.key == "thousands" {
                f.kind = FieldKind::Bool(false);
            }
        }
        draft.column = Some("npv".into());
        let text = written(table(&draft));
        assert!(
            text.contains("[risk.columns.book]") && text.contains("label = \"Book\""),
            "{text}"
        );
        assert!(text.contains("[risk.columns.npv]"), "{text}");
        assert!(text.contains("width = 90"), "{text}");
        assert!(text.contains("thousands = false"), "{text}");
        assert!(!text.contains("precision"), "{text}");
    }

    #[test]
    fn an_emptied_column_leaves_the_table_and_an_empty_object_is_removed() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        let overlay_object = overlay("[columns.npv]\nwidth = 90\n");
        let item = item_for(risk, "npv", &overlay_object).unwrap();
        let fields = views::column_fields(&item, &[], Destination::DatasetPresentation);
        let mut draft = Draft::new_object("risk", fields, toml::Table::new());
        draft.column_ctx = Some(ColumnContext {
            door: ColumnDoor::Dataset,
            layers: ColumnLayers::default(),
            overlay_object,
            item: Some(item),
        });
        draft.column = Some("npv".into());
        for f in &mut draft.fields {
            if f.key == "width" {
                f.kind = FieldKind::Text("auto".into());
            }
        }
        let table = table(&draft);
        assert!(table.is_empty(), "{table}");
        assert!(matches!(
            super::super::apply::object_value(
                "risk",
                toml_edit::Item::Table(table),
                Destination::DatasetPresentation
            ),
            super::super::apply::ObjectWrite::Remove
        ));
    }

    #[test]
    fn row_summary_is_empty_without_personalisation() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        assert_eq!(
            row_summary(&item_for(risk, "npv", &overlay("")).unwrap()),
            ""
        );
        let set = item_for(
            risk,
            "npv",
            &overlay("[columns.npv]\nscale = \"k\"\nprecision = 0\n"),
        )
        .unwrap();
        assert_eq!(row_summary(&set), " · k · 0 dp");
    }
}
