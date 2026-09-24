//! Dataset-level column presentation for the Schema dialog. This pure adapter seeds a
//! scratch column item, renders its dataset overlay, and supplies row summaries. Both
//! Schema and Views use its provenance calculation.
//!
//! Writes target `dataset_presentation.toml`. They retain other columns of the same
//! dataset because a user-layer dataset object replaces that object whole.

use super::views;
use super::{ColumnContext, ColumnDoor, Draft, Field, FieldKind, ListItem, Provenance};
use geode_core::config::Config;
use geode_core::schema::{ColumnType, DatasetSpec};
use geode_core::view::{ColumnFormat, ColumnPresentation};

/// The doc this door writes: `dataset_presentation.toml`, user layer.
/// Named here rather than spelled again, the same way `views::DOC` and
/// `views::PRESENTATION_DOC` are the one spelling for theirs.
pub const DOC: &str = geode_core::view::DATASET_PRESENTATION_DOC;

/// Per-column inputs shared by all seven provenance checks in one render. Compute
/// allocating baseline merges and kind defaults once for the column.
pub struct ProvenanceInputs<'a> {
    pub ctx: &'a ColumnContext,
    /// View definition with dataset presentation applied. Used only for the Views
    /// stage; the Dataset stage compares against the unset presentation.
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

/// Provenance of the current draft value, recomputed at paint so it tracks an edit
/// before persistence.
///
/// In Views, a value differing from the definition-plus-dataset baseline is `View`;
/// otherwise name the dataset or definition layer that sets the key. In Schema, a
/// nondefault value or an explicitly stored dataset key is `Dataset`. Other fields have
/// no badge.
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
            if differs_from(below) {
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

/// The `[<dataset>]` table of `dataset_presentation.toml` as the config holds it —
/// empty when there is none. Callers pass the pending-aware config
/// (`apply::config_with_pending`), never `services.config` alone: inside the 250 ms
/// write debounce the latter is the doc as it stood before the last keystroke, and a
/// writer seeded from it would render the other columns as they were BEFORE that
/// keystroke and undo it.
pub fn overlay_object(config: &Config, dataset: &str) -> toml::Table {
    config
        .doc(DOC)
        .and_then(|doc| doc.value.get(dataset))
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default()
}

/// Seed the Schema stage's scratch item from the dataset overlay alone. Return `None`
/// for an undeclared column. The underlying view definitions do not supply values at
/// this editing level, so unset fields use the kind default.
///
/// Roles with a view kind retain it; key and attribute columns derive a formatting kind
/// from their type so text columns use text defaults. Local parsing discards
/// diagnostics because it is reading presentation for one column, not reporting the
/// whole file.
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
        // `read_hidden: false`: membership belongs to a view, and the reader refuses
        // `hidden` here for the same reason.
        presentation.parse_column_keys(ct, false, &noop);
    }
    Some(ListItem {
        name: column.to_string(),
        included: true,
        presentation,
        kind: views::schema_role_kind(&spec.role)
            .or_else(|| kind_for_type(spec.ty))
            .map(str::to_string),
        note: None,
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

/// Render the dataset's whole presentation object, preserving other columns verbatim
/// and replacing the open column with its nondefault fields. Drop that column when no
/// keys remain; an empty dataset object becomes an overlay removal.
///
/// Only `columns` is retained at the dataset-object level. Unknown sibling keys are not
/// preserved. Other columns must survive because this document merges whole dataset
/// objects, so writing only the open column would erase them.
///
/// Fold the current fields into a clone before rendering, making this operation correct
/// even when the caller has not already folded the draft.
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

/// The Schema row's suffix: `" · k · 0 dp · delta"` when the dataset table sets
/// anything for this column, `""` otherwise.
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
            note: None,
        }
    }

    /// A stepped view field reports its current provenance before any write lands.
    fn ctx(door: ColumnDoor, layers: ColumnLayers, view: &ColumnPresentation) -> ColumnContext {
        let merged = {
            let mut p = layers.below_view();
            p.merge_over(view);
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
        };
        let c = ctx(
            ColumnDoor::View,
            layers,
            &ColumnPresentation {
                precision: Some(4),
                ..Default::default()
            },
        );
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
        let c = ctx(ColumnDoor::View, layers, &ColumnPresentation::default());
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
        let c = ctx(ColumnDoor::Dataset, layers, &ColumnPresentation::default());
        assert_eq!(
            provenance(&c, &field(&c, "width")),
            Some(Provenance::Dataset)
        );
        assert_eq!(provenance(&c, &field(&c, "label")), None);
    }

    /// Returning a view field to its baseline immediately restores the lower-layer
    /// badge; provenance must not depend on a saved view-overlay key.
    #[test]
    fn a_field_stepped_back_to_the_layer_below_reads_that_layer() {
        let layers = ColumnLayers {
            dataset: ColumnPresentation {
                scale: Some(Scale::Thousands),
                ..Default::default()
            },
            ..Default::default()
        };
        let c = ctx(
            ColumnDoor::View,
            layers,
            // The trader's own view-level override, as the doc holds it.
            &ColumnPresentation {
                scale: Some(Scale::Millions),
                ..Default::default()
            },
        );
        let mut scale = field(&c, "scale");
        assert_eq!(
            provenance(&c, &scale),
            Some(Provenance::View),
            "sanity: as opened, the field shows the view's own value"
        );
        if let FieldKind::Choice { options, selected } = &mut scale.kind {
            *selected = options
                .iter()
                .position(|o| o == views::scale_key(Scale::Thousands))
                .unwrap();
        }
        assert_eq!(
            provenance(&c, &scale),
            Some(Provenance::Dataset),
            "stepped back to the dataset level's own value, the chip \
             names the dataset — which is what the file will say 250 ms \
             later, since the writer omits every key equal to the layer \
             below"
        );
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

    /// A text key column receives text defaults despite lacking a view-column role.
    /// Numeric attributes retain numeric defaults without becoming aggregatable.
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
