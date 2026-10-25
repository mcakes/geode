//! The Vol views domain: `vol_views.toml`, the vol watchlist's tabs. A
//! view is a name, `cells` and an ordered list of columns; each column is
//! one measure chosen by family, tenor, delta or span, with label, group
//! and two glyphs, held as the column's own TOML table on its list item
//! (`ListItem::source`) so the writer never re-derives what the pickers
//! said. The catalogue is the grammar's families plus every history
//! dataset keyed by the underlying, so an analytic appears the moment its
//! dataset does.
//!
//! Every field is `Destination::Doc`: a column's label, group and glyphs
//! live in the view's own table, so there is no presentation overlay for
//! this domain. The `name` row is display-only — the scaffold has no path
//! that renames an object, and a typed name the writer could not honour
//! would fork the inherited copy for nothing.

use std::collections::BTreeSet;

use geode_core::config::{
    Config, Diagnostic, Layer, LayerDoc, Severity, VOL_VIEWS_DOC, merge_docs,
};
use geode_core::schema::SchemaSpec;
use geode_core::volviews::{
    self, Cells, ColumnChoice, Family, Raw, TableShape, VolColumnSpec, VolViewSpec,
};

use super::{Destination, Domain, Draft, Field, FieldKind, ListItem};

pub const DOC: &str = VOL_VIEWS_DOC;

/// The browse summary of the view named `name`, reading whether it is the
/// document's `default` from the merged doc.
pub fn summary_in(config: &Config, name: &str, value: &toml::Value) -> String {
    let is_default = config
        .doc(DOC)
        .and_then(|doc| doc.value.get("default"))
        .and_then(|v| v.as_str())
        == Some(name);
    summary(value, is_default)
}

/// `pair · 10 columns`, with ` · default` when `name` is the document's default.
pub fn summary(value: &toml::Value, is_default: bool) -> String {
    let cells = match value.get("cells").and_then(|v| v.as_str()) {
        Some("single") => "single",
        _ => "pair",
    };
    let n = value
        .get("columns")
        .and_then(|v| v.as_array())
        .map_or(0, |a| a.len());
    let mut s = format!("{cells} · {n} columns");
    if is_default {
        s.push_str(" · default");
    }
    s
}

/// The view's fields: `name`, `cells`, `default` and its ordered `columns`
/// over the catalogue. A view the doc does not hold (one being created)
/// opens with no columns and pair cells.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let (doc, _) = config.doc(DOC).map(volviews::from_doc).unwrap_or_default();
    let view = object.and_then(|name| doc.views.iter().find(|v| v.name == name));
    let name = object.unwrap_or_default().to_string();
    let cells = match view.map(|v| v.cells) {
        Some(Cells::Single) => "single",
        _ => "pair",
    };
    let is_default = object.is_some() && doc.default.as_deref() == object;
    let mut taken: Vec<String> = Vec::new();
    let items: Vec<ListItem> = view
        .map(|v| {
            v.columns
                .iter()
                .map(|c| {
                    let choice = ColumnChoice::from_spec(c);
                    let name = item_name(choice.as_ref(), &taken);
                    taken.push(name.clone());
                    ListItem {
                        name,
                        included: true,
                        presentation: Default::default(),
                        kind: None,
                        source: Some(column_source(c)),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    vec![
        super::views::text_row("name", "Name", name, Destination::Doc),
        super::views::choice_row(
            "cells",
            "Cells",
            vec!["pair".into(), "single".into()],
            cells,
            Destination::Doc,
        ),
        Field {
            key: "default".into(),
            label: "Default".into(),
            kind: FieldKind::Bool(is_default),
            dest: Destination::Doc,
            layer: None,
        },
        Field {
            key: "columns".into(),
            label: "Columns".into(),
            kind: FieldKind::OrderedList {
                items,
                available: Some(catalogue(config)),
            },
            dest: Destination::Doc,
            layer: None,
        },
    ]
}

/// One column's `[[columns]]` table as `toml::Table`, through the core writer.
fn column_source(spec: &VolColumnSpec) -> toml::Table {
    let view = VolViewSpec {
        name: "c".into(),
        cells: Cells::Pair,
        columns: vec![spec.clone()],
    };
    let text = super::object_text("c", volviews::to_table(&view, None));
    let parsed: toml::Table = text.parse().unwrap_or_default();
    parsed
        .get("c")
        .and_then(|v| v.get("columns"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default()
}

/// A column's spec read back from its item table (a `Raw` keeps its text:
/// the reader accepts any open-form suffix, so `smile_curv 3M 0.25` comes
/// back as a `Table` measure and is written as it was read).
pub(super) fn spec_of(source: &toml::Table) -> Option<VolColumnSpec> {
    let mut view = toml::Table::new();
    view.insert(
        "columns".into(),
        toml::Value::Array(vec![toml::Value::Table(source.clone())]),
    );
    let mut root = toml::Table::new();
    root.insert("config_version".into(), toml::Value::Integer(1));
    root.insert("c".into(), toml::Value::Table(view));
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.into(),
            file: "<item>".into(),
            table: root,
        }],
    );
    let (parsed, _) = volviews::from_doc(&doc);
    parsed.views.into_iter().next()?.columns.into_iter().next()
}

/// Each open-form dataset — a `history` dataset keyed by `underlying_ref`
/// and at most one more column, with one value column — by name, with the
/// count of key columns after the underlying (0 or 1). Sorted by name.
fn open_form_datasets(config: &Config) -> Vec<(String, usize)> {
    let schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    let mut out: Vec<(String, usize)> = schema
        .datasets
        .iter()
        .filter(|d| d.is_history())
        .filter(|d| {
            d.key.first().map(String::as_str) == Some("underlying_ref")
                && d.key.len() <= 2
                && d.history_values().len() == 1
        })
        .map(|d| (d.name.clone(), d.key.len() - 1))
        .collect();
    out.sort();
    out
}

/// The catalogue's families in its order: the twelve builtin families, then
/// each open-form dataset of the schema.
pub fn catalogue_families(config: &Config) -> Vec<Family> {
    let mut families: Vec<Family> = Family::builtin_families().to_vec();
    families.extend(
        open_form_datasets(config)
            .into_iter()
            .map(|(name, _)| Family::Table(name)),
    );
    families
}

/// The available rows: one per family of [`catalogue_families`], each at
/// the family's firsts. A dataset keyed by the underlying alone takes no
/// tenor, so its row carries none — the resolver would refuse one.
pub fn catalogue(config: &Config) -> Vec<ListItem> {
    let bare: BTreeSet<String> = open_form_datasets(config)
        .into_iter()
        .filter(|(_, after_underlying)| *after_underlying == 0)
        .map(|(name, _)| name)
        .collect();
    catalogue_families(config)
        .into_iter()
        .map(|f| {
            let mut choice = ColumnChoice::new(f.clone());
            if matches!(&f, Family::Table(name) if bare.contains(name)) {
                choice.tenor = None;
            }
            ListItem {
                name: f.label(),
                included: false,
                presentation: Default::default(),
                kind: None,
                source: Some(column_source(&choice.to_spec())),
            }
        })
        .collect()
}

/// The list row's name: the choice's head (or a raw column's text),
/// de-duplicated against `taken` with ` ·2`, ` ·3` suffixes so two equal
/// columns stay two rows the name-keyed drag can tell apart.
pub fn item_name(choice: Result<&ColumnChoice, &Raw>, taken: &[String]) -> String {
    let base = match choice {
        Ok(c) => c.head(),
        Err(r) => r.text.clone(),
    };
    if !taken.contains(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base} ·{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The name a catalogue row takes as it is copied into the view's list: its
/// column's head (`SSR 1W` for the `SSR` row), de-duplicated against the
/// names already in `items`.
pub(super) fn added_name(entry: &ListItem, items: &[ListItem]) -> String {
    let taken: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
    let spec = entry.source.as_ref().and_then(spec_of);
    match spec.as_ref().map(ColumnChoice::from_spec) {
        Some(Ok(choice)) => item_name(Ok(&choice), &taken),
        Some(Err(raw)) => item_name(Err(&raw), &taken),
        None => item_name(
            Err(&Raw {
                text: entry.name.clone(),
            }),
            &taken,
        ),
    }
}

/// The view as `vol_views.toml` holds it: `cells` and the items' own
/// tables in list order, over the draft's source so any key this writer
/// does not model survives. `default` is the document's, not the view's,
/// and is never written here.
pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item {
    debug_assert_eq!(dest, Destination::Doc);
    let cells = match draft.choice("cells") {
        Some("single") => Cells::Single,
        _ => Cells::Pair,
    };
    let columns: Vec<VolColumnSpec> = draft
        .list_items("columns")
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.source.as_ref().and_then(spec_of))
                .collect()
        })
        .unwrap_or_default();
    let view = VolViewSpec {
        name: draft.name.clone(),
        cells,
        columns,
    };
    let source = super::toml_table_to_edit(&draft.source);
    let mut item = volviews::to_table(&view, Some(&source));
    // An empty array of tables prints as nothing, so a view with no
    // columns would reach the file as a bare `[name]` header. Spell the
    // empty list, which the reader accepts, so the written object says
    // what it is.
    if view.columns.is_empty()
        && let Some(table) = item.as_table_mut()
    {
        table["columns"] = toml_edit::value(toml_edit::Array::new());
    }
    item
}

/// Everything the readers would say about the rendered view, against the
/// schema's shapes, plus the one check they cannot make: a name another
/// view already holds.
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    let item = to_table(draft, Destination::Doc);
    let table: toml::Table = super::object_text(&draft.name, item)
        .parse()
        .unwrap_or_default();
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.into(),
            file: "<draft>".into(),
            table,
        }],
    );
    let (parsed, mut diags) = volviews::from_doc(&doc);
    let schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    // A measure dataset is a history table whose first key column is the
    // underlying; the resolver checks the rest of the key and the value
    // count against the measure text — the bridge's own closure.
    let shape = |name: &str| -> Option<TableShape> {
        let ds = schema.dataset(name).filter(|d| d.is_history())?;
        (ds.key.first().map(String::as_str) == Some("underlying_ref")).then(|| TableShape {
            key_len: ds.key.len(),
            values: ds.history_values().len(),
        })
    };
    let (_, d2) = volviews::resolve(&parsed, &shape);
    diags.extend(d2);
    // By the reader's own rule (`Domain::names_collide`): `Vol` beside `vol`
    // is a duplicate the reader would skip.
    if let Some(name) = draft.text("name").filter(|n| *n != draft.name)
        && Domain::VolViews
            .objects(config)
            .iter()
            .any(|r| Domain::VolViews.names_collide(&r.name, name))
    {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!("a view named {name} exists"),
            path: Some(format!("vol_views.{}.name", draft.name)),
        });
    }
    diags
}

/// What each field means, for the edit footer's help line; under ~90
/// characters, since the slot is one line.
pub fn help(key: &str) -> &'static str {
    match key {
        "name" => "The tab's name; digits 1–9 pick tabs in this order",
        "cells" => "pair shows the level and its companion; single shows one and m swaps",
        "default" => "The tab a fresh tile opens",
        "columns" => "space adds from the list below; enter edits; x removes; shift+j/k reorder",
        _ => "",
    }
}

/// Help for a column stage's fields. This domain opens no column stage
/// yet, so every key reads as the empty sentence that keeps the slot.
pub fn column_help(_key: &str) -> &'static str {
    ""
}

/// Whether a text field permits typed editing: a column's label and group.
/// The view's `name` is display-only (the module doc has why).
pub fn text_editable(key: &str) -> bool {
    matches!(key, "label" | "group")
}

/// Normalize committed text. A name is trimmed and refused when empty,
/// spaced or reserved; any other text is trimmed.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if key != "name" {
        return Ok(text.to_string());
    }
    if text.is_empty() {
        return Err("a view needs a name".to_string());
    }
    if text.chars().any(char::is_whitespace) {
        return Err("a view name has no spaces".to_string());
    }
    if Domain::VolViews.is_reserved(text) {
        return Err(format!("{text} is reserved"));
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::{Domain, object_text};
    use super::*;
    use crate::shell::ShellServices;
    use geode_core::config::ConfigSources;

    fn config_with(vol_views: &str, datasets: &str) -> Config {
        let docs = vec![
            LayerDoc::builtin("vol_views", vol_views).unwrap(),
            LayerDoc::builtin("datasets", datasets).unwrap(),
        ];
        ShellServices::config_and_builtin(ConfigSources {
            builtin: docs,
            desk: None,
            user: None,
        })
        .0
    }

    const SSR_DATASET: &str = "[ssr]\nfamily = \"history\"\nkey = [\"underlying_ref\", \"tenor\"]\nhistory = \"800d\"\n[ssr.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n[ssr.columns.tenor]\ntype = \"utf8\"\nrole = \"dimension\"\n[ssr.columns.ssr]\ntype = \"f64\"\nrole = \"value\"\n";
    const TWO_TABS: &str = "config_version = 1\ndefault = \"vol\"\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[[vol.columns]]\nkind = \"vega\"\n[skew]\ncells = \"single\"\n[[skew.columns]]\nmeasure = \"skew 1Y\"\ncompanion = \"slider\"\n";

    #[test]
    fn the_browse_rows_are_the_tabs_in_order_and_never_the_default_key() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let rows = Domain::VolViews.objects(&config);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["vol", "skew"],
            "document order, as the tile's tabs are"
        );
        assert_eq!(rows[0].summary, "pair · 2 columns · default");
        assert_eq!(rows[1].summary, "single · 1 columns");
    }

    #[test]
    fn the_fields_list_the_columns_as_heads_and_the_catalogue_ends_with_the_datasets() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let fields = fields(&config, Some("skew"));
        assert_eq!(
            fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
            ["name", "cells", "default", "columns"]
        );
        let FieldKind::OrderedList { items, available } = &fields[3].kind else {
            panic!()
        };
        assert_eq!(items[0].name, "25Δ skew 1Y · slider");
        let available = available.as_ref().unwrap();
        assert_eq!(available[0].name, "ATFM");
        assert_eq!(available.last().unwrap().name, "SSR");
        assert_eq!(available.len(), 13);
        assert_eq!(
            available
                .last()
                .unwrap()
                .source
                .as_ref()
                .and_then(|s| s.get("measure"))
                .and_then(|v| v.as_str()),
            Some("ssr 1W"),
            "a two-key dataset's row carries the first tenor"
        );
    }

    #[test]
    fn a_dataset_keyed_by_the_underlying_alone_offers_no_tenor() {
        let bare = "[spot]\nfamily = \"history\"\nkey = [\"underlying_ref\"]\nhistory = \"800d\"\n[spot.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n[spot.columns.spot]\ntype = \"f64\"\nrole = \"value\"\n";
        let config = config_with(TWO_TABS, bare);
        let available = catalogue(&config);
        let spot = available.last().unwrap();
        assert_eq!(spot.name, "SPOT");
        assert_eq!(
            spot.source
                .as_ref()
                .and_then(|s| s.get("measure"))
                .and_then(|v| v.as_str()),
            Some("spot")
        );
        let families = catalogue_families(&config);
        assert_eq!(families.len(), 13);
        assert_eq!(families.last(), Some(&Family::Table("spot".into())));
    }

    #[test]
    fn a_new_view_opens_with_no_columns_and_pair_cells() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let draft = Domain::VolViews.new_draft(&config, "analytics");
        assert!(draft.list_items("columns").unwrap().is_empty());
        assert_eq!(draft.choice("cells"), Some("pair"));
        let item = Domain::VolViews.to_table(&draft, Destination::Doc);
        assert!(object_text("analytics", item).contains("columns = []"));
    }

    #[test]
    fn to_table_writes_the_items_sources_in_list_order_and_keeps_an_unknown_key() {
        let config = config_with(
            "config_version = 1\n[vol]\nnote = \"kept\"\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[[vol.columns]]\nkind = \"vega\"\n",
            SSR_DATASET,
        );
        let mut draft = Domain::VolViews.draft(&config, "vol");
        // Swap the two columns.
        let FieldKind::OrderedList { items, .. } = &mut draft.fields[3].kind else {
            panic!()
        };
        items.swap(0, 1);
        let text = object_text("vol", Domain::VolViews.to_table(&draft, Destination::Doc));
        let vega = text.find("kind = \"vega\"").unwrap();
        let atfm = text.find("measure = \"atfm 3M\"").unwrap();
        assert!(vega < atfm, "{text}");
        assert!(text.contains("note = \"kept\""), "{text}");
        assert!(
            !text.contains("default"),
            "the document's key is not the view's: {text}"
        );
    }

    #[test]
    fn toggling_default_writes_nothing_into_the_view_table() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut draft = Domain::VolViews.draft(&config, "skew");
        let FieldKind::Bool(b) = &mut draft.fields[2].kind else {
            panic!()
        };
        *b = true;
        let text = object_text("skew", Domain::VolViews.to_table(&draft, Destination::Doc));
        assert!(!text.contains("default"), "{text}");
        assert!(text.contains("cells = \"single\""), "{text}");
    }

    #[test]
    fn validate_speaks_the_readers_texts_and_warns_on_a_dataset_the_startup_schema_lacks() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut draft = Domain::VolViews.draft(&config, "vol");
        let FieldKind::OrderedList { items, .. } = &mut draft.fields[3].kind else {
            panic!()
        };
        items.push(ListItem {
            name: "SSR 3M 0.5".into(),
            included: true,
            presentation: Default::default(),
            kind: None,
            source: Some("measure = \"ssr 3M 0.5\"".parse().unwrap()),
        });
        let diags = Domain::VolViews.validate(&draft, &config);
        assert!(
            diags.iter().any(|d| d
                .message
                .contains("'ssr' takes 1 key value after the underlying, not 2")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_name_another_view_holds_is_refused_with_an_error() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut draft = Domain::VolViews.draft(&config, "skew");
        assert!(
            Domain::VolViews
                .validate(&draft, &config)
                .iter()
                .all(|d| d.severity != Severity::Error)
        );
        let FieldKind::Text(name) = &mut draft.fields[0].kind else {
            panic!()
        };
        *name = "vol".into();
        let diags = Domain::VolViews.validate(&draft, &config);
        let refusal = diags
            .iter()
            .find(|d| d.severity == Severity::Error)
            .unwrap();
        assert_eq!(refusal.message, "a view named vol exists");
        assert_eq!(refusal.path.as_deref(), Some("vol_views.skew.name"));
    }

    /// The catalogue is a fixed list of families: `space` copies a row into
    /// the view under its column's head, twice gives two columns, and `x`
    /// drops a member without growing the catalogue.
    #[test]
    fn space_copies_a_catalogue_row_under_its_head_and_x_drops_a_member() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        let mut draft = Domain::VolViews.draft(&config, "vol");
        let select = |draft: &mut Draft, row: super::super::EditRow| {
            let rows = draft.rows();
            draft.selected = rows.iter().position(|r| *r == row).unwrap();
        };
        let ssr = super::super::EditRow::Available { field: 3, item: 12 };
        select(&mut draft, ssr);
        assert!(draft.toggle_selected().changed());
        assert!(
            draft.toggle_selected().changed(),
            "the cursor stayed on the SSR row"
        );
        let names: Vec<&str> = draft
            .list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, ["ATFM 3M", "Book vega", "SSR 1W", "SSR 1W ·2"]);
        assert_eq!(draft.available_items("columns").unwrap().len(), 13);

        // `space` on a member refuses with the way out; `x` takes it.
        select(
            &mut draft,
            super::super::EditRow::Item { field: 3, item: 2 },
        );
        assert!(matches!(
            draft.toggle_selected(),
            super::super::Step::Refused(_)
        ));
        assert!(draft.remove_selected().changed());
        let names: Vec<&str> = draft
            .list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, ["ATFM 3M", "Book vega", "SSR 1W ·2"]);
        assert_eq!(draft.available_items("columns").unwrap().len(), 13);
        let text = object_text("vol", Domain::VolViews.to_table(&draft, Destination::Doc));
        assert_eq!(text.matches("measure = \"ssr 1W\"").count(), 1, "{text}");
    }

    #[test]
    fn item_names_de_duplicate_with_a_counter() {
        let c = ColumnChoice::new(Family::Atfm);
        let taken = vec!["ATFM 1W".to_string()];
        assert_eq!(item_name(Ok(&c), &taken), "ATFM 1W ·2");
        let taken = vec!["ATFM 1W".to_string(), "ATFM 1W ·2".to_string()];
        assert_eq!(item_name(Ok(&c), &taken), "ATFM 1W ·3");
        let raw = Raw {
            text: "smile_curv 3M 0.25".into(),
        };
        assert_eq!(item_name(Err(&raw), &[]), "smile_curv 3M 0.25");
    }

    #[test]
    fn a_raw_column_keeps_its_text_and_its_table() {
        let config = config_with(
            "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"smile_curv 3M 0.25\"\n",
            SSR_DATASET,
        );
        let draft = Domain::VolViews.draft(&config, "vol");
        let items = draft.list_items("columns").unwrap();
        assert_eq!(items[0].name, "smile_curv 3M 0.25");
        let text = object_text("vol", Domain::VolViews.to_table(&draft, Destination::Doc));
        assert!(text.contains("measure = \"smile_curv 3M 0.25\""), "{text}");
    }

    #[test]
    fn the_name_row_is_display_only_and_a_typed_name_is_checked() {
        assert!(!text_editable("name"));
        assert!(text_editable("label") && text_editable("group"));
        assert_eq!(parse_text("name", " vol "), Ok("vol".to_string()));
        assert_eq!(
            parse_text("name", "my vol"),
            Err("a view name has no spaces".to_string())
        );
        assert_eq!(
            parse_text("name", "  "),
            Err("a view needs a name".to_string())
        );
        assert_eq!(
            parse_text("name", "default"),
            Err("default is reserved".to_string())
        );
        assert_eq!(parse_text("label", " Spot "), Ok("Spot".to_string()));
    }

    #[test]
    fn reserved_names_are_the_documents_own_keys() {
        assert!(Domain::VolViews.is_reserved("config_version"));
        assert!(Domain::VolViews.is_reserved("default"));
        assert!(!Domain::VolViews.is_reserved("vol"));
        assert_eq!(Domain::VolViews.document_keys(), ["default"]);
        assert!(Domain::Views.document_keys().is_empty());
    }

    /// The reader skips a second `Vol` beside `vol`, so the dialog's gates
    /// treat the two as one name: creation and the clash check both refuse,
    /// while Views keeps its exact comparison.
    #[test]
    fn a_name_differing_only_in_case_is_taken_and_refused() {
        let config = config_with(TWO_TABS, SSR_DATASET);
        assert!(Domain::VolViews.names_collide("Vol", "vol"));
        assert!(!Domain::Views.names_collide("Vol", "vol"));
        assert!(Domain::VolViews.name_taken(&config, "Vol"));
        assert!(Domain::VolViews.name_taken(&config, "SKEW"));
        assert!(!Domain::VolViews.name_taken(&config, "analytics"));

        let mut draft = Domain::VolViews.draft(&config, "skew");
        let FieldKind::Text(name) = &mut draft.fields[0].kind else {
            panic!()
        };
        *name = "VOL".into();
        let diags = Domain::VolViews.validate(&draft, &config);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message == "a view named VOL exists"),
            "{diags:?}"
        );
    }
}
