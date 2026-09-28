//! Adapter for the nine numbered grouping slots used by `ctrl+1` through `ctrl+9`. Each
//! object is a bare array of dimension names, such as `3 = ["book", "lhu"]`, rather
//! than a table. The slot number is its identity and is displayed read-only; the dialog
//! does not rename or move slots.
//!
//! The editor shows the configured chain first, ticked and in chain order, then other
//! groupable columns unticked. This vocabulary includes grain keys and other groupable
//! dimensions; it differs from the categorical picker vocabulary. Ticking adds a
//! dimension, and reordering changes chain order. The chain text field accepts slash or
//! whitespace separators and validates names and duplicates.
//!
//! There is no separate available catalogue: `available: None` makes ticking the
//! membership operation, and `x` refuses with guidance to use `space`. Every edit
//! writes the grouping definition. The final dimension cannot be unticked because an
//! empty grouping definition cannot express an active empty slot.

use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::schema::SchemaSpec;
use geode_core::view::ColumnPresentation;

use super::{Destination, Draft, Field, FieldKind, ListItem, Step};

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

/// Build a slot's display-only identity and dimensions list. A missing slot uses empty
/// configuration. Configured dimensions lead in chain order; other groupable columns
/// follow unticked. All writable values target the definition.
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
    // other pickable column, in schema order, unticked — all of them the
    // object's OWN items, because ticking is the whole of membership
    // here (this module's own doc comment).
    let mut items: Vec<ListItem> = current
        .iter()
        .map(|name| ListItem {
            name: name.clone(),
            included: true,
            presentation: ColumnPresentation::default(),
            // No column-kind concept on this list at all — `kind` exists
            // only for Views' `columns_for` to fill in a brand new
            // `[[columns]]` entry, and a grouping slot's value is a bare
            // array, never a table with a `kind` key.
            kind: None,
            note: None,
        })
        .collect();
    for column in crate::shell::groupable_columns(config) {
        if current.contains(&column.column) {
            continue;
        }
        items.push(ListItem {
            name: column.column,
            included: false,
            presentation: ColumnPresentation::default(),
            kind: None,
            note: None,
        });
    }

    vec![
        Field {
            key: "slot".to_string(),
            label: "Slot".to_string(),
            kind: FieldKind::Text(object.unwrap_or("").to_string()),
            dest: Destination::Doc,
            layer: None,
        },
        Field {
            key: "dimensions".to_string(),
            label: "Dimensions".to_string(),
            kind: FieldKind::OrderedList {
                items,
                // No catalogue, not an empty one — see this module's doc.
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        },
    ]
}

/// The `dimensions` field's key — the one list the chain field edits.
const DIMENSIONS: &str = "dimensions";

/// A chain separator: `/`, as [`GroupingSlots::label_of`] spells the chain everywhere
/// it is shown, or any whitespace, so `book lhu desk` is as good as `book / lhu /
/// desk`.
fn is_separator(c: char) -> bool {
    c == '/' || c.is_whitespace()
}

/// The names a chain field's text spells, in typed order: split on
/// [`is_separator`], with runs of separators counting as one and a
/// leading or trailing one naming no segment.
pub fn parse_chain(text: &str) -> Vec<String> {
    text.split(is_separator)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The segment being typed — whatever follows the last separator, or the
/// whole text when there is none. What the completion list ranks by.
pub fn trailing_segment(text: &str) -> &str {
    text.rsplit(is_separator).next().unwrap_or("")
}

/// The names typed *before* the trailing segment: the part of the chain
/// already decided, which the completion list therefore stops offering.
fn completed_names(text: &str) -> Vec<String> {
    let head = &text[..text.len() - trailing_segment(text).len()];
    parse_chain(head)
}

/// Visible chain completions ranked against the trailing typed segment, excluding names
/// already entered. No field header participates in completion.
pub fn chain_candidates(draft: &Draft) -> Vec<crate::listfilter::Ranked> {
    let rows = draft.rows();
    let labels: Vec<String> = rows.iter().map(|r| draft.row_label(*r)).collect();
    let done = completed_names(&draft.query);
    let mut ranked = crate::listfilter::rank(&labels, trailing_segment(&draft.query));
    ranked.retain(|m| {
        let is_dimension = matches!(
            rows[m.row],
            super::EditRow::Item { field, .. } if draft.fields[field].key == DIMENSIONS
        );
        is_dimension && !done.contains(&labels[m.row])
    });
    ranked.sort_by_key(|m| m.row);
    ranked
}

impl Draft {
    /// `i`: open the chain field, seeded with the slot's current chain in the spelling
    /// every other surface uses, so appending is a separator and a name away. Pure —
    /// the mode switch that gives the shared `Input` the keys is the handler's, and the
    /// sync writes the field from `query` on its return.
    pub fn begin_chain_entry(&mut self) {
        let Some(field) = self.fields.iter().position(|f| f.key == DIMENSIONS) else {
            return;
        };
        let names: Vec<String> = self
            .list_items(DIMENSIONS)
            .unwrap_or_default()
            .iter()
            .filter(|i| i.included)
            .map(|i| i.name.clone())
            .collect();
        self.query = if names.is_empty() {
            String::new()
        } else {
            GroupingSlots::label_of(&names)
        };
        self.text_entry = Some(super::TextEntry {
            row: super::EditRow::Field(field),
            completions: super::Completions::Chain,
        });
        self.selected = 0;
    }

    /// `escape` in the chain field — [`Draft::cancel_text_entry`] under
    /// the name the chain tests use.
    pub fn cancel_chain_entry(&mut self) {
        self.cancel_text_entry();
    }

    /// `tab` in the chain field: replace the trailing segment with the
    /// highlighted candidate and open the next segment with the
    /// canonical ` / `. `false` when nothing is highlighted — every name
    /// already typed, or a segment nothing matches — leaving the text
    /// untouched.
    pub fn complete_chain(&mut self) -> bool {
        let rows = self.rows();
        let Some(row) = self.visible_rows().get(self.selected).map(|m| m.row) else {
            return false;
        };
        let mut names = completed_names(&self.query);
        names.push(self.row_label(rows[row]));
        self.query = format!("{} / ", GroupingSlots::label_of(&names));
        self.selected = 0;
        true
    }

    /// `enter` in the chain field: the typed names become the chain, in
    /// typed order — ticked and first, every other item after, unticked
    /// — and the field closes. [`Step::Refused`] keeps the field open
    /// with the text intact, so a typo is fixed rather than retyped:
    /// an empty chain (the state the config model cannot hold — the
    /// same words `Draft::step_selected` refuses the last untick with),
    /// a name twice, or a name no dataset carries as a dimension.
    /// [`Step::Inert`] when the chain typed back is the one already
    /// there: nothing to write, and the field still closes — closing is
    /// the visible answer.
    pub fn apply_chain(&mut self) -> Step {
        let names = parse_chain(&self.query);
        let Some(field) = self.fields.iter().position(|f| f.key == DIMENSIONS) else {
            return Step::Inert;
        };
        let label = self.fields[field].label.clone();
        if names.is_empty() {
            return Step::Refused(format!("{label} must keep at least one entry"));
        }
        if let Some(twice) = names
            .iter()
            .enumerate()
            .find(|(i, name)| names[..*i].contains(name))
        {
            return Step::Refused(format!("'{}' is listed twice", twice.1));
        }
        // Groupings has one list with ticked membership, so chain entry edits `items`.
        let FieldKind::OrderedList { items, .. } = &mut self.fields[field].kind else {
            return Step::Inert;
        };
        if let Some(unknown) = names.iter().find(|n| !items.iter().any(|i| &i.name == *n)) {
            return Step::Refused(format!(
                "'{unknown}' is not a dimension any dataset carries"
            ));
        }
        let before: Vec<String> = items
            .iter()
            .filter(|i| i.included)
            .map(|i| i.name.clone())
            .collect();
        // Decided before the list is touched, and the list is touched
        // only on a change: rewriting it as "typed names, then the rest"
        // would re-sort an order `shift+j`/`shift+k` had put an unticked
        // item into, leaving the draft dirty under an answer of "inert".
        if before == names {
            self.text_entry = None;
            self.query.clear();
            self.selected = 0;
            return Step::Inert;
        }
        let mut rest = std::mem::take(items);
        let mut out = Vec::with_capacity(rest.len());
        for name in &names {
            let pos = rest
                .iter()
                .position(|i| &i.name == name)
                .expect("every typed name was checked against the list above");
            let mut item = rest.remove(pos);
            item.included = true;
            out.push(item);
        }
        for mut item in rest {
            item.included = false;
            out.push(item);
        }
        *items = out;
        self.text_entry = None;
        self.query.clear();
        self.selected = 0;
        Step::Changed
    }
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

/// Everything wrong with the draft as it stands: the rendered array, parsed back and
/// read by exactly the reader that decides what a following tile groups by
/// (`GroupingSlots::from_doc`, the same one `hot_reload::rebuild_slots` calls) — on the
/// object being edited alone, wrapped in a document of its own, for the reason
/// `views::validate` gives for doing the same: validating the whole merged doc would
/// report every other slot's problems against this one object.
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

/// What each field means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)). No chord in the sentence:
/// the title crumb already paints `ctrl+N`, and a slot regroups every
/// tile following the shared frame, not one blotter (`frame::slot_N`).
pub fn help(key: &str) -> &'static str {
    match key {
        "slot" => "The slot's number — activating it regroups every tile following the frame",
        "dimensions" => {
            "The group-by chain, outermost first — every column the dataset can group by"
        }
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Domain, EditRow, Step};
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

    /// A config with one dataset carrying two dimension columns, a key
    /// column and one position-grain measure (so the position grain is
    /// *declared* — `groupable_columns` offers nothing from a dataset with
    /// no grain to scan) and no `[dimensions]` doc — the plainest fixture
    /// `groupable_columns` can read, and the one `fields` is built against.
    fn config_with_slot(groupings: &str) -> Config {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let groupings = LayerDoc::builtin("groupings", groupings).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![datasets, groupings],
            desk: None,
            user: None,
        })
    }

    /// `dimensions` offers the dataset's groupable columns: the slot's own
    /// chain first, in chain order and ticked, then every other groupable
    /// column, unticked, in schema order — `position_ref` included, a key
    /// column the compiler groups by happily even though the dimension
    /// picker (categorical columns only) never offers it.
    #[test]
    fn fields_offers_every_groupable_column_chain_first_then_the_rest() {
        let config = config_with_slot("3 = [\"lhu\"]\n");
        let fields = fields(&config, Some("3"));
        assert_eq!(fields[0].key, "slot");
        assert_eq!(fields[0].kind, FieldKind::Text("3".to_string()));
        let FieldKind::OrderedList { items, available } = &fields[1].kind else {
            panic!(
                "dimensions must be an ordered list, got {:?}",
                fields[1].kind
            );
        };
        assert!(
            available.is_none(),
            "ticking IS membership here, so there is no catalogue to \
             promote out of, got {available:?}"
        );
        assert_eq!(
            items
                .iter()
                .map(|i| (i.name.as_str(), i.included))
                .collect::<Vec<_>>(),
            vec![("lhu", true), ("book", false), ("position_ref", false)],
            "the chain leads, ticked; the rest of the catalogue follows, unticked"
        );
    }

    /// The optional object name supports both existing slots and an empty draft.
    #[test]
    fn an_object_that_does_not_exist_has_empty_fields_rather_than_panicking() {
        let config = config_with_slot("3 = [\"lhu\"]\n");
        for object in [None, Some("9")] {
            let fields = fields(&config, object);
            let FieldKind::OrderedList { items, .. } = &fields[1].kind else {
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
        // `book` and `position_ref` are on the list (every groupable
        // column is), unticked.
        assert_eq!(
            draft
                .list_items("dimensions")
                .unwrap()
                .iter()
                .map(|i| (i.name.as_str(), i.included))
                .collect::<Vec<_>>(),
            vec![("lhu", true), ("book", false), ("position_ref", false)]
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
        assert!(
            draft.move_item(1).is_some(),
            "book should move down past lhu"
        );
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

    /// Out-of-range slot keys are reported by the core reader as warnings. The adapter
    /// exposes that diagnostic and does not add a separate bound check.
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

    // ---- Chain entry -------------------------------------------

    /// Three groupable dimensions allow reorder, extension, and duplicate checks. The
    /// fixture declares the grain key required for its dataset to remain valid.
    fn config_with_three_dims(groupings: &str) -> Config {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.desk]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let groupings = LayerDoc::builtin("groupings", groupings).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![datasets, groupings],
            desk: None,
            user: None,
        })
    }

    fn ticked(draft: &Draft) -> Vec<(String, bool)> {
        draft
            .list_items("dimensions")
            .unwrap()
            .iter()
            .map(|i| (i.name.clone(), i.included))
            .collect()
    }

    fn candidate_names(draft: &Draft) -> Vec<String> {
        let rows = draft.rows();
        draft
            .visible_rows()
            .iter()
            .map(|m| draft.row_label(rows[m.row]))
            .collect()
    }

    /// Slash and whitespace both separate dimension names, preserving typed order.
    #[test]
    fn parse_chain_accepts_slash_and_space_separators() {
        for text in [
            "book / lhu",
            "book lhu",
            "book/lhu",
            "  book  /  lhu  ",
            "book // lhu",
        ] {
            assert_eq!(parse_chain(text), vec!["book", "lhu"], "{text:?}");
        }
        assert_eq!(parse_chain(""), Vec::<String>::new());
        assert_eq!(parse_chain(" / "), Vec::<String>::new());
    }

    /// The segment the completion list ranks by is whatever follows the
    /// last separator — empty right after one, so every remaining
    /// candidate shows.
    #[test]
    fn the_trailing_segment_is_what_follows_the_last_separator() {
        assert_eq!(trailing_segment("book / l"), "l");
        assert_eq!(trailing_segment("book l"), "l");
        assert_eq!(trailing_segment("book / "), "");
        assert_eq!(trailing_segment("book"), "book");
        assert_eq!(trailing_segment(""), "");
    }

    /// `i` seeds the field with the slot's current chain in the same
    /// spelling the browse row and the blotter header use, so a trader
    /// who wants to append types a separator and a name and nothing
    /// else.
    #[test]
    fn beginning_chain_entry_seeds_the_current_chain() {
        let config = config_with_three_dims("3 = [\"book\", \"lhu\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        assert!(!draft.chain_entry());
        draft.begin_chain_entry();
        assert!(draft.chain_entry());
        assert_eq!(draft.query, "book / lhu");
        assert_eq!(draft.selected, 0);
    }

    /// While the field is open the row list IS the completion list:
    /// only item rows, ranked by the trailing segment, minus every name
    /// already typed before it, in row order.
    #[test]
    fn chain_candidates_rank_the_trailing_segment_and_skip_completed_names() {
        let config = config_with_three_dims("3 = [\"book\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.begin_chain_entry();
        draft.query = String::new();
        assert_eq!(candidate_names(&draft), vec!["book", "lhu", "desk"]);
        draft.query = "book / ".to_string();
        assert_eq!(candidate_names(&draft), vec!["lhu", "desk"]);
        draft.query = "book / d".to_string();
        assert_eq!(candidate_names(&draft), vec!["desk"]);
        draft.query = "book / zzz".to_string();
        assert!(candidate_names(&draft).is_empty());
    }

    /// `tab` replaces the trailing segment with the highlighted
    /// candidate and opens the next segment with the canonical ` / `.
    #[test]
    fn completing_replaces_the_trailing_segment_with_the_highlighted_candidate() {
        let config = config_with_three_dims("3 = [\"book\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.begin_chain_entry();
        draft.query = "book d".to_string();
        draft.selected = 0;
        assert!(draft.complete_chain());
        assert_eq!(draft.query, "book / desk / ");
        // Nothing left to complete once every name is typed.
        draft.query = "book / desk / lhu / ".to_string();
        assert!(!draft.complete_chain());
        assert_eq!(draft.query, "book / desk / lhu / ", "unchanged");
    }

    /// `enter` makes the typed names the chain, in typed order, and
    /// closes the field.
    #[test]
    fn applying_a_chain_ticks_the_typed_names_in_typed_order() {
        let config = config_with_three_dims("3 = [\"book\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.begin_chain_entry();
        draft.query = "desk lhu".to_string();
        assert_eq!(draft.apply_chain(), Step::Changed);
        assert_eq!(
            ticked(&draft),
            vec![
                ("desk".to_string(), true),
                ("lhu".to_string(), true),
                ("book".to_string(), false)
            ]
        );
        assert!(!draft.chain_entry());
        assert_eq!(draft.query, "");
        assert!(draft.is_dirty());
    }

    /// A name no dataset carries, or one typed twice, is refused with
    /// the name in the reason — and the field stays open with the text
    /// intact so the trader can fix it rather than retype it.
    #[test]
    fn applying_refuses_an_unknown_or_duplicated_name_and_stays_open() {
        let config = config_with_three_dims("3 = [\"book\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.begin_chain_entry();
        draft.query = "book / npv".to_string();
        let Step::Refused(reason) = draft.apply_chain() else {
            panic!("an unknown name must be refused");
        };
        assert!(reason.contains("npv"), "{reason}");
        assert!(draft.chain_entry());
        assert_eq!(draft.query, "book / npv");
        assert_eq!(ticked(&draft)[0], ("book".to_string(), true), "untouched");

        draft.query = "book lhu book".to_string();
        let Step::Refused(reason) = draft.apply_chain() else {
            panic!("a duplicate must be refused");
        };
        assert!(
            reason.contains("book") && reason.contains("twice"),
            "{reason}"
        );
        assert!(draft.chain_entry());
    }

    /// An empty chain is the state the config model cannot hold
    /// (`Draft::step_selected`'s own `Destination::Doc` rule), so it is
    /// refused with the same words unticking the last dimension gets.
    #[test]
    fn applying_an_empty_chain_is_refused() {
        let config = config_with_three_dims("3 = [\"book\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.begin_chain_entry();
        draft.query = " / ".to_string();
        let Step::Refused(reason) = draft.apply_chain() else {
            panic!("an empty chain must be refused");
        };
        assert!(reason.contains("at least one"), "{reason}");
        assert!(draft.chain_entry());
    }

    /// The same chain typed back is nothing to write: inert, and the
    /// field still closes — closing is the visible answer.
    #[test]
    fn applying_the_unchanged_chain_is_inert_and_closes_the_field() {
        let config = config_with_three_dims("3 = [\"book\", \"lhu\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.begin_chain_entry();
        assert_eq!(draft.apply_chain(), Step::Inert);
        assert!(!draft.chain_entry());
        assert!(!draft.is_dirty());
    }

    /// An inert apply must not touch the list either: `shift+k` can put an unticked
    /// item above a ticked one (every Groupings row is a member, so `MoveItem` is live
    /// on all of them), and typing the same chain back would otherwise re-sort the list
    /// into "typed names first" — a dirty draft the handler was just told was inert.
    #[test]
    fn an_inert_apply_leaves_a_non_canonical_list_order_alone() {
        let config = config_with_three_dims("3 = [\"book\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        // Onto `lhu` (rows: Slot, Dimensions, book, lhu, desk) and above
        // `book`; then treat that order as the saved one.
        draft.selected = 3;
        assert_eq!(draft.move_item(-1), Some(0));
        draft.mark_saved();
        assert_eq!(ticked(&draft)[0].0, "lhu");
        assert!(!draft.is_dirty());

        draft.begin_chain_entry();
        assert_eq!(draft.query, "book");
        assert_eq!(draft.apply_chain(), Step::Inert);
        assert!(!draft.is_dirty(), "inert means nothing moved");
        assert_eq!(ticked(&draft)[0].0, "lhu", "the list order is untouched");
    }

    /// `escape` drops the text and the field; the chain is as it was.
    #[test]
    fn cancelling_chain_entry_restores_the_rows_and_clears_the_text() {
        let config = config_with_three_dims("3 = [\"book\"]\n");
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.begin_chain_entry();
        draft.query = "desk".to_string();
        draft.cancel_chain_entry();
        assert!(!draft.chain_entry());
        assert_eq!(draft.query, "");
        assert_eq!(
            candidate_names(&draft).len(),
            5,
            "Slot, Dimensions and three items"
        );
        assert!(!draft.is_dirty());
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
