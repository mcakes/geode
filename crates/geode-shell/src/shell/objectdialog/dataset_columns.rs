//! The Schema dialog's column stage — the dataset-level door into the
//! same seven-field stage the Views dialog has (dataset-presentation
//! spec §4). Pure: no gpui.

use super::views;
use super::{ColumnContext, ColumnDoor, Field, FieldKind, Provenance};
use geode_core::view::ColumnPresentation;

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
pub fn provenance_of(ctx: &ColumnContext, field: &Field) -> Option<Provenance> {
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
    let differs_from = |p: &ColumnPresentation, kind: &geode_core::view::ColumnFormat| {
        let effective = kind.clone().with(p);
        match (key, &field.kind) {
            ("label", FieldKind::Text(t)) => t.trim() != p.label.as_deref().unwrap_or(""),
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
    let kind = ctx
        .item
        .as_ref()
        .map(views::kind_default)
        .unwrap_or(geode_core::view::ColumnFormat::MEASURE);
    match ctx.door {
        ColumnDoor::Dataset => {
            let unset = ColumnPresentation::default();
            (differs_from(&unset, &kind) || set_in(&ctx.layers.dataset))
                .then_some(Provenance::Dataset)
        }
        ColumnDoor::View => {
            let below = ctx.layers.below_view();
            if differs_from(&below, &kind) || set_in(&ctx.layers.view) {
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
        assert_eq!(
            provenance_of(&c, &field(&c, "label")),
            Some(Provenance::Desk)
        );
        assert_eq!(
            provenance_of(&c, &field(&c, "scale")),
            Some(Provenance::Dataset)
        );
        assert_eq!(
            provenance_of(&c, &field(&c, "precision")),
            Some(Provenance::View)
        );
        assert_eq!(
            provenance_of(&c, &field(&c, "colour")),
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
        assert_eq!(provenance_of(&c, &scale), Some(Provenance::View));
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
            provenance_of(&c, &field(&c, "width")),
            Some(Provenance::Dataset)
        );
        assert_eq!(provenance_of(&c, &field(&c, "label")), None);
    }
}
