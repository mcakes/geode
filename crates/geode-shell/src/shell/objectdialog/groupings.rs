//! The `Domain::Groupings` adapter (spec §8.2): the nine grouping slots
//! `ctrl+1`..`ctrl+9` regroup a following blotter tile by (spec §4.2).
//!
//! ## The object's own value is not a table
//!
//! `groupings.toml` stores each slot as `3 = ["book", "lhu"]`
//! (`geode-core`'s `groupings::GroupingSlots::from_doc`, whose own module
//! doc says it plainly: "the top-level key IS the slot number"). The
//! value is a bare array, never a `[3]` sub-table the way a view or a
//! source is — every other domain built so far stores its object as a
//! table, so this is the first adapter the scaffold's write pipeline had
//! to generalise for: [`super::Domain::to_table`] returns a
//! `toml_edit::Item` rather than a `toml_edit::Table`, and
//! [`super::set_object`], [`super::object_text`] and
//! `apply::object_value`/`apply::run_writes` all moved from `Table` to
//! `Item` to carry either shape without knowing which one they were
//! handed.
//!
//! ## `slot` is display-only
//!
//! The design spec sketches `slot` as an editable `Number`, 1–9, and
//! `name` as a `Text` field. Neither survives contact with
//! `GroupingSlots::from_doc`: there is no `name` at all — the object
//! carries nothing but its dimension chain — and the slot number is the
//! object's own **identity**, not a field inside it. Editing it would be
//! a rename (move the array to a different top-level key), and a
//! standing ruling on this plan forbids renames under instant-apply: a
//! per-keystroke rename would write a table per prefix as the trader
//! stepped a `Number` from 3 toward 9, orphaning slots 4 through 8 along
//! the way.
//!
//! So `slot` here is a [`super::FieldKind::Text`] field: painted, never
//! stepped (`Draft::step_selected` has no arm that changes a `Text`
//! value, so `space` on this row is correctly "nothing changes with
//! space"). Moving a grouping to a different slot number is a real want,
//! but it is a move operation on an object — the same shape a rename is —
//! and belongs with whatever later task builds renaming properly.
//!
//! ## Ticking and reordering is the whole edit
//!
//! Unlike Views' `columns`, whose `OrderedList` only ever lists the
//! view's own current members, `dimensions` here lists **every** column
//! [`crate::shell::pickable_columns`] would (the same vocabulary
//! `mod+p`'s dimension picker offers): the slot's own chain, in chain
//! order, ticked; every other pickable column after, unticked, in schema
//! order. Ticking one on adds it to the chain; `shift+j`/`shift+k`
//! reorder whichever items are ticked.

use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::schema::SchemaSpec;

use super::{Destination, Draft, Field, FieldKind, ListItem};

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = "groupings";

/// The muted second line of a slot's browse row: its dimension chain, in
/// the same "a / b / c" spelling [`GroupingSlots::label_of`] uses
/// everywhere a slot is named on screen (the blotter tile's own header
/// included) — "the grouping string a slot is known by everywhere",
/// in that function's own words.
///
/// Read straight off the raw TOML value rather than through
/// `GroupingSlots::from_doc`, for the reason `views::summary` gives for
/// doing the same: a malformed slot — an empty array, a name no dataset
/// or derived dimension declares — is exactly the one a user opens this
/// dialog to fix, and `from_doc` drops such a slot from the merged
/// result entirely (`groupings.rs`: "slot dropped"), which would leave
/// the row with nothing to show.
pub fn summary(value: &toml::Value) -> String {
    let Some(array) = value.as_array() else {
        return "not a list of columns".to_string();
    };
    let names: Vec<String> = array
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if names.is_empty() {
        "empty".to_string()
    } else {
        GroupingSlots::label_of(&names)
    }
}

/// The fields of one slot (spec §8.2), or of no slot at all when `object`
/// names nothing — an empty `dimensions` list ticking nothing, which is
/// what a slot number nothing defines has to produce rather than
/// panicking (spec §4 has `fields` serve the create path too).
///
/// The chain read for `object` comes straight off `config.doc(DOC)`'s raw
/// value, not through `GroupingSlots::from_doc` — the reader drops a slot
/// outright on one unknown column name, which would make the edit stage
/// blind to the very content a trader opened it to fix (the same reason
/// `views::fields` keeps a view's own dataset in its `Choice` even when
/// the schema no longer has it).
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let current: Vec<String> = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // The slot's own chain first, in chain order, ticked; then every
    // other pickable column, in schema order, unticked — the same
    // "already-a-member first, the rest of the catalogue after" shape
    // `views::columns_for` builds for its own ordered list.
    let mut items: Vec<ListItem> = current
        .iter()
        .map(|name| ListItem {
            name: name.clone(),
            included: true,
            width: None,
        })
        .collect();
    for column in crate::shell::pickable_columns(config) {
        if current.contains(&column.column) {
            continue;
        }
        items.push(ListItem {
            name: column.column,
            included: false,
            width: None,
        });
    }

    vec![
        Field {
            key: "slot".to_string(),
            label: "Slot".to_string(),
            kind: FieldKind::Text(object.unwrap_or("").to_string()),
            dest: Destination::Doc,
        },
        Field {
            key: "dimensions".to_string(),
            label: "Dimensions".to_string(),
            kind: FieldKind::OrderedList { items },
            dest: Destination::Doc,
        },
    ]
}

/// The draft rendered as `groupings.toml`'s own value for this slot: a
/// bare array of the ticked names, in list order.
///
/// Never a table — see this module's own doc comment — and `dest` is not
/// matched on the way `views::to_table` matches it, because every field
/// this domain has is [`Destination::Doc`]; there is only one destination
/// a Groupings draft can ever be asked to render.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    let mut array = toml_edit::Array::new();
    for item in draft.list_items("dimensions").unwrap_or_default() {
        if item.included {
            array.push(item.name.as_str());
        }
    }
    toml_edit::Item::Value(array.into())
}

/// Everything wrong with the draft as it stands (spec §7.2): the
/// rendered array, parsed back and read by exactly the reader that
/// decides what a following tile groups by (`GroupingSlots::from_doc`,
/// the same one `hot_reload::rebuild_slots` calls) — on the object being
/// edited alone, wrapped in a document of its own, for the reason
/// `views::validate` gives for doing the same: validating the whole
/// merged doc would report every other slot's problems against this
/// one object.
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
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    let (_slots, diags) = GroupingSlots::from_doc(&doc, &schema, &dims);
    diags
}

/// The draft's `groupings.toml` entry, rendered and parsed back the way
/// the loader would read it off disk.
fn rendered_doc_table(draft: &Draft) -> toml::Table {
    super::object_text(&draft.name, to_table(draft, Destination::Doc))
        .parse::<toml::Table>()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::{Domain, EditRow};
    use super::*;
    use geode_core::config::ConfigSources;

    fn value(text: &str) -> toml::Value {
        text.parse::<toml::Value>().expect("fixture value parses")
    }

    #[test]
    fn the_summary_names_the_dimension_chain() {
        assert_eq!(
            summary(&value("[\"underlying_ref\", \"book\", \"position_ref\"]")),
            "underlying_ref / book / position_ref"
        );
    }

    /// A malformed slot is exactly the one a user opens the dialog to
    /// fix, so it still gets a row and the row still says what is wrong
    /// — the same rule `views::summary`'s own test pins.
    #[test]
    fn a_malformed_slot_still_describes_itself() {
        assert_eq!(
            summary(&toml::Value::String("oops".into())),
            "not a list of columns"
        );
        assert_eq!(summary(&value("[]")), "empty");
    }

    /// A config with one dataset carrying two dimension columns and no
    /// `[dimensions]` doc — the plainest fixture `pickable_columns` can
    /// read, and the one `fields` is built against.
    fn config_with_slot(groupings: &str) -> Config {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
        )
        .unwrap();
        let groupings = LayerDoc::builtin("groupings", groupings).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![datasets, groupings],
            desk: None,
            user: None,
        })
    }

    /// `dimensions` offers the dataset's pickable columns (spec's own
    /// words for this task): the slot's own chain first, in chain order
    /// and ticked, then every other pickable column, unticked — here
    /// `book` is the only one left over once `lhu` and `position_ref`
    /// (a key column, never pickable) are accounted for.
    #[test]
    fn fields_offers_every_pickable_column_chain_first_then_the_rest() {
        let config = config_with_slot("3 = [\"lhu\"]\n");
        let fields = fields(&config, Some("3"));
        assert_eq!(fields[0].key, "slot");
        assert_eq!(fields[0].kind, FieldKind::Text("3".to_string()));
        let FieldKind::OrderedList { items } = &fields[1].kind else {
            panic!(
                "dimensions must be an ordered list, got {:?}",
                fields[1].kind
            );
        };
        assert_eq!(
            items
                .iter()
                .map(|i| (i.name.as_str(), i.included))
                .collect::<Vec<_>>(),
            vec![("lhu", true), ("book", false)],
            "the chain leads, ticked; the rest of the catalogue follows, unticked"
        );
    }

    /// `fields` takes `Option<&str>` because spec §4 has it serve the
    /// create path too, and a slot nothing defines has to come back as an
    /// empty chain rather than a panic.
    #[test]
    fn an_object_that_does_not_exist_has_empty_fields_rather_than_panicking() {
        let config = config_with_slot("3 = [\"lhu\"]\n");
        for object in [None, Some("9")] {
            let fields = fields(&config, object);
            let FieldKind::OrderedList { items } = &fields[1].kind else {
                panic!("dimensions must be an ordered list");
            };
            assert!(
                items.iter().all(|i| !i.included),
                "{object:?} should have nothing ticked, got {items:?}"
            );
        }
    }

    /// `to_table` renders only the ticked items — an unticked catalogue
    /// column sitting in the draft's `dimensions` list (there to be
    /// ticked on, per this module's own doc comment) must never reach
    /// `groupings.toml` just because it is present in the list.
    #[test]
    fn to_table_excludes_unticked_items() {
        let config = config_with_slot("3 = [\"lhu\"]\n");
        let draft = Domain::Groupings.draft(&config, "3");
        // `book` is on the list (every pickable column is), unticked.
        assert_eq!(
            draft
                .list_items("dimensions")
                .unwrap()
                .iter()
                .map(|i| (i.name.as_str(), i.included))
                .collect::<Vec<_>>(),
            vec![("lhu", true), ("book", false)]
        );
        let item = to_table(&draft, Destination::Doc);
        let toml_edit::Item::Value(value) = &item else {
            panic!("a Groupings object is always a bare value, got {item:?}");
        };
        let names: Vec<&str> = value
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(names, vec!["lhu"], "book is unticked and must not appear");
    }

    /// The round trip `Domain::validate` leans on: what this adapter
    /// renders has to be exactly what `GroupingSlots::from_doc` reads
    /// back, in the same order, or a round-trip mismatch would surface
    /// as a diagnostic against an edit that changed nothing.
    #[test]
    fn to_table_round_trips_through_grouping_slots_from_doc() {
        let config = config_with_slot("3 = [\"book\", \"lhu\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        // Onto the first `dimensions` item (`book`) and swap it past `lhu`
        // — the same reorder the end-to-end test performs through real
        // keystrokes.
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| matches!(r, EditRow::Item { .. }))
            .expect("the fixture slot has dimensions");
        assert!(draft.move_item(1), "book should move down past lhu");
        let item = to_table(&draft, Destination::Doc);
        let text = super::super::object_text("3", item);
        let table: toml::Table = text.parse().unwrap();
        let doc = merge_docs(
            DOC,
            &[LayerDoc {
                layer: Layer::User,
                name: DOC.to_string(),
                file: std::path::PathBuf::from("<test>"),
                table,
            }],
        );
        let (schema, _) = config
            .doc("datasets")
            .map(SchemaSpec::from_doc)
            .unwrap_or_default();
        let (dims, _) = config
            .doc("dimensions")
            .map(DerivedDimensions::from_doc)
            .unwrap_or_default();
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            slots.get(3),
            Some(&["lhu".to_string(), "book".to_string()][..]),
            "the reorder must survive the round trip"
        );
    }

    /// A key outside 1–9 is not this adapter's to police — the ruling
    /// that dropped the sketched `Number` field means there is no bound
    /// to check here at all. `GroupingSlots::from_doc` still catches it,
    /// as a `Warning` (not an `Error`, so it never blocks a commit): this
    /// pins that the adapter's `validate` faithfully surfaces that
    /// reader's own diagnostic rather than silently accepting it.
    #[test]
    fn an_out_of_range_slot_key_still_warns_through_validate() {
        let config = config_with_slot("10 = [\"book\"]\n");
        let draft = Domain::Groupings.draft(&config, "10");
        assert_eq!(draft.diagnostics.len(), 1, "{:?}", draft.diagnostics);
        assert_eq!(
            draft.diagnostics[0].severity,
            geode_core::config::Severity::Warning
        );
        assert!(draft.diagnostics[0].message.contains("10"));
    }

    /// The shared walk every domain gets for free (`Domain::objects`,
    /// `derive_rows`): a slot's key IS its name, and the provenance
    /// markers work over it exactly as they do over a view's name.
    #[test]
    fn domain_groupings_lists_slots_with_their_owning_layer() {
        let builtin = LayerDoc::builtin("groupings", "3 = [\"book\"]\n4 = [\"lhu\"]\n").unwrap();
        let user = LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: "<test:user>".into(),
            table: "3 = [\"lhu\", \"book\"]\n".parse().unwrap(),
        };
        let config = Config::load(&ConfigSources {
            builtin: vec![builtin, user],
            desk: None,
            user: None,
        });
        let rows = Domain::Groupings.objects(&config);
        let three = rows.iter().find(|r| r.name == "3").expect("slot 3");
        let four = rows.iter().find(|r| r.name == "4").expect("slot 4");
        assert_eq!(three.layer, Some(Layer::User));
        assert!(three.overridden);
        assert_eq!(four.layer, Some(Layer::Builtin));
        assert!(!four.overridden);
    }
}
