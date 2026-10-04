//! Dataset-level column presentation for the Schema dialog. This pure adapter seeds a
//! scratch column item, renders its dataset overlay, and supplies row summaries. Both
//! Schema and Views use its provenance calculation.
//!
//! Writes target `dataset_presentation.toml`. They retain other columns of the same
//! dataset because a user-layer dataset object replaces that object whole.

use super::views;
use super::{ColumnContext, ColumnDoor, Draft, Field, ListItem, Provenance};
use geode_core::config::Config;
use geode_core::schema::{ColumnType, DatasetSpec};
use geode_core::view::ColumnPresentation;

/// The doc this door writes: `dataset_presentation.toml`, user layer.
/// Named here rather than spelled again, the same way `views::DOC` and
/// `views::PRESENTATION_DOC` are the one spelling for theirs.
pub const DOC: &str = geode_core::view::DATASET_PRESENTATION_DOC;

/// Per-column inputs shared by all seven provenance checks in one render: the open
/// door's context and which keys the column sets at the stage's layer.
pub struct ProvenanceInputs<'a> {
    pub ctx: &'a ColumnContext,
    pub set: views::PresentationKeys,
}

impl<'a> ProvenanceInputs<'a> {
    pub fn new(ctx: &'a ColumnContext, set: views::PresentationKeys) -> ProvenanceInputs<'a> {
        ProvenanceInputs { ctx, set }
    }
}

/// The layer a column-stage field's value comes from. A set field is the stage's own
/// layer (`View`, or `Dataset` from the Schema door) whatever its value — a pin equal to
/// its parent included. An inherited Views field names the layer that sets the key:
/// the dataset level above the desk view's definition, or nothing for the kind default.
/// An inherited Schema field has no badge: below the dataset level each view resolves
/// its own value.
pub fn provenance_of(inputs: &ProvenanceInputs, field: &Field) -> Option<Provenance> {
    let ctx = inputs.ctx;
    let key = field.key.as_str();
    match ctx.door {
        ColumnDoor::Dataset => inputs.set.has(key).then_some(Provenance::Dataset),
        ColumnDoor::View if inputs.set.has(key) => Some(Provenance::View),
        ColumnDoor::View if views::sets(&ctx.layers.dataset, key) => Some(Provenance::Dataset),
        ColumnDoor::View if views::sets(&ctx.layers.desk, key) => Some(Provenance::Desk),
        ColumnDoor::View => None,
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
    })
}

/// The `kind` a column whose ROLE names none should be presented as — the
/// fallback [`item_for`]'s doc explains, for `key` and `attribute`.
///
/// A non-numeric column is `"dimension"`, so `views::kind_default` gives
/// it [`ColumnFormat::TEXT`](geode_core::view::ColumnFormat::TEXT): precision 0,
/// no thousands separator, no color — the format a text column actually has
/// in a blotter. A numeric one answers `None` and keeps the MEASURE default, which is right for
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
/// and replacing the open column with the keys it sets at the dataset level. Drop that column when no
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
        let mut set = draft
            .presentation_set
            .get(open)
            .copied()
            .unwrap_or_default();
        views::fold_into(
            &mut folded,
            &draft.fields,
            &ColumnPresentation::default(),
            &mut set,
        );
        let t = views::set_keys_table(&folded.presentation, set);
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
    use super::super::{ColumnLayers, Destination, FieldKind, ListItem};
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

    fn provenance(ctx: &ColumnContext, set: &[&str], field: &Field) -> Option<Provenance> {
        let mut keys = views::PresentationKeys::default();
        for key in set {
            keys.set(key, true);
        }
        provenance_of(&ProvenanceInputs::new(ctx, keys), field)
    }

    /// A set field reads the stage's own layer; an inherited one names the layer that
    /// sets it, or nothing for the kind default.
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
        let set = ["precision"];
        assert_eq!(
            provenance(&c, &set, &field(&c, "label")),
            Some(Provenance::Desk)
        );
        assert_eq!(
            provenance(&c, &set, &field(&c, "scale")),
            Some(Provenance::Dataset)
        );
        assert_eq!(
            provenance(&c, &set, &field(&c, "precision")),
            Some(Provenance::View)
        );
        assert_eq!(
            provenance(&c, &set, &field(&c, "color")),
            None,
            "kind default: no chip"
        );
    }

    /// A pin equal to its parent is still the view's own value.
    #[test]
    fn a_set_field_reads_view_even_when_equal_to_its_parent() {
        let layers = ColumnLayers {
            dataset: ColumnPresentation {
                scale: Some(Scale::Thousands),
                ..Default::default()
            },
            ..Default::default()
        };
        let c = ctx(ColumnDoor::View, layers, &ColumnPresentation::default());
        assert_eq!(
            provenance(&c, &["scale"], &field(&c, "scale")),
            Some(Provenance::View)
        );
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
            provenance(&c, &["width"], &field(&c, "width")),
            Some(Provenance::Dataset)
        );
        assert_eq!(provenance(&c, &["width"], &field(&c, "label")), None);
    }

    /// Provenance is the set, not the value: an inherited field names its source
    /// whatever it happens to show.
    #[test]
    fn an_inherited_field_names_its_source_whatever_it_shows() {
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
        assert_eq!(provenance(&c, &[], &scale), Some(Provenance::Dataset));
    }
}

#[cfg(test)]
mod writer_tests {
    use super::super::{ColumnLayers, Destination, Draft, FieldKind};
    use super::*;
    use geode_core::view::ColumnFormat;
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
    fn schema_writer_keeps_other_columns_and_writes_only_set_keys() {
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
        // Width set on disk: kept. Thousands stepped: written. Precision pinned at
        // the kind default (2): written. Scale inherited: not written.
        for f in &mut draft.fields {
            if f.key == "thousands" {
                f.kind = FieldKind::Bool(false);
            }
        }
        let mut set = views::PresentationKeys::default();
        for key in ["width", "thousands", "precision"] {
            set.set(key, true);
        }
        draft.presentation_set.insert("npv".into(), set);
        draft.column = Some("npv".into());
        let text = written(table(&draft));
        assert!(
            text.contains("[risk.columns.book]") && text.contains("label = \"Book\""),
            "{text}"
        );
        assert!(text.contains("[risk.columns.npv]"), "{text}");
        assert!(text.contains("width = 90"), "{text}");
        assert!(text.contains("thousands = false"), "{text}");
        assert!(text.contains("precision = 2"), "a pinned default: {text}");
        assert!(!text.contains("scale"), "{text}");
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
        let mut set = views::PresentationKeys::default();
        set.set("width", true);
        draft.presentation_set.insert("npv".into(), set);
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
